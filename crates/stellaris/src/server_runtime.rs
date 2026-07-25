// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Quinn-only Stellaris v2 coordinator and trusted relay runtime.
//!
//! Persistent mutations complete before a protocol response is emitted. Live
//! control and relay sessions are deliberately process-local and must
//! authenticate again after a coordinator restart.

use std::{
    collections::HashMap,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration as StdDuration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ipnet::Ipv4Net;
use time::OffsetDateTime;
use tokio::{
    sync::{Semaphore, mpsc, watch},
    task::JoinSet,
};
use tracing::{debug, warn};

use crate::{
    coordinator::{
        AuthenticatedSession, Coordinator, CoordinatorError, CoordinatorEvent, MAX_PENDING_EVENTS,
        SessionCancellationReason, SessionLease, SessionReply,
    },
    coordinator_store::{
        CoordinatorStore, CoordinatorStoreError, EnrollmentCommit, EnrollmentCommitOutcome,
        PersistedEnrollmentResult, Sha256Fingerprint,
    },
    identity::{
        CertificateAuthority, IdentityError, NODE_CERTIFICATE_TTL, VerifiedNodeCertificate,
        csr_spki_fingerprint, extract_verified_node_certificate,
        verify_persisted_node_certificate_der,
    },
    metrics::{PacketDropReason, RuntimeMetrics},
    protocol::{
        AnnounceCandidates, CertificateIssued, ConnectPlan, ConnectRequest, ConnectionRole,
        ControlMessage, ControlWelcome, ENROLLMENT_ALPN, EnrollAccepted, EnrollRequest,
        EnrollmentCodec, EnrollmentMessage, ErrorCode, ErrorMessage, LookupPeer, MessageDirection,
        PeerDescriptor, PeerRecord, ProtocolError, RelayAccepted, RelayMessage, RelayReady,
        RenewCertificate, read_enrollment_message, read_message_from, read_relay_message,
        write_enrollment_message, write_message_for, write_relay_message,
    },
    registry::{EnrollmentTokenDigest, RegistryError, StaticNodeRegistry},
    routing::{
        MAX_OVERLAY_MTU, MIN_OVERLAY_MTU, PacketValidator, RouteError, RouteOutcome,
        RouteRegistration, RouteTable, SessionId,
    },
    transport::{
        AuthenticatedConnection, ConfiguredTransportFactory, ProtocolPurpose,
        ServerTransportConfig, TransportBackend, TransportConnection, TransportEndpoint,
        TransportError, TransportFactory, authenticate_connection_metadata,
    },
};

const CLOSE_NORMAL: u32 = 0;
const CLOSE_PROTOCOL: u32 = 1;
const CLOSE_REPLACED: u32 = 2;
const CLOSE_BUSY: u32 = 3;
const CLOSE_INITIAL_HANDSHAKE_TIMEOUT: u32 = 4;
const INITIAL_HANDSHAKE_TIMEOUT_REASON: &[u8] = b"application handshake timed out";
const ENROLLMENT_INITIAL_HANDSHAKE_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const CONTROL_INITIAL_HANDSHAKE_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const RELAY_INITIAL_HANDSHAKE_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const DEFAULT_CONNECT_PLAN_TTL: StdDuration = StdDuration::from_secs(30);
const RELAY_CONTROL_CHECK_INTERVAL: StdDuration = StdDuration::from_secs(1);
const REQUEST_REPLAY_WINDOW_BITS: u64 = 64;

fn renewal_overlap_capacity(configured: usize) -> usize {
    configured.saturating_mul(2)
}

#[derive(Clone, Copy)]
struct InitialHandshakeDeadline {
    expires_at: tokio::time::Instant,
}

impl InitialHandshakeDeadline {
    fn after(timeout: StdDuration) -> Self {
        Self {
            expires_at: tokio::time::Instant::now() + timeout,
        }
    }

    async fn run<T, F>(self, stage: &'static str, operation: F) -> Result<T, ServerRuntimeError>
    where
        F: Future<Output = Result<T, ServerRuntimeError>>,
    {
        tokio::time::timeout_at(self.expires_at, operation)
            .await
            .map_err(|_| ServerRuntimeError::InitialHandshakeTimeout(stage))?
    }
}

fn close_on_initial_handshake_timeout<T>(
    connection: &dyn TransportConnection,
    result: Result<T, ServerRuntimeError>,
) -> Result<T, ServerRuntimeError> {
    if matches!(result, Err(ServerRuntimeError::InitialHandshakeTimeout(_))) {
        connection.close(
            CLOSE_INITIAL_HANDSHAKE_TIMEOUT,
            INITIAL_HANDSHAKE_TIMEOUT_REASON,
        );
    }
    result
}

/// Paths created by `stellaris server init` and opened by normal startup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerStatePaths {
    pub node_ca_certificate: PathBuf,
    pub node_ca_private_key: PathBuf,
    pub coordinator_state: PathBuf,
}

impl ServerStatePaths {
    pub fn new(
        node_ca_certificate: impl Into<PathBuf>,
        node_ca_private_key: impl Into<PathBuf>,
        coordinator_state: impl Into<PathBuf>,
    ) -> Self {
        Self {
            node_ca_certificate: node_ca_certificate.into(),
            node_ca_private_key: node_ca_private_key.into(),
            coordinator_state: coordinator_state.into(),
        }
    }

    fn validate(&self) -> Result<(), ServerRuntimeError> {
        if self.node_ca_certificate == self.node_ca_private_key
            || self.node_ca_certificate == self.coordinator_state
            || self.node_ca_private_key == self.coordinator_state
        {
            return Err(ServerRuntimeError::InvalidConfiguration(
                "node CA and coordinator state paths must be distinct".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Creates all durable v2 server state. Normal server startup never calls
/// this function and therefore cannot silently replace missing state.
pub fn initialize_server_state(
    paths: &ServerStatePaths,
    overlay: Ipv4Net,
    max_nodes: usize,
) -> Result<(), ServerRuntimeError> {
    paths.validate()?;
    let certificate_exists = path_exists(&paths.node_ca_certificate);
    let key_exists = path_exists(&paths.node_ca_private_key);
    let coordinator_exists = path_exists(&paths.coordinator_state);
    let created_authority = match (certificate_exists, key_exists, coordinator_exists) {
        (false, false, false) => {
            CertificateAuthority::create(&paths.node_ca_certificate, &paths.node_ca_private_key)?;
            true
        }
        // A crash after the complete CA pair was committed but before the
        // coordinator snapshot was installed can safely resume. Strict load
        // validation prevents adopting a lone, corrupt, or mismatched file.
        (true, true, false) => {
            CertificateAuthority::load(&paths.node_ca_certificate, &paths.node_ca_private_key)?;
            false
        }
        _ => return Err(ServerRuntimeError::StateAlreadyInitialized),
    };

    if let Err(error) = CoordinatorStore::initialize(&paths.coordinator_state, overlay, max_nodes) {
        if created_authority {
            CertificateAuthority::rollback_new(
                &paths.node_ca_certificate,
                &paths.node_ca_private_key,
            )?;
        }
        return Err(error.into());
    }
    Ok(())
}

fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Complete runtime input after schema-v2 config and the static registry have
/// been loaded by the CLI.
#[derive(Clone)]
pub struct ServerRuntimeConfig {
    pub overlay: Ipv4Net,
    pub registry: Arc<StaticNodeRegistry>,
    pub state_paths: ServerStatePaths,
    pub server_name: String,
    pub deployment_certificate_pem: String,
    pub deployment_private_key_pem: String,
    pub enrollment_bind: SocketAddr,
    pub control_bind: SocketAddr,
    pub relay_bind: SocketAddr,
    pub max_nodes: usize,
    pub max_control_sessions: usize,
    pub max_pending_control_events: usize,
    pub max_concurrent_enrollments: usize,
    pub max_connections_per_ip: usize,
    pub route_queue_capacity: usize,
    pub mtu: u16,
}

impl std::fmt::Debug for ServerRuntimeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerRuntimeConfig")
            .field("overlay", &self.overlay)
            .field("state_paths", &self.state_paths)
            .field("server_name", &self.server_name)
            .field("deployment_certificate_pem", &"[PEM REDACTED]")
            .field("deployment_private_key_pem", &"[REDACTED]")
            .field("enrollment_bind", &self.enrollment_bind)
            .field("control_bind", &self.control_bind)
            .field("relay_bind", &self.relay_bind)
            .field("max_nodes", &self.max_nodes)
            .field("max_control_sessions", &self.max_control_sessions)
            .field(
                "max_pending_control_events",
                &self.max_pending_control_events,
            )
            .field(
                "max_concurrent_enrollments",
                &self.max_concurrent_enrollments,
            )
            .field("max_connections_per_ip", &self.max_connections_per_ip)
            .field("route_queue_capacity", &self.route_queue_capacity)
            .field("mtu", &self.mtu)
            .finish()
    }
}

impl ServerRuntimeConfig {
    pub fn validate(&self) -> Result<(), ServerRuntimeError> {
        self.state_paths.validate()?;
        if self.registry.overlay() != self.overlay {
            return Err(ServerRuntimeError::InvalidConfiguration(
                "static registry overlay does not match the server network".to_owned(),
            ));
        }
        if self.registry.len() > self.max_nodes || self.max_nodes == 0 || self.max_nodes > 256 {
            return Err(ServerRuntimeError::InvalidConfiguration(
                "max_nodes must cover the registry and be in 1..=256".to_owned(),
            ));
        }
        if self.max_control_sessions == 0 || self.max_control_sessions > self.max_nodes {
            return Err(ServerRuntimeError::InvalidConfiguration(
                "max_control_sessions must be in 1..=max_nodes".to_owned(),
            ));
        }
        if !(2..=MAX_PENDING_EVENTS).contains(&self.max_pending_control_events) {
            return Err(ServerRuntimeError::InvalidConfiguration(format!(
                "max_pending_control_events must be in 2..={MAX_PENDING_EVENTS}"
            )));
        }
        if self.max_concurrent_enrollments == 0
            || self.max_concurrent_enrollments > MAX_PENDING_EVENTS
            || self.max_connections_per_ip == 0
            || self.max_connections_per_ip > self.max_nodes
            || self.route_queue_capacity == 0
            || self.route_queue_capacity > self.max_nodes
        {
            return Err(ServerRuntimeError::InvalidConfiguration(
                "enrollment capacity must be in 1..=1024; per-IP and route capacities must be in 1..=max_nodes"
                    .to_owned(),
            ));
        }
        if !(MIN_OVERLAY_MTU..=MAX_OVERLAY_MTU).contains(&usize::from(self.mtu)) {
            return Err(ServerRuntimeError::InvalidConfiguration(format!(
                "MTU must be in {MIN_OVERLAY_MTU}..={MAX_OVERLAY_MTU}"
            )));
        }
        if self.server_name.is_empty()
            || self.server_name.len() > 253
            || !self.server_name.is_ascii()
        {
            return Err(ServerRuntimeError::InvalidConfiguration(
                "server_name must be 1-253 ASCII bytes".to_owned(),
            ));
        }
        if self.deployment_certificate_pem.is_empty() || self.deployment_private_key_pem.is_empty()
        {
            return Err(ServerRuntimeError::InvalidConfiguration(
                "deployment TLS certificate and private key are required".to_owned(),
            ));
        }
        let listeners = [self.enrollment_bind, self.control_bind, self.relay_bind];
        if listeners.iter().any(|address| address.port() == 0)
            || listeners[0] == listeners[1]
            || listeners[0] == listeners[2]
            || listeners[1] == listeners[2]
        {
            return Err(ServerRuntimeError::InvalidConfiguration(
                "enrollment, control, and relay require distinct non-zero UDP addresses".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Bound enrollment, control, and relay listeners.
pub struct ServerRuntime {
    enrollment: Arc<dyn TransportEndpoint>,
    control: Arc<dyn TransportEndpoint>,
    relay: Arc<dyn TransportEndpoint>,
    core: Arc<ServerCore>,
}

impl ServerRuntime {
    pub async fn bind(config: ServerRuntimeConfig) -> Result<Self, ServerRuntimeError> {
        Self::bind_with_factory(
            config,
            Arc::new(ConfiguredTransportFactory::new(TransportBackend::Quinn)),
        )
        .await
    }

    pub async fn bind_with_factory(
        config: ServerRuntimeConfig,
        factory: Arc<dyn TransportFactory>,
    ) -> Result<Self, ServerRuntimeError> {
        config.validate()?;
        if factory.backend() != TransportBackend::Quinn {
            return Err(ServerRuntimeError::InvalidConfiguration(
                "Stellaris v2 server runtime requires Quinn".to_owned(),
            ));
        }

        let authority = Arc::new(CertificateAuthority::load(
            &config.state_paths.node_ca_certificate,
            &config.state_paths.node_ca_private_key,
        )?);
        let store = Arc::new(CoordinatorStore::open(
            &config.state_paths.coordinator_state,
            config.overlay,
            config.max_nodes,
        )?);
        store.validate_registry(&config.registry)?;
        validate_persisted_enrollment_results(
            &store,
            &authority,
            config.overlay,
            OffsetDateTime::now_utc(),
        )?;
        let coordinator = Arc::new(Coordinator::from_snapshot(
            config.overlay,
            config.max_control_sessions,
            config.max_pending_control_events,
            store.directory_snapshot(),
        )?);
        let routes = Arc::new(RouteTable::new(
            config.overlay,
            config.route_queue_capacity,
        )?);
        let validator = PacketValidator::new(config.overlay, usize::from(config.mtu))?;

        let enrollment = factory
            .server_endpoint(ServerTransportConfig::new(
                config.enrollment_bind,
                &config.server_name,
                &config.deployment_certificate_pem,
                &config.deployment_private_key_pem,
                ProtocolPurpose::Enrollment,
            ))
            .await?;
        let control = factory
            .server_endpoint(
                ServerTransportConfig::new(
                    config.control_bind,
                    &config.server_name,
                    &config.deployment_certificate_pem,
                    &config.deployment_private_key_pem,
                    ProtocolPurpose::Control,
                )
                .with_client_ca_certificate(authority.certificate_pem()),
            )
            .await?;
        let relay = factory
            .server_endpoint(
                ServerTransportConfig::new(
                    config.relay_bind,
                    &config.server_name,
                    &config.deployment_certificate_pem,
                    &config.deployment_private_key_pem,
                    ProtocolPurpose::Relay,
                )
                .with_client_ca_certificate(authority.certificate_pem()),
            )
            .await?;

        let metrics = Arc::new(RuntimeMetrics::default());
        let relay_hub = Arc::new(RelayHub::new(Arc::clone(&routes)));
        let core = Arc::new(ServerCore {
            overlay: config.overlay,
            registry: config.registry,
            authority,
            store,
            coordinator,
            routes,
            validator,
            control_hub: ControlHub::new(Arc::clone(&metrics)),
            relay_hub,
            connection_limiter: Arc::new(ConnectionLimiter::new(config.max_connections_per_ip)),
            registration_gate: tokio::sync::Mutex::new(()),
            enrollment_limit: Arc::new(Semaphore::new(config.max_concurrent_enrollments)),
            control_limit: Arc::new(Semaphore::new(renewal_overlap_capacity(
                config.max_control_sessions,
            ))),
            relay_limit: Arc::new(Semaphore::new(renewal_overlap_capacity(
                config.max_control_sessions,
            ))),
            control_queue_capacity: config.max_pending_control_events,
            mtu: config.mtu,
            connect_plan_ttl: DEFAULT_CONNECT_PLAN_TTL,
            metrics,
        });
        Ok(Self {
            enrollment,
            control,
            relay,
            core,
        })
    }

    pub async fn local_addresses(
        &self,
    ) -> Result<(SocketAddr, SocketAddr, SocketAddr), ServerRuntimeError> {
        Ok((
            self.enrollment.local_address().await?,
            self.control.local_address().await?,
            self.relay.local_address().await?,
        ))
    }

    pub fn metrics(&self) -> Arc<RuntimeMetrics> {
        Arc::clone(&self.core.metrics)
    }

    pub async fn run(self) -> Result<(), ServerRuntimeError> {
        self.run_until(std::future::pending()).await
    }

    pub async fn run_until<F>(self, shutdown: F) -> Result<(), ServerRuntimeError>
    where
        F: Future<Output = ()> + Send,
    {
        let mut listeners = JoinSet::new();
        listeners.spawn(accept_loop(
            self.enrollment,
            Arc::clone(&self.core),
            ListenerRole::Enrollment,
        ));
        listeners.spawn(accept_loop(
            self.control,
            Arc::clone(&self.core),
            ListenerRole::Control,
        ));
        listeners.spawn(accept_loop(self.relay, self.core, ListenerRole::Relay));

        tokio::pin!(shutdown);
        let result = tokio::select! {
            () = &mut shutdown => Ok(()),
            outcome = listeners.join_next() => match outcome {
                Some(Ok(result)) => result,
                Some(Err(error)) => Err(ServerRuntimeError::ListenerTask(error.to_string())),
                None => Err(ServerRuntimeError::ListenerTask("all listeners stopped".to_owned())),
            },
        };
        listeners.abort_all();
        while listeners.join_next().await.is_some() {}
        result
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ListenerRole {
    Enrollment,
    Control,
    Relay,
}

struct ConnectionLimiter {
    max_per_ip: usize,
    active: Mutex<HashMap<(IpAddr, ListenerRole), usize>>,
}

impl ConnectionLimiter {
    fn new(max_per_ip: usize) -> Self {
        Self {
            max_per_ip,
            active: Mutex::new(HashMap::new()),
        }
    }

    fn try_acquire(
        self: &Arc<Self>,
        remote_ip: IpAddr,
        role: ListenerRole,
    ) -> Option<ConnectionPermit> {
        let key = (remote_ip, role);
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = active.entry(key).or_default();
        let limit = match role {
            ListenerRole::Enrollment => self.max_per_ip,
            ListenerRole::Control | ListenerRole::Relay => {
                renewal_overlap_capacity(self.max_per_ip)
            }
        };
        if *count >= limit {
            return None;
        }
        *count += 1;
        drop(active);
        Some(ConnectionPermit {
            limiter: Arc::clone(self),
            key,
        })
    }

    #[cfg(test)]
    fn active_for(&self, remote_ip: IpAddr, role: ListenerRole) -> usize {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(remote_ip, role))
            .copied()
            .unwrap_or(0)
    }
}

struct ConnectionPermit {
    limiter: Arc<ConnectionLimiter>,
    key: (IpAddr, ListenerRole),
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut active = self
            .limiter
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(count) = active.get_mut(&self.key) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            active.remove(&self.key);
        }
    }
}

async fn accept_loop(
    endpoint: Arc<dyn TransportEndpoint>,
    core: Arc<ServerCore>,
    role: ListenerRole,
) -> Result<(), ServerRuntimeError> {
    loop {
        let connection = endpoint.accept().await?;
        let semaphore = match role {
            ListenerRole::Enrollment => Arc::clone(&core.enrollment_limit),
            ListenerRole::Control => Arc::clone(&core.control_limit),
            ListenerRole::Relay => Arc::clone(&core.relay_limit),
        };
        let Ok(permit) = semaphore.try_acquire_owned() else {
            connection.close(CLOSE_BUSY, b"server busy");
            continue;
        };
        let remote_ip = connection.remote_address().ip();
        let Some(ip_permit) = core.connection_limiter.try_acquire(remote_ip, role) else {
            connection.close(CLOSE_BUSY, b"per-IP connection limit reached");
            continue;
        };
        let handler_core = Arc::clone(&core);
        tokio::spawn(async move {
            let _permit = permit;
            let _ip_permit = ip_permit;
            let result = match role {
                ListenerRole::Enrollment => handler_core.handle_enrollment(connection).await,
                ListenerRole::Control => handler_core.handle_control(connection).await,
                ListenerRole::Relay => handler_core.handle_relay(connection).await,
            };
            if let Err(error) = result {
                debug!(?role, %error, "v2 connection closed");
            }
        });
    }
}

struct ServerCore {
    overlay: Ipv4Net,
    registry: Arc<StaticNodeRegistry>,
    authority: Arc<CertificateAuthority>,
    store: Arc<CoordinatorStore>,
    coordinator: Arc<Coordinator>,
    routes: Arc<RouteTable>,
    validator: PacketValidator,
    control_hub: ControlHub,
    relay_hub: Arc<RelayHub>,
    connection_limiter: Arc<ConnectionLimiter>,
    registration_gate: tokio::sync::Mutex<()>,
    enrollment_limit: Arc<Semaphore>,
    control_limit: Arc<Semaphore>,
    relay_limit: Arc<Semaphore>,
    control_queue_capacity: usize,
    mtu: u16,
    connect_plan_ttl: StdDuration,
    metrics: Arc<RuntimeMetrics>,
}

impl ServerCore {
    async fn handle_enrollment(
        self: &Arc<Self>,
        mut connection: Box<dyn TransportConnection>,
    ) -> Result<(), ServerRuntimeError> {
        let deadline = InitialHandshakeDeadline::after(ENROLLMENT_INITIAL_HANDSHAKE_TIMEOUT);
        let request = deadline
            .run("enrollment request", async {
                if connection.negotiated_alpn().await?.as_slice() != ENROLLMENT_ALPN {
                    connection.close(CLOSE_PROTOCOL, b"wrong enrollment ALPN");
                    return Err(ServerRuntimeError::Transport(TransportError::AlpnMismatch));
                }
                let (mut reader, mut writer) = connection.accept_bi().await?;
                match read_enrollment_message(&mut reader, MessageDirection::AgentToCoordinator)
                    .await?
                {
                    EnrollmentMessage::EnrollRequest(request) => Ok(Some((request, writer))),
                    _ => {
                        write_enrollment_message(
                            &mut writer,
                            &EnrollmentMessage::Error(protocol_violation(None)),
                            MessageDirection::CoordinatorToAgent,
                        )
                        .await?;
                        connection.close(CLOSE_PROTOCOL, b"invalid enrollment message");
                        Ok(None)
                    }
                }
            })
            .await;
        let Some((request, mut writer)) =
            close_on_initial_handshake_timeout(connection.as_ref(), request)?
        else {
            return Ok(());
        };

        let core = Arc::clone(self);
        let outcome = deadline
            .run("enrollment persistence", async {
                tokio::task::spawn_blocking(move || core.process_enrollment(request))
                    .await
                    .map_err(|error| ServerRuntimeError::BlockingTask(error.to_string()))
            })
            .await;
        let outcome = match close_on_initial_handshake_timeout(connection.as_ref(), outcome) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.metrics.record_enrollment(false);
                return Err(error);
            }
        };
        self.metrics.record_enrollment(outcome.is_ok());
        let response = match outcome {
            Ok(accepted) => EnrollmentMessage::EnrollAccepted(accepted),
            Err(error) => {
                warn!(%error, "enrollment request rejected");
                EnrollmentMessage::Error(enrollment_public_error(&error))
            }
        };
        let response = deadline
            .run("enrollment response", async {
                write_enrollment_message(
                    &mut writer,
                    &response,
                    MessageDirection::CoordinatorToAgent,
                )
                .await?;
                Ok(())
            })
            .await;
        close_on_initial_handshake_timeout(connection.as_ref(), response)?;
        connection.close(CLOSE_NORMAL, b"enrollment complete");
        Ok(())
    }

    fn process_enrollment(
        &self,
        request: EnrollRequest,
    ) -> Result<EnrollAccepted, ServerRuntimeError> {
        let binding = self
            .registry
            .authenticate_enrollment(&request.node_id, &request.enrollment_token)?;
        let token_sha256 = EnrollmentTokenDigest::from_token_text(&request.enrollment_token)?;
        let encoded_request = EnrollmentCodec::encode(
            &EnrollmentMessage::EnrollRequest(request.clone()),
            MessageDirection::AgentToCoordinator,
        )?;
        let request_sha256 = Sha256Fingerprint::digest(&encoded_request);
        if let Some(result) = self.store.lookup_enrollment(
            &binding.node_id,
            binding.overlay_ip,
            &request.enrollment_id,
            &request_sha256,
            &token_sha256,
        )? {
            return enrollment_result_to_message(
                &request,
                binding.overlay_ip,
                self.overlay,
                self.mtu,
                result,
            );
        }

        let csr_der = STANDARD
            .decode(&request.csr_der_base64)
            .map_err(|_| ServerRuntimeError::InvalidEnrollmentRequest)?;
        let authorized_spki_sha256 = Sha256Fingerprint::from_str(&csr_spki_fingerprint(&csr_der)?)?;
        let issued = self.authority.issue_node_certificate(
            &csr_der,
            &binding.node_id,
            self.overlay,
            binding.overlay_ip,
        )?;
        let persisted = PersistedEnrollmentResult {
            node_certificate_pem: issued.pem().to_owned(),
            node_ca_pem: self.authority.certificate_pem().to_owned(),
            certificate_fingerprint: Sha256Fingerprint::from_str(issued.fingerprint())?,
            certificate_not_after_unix: issued.not_after().unix_timestamp(),
        };
        let commit = EnrollmentCommit {
            node_id: binding.node_id,
            overlay_ip: binding.overlay_ip,
            enrollment_id: request.enrollment_id.clone(),
            request_sha256,
            token_sha256,
            authorized_spki_sha256,
            result: persisted,
        };
        let (EnrollmentCommitOutcome::Committed(result)
        | EnrollmentCommitOutcome::Replayed(result)) = self.store.commit_enrollment(commit)?;
        enrollment_result_to_message(&request, binding.overlay_ip, self.overlay, self.mtu, result)
    }

    fn authenticate_node(
        &self,
        connection: &AuthenticatedConnection,
    ) -> Result<VerifiedNodeCertificate, ServerRuntimeError> {
        let verified = extract_verified_node_certificate(
            connection,
            self.authority.certificate_der(),
            self.overlay,
            OffsetDateTime::now_utc(),
        )?;
        self.authorize_node(&verified)?;
        Ok(verified)
    }

    fn authorize_node(&self, verified: &VerifiedNodeCertificate) -> Result<(), ServerRuntimeError> {
        let binding = self
            .registry
            .enabled_binding(verified.node_id())
            .ok_or(ServerRuntimeError::NodeUnauthorized)?;
        if binding.overlay_ip != verified.overlay_ip() {
            return Err(ServerRuntimeError::NodeUnauthorized);
        }
        let durable = self
            .store
            .node(verified.node_id())
            .ok_or(ServerRuntimeError::NodeUnauthorized)?;
        let authorized = durable
            .authorized_spki_sha256
            .as_ref()
            .is_some_and(|fingerprint| fingerprint.to_string() == verified.spki_fingerprint());
        if durable.overlay_ip != verified.overlay_ip()
            || durable.revocation_epoch.is_some()
            || !authorized
        {
            return Err(ServerRuntimeError::NodeUnauthorized);
        }
        Ok(())
    }

    async fn handle_control(
        self: &Arc<Self>,
        connection: Box<dyn TransportConnection>,
    ) -> Result<(), ServerRuntimeError> {
        let deadline = InitialHandshakeDeadline::after(CONTROL_INITIAL_HANDSHAKE_TIMEOUT);
        let metadata = deadline
            .run("control authentication", async {
                Ok(
                    authenticate_connection_metadata(connection.as_ref(), ProtocolPurpose::Control)
                        .await?,
                )
            })
            .await;
        let metadata = close_on_initial_handshake_timeout(connection.as_ref(), metadata)?;
        let mut connection = AuthenticatedConnection::from_metadata(connection, metadata);
        let verified = self.authenticate_node(&connection)?;
        let stream = deadline
            .run("control stream", async {
                Ok(connection.accept_bi().await?)
            })
            .await;
        let (mut reader, mut writer) =
            close_on_initial_handshake_timeout(connection.as_ref(), stream)?;
        let session_id = SessionId::new();

        let registration = deadline
            .run("control registration", async {
                let _registration = self.registration_gate.lock().await;
                self.authorize_node(&verified)?;
                let incarnation = self
                    .store
                    .advance_incarnation(verified.node_id(), verified.overlay_ip())?;
                let lease = self.coordinator.register_authenticated(
                    AuthenticatedSession::from_verified_certificate(&verified, session_id),
                )?;
                let (sender, receiver) = mpsc::channel(self.control_queue_capacity);
                self.control_hub.install(ActiveControl {
                    lease,
                    node_id: verified.node_id().to_owned(),
                    overlay_ip: verified.overlay_ip(),
                    incarnation,
                    certificate_fingerprint: verified.fingerprint().to_owned(),
                    certificate_not_after: verified.not_after(),
                    candidate_epoch: 0,
                    candidates: Vec::new(),
                    sender,
                });
                Ok((lease, incarnation, receiver))
            })
            .await;
        let (lease, incarnation, mut commands) =
            close_on_initial_handshake_timeout(connection.as_ref(), registration)?;
        self.metrics
            .set_control_sessions(self.control_hub.active_count());
        self.dispatch_coordinator_events();

        let welcome = ControlMessage::ControlWelcome(ControlWelcome {
            session_id: session_id.to_string(),
            incarnation,
            overlay_ip: verified.overlay_ip(),
            overlay_cidr: self.overlay,
            mtu: self.mtu,
            certificate_not_after_unix_seconds: positive_unix(verified.not_after())?,
            coordinator_time_unix_seconds: positive_unix(OffsetDateTime::now_utc())?,
        });
        let run_result = async {
            let welcome = deadline
                .run("ControlWelcome", async {
                    write_message_for(
                        &mut writer,
                        &welcome,
                        MessageDirection::CoordinatorToAgent,
                    )
                    .await?;
                    Ok(())
                })
                .await;
            close_on_initial_handshake_timeout(connection.as_ref(), welcome)?;
            let mut replay = RequestReplayWindow::default();
            let expires_in = seconds_until(verified.not_after());
            let expiry = tokio::time::sleep(expires_in);
            tokio::pin!(expiry);

            loop {
                tokio::select! {
                    incoming = read_message_from(
                        &mut reader,
                        MessageDirection::AgentToCoordinator,
                    ) => {
                        let incoming = incoming?;
                        let action = self.handle_control_message(
                            lease,
                            &verified,
                            incoming,
                            &mut replay,
                        )?;
                        if let Some(message) = action.reply {
                            write_message_for(
                                &mut writer,
                                &message,
                                MessageDirection::CoordinatorToAgent,
                            ).await?;
                        }
                        if action.close {
                            connection.close(CLOSE_PROTOCOL, b"control protocol violation");
                            break;
                        }
                    }
                    command = commands.recv() => match command {
                        Some(ControlCommand::Message(message)) => {
                            write_message_for(
                                &mut writer,
                                &message,
                                MessageDirection::CoordinatorToAgent,
                            ).await?;
                        }
                        Some(ControlCommand::Close) | None => {
                            connection.close(CLOSE_REPLACED, b"control session replaced or revoked");
                            break;
                        }
                    },
                    () = &mut expiry => {
                        connection.close(CLOSE_NORMAL, b"node certificate expired");
                        break;
                    }
                }
            }
            Ok::<(), ServerRuntimeError>(())
        }
        .await;

        self.control_hub.remove(lease);
        self.metrics
            .set_control_sessions(self.control_hub.active_count());
        self.coordinator.close_session(lease);
        self.close_relays_for_control(lease.session_id());
        connection.close(CLOSE_NORMAL, b"control session closed");
        run_result
    }

    fn handle_control_message(
        &self,
        lease: SessionLease,
        verified: &VerifiedNodeCertificate,
        message: ControlMessage,
        replay: &mut RequestReplayWindow,
    ) -> Result<ControlAction, ServerRuntimeError> {
        match message {
            ControlMessage::AnnounceCandidates(announcement) => {
                let reply = self.coordinator.handle(
                    lease,
                    ControlMessage::AnnounceCandidates(announcement.clone()),
                );
                if matches!(reply, SessionReply::Accepted) {
                    self.control_hub.update_candidates(lease, announcement);
                }
                Ok(ControlAction::from_session_reply(reply))
            }
            ControlMessage::LookupPeer(request) => {
                if !replay.accept(request.request_id) {
                    return Ok(ControlAction::reply(replay_error(request.request_id)));
                }
                Ok(ControlAction::reply(self.lookup_peer(request)))
            }
            ControlMessage::ConnectRequest(request) => {
                if !replay.accept(request.request_id) {
                    return Ok(ControlAction::reply(replay_error(request.request_id)));
                }
                Ok(ControlAction::reply(self.plan_connection(lease, request)))
            }
            ControlMessage::RenewCertificate(request) => {
                if !replay.accept(request.request_id) {
                    return Ok(ControlAction::reply(replay_error(request.request_id)));
                }
                Ok(ControlAction::reply(
                    self.renew_certificate(verified, request),
                ))
            }
            ControlMessage::Error(_) => Ok(ControlAction::accepted()),
            ControlMessage::PeerRecord(_)
            | ControlMessage::PeerRevoked(_)
            | ControlMessage::ControlWelcome(_)
            | ControlMessage::ConnectPlan(_)
            | ControlMessage::CertificateIssued(_) => Ok(ControlAction {
                reply: Some(ControlMessage::Error(protocol_violation(None))),
                close: true,
            }),
        }
    }

    fn lookup_peer(&self, request: LookupPeer) -> ControlMessage {
        match self
            .control_hub
            .peer_record(request.overlay_ip, request.request_id)
        {
            Some(record) => ControlMessage::PeerRecord(record),
            None => ControlMessage::Error(ErrorMessage {
                request_id: Some(request.request_id),
                code: ErrorCode::PeerNotFound,
                message: "peer is not currently online".to_owned(),
                retryable: true,
            }),
        }
    }

    fn plan_connection(&self, requester: SessionLease, request: ConnectRequest) -> ControlMessage {
        let Some(local) = self.control_hub.descriptor_for_lease(requester) else {
            return session_inactive_error(Some(request.request_id));
        };
        let Some(target) = self.control_hub.descriptor_by_overlay(request.overlay_ip) else {
            return peer_offline_error(request.request_id);
        };
        if local.overlay_ip == target.overlay_ip {
            return ControlMessage::Error(ErrorMessage {
                request_id: Some(request.request_id),
                code: ErrorCode::InvalidRequest,
                message: "cannot create a P2P plan to the local node".to_owned(),
                retryable: false,
            });
        }

        let connection_id = SessionId::new().to_string();
        let now = OffsetDateTime::now_utc();
        let Ok(plan_ttl_seconds) = i64::try_from(self.connect_plan_ttl.as_secs()) else {
            return internal_error(Some(request.request_id));
        };
        let Ok(now_unix_seconds) = positive_unix(now) else {
            return internal_error(Some(request.request_id));
        };
        let Ok(planned_expiry) = positive_unix(now + time::Duration::seconds(plan_ttl_seconds))
        else {
            return internal_error(Some(request.request_id));
        };
        let expires_at_unix_seconds = planned_expiry
            .min(local.certificate_not_after_unix_seconds)
            .min(target.certificate_not_after_unix_seconds);
        if expires_at_unix_seconds <= now_unix_seconds {
            return peer_offline_error(request.request_id);
        }
        let target_plan = ControlMessage::ConnectPlan(ConnectPlan {
            request_id: request.request_id,
            connection_id: connection_id.clone(),
            role: ConnectionRole::Responder,
            peer: local,
            expires_at_unix_seconds,
        });
        if !self
            .control_hub
            .send_to_overlay(request.overlay_ip, target_plan)
        {
            return peer_offline_error(request.request_id);
        }
        ControlMessage::ConnectPlan(ConnectPlan {
            request_id: request.request_id,
            connection_id,
            role: ConnectionRole::Initiator,
            peer: target,
            expires_at_unix_seconds,
        })
    }

    fn renew_certificate(
        &self,
        verified: &VerifiedNodeCertificate,
        request: RenewCertificate,
    ) -> ControlMessage {
        let result = (|| {
            self.authorize_node(verified)?;
            let csr_der = STANDARD
                .decode(&request.csr_der_base64)
                .map_err(|_| ServerRuntimeError::InvalidEnrollmentRequest)?;
            if csr_spki_fingerprint(&csr_der)? != verified.spki_fingerprint() {
                return Err(ServerRuntimeError::RenewalKeyMismatch);
            }
            let issued = self.authority.issue_node_certificate(
                &csr_der,
                verified.node_id(),
                self.overlay,
                verified.overlay_ip(),
            )?;
            Ok(ControlMessage::CertificateIssued(CertificateIssued {
                request_id: request.request_id,
                certificate_chain_der_base64: vec![
                    STANDARD.encode(issued.der()),
                    STANDARD.encode(self.authority.certificate_der()),
                ],
                not_before_unix_seconds: positive_unix(issued.not_before())?,
                not_after_unix_seconds: positive_unix(issued.not_after())?,
            }))
        })();
        self.metrics.record_renewal(result.is_ok());
        match result {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, node_id = verified.node_id(), "certificate renewal rejected");
                ControlMessage::Error(ErrorMessage {
                    request_id: Some(request.request_id),
                    code: if matches!(error, ServerRuntimeError::RenewalKeyMismatch) {
                        ErrorCode::InvalidRequest
                    } else {
                        ErrorCode::Internal
                    },
                    message: "certificate renewal failed".to_owned(),
                    retryable: !matches!(error, ServerRuntimeError::RenewalKeyMismatch),
                })
            }
        }
    }

    fn dispatch_coordinator_events(&self) {
        while let Some(event) = self.coordinator.pop_event() {
            match event {
                CoordinatorEvent::BroadcastRevocation(notification) => {
                    // A failed staged replacement can leave an older current
                    // session only in the runtime hub, so revocation must use
                    // the durable overlay binding rather than one coordinator lease.
                    for session_id in self.control_hub.close_overlay(notification.overlay_ip) {
                        self.close_relays_for_control(session_id);
                    }
                    self.control_hub
                        .broadcast(ControlMessage::PeerRevoked(notification));
                    self.metrics
                        .set_control_sessions(self.control_hub.active_count());
                }
                CoordinatorEvent::SendRevocation {
                    lease,
                    notification,
                } => {
                    self.control_hub
                        .send_to_lease(lease, ControlMessage::PeerRevoked(notification));
                }
                CoordinatorEvent::CancelSession { lease, reason } => match reason {
                    SessionCancellationReason::Replaced => {}
                    SessionCancellationReason::Revoked => {
                        for session_id in self.control_hub.close_overlay_for_lease(lease) {
                            self.close_relays_for_control(session_id);
                        }
                        self.metrics
                            .set_control_sessions(self.control_hub.active_count());
                    }
                },
            }
        }
    }

    fn close_relays_for_control(&self, session_id: SessionId) {
        let closed = self.relay_hub.close_control(session_id);
        for _ in 0..closed {
            self.metrics.record_path_transition();
        }
        self.metrics
            .set_relay_sessions(self.relay_hub.active_count());
    }

    async fn handle_relay(
        self: &Arc<Self>,
        connection: Box<dyn TransportConnection>,
    ) -> Result<(), ServerRuntimeError> {
        let deadline = InitialHandshakeDeadline::after(RELAY_INITIAL_HANDSHAKE_TIMEOUT);
        let metadata = deadline
            .run("relay authentication", async {
                Ok(
                    authenticate_connection_metadata(connection.as_ref(), ProtocolPurpose::Relay)
                        .await?,
                )
            })
            .await;
        let metadata = close_on_initial_handshake_timeout(connection.as_ref(), metadata)?;
        let mut connection = AuthenticatedConnection::from_metadata(connection, metadata);
        let verified = self.authenticate_node(&connection)?;
        let stream = deadline
            .run("relay stream", async { Ok(connection.accept_bi().await?) })
            .await;
        let (mut reader, mut writer) =
            close_on_initial_handshake_timeout(connection.as_ref(), stream)?;
        let bind = deadline
            .run("RelayBind", async {
                Ok(read_relay_message(&mut reader, MessageDirection::AgentToCoordinator).await?)
            })
            .await;
        let bind = close_on_initial_handshake_timeout(connection.as_ref(), bind)?;
        let bind = match bind {
            RelayMessage::RelayBind(bind) => bind,
            _ => {
                let response = deadline
                    .run("RelayBind rejection", async {
                        write_relay_message(
                            &mut writer,
                            &RelayMessage::Error(protocol_violation(None)),
                            MessageDirection::CoordinatorToAgent,
                        )
                        .await?;
                        Ok(())
                    })
                    .await;
                close_on_initial_handshake_timeout(connection.as_ref(), response)?;
                connection.close(CLOSE_PROTOCOL, b"expected RelayBind");
                return Ok(());
            }
        };
        let control_session = SessionId::from_str(&bind.control_session_id)
            .map_err(|_| ServerRuntimeError::InvalidRelayBinding)?;
        if !self.control_hub.binding_matches(
            verified.node_id(),
            verified.overlay_ip(),
            control_session,
            bind.incarnation,
        ) {
            let response = deadline
                .run("RelayBind rejection", async {
                    write_relay_message(
                        &mut writer,
                        &RelayMessage::Error(session_error()),
                        MessageDirection::CoordinatorToAgent,
                    )
                    .await?;
                    Ok(())
                })
                .await;
            close_on_initial_handshake_timeout(connection.as_ref(), response)?;
            connection.close(CLOSE_PROTOCOL, b"relay control lease is not current");
            return Ok(());
        }

        let relay_session = SessionId::new();
        let accepted = deadline
            .run("RelayAccepted", async {
                write_relay_message(
                    &mut writer,
                    &RelayMessage::RelayAccepted(RelayAccepted {
                        relay_session_id: relay_session.to_string(),
                        mtu: self.mtu,
                        max_datagram_size: self.mtu,
                    }),
                    MessageDirection::CoordinatorToAgent,
                )
                .await?;
                Ok(())
            })
            .await;
        close_on_initial_handshake_timeout(connection.as_ref(), accepted)?;
        let ready = deadline
            .run("RelayReady", async {
                Ok(read_relay_message(&mut reader, MessageDirection::AgentToCoordinator).await?)
            })
            .await;
        let ready = close_on_initial_handshake_timeout(connection.as_ref(), ready)?;
        if ready
            != RelayMessage::RelayReady(RelayReady {
                relay_session_id: relay_session.to_string(),
            })
            || !self.control_hub.binding_matches(
                verified.node_id(),
                verified.overlay_ip(),
                control_session,
                bind.incarnation,
            )
        {
            connection.close(CLOSE_PROTOCOL, b"invalid RelayReady or stale control lease");
            return Err(ServerRuntimeError::InvalidRelayBinding);
        }

        let (mut outbound, mut close) = self.control_hub.activate_relay(
            self.relay_hub.as_ref(),
            verified.node_id(),
            verified.overlay_ip(),
            control_session,
            bind.incarnation,
            relay_session,
        )?;
        write_relay_message(
            &mut writer,
            &RelayMessage::RelayReady(RelayReady {
                relay_session_id: relay_session.to_string(),
            }),
            MessageDirection::CoordinatorToAgent,
        )
        .await?;
        self.metrics
            .set_control_sessions(self.control_hub.active_count());
        self.metrics
            .set_relay_sessions(self.relay_hub.active_count());
        self.metrics.record_path_transition();
        if !self.control_hub.binding_matches(
            verified.node_id(),
            verified.overlay_ip(),
            control_session,
            bind.incarnation,
        ) {
            if self.relay_hub.remove(verified.overlay_ip(), relay_session) {
                self.metrics.record_path_transition();
            }
            self.metrics
                .set_relay_sessions(self.relay_hub.active_count());
            connection.close(CLOSE_REPLACED, b"control lease ended during relay setup");
            return Ok(());
        }

        let run_result = async {
            let expiry = tokio::time::sleep(seconds_until(verified.not_after()));
            tokio::pin!(expiry);
            let mut control_check = tokio::time::interval(RELAY_CONTROL_CHECK_INTERVAL);
            loop {
                tokio::select! {
                    incoming = connection.recv_datagram() => {
                        let packet = incoming?;
                        match self.routes.route_from(
                            relay_session,
                            verified.overlay_ip(),
                            packet,
                            &self.validator,
                        ) {
                            Ok(RouteOutcome::Forwarded) => self.metrics.record_relay_packet(),
                            Ok(RouteOutcome::DroppedQueueFull) => self
                                .metrics
                                .record_drop_reason(PacketDropReason::QueueFull),
                            Err(RouteError::DestinationOffline(_)) => self
                                .metrics
                                .record_drop_reason(PacketDropReason::UnavailablePath),
                            Err(RouteError::StaleSession) => break,
                            Err(error) => {
                                self
                                    .metrics
                                    .record_drop_reason(PacketDropReason::InvalidPacket);
                                return Err(ServerRuntimeError::Route(error));
                            }
                        }
                    }
                    packet = outbound.recv() => match packet {
                        Some(packet) => match connection.send_datagram(packet) {
                            Ok(()) => self
                                .metrics
                                .observe_relay_queue_depth(outbound.len().saturating_add(1)),
                            Err(TransportError::DatagramQueueFull) => {
                                self.routes.record_transport_drop();
                                self
                                    .metrics
                                    .record_drop_reason(PacketDropReason::QueueFull);
                            }
                            Err(TransportError::DatagramTooLarge) => {
                                self.routes.record_transport_drop();
                                self
                                    .metrics
                                    .record_drop_reason(PacketDropReason::InvalidPacket);
                            }
                            Err(error) => {
                                self
                                    .metrics
                                    .record_drop_reason(PacketDropReason::Transport);
                                return Err(ServerRuntimeError::Transport(error));
                            }
                        },
                        None => break,
                    },
                    _ = close.changed() => break,
                    _ = control_check.tick() => {
                        if !self.control_hub.binding_matches(
                            verified.node_id(),
                            verified.overlay_ip(),
                            control_session,
                            bind.incarnation,
                        ) {
                            break;
                        }
                    }
                    () = &mut expiry => break,
                }
            }
            Ok::<(), ServerRuntimeError>(())
        }
        .await;

        if let Ok(dropped) = connection.dropped_incoming_datagrams() {
            self.routes.record_transport_drops(dropped);
            self.metrics
                .record_drops_by_reason(PacketDropReason::Transport, dropped);
        }
        if self.relay_hub.remove(verified.overlay_ip(), relay_session) {
            self.metrics.record_path_transition();
        }
        self.metrics
            .set_relay_sessions(self.relay_hub.active_count());
        connection.close(CLOSE_NORMAL, b"relay session closed");
        run_result
    }
}

fn validate_persisted_enrollment_results(
    store: &CoordinatorStore,
    authority: &CertificateAuthority,
    overlay: Ipv4Net,
    now: OffsetDateTime,
) -> Result<(), ServerRuntimeError> {
    for node in store.snapshot().nodes {
        let Some(enrollment) = node.enrollment else {
            continue;
        };
        let result = enrollment.result;
        let persisted_ca = pem::parse(&result.node_ca_pem)
            .map_err(|_| ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
        if persisted_ca.tag() != "CERTIFICATE"
            || persisted_ca.contents() != authority.certificate_der()
        {
            return Err(ServerRuntimeError::InvalidPersistedEnrollmentResult);
        }

        let leaf = pem::parse(&result.node_certificate_pem)
            .map_err(|_| ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
        if leaf.tag() != "CERTIFICATE" {
            return Err(ServerRuntimeError::InvalidPersistedEnrollmentResult);
        }
        let verified = verify_persisted_node_certificate_der(
            leaf.contents(),
            authority.certificate_der(),
            overlay,
            now,
        )
        .map_err(|_| ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
        let fingerprint = Sha256Fingerprint::from_str(verified.fingerprint())
            .map_err(|_| ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
        let spki_fingerprint = Sha256Fingerprint::from_str(verified.spki_fingerprint())
            .map_err(|_| ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
        let authorized_spki = node
            .authorized_spki_sha256
            .ok_or(ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
        if verified.node_id() != node.node_id
            || verified.overlay_ip() != node.overlay_ip
            || !fingerprint.constant_time_eq(&result.certificate_fingerprint)
            || !spki_fingerprint.constant_time_eq(&authorized_spki)
            || verified.not_after().unix_timestamp() != result.certificate_not_after_unix
        {
            return Err(ServerRuntimeError::InvalidPersistedEnrollmentResult);
        }
    }
    Ok(())
}

fn enrollment_result_to_message(
    request: &EnrollRequest,
    overlay_ip: Ipv4Addr,
    overlay_cidr: Ipv4Net,
    mtu: u16,
    result: PersistedEnrollmentResult,
) -> Result<EnrollAccepted, ServerRuntimeError> {
    let leaf = pem::parse(&result.node_certificate_pem)
        .map_err(|_| ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
    let authority = pem::parse(&result.node_ca_pem)
        .map_err(|_| ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
    let not_after = u64::try_from(result.certificate_not_after_unix)
        .map_err(|_| ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
    let ttl = u64::try_from(NODE_CERTIFICATE_TTL.whole_seconds())
        .map_err(|_| ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
    let not_before = not_after
        .checked_sub(ttl)
        .ok_or(ServerRuntimeError::InvalidPersistedEnrollmentResult)?;
    Ok(EnrollAccepted {
        enrollment_id: request.enrollment_id.clone(),
        node_id: request.node_id.clone(),
        overlay_ip,
        overlay_cidr,
        mtu,
        certificate_chain_der_base64: vec![
            STANDARD.encode(leaf.contents()),
            STANDARD.encode(authority.contents()),
        ],
        node_ca_certificate_der_base64: STANDARD.encode(authority.contents()),
        not_before_unix_seconds: not_before,
        not_after_unix_seconds: not_after,
    })
}

fn enrollment_public_error(error: &ServerRuntimeError) -> ErrorMessage {
    let retryable = matches!(
        error,
        ServerRuntimeError::CoordinatorStore(CoordinatorStoreError::Io { .. })
            | ServerRuntimeError::CoordinatorStore(CoordinatorStoreError::Poisoned)
            | ServerRuntimeError::BlockingTask(_)
    );
    ErrorMessage {
        request_id: None,
        code: if retryable {
            ErrorCode::Internal
        } else {
            ErrorCode::InvalidRequest
        },
        message: if retryable {
            "enrollment could not be committed".to_owned()
        } else {
            "enrollment credentials or request are invalid".to_owned()
        },
        retryable,
    }
}

struct ControlHub {
    state: Mutex<ControlHubState>,
    metrics: Arc<RuntimeMetrics>,
}

#[derive(Default)]
struct ControlHubState {
    sessions: HashMap<SessionId, ActiveControl>,
    current_by_overlay: HashMap<Ipv4Addr, SessionId>,
    staged_by_overlay: HashMap<Ipv4Addr, SessionId>,
}

#[derive(Clone)]
struct ActiveControl {
    lease: SessionLease,
    node_id: String,
    overlay_ip: Ipv4Addr,
    incarnation: u64,
    certificate_fingerprint: String,
    certificate_not_after: OffsetDateTime,
    candidate_epoch: u64,
    candidates: Vec<crate::protocol::Candidate>,
    sender: mpsc::Sender<ControlCommand>,
}

#[derive(Clone, Debug)]
enum ControlCommand {
    Message(ControlMessage),
    Close,
}

impl ControlHub {
    fn new(metrics: Arc<RuntimeMetrics>) -> Self {
        Self {
            state: Mutex::new(ControlHubState::default()),
            metrics,
        }
    }

    fn try_send(&self, sender: &mpsc::Sender<ControlCommand>, command: ControlCommand) -> bool {
        if sender.try_send(command).is_err() {
            return false;
        }
        self.metrics
            .observe_control_queue_depth(sender.max_capacity() - sender.capacity());
        true
    }

    fn install(&self, control: ActiveControl) {
        let mut state = self.lock();
        let overlay_ip = control.overlay_ip;
        let session_id = control.lease.session_id();
        if state.current_by_overlay.contains_key(&overlay_ip)
            || state.staged_by_overlay.contains_key(&overlay_ip)
        {
            if let Some(previous_id) = state.staged_by_overlay.insert(overlay_ip, session_id)
                && previous_id != session_id
                && let Some(previous) = state.sessions.remove(&previous_id)
            {
                self.try_send(&previous.sender, ControlCommand::Close);
            }
        } else {
            state.current_by_overlay.insert(overlay_ip, session_id);
        }
        state.sessions.insert(session_id, control);
    }

    fn remove(&self, lease: SessionLease) {
        let mut state = self.lock();
        let Some(existing) = state.sessions.get(&lease.session_id()) else {
            return;
        };
        if existing.lease != lease {
            return;
        }
        let overlay_ip = existing.overlay_ip;
        state.sessions.remove(&lease.session_id());
        if state.current_by_overlay.get(&overlay_ip) == Some(&lease.session_id()) {
            state.current_by_overlay.remove(&overlay_ip);
        }
        if state.staged_by_overlay.get(&overlay_ip) == Some(&lease.session_id()) {
            state.staged_by_overlay.remove(&overlay_ip);
        }
    }

    fn update_candidates(&self, lease: SessionLease, announcement: AnnounceCandidates) {
        let mut state = self.lock();
        if let Some(control) = state
            .sessions
            .get_mut(&lease.session_id())
            .filter(|control| control.lease == lease)
        {
            control.candidate_epoch = announcement.epoch;
            control.candidates = announcement.candidates;
        }
    }

    fn binding_matches(
        &self,
        node_id: &str,
        overlay_ip: Ipv4Addr,
        session_id: SessionId,
        incarnation: u64,
    ) -> bool {
        let state = self.lock();
        (state.current_by_overlay.get(&overlay_ip) == Some(&session_id)
            || state.staged_by_overlay.get(&overlay_ip) == Some(&session_id))
            && state.sessions.get(&session_id).is_some_and(|control| {
                control.node_id == node_id
                    && control.overlay_ip == overlay_ip
                    && control.incarnation == incarnation
            })
    }

    fn activate_relay(
        &self,
        relay_hub: &RelayHub,
        node_id: &str,
        overlay_ip: Ipv4Addr,
        control_session: SessionId,
        incarnation: u64,
        relay_session: SessionId,
    ) -> Result<(mpsc::Receiver<bytes::Bytes>, watch::Receiver<bool>), ServerRuntimeError> {
        let mut state = self.lock();
        let is_current = state.current_by_overlay.get(&overlay_ip) == Some(&control_session);
        let is_staged = state.staged_by_overlay.get(&overlay_ip) == Some(&control_session);
        let matches = (is_current || is_staged)
            && state.sessions.get(&control_session).is_some_and(|control| {
                control.node_id == node_id
                    && control.overlay_ip == overlay_ip
                    && control.incarnation == incarnation
            });
        if !matches {
            return Err(ServerRuntimeError::InvalidRelayBinding);
        }

        let installed = relay_hub.install(
            node_id.to_owned(),
            overlay_ip,
            relay_session,
            control_session,
        )?;
        if is_staged {
            state.staged_by_overlay.remove(&overlay_ip);
            if let Some(previous_id) = state.current_by_overlay.insert(overlay_ip, control_session)
                && previous_id != control_session
                && let Some(previous) = state.sessions.remove(&previous_id)
            {
                self.try_send(&previous.sender, ControlCommand::Close);
            }
        }
        Ok(installed)
    }

    fn close_overlay_for_lease(&self, lease: SessionLease) -> Vec<SessionId> {
        let overlay_ip = {
            let state = self.lock();
            state
                .sessions
                .get(&lease.session_id())
                .filter(|control| control.lease == lease)
                .map(|control| control.overlay_ip)
        };
        overlay_ip.map_or_else(Vec::new, |overlay_ip| self.close_overlay(overlay_ip))
    }

    fn close_overlay(&self, overlay_ip: Ipv4Addr) -> Vec<SessionId> {
        let mut state = self.lock();
        state.current_by_overlay.remove(&overlay_ip);
        state.staged_by_overlay.remove(&overlay_ip);
        let sessions: Vec<_> = state
            .sessions
            .iter()
            .filter_map(|(session_id, control)| {
                (control.overlay_ip == overlay_ip).then_some(*session_id)
            })
            .collect();
        for session_id in &sessions {
            if let Some(control) = state.sessions.remove(session_id) {
                self.try_send(&control.sender, ControlCommand::Close);
            }
        }
        sessions
    }

    fn descriptor_by_overlay(&self, overlay_ip: Ipv4Addr) -> Option<PeerDescriptor> {
        let state = self.lock();
        let session_id = state.current_by_overlay.get(&overlay_ip)?;
        state.sessions.get(session_id).and_then(descriptor)
    }

    fn descriptor_for_lease(&self, lease: SessionLease) -> Option<PeerDescriptor> {
        let state = self.lock();
        let control = state
            .sessions
            .get(&lease.session_id())
            .filter(|control| control.lease == lease)
            .filter(|control| {
                state.current_by_overlay.get(&control.overlay_ip) == Some(&lease.session_id())
            })?;
        descriptor(control)
    }

    fn peer_record(&self, overlay_ip: Ipv4Addr, request_id: u64) -> Option<PeerRecord> {
        let state = self.lock();
        let session_id = state.current_by_overlay.get(&overlay_ip)?;
        let control = state.sessions.get(session_id)?;
        Some(PeerRecord {
            request_id,
            node_id: control.node_id.clone(),
            overlay_ip,
            incarnation: control.incarnation,
            session_id: control.lease.session_id().to_string(),
            certificate_fingerprint: control.certificate_fingerprint.clone(),
            certificate_not_after_unix_seconds: positive_unix(control.certificate_not_after)
                .ok()?,
            epoch: control.candidate_epoch,
            candidates: control.candidates.clone(),
        })
    }

    fn send_to_overlay(&self, overlay_ip: Ipv4Addr, message: ControlMessage) -> bool {
        let state = self.lock();
        let Some(session_id) = state.current_by_overlay.get(&overlay_ip) else {
            return false;
        };
        state
            .sessions
            .get(session_id)
            .is_some_and(|control| self.try_send(&control.sender, ControlCommand::Message(message)))
    }

    fn send_to_lease(&self, lease: SessionLease, message: ControlMessage) {
        if let Some(control) = self
            .lock()
            .sessions
            .get(&lease.session_id())
            .filter(|control| control.lease == lease)
        {
            self.try_send(&control.sender, ControlCommand::Message(message));
        }
    }

    fn broadcast(&self, message: ControlMessage) {
        for control in self.lock().sessions.values() {
            self.try_send(&control.sender, ControlCommand::Message(message.clone()));
        }
    }

    fn active_count(&self) -> usize {
        self.lock().sessions.len()
    }

    fn lock(&self) -> MutexGuard<'_, ControlHubState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn descriptor(control: &ActiveControl) -> Option<PeerDescriptor> {
    Some(PeerDescriptor {
        node_id: control.node_id.clone(),
        overlay_ip: control.overlay_ip,
        incarnation: control.incarnation,
        session_id: control.lease.session_id().to_string(),
        certificate_fingerprint: control.certificate_fingerprint.clone(),
        certificate_not_after_unix_seconds: positive_unix(control.certificate_not_after).ok()?,
        candidate_epoch: control.candidate_epoch,
        candidates: control.candidates.clone(),
    })
}

struct RelayHub {
    routes: Arc<RouteTable>,
    state: Mutex<HashMap<Ipv4Addr, ActiveRelay>>,
}

struct ActiveRelay {
    relay_session: SessionId,
    control_session: SessionId,
    close: watch::Sender<bool>,
}

impl RelayHub {
    fn new(routes: Arc<RouteTable>) -> Self {
        Self {
            routes,
            state: Mutex::new(HashMap::new()),
        }
    }

    fn install(
        &self,
        node_id: String,
        overlay_ip: Ipv4Addr,
        relay_session: SessionId,
        control_session: SessionId,
    ) -> Result<(mpsc::Receiver<bytes::Bytes>, watch::Receiver<bool>), RouteError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (close, close_receiver) = watch::channel(false);
        let receiver = self.routes.replace_active(RouteRegistration {
            node_id,
            overlay_ip,
            session_id: relay_session,
        })?;
        let previous = state.insert(
            overlay_ip,
            ActiveRelay {
                relay_session,
                control_session,
                close,
            },
        );
        if let Some(previous) = previous {
            let _ = previous.close.send(true);
        }
        Ok((receiver, close_receiver))
    }

    fn remove(&self, overlay_ip: Ipv4Addr, relay_session: SessionId) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .get(&overlay_ip)
            .is_some_and(|active| active.relay_session == relay_session)
        {
            state.remove(&overlay_ip);
            self.routes.remove_session(overlay_ip, relay_session);
            return true;
        }
        false
    }

    fn close_control(&self, control_session: SessionId) -> usize {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let overlays: Vec<_> = state
            .iter()
            .filter_map(|(overlay, active)| {
                (active.control_session == control_session).then_some(*overlay)
            })
            .collect();
        let closed = overlays.len();
        for overlay in overlays {
            if let Some(active) = state.remove(&overlay) {
                self.routes.remove_session(overlay, active.relay_session);
                let _ = active.close.send(true);
            }
        }
        closed
    }

    fn active_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RequestReplayWindow {
    high_watermark: u64,
    seen: u64,
}

impl RequestReplayWindow {
    fn accept(&mut self, request_id: u64) -> bool {
        if request_id == 0 {
            return false;
        }
        if self.high_watermark == 0 {
            self.high_watermark = request_id;
            self.seen = 1;
            return true;
        }
        if request_id > self.high_watermark {
            let shift = request_id - self.high_watermark;
            self.seen = if shift >= REQUEST_REPLAY_WINDOW_BITS {
                1
            } else {
                (self.seen << shift) | 1
            };
            self.high_watermark = request_id;
            return true;
        }
        let distance = self.high_watermark - request_id;
        if distance >= REQUEST_REPLAY_WINDOW_BITS {
            return false;
        }
        let bit = 1_u64 << distance;
        if self.seen & bit != 0 {
            return false;
        }
        self.seen |= bit;
        true
    }
}

struct ControlAction {
    reply: Option<ControlMessage>,
    close: bool,
}

impl ControlAction {
    const fn accepted() -> Self {
        Self {
            reply: None,
            close: false,
        }
    }

    fn reply(message: ControlMessage) -> Self {
        Self {
            reply: Some(message),
            close: false,
        }
    }

    fn from_session_reply(reply: SessionReply) -> Self {
        Self {
            reply: reply.message().cloned(),
            close: reply.should_close(),
        }
    }
}

fn positive_unix(value: OffsetDateTime) -> Result<u64, ServerRuntimeError> {
    u64::try_from(value.unix_timestamp()).map_err(|_| ServerRuntimeError::InvalidTimestamp)
}

fn seconds_until(deadline: OffsetDateTime) -> StdDuration {
    let seconds = deadline
        .unix_timestamp()
        .saturating_sub(OffsetDateTime::now_utc().unix_timestamp());
    StdDuration::from_secs(u64::try_from(seconds).unwrap_or(0))
}

fn protocol_violation(request_id: Option<u64>) -> ErrorMessage {
    ErrorMessage {
        request_id,
        code: ErrorCode::ProtocolViolation,
        message: "message is not valid in this protocol state".to_owned(),
        retryable: false,
    }
}

fn replay_error(request_id: u64) -> ControlMessage {
    ControlMessage::Error(ErrorMessage {
        request_id: Some(request_id),
        code: ErrorCode::ReplayDetected,
        message: "request ID was recently used in this control session".to_owned(),
        retryable: false,
    })
}

fn peer_offline_error(request_id: u64) -> ControlMessage {
    ControlMessage::Error(ErrorMessage {
        request_id: Some(request_id),
        code: ErrorCode::PeerNotFound,
        message: "peer is not currently online".to_owned(),
        retryable: true,
    })
}

fn session_inactive_error(request_id: Option<u64>) -> ControlMessage {
    ControlMessage::Error(ErrorMessage {
        request_id,
        code: ErrorCode::Revoked,
        message: "control session is not active".to_owned(),
        retryable: false,
    })
}

fn internal_error(request_id: Option<u64>) -> ControlMessage {
    ControlMessage::Error(ErrorMessage {
        request_id,
        code: ErrorCode::Internal,
        message: "coordinator could not complete the request".to_owned(),
        retryable: true,
    })
}

fn session_error() -> ErrorMessage {
    ErrorMessage {
        request_id: None,
        code: ErrorCode::Revoked,
        message: "relay requires the current authenticated control session".to_owned(),
        retryable: true,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServerRuntimeError {
    #[error("invalid server runtime configuration: {0}")]
    InvalidConfiguration(String),
    #[error("v2 server state is already initialized or only partially absent")]
    StateAlreadyInitialized,
    #[error("node is not authorized by the static registry and durable SPKI binding")]
    NodeUnauthorized,
    #[error("enrollment request is invalid")]
    InvalidEnrollmentRequest,
    #[error("persisted enrollment result is invalid")]
    InvalidPersistedEnrollmentResult,
    #[error("certificate renewal must retain the current node public key")]
    RenewalKeyMismatch,
    #[error("relay bind does not name the current control lease")]
    InvalidRelayBinding,
    #[error("initial application handshake timed out while waiting for {0}")]
    InitialHandshakeTimeout(&'static str),
    #[error("timestamp cannot be represented by protocol v2")]
    InvalidTimestamp,
    #[error("listener task failed: {0}")]
    ListenerTask(String),
    #[error("blocking server task failed: {0}")]
    BlockingTask(String),
    #[error("identity operation failed: {0}")]
    Identity(#[from] IdentityError),
    #[error("static registry operation failed: {0}")]
    Registry(#[from] RegistryError),
    #[error("coordinator persistence failed: {0}")]
    CoordinatorStore(#[from] CoordinatorStoreError),
    #[error("coordinator state operation failed: {0}")]
    Coordinator(#[from] CoordinatorError),
    #[error("v2 protocol operation failed: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("overlay route operation failed: {0}")]
    Route(#[from] RouteError),
    #[error("transport operation failed: {0}")]
    Transport(#[from] TransportError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use crate::{
        identity::{IssuedCertificate, NodeKey},
        registry::{ENROLLMENT_TOKEN_BYTES, EnrollmentToken},
    };
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(unix)]
    static NEXT_STATE_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    #[cfg(unix)]
    struct StateTestDirectory(PathBuf);

    #[cfg(unix)]
    impl StateTestDirectory {
        fn new(label: &str) -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let sequence = NEXT_STATE_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "stellaris-server-state-{label}-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create server state test directory");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("secure server state test directory");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    #[cfg(unix)]
    impl Drop for StateTestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    struct PersistedEnrollmentFixture {
        _directory: StateTestDirectory,
        authority: CertificateAuthority,
        store: CoordinatorStore,
        csr_der: Vec<u8>,
        issued: IssuedCertificate,
        overlay: Ipv4Net,
        overlay_ip: Ipv4Addr,
    }

    #[cfg(unix)]
    impl PersistedEnrollmentFixture {
        fn new(label: &str) -> Self {
            let directory = StateTestDirectory::new(label);
            let overlay = "10.91.0.0/24".parse().expect("fixture overlay");
            let overlay_ip = "10.91.0.2".parse().expect("fixture address");
            let authority = CertificateAuthority::create(
                directory.join("node-ca.pem"),
                directory.join("node-ca.key"),
            )
            .expect("create node CA fixture");
            let node_key = NodeKey::load_or_create(directory.join("node.key"))
                .expect("create node key fixture");
            let csr_der = node_key.create_csr_der().expect("create CSR fixture");
            let issued = authority
                .issue_node_certificate(&csr_der, "edge-a", overlay, overlay_ip)
                .expect("issue node certificate fixture");
            let store =
                CoordinatorStore::initialize(directory.join("coordinator.json"), overlay, 256)
                    .expect("initialize coordinator fixture");
            Self {
                _directory: directory,
                authority,
                store,
                csr_der,
                issued,
                overlay,
                overlay_ip,
            }
        }

        fn result_for(&self, issued: &IssuedCertificate) -> PersistedEnrollmentResult {
            PersistedEnrollmentResult {
                node_certificate_pem: issued.pem().to_owned(),
                node_ca_pem: self.authority.certificate_pem().to_owned(),
                certificate_fingerprint: Sha256Fingerprint::from_str(issued.fingerprint())
                    .expect("certificate fingerprint fixture"),
                certificate_not_after_unix: issued.not_after().unix_timestamp(),
            }
        }

        fn commit(&self, result: PersistedEnrollmentResult, authorized_spki: Sha256Fingerprint) {
            self.store
                .commit_enrollment(EnrollmentCommit {
                    node_id: "edge-a".to_owned(),
                    overlay_ip: self.overlay_ip,
                    enrollment_id: "00000000-0000-0000-0000-000000000001".to_owned(),
                    request_sha256: Sha256Fingerprint::digest(b"enrollment request"),
                    token_sha256: EnrollmentTokenDigest::from_token(&EnrollmentToken::from_bytes(
                        [7; ENROLLMENT_TOKEN_BYTES],
                    )),
                    authorized_spki_sha256: authorized_spki,
                    result,
                })
                .expect("commit enrollment fixture");
        }

        fn csr_spki(&self) -> Sha256Fingerprint {
            Sha256Fingerprint::from_str(
                &csr_spki_fingerprint(&self.csr_der).expect("CSR SPKI fingerprint fixture"),
            )
            .expect("parse CSR SPKI fingerprint fixture")
        }
    }

    #[derive(Default)]
    struct CloseRecordingConnection {
        close: Mutex<Option<(u32, Vec<u8>)>>,
    }

    impl crate::transport::sealed::Sealed for CloseRecordingConnection {}

    #[async_trait::async_trait]
    impl TransportConnection for CloseRecordingConnection {
        async fn open_bi(
            &self,
        ) -> Result<
            (
                crate::transport::BoxReadStream,
                crate::transport::BoxWriteStream,
            ),
            TransportError,
        > {
            std::future::pending().await
        }

        async fn accept_bi(
            &mut self,
        ) -> Result<
            (
                crate::transport::BoxReadStream,
                crate::transport::BoxWriteStream,
            ),
            TransportError,
        > {
            std::future::pending().await
        }

        fn send_datagram(&self, _packet: bytes::Bytes) -> Result<(), TransportError> {
            Err(TransportError::ConnectionClosed)
        }

        async fn recv_datagram(&mut self) -> Result<bytes::Bytes, TransportError> {
            std::future::pending().await
        }

        fn remote_address(&self) -> SocketAddr {
            SocketAddr::from(([192, 0, 2, 1], 443))
        }

        async fn negotiated_alpn(&self) -> Result<Vec<u8>, TransportError> {
            std::future::pending().await
        }

        async fn peer_certificate_chain_der(&self) -> Result<Vec<Vec<u8>>, TransportError> {
            std::future::pending().await
        }

        fn close(&self, code: u32, reason: &[u8]) {
            *self
                .close
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((code, reason.to_vec()));
        }

        async fn closed(&self) -> TransportError {
            TransportError::ConnectionClosed
        }
    }

    fn config() -> ServerRuntimeConfig {
        let overlay = "10.91.0.0/24".parse().expect("fixture overlay");
        ServerRuntimeConfig {
            overlay,
            registry: Arc::new(
                StaticNodeRegistry::new(overlay, []).expect("empty fixture registry"),
            ),
            state_paths: ServerStatePaths::new(
                "/tmp/stellaris-test-ca.pem",
                "/tmp/stellaris-test-ca.key",
                "/tmp/stellaris-test-state.json",
            ),
            server_name: "localhost".to_owned(),
            deployment_certificate_pem: "certificate".to_owned(),
            deployment_private_key_pem: "private-key".to_owned(),
            enrollment_bind: "127.0.0.1:7400".parse().unwrap(),
            control_bind: "127.0.0.1:7401".parse().unwrap(),
            relay_bind: "127.0.0.1:7402".parse().unwrap(),
            max_nodes: 256,
            max_control_sessions: 256,
            max_pending_control_events: 256,
            max_concurrent_enrollments: 16,
            max_connections_per_ip: 8,
            route_queue_capacity: 256,
            mtu: 1100,
        }
    }

    fn test_control(
        session_id: SessionId,
        generation: u64,
        incarnation: u64,
    ) -> (ActiveControl, mpsc::Receiver<ControlCommand>) {
        let overlay_ip = "10.91.0.2".parse().expect("fixture address");
        let certificate_not_after = OffsetDateTime::now_utc() + time::Duration::hours(1);
        let lease = SessionLease::for_test(session_id, generation, certificate_not_after);
        let (sender, receiver) = mpsc::channel(4);
        (
            ActiveControl {
                lease,
                node_id: "edge-a".to_owned(),
                overlay_ip,
                incarnation,
                certificate_fingerprint: format!("{generation:064x}"),
                certificate_not_after,
                candidate_epoch: 0,
                candidates: Vec::new(),
                sender,
            },
            receiver,
        )
    }

    #[test]
    fn config_requires_three_distinct_listener_addresses() {
        let mut config = config();
        assert!(config.validate().is_ok());
        config.relay_bind = config.control_bind;
        assert!(matches!(
            config.validate(),
            Err(ServerRuntimeError::InvalidConfiguration(_))
        ));
    }

    #[test]
    fn config_rejects_v1_shaped_capacity_and_path_aliases() {
        let mut runtime_config = config();
        runtime_config.max_nodes = 0;
        assert!(runtime_config.validate().is_err());
        runtime_config.max_nodes = 256;
        runtime_config.max_pending_control_events = MAX_PENDING_EVENTS + 1;
        assert!(runtime_config.validate().is_err());
        runtime_config.max_pending_control_events = 256;
        runtime_config.route_queue_capacity = 257;
        assert!(runtime_config.validate().is_err());
        runtime_config.route_queue_capacity = 256;
        runtime_config.state_paths.coordinator_state =
            runtime_config.state_paths.node_ca_private_key.clone();
        assert!(runtime_config.validate().is_err());

        let mut single_node = config();
        single_node.max_nodes = 1;
        single_node.max_control_sessions = 1;
        single_node.max_pending_control_events = 2;
        single_node.max_concurrent_enrollments = 2;
        single_node.max_connections_per_ip = 1;
        single_node.route_queue_capacity = 1;
        assert!(single_node.validate().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn server_state_initialization_failure_rolls_back_and_can_be_retried() {
        let directory = StateTestDirectory::new("transaction-rollback");
        let paths = ServerStatePaths::new(
            directory.join("node-ca.pem"),
            directory.join("node-ca.key"),
            directory.join("coordinator.json"),
        );
        let overlay = "10.91.0.0/24".parse().expect("fixture overlay");

        assert!(matches!(
            initialize_server_state(&paths, overlay, 0),
            Err(ServerRuntimeError::CoordinatorStore(
                CoordinatorStoreError::InvalidCapacity(0)
            ))
        ));
        assert!(!paths.node_ca_certificate.exists());
        assert!(!paths.node_ca_private_key.exists());
        assert!(!paths.coordinator_state.exists());

        initialize_server_state(&paths, overlay, 256)
            .expect("retry complete server initialization after rollback");
        CertificateAuthority::load(&paths.node_ca_certificate, &paths.node_ca_private_key)
            .expect("load committed CA");
        CoordinatorStore::open(&paths.coordinator_state, overlay, 256)
            .expect("open committed coordinator state");
    }

    #[cfg(unix)]
    #[test]
    fn server_state_initialization_resumes_after_complete_ca_crash_window() {
        let directory = StateTestDirectory::new("resume-after-ca");
        let paths = ServerStatePaths::new(
            directory.join("node-ca.pem"),
            directory.join("node-ca.key"),
            directory.join("coordinator.json"),
        );
        let overlay = "10.91.0.0/24".parse().expect("fixture overlay");
        let before =
            CertificateAuthority::create(&paths.node_ca_certificate, &paths.node_ca_private_key)
                .expect("commit CA before simulated crash")
                .certificate_der()
                .to_vec();

        initialize_server_state(&paths, overlay, 256)
            .expect("resume initialization from a complete validated CA pair");
        let after =
            CertificateAuthority::load(&paths.node_ca_certificate, &paths.node_ca_private_key)
                .expect("load resumed CA");
        assert_eq!(after.certificate_der(), before);
        CoordinatorStore::open(&paths.coordinator_state, overlay, 256)
            .expect("coordinator state was completed");
    }

    #[cfg(unix)]
    #[test]
    fn server_state_initialization_rejects_a_lone_ca_key() {
        let directory = StateTestDirectory::new("reject-lone-key");
        let paths = ServerStatePaths::new(
            directory.join("node-ca.pem"),
            directory.join("node-ca.key"),
            directory.join("coordinator.json"),
        );
        NodeKey::load_or_create(&paths.node_ca_private_key).expect("create lone key fixture");

        assert!(matches!(
            initialize_server_state(
                &paths,
                "10.91.0.0/24".parse().expect("fixture overlay"),
                256,
            ),
            Err(ServerRuntimeError::StateAlreadyInitialized)
        ));
        assert!(paths.node_ca_private_key.exists());
        assert!(!paths.node_ca_certificate.exists());
        assert!(!paths.coordinator_state.exists());
    }

    #[cfg(unix)]
    #[test]
    fn persisted_enrollment_validation_accepts_expired_cryptographic_history() {
        let fixture = PersistedEnrollmentFixture::new("expired-history");
        let verified = verify_persisted_node_certificate_der(
            fixture.issued.der(),
            fixture.authority.certificate_der(),
            fixture.overlay,
            fixture.issued.not_after() + time::Duration::seconds(1),
        )
        .expect("verify expired certificate fixture");
        assert_eq!(verified.node_id(), "edge-a");
        assert_eq!(verified.overlay_ip(), fixture.overlay_ip);
        assert_eq!(
            Sha256Fingerprint::from_str(verified.spki_fingerprint())
                .expect("verified SPKI fingerprint"),
            fixture.csr_spki(),
            "CSR and issued-certificate SPKI fingerprints must use the same DER encoding",
        );
        fixture.commit(fixture.result_for(&fixture.issued), fixture.csr_spki());

        validate_persisted_enrollment_results(
            &fixture.store,
            &fixture.authority,
            fixture.overlay,
            fixture.issued.not_after() + time::Duration::seconds(1),
        )
        .expect("expired enrollment result remains a verified idempotent response");
    }

    #[cfg(unix)]
    #[test]
    fn persisted_enrollment_validation_rejects_cryptographic_inconsistency() {
        enum Tamper {
            NodeCa,
            Signature,
            NodeId,
            OverlayIp,
            Spki,
            Fingerprint,
            NotAfter,
        }

        for (label, tamper) in [
            ("node-ca", Tamper::NodeCa),
            ("signature", Tamper::Signature),
            ("node-id", Tamper::NodeId),
            ("overlay-ip", Tamper::OverlayIp),
            ("spki", Tamper::Spki),
            ("fingerprint", Tamper::Fingerprint),
            ("not-after", Tamper::NotAfter),
        ] {
            let fixture = PersistedEnrollmentFixture::new(label);
            let mut result = fixture.result_for(&fixture.issued);
            let mut authorized_spki = fixture.csr_spki();
            match tamper {
                Tamper::NodeCa => {
                    let other_authority = CertificateAuthority::create(
                        fixture._directory.join("other-ca.pem"),
                        fixture._directory.join("other-ca.key"),
                    )
                    .expect("create mismatched CA fixture");
                    result.node_ca_pem = other_authority.certificate_pem().to_owned();
                }
                Tamper::Signature => {
                    let mut corrupted = fixture.issued.der().to_vec();
                    let last = corrupted.last_mut().expect("non-empty certificate DER");
                    *last ^= 1;
                    result.node_certificate_pem =
                        pem::encode(&pem::Pem::new("CERTIFICATE", corrupted.clone()));
                    result.certificate_fingerprint = Sha256Fingerprint::digest(&corrupted);
                }
                Tamper::NodeId => {
                    let issued = fixture
                        .authority
                        .issue_node_certificate(
                            &fixture.csr_der,
                            "edge-b",
                            fixture.overlay,
                            fixture.overlay_ip,
                        )
                        .expect("issue wrong-node fixture");
                    result = fixture.result_for(&issued);
                }
                Tamper::OverlayIp => {
                    let issued = fixture
                        .authority
                        .issue_node_certificate(
                            &fixture.csr_der,
                            "edge-a",
                            fixture.overlay,
                            "10.91.0.3".parse().expect("alternate fixture address"),
                        )
                        .expect("issue wrong-address fixture");
                    result = fixture.result_for(&issued);
                }
                Tamper::Spki => {
                    authorized_spki = Sha256Fingerprint::digest(b"unauthorized SPKI");
                }
                Tamper::Fingerprint => {
                    result.certificate_fingerprint =
                        Sha256Fingerprint::digest(b"wrong certificate fingerprint");
                }
                Tamper::NotAfter => {
                    result.certificate_not_after_unix += 1;
                }
            }
            fixture.commit(result, authorized_spki);

            assert!(
                matches!(
                    validate_persisted_enrollment_results(
                        &fixture.store,
                        &fixture.authority,
                        fixture.overlay,
                        OffsetDateTime::now_utc(),
                    ),
                    Err(ServerRuntimeError::InvalidPersistedEnrollmentResult)
                ),
                "tamper case {label} was accepted"
            );
        }
    }

    #[test]
    fn replay_window_accepts_bounded_reordering_once() {
        let mut window = RequestReplayWindow::default();
        assert!(window.accept(100));
        assert!(window.accept(102));
        assert!(window.accept(101));
        assert!(!window.accept(101));
        assert!(window.accept(200));
        assert!(!window.accept(100));
        assert!(!window.accept(0));
    }

    #[test]
    fn per_ip_limits_are_role_scoped_and_released_by_raii() {
        let limiter = Arc::new(ConnectionLimiter::new(1));
        let ip: IpAddr = "192.0.2.10".parse().unwrap();
        let control = limiter
            .try_acquire(ip, ListenerRole::Control)
            .expect("first control connection");
        let replacement_control = limiter
            .try_acquire(ip, ListenerRole::Control)
            .expect("renewal control overlap");
        assert!(limiter.try_acquire(ip, ListenerRole::Control).is_none());
        let relay = limiter
            .try_acquire(ip, ListenerRole::Relay)
            .expect("relay has an independent role limit");
        let replacement_relay = limiter
            .try_acquire(ip, ListenerRole::Relay)
            .expect("renewal relay overlap");
        assert!(limiter.try_acquire(ip, ListenerRole::Relay).is_none());
        let enrollment = limiter
            .try_acquire(ip, ListenerRole::Enrollment)
            .expect("first enrollment connection");
        assert!(limiter.try_acquire(ip, ListenerRole::Enrollment).is_none());
        assert_eq!(limiter.active_for(ip, ListenerRole::Control), 2);
        assert_eq!(limiter.active_for(ip, ListenerRole::Relay), 2);

        drop(control);
        assert_eq!(limiter.active_for(ip, ListenerRole::Control), 1);
        assert!(limiter.try_acquire(ip, ListenerRole::Control).is_some());
        drop(replacement_control);
        drop(relay);
        drop(replacement_relay);
        drop(enrollment);
    }

    #[test]
    fn maximum_node_capacity_allows_one_renewal_overlap() {
        assert_eq!(renewal_overlap_capacity(256), 512);
        assert!(config().validate().is_ok());
    }

    #[test]
    fn staged_control_does_not_replace_current_until_relay_activation() {
        let metrics = Arc::new(RuntimeMetrics::default());
        let hub = ControlHub::new(metrics);
        let old_id = SessionId::new();
        let staged_id = SessionId::new();
        let (old, mut old_commands) = test_control(old_id, 1, 1);
        let old_lease = old.lease;
        let (staged, _staged_commands) = test_control(staged_id, 2, 2);
        let staged_lease = staged.lease;
        hub.install(old);
        hub.install(staged);

        assert_eq!(
            hub.descriptor_by_overlay("10.91.0.2".parse().unwrap())
                .expect("current descriptor")
                .session_id,
            old_id.to_string()
        );
        assert!(hub.descriptor_for_lease(staged_lease).is_none());
        assert!(hub.binding_matches("edge-a", "10.91.0.2".parse().unwrap(), old_id, 1));
        assert!(hub.binding_matches("edge-a", "10.91.0.2".parse().unwrap(), staged_id, 2));
        assert!(matches!(
            old_commands.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        hub.remove(staged_lease);
        assert!(hub.descriptor_for_lease(old_lease).is_some());
    }

    #[test]
    fn relay_ready_promotes_staged_control_and_preserves_aba_guards() {
        let address = "10.91.0.2".parse().unwrap();
        let routes = Arc::new(RouteTable::new("10.91.0.0/24".parse().unwrap(), 4).unwrap());
        let relay_hub = RelayHub::new(Arc::clone(&routes));
        let hub = ControlHub::new(Arc::new(RuntimeMetrics::default()));
        let old_control = SessionId::new();
        let staged_control = SessionId::new();
        let old_relay = SessionId::new();
        let new_relay = SessionId::new();
        let (old, mut old_commands) = test_control(old_control, 1, 1);
        let old_lease = old.lease;
        let (staged, _staged_commands) = test_control(staged_control, 2, 2);
        hub.install(old);
        let (_old_packets, old_close) = relay_hub
            .install("edge-a".to_owned(), address, old_relay, old_control)
            .unwrap();
        hub.install(staged);

        let (_new_packets, _new_close) = hub
            .activate_relay(&relay_hub, "edge-a", address, staged_control, 2, new_relay)
            .unwrap();
        assert!(matches!(old_commands.try_recv(), Ok(ControlCommand::Close)));
        assert!(*old_close.borrow());
        assert!(routes.is_current_session(address, new_relay));
        assert_eq!(
            hub.descriptor_by_overlay(address).unwrap().session_id,
            staged_control.to_string()
        );

        hub.remove(old_lease);
        relay_hub.remove(address, old_relay);
        assert!(routes.is_current_session(address, new_relay));
    }

    #[test]
    fn replacing_staged_control_and_revocation_retire_every_overlay_session() {
        let address = "10.91.0.2".parse().unwrap();
        let routes = Arc::new(RouteTable::new("10.91.0.0/24".parse().unwrap(), 4).unwrap());
        let relay_hub = RelayHub::new(Arc::clone(&routes));
        let hub = ControlHub::new(Arc::new(RuntimeMetrics::default()));
        let current_id = SessionId::new();
        let first_staged_id = SessionId::new();
        let last_staged_id = SessionId::new();
        let relay_id = SessionId::new();
        let (current, _current_commands) = test_control(current_id, 1, 1);
        let (first_staged, mut first_commands) = test_control(first_staged_id, 2, 2);
        let first_lease = first_staged.lease;
        let (last_staged, _last_commands) = test_control(last_staged_id, 3, 3);
        let last_lease = last_staged.lease;
        hub.install(current);
        relay_hub
            .install("edge-a".to_owned(), address, relay_id, current_id)
            .unwrap();
        hub.install(first_staged);
        hub.install(last_staged);
        assert!(matches!(
            first_commands.try_recv(),
            Ok(ControlCommand::Close)
        ));
        hub.remove(first_lease);
        assert!(hub.binding_matches("edge-a", address, current_id, 1));
        assert!(hub.binding_matches("edge-a", address, last_staged_id, 3));

        let retired = hub.close_overlay_for_lease(last_lease);
        assert_eq!(retired.len(), 2);
        for session_id in retired {
            relay_hub.close_control(session_id);
        }
        assert!(!hub.binding_matches("edge-a", address, current_id, 1));
        assert!(!hub.binding_matches("edge-a", address, last_staged_id, 3));
        assert!(routes.is_empty());
    }

    #[test]
    fn revocation_by_overlay_retires_current_after_staged_control_fails() {
        let address = "10.91.0.2".parse().unwrap();
        let routes = Arc::new(RouteTable::new("10.91.0.0/24".parse().unwrap(), 4).unwrap());
        let relay_hub = RelayHub::new(Arc::clone(&routes));
        let hub = ControlHub::new(Arc::new(RuntimeMetrics::default()));
        let current_id = SessionId::new();
        let staged_id = SessionId::new();
        let relay_id = SessionId::new();
        let (current, mut current_commands) = test_control(current_id, 1, 1);
        let (staged, _staged_commands) = test_control(staged_id, 2, 2);
        let staged_lease = staged.lease;

        hub.install(current);
        relay_hub
            .install("edge-a".to_owned(), address, relay_id, current_id)
            .unwrap();
        hub.install(staged);
        hub.remove(staged_lease);

        let retired = hub.close_overlay(address);
        assert_eq!(retired, vec![current_id]);
        for session_id in retired {
            relay_hub.close_control(session_id);
        }

        assert!(matches!(
            current_commands.try_recv(),
            Ok(ControlCommand::Close)
        ));
        assert!(!hub.binding_matches("edge-a", address, current_id, 1));
        assert!(routes.is_empty());
    }

    #[test]
    fn control_hub_records_command_queue_depth_high_watermark() {
        let metrics = Arc::new(RuntimeMetrics::default());
        let hub = ControlHub::new(Arc::clone(&metrics));
        let (sender, mut receiver) = mpsc::channel(3);

        assert!(hub.try_send(&sender, ControlCommand::Close));
        assert!(hub.try_send(&sender, ControlCommand::Close));
        assert_eq!(metrics.snapshot().control_queue_depth_high_watermark, 2);

        assert!(matches!(receiver.try_recv(), Ok(ControlCommand::Close)));
        assert!(hub.try_send(&sender, ControlCommand::Close));
        assert_eq!(
            metrics.snapshot().control_queue_depth_high_watermark,
            2,
            "draining and refilling below the peak must not lower the watermark",
        );

        assert!(hub.try_send(&sender, ControlCommand::Close));
        assert!(!hub.try_send(&sender, ControlCommand::Close));
        assert_eq!(
            metrics.snapshot().control_queue_depth_high_watermark,
            3,
            "a rejected command must not report a depth beyond channel capacity",
        );
    }

    #[tokio::test]
    async fn initial_handshake_deadline_reports_the_waiting_stage() {
        let deadline = InitialHandshakeDeadline {
            expires_at: tokio::time::Instant::now(),
        };
        let result = deadline
            .run(
                "RelayReady",
                std::future::pending::<Result<(), ServerRuntimeError>>(),
            )
            .await;

        assert!(matches!(
            result,
            Err(ServerRuntimeError::InitialHandshakeTimeout("RelayReady"))
        ));
    }

    #[test]
    fn initial_handshake_timeout_uses_a_dedicated_close_code() {
        let connection = CloseRecordingConnection::default();
        let result: Result<(), ServerRuntimeError> = close_on_initial_handshake_timeout(
            &connection,
            Err(ServerRuntimeError::InitialHandshakeTimeout(
                "control stream",
            )),
        );

        assert!(matches!(
            result,
            Err(ServerRuntimeError::InitialHandshakeTimeout(
                "control stream"
            ))
        ));
        assert_eq!(
            *connection
                .close
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            Some((
                CLOSE_INITIAL_HANDSHAKE_TIMEOUT,
                INITIAL_HANDSHAKE_TIMEOUT_REASON.to_vec(),
            ))
        );

        let connection = CloseRecordingConnection::default();
        let result: Result<(), ServerRuntimeError> = close_on_initial_handshake_timeout(
            &connection,
            Err(ServerRuntimeError::InvalidRelayBinding),
        );
        assert!(matches!(
            result,
            Err(ServerRuntimeError::InvalidRelayBinding)
        ));
        assert!(
            connection
                .close
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none()
        );
    }

    #[test]
    fn stale_relay_cleanup_cannot_remove_a_ready_replacement() {
        let overlay = "10.91.0.0/24".parse().unwrap();
        let routes = Arc::new(RouteTable::new(overlay, 4).unwrap());
        let hub = RelayHub::new(Arc::clone(&routes));
        let address = "10.91.0.2".parse().unwrap();
        let old_relay = SessionId::new();
        let old_control = SessionId::new();
        let new_relay = SessionId::new();
        let new_control = SessionId::new();

        let (_old_packets, _old_close) = hub
            .install("edge-a".to_owned(), address, old_relay, old_control)
            .unwrap();
        let (_new_packets, _new_close) = hub
            .install("edge-a".to_owned(), address, new_relay, new_control)
            .unwrap();
        hub.remove(address, old_relay);
        hub.close_control(old_control);

        assert!(routes.is_current_session(address, new_relay));
        hub.close_control(new_control);
        assert!(routes.is_empty());
    }

    #[test]
    fn public_enrollment_errors_do_not_expose_credentials_or_node_existence() {
        let authentication = ServerRuntimeError::Registry(RegistryError::AuthenticationFailed);
        let conflict =
            ServerRuntimeError::CoordinatorStore(CoordinatorStoreError::EnrollmentTokenConsumed);
        assert_eq!(
            enrollment_public_error(&authentication),
            enrollment_public_error(&conflict)
        );
    }
}
