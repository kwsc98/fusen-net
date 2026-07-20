// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, UdpSocket as StdUdpSocket},
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    str::FromStr as _,
    sync::Arc,
    time::{Duration, Instant},
};

use fusen_net::{
    address::{NodeToken, StaticAddressAllocator, StaticBinding, TokenDigest},
    data_plane::PacketValidator,
    node::{EdgeRuntime, NodeIdentity, RelayRuntime},
    routing::RouteTable,
    transport::{
        ClientTransportConfig, ServerTransportConfig, TransportBackend, make_client_endpoint,
        make_server_endpoint,
    },
    tun::{NativeRouteManager, NativeTunFactory},
};
use ipnet::Ipv4Net;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpStream, UdpSocket},
    process::Command,
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};
use uuid::Uuid;

use super::{ECHO_PAYLOAD, TestError, TestResult, require_success};

const OVERLAY: &str = "10.203.0.0/24";
const EDGE_A_OVERLAY: Ipv4Addr = Ipv4Addr::new(10, 203, 0, 2);
const EDGE_B_OVERLAY: Ipv4Addr = Ipv4Addr::new(10, 203, 0, 3);
const SERVER_NAME: &str = "relay.test";
const RELAY_A_UNDERLAY: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
const RELAY_B_UNDERLAY: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 5);
const UDP_PORT: u16 = 39_011;
const TCP_PORT: u16 = 39_012;
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const CHILD_STOP_TIMEOUT: Duration = Duration::from_secs(8);
// Measured after a protocol warm-up. These tolerate allocator/runtime noise
// while still catching connection, descriptor, or worker leaks over 30 minutes.
const SOAK_MAX_RSS_GROWTH_KIB: u64 = 32 * 1024;
const SOAK_MAX_FD_GROWTH: u64 = 8;
const SOAK_MAX_THREAD_GROWTH: u64 = 2;

#[derive(Clone, Copy, Debug)]
pub(super) enum NamespaceScenario {
    Standard,
    RelayRestart,
    FaultInjection,
    Soak,
}

struct TestCertificate {
    certificate_pem: String,
    private_key_pem: String,
}

#[derive(Clone, Copy)]
struct ProcessTarget {
    name: &'static str,
    pid: u32,
}

#[derive(Clone, Copy, Debug)]
struct ProcessResources {
    rss_kib: u64,
    open_fds: u64,
    threads: u64,
}

impl TestCertificate {
    fn generate() -> TestResult<Self> {
        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec![SERVER_NAME.to_owned()])?;
        Ok(Self {
            certificate_pem: cert.pem(),
            private_key_pem: key_pair.serialize_pem(),
        })
    }
}

struct NamespaceLab {
    namespace_a: String,
    namespace_b: String,
    root_a: String,
    root_b: String,
    peer_a: String,
    peer_b: String,
    tun_a: String,
    tun_b: String,
    directory: PathBuf,
    namespace_a_created: bool,
    namespace_b_created: bool,
    root_a_created: bool,
    root_b_created: bool,
    directory_created: bool,
}

#[derive(Clone, Copy)]
enum LabSide {
    A,
    B,
}

impl NamespaceLab {
    fn create() -> TestResult<Self> {
        ensure_root()?;
        let identifier = Uuid::new_v4().simple().to_string();
        let suffix = &identifier[..8];
        let mut lab = Self {
            namespace_a: format!("fusen-e2e-a-{suffix}"),
            namespace_b: format!("fusen-e2e-b-{suffix}"),
            root_a: format!("fn{suffix}a0"),
            root_b: format!("fn{suffix}b0"),
            peer_a: format!("fn{suffix}a1"),
            peer_b: format!("fn{suffix}b1"),
            tun_a: format!("ft{suffix}a"),
            tun_b: format!("ft{suffix}b"),
            directory: std::env::temp_dir().join(format!("fusen-net-real-tun-{identifier}")),
            namespace_a_created: false,
            namespace_b_created: false,
            root_a_created: false,
            root_b_created: false,
            directory_created: false,
        };
        fs::create_dir(&lab.directory)?;
        lab.directory_created = true;

        run_ip(["netns", "add", &lab.namespace_a])?;
        lab.namespace_a_created = true;
        run_ip(["netns", "add", &lab.namespace_b])?;
        lab.namespace_b_created = true;
        let namespace_a = lab.namespace_a.clone();
        let root_a = lab.root_a.clone();
        let peer_a = lab.peer_a.clone();
        lab.configure_side(
            LabSide::A,
            &namespace_a,
            &root_a,
            &peer_a,
            "192.0.2.1/30",
            "192.0.2.2/30",
        )?;
        let namespace_b = lab.namespace_b.clone();
        let root_b = lab.root_b.clone();
        let peer_b = lab.peer_b.clone();
        lab.configure_side(
            LabSide::B,
            &namespace_b,
            &root_b,
            &peer_b,
            "192.0.2.5/30",
            "192.0.2.6/30",
        )?;
        Ok(lab)
    }

