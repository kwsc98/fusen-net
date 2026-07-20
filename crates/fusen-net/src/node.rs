// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Role-neutral node model and relay/edge runtimes.

use std::{
    fmt,
    net::SocketAddr,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use rand::Rng;
use tokio::{
    sync::{Semaphore, mpsc, watch},
    task::JoinSet,
};
use tracing::{debug, error, warn};

use crate::{
    address::{
        AddressAllocator, AddressError, AddressLease, NodeToken, validate_node_id,
        validate_overlay_address,
    },
    control::{
        ControlMessage, ErrorCode, ErrorMessage, ProtocolError, Ready, Register, RegisterAccepted,
        read_message, write_message,
    },
    data_plane::PacketValidator,
    routing::{RouteError, RouteRegistration, RouteTable, SessionId},
    transport::{TransportConnection, TransportEndpoint, TransportError},
    tun::{RouteLease, RouteManager, TunConfig, TunError, TunFactory},
};

pub const ROUTE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_MAX_RELAY_CONNECTIONS: usize = 1024;
pub const RECONNECT_MIN_DELAY: Duration = Duration::from_secs(1);
pub const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeMode {
    Relay,
    Edge,
    Hybrid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeCapabilities {
    listen: bool,
    dial: bool,
    route: bool,
    tun: bool,
}

impl NodeCapabilities {
    pub const RELAY: Self = Self::new(true, false, true, false);
    pub const EDGE: Self = Self::new(false, true, false, true);

    pub const fn new(listen: bool, dial: bool, route: bool, tun: bool) -> Self {
        Self {
            listen,
            dial,
            route,
            tun,
        }
    }

    pub const fn can_listen(self) -> bool {
        self.listen
    }

    pub const fn can_dial(self) -> bool {
        self.dial
    }

    pub const fn can_route(self) -> bool {
        self.route
    }

    pub const fn has_tun(self) -> bool {
        self.tun
    }

    const fn is_empty(self) -> bool {
        !self.listen && !self.dial && !self.route && !self.tun
    }

    const fn contains(self, required: Self) -> bool {
        (!required.listen || self.listen)
            && (!required.dial || self.dial)
            && (!required.route || self.route)
            && (!required.tun || self.tun)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct NodeIdentity {
    node_id: String,
    token: String,
}

impl NodeIdentity {
    pub fn new(node_id: impl Into<String>, encoded_token: &str) -> Result<Self, NodeError> {
        let node_id = node_id.into();
        validate_node_id(&node_id)?;
        let token = NodeToken::parse(encoded_token)?.encode();
        Ok(Self { node_id, token })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    fn registration(&self) -> Register {
        Register {
            node_id: self.node_id.clone(),
            token: self.token.clone(),
        }
    }
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NodeIdentity")
            .field("node_id", &self.node_id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct NodeRuntime {
    identity: Option<NodeIdentity>,
    capabilities: NodeCapabilities,
}

impl NodeRuntime {
    pub const fn relay() -> Self {
        Self {
            identity: None,
            capabilities: NodeCapabilities::RELAY,
        }
    }

    pub fn edge(identity: NodeIdentity) -> Self {
        Self {
            identity: Some(identity),
            capabilities: NodeCapabilities::EDGE,
        }
    }

    pub fn with_capabilities(
        identity: Option<NodeIdentity>,
        capabilities: NodeCapabilities,
    ) -> Result<Self, NodeError> {
        if capabilities.is_empty() {
            return Err(NodeError::NoNodeCapabilities);
        }
        if (capabilities.can_dial() || capabilities.has_tun()) && identity.is_none() {
            return Err(NodeError::NodeIdentityRequired);
        }
        Ok(Self {
            identity,
            capabilities,
        })
    }

    pub fn mode(&self) -> NodeMode {
        if self.capabilities == NodeCapabilities::RELAY {
            NodeMode::Relay
        } else if self.capabilities == NodeCapabilities::EDGE {
            NodeMode::Edge
        } else {
            NodeMode::Hybrid
        }
    }

    pub fn identity(&self) -> Option<&NodeIdentity> {
        self.identity.as_ref()
    }

    pub const fn capabilities(&self) -> NodeCapabilities {
        self.capabilities
    }
}

#[derive(Clone)]
pub struct RelayRuntime {
    node: NodeRuntime,
    allocator: Arc<dyn AddressAllocator>,
    routes: Arc<RouteTable>,
    validator: PacketValidator,
    connection_slots: Arc<Semaphore>,
}

impl RelayRuntime {
    pub fn new(
        allocator: Arc<dyn AddressAllocator>,
        routes: Arc<RouteTable>,
        validator: PacketValidator,
    ) -> Result<Self, NodeError> {
        Self::with_connection_limit(allocator, routes, validator, DEFAULT_MAX_RELAY_CONNECTIONS)
    }

    fn with_connection_limit(
        allocator: Arc<dyn AddressAllocator>,
        routes: Arc<RouteTable>,
        validator: PacketValidator,
        max_connections: usize,
    ) -> Result<Self, NodeError> {
        Self::with_node_and_connection_limit(
            NodeRuntime::relay(),
            allocator,
            routes,
            validator,
            max_connections,
        )
    }

    pub fn for_node(
        node: NodeRuntime,
        allocator: Arc<dyn AddressAllocator>,
        routes: Arc<RouteTable>,
        validator: PacketValidator,
    ) -> Result<Self, NodeError> {
        Self::with_node_and_connection_limit(
            node,
            allocator,
            routes,
            validator,
            DEFAULT_MAX_RELAY_CONNECTIONS,
        )
    }

    fn with_node_and_connection_limit(
        node: NodeRuntime,
        allocator: Arc<dyn AddressAllocator>,
        routes: Arc<RouteTable>,
        validator: PacketValidator,
        max_connections: usize,
    ) -> Result<Self, NodeError> {
        if !node.capabilities().contains(NodeCapabilities::RELAY) {
            return Err(NodeError::MissingNodeCapabilities("listen + route"));
        }
        if routes.overlay() != validator.overlay() {
            return Err(NodeError::OverlayMismatch);
        }
        if max_connections == 0 {
            return Err(NodeError::InvalidConnectionLimit);
        }
        Ok(Self {
            node,
            allocator,
            routes,
            validator,
            connection_slots: Arc::new(Semaphore::new(max_connections)),
        })
    }

    pub fn node(&self) -> &NodeRuntime {
        &self.node
    }

    pub fn routes(&self) -> &Arc<RouteTable> {
        &self.routes
    }

    /// Runs all listeners against one allocator and route table. Each
    /// transport listener remains backend-specific, while authenticated
    /// sessions share the same data plane.
    pub async fn run_multi(
        self: Arc<Self>,
        listeners: Vec<Arc<dyn TransportEndpoint>>,
    ) -> Result<(), NodeError> {
        if listeners.is_empty() {
            return Err(NodeError::NoListeners);
        }

        let mut tasks = JoinSet::new();
        for listener in listeners {
            let runtime = self.clone();
            tasks.spawn(async move { runtime.accept_loop(listener).await });
        }

        let result = match tasks.join_next().await {
            Some(Ok(result)) => result,
            Some(Err(error)) => Err(NodeError::TaskFailed(error.to_string())),
            None => Err(NodeError::NoListeners),
        };
        tasks.abort_all();
        result
    }

    async fn accept_loop(
        self: Arc<Self>,
        listener: Arc<dyn TransportEndpoint>,
    ) -> Result<(), NodeError> {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let connection = accepted?;
                    let Ok(connection_slot) = self.connection_slots.clone().try_acquire_owned() else {
                        warn!(
                            remote_address = %connection.remote_address(),
                            "dropping relay connection because the connection limit is reached"
                        );
                        continue;
                    };
                    let runtime = self.clone();
                    connections.spawn(async move {
                        let _connection_slot = connection_slot;
                        if let Err(error) = runtime.handle_connection(connection).await {
                            debug!(reason = %error, "relay connection closed");
                        }
                    });
                }
                joined = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = joined {
                        warn!(%error, "relay connection task failed");
                    }
                }
            }
        }
    }

    async fn handle_connection(
        self: Arc<Self>,
        connection: Box<dyn TransportConnection>,
    ) -> Result<(), NodeError> {
        self.handle_connection_with_timeout(connection, HANDSHAKE_TIMEOUT)
            .await
    }

    async fn handle_connection_with_timeout(
        self: Arc<Self>,
        mut connection: Box<dyn TransportConnection>,
        handshake_timeout: Duration,
    ) -> Result<(), NodeError> {
        let handshake = async {
            let (mut reader, mut writer) = connection.accept_bi().await?;
            let registration = match read_message(&mut reader).await {
                Ok(ControlMessage::Register(registration)) => registration,
                Ok(_) => {
                    send_control_error(
                        &mut writer,
                        ErrorCode::ProtocolViolation,
                        "expected Register",
                        false,
                    )
                    .await;
                    return Err(NodeError::InvalidHandshake("expected Register"));
                }
                Err(error) => {
                    send_control_error(
                        &mut writer,
                        ErrorCode::ProtocolViolation,
                        "invalid control frame",
                        false,
                    )
                    .await;
                    return Err(error.into());
                }
            };

            let lease = match self
                .allocator
                .allocate(&registration.node_id, &registration.token)
            {
                Ok(lease) => lease,
                Err(error) => {
                    send_control_error(
                        &mut writer,
                        ErrorCode::AuthenticationFailed,
                        "authentication failed",
                        false,
                    )
                    .await;
                    return Err(error.into());
                }
            };

            let session_id = SessionId::new();
            let receiver = match self.routes.reserve(RouteRegistration {
                node_id: lease.node_id.clone(),
                overlay_ip: lease.overlay_ip,
                session_id,
            }) {
                Ok(receiver) => receiver,
                Err(error @ (RouteError::DuplicateNode(_) | RouteError::DuplicateAddress(_))) => {
                    send_control_error(
                        &mut writer,
                        ErrorCode::DuplicateNode,
                        "node already has an active session",
                        true,
                    )
                    .await;
                    if let Err(release_error) = self.allocator.release(&lease) {
                        warn!(reason = %release_error, "failed to release address lease after duplicate session");
                    }
                    return Err(error.into());
                }
                Err(error) => {
                    send_control_error(
                        &mut writer,
                        ErrorCode::Internal,
                        "route reservation failed",
                        true,
                    )
                    .await;
                    if let Err(release_error) = self.allocator.release(&lease) {
                        warn!(reason = %release_error, "failed to release address lease after route reservation error");
                    }
                    return Err(error.into());
                }
            };
            let cleanup = RelaySessionCleanup {
                allocator: self.allocator.clone(),
                routes: self.routes.clone(),
                lease: lease.clone(),
                session_id,
            };

            write_message(
                &mut writer,
                &ControlMessage::RegisterAccepted(RegisterAccepted {
                    overlay_ip: lease.overlay_ip,
                    overlay: self.validator.overlay(),
                    mtu: self.validator.mtu() as u16,
                    session_id: session_id.to_string(),
                }),
            )
            .await?;

            let ready = loop {
                tokio::select! {
                    biased;
                    incoming = connection.recv_datagram() => {
                        incoming?;
                        debug!("dropping overlay datagram received before Ready");
                    }
                    control = read_message(&mut reader) => break control?,
                }
            };
            match ready {
                ControlMessage::Ready(_) => {}
                _ => {
                    send_control_error(
                        &mut writer,
                        ErrorCode::ProtocolViolation,
                        "expected Ready",
                        false,
                    )
                    .await;
                    return Err(NodeError::InvalidHandshake("expected Ready"));
                }
            }
            if !self.routes.activate_session(lease.overlay_ip, session_id) {
                return Err(NodeError::Route(RouteError::StaleSession));
            }

            Ok((receiver, lease, session_id, cleanup))
        };

        let (receiver, lease, session_id, _cleanup) =
            tokio::time::timeout(handshake_timeout, handshake)
                .await
                .map_err(|_| NodeError::HandshakeTimeout)??;

        self.route_session(connection, receiver, lease.overlay_ip, session_id)
            .await
    }

    async fn route_session(
        &self,
        mut connection: Box<dyn TransportConnection>,
        mut receiver: mpsc::Receiver<bytes::Bytes>,
        source_address: std::net::Ipv4Addr,
        session_id: SessionId,
    ) -> Result<(), NodeError> {
        let mut observed_receive_drops = 0;
        loop {
            let receive_drops =
                observe_receive_drops(connection.as_ref(), &mut observed_receive_drops)?;
            self.routes.record_transport_drops(receive_drops);
            tokio::select! {
                incoming = connection.recv_datagram() => {
                    let packet = incoming?;
                    match self.routes.route_from(
                        session_id,
                        source_address,
                        packet,
                        &self.validator,
                    ) {
                        Ok(_) => {}
                        Err(RouteError::StaleSession) => return Err(RouteError::StaleSession.into()),
                        Err(error) => debug!(reason = %error, "dropping invalid overlay datagram"),
                    }
                }
                outgoing = receiver.recv() => {
                    let Some(packet) = outgoing else {
                        return Err(RouteError::StaleSession.into());
                    };
                    match connection.send_datagram(packet) {
                        Ok(()) => {}
                        Err(error @ (TransportError::DatagramQueueFull | TransportError::DatagramTooLarge)) => {
                            self.routes.record_transport_drop();
                            debug!(reason = %error, "dropping outbound overlay datagram");
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
    }
}

struct RelaySessionCleanup {
    allocator: Arc<dyn AddressAllocator>,
    routes: Arc<RouteTable>,
    lease: AddressLease,
    session_id: SessionId,
}

impl Drop for RelaySessionCleanup {
    fn drop(&mut self) {
        self.routes
            .remove_session(self.lease.overlay_ip, self.session_id);
        if let Err(error) = self.allocator.release(&self.lease) {
            warn!(reason = %error, "failed to release relay address lease");
        }
    }
}

pub struct EdgeRuntime {
    node: NodeRuntime,
    server_address: SocketAddr,
    server_name: String,
    tun_name: Option<String>,
    dropped_transport: AtomicU64,
}

impl EdgeRuntime {
    pub fn new(
        identity: NodeIdentity,
        server_address: SocketAddr,
        server_name: impl Into<String>,
        tun_name: Option<String>,
    ) -> Result<Self, NodeError> {
        Self::for_node(
            NodeRuntime::edge(identity),
            server_address,
            server_name,
            tun_name,
        )
    }

    pub fn for_node(
        node: NodeRuntime,
        server_address: SocketAddr,
        server_name: impl Into<String>,
        tun_name: Option<String>,
    ) -> Result<Self, NodeError> {
        if !node.capabilities().contains(NodeCapabilities::EDGE) {
            return Err(NodeError::MissingNodeCapabilities("dial + TUN"));
        }
        if node.identity().is_none() {
            return Err(NodeError::NodeIdentityRequired);
        }
        let server_name = server_name.into();
        if server_name.is_empty() {
            return Err(NodeError::InvalidServerName);
        }
        Ok(Self {
            node,
            server_address,
            server_name,
            tun_name,
            dropped_transport: AtomicU64::new(0),
        })
    }

    pub fn node(&self) -> &NodeRuntime {
        &self.node
    }

    pub fn dropped_transport(&self) -> u64 {
        self.dropped_transport.load(Ordering::Relaxed)
    }

    pub async fn run_session(
        &self,
        endpoint: Arc<dyn TransportEndpoint>,
        tun_factory: Arc<dyn TunFactory>,
        route_manager: Arc<dyn RouteManager>,
    ) -> Result<(), NodeError> {
        let (_shutdown_sender, mut shutdown) = watch::channel(false);
        self.run_session_with_shutdown(endpoint, tun_factory, route_manager, &mut shutdown)
            .await
    }

    /// Runs one session and performs route cleanup before returning when the
    /// watch value becomes `true`.
    pub async fn run_session_with_shutdown(
        &self,
        endpoint: Arc<dyn TransportEndpoint>,
        tun_factory: Arc<dyn TunFactory>,
        route_manager: Arc<dyn RouteManager>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), NodeError> {
        self.run_session_with_shutdown_and_timeout(
            endpoint,
            tun_factory,
            route_manager,
            shutdown,
            HANDSHAKE_TIMEOUT,
        )
        .await
    }

    async fn run_session_with_shutdown_and_timeout(
        &self,
        endpoint: Arc<dyn TransportEndpoint>,
        tun_factory: Arc<dyn TunFactory>,
        route_manager: Arc<dyn RouteManager>,
        shutdown: &mut watch::Receiver<bool>,
        handshake_timeout: Duration,
    ) -> Result<(), NodeError> {
        let identity = self.node.identity().ok_or(NodeError::InvalidNodeMode)?;
        let handshake = async {
            let connection = endpoint
                .connect(self.server_address, &self.server_name)
                .await?;
            let (mut reader, mut writer) = connection.open_bi().await?;
            write_message(
                &mut writer,
                &ControlMessage::Register(identity.registration()),
            )
            .await?;
            let response = read_message(&mut reader).await?;
            Ok::<_, NodeError>((connection, reader, writer, response))
        };
        let (mut connection, _reader, mut writer, response) = tokio::select! {
            result = tokio::time::timeout(handshake_timeout, handshake) => {
                result.map_err(|_| NodeError::HandshakeTimeout)??
            }
            _ = wait_for_shutdown(shutdown) => return Ok(()),
        };

        let accepted = match response {
            ControlMessage::RegisterAccepted(accepted) => accepted,
            ControlMessage::Error(error) => return Err(NodeError::RemoteRejected(error)),
            _ => return Err(NodeError::InvalidHandshake("expected RegisterAccepted")),
        };
        SessionId::from_str(&accepted.session_id)
            .map_err(|_| NodeError::InvalidHandshake("invalid session ID"))?;
        validate_overlay_address(accepted.overlay, accepted.overlay_ip)?;
        let validator = PacketValidator::new(accepted.overlay, usize::from(accepted.mtu))?;

        let tun_config = TunConfig {
            name: self.tun_name.clone(),
            address: accepted.overlay_ip,
            overlay: accepted.overlay,
            mtu: accepted.mtu,
        };
        let mut device = tokio::select! {
            result = tun_factory.create(&tun_config) => result?,
            _ = wait_for_shutdown(shutdown) => return Ok(()),
        };
        if *shutdown.borrow() {
            return Ok(());
        }

        // Route installation is deliberately not cancellation-sensitive. If
        // shutdown wins after the native command changed system state but
        // before this future returned, cancelling here would lose the lease
        // needed for compensation. Implementations must bound installation;
        // NativeRouteManager enforces a five-second command timeout.
        let route_install_started = Instant::now();
        let route_lease = route_manager
            .install_overlay_route(&tun_config, device.name())
            .await?;
        let cleanup_timeout = if *shutdown.borrow() {
            ROUTE_CLEANUP_TIMEOUT.saturating_sub(route_install_started.elapsed())
        } else {
            ROUTE_CLEANUP_TIMEOUT
        };

        let result: Result<(), NodeError> = async {
            let mut observed_receive_drops = 0;
            tokio::select! {
                result = write_message(&mut writer, &ControlMessage::Ready(Ready {})) => result?,
                _ = wait_for_shutdown(shutdown) => return Ok(()),
            }
            loop {
                let receive_drops = observe_receive_drops(
                    connection.as_ref(),
                    &mut observed_receive_drops,
                )?;
                self.dropped_transport
                    .fetch_add(receive_drops, Ordering::Relaxed);
                tokio::select! {
                    _ = wait_for_shutdown(shutdown) => return Ok(()),
                    incoming = connection.recv_datagram() => {
                        let packet = incoming?;
                        if let Err(error) = validator.validate_inbound(&packet, accepted.overlay_ip) {
                            debug!(reason = %error, "dropping invalid inbound overlay datagram");
                            continue;
                        }
                        tokio::select! {
                            result = device.write_packet(packet) => result?,
                            _ = wait_for_shutdown(shutdown) => return Ok(()),
                        }
                    }
                    outgoing = device.read_packet() => {
                        let packet = outgoing?;
                        if let Err(error) = validator.validate(&packet, accepted.overlay_ip) {
                            debug!(reason = %error, "dropping invalid outbound overlay datagram");
                            continue;
                        }
                        match connection.send_datagram(packet) {
                            Ok(()) => {}
                            Err(error @ (TransportError::DatagramQueueFull | TransportError::DatagramTooLarge)) => {
                                self.dropped_transport.fetch_add(1, Ordering::Relaxed);
                                debug!(reason = %error, "dropping outbound TUN datagram");
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                }
            }
        }
        .await;

        let cleanup = cleanup_route(route_manager.as_ref(), route_lease, cleanup_timeout).await;
        match (result, cleanup) {
            (Err(session), Ok(())) => Err(session),
            (Err(session), Err(cleanup)) => {
                error!(%session, %cleanup, "overlay route cleanup failed after session error");
                Err(NodeError::SessionCleanupFailed {
                    session: Box::new(session),
                    cleanup: Box::new(cleanup),
                })
            }
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(error),
        }
    }

    pub async fn run_with_reconnect(
        &self,
        endpoint: Arc<dyn TransportEndpoint>,
        tun_factory: Arc<dyn TunFactory>,
        route_manager: Arc<dyn RouteManager>,
    ) -> Result<(), NodeError> {
        let (_shutdown_sender, mut shutdown) = watch::channel(false);
        self.run_with_reconnect_until(endpoint, tun_factory, route_manager, &mut shutdown)
            .await
    }

    pub async fn run_with_reconnect_until(
        &self,
        endpoint: Arc<dyn TransportEndpoint>,
        tun_factory: Arc<dyn TunFactory>,
        route_manager: Arc<dyn RouteManager>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), NodeError> {
        let mut backoff = ReconnectBackoff::default();
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let session_started = Instant::now();
            let result = self
                .run_session_with_shutdown(
                    endpoint.clone(),
                    tun_factory.clone(),
                    route_manager.clone(),
                    shutdown,
                )
                .await;
            if *shutdown.borrow() {
                return result;
            }
            if let Err(error) = result.as_ref()
                && !error.is_retryable()
            {
                return result;
            }
            if session_started.elapsed() >= RECONNECT_MAX_DELAY {
                backoff.reset();
            }
            let delay = backoff.next_delay();
            warn!(?delay, "edge session disconnected; reconnecting");
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = wait_for_shutdown(shutdown) => return Ok(()),
            }
        }
    }
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if shutdown.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

fn observe_receive_drops(
    connection: &dyn TransportConnection,
    observed: &mut u64,
) -> Result<u64, NodeError> {
    let current = connection.dropped_incoming_datagrams()?;
    let delta = current.saturating_sub(*observed);
    *observed = current;
    Ok(delta)
}

async fn cleanup_route(
    route_manager: &dyn RouteManager,
    lease: RouteLease,
    cleanup_timeout: Duration,
) -> Result<(), NodeError> {
    match tokio::time::timeout(cleanup_timeout, route_manager.remove_overlay_route(lease)).await {
        Ok(result) => result.map_err(NodeError::from),
        Err(_) => Err(NodeError::RouteCleanupTimeout),
    }
}

async fn send_control_error<W>(writer: &mut W, code: ErrorCode, message: &str, retryable: bool)
where
    W: tokio::io::AsyncWrite + Unpin + ?Sized,
{
    if let Err(error) = write_message(
        writer,
        &ControlMessage::Error(ErrorMessage {
            code,
            message: message.to_owned(),
            retryable,
        }),
    )
    .await
    {
        debug!(reason = %error, "failed to send control-plane error response");
    }
}

#[derive(Clone, Debug)]
pub struct ReconnectBackoff {
    next: Duration,
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        Self {
            next: RECONNECT_MIN_DELAY,
        }
    }
}

impl ReconnectBackoff {
    pub fn reset(&mut self) {
        self.next = RECONNECT_MIN_DELAY;
    }

    pub fn next_delay(&mut self) -> Duration {
        let base = self.next;
        self.next = self.next.saturating_mul(2).min(RECONNECT_MAX_DELAY);
        let jitter = rand::thread_rng().gen_range(0.8_f64..=1.2_f64);
        Duration::from_secs_f64((base.as_secs_f64() * jitter).clamp(1.0, 30.0))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Route(#[from] RouteError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Tun(#[from] TunError),
    #[error("relay route table and validator use different overlays")]
    OverlayMismatch,
    #[error("at least one transport listener is required")]
    NoListeners,
    #[error("relay connection limit must be greater than zero")]
    InvalidConnectionLimit,
    #[error("control handshake did not finish within ten seconds")]
    HandshakeTimeout,
    #[error("invalid handshake: {0}")]
    InvalidHandshake(&'static str),
    #[error("edge runtime requires an identity")]
    InvalidNodeMode,
    #[error("a node must have at least one runtime capability")]
    NoNodeCapabilities,
    #[error("dial or TUN capabilities require a node identity")]
    NodeIdentityRequired,
    #[error("node is missing required runtime capabilities: {0}")]
    MissingNodeCapabilities(&'static str),
    #[error("TLS server name must not be empty")]
    InvalidServerName,
    #[error("remote rejected registration: {0:?}")]
    RemoteRejected(ErrorMessage),
    #[error("route cleanup did not finish within five seconds")]
    RouteCleanupTimeout,
    #[error("session failed ({session}); route cleanup failed ({cleanup})")]
    SessionCleanupFailed {
        session: Box<NodeError>,
        cleanup: Box<NodeError>,
    },
    #[error("runtime task failed: {0}")]
    TaskFailed(String),
}

impl NodeError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RemoteRejected(error) => error.retryable,
            Self::Address(AddressError::AuthenticationFailed)
            | Self::InvalidHandshake(_)
            | Self::InvalidNodeMode
            | Self::NoNodeCapabilities
            | Self::NodeIdentityRequired
            | Self::MissingNodeCapabilities(_)
            | Self::InvalidServerName
            | Self::RouteCleanupTimeout
            | Self::SessionCleanupFailed { .. } => false,
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        address::{StaticAddressAllocator, StaticBinding},
        transport::{BoxReadStream, BoxWriteStream},
    };

    fn relay(max_connections: usize) -> Arc<RelayRuntime> {
        let overlay = "10.42.0.0/24".parse().expect("network");
        let allocator = Arc::new(
            StaticAddressAllocator::new(overlay, std::iter::empty::<StaticBinding>())
                .expect("allocator"),
        );
        let routes = Arc::new(RouteTable::with_default_capacity(overlay));
        let validator = PacketValidator::new(overlay, 1100).expect("validator");
        Arc::new(
            RelayRuntime::with_connection_limit(allocator, routes, validator, max_connections)
                .expect("relay"),
        )
    }

    struct PendingHandshakeConnection;

    struct PendingEndpoint;

    #[async_trait::async_trait]
    impl TransportEndpoint for PendingEndpoint {
        async fn accept(&self) -> Result<Box<dyn TransportConnection>, TransportError> {
            Err(TransportError::UnsupportedRole)
        }

        async fn connect(
            &self,
            _address: SocketAddr,
            _server_name: &str,
        ) -> Result<Box<dyn TransportConnection>, TransportError> {
            std::future::pending().await
        }
    }

    struct UnusedTunFactory;

    #[async_trait::async_trait]
    impl TunFactory for UnusedTunFactory {
        async fn create(
            &self,
            _config: &TunConfig,
        ) -> Result<Box<dyn crate::tun::PacketDevice>, TunError> {
            Err(TunError::Closed)
        }
    }

    struct UnusedRouteManager;

    #[async_trait::async_trait]
    impl RouteManager for UnusedRouteManager {
        async fn install_overlay_route(
            &self,
            _config: &TunConfig,
            _interface_name: &str,
        ) -> Result<RouteLease, TunError> {
            Err(TunError::Closed)
        }

        async fn remove_overlay_route(&self, _lease: RouteLease) -> Result<(), TunError> {
            Err(TunError::Closed)
        }
    }

    #[async_trait::async_trait]
    impl TransportConnection for PendingHandshakeConnection {
        async fn open_bi(&self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
            Err(TransportError::UnsupportedRole)
        }

        async fn accept_bi(&mut self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
            std::future::pending().await
        }

        fn send_datagram(&self, _packet: bytes::Bytes) -> Result<(), TransportError> {
            Err(TransportError::ConnectionClosed)
        }

        async fn recv_datagram(&mut self) -> Result<bytes::Bytes, TransportError> {
            Err(TransportError::ConnectionClosed)
        }

        fn remote_address(&self) -> SocketAddr {
            SocketAddr::from(([127, 0, 0, 1], 7000))
        }

        async fn closed(&self) -> TransportError {
            TransportError::ConnectionClosed
        }
    }

    fn identity() -> NodeIdentity {
        NodeIdentity::new("edge-a", &NodeToken::from_bytes([7; 32]).encode()).expect("identity")
    }

    #[test]
    fn identity_debug_output_redacts_token() {
        let output = format!("{:?}", identity());
        assert!(output.contains("edge-a"));
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains(&NodeToken::from_bytes([7; 32]).encode()));
    }

    #[test]
    fn role_neutral_model_composes_identity_and_capabilities() {
        let relay = NodeRuntime::relay();
        assert_eq!(relay.mode(), NodeMode::Relay);
        assert!(relay.identity().is_none());
        assert!(relay.capabilities().can_listen());
        assert!(relay.capabilities().can_route());
        assert!(!relay.capabilities().can_dial());
        assert!(!relay.capabilities().has_tun());

        let edge = NodeRuntime::edge(identity());
        assert_eq!(edge.mode(), NodeMode::Edge);
        assert_eq!(edge.identity().map(NodeIdentity::node_id), Some("edge-a"));
        assert!(edge.capabilities().can_dial());
        assert!(edge.capabilities().has_tun());

        let hybrid = NodeRuntime::with_capabilities(
            Some(identity()),
            NodeCapabilities::new(true, true, true, true),
        )
        .expect("hybrid capability set");
        assert_eq!(hybrid.mode(), NodeMode::Hybrid);
        assert!(hybrid.capabilities().contains(NodeCapabilities::RELAY));
        assert!(hybrid.capabilities().contains(NodeCapabilities::EDGE));
    }

    #[test]
    fn active_edge_capabilities_require_an_identity() {
        let error = NodeRuntime::with_capabilities(None, NodeCapabilities::EDGE)
            .expect_err("edge capability set without identity must fail");
        assert!(matches!(error, NodeError::NodeIdentityRequired));
    }

    #[test]
    fn reconnect_backoff_is_jittered_and_capped() {
        let mut backoff = ReconnectBackoff::default();
        for attempt in 0..12 {
            let delay = backoff.next_delay();
            assert!(delay >= RECONNECT_MIN_DELAY);
            assert!(delay <= RECONNECT_MAX_DELAY);
            if attempt == 0 {
                assert!(delay <= Duration::from_millis(1200));
            }
        }
        backoff.reset();
        let reset = backoff.next_delay();
        assert!(reset >= RECONNECT_MIN_DELAY);
        assert!(reset <= Duration::from_millis(1200));
    }

    #[test]
    fn relay_connection_limit_is_shared_and_released() {
        let runtime = relay(1);
        let first = runtime
            .connection_slots
            .clone()
            .try_acquire_owned()
            .expect("first connection slot");
        assert!(
            runtime
                .connection_slots
                .clone()
                .try_acquire_owned()
                .is_err()
        );
        drop(first);
        assert!(runtime.connection_slots.clone().try_acquire_owned().is_ok());
    }

    #[tokio::test]
    async fn stalled_relay_handshake_times_out() {
        let error = relay(1)
            .handle_connection_with_timeout(
                Box::new(PendingHandshakeConnection),
                Duration::from_millis(10),
            )
            .await
            .expect_err("stalled handshake must fail");
        assert!(matches!(error, NodeError::HandshakeTimeout));
    }

    #[tokio::test]
    async fn edge_shutdown_cancels_pending_connect() {
        let runtime = EdgeRuntime::new(
            identity(),
            "127.0.0.1:7000".parse().expect("server address"),
            "localhost",
            None,
        )
        .expect("edge runtime");
        let (_shutdown_sender, mut shutdown) = watch::channel(true);
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            runtime.run_session_with_shutdown(
                Arc::new(PendingEndpoint),
                Arc::new(UnusedTunFactory),
                Arc::new(UnusedRouteManager),
                &mut shutdown,
            ),
        )
        .await
        .expect("shutdown must not wait for connect");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn stalled_edge_handshake_times_out() {
        let runtime = EdgeRuntime::new(
            identity(),
            "127.0.0.1:7000".parse().expect("server address"),
            "localhost",
            None,
        )
        .expect("edge runtime");
        let (_shutdown_sender, mut shutdown) = watch::channel(false);
        let error = runtime
            .run_session_with_shutdown_and_timeout(
                Arc::new(PendingEndpoint),
                Arc::new(UnusedTunFactory),
                Arc::new(UnusedRouteManager),
                &mut shutdown,
                Duration::from_millis(10),
            )
            .await
            .expect_err("stalled edge handshake must fail");
        assert!(matches!(error, NodeError::HandshakeTimeout));
    }
}
