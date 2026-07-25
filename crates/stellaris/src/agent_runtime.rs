// SPDX-License-Identifier: Apache-2.0 OR MIT

#![cfg_attr(not(feature = "backend-quinn"), allow(dead_code, unused_imports))]

//! Stellaris v2 Agent runtime.
//!
//! The TUN device has process lifetime while control, Relay, and P2P paths are
//! replaceable sessions. Outbound packets make one path decision: a failed
//! P2P send invalidates that path for subsequent packets but is never retried
//! over Relay.

use std::{
    collections::HashMap,
    fmt, fs,
    future::{Future, pending},
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    path::PathBuf,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use ipnet::Ipv4Net;
use rand::Rng as _;
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    time::{MissedTickBehavior, sleep, sleep_until, timeout, timeout_at},
};

use crate::{
    coordination::CertificateFingerprint,
    identity::{
        IdentityError, NodeKey, VerifiedNodeCertificate, extract_verified_node_certificate,
        verify_node_certificate_der,
    },
    identity_store::{InstalledNodeIdentity, NodeIdentityStore, NodeIdentityStoreError},
    metrics::{PacketDropReason, RuntimeMetrics},
    peer_manager::{
        ConnectionAction, ConnectionDirection, PeerConnectionId, PeerManager, PeerManagerError,
        PeerPathState, ProbeGeneration, ReadyOutcome, SelectedPath,
    },
    protocol::{
        AnnounceCandidates, Candidate, CandidateKind, CertificateIssued, ConnectPlan,
        ConnectRequest, ConnectionRole, ControlMessage, ControlWelcome, ENROLLMENT_ALPN,
        EnrollAccepted, EnrollRequest, EnrollmentMessage, ErrorMessage, MAX_CANDIDATES,
        MessageDirection, P2pHello, P2pMessage, P2pReady, PeerDescriptor, ProtocolError,
        RelayAccepted, RelayBind, RelayMessage, RelayReady, RenewCertificate,
        read_enrollment_message, read_message_from, read_p2p_message, read_relay_message,
        write_enrollment_message, write_message_for, write_p2p_message, write_relay_message,
    },
    registry::validate_node_id,
    routing::{PacketValidator, RouteError, SessionId},
    transport::{
        AuthenticatedConnection, BoxReadStream, BoxWriteStream, ClientTransportConfig,
        ConfiguredTransportFactory, ProtocolPurpose, TransportBackend, TransportConnection,
        TransportEndpoint, TransportError, TransportFactory, authenticate_connection,
    },
    tun::{
        NativeRouteManager, NativeTunFactory, PacketDevice, RouteManager, TunConfig, TunError,
        TunFactory,
    },
};

#[cfg(feature = "backend-quinn")]
use crate::transport::HybridQuinnEndpoint;

const DEFAULT_QUEUE_CAPACITY: usize = 256;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_RECONNECT_INITIAL: Duration = Duration::from_secs(1);
const DEFAULT_RECONNECT_MAX: Duration = Duration::from_secs(30);
const CONNECT_REQUEST_COOLDOWN: Duration = Duration::from_secs(10);
const RENEWAL_RETRY_DELAY: Duration = Duration::from_secs(60);
const RENEWAL_JITTER_SECONDS: i64 = 30 * 60;
const MAX_CONCURRENT_P2P_HANDSHAKES: usize = 32;
const P2P_CLOSE_CODE: u32 = 0x200;
const SESSION_CLOSE_CODE: u32 = 0x201;

#[derive(Clone, Copy)]
struct P2pHandshakeDeadline {
    expires_at: tokio::time::Instant,
}

impl P2pHandshakeDeadline {
    fn after(duration: Duration) -> Self {
        Self {
            expires_at: tokio::time::Instant::now() + duration,
        }
    }

