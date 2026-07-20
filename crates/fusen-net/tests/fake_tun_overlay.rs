// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use bytes::Bytes;
#[cfg(all(
    feature = "backend-quinn",
    feature = "backend-s2n",
    feature = "backend-gm-quic"
))]
use fusen_net::transport::{
    ClientTransportConfig, ServerTransportConfig, TransportBackend, make_client_endpoint,
    make_server_endpoint,
};
use fusen_net::{
    address::{NodeToken, StaticAddressAllocator, StaticBinding, TokenDigest},
    control::{ControlMessage, Ready, Register, read_message, write_message},
    data_plane::PacketValidator,
    node::{EdgeRuntime, NodeError, NodeIdentity, RelayRuntime},
    routing::RouteTable,
    transport::{
        BoxReadStream, BoxWriteStream, TransportConnection, TransportEndpoint, TransportError,
    },
    tun::{PacketDevice, RouteLease, RouteManager, TunConfig, TunError, TunFactory},
};
use ipnet::Ipv4Net;
#[cfg(all(
    feature = "backend-quinn",
    feature = "backend-s2n",
    feature = "backend-gm-quic"
))]
use rcgen::{CertifiedKey, generate_simple_self_signed};
use tokio::{
    io::{DuplexStream, duplex, split},
    sync::{Mutex as AsyncMutex, Notify, Semaphore, mpsc, mpsc::error::TrySendError, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};

const TEST_TIMEOUT: Duration = Duration::from_secs(2);
const RECOVERY_TARGET: Duration = Duration::from_secs(30);
const QUIET_PERIOD: Duration = Duration::from_millis(150);
const MTU: u16 = 1100;
const RELAY_ADDRESS: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 4433);
const EDGE_A_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 42, 0, 2);
const EDGE_B_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 42, 0, 3);

struct MemoryConnection {
    control: AsyncMutex<Option<DuplexStream>>,
    datagram_sender: mpsc::Sender<Bytes>,
    datagram_receiver: AsyncMutex<mpsc::Receiver<Bytes>>,
    received_observer: Option<mpsc::Sender<Bytes>>,
    remote_address: SocketAddr,
}

impl MemoryConnection {
    async fn take_control_stream(&self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
        let stream = self
            .control
            .lock()
            .await
            .take()
            .ok_or(TransportError::UnsupportedRole)?;
        let (reader, writer) = split(stream);
        Ok((Box::pin(reader), Box::pin(writer)))
    }
}

#[async_trait]
impl TransportConnection for MemoryConnection {
    async fn open_bi(&self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
        self.take_control_stream().await
    }

    async fn accept_bi(&mut self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
        self.take_control_stream().await
    }

    fn send_datagram(&self, packet: Bytes) -> Result<(), TransportError> {
        self.datagram_sender
            .try_send(packet)
            .map_err(|error| match error {
                TrySendError::Full(_) => TransportError::DatagramQueueFull,
                TrySendError::Closed(_) => TransportError::ConnectionClosed,
            })
    }

    async fn recv_datagram(&mut self) -> Result<Bytes, TransportError> {
        let packet = self
            .datagram_receiver
            .lock()
            .await
            .recv()
            .await
            .ok_or(TransportError::ConnectionClosed)?;
        if let Some(observer) = &self.received_observer {
            let _ = observer.try_send(packet.clone());
        }
        Ok(packet)
    }

    fn remote_address(&self) -> SocketAddr {
        self.remote_address
    }

    async fn closed(&self) -> TransportError {
        self.datagram_sender.closed().await;
        TransportError::ConnectionClosed
    }
}

struct MemoryServerEndpoint {
    connections: AsyncMutex<mpsc::Receiver<Box<dyn TransportConnection>>>,
}

#[async_trait]
impl TransportEndpoint for MemoryServerEndpoint {
    async fn accept(&self) -> Result<Box<dyn TransportConnection>, TransportError> {
        self.connections
            .lock()
            .await
            .recv()
            .await
            .ok_or(TransportError::EndpointClosed)
    }

    async fn connect(
        &self,
        _address: SocketAddr,
        _server_name: &str,
    ) -> Result<Box<dyn TransportConnection>, TransportError> {
        Err(TransportError::UnsupportedRole)
    }
}

struct MemoryClientEndpoint {
    connections: mpsc::Sender<Box<dyn TransportConnection>>,
    received_observer: mpsc::Sender<Bytes>,
    next_port: AtomicU16,
}

#[async_trait]
impl TransportEndpoint for MemoryClientEndpoint {
    async fn accept(&self) -> Result<Box<dyn TransportConnection>, TransportError> {
        Err(TransportError::UnsupportedRole)
    }

    async fn connect(
        &self,
        _address: SocketAddr,
        _server_name: &str,
    ) -> Result<Box<dyn TransportConnection>, TransportError> {
        let client_port = self.next_port.fetch_add(1, Ordering::Relaxed);
        let client_address = SocketAddr::from(([127, 0, 0, 1], client_port));
        let (client, server) = memory_connection_pair(
            client_address,
            RELAY_ADDRESS,
            self.received_observer.clone(),
        );
        self.connections
            .send(Box::new(server))
            .await
            .map_err(|_| TransportError::EndpointClosed)?;
        Ok(Box::new(client))
    }
}