    fn configure_side(
        &mut self,
        side: LabSide,
        namespace: &str,
        root_interface: &str,
        peer_interface: &str,
        root_address: &str,
        peer_address: &str,
    ) -> TestResult {
        run_ip([
            "link",
            "add",
            root_interface,
            "type",
            "veth",
            "peer",
            "name",
            peer_interface,
        ])?;
        match side {
            LabSide::A => self.root_a_created = true,
            LabSide::B => self.root_b_created = true,
        }
        run_ip(["link", "set", peer_interface, "netns", namespace])?;
        run_ip(["address", "add", root_address, "dev", root_interface])?;
        run_ip(["link", "set", root_interface, "up"])?;
        run_ip([
            "-n",
            namespace,
            "address",
            "add",
            peer_address,
            "dev",
            peer_interface,
        ])?;
        run_ip(["-n", namespace, "link", "set", "lo", "up"])?;
        run_ip(["-n", namespace, "link", "set", peer_interface, "up"])?;
        Ok(())
    }

    fn file(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    fn cleanup(&mut self) -> TestResult {
        let mut errors = Vec::new();
        if self.root_a_created {
            match run_ip(["link", "delete", &self.root_a]) {
                Ok(()) => self.root_a_created = false,
                Err(error) => errors.push(format!("delete {}: {error}", self.root_a)),
            }
        }
        if self.root_b_created {
            match run_ip(["link", "delete", &self.root_b]) {
                Ok(()) => self.root_b_created = false,
                Err(error) => errors.push(format!("delete {}: {error}", self.root_b)),
            }
        }
        if self.namespace_a_created {
            match run_ip(["netns", "delete", &self.namespace_a]) {
                Ok(()) => self.namespace_a_created = false,
                Err(error) => errors.push(format!("delete {}: {error}", self.namespace_a)),
            }
        }
        if self.namespace_b_created {
            match run_ip(["netns", "delete", &self.namespace_b]) {
                Ok(()) => self.namespace_b_created = false,
                Err(error) => errors.push(format!("delete {}: {error}", self.namespace_b)),
            }
        }
        if self.directory_created {
            match fs::remove_dir_all(&self.directory) {
                Ok(()) => self.directory_created = false,
                Err(error) => errors.push(format!("delete {}: {error}", self.directory.display())),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("test lab cleanup failed: {}", errors.join("; ")).into())
        }
    }
}

impl Drop for NamespaceLab {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("{error}");
        }
    }
}

struct RelayHandle {
    task: JoinHandle<Result<(), fusen_net::node::NodeError>>,
}