    async fn run<T, F>(self, stage: &'static str, operation: F) -> Result<T, AgentRuntimeError>
    where
        F: Future<Output = Result<T, AgentRuntimeError>>,
    {
        timeout_at(self.expires_at, operation)
            .await
            .map_err(|_| AgentRuntimeError::Timeout(stage))?
    }
}

struct AuthenticatedP2pHandshake {
    connection: AuthenticatedConnection,
    verified: VerifiedNodeCertificate,
    direction: ConnectionDirection,
    deadline: P2pHandshakeDeadline,
}

#[derive(Clone)]
pub struct AgentRuntimeConfig {
    pub node_id: String,
    pub tun_name: Option<String>,
    pub identity_directory: PathBuf,
    pub enrollment_token: Option<String>,
    pub deployment_ca_pem: String,
    pub server_name: String,
    pub enrollment_address: SocketAddr,
    pub control_address: SocketAddr,
    pub relay_address: SocketAddr,
    pub p2p_bind_address: SocketAddr,
    pub p2p_idle_timeout: Duration,
    pub queue_capacity: usize,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub reconnect_initial: Duration,
    pub reconnect_max: Duration,
}

impl AgentRuntimeConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: impl Into<String>,
        identity_directory: impl Into<PathBuf>,
        deployment_ca_pem: impl Into<String>,
        server_name: impl Into<String>,
        enrollment_address: SocketAddr,
        control_address: SocketAddr,
        relay_address: SocketAddr,
        p2p_bind_address: SocketAddr,
    ) -> Self {
        Self {
            node_id: node_id.into(),
            tun_name: None,
            identity_directory: identity_directory.into(),
            enrollment_token: None,
            deployment_ca_pem: deployment_ca_pem.into(),
            server_name: server_name.into(),
            enrollment_address,
            control_address,
            relay_address,
            p2p_bind_address,
            p2p_idle_timeout: crate::peer_manager::DEFAULT_P2P_IDLE_TIMEOUT,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            reconnect_initial: DEFAULT_RECONNECT_INITIAL,
            reconnect_max: DEFAULT_RECONNECT_MAX,
        }
    }

    pub fn validate(&self) -> Result<(), AgentRuntimeError> {
        validate_node_id(&self.node_id)
            .map_err(|_| AgentRuntimeError::Configuration("invalid node ID".to_owned()))?;
        if self.identity_directory.as_os_str().is_empty() {
            return Err(AgentRuntimeError::Configuration(
                "identity directory must not be empty".to_owned(),
            ));
        }
        if self.deployment_ca_pem.is_empty() {
            return Err(AgentRuntimeError::Configuration(
                "deployment CA must not be empty".to_owned(),
            ));
        }
        if self.server_name.is_empty()
            || self.server_name.len() > 253
            || !self.server_name.is_ascii()
        {
            return Err(AgentRuntimeError::Configuration(
                "server name must be non-empty ASCII and at most 253 bytes".to_owned(),
            ));
        }
        let addresses = [
            self.enrollment_address,
            self.control_address,
            self.relay_address,
        ];
        if addresses.iter().any(|address| {
            address.port() == 0
                || address.ip().is_unspecified()
                || address.ip().is_multicast()
                || matches!(address.ip(), IpAddr::V4(ip) if ip.is_broadcast())
        }) {
            return Err(AgentRuntimeError::Configuration(
                "coordinator addresses must be concrete unicast sockets".to_owned(),
            ));
        }
        if self.p2p_bind_address.port() == 0
            || !self.p2p_bind_address.is_ipv4()
            || self.p2p_bind_address.ip().is_multicast()
            || matches!(self.p2p_bind_address.ip(), IpAddr::V4(ip) if ip.is_broadcast())
        {
            return Err(AgentRuntimeError::Configuration(
                "P2P bind must be an IPv4 unicast socket with a non-zero port".to_owned(),
            ));
        }
        if self.queue_capacity == 0 {
            return Err(AgentRuntimeError::Configuration(
                "runtime queue capacity must be non-zero".to_owned(),
            ));
        }
        for (name, value) in [
            ("connect timeout", self.connect_timeout),
            ("handshake timeout", self.handshake_timeout),
            ("initial reconnect delay", self.reconnect_initial),
            ("maximum reconnect delay", self.reconnect_max),
        ] {
            if value.is_zero() {
                return Err(AgentRuntimeError::Configuration(format!(
                    "{name} must be non-zero"
                )));
            }
        }
        if self.reconnect_initial > self.reconnect_max {
            return Err(AgentRuntimeError::Configuration(
                "initial reconnect delay exceeds maximum reconnect delay".to_owned(),
            ));
        }
        if !(crate::peer_manager::MIN_P2P_IDLE_TIMEOUT..=crate::peer_manager::MAX_P2P_IDLE_TIMEOUT)
            .contains(&self.p2p_idle_timeout)
        {
            return Err(AgentRuntimeError::Configuration(
                "P2P idle timeout is outside the supported range".to_owned(),
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for AgentRuntimeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentRuntimeConfig")
            .field("node_id", &self.node_id)
            .field("tun_name", &self.tun_name)
            .field("identity_directory", &self.identity_directory)
            .field(
                "enrollment_token",
                &self.enrollment_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("deployment_ca_pem", &"[PEM REDACTED]")
            .field("server_name", &self.server_name)
            .field("enrollment_address", &self.enrollment_address)
            .field("control_address", &self.control_address)
            .field("relay_address", &self.relay_address)
            .field("p2p_bind_address", &self.p2p_bind_address)
            .field("p2p_idle_timeout", &self.p2p_idle_timeout)
            .field("queue_capacity", &self.queue_capacity)
            .finish_non_exhaustive()
    }
}

pub struct AgentRuntime {
    config: Arc<AgentRuntimeConfig>,
    transport_factory: Arc<dyn TransportFactory>,
    tun_factory: Arc<dyn TunFactory>,
    route_manager: Arc<dyn RouteManager>,
    metrics: Arc<RuntimeMetrics>,
}

impl AgentRuntime {
    pub fn new(config: AgentRuntimeConfig) -> Result<Self, AgentRuntimeError> {
        Self::with_dependencies(
            config,
            Arc::new(ConfiguredTransportFactory::new(TransportBackend::Quinn)),
            Arc::new(NativeTunFactory),
            Arc::new(NativeRouteManager),
        )
    }

    pub fn with_dependencies(
        config: AgentRuntimeConfig,
        transport_factory: Arc<dyn TransportFactory>,
        tun_factory: Arc<dyn TunFactory>,
        route_manager: Arc<dyn RouteManager>,
    ) -> Result<Self, AgentRuntimeError> {
        config.validate()?;
        if transport_factory.backend() != TransportBackend::Quinn {
            return Err(AgentRuntimeError::Configuration(
                "Stellaris v2 Agent runtime requires Quinn".to_owned(),
            ));
        }
        Ok(Self {
            config: Arc::new(config),
            transport_factory,
            tun_factory,
            route_manager,
            metrics: Arc::new(RuntimeMetrics::default()),
        })
    }

    pub fn metrics(&self) -> Arc<RuntimeMetrics> {
        Arc::clone(&self.metrics)
    }

    pub async fn run(&self) -> Result<(), AgentRuntimeError> {
        self.run_until(pending::<()>()).await
    }

    pub async fn run_until<F>(&self, shutdown: F) -> Result<(), AgentRuntimeError>
    where
        F: Future<Output = ()> + Send,
    {
        #[cfg(feature = "backend-quinn")]
        {
            self.run_quinn(shutdown).await
        }
        #[cfg(not(feature = "backend-quinn"))]
        {
            let _ = shutdown;
            Err(AgentRuntimeError::QuinnUnavailable)
        }
    }

    #[cfg(feature = "backend-quinn")]
    async fn run_quinn<F>(&self, shutdown: F) -> Result<(), AgentRuntimeError>
    where
        F: Future<Output = ()> + Send,
    {
        let identity_store = Arc::new(NodeIdentityStore::load_or_create(
            &self.config.identity_directory,
        )?);
        let identity = self.ensure_identity(&identity_store).await?;
        let first_control = self.prepare_control(&identity).await?;
        let network = AgentNetwork::from_welcome(&first_control.welcome)?;
        validate_installed_identity(&self.config, &identity, network)?;
        let first_relay = self
            .prepare_relay(&identity, &first_control.welcome, network)
            .await?;
        let private_key_pem = read_private_key(&identity_store)?;
        let p2p_endpoint = Arc::new(HybridQuinnEndpoint::bind(
            self.config.p2p_bind_address,
            identity.node_certificate_pem(),
            &private_key_pem,
            identity.node_ca_pem(),
        )?);

        let tun_config = TunConfig {
            name: self.config.tun_name.clone(),
            address: identity.overlay_ip,
            overlay: network.overlay,
            mtu: network.mtu,
        };
        let device = self.tun_factory.create(&tun_config).await?;
        let interface_name = device.name().to_owned();
        let route = self
            .route_manager
            .install_overlay_route(&tun_config, &interface_name)
            .await?;

        let validator = PacketValidator::new(network.overlay, usize::from(network.mtu))?;
        let peer_manager = Arc::new(PeerManager::new(
            self.config.node_id.clone(),
            network.overlay,
            crate::peer_manager::DEFAULT_MAX_MANAGED_PEERS,
            self.config.p2p_idle_timeout,
        )?);
        let (tun_sender, tun_receiver) = mpsc::channel(self.config.queue_capacity);
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let endpoint: Arc<dyn TransportEndpoint> = p2p_endpoint.clone();
        let shared = Arc::new(RuntimeShared::new(
            Arc::clone(&self.config),
            network,
            validator,
            peer_manager,
            endpoint,
            identity_store,
            tun_sender,
            Arc::clone(&self.metrics),
        ));

        let mut tun_task = tokio::spawn(run_tun(
            device,
            tun_receiver,
            Arc::clone(&shared),
            shutdown_receiver.clone(),
        ));
        let mut accept_task = tokio::spawn(run_p2p_acceptor(
            Arc::clone(&shared),
            shutdown_receiver.clone(),
        ));
        let mut reaper_task = tokio::spawn(run_p2p_reaper(Arc::clone(&shared), shutdown_receiver));

        tokio::pin!(shutdown);
        let mut reconnect_delay = Duration::ZERO;
        let mut terminal_error = None;
        let mut prepared_session = Some(PreparedSession {
            control: first_control,
            relay: Some(first_relay),
        });
        loop {
            if !reconnect_delay.is_zero() {
                let delay = sleep(reconnect_delay);
                tokio::pin!(delay);
                tokio::select! {
                    () = &mut shutdown => break,
                    result = &mut tun_task => {
                        terminal_error = Some(join_runtime_task("TUN", result));
                        break;
                    }
                    result = &mut accept_task => {
                        terminal_error = Some(join_runtime_task("P2P acceptor", result));
                        break;
                    }
                    result = &mut reaper_task => {
                        terminal_error = Some(join_runtime_task("P2P reaper", result));
                        break;
                    }
                    () = &mut delay => {}
                }
            }

            let current = shared
                .identity_store
                .installed_identity()
                .ok_or(AgentRuntimeError::MissingInstalledIdentity)?;
            if !current.is_valid_at(OffsetDateTime::now_utc()) {
                terminal_error = Some(AgentRuntimeError::CertificateExpired);
                break;
            }
            let session = self.run_coordination_session(
                Arc::clone(&shared),
                current.clone(),
                prepared_session.take(),
            );
            tokio::pin!(session);
            let outcome = tokio::select! {
                () = &mut shutdown => break,
                result = &mut tun_task => {
                    terminal_error = Some(join_runtime_task("TUN", result));
                    break;
                }
                result = &mut accept_task => {
                    terminal_error = Some(join_runtime_task("P2P acceptor", result));
                    break;
                }
                result = &mut reaper_task => {
                    terminal_error = Some(join_runtime_task("P2P reaper", result));
                    break;
                }
                result = &mut session => result,
            };

            shared.end_control_lease();
            match outcome {
                Ok(SessionExit::CredentialsRotated(replacement_session)) => {
                    let rotation = async {
                        let replacement = shared
                            .identity_store
                            .installed_identity()
                            .ok_or(AgentRuntimeError::MissingInstalledIdentity)?;
                        let key = read_private_key(&shared.identity_store)?;
                        p2p_endpoint
                            .rotate_identity(replacement.node_certificate_pem(), &key)
                            .await?;
                        Ok::<(), AgentRuntimeError>(())
                    }
                    .await;
                    if let Err(error) = rotation {
                        terminal_error = Some(error);
                        break;
                    }
                    prepared_session = Some(replacement_session);
                    reconnect_delay = Duration::ZERO;
                }
                Ok(SessionExit::Reconnect) => {
                    reconnect_delay = next_reconnect_delay(
                        reconnect_delay,
                        self.config.reconnect_initial,
                        self.config.reconnect_max,
                    );
                }
                Err(error) => {
                    if matches!(error, AgentRuntimeError::CertificateExpired) {
                        terminal_error = Some(error);
                        break;
                    }
                    tracing::warn!(error = %error, "coordination session ended; reconnecting");
                    reconnect_delay = next_reconnect_delay(
                        reconnect_delay,
                        self.config.reconnect_initial,
                        self.config.reconnect_max,
                    );
                }
            }
        }

        let _ = shutdown_sender.send(true);
        shared.shutdown_all();
        p2p_endpoint.close().await;
        let _ = timeout(Duration::from_secs(2), async {
            let _ = (&mut tun_task).await;
            let _ = (&mut accept_task).await;
            let _ = (&mut reaper_task).await;
        })
        .await;
        self.route_manager.remove_overlay_route(route).await?;
        if let Some(error) = terminal_error {
            return Err(error);
        }
        Ok(())
    }

    async fn ensure_identity(
        &self,
        store: &Arc<NodeIdentityStore>,
    ) -> Result<InstalledNodeIdentity, AgentRuntimeError> {
        if let Some(identity) = store.installed_identity()
            && identity.node_id == self.config.node_id
            && identity.is_valid_at(OffsetDateTime::now_utc())
        {
            if let Some(pending) = store.pending_enrollment() {
                if pending.node_id != identity.node_id {
                    return Err(NodeIdentityStoreError::PendingEnrollmentConflict.into());
                }
                store.clear_pending_enrollment(&pending.enrollment_id)?;
            }
            return Ok(identity);
        }
        let result = self.enroll(store).await;
        self.metrics.record_enrollment(result.is_ok());
        result
    }

    async fn enroll(
        &self,
        store: &Arc<NodeIdentityStore>,
    ) -> Result<InstalledNodeIdentity, AgentRuntimeError> {
        let token = self
            .config
            .enrollment_token
            .as_deref()
            .ok_or(AgentRuntimeError::EnrollmentTokenRequired)?;
        let pending = store.prepare_enrollment(&self.config.node_id, token)?;
        let request = EnrollRequest {
            enrollment_id: pending.enrollment_id.clone(),
            node_id: pending.node_id.clone(),
            enrollment_token: pending.enrollment_token().to_owned(),
            csr_der_base64: STANDARD.encode(pending.csr_der()?),
        };
        let endpoint = self
            .transport_factory
            .client_endpoint(ClientTransportConfig::new(
                SocketAddr::from(([0, 0, 0, 0], 0)),
                self.config.deployment_ca_pem.clone(),
                ProtocolPurpose::Enrollment,
            ))
            .await?;
        let connection = timeout(
            self.config.connect_timeout,
            endpoint.connect(self.config.enrollment_address, &self.config.server_name),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("enrollment connect"))??;
        if connection.negotiated_alpn().await?.as_slice() != ENROLLMENT_ALPN {
            connection.close(SESSION_CLOSE_CODE, b"wrong enrollment ALPN");
            return Err(AgentRuntimeError::Transport(TransportError::AlpnMismatch));
        }
        let (mut reader, mut writer) = connection.open_bi().await?;
        timeout(
            self.config.handshake_timeout,
            write_enrollment_message(
                &mut writer,
                &EnrollmentMessage::EnrollRequest(request),
                MessageDirection::AgentToCoordinator,
            ),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("enrollment request"))??;
        let response = timeout(
            self.config.handshake_timeout,
            read_enrollment_message(&mut reader, MessageDirection::CoordinatorToAgent),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("enrollment response"))??;
        connection.close(0, b"enrollment complete");
        let accepted = match response {
            EnrollmentMessage::EnrollAccepted(accepted) => accepted,
            EnrollmentMessage::Error(error) => return Err(remote_error("enrollment", error)),
            EnrollmentMessage::EnrollRequest(_) => {
                return Err(AgentRuntimeError::UnexpectedMessage("EnrollRequest"));
            }
        };
        let installed =
            install_enrollment(store, &pending.enrollment_id, &pending.node_id, accepted)?;
        Ok(installed)
    }

    async fn prepare_control(
        &self,
        identity: &InstalledNodeIdentity,
    ) -> Result<PreparedControl, AgentRuntimeError> {
        let key = read_private_key_path(&self.config.identity_directory.join("node-key.pem"))?;
        let endpoint = self
            .transport_factory
            .client_endpoint(
                ClientTransportConfig::new(
                    SocketAddr::from(([0, 0, 0, 0], 0)),
                    self.config.deployment_ca_pem.clone(),
                    ProtocolPurpose::Control,
                )
                .with_client_identity(identity.node_certificate_pem(), key),
            )
            .await?;
        let connection = timeout(
            self.config.connect_timeout,
            endpoint.connect(self.config.control_address, &self.config.server_name),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("control connect"))??;
        let connection = authenticate_connection(connection, ProtocolPurpose::Control).await?;
        let (mut reader, writer) = connection.open_bi().await?;
        let welcome = timeout(
            self.config.handshake_timeout,
            read_message_from(&mut reader, MessageDirection::CoordinatorToAgent),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("ControlWelcome"))??;
        let ControlMessage::ControlWelcome(welcome) = welcome else {
            connection.close(SESSION_CLOSE_CODE, b"ControlWelcome required");
            return Err(AgentRuntimeError::UnexpectedMessage("ControlWelcome"));
        };
        validate_control_welcome(&self.config, identity, &welcome)?;
        Ok(PreparedControl {
            connection,
            reader,
            writer,
            welcome,
        })
    }

    async fn prepare_relay(
        &self,
        identity: &InstalledNodeIdentity,
        welcome: &ControlWelcome,
        network: AgentNetwork,
    ) -> Result<AuthenticatedConnection, AgentRuntimeError> {
        let key = read_private_key_path(&self.config.identity_directory.join("node-key.pem"))?;
        let endpoint = self
            .transport_factory
            .client_endpoint(
                ClientTransportConfig::new(
                    SocketAddr::from(([0, 0, 0, 0], 0)),
                    self.config.deployment_ca_pem.clone(),
                    ProtocolPurpose::Relay,
                )
                .with_client_identity(identity.node_certificate_pem(), key),
            )
            .await?;
        let connection = timeout(
            self.config.connect_timeout,
            endpoint.connect(self.config.relay_address, &self.config.server_name),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("Relay connect"))??;
        let connection = authenticate_connection(connection, ProtocolPurpose::Relay).await?;
        let (mut reader, mut writer) = connection.open_bi().await?;
        timeout(
            self.config.handshake_timeout,
            write_relay_message(
                &mut writer,
                &RelayMessage::RelayBind(RelayBind {
                    control_session_id: welcome.session_id.clone(),
                    incarnation: welcome.incarnation,
                }),
                MessageDirection::AgentToCoordinator,
            ),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("RelayBind"))??;
        let accepted = timeout(
            self.config.handshake_timeout,
            read_relay_message(&mut reader, MessageDirection::CoordinatorToAgent),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("RelayAccepted"))??;
        let RelayMessage::RelayAccepted(RelayAccepted {
            relay_session_id,
            mtu,
            max_datagram_size,
        }) = accepted
        else {
            connection.close(SESSION_CLOSE_CODE, b"RelayAccepted required");
            return Err(AgentRuntimeError::UnexpectedMessage("RelayAccepted"));
        };
        if mtu != network.mtu || max_datagram_size < mtu {
            connection.close(SESSION_CLOSE_CODE, b"Relay MTU mismatch");
            return Err(AgentRuntimeError::NetworkParametersChanged);
        }
        let ready = RelayMessage::RelayReady(RelayReady { relay_session_id });
        timeout(
            self.config.handshake_timeout,
            write_relay_message(&mut writer, &ready, MessageDirection::AgentToCoordinator),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("RelayReady"))??;
        let confirmed = timeout(
            self.config.handshake_timeout,
            read_relay_message(&mut reader, MessageDirection::CoordinatorToAgent),
        )
        .await
        .map_err(|_| AgentRuntimeError::Timeout("RelayReady confirmation"))??;
        if confirmed != ready {
            connection.close(SESSION_CLOSE_CODE, b"RelayReady confirmation required");
            return Err(AgentRuntimeError::UnexpectedMessage(
                "RelayReady confirmation",
            ));
        }
        Ok(connection)
    }

    async fn run_coordination_session(
        &self,
        shared: Arc<RuntimeShared>,
        identity: InstalledNodeIdentity,
        prepared: Option<PreparedSession>,
    ) -> Result<SessionExit, AgentRuntimeError> {
        let (prepared_control, prepared_relay) = match prepared {
            Some(PreparedSession { control, relay }) => (control, relay),
            None => (self.prepare_control(&identity).await?, None),
        };
        let PreparedControl {
            connection: control_connection,
            reader,
            mut writer,
            welcome,
        } = prepared_control;
        if AgentNetwork::from_welcome(&welcome)? != shared.network {
            control_connection.close(SESSION_CLOSE_CODE, b"network parameters changed");
            return Err(AgentRuntimeError::NetworkParametersChanged);
        }
        let local = LocalControlIdentity {
            session_id: welcome.session_id.clone(),
            incarnation: welcome.incarnation,
            certificate_fingerprint: certificate_fingerprint_from_pem(
                identity.node_certificate_pem(),
            )?,
        };
        let candidates = host_candidates(
            shared.p2p_endpoint.local_address().await?,
            self.config.control_address,
        );
        if !candidates.is_empty() {
            write_message_for(
                &mut writer,
                &ControlMessage::AnnounceCandidates(AnnounceCandidates {
                    epoch: 1,
                    candidates,
                }),
                MessageDirection::AgentToCoordinator,
            )
            .await?;
        }

        let relay_connection = match prepared_relay {
            Some(connection) => connection,
            None => {
                self.prepare_relay(&identity, &welcome, shared.network)
                    .await?
            }
        };
        let (control_sender, control_receiver) = mpsc::channel(self.config.queue_capacity);
        let (rotation_sender, mut rotation_receiver) = mpsc::channel(1);
        let control_handle_id = shared.install_control(control_sender);
        let (relay_sender, relay_receiver) = mpsc::channel(self.config.queue_capacity);
        let (relay_close_sender, relay_close_receiver) = watch::channel(false);
        let relay_handle_id = shared.install_relay(relay_sender, relay_close_sender);
        let mut relay_task = tokio::spawn(run_relay_connection(
            relay_connection,
            relay_receiver,
            relay_close_receiver,
            Arc::clone(&shared),
        ));
        let mut control_task = tokio::spawn(run_control_stream(
            reader,
            writer,
            control_receiver,
            Arc::clone(&shared),
            identity.clone(),
            local,
            rotation_sender,
        ));
        self.metrics.set_control_sessions(1);
        self.metrics.set_relay_sessions(1);

        let expiry = instant_for(identity.not_after()?);
        let result = tokio::select! {
            result = &mut control_task => match result {
                Ok(Ok(())) => Ok(SessionExit::Reconnect),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(AgentRuntimeError::TaskJoin {
                    task: "control session",
                    source: error,
                }),
            },
            result = &mut relay_task => match result {
                Ok(Ok(())) => Ok(SessionExit::Reconnect),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(AgentRuntimeError::TaskJoin {
                    task: "Relay session",
                    source: error,
                }),
            },
            replacement = rotation_receiver.recv() => {
                let replacement = replacement
                    .ok_or(AgentRuntimeError::RuntimeTaskStopped("certificate rotation"))?;
                let prepared = self.prepare_replacement_session(
                    &shared,
                    replacement,
                    identity.not_after()?,
                ).await?;
                Ok(SessionExit::CredentialsRotated(prepared))
            },
            () = sleep_until(expiry) => Err(AgentRuntimeError::CertificateExpired),
        };

        control_connection.close(SESSION_CLOSE_CODE, b"control session replaced");
        shared.clear_control(control_handle_id);
        shared.clear_relay(relay_handle_id);
        self.metrics.set_control_sessions(0);
        self.metrics.set_relay_sessions(0);
        control_task.abort();
        relay_task.abort();
        result
    }

    async fn prepare_replacement_session(
        &self,
        shared: &RuntimeShared,
        identity: InstalledNodeIdentity,
        old_not_after: OffsetDateTime,
    ) -> Result<PreparedSession, AgentRuntimeError> {
        let mut delay = self.config.reconnect_initial;
        loop {
            if OffsetDateTime::now_utc() >= old_not_after
                || !identity.is_valid_at(OffsetDateTime::now_utc())
            {
                return Err(AgentRuntimeError::CertificateExpired);
            }
            match self.prepare_control(&identity).await {
                Ok(control) => {
                    if AgentNetwork::from_welcome(&control.welcome)? != shared.network {
                        control.connection.close(
                            SESSION_CLOSE_CODE,
                            b"replacement network parameters changed",
                        );
                        return Err(AgentRuntimeError::NetworkParametersChanged);
                    }
                    match self
                        .prepare_relay(&identity, &control.welcome, shared.network)
                        .await
                    {
                        Ok(relay) => {
                            return Ok(PreparedSession {
                                control,
                                relay: Some(relay),
                            });
                        }
                        Err(error) => {
                            control
                                .connection
                                .close(SESSION_CLOSE_CODE, b"replacement Relay was not ready");
                            tracing::warn!(error = %error,
                                "replacement Relay failed; retaining old session while retrying");
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error,
                        "replacement control failed; retaining old session while retrying");
                }
            }
            sleep(delay).await;
            delay = next_reconnect_delay(
                delay,
                self.config.reconnect_initial,
                self.config.reconnect_max,
            );
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AgentNetwork {
    overlay: Ipv4Net,
    mtu: u16,
}

impl AgentNetwork {
    fn from_welcome(welcome: &ControlWelcome) -> Result<Self, AgentRuntimeError> {
        PacketValidator::new(welcome.overlay_cidr, usize::from(welcome.mtu))?;
        if !is_overlay_unicast(welcome.overlay_cidr, welcome.overlay_ip) {
            return Err(AgentRuntimeError::InvalidControlWelcome);
        }
        Ok(Self {
            overlay: welcome.overlay_cidr,
            mtu: welcome.mtu,
        })
    }
}

struct PreparedControl {
    connection: AuthenticatedConnection,
    reader: BoxReadStream,
    writer: BoxWriteStream,
    welcome: ControlWelcome,
}

struct PreparedSession {
    control: PreparedControl,
    relay: Option<AuthenticatedConnection>,
}

#[derive(Clone, Debug)]
struct LocalControlIdentity {
    session_id: String,
    incarnation: u64,
    certificate_fingerprint: String,
}

#[derive(Clone)]
struct ControlHandle {
    id: u64,
    sender: mpsc::Sender<ControlMessage>,
}

#[derive(Clone)]
struct RelayHandle {
    id: u64,
    sender: mpsc::Sender<Bytes>,
    close: watch::Sender<bool>,
}

#[derive(Clone)]
struct P2pHandle {
    sender: mpsc::Sender<Bytes>,
    close: watch::Sender<bool>,
}

#[derive(Clone, Copy)]
struct P2pActorLease {
    overlay_ip: Ipv4Addr,
    generation: ProbeGeneration,
    connection_id: PeerConnectionId,
    peer_certificate_not_after: OffsetDateTime,
}

#[derive(Clone)]
struct PendingPlan {
    plan: ConnectPlan,
    generation: ProbeGeneration,
    local: LocalControlIdentity,
}

struct RuntimeShared {
    config: Arc<AgentRuntimeConfig>,
    network: AgentNetwork,
    validator: PacketValidator,
    peer_manager: Arc<PeerManager>,
    p2p_endpoint: Arc<dyn TransportEndpoint>,
    identity_store: Arc<NodeIdentityStore>,
    tun_sender: mpsc::Sender<Bytes>,
    metrics: Arc<RuntimeMetrics>,
    next_handle_id: AtomicU64,
    next_request_id: AtomicU64,
    control: Mutex<Option<ControlHandle>>,
    relay: Mutex<Option<RelayHandle>>,
    p2p: Mutex<HashMap<PeerConnectionId, P2pHandle>>,
    plans: Mutex<HashMap<String, PendingPlan>>,
    connect_requests: Mutex<HashMap<Ipv4Addr, Instant>>,
    p2p_handshake_gate: Arc<Semaphore>,
}

impl RuntimeShared {
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: Arc<AgentRuntimeConfig>,
        network: AgentNetwork,
        validator: PacketValidator,
        peer_manager: Arc<PeerManager>,
        p2p_endpoint: Arc<dyn TransportEndpoint>,
        identity_store: Arc<NodeIdentityStore>,
        tun_sender: mpsc::Sender<Bytes>,
        metrics: Arc<RuntimeMetrics>,
    ) -> Self {
        Self {
            config,
            network,
            validator,
            peer_manager,
            p2p_endpoint,
            identity_store,
            tun_sender,
            metrics,
            next_handle_id: AtomicU64::new(1),
            next_request_id: AtomicU64::new(1),
            control: Mutex::new(None),
            relay: Mutex::new(None),
            p2p: Mutex::new(HashMap::new()),
            plans: Mutex::new(HashMap::new()),
            connect_requests: Mutex::new(HashMap::new()),
            p2p_handshake_gate: Arc::new(Semaphore::new(MAX_CONCURRENT_P2P_HANDSHAKES)),
        }
    }

    fn install_control(&self, sender: mpsc::Sender<ControlMessage>) -> u64 {
        let id = self.next_handle_id();
        *lock(&self.control) = Some(ControlHandle { id, sender });
        id
    }

    fn clear_control(&self, id: u64) {
        let mut control = lock(&self.control);
        if control.as_ref().is_some_and(|handle| handle.id == id) {
            *control = None;
        }
    }

    fn install_relay(&self, sender: mpsc::Sender<Bytes>, close: watch::Sender<bool>) -> u64 {
        let id = self.next_handle_id();
        if let Some(previous) = lock(&self.relay).replace(RelayHandle { id, sender, close }) {
            let _ = previous.close.send(true);
        }
        id
    }

    fn clear_relay(&self, id: u64) {
        let mut relay = lock(&self.relay);
        if relay.as_ref().is_some_and(|handle| handle.id == id)
            && let Some(handle) = relay.take()
        {
            let _ = handle.close.send(true);
        }
    }

    fn end_control_lease(&self) {
        if let Some(relay) = lock(&self.relay).take() {
            let _ = relay.close.send(true);
        }
        *lock(&self.control) = None;
        let plans: Vec<_> = lock(&self.plans).drain().map(|(_, plan)| plan).collect();
        for pending in plans {
            let _ = self
                .peer_manager
                .probe_failed(pending.plan.peer.overlay_ip, pending.generation);
        }
        lock(&self.connect_requests).clear();
    }

    fn shutdown_all(&self) {
        self.end_control_lease();
        let handles: Vec<_> = lock(&self.p2p).drain().map(|(_, handle)| handle).collect();
        for handle in handles {
            let _ = handle.close.send(true);
        }
        for snapshot in self.peer_manager.snapshots() {
            let _ = self.peer_manager.forget_peer(snapshot.overlay_ip);
        }
        self.metrics.set_p2p_sessions(0);
    }

    fn dispatch_outbound(&self, packet: Bytes) {
        let metadata = match self.validator.validate(&packet, self.local_overlay_ip()) {
            Ok(metadata) if metadata.destination != self.local_overlay_ip() => metadata,
            Ok(_) | Err(_) => {
                self.metrics
                    .record_drop_reason(PacketDropReason::InvalidPacket);
                return;
            }
        };
        let destination = metadata.destination;
        let decision = self.peer_manager.select_path_at(
            destination,
            Instant::now(),
            unix_now().unwrap_or(u64::MAX),
        );
        if let Some(ConnectionAction::Close { connection_id, .. }) = decision.action {
            self.close_p2p(connection_id);
            self.metrics.record_path_transition();
        }
        let p2p_sender = match decision.selected {
            SelectedPath::P2p(connection_id) => lock(&self.p2p)
                .get(&connection_id)
                .map(|handle| handle.sender.clone()),
            SelectedPath::Relay => None,
        };
        let relay_sender = lock(&self.relay)
            .as_ref()
            .map(|handle| handle.sender.clone());
        match enqueue_selected(decision.selected, packet, p2p_sender, relay_sender) {
            EnqueueOutcome::P2pQueued(depth) => {
                self.metrics.record_p2p_packet();
                self.metrics.observe_p2p_queue_depth(depth);
            }
            EnqueueOutcome::RelayQueued(depth) => {
                self.metrics.record_relay_packet();
                self.metrics.observe_relay_queue_depth(depth);
            }
            EnqueueOutcome::P2pFailed(connection_id, reason) => {
                self.fail_p2p(destination, connection_id);
                self.metrics.record_drop_reason(reason);
            }
            EnqueueOutcome::RelayFailed(reason) => self.metrics.record_drop_reason(reason),
        }
        if decision.selected == SelectedPath::Relay {
            self.request_connect(destination);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnqueueOutcome {
    P2pQueued(usize),
    RelayQueued(usize),
    P2pFailed(PeerConnectionId, PacketDropReason),
    RelayFailed(PacketDropReason),
}

fn enqueue_selected(
    selected: SelectedPath,
    packet: Bytes,
    p2p_sender: Option<mpsc::Sender<Bytes>>,
    relay_sender: Option<mpsc::Sender<Bytes>>,
) -> EnqueueOutcome {
    match selected {
        SelectedPath::P2p(connection_id) => match try_enqueue(p2p_sender, packet) {
            Ok(depth) => EnqueueOutcome::P2pQueued(depth),
            Err(reason) => EnqueueOutcome::P2pFailed(connection_id, reason),
        },
        SelectedPath::Relay => match try_enqueue(relay_sender, packet) {
            Ok(depth) => EnqueueOutcome::RelayQueued(depth),
            Err(reason) => EnqueueOutcome::RelayFailed(reason),
        },
    }
}

fn try_enqueue(
    sender: Option<mpsc::Sender<Bytes>>,
    packet: Bytes,
) -> Result<usize, PacketDropReason> {
    let Some(sender) = sender else {
        return Err(PacketDropReason::UnavailablePath);
    };
    match sender.try_send(packet) {
        Ok(()) => Ok(sender.max_capacity().saturating_sub(sender.capacity())),
        Err(mpsc::error::TrySendError::Full(_)) => Err(PacketDropReason::QueueFull),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(PacketDropReason::UnavailablePath),
    }
}

fn try_send_drop_reason<T>(error: &mpsc::error::TrySendError<T>) -> PacketDropReason {
    match error {
        mpsc::error::TrySendError::Full(_) => PacketDropReason::QueueFull,
        mpsc::error::TrySendError::Closed(_) => PacketDropReason::UnavailablePath,
    }
}

impl RuntimeShared {
    fn request_connect(&self, destination: Ipv4Addr) {
        let now = Instant::now();
        {
            let mut requests = lock(&self.connect_requests);
            if requests
                .get(&destination)
                .is_some_and(|last| now.saturating_duration_since(*last) < CONNECT_REQUEST_COOLDOWN)
            {
                return;
            }
            if requests.len() >= crate::peer_manager::MAX_MANAGED_PEERS
                && !requests.contains_key(&destination)
            {
                return;
            }
            requests.insert(destination, now);
        }
        let sender = lock(&self.control)
            .as_ref()
            .map(|handle| handle.sender.clone());
        if let Some(sender) = sender
            && sender
                .try_send(ControlMessage::ConnectRequest(ConnectRequest {
                    request_id: self.next_request_id(),
                    overlay_ip: destination,
                }))
                .is_ok()
        {
            self.metrics.observe_control_queue_depth(
                sender.max_capacity().saturating_sub(sender.capacity()),
            );
        }
    }

    fn install_p2p(&self, id: PeerConnectionId, handle: P2pHandle) {
        if let Some(previous) = lock(&self.p2p).insert(id, handle) {
            let _ = previous.close.send(true);
        }
        self.metrics.set_p2p_sessions(lock(&self.p2p).len());
        self.metrics.record_path_transition();
    }

    fn close_p2p(&self, id: PeerConnectionId) {
        if let Some(handle) = lock(&self.p2p).remove(&id) {
            let _ = handle.close.send(true);
        }
        self.metrics.set_p2p_sessions(lock(&self.p2p).len());
    }

    fn fail_p2p(&self, overlay_ip: Ipv4Addr, id: PeerConnectionId) {
        if let Some(snapshot) = self.peer_manager.snapshot(overlay_ip)
            && let PeerPathState::P2pReady {
                generation,
                connection_id,
                ..
            } = snapshot.path
            && connection_id == id
        {
            let _ = self
                .peer_manager
                .connection_closed(overlay_ip, generation, id);
        }
        self.close_p2p(id);
        self.metrics.record_path_transition();
    }

    fn local_overlay_ip(&self) -> Ipv4Addr {
        self.identity_store
            .installed_identity()
            .map(|identity| identity.overlay_ip)
            .expect("runtime identity is installed for the TUN lifetime")
    }

    fn next_handle_id(&self) -> u64 {
        next_nonzero(&self.next_handle_id)
    }

    fn next_request_id(&self) -> u64 {
        next_nonzero(&self.next_request_id)
    }
}

async fn run_tun(
    mut device: Box<dyn PacketDevice>,
    mut inbound: mpsc::Receiver<Bytes>,
    shared: Arc<RuntimeShared>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), AgentRuntimeError> {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            packet = device.read_packet() => {
                shared.dispatch_outbound(packet?);
            }
            packet = inbound.recv() => {
                let Some(packet) = packet else {
                    return Ok(());
                };
                device.write_packet(packet).await?;
            }
        }
    }
}

async fn run_relay_connection(
    mut connection: AuthenticatedConnection,
    mut outbound: mpsc::Receiver<Bytes>,
    mut close: watch::Receiver<bool>,
    shared: Arc<RuntimeShared>,
) -> Result<(), AgentRuntimeError> {
    loop {
        tokio::select! {
            changed = close.changed() => {
                if changed.is_err() || *close.borrow() {
                    connection.close(SESSION_CLOSE_CODE, b"Relay lease ended");
                    return Ok(());
                }
            }
            packet = outbound.recv() => {
                let Some(packet) = packet else {
                    connection.close(SESSION_CLOSE_CODE, b"Relay sender closed");
                    return Ok(());
                };
                if send_relay_datagram(connection.as_ref(), packet, &shared.metrics)? {
                    shared
                        .metrics
                        .observe_relay_queue_depth(outbound.len().saturating_add(1));
                }
            }
            packet = connection.recv_datagram() => {
                let packet = match packet {
                    Ok(packet) => packet,
                    Err(error) => {
                        shared.metrics.record_drop_reason(PacketDropReason::Transport);
                        return Err(error.into());
                    }
                };
                if shared
                    .validator
                    .validate_inbound(&packet, shared.local_overlay_ip())
                    .is_err()
                {
                    shared.metrics.record_drop_reason(PacketDropReason::InvalidPacket);
                    continue;
                }
                if let Err(error) = shared.tun_sender.try_send(packet) {
                    shared.metrics.record_drop_reason(try_send_drop_reason(&error));
                }
            }
        }
    }
}

/// Sends one Relay packet. Local Datagram backpressure and size rejection drop
/// only this packet; errors that invalidate the transport end the session.
fn send_relay_datagram(
    connection: &dyn TransportConnection,
    packet: Bytes,
    metrics: &RuntimeMetrics,
) -> Result<bool, AgentRuntimeError> {
    match connection.send_datagram(packet) {
        Ok(()) => Ok(true),
        Err(TransportError::DatagramQueueFull) => {
            metrics.record_drop_reason(PacketDropReason::QueueFull);
            Ok(false)
        }
        Err(TransportError::DatagramTooLarge) => {
            metrics.record_drop_reason(PacketDropReason::InvalidPacket);
            Ok(false)
        }
        Err(error) => {
            metrics.record_drop_reason(PacketDropReason::Transport);
            Err(error.into())
        }
    }
}

async fn run_control_stream(
    mut reader: BoxReadStream,
    mut writer: BoxWriteStream,
    mut outbound: mpsc::Receiver<ControlMessage>,
    shared: Arc<RuntimeShared>,
    identity: InstalledNodeIdentity,
    local: LocalControlIdentity,
    rotation_sender: mpsc::Sender<InstalledNodeIdentity>,
) -> Result<(), AgentRuntimeError> {
    let mut pending_renewal = None;
    let mut renew_at = renewal_instant(&identity)?;
    loop {
        let renewal_timer = sleep_until(renew_at);
        tokio::pin!(renewal_timer);
        tokio::select! {
            command = outbound.recv() => {
                let Some(command) = command else {
                    return Ok(());
                };
                write_message_for(
                    &mut writer,
                    &command,
                    MessageDirection::AgentToCoordinator,
                ).await?;
            }
            message = read_message_from(
                &mut reader,
                MessageDirection::CoordinatorToAgent,
            ) => {
                match message? {
                    ControlMessage::ConnectPlan(plan) => {
                        prepare_connect_plan(Arc::clone(&shared), local.clone(), plan)?;
                    }
                    ControlMessage::PeerRecord(record) => {
                        if let Some(id) = shared.peer_manager.update_peer_record(&record)? {
                            shared.close_p2p(id);
                        }
                    }
                    ControlMessage::PeerRevoked(revocation) => {
                        if let Some(id) = shared.peer_manager.apply_revocation(revocation)? {
                            shared.close_p2p(id);
                        }
                    }
                    ControlMessage::CertificateIssued(issued) => {
                        if pending_renewal != Some(issued.request_id) {
                            return Err(AgentRuntimeError::UnexpectedMessage(
                                "unsolicited CertificateIssued",
                            ));
                        }
                        let result = install_renewal(
                            &shared.identity_store,
                            &identity,
                            shared.network.overlay,
                            issued,
                        );
                        shared.metrics.record_renewal(result.is_ok());
                        let replacement = result?;
                        rotation_sender.send(replacement.clone()).await
                            .map_err(|_| AgentRuntimeError::RuntimeTaskStopped(
                                "certificate rotation receiver",
                            ))?;
                        pending_renewal = None;
                        renew_at = renewal_instant(&replacement)?;
                    }
                    ControlMessage::Error(error) => {
                        if error.request_id == pending_renewal {
                            shared.metrics.record_renewal(false);
                            pending_renewal = None;
                            renew_at = tokio::time::Instant::now() + RENEWAL_RETRY_DELAY;
                        } else {
                            tracing::debug!(code = ?error.code, message = %error.message,
                                "coordinator rejected Agent request");
                        }
                    }
                    ControlMessage::ControlWelcome(_)
                    | ControlMessage::AnnounceCandidates(_)
                    | ControlMessage::LookupPeer(_)
                    | ControlMessage::ConnectRequest(_)
                    | ControlMessage::RenewCertificate(_) => {
                        return Err(AgentRuntimeError::UnexpectedMessage(
                            "invalid coordinator control message",
                        ));
                    }
                }
            }
            () = &mut renewal_timer, if pending_renewal.is_none() => {
                let request_id = shared.next_request_id();
                let key = NodeKey::load_or_create(shared.identity_store.key_path())?;
                let csr = key.create_csr_der()?;
                write_message_for(
                    &mut writer,
                    &ControlMessage::RenewCertificate(RenewCertificate {
                        request_id,
                        csr_der_base64: STANDARD.encode(csr),
                    }),
                    MessageDirection::AgentToCoordinator,
                ).await?;
                pending_renewal = Some(request_id);
                renew_at = instant_for(identity.not_after()?);
            }
        }
    }
}

fn prepare_connect_plan(
    shared: Arc<RuntimeShared>,
    local: LocalControlIdentity,
    plan: ConnectPlan,
) -> Result<(), AgentRuntimeError> {
    let now = unix_now()?;
    if plan.expires_at_unix_seconds <= now {
        return Err(AgentRuntimeError::ExpiredConnectPlan);
    }
    validate_peer_descriptor(&shared, &plan.peer)?;
    let start = match shared
        .peer_manager
        .start_connect_plan(&plan, Instant::now(), now)
    {
        Ok(start) => start,
        Err(PeerManagerError::P2pAlreadyReady(_)) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for action in &start.actions {
        if let ConnectionAction::Close { connection_id, .. } = action {
            shared.close_p2p(*connection_id);
        }
    }
    let generation = start.generation;
    let action_candidate = start.actions.iter().find_map(|action| match action {
        ConnectionAction::Dial { candidate, .. } => Some(candidate.clone()),
        ConnectionAction::AwaitInbound { .. } | ConnectionAction::Close { .. } => None,
    });
    let mut plans = lock(&shared.plans);
    plans.retain(|_, pending| pending.plan.peer.overlay_ip != plan.peer.overlay_ip);
    plans.insert(
        plan.connection_id.clone(),
        PendingPlan {
            plan: plan.clone(),
            generation,
            local: local.clone(),
        },
    );
    drop(plans);
    let initial_candidate = action_candidate.or_else(|| {
        plan.peer
            .candidates
            .iter()
            .filter(|candidate| candidate.address.is_ipv4())
            .max_by_key(|candidate| candidate.priority)
            .cloned()
    });
    // Both roles dial from the Hybrid socket. The application role still
    // determines which side opens the P2P stream; this creates the pair of
    // NAT mappings without weakening Hello/Ready direction checks.
    if let Some(candidate) = initial_candidate {
        match Arc::clone(&shared.p2p_handshake_gate).try_acquire_owned() {
            Ok(permit) => {
                tokio::spawn(run_outbound_p2p(
                    shared, local, plan, generation, candidate, permit,
                ));
            }
            Err(_) => {
                shared.metrics.record_p2p_connection(false);
                if plan.role == ConnectionRole::Initiator {
                    let _ = shared
                        .peer_manager
                        .probe_failed(plan.peer.overlay_ip, generation);
                }
            }
        }
    }
    Ok(())
}

async fn run_p2p_reaper(
    shared: Arc<RuntimeShared>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), AgentRuntimeError> {
    let interval_duration = shared.config.p2p_idle_timeout.min(Duration::from_secs(30));
    let mut interval = tokio::time::interval(interval_duration);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            _ = interval.tick() => {
                let now = unix_now()?;
                for action in shared.peer_manager.maintenance(Instant::now(), now) {
                    if let ConnectionAction::Close { connection_id, .. } = action {
                        shared.close_p2p(connection_id);
                        shared.metrics.record_path_transition();
                    }
                }
                let expired: Vec<_> = lock(&shared.plans)
                    .iter()
                    .filter(|(_, pending)| pending.plan.expires_at_unix_seconds <= now)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in expired {
                    lock(&shared.plans).remove(&id);
                }
            }
        }
    }
}

async fn run_p2p_acceptor(
    shared: Arc<RuntimeShared>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), AgentRuntimeError> {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            connection = shared.p2p_endpoint.accept() => {
                let connection = connection?;
                let Some(permit) = admit_inbound_p2p_handshake(
                    &shared.p2p_handshake_gate,
                    connection.as_ref(),
                ) else {
                    shared.metrics.record_p2p_connection(false);
                    continue;
                };
                tokio::spawn(handle_inbound_p2p(
                    Arc::clone(&shared), connection, permit,
                ));
            }
        }
    }
}

fn admit_inbound_p2p_handshake(
    gate: &Arc<Semaphore>,
    connection: &dyn TransportConnection,
) -> Option<OwnedSemaphorePermit> {
    match Arc::clone(gate).try_acquire_owned() {
        Ok(permit) => Some(permit),
        Err(_) => {
            connection.close(P2P_CLOSE_CODE, b"P2P handshake capacity exceeded");
            None
        }
    }
}

async fn handle_inbound_p2p(
    shared: Arc<RuntimeShared>,
    connection: Box<dyn TransportConnection>,
    _permit: OwnedSemaphorePermit,
) {
    let result = async {
        let deadline = P2pHandshakeDeadline::after(shared.config.handshake_timeout);
        let handshake =
            authenticate_p2p_transport(&shared, connection, ConnectionDirection::Inbound, deadline)
                .await?;
        let now = unix_now()?;
        let pending = lock(&shared.plans)
            .values()
            .filter(|pending| {
                pending.plan.expires_at_unix_seconds > now
                    && pending.plan.peer.node_id == handshake.verified.node_id()
                    && pending.plan.peer.overlay_ip == handshake.verified.overlay_ip()
                    && pending.plan.peer.certificate_fingerprint == handshake.verified.fingerprint()
            })
            .max_by_key(|pending| pending.plan.expires_at_unix_seconds)
            .cloned()
            .ok_or(AgentRuntimeError::UnknownConnectPlan)?;
        match pending.plan.role {
            ConnectionRole::Initiator => {
                finish_initiator_p2p(
                    Arc::clone(&shared),
                    handshake,
                    &pending.local,
                    &pending.plan,
                    pending.generation,
                )
                .await
            }
            ConnectionRole::Responder => finish_responder_p2p(Arc::clone(&shared), handshake).await,
        }
    }
    .await;
    if let Err(error) = result {
        shared.metrics.record_p2p_connection(false);
        tracing::debug!(error = %error, "rejected inbound P2P connection");
    }
}

async fn run_outbound_p2p(
    shared: Arc<RuntimeShared>,
    local: LocalControlIdentity,
    plan: ConnectPlan,
    generation: ProbeGeneration,
    initial_candidate: Candidate,
    _permit: OwnedSemaphorePermit,
) {
    let result = connect_outbound_p2p(
        Arc::clone(&shared),
        &local,
        &plan,
        generation,
        initial_candidate,
    )
    .await;
    if let Err(error) = result {
        shared.metrics.record_p2p_connection(false);
        if plan.role == ConnectionRole::Initiator {
            let _ = shared
                .peer_manager
                .probe_failed(plan.peer.overlay_ip, generation);
        }
        tracing::debug!(error = %error, peer = %plan.peer.node_id, "P2P probe failed");
    }
}

async fn connect_outbound_p2p(
    shared: Arc<RuntimeShared>,
    local: &LocalControlIdentity,
    plan: &ConnectPlan,
    generation: ProbeGeneration,
    initial_candidate: Candidate,
) -> Result<(), AgentRuntimeError> {
    if plan.expires_at_unix_seconds <= unix_now()? {
        return Err(AgentRuntimeError::ExpiredConnectPlan);
    }
    let mut candidate = initial_candidate;
    loop {
        let attempt = timeout(
            shared.config.connect_timeout,
            shared
                .p2p_endpoint
                .connect(candidate.address, &plan.peer.overlay_ip.to_string()),
        )
        .await;
        let connection = match attempt {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                let error = AgentRuntimeError::Transport(error);
                if !advance_probe(&shared, plan, generation, &mut candidate).await? {
                    return Err(error);
                }
                continue;
            }
            Err(_) => {
                let error = AgentRuntimeError::Timeout("P2P connect");
                if !advance_probe(&shared, plan, generation, &mut candidate).await? {
                    return Err(error);
                }
                continue;
            }
        };
        let deadline = P2pHandshakeDeadline::after(shared.config.handshake_timeout);
        let handshake = authenticate_p2p_transport(
            &shared,
            connection,
            ConnectionDirection::Outbound,
            deadline,
        )
        .await?;
        let result = match plan.role {
            ConnectionRole::Initiator => {
                finish_initiator_p2p(Arc::clone(&shared), handshake, local, plan, generation).await
            }
            ConnectionRole::Responder => finish_responder_p2p(Arc::clone(&shared), handshake).await,
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                if !advance_probe(&shared, plan, generation, &mut candidate).await? {
                    return Err(error);
                }
            }
        }
    }
}

async fn advance_probe(
    shared: &RuntimeShared,
    plan: &ConnectPlan,
    generation: ProbeGeneration,
    candidate: &mut Candidate,
) -> Result<bool, AgentRuntimeError> {
    let now = unix_now()?;
    if now >= plan.expires_at_unix_seconds
        || !shared.peer_manager.dial_failed_at(
            plan.peer.overlay_ip,
            generation,
            Instant::now(),
            now,
        )?
    {
        return Ok(false);
    }
    loop {
        let now = unix_now()?;
        if now >= plan.expires_at_unix_seconds {
            return Ok(false);
        }
        match shared
            .peer_manager
            .next_dial_action(plan.peer.overlay_ip, Instant::now(), now)
        {
            Ok(Some(ConnectionAction::Dial {
                generation: current,
                role,
                candidate: next,
                ..
            })) if current == generation && role == plan.role => {
                *candidate = next;
                return Ok(true);
            }
            Ok(Some(_)) => return Err(AgentRuntimeError::InvalidPeerDescriptor),
            Ok(None) => {
                if shared
                    .peer_manager
                    .snapshot(plan.peer.overlay_ip)
                    .is_some_and(|snapshot| matches!(snapshot.path, PeerPathState::P2pReady { .. }))
                {
                    return Ok(false);
                }
                sleep(Duration::from_millis(50)).await;
            }
            Err(PeerManagerError::ConnectPlanExpired) => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
}

async fn finish_initiator_p2p(
    shared: Arc<RuntimeShared>,
    handshake: AuthenticatedP2pHandshake,
    local: &LocalControlIdentity,
    plan: &ConnectPlan,
    generation: ProbeGeneration,
) -> Result<(), AgentRuntimeError> {
    let AuthenticatedP2pHandshake {
        connection,
        verified,
        direction,
        deadline,
    } = handshake;
    verify_descriptor_certificate(&plan.peer, &verified)?;
    let (mut reader, mut writer) = deadline
        .run("P2P initiator stream", async {
            Ok(connection.open_bi().await?)
        })
        .await?;
    deadline
        .run("P2pHello", async {
            write_p2p_message(
                &mut writer,
                &P2pMessage::P2pHello(P2pHello {
                    connection_id: plan.connection_id.clone(),
                    control_session_id: local.session_id.clone(),
                    incarnation: local.incarnation,
                    certificate_fingerprint: local.certificate_fingerprint.clone(),
                }),
                MessageDirection::InitiatorToResponder,
            )
            .await?;
            Ok(())
        })
        .await?;
    let ready = deadline
        .run("P2pReady", async {
            Ok(read_p2p_message(&mut reader, MessageDirection::ResponderToInitiator).await?)
        })
        .await?;
    let P2pMessage::P2pReady(ready) = ready else {
        return Err(AgentRuntimeError::UnexpectedMessage("P2pReady"));
    };
    if ready.connection_id != plan.connection_id || ready.mtu != shared.network.mtu {
        return Err(AgentRuntimeError::InvalidP2pHandshake);
    }
    activate_p2p(
        shared,
        connection,
        plan.peer.overlay_ip,
        generation,
        &plan.connection_id,
        verified,
        direction,
    )
    .await
}

async fn finish_responder_p2p(
    shared: Arc<RuntimeShared>,
    handshake: AuthenticatedP2pHandshake,
) -> Result<(), AgentRuntimeError> {
    let AuthenticatedP2pHandshake {
        mut connection,
        verified,
        direction,
        deadline,
    } = handshake;
    let (mut reader, mut writer) = deadline
        .run("P2P responder stream", async {
            Ok(connection.accept_bi().await?)
        })
        .await?;
    let hello = deadline
        .run("P2pHello", async {
            Ok(read_p2p_message(&mut reader, MessageDirection::InitiatorToResponder).await?)
        })
        .await?;
    let P2pMessage::P2pHello(hello) = hello else {
        return Err(AgentRuntimeError::UnexpectedMessage("P2pHello"));
    };
    let pending = lock(&shared.plans)
        .get(&hello.connection_id)
        .cloned()
        .ok_or(AgentRuntimeError::UnknownConnectPlan)?;
    if pending.plan.role != ConnectionRole::Responder
        || pending.plan.expires_at_unix_seconds <= unix_now()?
    {
        return Err(AgentRuntimeError::ExpiredConnectPlan);
    }
    verify_hello(&hello, &pending.plan.peer, &verified)?;
    deadline
        .run("P2pReady", async {
            write_p2p_message(
                &mut writer,
                &P2pMessage::P2pReady(P2pReady {
                    connection_id: hello.connection_id,
                    mtu: shared.network.mtu,
                }),
                MessageDirection::ResponderToInitiator,
            )
            .await?;
            Ok(())
        })
        .await?;
    activate_p2p(
        shared,
        connection,
        pending.plan.peer.overlay_ip,
        pending.generation,
        &pending.plan.connection_id,
        verified,
        direction,
    )
    .await
}

async fn authenticate_p2p_transport(
    shared: &RuntimeShared,
    connection: Box<dyn TransportConnection>,
    direction: ConnectionDirection,
    deadline: P2pHandshakeDeadline,
) -> Result<AuthenticatedP2pHandshake, AgentRuntimeError> {
    let connection = deadline
        .run("P2P TLS authentication", async {
            Ok(authenticate_connection(connection, ProtocolPurpose::P2p).await?)
        })
        .await?;
    let ca_der = node_ca_der(&shared.identity_store)?;
    let verified = extract_verified_node_certificate(
        &connection,
        &ca_der,
        shared.network.overlay,
        OffsetDateTime::now_utc(),
    )?;
    Ok(AuthenticatedP2pHandshake {
        connection,
        verified,
        direction,
        deadline,
    })
}

async fn activate_p2p(
    shared: Arc<RuntimeShared>,
    connection: AuthenticatedConnection,
    overlay_ip: Ipv4Addr,
    generation: ProbeGeneration,
    plan_connection_id: &str,
    verified: VerifiedNodeCertificate,
    direction: ConnectionDirection,
) -> Result<(), AgentRuntimeError> {
    let peer_certificate_not_after = verified.not_after();
    let fingerprint = CertificateFingerprint::from_str(verified.fingerprint())
        .map_err(|_| AgentRuntimeError::InvalidPeerDescriptor)?;
    let plan_connection_id = SessionId::from_str(plan_connection_id)
        .map_err(|_| AgentRuntimeError::InvalidP2pHandshake)?;
    match shared.peer_manager.mark_ready_for_plan(
        overlay_ip,
        plan_connection_id,
        &fingerprint,
        direction,
        Instant::now(),
        unix_now()?,
    )? {
        ReadyOutcome::Activated { connection_id } => {
            start_p2p_actor(
                Arc::clone(&shared),
                connection,
                P2pActorLease {
                    overlay_ip,
                    generation,
                    connection_id,
                    peer_certificate_not_after,
                },
            );
        }
        ReadyOutcome::Replaced {
            connection_id,
            connection_to_close,
        } => {
            shared.close_p2p(connection_to_close);
            start_p2p_actor(
                Arc::clone(&shared),
                connection,
                P2pActorLease {
                    overlay_ip,
                    generation,
                    connection_id,
                    peer_certificate_not_after,
                },
            );
        }
        ReadyOutcome::KeptExisting { .. } => {
            connection.close(P2P_CLOSE_CODE, b"duplicate P2P connection");
        }
    }
    shared.metrics.record_p2p_connection(true);
    Ok(())
}

fn start_p2p_actor(
    shared: Arc<RuntimeShared>,
    connection: AuthenticatedConnection,
    lease: P2pActorLease,
) {
    let (sender, receiver) = mpsc::channel(shared.config.queue_capacity);
    let (close_sender, close_receiver) = watch::channel(false);
    shared.install_p2p(
        lease.connection_id,
        P2pHandle {
            sender,
            close: close_sender,
        },
    );
    tokio::spawn(run_p2p_connection(
        connection,
        receiver,
        close_receiver,
        shared,
        lease,
    ));
}

async fn run_p2p_connection(
    mut connection: AuthenticatedConnection,
    mut outbound: mpsc::Receiver<Bytes>,
    mut close: watch::Receiver<bool>,
    shared: Arc<RuntimeShared>,
    lease: P2pActorLease,
) {
    let certificate_expiry = sleep_until(instant_for(lease.peer_certificate_not_after));
    tokio::pin!(certificate_expiry);
    loop {
        let ended = tokio::select! {
            changed = close.changed() => changed.is_err() || *close.borrow(),
            () = &mut certificate_expiry => true,
            packet = outbound.recv() => {
                match packet {
                    Some(packet) => {
                        if !shared.peer_manager.is_current_connection(
                            lease.overlay_ip,
                            lease.generation,
                            lease.connection_id,
                        ) {
                            shared.metrics.record_drop_reason(PacketDropReason::UnavailablePath);
                            true
                        } else if connection.send_datagram(packet).is_err() {
                            shared.metrics.record_drop_reason(PacketDropReason::Transport);
                            true
                        } else {
                            false
                        }
                    }
                    None => true,
                }
            }
            packet = connection.recv_datagram() => {
                match packet {
                    Ok(packet) => {
                        if !shared.peer_manager.is_current_connection(
                            lease.overlay_ip,
                            lease.generation,
                            lease.connection_id,
                        ) {
                            shared.metrics.record_drop_reason(PacketDropReason::UnavailablePath);
                            true
                        } else {
                            if shared.validator.validate_peer_inbound(
                                &packet,
                                lease.overlay_ip,
                                shared.local_overlay_ip(),
                            ).is_err() {
                                shared.metrics.record_drop_reason(PacketDropReason::InvalidPacket);
                            } else if let Err(error) = shared.tun_sender.try_send(packet) {
                                shared.metrics.record_drop_reason(try_send_drop_reason(&error));
                            }
                            false
                        }
                    }
                    Err(_) => {
                        shared.metrics.record_drop_reason(PacketDropReason::Transport);
                        true
                    },
                }
            }
        };
        if ended {
            break;
        }
    }
    connection.close(P2P_CLOSE_CODE, b"P2P path ended");
    if shared.peer_manager.connection_closed(
        lease.overlay_ip,
        lease.generation,
        lease.connection_id,
    ) {
        shared.close_p2p(lease.connection_id);
        shared.metrics.record_path_transition();
    }
}

fn install_enrollment(
    store: &NodeIdentityStore,
    expected_enrollment_id: &str,
    expected_node_id: &str,
    accepted: EnrollAccepted,
) -> Result<InstalledNodeIdentity, AgentRuntimeError> {
    if accepted.enrollment_id != expected_enrollment_id
        || accepted.node_id != expected_node_id
        || accepted.certificate_chain_der_base64.len() != 2
    {
        return Err(AgentRuntimeError::EnrollmentResponseMismatch);
    }
    PacketValidator::new(accepted.overlay_cidr, usize::from(accepted.mtu))?;
    if !is_overlay_unicast(accepted.overlay_cidr, accepted.overlay_ip) {
        return Err(AgentRuntimeError::EnrollmentResponseMismatch);
    }
    let leaf_der = decode_first_certificate(&accepted.certificate_chain_der_base64)?;
    let ca_der = decode_der(&accepted.node_ca_certificate_der_base64)?;
    if let Some(encoded_chain_ca) = accepted.certificate_chain_der_base64.get(1)
        && decode_der(encoded_chain_ca)? != ca_der
    {
        return Err(AgentRuntimeError::EnrollmentResponseMismatch);
    }
    let now = OffsetDateTime::now_utc();
    let verified = verify_node_certificate_der(&leaf_der, &ca_der, accepted.overlay_cidr, now)?;
    if verified.node_id() != accepted.node_id || verified.overlay_ip() != accepted.overlay_ip {
        return Err(AgentRuntimeError::EnrollmentResponseMismatch);
    }
    validate_reported_certificate_validity(
        verified.not_before(),
        verified.not_after(),
        accepted.not_before_unix_seconds,
        accepted.not_after_unix_seconds,
    )?;
    let identity = store.install_certificate(
        &accepted.node_id,
        accepted.overlay_ip,
        &certificate_pem(&ca_der),
        &certificate_pem(&leaf_der),
        now,
    )?;
    store.clear_pending_enrollment(expected_enrollment_id)?;
    Ok(identity)
}

fn install_renewal(
    store: &NodeIdentityStore,
    current: &InstalledNodeIdentity,
    overlay: Ipv4Net,
    issued: CertificateIssued,
) -> Result<InstalledNodeIdentity, AgentRuntimeError> {
    if issued.certificate_chain_der_base64.len() != 2 {
        return Err(AgentRuntimeError::MissingCertificateChain);
    }
    let leaf_der = decode_first_certificate(&issued.certificate_chain_der_base64)?;
    let current_ca_der = parse_one_certificate(current.node_ca_pem())?;
    if let Some(encoded_ca) = issued.certificate_chain_der_base64.get(1)
        && decode_der(encoded_ca)? != current_ca_der
    {
        return Err(AgentRuntimeError::RenewalCaChanged);
    }
    let verified = verify_node_certificate_der(
        &leaf_der,
        &current_ca_der,
        overlay,
        OffsetDateTime::now_utc(),
    )?;
    if verified.node_id() != current.node_id || verified.overlay_ip() != current.overlay_ip {
        return Err(NodeIdentityStoreError::CertificateIdentityMismatch.into());
    }
    validate_reported_certificate_validity(
        verified.not_before(),
        verified.not_after(),
        issued.not_before_unix_seconds,
        issued.not_after_unix_seconds,
    )?;
    let identity = store.install_certificate(
        &current.node_id,
        current.overlay_ip,
        current.node_ca_pem(),
        &certificate_pem(&leaf_der),
        OffsetDateTime::now_utc(),
    )?;
    Ok(identity)
}

fn validate_reported_certificate_validity(
    certificate_not_before: OffsetDateTime,
    certificate_not_after: OffsetDateTime,
    reported_not_before: u64,
    reported_not_after: u64,
) -> Result<(), AgentRuntimeError> {
    let not_before = u64::try_from(certificate_not_before.unix_timestamp())
        .map_err(|_| AgentRuntimeError::InvalidCertificateValidity)?;
    let not_after = u64::try_from(certificate_not_after.unix_timestamp())
        .map_err(|_| AgentRuntimeError::InvalidCertificateValidity)?;
    if not_before != reported_not_before || not_after != reported_not_after {
        return Err(AgentRuntimeError::InvalidCertificateValidity);
    }
    Ok(())
}

fn validate_control_welcome(
    config: &AgentRuntimeConfig,
    identity: &InstalledNodeIdentity,
    welcome: &ControlWelcome,
) -> Result<(), AgentRuntimeError> {
    AgentNetwork::from_welcome(welcome)?;
    let session_id = uuid::Uuid::parse_str(&welcome.session_id)
        .map_err(|_| AgentRuntimeError::InvalidControlWelcome)?;
    let not_after = u64::try_from(identity.not_after()?.unix_timestamp())
        .map_err(|_| AgentRuntimeError::InvalidCertificateValidity)?;
    if session_id.get_version() != Some(uuid::Version::Random)
        || welcome.incarnation == 0
        || welcome.overlay_ip != identity.overlay_ip
        || identity.node_id != config.node_id
        || welcome.certificate_not_after_unix_seconds != not_after
    {
        return Err(AgentRuntimeError::InvalidControlWelcome);
    }
    Ok(())
}

fn validate_installed_identity(
    config: &AgentRuntimeConfig,
    identity: &InstalledNodeIdentity,
    network: AgentNetwork,
) -> Result<(), AgentRuntimeError> {
    if identity.node_id != config.node_id
        || !identity.is_valid_at(OffsetDateTime::now_utc())
        || !is_overlay_unicast(network.overlay, identity.overlay_ip)
    {
        return Err(AgentRuntimeError::InvalidInstalledIdentity);
    }
    Ok(())
}

fn validate_peer_descriptor(
    shared: &RuntimeShared,
    descriptor: &PeerDescriptor,
) -> Result<(), AgentRuntimeError> {
    if descriptor.node_id == shared.config.node_id
        || !is_overlay_unicast(shared.network.overlay, descriptor.overlay_ip)
        || descriptor.certificate_not_after_unix_seconds <= unix_now()?
        || descriptor
            .candidates
            .iter()
            .all(|candidate| !candidate.address.is_ipv4())
    {
        return Err(AgentRuntimeError::InvalidPeerDescriptor);
    }
    SessionId::from_str(&descriptor.session_id)
        .map_err(|_| AgentRuntimeError::InvalidPeerDescriptor)?;
    CertificateFingerprint::from_str(&descriptor.certificate_fingerprint)
        .map_err(|_| AgentRuntimeError::InvalidPeerDescriptor)?;
    Ok(())
}

fn verify_descriptor_certificate(
    descriptor: &PeerDescriptor,
    verified: &VerifiedNodeCertificate,
) -> Result<(), AgentRuntimeError> {
    let verified_not_after = u64::try_from(verified.not_after().unix_timestamp())
        .map_err(|_| AgentRuntimeError::InvalidPeerDescriptor)?;
    if verified.node_id() != descriptor.node_id
        || verified.overlay_ip() != descriptor.overlay_ip
        || verified.fingerprint() != descriptor.certificate_fingerprint
        || verified_not_after != descriptor.certificate_not_after_unix_seconds
    {
        return Err(AgentRuntimeError::PeerCertificateMismatch);
    }
    Ok(())
}

fn verify_hello(
    hello: &P2pHello,
    descriptor: &PeerDescriptor,
    verified: &VerifiedNodeCertificate,
) -> Result<(), AgentRuntimeError> {
    verify_descriptor_certificate(descriptor, verified)?;
    if hello.control_session_id != descriptor.session_id
        || hello.incarnation != descriptor.incarnation
        || hello.certificate_fingerprint != descriptor.certificate_fingerprint
    {
        return Err(AgentRuntimeError::InvalidP2pHandshake);
    }
    Ok(())
}

fn host_candidates(local: SocketAddr, coordinator: SocketAddr) -> Vec<Candidate> {
    let port = local.port();
    let mut addresses = match local.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => vec![ip],
        IpAddr::V4(_) => {
            let mut addresses = local_interface_ipv4_addresses();
            if let Some(derived) = derive_local_ipv4(coordinator) {
                addresses.push(derived);
            }
            addresses
        }
        IpAddr::V6(_) => return Vec::new(),
    };
    addresses.retain(|ip| {
        !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast() && !ip.is_loopback()
    });
    addresses.sort_unstable();
    addresses.dedup();
    addresses.truncate(MAX_CANDIDATES);
    addresses
        .into_iter()
        .enumerate()
        .map(|(index, ip)| Candidate {
            address: SocketAddr::from((ip, port)),
            kind: CandidateKind::Host,
            priority: u32::try_from(MAX_CANDIDATES - index).unwrap_or(1),
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn local_interface_ipv4_addresses() -> Vec<Ipv4Addr> {
    let mut head = std::ptr::null_mut();
    // getifaddrs owns the linked list until the matching freeifaddrs call.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Vec::new();
    }
    let mut addresses = Vec::new();
    let mut current = head;
    while !current.is_null() {
        // SAFETY: current belongs to the live getifaddrs list and is advanced
        // only through its next pointer before the list is freed.
        let interface = unsafe { &*current };
        let flags = interface.ifa_flags as libc::c_int;
        if flags & libc::IFF_UP != 0
            && flags & libc::IFF_LOOPBACK == 0
            && !interface.ifa_addr.is_null()
        {
            // SAFETY: sa_family identifies this address as sockaddr_in.
            let address = unsafe { &*interface.ifa_addr };
            if i32::from(address.sa_family) == libc::AF_INET {
                // SAFETY: the family check above establishes sockaddr_in layout.
                let ipv4 = unsafe { &*(interface.ifa_addr.cast::<libc::sockaddr_in>()) };
                addresses.push(Ipv4Addr::from(u32::from_be(ipv4.sin_addr.s_addr)));
            }
        }
        current = interface.ifa_next;
    }
    // SAFETY: head is the pointer returned by the successful getifaddrs call.
    unsafe { libc::freeifaddrs(head) };
    addresses
}

#[cfg(not(target_os = "linux"))]
fn local_interface_ipv4_addresses() -> Vec<Ipv4Addr> {
    Vec::new()
}

fn derive_local_ipv4(remote: SocketAddr) -> Option<Ipv4Addr> {
    let remote = match remote {
        SocketAddr::V4(remote) => SocketAddr::V4(remote),
        SocketAddr::V6(_) => return None,
    };
    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0))).ok()?;
    socket.connect(remote).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        IpAddr::V4(_) | IpAddr::V6(_) => None,
    }
}

fn renewal_instant(
    identity: &InstalledNodeIdentity,
) -> Result<tokio::time::Instant, AgentRuntimeError> {
    let not_before = identity.not_before()?.unix_timestamp();
    let not_after = identity.not_after()?.unix_timestamp();
    let midpoint = not_before + (not_after - not_before) / 2;
    let jitter = rand::thread_rng().gen_range(-RENEWAL_JITTER_SECONDS..=RENEWAL_JITTER_SECONDS);
    let renewal = OffsetDateTime::from_unix_timestamp((midpoint + jitter).min(not_after - 1))
        .map_err(|_| AgentRuntimeError::InvalidCertificateValidity)?;
    Ok(instant_for(renewal))
}

fn instant_for(time: OffsetDateTime) -> tokio::time::Instant {
    let now = OffsetDateTime::now_utc();
    if time <= now {
        tokio::time::Instant::now()
    } else {
        let seconds = u64::try_from((time - now).whole_seconds()).unwrap_or(u64::MAX);
        tokio::time::Instant::now() + Duration::from_secs(seconds)
    }
}

fn next_reconnect_delay(current: Duration, initial: Duration, maximum: Duration) -> Duration {
    if current.is_zero() {
        initial
    } else {
        current.saturating_mul(2).min(maximum)
    }
}

fn node_ca_der(store: &NodeIdentityStore) -> Result<Vec<u8>, AgentRuntimeError> {
    let identity = store
        .installed_identity()
        .ok_or(AgentRuntimeError::MissingInstalledIdentity)?;
    parse_one_certificate(identity.node_ca_pem())
}

fn certificate_fingerprint_from_pem(pem: &str) -> Result<String, AgentRuntimeError> {
    let digest = Sha256::digest(parse_one_certificate(pem)?);
    let mut value = String::from("sha256:");
    for byte in digest {
        use fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    Ok(value)
}

fn parse_one_certificate(value: &str) -> Result<Vec<u8>, AgentRuntimeError> {
    let blocks =
        pem::parse_many(value.as_bytes()).map_err(|_| AgentRuntimeError::InvalidCertificatePem)?;
    if blocks.len() != 1 || blocks[0].tag() != "CERTIFICATE" {
        return Err(AgentRuntimeError::InvalidCertificatePem);
    }
    Ok(blocks[0].contents().to_vec())
}

fn decode_first_certificate(values: &[String]) -> Result<Vec<u8>, AgentRuntimeError> {
    values
        .first()
        .ok_or(AgentRuntimeError::MissingCertificateChain)
        .and_then(|value| decode_der(value))
}

fn decode_der(value: &str) -> Result<Vec<u8>, AgentRuntimeError> {
    STANDARD
        .decode(value)
        .map_err(|_| AgentRuntimeError::InvalidCertificateDer)
}

fn certificate_pem(der: &[u8]) -> String {
    pem::encode(&pem::Pem::new("CERTIFICATE", der))
}

fn read_private_key(store: &NodeIdentityStore) -> Result<String, AgentRuntimeError> {
    read_private_key_path(&store.key_path())
}

fn read_private_key_path(path: &PathBuf) -> Result<String, AgentRuntimeError> {
    fs::read_to_string(path).map_err(|source| AgentRuntimeError::ReadPrivateKey {
        path: path.clone(),
        source,
    })
}

fn unix_now() -> Result<u64, AgentRuntimeError> {
    u64::try_from(OffsetDateTime::now_utc().unix_timestamp())
        .map_err(|_| AgentRuntimeError::InvalidCertificateValidity)
}

fn remote_error(context: &'static str, error: ErrorMessage) -> AgentRuntimeError {
    AgentRuntimeError::Remote {
        context,
        code: format!("{:?}", error.code),
        message: error.message,
        retryable: error.retryable,
    }
}

fn next_nonzero(counter: &AtomicU64) -> u64 {
    loop {
        let value = counter.fetch_add(1, Ordering::Relaxed);
        if value != 0 {
            return value;
        }
    }
}

fn is_overlay_unicast(overlay: Ipv4Net, address: Ipv4Addr) -> bool {
    overlay.contains(&address)
        && address != overlay.network()
        && address != overlay.broadcast()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_broadcast()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn join_runtime_task(
    task: &'static str,
    result: Result<Result<(), AgentRuntimeError>, tokio::task::JoinError>,
) -> AgentRuntimeError {
    match result {
        Ok(Ok(())) => AgentRuntimeError::RuntimeTaskStopped(task),
        Ok(Err(error)) => error,
        Err(source) => AgentRuntimeError::TaskJoin { task, source },
    }
}

enum SessionExit {
    Reconnect,
    CredentialsRotated(PreparedSession),
}

#[derive(Debug, thiserror::Error)]
pub enum AgentRuntimeError {
    #[error("invalid Agent runtime configuration: {0}")]
    Configuration(String),
    #[error("node identity store failed: {0}")]
    IdentityStore(#[from] NodeIdentityStoreError),
    #[error("node identity verification failed: {0}")]
    Identity(#[from] IdentityError),
    #[error("transport failed: {0}")]
    Transport(#[from] TransportError),
    #[error("v2 protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("TUN or route operation failed: {0}")]
    Tun(#[from] TunError),
    #[error("overlay packet validation failed: {0}")]
    Routing(#[from] RouteError),
    #[error("operation timed out while waiting for {0}")]
    Timeout(&'static str),
    #[error("enrollment token is required because no valid node identity is installed")]
    EnrollmentTokenRequired,
    #[error("enrollment response does not match the persisted request")]
    EnrollmentResponseMismatch,
    #[error("no installed node identity is available")]
    MissingInstalledIdentity,
    #[error("installed node identity is invalid for the coordinator network")]
    InvalidInstalledIdentity,
    #[error("control Welcome is inconsistent with the authenticated node identity")]
    InvalidControlWelcome,
    #[error("coordinator changed overlay CIDR or MTU during this Agent run")]
    NetworkParametersChanged,
    #[error("node certificate expired")]
    CertificateExpired,
    #[error("certificate validity metadata is inconsistent")]
    InvalidCertificateValidity,
    #[error("certificate chain is missing its leaf certificate")]
    MissingCertificateChain,
    #[error("certificate DER is not canonical base64")]
    InvalidCertificateDer,
    #[error("invalid single-certificate PEM document")]
    InvalidCertificatePem,
    #[error("certificate renewal attempted to replace the node CA")]
    RenewalCaChanged,
    #[error("invalid or stale peer descriptor")]
    InvalidPeerDescriptor,
    #[error("peer TLS certificate does not match its connection plan")]
    PeerCertificateMismatch,
    #[error("P2P handshake does not match its connection plan")]
    InvalidP2pHandshake,
    #[error("connection plan has expired")]
    ExpiredConnectPlan,
    #[error("no matching connection plan exists")]
    UnknownConnectPlan,
    #[error("peer has no usable IPv4 host candidate")]
    NoUsablePeerCandidate,
    #[error("unexpected v2 message: {0}")]
    UnexpectedMessage(&'static str),
    #[error("coordinator rejected {context}: {code}: {message} (retryable={retryable})")]
    Remote {
        context: &'static str,
        code: String,
        message: String,
        retryable: bool,
    },
    #[error("cannot read private key {path}: {source}")]
    ReadPrivateKey {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime task {0} stopped unexpectedly")]
    RuntimeTaskStopped(&'static str),
    #[error("runtime task {task} failed to join: {source}")]
    TaskJoin {
        task: &'static str,
        #[source]
        source: tokio::task::JoinError,
    },
    #[error("this build does not include the Quinn v2 Agent runtime")]
    QuinnUnavailable,
    #[error("peer manager failed: {0}")]
    PeerManager(#[from] PeerManagerError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PeerDescriptor;
    use std::collections::VecDeque;

    #[derive(Clone, Copy)]
    enum ScriptedSendResult {
        Sent,
        QueueFull,
        TooLarge,
        ConnectionClosed,
    }

    #[derive(Default)]
    struct CloseRecordingConnection {
        close: Mutex<Option<(u32, Vec<u8>)>>,
        send_results: Mutex<VecDeque<ScriptedSendResult>>,
    }

    impl CloseRecordingConnection {
        fn with_send_results(results: impl IntoIterator<Item = ScriptedSendResult>) -> Self {
            Self {
                close: Mutex::new(None),
                send_results: Mutex::new(results.into_iter().collect()),
            }
        }
    }

    impl crate::transport::sealed::Sealed for CloseRecordingConnection {}

    #[async_trait::async_trait]
    impl TransportConnection for CloseRecordingConnection {
        async fn open_bi(&self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
            panic!("unused test method")
        }

        async fn accept_bi(&mut self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
            panic!("unused test method")
        }

        fn send_datagram(&self, _packet: Bytes) -> Result<(), TransportError> {
            match lock(&self.send_results)
                .pop_front()
                .expect("scripted send result")
            {
                ScriptedSendResult::Sent => Ok(()),
                ScriptedSendResult::QueueFull => Err(TransportError::DatagramQueueFull),
                ScriptedSendResult::TooLarge => Err(TransportError::DatagramTooLarge),
                ScriptedSendResult::ConnectionClosed => Err(TransportError::ConnectionClosed),
            }
        }

        async fn recv_datagram(&mut self) -> Result<Bytes, TransportError> {
            panic!("unused test method")
        }

        fn remote_address(&self) -> SocketAddr {
            SocketAddr::from(([192, 0, 2, 10], 7003))
        }

        async fn negotiated_alpn(&self) -> Result<Vec<u8>, TransportError> {
            panic!("unused test method")
        }

        async fn peer_certificate_chain_der(&self) -> Result<Vec<Vec<u8>>, TransportError> {
            panic!("unused test method")
        }

        fn close(&self, code: u32, reason: &[u8]) {
            *lock(&self.close) = Some((code, reason.to_vec()));
        }

        async fn closed(&self) -> TransportError {
            panic!("unused test method")
        }
    }

    #[cfg(unix)]
    use crate::{
        identity::CertificateAuthority,
        registry::{ENROLLMENT_TOKEN_BYTES, EnrollmentToken},
    };

    #[cfg(unix)]
    struct TestIdentityDirectory(PathBuf);

    #[cfg(unix)]
    impl TestIdentityDirectory {
        fn new(label: &str) -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let path = std::env::temp_dir().join(format!(
                "stellaris-agent-runtime-{label}-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            std::fs::create_dir(&path).expect("create identity test directory");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("secure identity test directory");
            Self(path)
        }
    }

    #[cfg(unix)]
    impl AsRef<std::path::Path> for TestIdentityDirectory {
        fn as_ref(&self) -> &std::path::Path {
            &self.0
        }
    }

    #[cfg(unix)]
    impl Drop for TestIdentityDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config() -> AgentRuntimeConfig {
        AgentRuntimeConfig::new(
            "edge-a",
            "/tmp/stellaris-agent-runtime-test",
            "deployment CA",
            "coordinator.example",
            SocketAddr::from(([192, 0, 2, 1], 7000)),
            SocketAddr::from(([192, 0, 2, 1], 7001)),
            SocketAddr::from(([192, 0, 2, 1], 7002)),
            SocketAddr::from(([0, 0, 0, 0], 7003)),
        )
    }

    fn fingerprint() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

    fn ipv4_packet(source: Ipv4Addr, destination: Ipv4Addr) -> Bytes {
        let mut packet = vec![0_u8; 20];
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&20_u16.to_be_bytes());
        packet[8] = 64;
        packet[9] = 17;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        Bytes::from(packet)
    }

    fn ready_connection_id() -> PeerConnectionId {
        let overlay: Ipv4Net = "10.42.0.0/24".parse().expect("overlay");
        let manager = PeerManager::with_defaults("edge-a", overlay).expect("manager");
        let now = 100_u64;
        let plan = ConnectPlan {
            request_id: 1,
            connection_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            role: ConnectionRole::Initiator,
            peer: PeerDescriptor {
                node_id: "edge-b".to_owned(),
                overlay_ip: "10.42.0.3".parse().expect("peer IP"),
                incarnation: 1,
                session_id: "550e8400-e29b-41d4-a716-446655440001".to_owned(),
                certificate_fingerprint: fingerprint(),
                certificate_not_after_unix_seconds: now + 60,
                candidate_epoch: 1,
                candidates: vec![Candidate {
                    address: SocketAddr::from(([192, 0, 2, 3], 7003)),
                    kind: CandidateKind::Host,
                    priority: 1,
                }],
            },
            expires_at_unix_seconds: now + 30,
        };
        manager
            .start_connect_plan(&plan, Instant::now(), now)
            .expect("start plan");
        let result = manager
            .mark_ready_for_plan(
                plan.peer.overlay_ip,
                SessionId::from_str(&plan.connection_id).expect("plan ID"),
                &CertificateFingerprint::from_str(&fingerprint()).expect("fingerprint"),
                ConnectionDirection::Outbound,
                Instant::now(),
                now,
            )
            .expect("ready");
        match result {
            ReadyOutcome::Activated { connection_id } => connection_id,
            ReadyOutcome::Replaced { .. } | ReadyOutcome::KeptExisting { .. } => {
                panic!("first connection must activate")
            }
        }
    }

    #[test]
    fn config_requires_only_local_and_coordinator_inputs() {
        let config = config();
        config.validate().expect("valid config");
        assert!(!format!("{config:?}").contains("deployment CA"));

        let mut invalid = config;
        invalid.queue_capacity = 0;
        assert!(matches!(
            invalid.validate(),
            Err(AgentRuntimeError::Configuration(_))
        ));
    }

    #[test]
    fn inbound_p2p_handshake_capacity_is_bounded_and_rejected() {
        let gate = Arc::new(Semaphore::new(1));
        let first = CloseRecordingConnection::default();
        let first_permit =
            admit_inbound_p2p_handshake(&gate, &first).expect("first handshake admitted");
        let rejected = CloseRecordingConnection::default();

        assert!(admit_inbound_p2p_handshake(&gate, &rejected).is_none());
        assert_eq!(
            *lock(&rejected.close),
            Some((P2P_CLOSE_CODE, b"P2P handshake capacity exceeded".to_vec()))
        );

        drop(first_permit);
        assert!(admit_inbound_p2p_handshake(&gate, &rejected).is_some());
    }

    #[test]
    fn relay_local_datagram_rejections_drop_one_packet_without_failing_the_session() {
        let connection = CloseRecordingConnection::with_send_results([
            ScriptedSendResult::QueueFull,
            ScriptedSendResult::TooLarge,
            ScriptedSendResult::Sent,
            ScriptedSendResult::ConnectionClosed,
        ]);
        let metrics = RuntimeMetrics::default();
        let packet = Bytes::from_static(b"packet");

        assert!(matches!(
            send_relay_datagram(&connection, packet.clone(), &metrics),
            Ok(false)
        ));
        assert!(matches!(
            send_relay_datagram(&connection, packet.clone(), &metrics),
            Ok(false)
        ));
        assert!(matches!(
            send_relay_datagram(&connection, packet.clone(), &metrics),
            Ok(true)
        ));
        assert!(matches!(
            send_relay_datagram(&connection, packet, &metrics),
            Err(AgentRuntimeError::Transport(
                TransportError::ConnectionClosed
            ))
        ));

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.queue_full_drops, 1);
        assert_eq!(snapshot.invalid_packet_drops, 1);
        assert_eq!(snapshot.transport_drops, 1);
        assert_eq!(snapshot.dropped_packets, 3);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn valid_installed_identity_does_not_require_an_enrollment_token() {
        let directory = TestIdentityDirectory::new("tokenless-installed-identity");
        let store = Arc::new(
            NodeIdentityStore::load_or_create(&directory).expect("open identity test store"),
        );
        let token = EnrollmentToken::from_bytes([7; ENROLLMENT_TOKEN_BYTES]).encode();
        let pending = store
            .prepare_enrollment("edge-a", &token)
            .expect("prepare identity fixture enrollment");
        let authority = CertificateAuthority::create(
            directory.as_ref().join("node-ca.pem"),
            directory.as_ref().join("node-ca.key"),
        )
        .expect("create identity fixture CA");
        let overlay: Ipv4Net = "10.42.0.0/24".parse().expect("fixture overlay");
        let overlay_ip = "10.42.0.2".parse().expect("fixture address");
        let issued = authority
            .issue_node_certificate(
                &pending.csr_der().expect("decode fixture CSR"),
                "edge-a",
                overlay,
                overlay_ip,
            )
            .expect("issue identity fixture certificate");
        let installed = store
            .install_certificate(
                "edge-a",
                overlay_ip,
                authority.certificate_pem(),
                issued.pem(),
                OffsetDateTime::now_utc(),
            )
            .expect("install identity fixture");
        assert_eq!(store.pending_enrollment(), Some(pending));
        drop(store);
        let store = Arc::new(
            NodeIdentityStore::load_or_create(&directory)
                .expect("reopen crash-window identity store"),
        );

        let mut runtime_config = config();
        runtime_config.identity_directory = directory.as_ref().to_path_buf();
        runtime_config.enrollment_token = None;
        let runtime = AgentRuntime::new(runtime_config).expect("create Agent runtime");
        let selected = runtime
            .ensure_identity(&store)
            .await
            .expect("reuse valid identity without enrollment token");

        assert_eq!(selected, installed);
        assert!(store.pending_enrollment().is_none());
        drop(store);
        let reopened = NodeIdentityStore::load_or_create(&directory)
            .expect("reopen identity store after pending cleanup");
        assert_eq!(reopened.installed_identity(), Some(installed));
        assert!(reopened.pending_enrollment().is_none());

        reopened
            .prepare_enrollment("edge-b", &token)
            .expect("prepare conflicting pending request");
        let reopened = Arc::new(reopened);
        assert!(matches!(
            runtime.ensure_identity(&reopened).await,
            Err(AgentRuntimeError::IdentityStore(
                NodeIdentityStoreError::PendingEnrollmentConflict
            ))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejected_enrollment_metadata_does_not_change_identity_or_pending_request() {
        let directory = TestIdentityDirectory::new("rejected-enrollment-metadata");
        let store = NodeIdentityStore::load_or_create(&directory).expect("open identity store");
        let token = EnrollmentToken::from_bytes([7; ENROLLMENT_TOKEN_BYTES]).encode();
        let pending = store
            .prepare_enrollment("edge-a", &token)
            .expect("prepare enrollment request");
        let authority = CertificateAuthority::create(
            directory.as_ref().join("fixture-node-ca.pem"),
            directory.as_ref().join("fixture-node-ca.key"),
        )
        .expect("create fixture node CA");
        let overlay: Ipv4Net = "10.42.0.0/24".parse().expect("fixture overlay");
        let overlay_ip = "10.42.0.2".parse().expect("fixture address");
        let csr = pending.csr_der().expect("decode fixture CSR");
        let current_certificate = authority
            .issue_node_certificate(&csr, "edge-a", overlay, overlay_ip)
            .expect("issue current fixture certificate");
        let current = store
            .install_certificate(
                "edge-a",
                overlay_ip,
                authority.certificate_pem(),
                current_certificate.pem(),
                OffsetDateTime::now_utc(),
            )
            .expect("install current fixture identity");
        let replacement = authority
            .issue_node_certificate(&csr, "edge-a", overlay, overlay_ip)
            .expect("issue replacement fixture certificate");
        let mut accepted = EnrollAccepted {
            enrollment_id: pending.enrollment_id.clone(),
            node_id: pending.node_id.clone(),
            overlay_ip,
            overlay_cidr: overlay,
            mtu: 1100,
            certificate_chain_der_base64: vec![
                STANDARD.encode(replacement.der()),
                STANDARD.encode(authority.certificate_der()),
            ],
            node_ca_certificate_der_base64: STANDARD.encode(authority.certificate_der()),
            not_before_unix_seconds: u64::try_from(replacement.not_before().unix_timestamp())
                .expect("positive not-before"),
            not_after_unix_seconds: u64::try_from(replacement.not_after().unix_timestamp())
                .expect("positive not-after"),
        };
        accepted.not_after_unix_seconds += 1;

        assert!(matches!(
            install_enrollment(&store, &pending.enrollment_id, &pending.node_id, accepted,),
            Err(AgentRuntimeError::InvalidCertificateValidity)
        ));
        assert_eq!(store.installed_identity(), Some(current.clone()));
        assert_eq!(store.pending_enrollment(), Some(pending.clone()));

        drop(store);
        let reopened = NodeIdentityStore::load_or_create(&directory)
            .expect("reopen identity store after rejected enrollment");
        assert_eq!(reopened.installed_identity(), Some(current));
        assert_eq!(reopened.pending_enrollment(), Some(pending));
    }

    #[cfg(unix)]
    #[test]
    fn rejected_renewal_metadata_does_not_replace_installed_identity() {
        let directory = TestIdentityDirectory::new("rejected-renewal-metadata");
        let store = NodeIdentityStore::load_or_create(&directory).expect("open identity store");
        let key = NodeKey::load_or_create(store.key_path()).expect("load fixture node key");
        let csr = key.create_csr_der().expect("create fixture CSR");
        let authority = CertificateAuthority::create(
            directory.as_ref().join("fixture-node-ca.pem"),
            directory.as_ref().join("fixture-node-ca.key"),
        )
        .expect("create fixture node CA");
        let overlay: Ipv4Net = "10.42.0.0/24".parse().expect("fixture overlay");
        let overlay_ip = "10.42.0.2".parse().expect("fixture address");
        let current_certificate = authority
            .issue_node_certificate(&csr, "edge-a", overlay, overlay_ip)
            .expect("issue current fixture certificate");
        let current = store
            .install_certificate(
                "edge-a",
                overlay_ip,
                authority.certificate_pem(),
                current_certificate.pem(),
                OffsetDateTime::now_utc(),
            )
            .expect("install current fixture identity");
        let replacement = authority
            .issue_node_certificate(&csr, "edge-a", overlay, overlay_ip)
            .expect("issue replacement fixture certificate");
        assert_ne!(
            replacement.pem(),
            current.node_certificate_pem(),
            "fixture renewal must produce a distinct certificate",
        );
        let reported_not_before =
            u64::try_from(replacement.not_before().unix_timestamp()).expect("positive not-before");
        let reported_not_after =
            u64::try_from(replacement.not_after().unix_timestamp()).expect("positive not-after");
        let issued = CertificateIssued {
            request_id: 1,
            certificate_chain_der_base64: vec![
                STANDARD.encode(replacement.der()),
                STANDARD.encode(authority.certificate_der()),
            ],
            not_before_unix_seconds: reported_not_before,
            not_after_unix_seconds: reported_not_after,
        };
        let mut inconsistent = issued.clone();
        inconsistent.not_after_unix_seconds += 1;

        assert!(matches!(
            install_renewal(&store, &current, overlay, inconsistent),
            Err(AgentRuntimeError::InvalidCertificateValidity)
        ));
        assert_eq!(store.installed_identity(), Some(current.clone()));

        drop(store);
        let reopened = NodeIdentityStore::load_or_create(&directory)
            .expect("reopen identity store after rejected renewal");
        assert_eq!(reopened.installed_identity(), Some(current.clone()));

        let installed = install_renewal(&reopened, &current, overlay, issued)
            .expect("install renewal after validating consistent metadata");
        assert_eq!(
            parse_one_certificate(installed.node_certificate_pem()).expect("parse installed leaf"),
            replacement.der(),
        );
    }

    #[test]
    fn network_parameters_come_from_control_welcome() {
        let welcome = ControlWelcome {
            session_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            incarnation: 1,
            overlay_ip: "10.42.0.2".parse().expect("IP"),
            overlay_cidr: "10.42.0.0/24".parse().expect("overlay"),
            mtu: 1100,
            certificate_not_after_unix_seconds: 1,
            coordinator_time_unix_seconds: 1,
        };
        assert_eq!(
            AgentNetwork::from_welcome(&welcome).expect("network"),
            AgentNetwork {
                overlay: welcome.overlay_cidr,
                mtu: 1100,
            }
        );
    }

    #[test]
    fn concrete_p2p_bind_announces_only_that_address() {
        let local = SocketAddr::from(([192, 0, 2, 20], 7010));
        let candidates = host_candidates(local, SocketAddr::from(([192, 0, 2, 1], 7001)));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].address, local);
        assert_eq!(candidates[0].kind, CandidateKind::Host);
    }

    #[tokio::test]
    async fn failed_p2p_packet_is_not_retried_over_relay() {
        let connection_id = ready_connection_id();
        let (p2p_sender, p2p_receiver) = mpsc::channel(1);
        drop(p2p_receiver);
        let (relay_sender, mut relay_receiver) = mpsc::channel(1);
        let packet = ipv4_packet(
            "10.42.0.2".parse().expect("source"),
            "10.42.0.3".parse().expect("destination"),
        );

        assert_eq!(
            enqueue_selected(
                SelectedPath::P2p(connection_id),
                packet.clone(),
                Some(p2p_sender),
                Some(relay_sender.clone()),
            ),
            EnqueueOutcome::P2pFailed(connection_id, PacketDropReason::UnavailablePath)
        );
        assert!(matches!(
            relay_receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        assert_eq!(
            enqueue_selected(
                SelectedPath::Relay,
                packet.clone(),
                None,
                Some(relay_sender)
            ),
            EnqueueOutcome::RelayQueued(1)
        );
        assert_eq!(relay_receiver.recv().await, Some(packet));
    }

    #[test]
    fn peer_inbound_requires_authenticated_source_and_local_destination() {
        let validator = PacketValidator::new("10.42.0.0/24".parse().expect("overlay"), 1100)
            .expect("validator");
        let packet = ipv4_packet(
            "10.42.0.4".parse().expect("source"),
            "10.42.0.2".parse().expect("destination"),
        );
        assert!(
            validator
                .validate_peer_inbound(
                    &packet,
                    "10.42.0.3".parse().expect("authenticated peer"),
                    "10.42.0.2".parse().expect("local"),
                )
                .is_err()
        );
    }

    #[test]
    fn reconnect_delay_is_bounded() {
        let initial = Duration::from_secs(1);
        let maximum = Duration::from_secs(4);
        assert_eq!(
            next_reconnect_delay(Duration::ZERO, initial, maximum),
            initial
        );
        assert_eq!(
            next_reconnect_delay(initial, initial, maximum),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_reconnect_delay(Duration::from_secs(4), initial, maximum),
            maximum
        );
    }

    #[test]
    fn generated_ids_skip_zero_after_wraparound() {
        let counter = AtomicU64::new(u64::MAX);
        assert_eq!(next_nonzero(&counter), u64::MAX);
        assert_eq!(next_nonzero(&counter), 1);
    }
}