fn memory_connection_pair(
    client_address: SocketAddr,
    server_address: SocketAddr,
    received_observer: mpsc::Sender<Bytes>,
) -> (MemoryConnection, MemoryConnection) {
    let (client_control, server_control) = duplex(64 * 1024);
    let (client_datagrams, server_datagram_receiver) = mpsc::channel(256);
    let (server_datagrams, client_datagram_receiver) = mpsc::channel(256);

    let client = MemoryConnection {
        control: AsyncMutex::new(Some(client_control)),
        datagram_sender: client_datagrams,
        datagram_receiver: AsyncMutex::new(client_datagram_receiver),
        received_observer: None,
        remote_address: server_address,
    };
    let server = MemoryConnection {
        control: AsyncMutex::new(Some(server_control)),
        datagram_sender: server_datagrams,
        datagram_receiver: AsyncMutex::new(server_datagram_receiver),
        received_observer: Some(received_observer),
        remote_address: client_address,
    };
    (client, server)
}

struct MemoryProbe {
    received: AsyncMutex<mpsc::Receiver<Bytes>>,
}

impl MemoryProbe {
    async fn wait_for_relay_receive(&self, expected: &Bytes) {
        timeout(TEST_TIMEOUT, async {
            loop {
                let packet = self
                    .received
                    .lock()
                    .await
                    .recv()
                    .await
                    .expect("memory transport observer must remain open");
                if packet == *expected {
                    return;
                }
            }
        })
        .await
        .expect("relay did not receive the expected datagram");
    }
}

fn memory_endpoints() -> (
    Arc<dyn TransportEndpoint>,
    Arc<dyn TransportEndpoint>,
    Arc<MemoryProbe>,
) {
    let (connection_sender, connection_receiver) = mpsc::channel(256);
    let (received_sender, received_receiver) = mpsc::channel(256);
    let server: Arc<dyn TransportEndpoint> = Arc::new(MemoryServerEndpoint {
        connections: AsyncMutex::new(connection_receiver),
    });
    let client: Arc<dyn TransportEndpoint> = Arc::new(MemoryClientEndpoint {
        connections: connection_sender,
        received_observer: received_sender,
        next_port: AtomicU16::new(20_000),
    });
    let probe = Arc::new(MemoryProbe {
        received: AsyncMutex::new(received_receiver),
    });
    (server, client, probe)
}

struct FakePacketDevice {
    name: String,
    mtu: u16,
    incoming: Arc<AsyncMutex<mpsc::Receiver<Bytes>>>,
    outgoing: mpsc::Sender<Bytes>,
    read_generation: Arc<AtomicUsize>,
    read_started_notify: Arc<Notify>,
    read_started: bool,
}

#[async_trait]
impl PacketDevice for FakePacketDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn mtu(&self) -> u16 {
        self.mtu
    }

    async fn read_packet(&mut self) -> Result<Bytes, TunError> {
        if !self.read_started {
            self.read_started = true;
            self.read_generation.fetch_add(1, Ordering::Release);
            self.read_started_notify.notify_waiters();
        }
        self.incoming
            .lock()
            .await
            .recv()
            .await
            .ok_or(TunError::Closed)
    }

    async fn write_packet(&mut self, packet: Bytes) -> Result<(), TunError> {
        self.outgoing
            .send(packet)
            .await
            .map_err(|_| TunError::Closed)
    }
}

struct FakeTunFactory {
    name: String,
    incoming: Arc<AsyncMutex<mpsc::Receiver<Bytes>>>,
    outgoing: mpsc::Sender<Bytes>,
    read_generation: Arc<AtomicUsize>,
    read_started_notify: Arc<Notify>,
}

#[async_trait]
impl TunFactory for FakeTunFactory {
    async fn create(&self, _config: &TunConfig) -> Result<Box<dyn PacketDevice>, TunError> {
        Ok(Box::new(FakePacketDevice {
            name: self.name.clone(),
            mtu: MTU,
            incoming: self.incoming.clone(),
            outgoing: self.outgoing.clone(),
            read_generation: self.read_generation.clone(),
            read_started_notify: self.read_started_notify.clone(),
            read_started: false,
        }))
    }
}

struct FakeTunHandle {
    incoming: mpsc::Sender<Bytes>,
    outgoing: AsyncMutex<mpsc::Receiver<Bytes>>,
    read_generation: Arc<AtomicUsize>,
    read_started_notify: Arc<Notify>,
}

impl FakeTunHandle {
    async fn inject(&self, packet: Bytes) {
        self.incoming
            .send(packet)
            .await
            .expect("fake TUN must remain open");
    }

    async fn receive_within(&self, duration: Duration) -> Option<Bytes> {
        timeout(duration, async { self.outgoing.lock().await.recv().await })
            .await
            .ok()
            .flatten()
    }

    async fn wait_until_reading(&self) {
        self.wait_until_reading_generation(1).await;
    }