impl RelayHandle {
    async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for RelayHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) async fn run_namespace_scenario(
    scenario: NamespaceScenario,
    test_name: &'static str,
) -> TestResult {
    let mut lab = NamespaceLab::create()?;
    let backend = selected_backend()?;
    let certificate = TestCertificate::generate()?;
    let certificate_path = lab.file("ca.pem");
    fs::write(&certificate_path, &certificate.certificate_pem)?;
    let server_port = reserve_udp_port()?;
    let mut relay = Some(start_relay(backend, server_port, &certificate)?);

    let mut edge_a = spawn_edge(
        &lab,
        test_name,
        "edge-a",
        &lab.namespace_a,
        &lab.tun_a,
        EDGE_B_OVERLAY,
        SocketAddr::from((RELAY_A_UNDERLAY, server_port)),
        &certificate_path,
    )?;
    let mut edge_b = spawn_edge(
        &lab,
        test_name,
        "edge-b",
        &lab.namespace_b,
        &lab.tun_b,
        EDGE_A_OVERLAY,
        SocketAddr::from((RELAY_B_UNDERLAY, server_port)),
        &certificate_path,
    )?;

    wait_for_edge_route(&lab.namespace_a, &lab.tun_a, &mut edge_a).await?;
    wait_for_edge_route(&lab.namespace_b, &lab.tun_b, &mut edge_b).await?;

    let scenario_result = match scenario {
        NamespaceScenario::Standard => run_standard_scenario(&lab, test_name).await,
        NamespaceScenario::RelayRestart => {
            run_restart_scenario(
                &lab,
                test_name,
                backend,
                server_port,
                &certificate,
                &mut relay,
            )
            .await
        }
        NamespaceScenario::FaultInjection => run_fault_scenario(&lab).await,
        NamespaceScenario::Soak => {
            let processes = [
                ProcessTarget {
                    name: "relay",
                    pid: std::process::id(),
                },
                ProcessTarget {
                    name: "edge-a",
                    pid: edge_a.id().ok_or("edge-a has no process ID")?,
                },
                ProcessTarget {
                    name: "edge-b",
                    pid: edge_b.id().ok_or("edge-b has no process ID")?,
                },
            ];
            run_soak_scenario(&lab, test_name, &processes).await
        }
    };

    let cleanup_started = Instant::now();
    fs::write(lab.file("stop-edge-a"), b"stop")?;
    fs::write(lab.file("stop-edge-b"), b"stop")?;
    let cleanup_result: TestResult = async {
        wait_child_success(&mut edge_a, "edge-a", CHILD_STOP_TIMEOUT).await?;
        wait_child_success(&mut edge_b, "edge-b", CHILD_STOP_TIMEOUT).await?;
        if namespace_route_exists(&lab.namespace_a, &lab.tun_a)?
            || namespace_route_exists(&lab.namespace_b, &lab.tun_b)?
        {
            return Err("an overlay route remained after both Edge runtimes stopped".into());
        }
        if namespace_interface_exists(&lab.namespace_a, &lab.tun_a)?
            || namespace_interface_exists(&lab.namespace_b, &lab.tun_b)?
        {
            return Err("a TUN interface remained after both Edge runtimes stopped".into());
        }
        if cleanup_started.elapsed() > Duration::from_secs(5) {
            return Err(format!(
                "Edge route cleanup exceeded five seconds: {:?}",
                cleanup_started.elapsed()
            )
            .into());
        }
        Ok(())
    }
    .await;
    if let Some(relay) = relay.take() {
        relay.stop().await;
    }

    scenario_result?;
    cleanup_result?;
    lab.cleanup()
}

async fn run_restart_scenario(
    lab: &NamespaceLab,
    test_name: &str,
    backend: TransportBackend,
    server_port: u16,
    certificate: &TestCertificate,
    relay: &mut Option<RelayHandle>,
) -> TestResult {
    run_ping_until_success(lab, Duration::from_secs(10)).await?;
    let recovery_started = Instant::now();
    relay
        .take()
        .ok_or("Relay handle is missing before restart")?
        .stop()
        .await;
    *relay = Some(restart_relay(backend, server_port, certificate).await?);
    let recovery_budget = Duration::from_secs(30).saturating_sub(recovery_started.elapsed());
    if recovery_budget.is_zero() {
        return Err("Relay restart exhausted the 30-second recovery budget".into());
    }
    run_ping_until_success(lab, recovery_budget).await?;
    run_application_echo(lab, test_name, 1, 0).await
}

pub(super) async fn run_child_role(role: &str) -> TestResult {
    match role {
        "edge-a" | "edge-b" => run_edge_child(role).await,
        "echo-service" => run_echo_service_child().await,
        "echo-probe" => run_echo_probe_child().await,
        _ => Err(format!("unknown FUSEN_REAL_TUN_ROLE {role:?}").into()),
    }
}

fn start_relay(
    backend: TransportBackend,
    server_port: u16,
    certificate: &TestCertificate,
) -> TestResult<RelayHandle> {
    let overlay = Ipv4Net::from_str(OVERLAY)?;
    let token_a = edge_token("edge-a");
    let token_b = edge_token("edge-b");
    let allocator = Arc::new(StaticAddressAllocator::new(
        overlay,
        [
            StaticBinding::new("edge-a", TokenDigest::from_token(&token_a), EDGE_A_OVERLAY),
            StaticBinding::new("edge-b", TokenDigest::from_token(&token_b), EDGE_B_OVERLAY),
        ],
    )?);
    let routes = Arc::new(RouteTable::with_default_capacity(overlay));
    let validator = PacketValidator::new(overlay, 1100)?;
    let runtime = Arc::new(RelayRuntime::new(allocator, routes, validator)?);
    let endpoint = make_server_endpoint(
        backend,
        ServerTransportConfig::new(
            SocketAddr::from(([0, 0, 0, 0], server_port)),
            SERVER_NAME,
            certificate.certificate_pem.clone(),
            certificate.private_key_pem.clone(),
        ),
    )?;
    Ok(RelayHandle {
        task: tokio::spawn(runtime.run_multi(vec![endpoint])),
    })
}

async fn restart_relay(
    backend: TransportBackend,
    server_port: u16,
    certificate: &TestCertificate,
) -> TestResult<RelayHandle> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match start_relay(backend, server_port, certificate) {
            Ok(relay) => return Ok(relay),
            Err(error) if Instant::now() < deadline => {
                eprintln!("waiting to rebind Relay UDP listener: {error}");
                sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_edge(
    lab: &NamespaceLab,
    test_name: &str,
    role: &str,
    namespace: &str,
    tun_name: &str,
    remote_overlay_address: Ipv4Addr,
    server_address: SocketAddr,
    certificate_path: &Path,
) -> TestResult<tokio::process::Child> {
    let stop_file = lab.file(&format!("stop-{role}"));
    spawn_test_role(
        namespace,
        test_name,
        role,
        [
            ("FUSEN_REAL_TUN_TUN_NAME", tun_name.to_owned()),
            (
                "FUSEN_REAL_TUN_REMOTE_OVERLAY_ADDRESS",
                remote_overlay_address.to_string(),
            ),
            ("FUSEN_REAL_TUN_SERVER_ADDRESS", server_address.to_string()),
            (
                "FUSEN_REAL_TUN_CA_FILE",
                certificate_path.display().to_string(),
            ),
            ("FUSEN_REAL_TUN_STOP_FILE", stop_file.display().to_string()),
        ],
    )
}

async fn run_edge_child(role: &str) -> TestResult {
    let backend = selected_backend()?;
    let tun_name = required_env("FUSEN_REAL_TUN_TUN_NAME")?;
    let remote_overlay_address =
        required_env("FUSEN_REAL_TUN_REMOTE_OVERLAY_ADDRESS")?.parse::<Ipv4Addr>()?;
    let server_address = required_env("FUSEN_REAL_TUN_SERVER_ADDRESS")?.parse()?;
    let ca_pem = fs::read_to_string(required_env("FUSEN_REAL_TUN_CA_FILE")?)?;
    let stop_file = PathBuf::from(required_env("FUSEN_REAL_TUN_STOP_FILE")?);
    let overlay = Ipv4Net::from_str(OVERLAY)?;
    let endpoint = make_client_endpoint(
        backend,
        ClientTransportConfig::new(SocketAddr::from(([0, 0, 0, 0], 0)), ca_pem),
    )?;
    let token = edge_token(role);
    let identity = NodeIdentity::new(role, &token.encode())?;
    let runtime = EdgeRuntime::new(
        identity,
        server_address,
        SERVER_NAME,
        Some(tun_name.clone()),
    )?;
    let (shutdown_sender, mut shutdown_receiver) = watch::channel(false);
    let watcher = tokio::spawn(async move {
        while !stop_file.exists() {
            sleep(Duration::from_millis(25)).await;
        }
        let _ = shutdown_sender.send(true);
    });
    let result = runtime
        .run_with_reconnect_until(
            endpoint,
            Arc::new(NativeTunFactory),
            Arc::new(NativeRouteManager),
            &mut shutdown_receiver,
        )
        .await;
    watcher.abort();
    result?;
    if super::route_points_to_interface(overlay, remote_overlay_address, &tun_name).await? {
        return Err(format!("{role} route remained after graceful shutdown").into());
    }
    Ok(())
}

fn edge_token(role: &str) -> NodeToken {
    match role {
        "edge-a" => NodeToken::from_bytes([0xa1; 32]),
        "edge-b" => NodeToken::from_bytes([0xb2; 32]),
        _ => NodeToken::from_bytes([0; 32]),
    }
}

async fn run_standard_scenario(lab: &NamespaceLab, test_name: &str) -> TestResult {
    let ping = run_namespace_ping(lab, 100, 32, Some("0.02")).await?;
    require_success(ping.clone(), "100-packet overlay ping")?;
    let output = String::from_utf8_lossy(&ping.stdout);
    if !output.contains(", 0% packet loss") {
        return Err(format!("release ping gate was not lossless:\n{output}").into());
    }
    run_application_echo(lab, test_name, 1, 0).await
}

async fn run_application_echo(
    lab: &NamespaceLab,
    test_name: &str,
    iterations: u32,
    interval_ms: u64,
) -> TestResult {
    let ready_file = lab.file("echo-ready");
    let _ = fs::remove_file(&ready_file);
    let mut service = spawn_test_role(
        &lab.namespace_b,
        test_name,
        "echo-service",
        [
            ("FUSEN_REAL_TUN_ITERATIONS", iterations.to_string()),
            (
                "FUSEN_REAL_TUN_READY_FILE",
                ready_file.display().to_string(),
            ),
        ],
    )?;
    wait_for_file_or_child(&ready_file, &mut service, "echo-service").await?;
    let mut probe = spawn_test_role(
        &lab.namespace_a,
        test_name,
        "echo-probe",
        [
            ("FUSEN_REAL_TUN_ITERATIONS", iterations.to_string()),
            ("FUSEN_REAL_TUN_INTERVAL_MS", interval_ms.to_string()),
        ],
    )?;
    let operation_timeout = Duration::from_secs(u64::from(iterations).saturating_mul(2).max(30));
    wait_child_success(&mut probe, "echo-probe", operation_timeout).await?;
    wait_child_success(&mut service, "echo-service", operation_timeout).await
}

async fn run_echo_service_child() -> TestResult {
    let iterations = required_env("FUSEN_REAL_TUN_ITERATIONS")?.parse::<u32>()?;
    let ready_file = PathBuf::from(required_env("FUSEN_REAL_TUN_READY_FILE")?);
    let udp = UdpSocket::bind(SocketAddr::from((EDGE_B_OVERLAY, UDP_PORT))).await?;
    let tcp = tokio::net::TcpListener::bind(SocketAddr::from((EDGE_B_OVERLAY, TCP_PORT))).await?;
    fs::write(ready_file, b"ready")?;

    let udp_task = async {
        let mut buffer = [0_u8; 256];
        for _ in 0..iterations {
            let (length, peer) = udp.recv_from(&mut buffer).await?;
            udp.send_to(&buffer[..length], peer).await?;
        }
        Ok::<(), TestError>(())
    };
    let tcp_task = async {
        for _ in 0..iterations {
            let (mut stream, _) = tcp.accept().await?;
            let mut request = vec![0_u8; ECHO_PAYLOAD.len()];
            stream.read_exact(&mut request).await?;
            stream.write_all(&request).await?;
            stream.shutdown().await?;
        }
        Ok::<(), TestError>(())
    };
    tokio::try_join!(udp_task, tcp_task)?;
    Ok(())
}

async fn run_echo_probe_child() -> TestResult {
    let iterations = required_env("FUSEN_REAL_TUN_ITERATIONS")?.parse::<u32>()?;
    let interval =
        Duration::from_millis(required_env("FUSEN_REAL_TUN_INTERVAL_MS")?.parse::<u64>()?);
    for iteration in 0..iterations {
        let udp = UdpSocket::bind(SocketAddr::from((EDGE_A_OVERLAY, 0))).await?;
        udp.send_to(ECHO_PAYLOAD, SocketAddr::from((EDGE_B_OVERLAY, UDP_PORT)))
            .await?;
        let mut response = [0_u8; 256];
        let (length, source) = timeout(Duration::from_secs(10), udp.recv_from(&mut response))
            .await
            .map_err(|_| "overlay UDP echo timed out")??;
        if source != SocketAddr::from((EDGE_B_OVERLAY, UDP_PORT))
            || &response[..length] != ECHO_PAYLOAD
        {
            return Err("overlay UDP echo mismatch".into());
        }

        let mut tcp = timeout(
            Duration::from_secs(10),
            TcpStream::connect(SocketAddr::from((EDGE_B_OVERLAY, TCP_PORT))),
        )
        .await
        .map_err(|_| "overlay TCP connect timed out")??;
        tcp.write_all(ECHO_PAYLOAD).await?;
        let mut response = vec![0_u8; ECHO_PAYLOAD.len()];
        timeout(Duration::from_secs(10), tcp.read_exact(&mut response))
            .await
            .map_err(|_| "overlay TCP echo timed out")??;
        if response != ECHO_PAYLOAD {
            return Err("overlay TCP echo mismatch".into());
        }
        if iteration + 1 < iterations {
            sleep(interval).await;
        }
    }
    Ok(())
}

async fn run_fault_scenario(lab: &NamespaceLab) -> TestResult {
    run_ping_until_success(lab, Duration::from_secs(10)).await?;

    run_netns_command(
        &lab.namespace_a,
        [
            "tc",
            "qdisc",
            "replace",
            "dev",
            &lab.peer_a,
            "root",
            "netem",
            "loss",
            "10%",
        ],
    )?;
    let impaired = run_namespace_ping(lab, 100, 32, Some("0.01")).await;
    remove_qdisc(lab)?;
    let impaired = impaired?;
    require_success(impaired.clone(), "ping under packet loss")?;
    let loss = ping_packet_loss_percent(&impaired)
        .ok_or("could not parse packet loss from the impaired ping output")?;
    if !(0.0..100.0).contains(&loss) || loss == 0.0 {
        return Err(format!("netem packet-loss gate observed {loss}% loss").into());
    }

    run_netns_command(
        &lab.namespace_a,
        [
            "tc",
            "qdisc",
            "replace",
            "dev",
            &lab.peer_a,
            "root",
            "netem",
            "delay",
            "100ms",
            "reorder",
            "100%",
            "gap",
            "2",
        ],
    )?;
    let reordered = run_namespace_ping(lab, 100, 32, Some("0.01")).await;
    remove_qdisc(lab)?;
    let reordered = reordered?;
    require_success(reordered.clone(), "ping under packet reordering")?;
    if !ping_observed_reordering(&reordered) {
        return Err("netem reordering gate observed no out-of-order ICMP replies".into());
    }

    install_mtu_blackhole(lab)?;
    let small = run_namespace_ping(lab, 1, 64, None).await;
    let large = run_namespace_ping(lab, 1, 1000, None).await;
    remove_qdisc(lab)?;
    require_success(small?, "small ping through MTU black-hole filter")?;
    if large?.status.success() {
        return Err("the MTU black-hole filter did not drop a 1000-byte ping".into());
    }
    require_success(
        run_namespace_ping(lab, 1, 1000, None).await?,
        "large ping after MTU black-hole removal",
    )
}

fn ping_packet_loss_percent(output: &std::process::Output) -> Option<f64> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().find_map(|line| {
        line.split(',').find_map(|field| {
            let field = field.trim();
            let percent = field.strip_suffix("% packet loss")?.trim();
            percent.parse().ok()
        })
    })
}

fn ping_observed_reordering(output: &std::process::Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sequences = stdout.lines().filter_map(|line| {
        let sequence = line.split("icmp_seq=").nth(1)?;
        let digits = sequence
            .as_bytes()
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        sequence[..digits].parse::<u32>().ok()
    });
    let mut previous = None;
    for sequence in sequences {
        if previous.is_some_and(|previous| sequence < previous) {
            return true;
        }
        previous = Some(sequence);
    }
    false
}

fn install_mtu_blackhole(lab: &NamespaceLab) -> TestResult {
    run_netns_command(
        &lab.namespace_a,
        [
            "tc",
            "qdisc",
            "replace",
            "dev",
            &lab.peer_a,
            "root",
            "handle",
            "1:",
            "prio",
        ],
    )?;
    run_netns_command(
        &lab.namespace_a,
        [
            "tc",
            "qdisc",
            "add",
            "dev",
            &lab.peer_a,
            "parent",
            "1:3",
            "handle",
            "30:",
            "netem",
            "loss",
            "100%",
        ],
    )?;
    run_netns_command(
        &lab.namespace_a,
        [
            "tc",
            "filter",
            "add",
            "dev",
            &lab.peer_a,
            "protocol",
            "ip",
            "parent",
            "1:",
            "prio",
            "3",
            "u32",
            "match",
            "u16",
            "0x0400",
            "0xfc00",
            "at",
            "2",
            "flowid",
            "1:3",
        ],
    )
}

fn remove_qdisc(lab: &NamespaceLab) -> TestResult {
    run_netns_command(
        &lab.namespace_a,
        ["tc", "qdisc", "delete", "dev", &lab.peer_a, "root"],
    )
}

async fn run_soak_scenario(
    lab: &NamespaceLab,
    test_name: &str,
    processes: &[ProcessTarget],
) -> TestResult {
    let seconds = std::env::var("FUSEN_REAL_TUN_SOAK_SECONDS")
        .unwrap_or_else(|_| "1800".to_owned())
        .parse::<u32>()?;
    if seconds == 0 {
        return Err("FUSEN_REAL_TUN_SOAK_SECONDS must be positive".into());
    }
    run_ping_until_success(lab, Duration::from_secs(10)).await?;
    run_application_echo(lab, test_name, 1, 0).await?;
    let before = snapshot_process_resources(processes)?;
    run_application_echo(lab, test_name, seconds, 1000).await?;
    let after = snapshot_process_resources(processes)?;
    verify_soak_resource_growth(processes, &before, &after)?;
    require_success(
        run_namespace_ping(lab, 100, 32, Some("0.02")).await?,
        "post-soak lossless ping",
    )
}

fn snapshot_process_resources(processes: &[ProcessTarget]) -> TestResult<Vec<ProcessResources>> {
    processes
        .iter()
        .map(|process| snapshot_process_resource(*process))
        .collect()
}

fn snapshot_process_resource(process: ProcessTarget) -> TestResult<ProcessResources> {
    let process_directory = PathBuf::from(format!("/proc/{}", process.pid));
    let status = fs::read_to_string(process_directory.join("status")).map_err(|error| {
        format!(
            "failed to read {} process {} status: {error}",
            process.name, process.pid
        )
    })?;
    let rss_kib = parse_process_status_value(&status, "VmRSS:").ok_or_else(|| {
        format!(
            "{} process {} status has no VmRSS",
            process.name, process.pid
        )
    })?;
    let threads = parse_process_status_value(&status, "Threads:").ok_or_else(|| {
        format!(
            "{} process {} status has no Threads",
            process.name, process.pid
        )
    })?;
    let open_fds = fs::read_dir(process_directory.join("fd"))
        .map_err(|error| {
            format!(
                "failed to inspect {} process {} descriptors: {error}",
                process.name, process.pid
            )
        })?
        .try_fold(0_u64, |count, entry| {
            entry.map(|_| count + 1).map_err(|error| {
                format!(
                    "failed to inspect {} process {} descriptor: {error}",
                    process.name, process.pid
                )
            })
        })?;
    Ok(ProcessResources {
        rss_kib,
        open_fds,
        threads,
    })
}

fn parse_process_status_value(status: &str, field: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        line.strip_prefix(field)?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

fn verify_soak_resource_growth(
    processes: &[ProcessTarget],
    before: &[ProcessResources],
    after: &[ProcessResources],
) -> TestResult {
    for ((process, before), after) in processes.iter().zip(before).zip(after) {
        let rss_growth = after.rss_kib.saturating_sub(before.rss_kib);
        let fd_growth = after.open_fds.saturating_sub(before.open_fds);
        let thread_growth = after.threads.saturating_sub(before.threads);
        if rss_growth > SOAK_MAX_RSS_GROWTH_KIB
            || fd_growth > SOAK_MAX_FD_GROWTH
            || thread_growth > SOAK_MAX_THREAD_GROWTH
        {
            return Err(format!(
                "{} process {} exceeded soak resource tolerances: before={before:?}, after={after:?}, limits=rss+{}KiB/fd+{}/threads+{}",
                process.name,
                process.pid,
                SOAK_MAX_RSS_GROWTH_KIB,
                SOAK_MAX_FD_GROWTH,
                SOAK_MAX_THREAD_GROWTH,
            )
            .into());
        }
    }
    Ok(())
}

async fn run_namespace_ping(
    lab: &NamespaceLab,
    count: u32,
    payload_size: u16,
    interval: Option<&str>,
) -> TestResult<std::process::Output> {
    let mut command = Command::new("ip");
    command.args([
        "netns",
        "exec",
        &lab.namespace_a,
        "ping",
        "-n",
        "-c",
        &count.to_string(),
        "-W",
        "2",
        "-M",
        "do",
        "-s",
        &payload_size.to_string(),
    ]);
    command.env("LC_ALL", "C");
    if let Some(interval) = interval {
        command.args(["-i", interval]);
    }
    command.arg(EDGE_B_OVERLAY.to_string());
    timeout(
        Duration::from_secs(u64::from(count).saturating_mul(3).max(10)),
        command.output(),
    )
    .await
    .map_err(|_| "namespace ping timed out")?
    .map_err(Into::into)
}

async fn run_ping_until_success(lab: &NamespaceLab, maximum: Duration) -> TestResult {
    let deadline = Instant::now() + maximum;
    loop {
        let output = run_namespace_ping(lab, 1, 32, None).await?;
        if output.status.success() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "overlay ping did not recover within {maximum:?}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        sleep(Duration::from_millis(200)).await;
    }
}

fn spawn_test_role<const N: usize>(
    namespace: &str,
    test_name: &str,
    role: &str,
    variables: [(&str, String); N],
) -> TestResult<tokio::process::Child> {
    let executable = std::env::current_exe()?;
    let mut command = Command::new("ip");
    command
        .args(["netns", "exec", namespace])
        .arg(executable)
        .args([
            "--exact",
            test_name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FUSEN_REAL_TUN_ROLE", role)
        .env("FUSEN_REAL_TUN_BACKEND", selected_backend_name()?)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    for (name, value) in variables {
        command.env(name, value);
    }
    command.spawn().map_err(Into::into)
}

async fn wait_for_edge_route(
    namespace: &str,
    interface: &str,
    child: &mut tokio::process::Child,
) -> TestResult {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if namespace_route_exists(namespace, interface)? {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!("Edge child exited before route installation: {status}").into());
        }
        if Instant::now() >= deadline {
            return Err(format!("route {OVERLAY} was not installed on {interface}").into());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_file_or_child(
    path: &Path,
    child: &mut tokio::process::Child,
    name: &str,
) -> TestResult {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!("{name} exited before becoming ready: {status}").into());
        }
        if Instant::now() >= deadline {
            return Err(format!("{name} did not become ready").into());
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_child_success(
    child: &mut tokio::process::Child,
    name: &str,
    maximum: Duration,
) -> TestResult {
    let status = timeout(maximum, child.wait())
        .await
        .map_err(|_| format!("{name} did not exit within {maximum:?}"))??;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{name} exited with {status}").into())
    }
}

fn namespace_route_exists(namespace: &str, interface: &str) -> TestResult<bool> {
    let output = StdCommand::new("ip")
        .args(["-n", namespace, "-4", "route", "show", "exact", OVERLAY])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "failed to inspect {namespace} route: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        fields
            .windows(2)
            .any(|pair| pair[0] == "dev" && pair[1] == interface)
    }))
}

fn namespace_interface_exists(namespace: &str, interface: &str) -> TestResult<bool> {
    let output = StdCommand::new("ip")
        .args(["-n", namespace, "-o", "link", "show"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "failed to inspect {namespace} interfaces: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.split(':')
            .nth(1)
            .and_then(|name| name.trim().split('@').next())
            .is_some_and(|name| name == interface)
    }))
}