    async fn wait_until_reading_generation(&self, expected: usize) {
        timeout(TEST_TIMEOUT, async {
            loop {
                let notified = self.read_started_notify.notified();
                if self.read_generation.load(Ordering::Acquire) >= expected {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("edge never entered its packet loop");
    }

    fn is_reading(&self) -> bool {
        self.read_generation.load(Ordering::Acquire) > 0
    }
}

fn fake_tun(name: &str) -> (Arc<dyn TunFactory>, FakeTunHandle) {
    let (incoming_sender, incoming_receiver) = mpsc::channel(16);
    let (outgoing_sender, outgoing_receiver) = mpsc::channel(16);
    let incoming_receiver = Arc::new(AsyncMutex::new(incoming_receiver));
    let read_generation = Arc::new(AtomicUsize::new(0));
    let read_started_notify = Arc::new(Notify::new());
    let factory: Arc<dyn TunFactory> = Arc::new(FakeTunFactory {
        name: name.to_owned(),
        incoming: incoming_receiver,
        outgoing: outgoing_sender,
        read_generation: read_generation.clone(),
        read_started_notify: read_started_notify.clone(),
    });
    let handle = FakeTunHandle {
        incoming: incoming_sender,
        outgoing: AsyncMutex::new(outgoing_receiver),
        read_generation,
        read_started_notify,
    };
    (factory, handle)
}

struct FakeRouteManager {
    install_gate: Option<Arc<Semaphore>>,
    install_started: AtomicBool,
    install_started_notify: Notify,
    installed: AtomicUsize,
    installed_notify: Notify,
    removed: AtomicUsize,
    removed_notify: Notify,
}

impl FakeRouteManager {
    fn immediate() -> Arc<Self> {
        Arc::new(Self {
            install_gate: None,
            install_started: AtomicBool::new(false),
            install_started_notify: Notify::new(),
            installed: AtomicUsize::new(0),
            installed_notify: Notify::new(),
            removed: AtomicUsize::new(0),
            removed_notify: Notify::new(),
        })
    }

    fn blocked() -> Arc<Self> {
        Arc::new(Self {
            install_gate: Some(Arc::new(Semaphore::new(0))),
            install_started: AtomicBool::new(false),
            install_started_notify: Notify::new(),
            installed: AtomicUsize::new(0),
            installed_notify: Notify::new(),
            removed: AtomicUsize::new(0),
            removed_notify: Notify::new(),
        })
    }

    fn release_install(&self) {
        self.install_gate
            .as_ref()
            .expect("route manager must have an install gate")
            .add_permits(1);
    }

    async fn wait_until_install_started(&self) {
        timeout(TEST_TIMEOUT, async {
            loop {
                let notified = self.install_started_notify.notified();
                if self.install_started.load(Ordering::Acquire) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("route installation did not start");
    }

    async fn wait_until_installed(&self, expected: usize) {
        timeout(TEST_TIMEOUT, async {
            loop {
                let notified = self.installed_notify.notified();
                if self.installed.load(Ordering::Acquire) >= expected {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("overlay route was not installed {expected} times"));
    }

    async fn wait_until_removed(&self) {
        self.wait_until_removed_count(1).await;
    }

    async fn wait_until_removed_count(&self, expected: usize) {
        timeout(TEST_TIMEOUT, async {
            loop {
                let notified = self.removed_notify.notified();
                if self.removed.load(Ordering::Acquire) >= expected {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("overlay route was not removed {expected} times"));
    }
}

#[async_trait]
impl RouteManager for FakeRouteManager {
    async fn install_overlay_route(
        &self,
        config: &TunConfig,
        interface_name: &str,
    ) -> Result<RouteLease, TunError> {
        self.install_started.store(true, Ordering::Release);
        self.install_started_notify.notify_waiters();
        if let Some(gate) = &self.install_gate {
            let permit = gate.acquire().await.map_err(|_| {
                TunError::InvalidConfiguration("fake route gate was closed".to_owned())
            })?;
            permit.forget();
        }
        self.installed.fetch_add(1, Ordering::Release);
        self.installed_notify.notify_waiters();
        Ok(RouteLease::new(interface_name, config.overlay))
    }

    async fn remove_overlay_route(&self, _lease: RouteLease) -> Result<(), TunError> {
        self.removed.fetch_add(1, Ordering::Release);
        self.removed_notify.notify_waiters();
        Ok(())
    }
}

struct RunningEdge {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<Result<(), NodeError>>,
    tun: FakeTunHandle,
    route_manager: Arc<FakeRouteManager>,
}

impl RunningEdge {
    async fn stop(self) {
        self.shutdown
            .send(true)
            .expect("edge session must still be running");
        timeout(TEST_TIMEOUT, self.task)
            .await
            .expect("edge did not stop")
            .expect("edge task panicked")
            .expect("edge session failed");
        self.route_manager.wait_until_removed().await;
    }
}

fn start_edge(
    identity: NodeIdentity,
    tun_name: &str,
    endpoint: Arc<dyn TransportEndpoint>,
    route_manager: Arc<FakeRouteManager>,
) -> RunningEdge {
    start_edge_at(
        identity,
        tun_name,
        endpoint,
        route_manager,
        RELAY_ADDRESS,
        "relay.test",
    )
}

fn start_edge_at(
    identity: NodeIdentity,
    tun_name: &str,
    endpoint: Arc<dyn TransportEndpoint>,
    route_manager: Arc<FakeRouteManager>,
    relay_address: SocketAddr,
    server_name: &str,
) -> RunningEdge {
    let runtime =
        EdgeRuntime::new(identity, relay_address, server_name, None).expect("valid edge runtime");
    let (tun_factory, tun) = fake_tun(tun_name);
    let (shutdown, mut shutdown_receiver) = watch::channel(false);
    let task_route_manager = route_manager.clone();
    let task = tokio::spawn(async move {
        runtime
            .run_session_with_shutdown(
                endpoint,
                tun_factory,
                task_route_manager,
                &mut shutdown_receiver,
            )
            .await
    });
    RunningEdge {
        shutdown,
        task,
        tun,
        route_manager,
    }
}

fn start_reconnecting_edge(
    identity: NodeIdentity,
    tun_name: &str,
    endpoint: Arc<dyn TransportEndpoint>,
    route_manager: Arc<FakeRouteManager>,
) -> RunningEdge {
    let runtime =
        EdgeRuntime::new(identity, RELAY_ADDRESS, "relay.test", None).expect("valid edge runtime");
    let (tun_factory, tun) = fake_tun(tun_name);
    let (shutdown, mut shutdown_receiver) = watch::channel(false);
    let task_route_manager = route_manager.clone();
    let task = tokio::spawn(async move {
        runtime
            .run_with_reconnect_until(
                endpoint,
                tun_factory,
                task_route_manager,
                &mut shutdown_receiver,
            )
            .await
    });
    RunningEdge {
        shutdown,
        task,
        tun,
        route_manager,
    }
}

struct TestRelay {
    routes: Arc<RouteTable>,
    client_endpoint: Arc<dyn TransportEndpoint>,
    probe: Arc<MemoryProbe>,
    task: JoinHandle<Result<(), NodeError>>,
    identity_a: NodeIdentity,
    identity_b: NodeIdentity,
    token_b: String,
}

impl TestRelay {
    fn start() -> Self {
        let overlay = overlay();
        let token_a = NodeToken::from_bytes([0x0a; 32]);
        let token_b = NodeToken::from_bytes([0x0b; 32]);
        let allocator = Arc::new(
            StaticAddressAllocator::new(
                overlay,
                [
                    StaticBinding::new("edge-a", TokenDigest::from_token(&token_a), EDGE_A_ADDRESS),
                    StaticBinding::new("edge-b", TokenDigest::from_token(&token_b), EDGE_B_ADDRESS),
                ],
            )
            .expect("valid static bindings"),
        );
        let routes = Arc::new(RouteTable::with_default_capacity(overlay));
        let validator = PacketValidator::new(overlay, usize::from(MTU)).expect("valid validator");
        let relay = Arc::new(
            RelayRuntime::new(allocator, routes.clone(), validator).expect("valid relay runtime"),
        );
        let (server_endpoint, client_endpoint, probe) = memory_endpoints();
        let task = tokio::spawn(relay.run_multi(vec![server_endpoint]));

        let token_b = token_b.encode();
        Self {
            routes,
            client_endpoint,
            probe,
            task,
            identity_a: NodeIdentity::new("edge-a", &token_a.encode()).expect("valid identity"),
            identity_b: NodeIdentity::new("edge-b", &token_b).expect("valid identity"),
            token_b,
        }
    }

    async fn stop(self) {
        self.task.abort();
        let result = self.task.await;
        assert!(result.is_err_and(|error| error.is_cancelled()));
    }
}

struct RestartableTestRelay {
    server_endpoint: Arc<dyn TransportEndpoint>,
    client_endpoint: Arc<dyn TransportEndpoint>,
    routes: Arc<RouteTable>,
    task: Option<JoinHandle<Result<(), NodeError>>>,
    identity_a: NodeIdentity,
    identity_b: NodeIdentity,
}

impl RestartableTestRelay {
    fn start() -> Self {
        let token_a = NodeToken::from_bytes([0x0a; 32]);
        let token_b = NodeToken::from_bytes([0x0b; 32]);
        let (server_endpoint, client_endpoint, _probe) = memory_endpoints();
        let (runtime, routes) = Self::new_runtime();
        let task = tokio::spawn(runtime.run_multi(vec![server_endpoint.clone()]));
        Self {
            server_endpoint,
            client_endpoint,
            routes,
            task: Some(task),
            identity_a: NodeIdentity::new("edge-a", &token_a.encode())
                .expect("valid edge A identity"),
            identity_b: NodeIdentity::new("edge-b", &token_b.encode())
                .expect("valid edge B identity"),
        }
    }

    fn new_runtime() -> (Arc<RelayRuntime>, Arc<RouteTable>) {
        let overlay = overlay();
        let token_a = NodeToken::from_bytes([0x0a; 32]);
        let token_b = NodeToken::from_bytes([0x0b; 32]);
        let allocator = Arc::new(
            StaticAddressAllocator::new(
                overlay,
                [
                    StaticBinding::new("edge-a", TokenDigest::from_token(&token_a), EDGE_A_ADDRESS),
                    StaticBinding::new("edge-b", TokenDigest::from_token(&token_b), EDGE_B_ADDRESS),
                ],
            )
            .expect("valid restartable static bindings"),
        );
        let routes = Arc::new(RouteTable::with_default_capacity(overlay));
        let validator = PacketValidator::new(overlay, usize::from(MTU)).expect("valid validator");
        let runtime = Arc::new(
            RelayRuntime::new(allocator, routes.clone(), validator).expect("valid relay runtime"),
        );
        (runtime, routes)
    }

    async fn stop_runtime(&mut self) {
        let task = self.task.take().expect("relay runtime must be running");
        task.abort();
        let result = timeout(TEST_TIMEOUT, task)
            .await
            .expect("relay runtime did not stop");
        assert!(result.is_err_and(|error| error.is_cancelled()));
    }

    fn restart_runtime(&mut self) {
        assert!(self.task.is_none(), "old relay runtime must be stopped");
        let (runtime, routes) = Self::new_runtime();
        self.routes = routes;
        self.task = Some(tokio::spawn(
            runtime.run_multi(vec![self.server_endpoint.clone()]),
        ));
    }
}

fn overlay() -> Ipv4Net {
    "10.42.0.0/24".parse().expect("valid overlay")
}

fn ipv4_packet(source: Ipv4Addr, destination: Ipv4Addr, payload: &[u8]) -> Bytes {
    let total_len = 20 + payload.len();
    let mut packet = vec![0_u8; total_len];
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(
        &u16::try_from(total_len)
            .expect("test packet fits in an IPv4 datagram")
            .to_be_bytes(),
    );
    packet[8] = 64;
    packet[9] = 17;
    packet[12..16].copy_from_slice(&source.octets());
    packet[16..20].copy_from_slice(&destination.octets());
    packet[20..].copy_from_slice(payload);
    Bytes::from(packet)
}

async fn wait_for_route_count(routes: &RouteTable, expected: usize) {
    timeout(TEST_TIMEOUT, async {
        while routes.len() != expected {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "route table did not reach {expected} entries; current count is {}",
            routes.len()
        )
    });
}

async fn wait_for_active_memory_routes(source: &FakeTunHandle, destination: &FakeTunHandle) {
    let probe = ipv4_packet(EDGE_A_ADDRESS, EDGE_B_ADDRESS, b"restart-ready-probe");
    timeout(TEST_TIMEOUT, async {
        loop {
            source.inject(probe.clone()).await;
            if let Some(received) = destination.receive_within(Duration::from_millis(20)).await {
                assert_eq!(received, probe, "unexpected packet while probing Ready");
                return;
            }
        }
    })
    .await
    .expect("restarted Relay did not activate both routes after Ready");
}

#[cfg(all(
    feature = "backend-quinn",
    feature = "backend-s2n",
    feature = "backend-gm-quic"
))]
mod real_udp_multi_listener {
    use super::*;

    const SERVER_NAME: &str = "localhost";

    fn reserve_listener_addresses() -> [SocketAddr; 3] {
        let quinn = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("reserve Quinn listener address");
        let s2n = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("reserve s2n listener address");
        let gm_quic = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("reserve gm-quic listener address");
        let addresses = [
            quinn.local_addr().expect("Quinn listener address"),
            s2n.local_addr().expect("s2n listener address"),
            gm_quic.local_addr().expect("gm-quic listener address"),
        ];
        assert_ne!(addresses[0], addresses[1]);
        assert_ne!(addresses[0], addresses[2]);
        assert_ne!(addresses[1], addresses[2]);
        addresses
    }

    const fn backend_name(backend: TransportBackend) -> &'static str {
        match backend {
            TransportBackend::Quinn => "quinn",
            TransportBackend::S2n => "s2n",
            TransportBackend::GmQuic => "gm-quic",
        }
    }

    fn client_endpoint(
        backend: TransportBackend,
        certificate_pem: &str,
    ) -> Arc<dyn TransportEndpoint> {
        make_client_endpoint(
            backend,
            ClientTransportConfig::new(SocketAddr::from(([127, 0, 0, 1], 0)), certificate_pem),
        )
        .unwrap_or_else(|error| panic!("{backend:?} client endpoint failed: {error}"))
    }

    pub(super) async fn wait_for_active_routes(
        source: &FakeTunHandle,
        destination: &FakeTunHandle,
    ) {
        let probe = ipv4_packet(EDGE_A_ADDRESS, EDGE_B_ADDRESS, b"ready-probe");
        timeout(TEST_TIMEOUT, async {
            loop {
                source.inject(probe.clone()).await;
                if let Some(received) = destination.receive_within(Duration::from_millis(20)).await
                {
                    assert_eq!(received, probe, "unexpected packet while probing Ready");
                    return;
                }
            }
        })
        .await
        .expect("Relay did not activate both routes after Ready");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn relay_routes_all_nine_backend_pairs_over_real_udp() {
        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec![SERVER_NAME.to_owned()])
                .expect("localhost test certificate generation");
        let certificate_pem = cert.pem();
        let private_key_pem = key_pair.serialize_pem();
        let listener_addresses = reserve_listener_addresses();
        let backends = [
            (TransportBackend::Quinn, listener_addresses[0]),
            (TransportBackend::S2n, listener_addresses[1]),
            (TransportBackend::GmQuic, listener_addresses[2]),
        ];
        let listeners = backends
            .iter()
            .map(|(backend, address)| {
                make_server_endpoint(
                    *backend,
                    ServerTransportConfig::new(
                        *address,
                        SERVER_NAME,
                        certificate_pem.clone(),
                        private_key_pem.clone(),
                    ),
                )
                .unwrap_or_else(|error| panic!("{backend:?} listener failed: {error}"))
            })
            .collect::<Vec<_>>();

        let token_a = NodeToken::from_bytes([0x2a; 32]);
        let token_b = NodeToken::from_bytes([0x2b; 32]);
        let allocator = Arc::new(
            StaticAddressAllocator::new(
                overlay(),
                [
                    StaticBinding::new(
                        "udp-edge-a",
                        TokenDigest::from_token(&token_a),
                        EDGE_A_ADDRESS,
                    ),
                    StaticBinding::new(
                        "udp-edge-b",
                        TokenDigest::from_token(&token_b),
                        EDGE_B_ADDRESS,
                    ),
                ],
            )
            .expect("valid real UDP static bindings"),
        );
        let routes = Arc::new(RouteTable::with_default_capacity(overlay()));
        let validator =
            PacketValidator::new(overlay(), usize::from(MTU)).expect("valid packet validator");
        let relay = Arc::new(
            RelayRuntime::new(allocator, routes.clone(), validator).expect("valid relay runtime"),
        );
        let relay_task = tokio::spawn(relay.run_multi(listeners));
        let identity_a =
            NodeIdentity::new("udp-edge-a", &token_a.encode()).expect("valid edge A identity");
        let identity_b =
            NodeIdentity::new("udp-edge-b", &token_b.encode()).expect("valid edge B identity");

        for &(source_backend, source_address) in &backends {
            for &(destination_backend, destination_address) in &backends {
                assert!(
                    !relay_task.is_finished(),
                    "relay stopped before {source_backend:?} -> {destination_backend:?}"
                );
                let source_name = format!(
                    "udp-{}-{}-a",
                    backend_name(source_backend),
                    backend_name(destination_backend)
                );
                let destination_name = format!(
                    "udp-{}-{}-b",
                    backend_name(source_backend),
                    backend_name(destination_backend)
                );
                let source = start_edge_at(
                    identity_a.clone(),
                    &source_name,
                    client_endpoint(source_backend, &certificate_pem),
                    FakeRouteManager::immediate(),
                    source_address,
                    SERVER_NAME,
                );
                let destination = start_edge_at(
                    identity_b.clone(),
                    &destination_name,
                    client_endpoint(destination_backend, &certificate_pem),
                    FakeRouteManager::immediate(),
                    destination_address,
                    SERVER_NAME,
                );

                source.tun.wait_until_reading().await;
                destination.tun.wait_until_reading().await;
                wait_for_route_count(&routes, 2).await;
                wait_for_active_routes(&source.tun, &destination.tun).await;

                let forward_payload = format!("{source_backend:?}-to-{destination_backend:?}");
                let forward =
                    ipv4_packet(EDGE_A_ADDRESS, EDGE_B_ADDRESS, forward_payload.as_bytes());
                source.tun.inject(forward.clone()).await;
                assert_eq!(
                    destination.tun.receive_within(TEST_TIMEOUT).await,
                    Some(forward),
                    "failed {source_backend:?} -> {destination_backend:?} packet"
                );

                let reverse_payload = format!("{destination_backend:?}-to-{source_backend:?}");
                let reverse =
                    ipv4_packet(EDGE_B_ADDRESS, EDGE_A_ADDRESS, reverse_payload.as_bytes());
                destination.tun.inject(reverse.clone()).await;
                assert_eq!(
                    source.tun.receive_within(TEST_TIMEOUT).await,
                    Some(reverse),
                    "failed {destination_backend:?} -> {source_backend:?} packet"
                );

                source.stop().await;
                destination.stop().await;
                wait_for_route_count(&routes, 0).await;
            }
        }

        relay_task.abort();
        let relay_result = timeout(TEST_TIMEOUT, relay_task)
            .await
            .expect("relay task did not stop");
        assert!(relay_result.is_err_and(|error| error.is_cancelled()));
    }
}

#[tokio::test]
async fn relay_routes_fake_tun_packets_bidirectionally_and_allows_reconnect() {
    let relay = TestRelay::start();
    let manager_a = FakeRouteManager::immediate();
    let manager_b = FakeRouteManager::immediate();
    let edge_a = start_edge(
        relay.identity_a.clone(),
        "fake-a",
        relay.client_endpoint.clone(),
        manager_a,
    );
    let edge_b = start_edge(
        relay.identity_b.clone(),
        "fake-b",
        relay.client_endpoint.clone(),
        manager_b,
    );

    edge_a.tun.wait_until_reading().await;
    edge_b.tun.wait_until_reading().await;
    wait_for_route_count(&relay.routes, 2).await;

    let packet_a_to_b = ipv4_packet(EDGE_A_ADDRESS, EDGE_B_ADDRESS, b"a-to-b");
    edge_a.tun.inject(packet_a_to_b.clone()).await;
    assert_eq!(
        edge_b.tun.receive_within(TEST_TIMEOUT).await,
        Some(packet_a_to_b)
    );

    let packet_b_to_a = ipv4_packet(EDGE_B_ADDRESS, EDGE_A_ADDRESS, b"b-to-a");
    edge_b.tun.inject(packet_b_to_a.clone()).await;
    assert_eq!(
        edge_a.tun.receive_within(TEST_TIMEOUT).await,
        Some(packet_b_to_a)
    );

    edge_a.stop().await;
    wait_for_route_count(&relay.routes, 1).await;

    let reconnected_a = start_edge(
        relay.identity_a.clone(),
        "fake-a-reconnected",
        relay.client_endpoint.clone(),
        FakeRouteManager::immediate(),
    );
    reconnected_a.tun.wait_until_reading().await;
    wait_for_route_count(&relay.routes, 2).await;

    let packet_after_reconnect = ipv4_packet(EDGE_B_ADDRESS, EDGE_A_ADDRESS, b"after-reconnect");
    edge_b.tun.inject(packet_after_reconnect.clone()).await;
    assert_eq!(
        reconnected_a.tun.receive_within(TEST_TIMEOUT).await,
        Some(packet_after_reconnect)
    );

    reconnected_a.stop().await;
    edge_b.stop().await;
    wait_for_route_count(&relay.routes, 0).await;
    relay.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edges_reconnect_after_relay_restart_and_clean_up_routes() {
    let mut relay = RestartableTestRelay::start();
    let manager_a = FakeRouteManager::immediate();
    let manager_b = FakeRouteManager::immediate();
    let edge_a = start_reconnecting_edge(
        relay.identity_a.clone(),
        "fake-restart-a",
        relay.client_endpoint.clone(),
        manager_a.clone(),
    );
    let edge_b = start_reconnecting_edge(
        relay.identity_b.clone(),
        "fake-restart-b",
        relay.client_endpoint.clone(),
        manager_b.clone(),
    );

    manager_a.wait_until_installed(1).await;
    manager_b.wait_until_installed(1).await;
    edge_a.tun.wait_until_reading_generation(1).await;
    edge_b.tun.wait_until_reading_generation(1).await;
    wait_for_active_memory_routes(&edge_a.tun, &edge_b.tun).await;
    assert_eq!(relay.routes.len(), 2);

    let recovery_started = Instant::now();
    let old_routes = relay.routes.clone();
    relay.stop_runtime().await;

    timeout(RECOVERY_TARGET, async {
        manager_a.wait_until_removed_count(1).await;
        manager_b.wait_until_removed_count(1).await;
        assert_eq!(old_routes.len(), 0, "stopped Relay retained old sessions");

        relay.restart_runtime();
        manager_a.wait_until_installed(2).await;
        manager_b.wait_until_installed(2).await;
        edge_a.tun.wait_until_reading_generation(2).await;
        edge_b.tun.wait_until_reading_generation(2).await;
        wait_for_active_memory_routes(&edge_a.tun, &edge_b.tun).await;

        let forward = ipv4_packet(
            EDGE_A_ADDRESS,
            EDGE_B_ADDRESS,
            b"after-relay-restart-a-to-b",
        );
        edge_a.tun.inject(forward.clone()).await;
        assert_eq!(edge_b.tun.receive_within(TEST_TIMEOUT).await, Some(forward));

        let reverse = ipv4_packet(
            EDGE_B_ADDRESS,
            EDGE_A_ADDRESS,
            b"after-relay-restart-b-to-a",
        );
        edge_b.tun.inject(reverse.clone()).await;
        assert_eq!(edge_a.tun.receive_within(TEST_TIMEOUT).await, Some(reverse));
    })
    .await
    .expect("edges did not recover within the 30-second restart target");
    assert!(recovery_started.elapsed() < RECOVERY_TARGET);
    assert_eq!(relay.routes.len(), 2);

    edge_a.stop().await;
    edge_b.stop().await;
    manager_a.wait_until_removed_count(2).await;
    manager_b.wait_until_removed_count(2).await;
    relay.stop_runtime().await;
    assert_eq!(relay.routes.len(), 0);
}

#[tokio::test]
async fn shutdown_during_route_install_waits_for_lease_and_removes_route() {
    let relay = TestRelay::start();
    let route_manager = FakeRouteManager::blocked();
    let edge = start_edge(
        relay.identity_a.clone(),
        "fake-install-shutdown",
        relay.client_endpoint.clone(),
        route_manager.clone(),
    );
    route_manager.wait_until_install_started().await;

    edge.shutdown
        .send(true)
        .expect("edge must still be installing its route");
    tokio::task::yield_now().await;
    assert!(
        !edge.task.is_finished(),
        "shutdown cancelled route installation before preserving its lease"
    );

    route_manager.release_install();
    timeout(TEST_TIMEOUT, edge.task)
        .await
        .expect("edge did not stop after route installation completed")
        .expect("edge task panicked")
        .expect("edge session failed");
    route_manager.wait_until_installed(1).await;
    route_manager.wait_until_removed_count(1).await;

    relay.stop().await;
}

#[tokio::test]
async fn relay_keeps_reserved_edge_inactive_until_ready() {
    let relay = TestRelay::start();
    let edge_a = start_edge(
        relay.identity_a.clone(),
        "fake-ready-a",
        relay.client_endpoint.clone(),
        FakeRouteManager::immediate(),
    );
    edge_a.tun.wait_until_reading().await;

    let blocked_manager = FakeRouteManager::blocked();
    let edge_b = start_edge(
        relay.identity_b.clone(),
        "fake-ready-b",
        relay.client_endpoint.clone(),
        blocked_manager.clone(),
    );
    blocked_manager.wait_until_install_started().await;
    wait_for_route_count(&relay.routes, 2).await;
    assert!(!edge_b.tun.is_reading());

    let packet_before_ready =
        ipv4_packet(EDGE_A_ADDRESS, EDGE_B_ADDRESS, b"before-ready-must-drop");
    edge_a.tun.inject(packet_before_ready.clone()).await;
    relay
        .probe
        .wait_for_relay_receive(&packet_before_ready)
        .await;
    assert_eq!(edge_b.tun.receive_within(QUIET_PERIOD).await, None);

    blocked_manager.release_install();
    edge_b.tun.wait_until_reading().await;
    assert_eq!(edge_b.tun.receive_within(QUIET_PERIOD).await, None);

    let packet_after_ready =
        ipv4_packet(EDGE_A_ADDRESS, EDGE_B_ADDRESS, b"after-ready-must-forward");
    edge_a.tun.inject(packet_after_ready.clone()).await;
    assert_eq!(
        edge_b.tun.receive_within(TEST_TIMEOUT).await,
        Some(packet_after_ready)
    );

    edge_a.stop().await;
    edge_b.stop().await;
    wait_for_route_count(&relay.routes, 0).await;
    relay.stop().await;
}

#[tokio::test]
async fn relay_discards_datagram_sent_by_registered_peer_before_ready() {
    let relay = TestRelay::start();
    let edge_a = start_edge(
        relay.identity_a.clone(),
        "fake-pre-ready-a",
        relay.client_endpoint.clone(),
        FakeRouteManager::immediate(),
    );
    edge_a.tun.wait_until_reading().await;

    let early_connection = relay
        .client_endpoint
        .connect(RELAY_ADDRESS, "relay.test")
        .await
        .expect("memory connection");
    let (mut reader, mut writer) = early_connection.open_bi().await.expect("control stream");
    write_message(
        &mut writer,
        &ControlMessage::Register(Register {
            node_id: "edge-b".to_owned(),
            token: relay.token_b.clone(),
        }),
    )
    .await
    .expect("registration");
    let accepted = match read_message(&mut reader)
        .await
        .expect("registration response")
    {
        ControlMessage::RegisterAccepted(accepted) => accepted,
        other => panic!("expected RegisterAccepted, got {other:?}"),
    };
    assert_eq!(accepted.overlay_ip, EDGE_B_ADDRESS);
    assert_eq!(accepted.overlay, overlay());
    assert_eq!(accepted.mtu, MTU);
    assert!(!accepted.session_id.is_empty());

    let packet_before_ready = ipv4_packet(
        EDGE_B_ADDRESS,
        EDGE_A_ADDRESS,
        b"sender-before-ready-must-drop",
    );
    early_connection
        .send_datagram(packet_before_ready.clone())
        .expect("pre-Ready datagram enters memory transport");
    relay
        .probe
        .wait_for_relay_receive(&packet_before_ready)
        .await;

    write_message(&mut writer, &ControlMessage::Ready(Ready {}))
        .await
        .expect("Ready");
    wait_for_route_count(&relay.routes, 2).await;
    assert_eq!(edge_a.tun.receive_within(QUIET_PERIOD).await, None);

    let packet_after_ready = ipv4_packet(
        EDGE_B_ADDRESS,
        EDGE_A_ADDRESS,
        b"sender-after-ready-must-forward",
    );
    early_connection
        .send_datagram(packet_after_ready.clone())
        .expect("post-Ready datagram enters memory transport");
    assert_eq!(
        edge_a.tun.receive_within(TEST_TIMEOUT).await,
        Some(packet_after_ready)
    );

    drop(reader);
    drop(writer);
    drop(early_connection);
    wait_for_route_count(&relay.routes, 1).await;
    edge_a.stop().await;
    wait_for_route_count(&relay.routes, 0).await;
    relay.stop().await;
}

#[tokio::test]
async fn relay_handles_invalid_packets_and_bounded_backpressure_then_shuts_down() {
    let relay = TestRelay::start();
    let edge_a = start_edge(
        relay.identity_a.clone(),
        "fake-pressure-a",
        relay.client_endpoint.clone(),
        FakeRouteManager::immediate(),
    );
    let edge_b = start_edge(
        relay.identity_b.clone(),
        "fake-pressure-b",
        relay.client_endpoint.clone(),
        FakeRouteManager::immediate(),
    );
    edge_a.tun.wait_until_reading().await;
    edge_b.tun.wait_until_reading().await;
    wait_for_route_count(&relay.routes, 2).await;

    for invalid in [
        Bytes::from_static(&[0_u8; 20]),
        ipv4_packet(Ipv4Addr::new(10, 42, 0, 99), EDGE_B_ADDRESS, b"spoofed"),
        ipv4_packet(EDGE_A_ADDRESS, Ipv4Addr::new(224, 0, 0, 1), b"multicast"),
        ipv4_packet(EDGE_A_ADDRESS, Ipv4Addr::new(10, 43, 0, 3), b"outside"),
        ipv4_packet(EDGE_A_ADDRESS, EDGE_B_ADDRESS, &[0_u8; 1081]),
    ] {
        edge_a.tun.inject(invalid).await;
    }
    assert_eq!(edge_b.tun.receive_within(QUIET_PERIOD).await, None);

    for sequence in 0..700_u16 {
        let packet = ipv4_packet(EDGE_A_ADDRESS, EDGE_B_ADDRESS, &sequence.to_be_bytes());
        edge_a.tun.inject(packet).await;
        sleep(Duration::from_millis(1)).await;
    }
    timeout(Duration::from_secs(5), async {
        while relay.routes.dropped_queue_full() == 0 && relay.routes.dropped_transport() == 0 {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("relay did not apply a bounded Datagram drop policy");

    edge_a.stop().await;
    edge_b.stop().await;
    wait_for_route_count(&relay.routes, 0).await;
    relay.stop().await;
}