fn selected_backend() -> TestResult<TransportBackend> {
    match selected_backend_name()?.as_str() {
        "quinn" => Ok(TransportBackend::Quinn),
        "s2n" => Ok(TransportBackend::S2n),
        "gm-quic" => Ok(TransportBackend::GmQuic),
        backend => Err(format!("unsupported FUSEN_REAL_TUN_BACKEND {backend:?}").into()),
    }
}

fn selected_backend_name() -> TestResult<String> {
    Ok(std::env::var("FUSEN_REAL_TUN_BACKEND").unwrap_or_else(|_| "quinn".to_owned()))
}

fn reserve_udp_port() -> TestResult<u16> {
    let socket = StdUdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0)))?;
    Ok(socket.local_addr()?.port())
}

fn required_env(name: &str) -> TestResult<String> {
    std::env::var(name)
        .map_err(|_| format!("required environment variable {name} is missing").into())
}

fn ensure_root() -> TestResult {
    let output = StdCommand::new("id").arg("-u").output()?;
    if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "0" {
        Ok(())
    } else {
        Err("Linux namespace E2E must run as root on a disposable runner".into())
    }
}

fn run_ip<const N: usize>(arguments: [&str; N]) -> TestResult {
    run_checked(StdCommand::new("ip").args(arguments), "ip")
}

fn run_netns_command<const N: usize>(namespace: &str, arguments: [&str; N]) -> TestResult {
    let mut command = StdCommand::new("ip");
    command.args(["netns", "exec", namespace]).args(arguments);
    run_checked(&mut command, "network namespace command")
}

fn run_checked(command: &mut StdCommand, operation: &str) -> TestResult {
    let output = command.output()?;
    require_success(output, operation)
}
