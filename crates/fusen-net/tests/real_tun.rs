// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Privileged native-TUN release gates.
//!
//! These tests are ignored by default. Run them through `tests/e2e` so the
//! platform prerequisites and the scenario selection remain explicit.

#![cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]

use std::{
    error::Error,
    net::{Ipv4Addr, SocketAddr},
    process::Output,
    time::{Duration, Instant},
};

use bytes::{BufMut as _, Bytes, BytesMut};
use fusen_net::tun::{
    NativeRouteManager, NativeTunFactory, PacketDevice, RouteManager, TunConfig, TunFactory,
};
use ipnet::Ipv4Net;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpStream, UdpSocket},
    process::Command,
    sync::oneshot,
    time::{sleep, timeout},
};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use uuid::Uuid;

type TestError = Box<dyn Error + Send + Sync + 'static>;
type TestResult<T = ()> = Result<T, TestError>;

const NATIVE_TEST_TIMEOUT: Duration = Duration::from_secs(15);
const NATIVE_MTU: u16 = 1100;
const ECHO_PAYLOAD: &[u8] = b"fusen-net-real-tun-echo";

#[derive(Default)]
struct ProtocolProgress {
    icmp: bool,
    udp: bool,
    tcp: bool,
}

impl ProtocolProgress {
    const fn complete(&self) -> bool {
        self.icmp && self.udp && self.tcp
    }
}

/// Exercises the actual platform adapter and host network stack. The remote
/// overlay address is emulated at the packet boundary, so this test needs only
/// one privileged disposable runner and does not claim to exercise QUIC.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires administrator privileges and a disposable native-TUN runner"]
async fn native_tun_protocol_and_route_lifecycle() {
    if let Err(error) = run_native_tun_protocol_and_route_lifecycle().await {
        panic!("native TUN lifecycle gate failed: {error}");
    }
}

async fn run_native_tun_protocol_and_route_lifecycle() -> TestResult {
    let overlay = native_overlay()?;
    let local = nth_address(overlay, 1)?;
    let remote = nth_address(overlay, 2)?;
    let requested_interface = native_test_interface_name();
    if let Some(interface) = requested_interface.as_deref()
        && native_interface_exists(interface).await?
    {
        return Err(format!("refusing to reuse existing native interface {interface}").into());
    }
    let config = TunConfig {
        name: requested_interface,
        address: local,
        overlay,
        mtu: NATIVE_MTU,
    };

    let factory = NativeTunFactory;
    let manager = NativeRouteManager;
    let device = factory.create(&config).await?;
    let interface = device.name().to_owned();
    let lease = manager.install_overlay_route(&config, &interface).await?;

    let (device, protocol_result) =
        exercise_native_protocols(device, overlay, local, remote, &interface).await;

    let cleanup_started = Instant::now();
    let cleanup_result =
        if device.is_some() || route_points_to_interface(overlay, remote, &interface).await? {
            manager.remove_overlay_route(lease).await
        } else {
            Ok(())
        };
    let route_removed = !route_points_to_interface(overlay, remote, &interface).await?;
    drop(device);
    let interface_cleanup_result = wait_for_native_interface_removal(&interface).await;
    let cleanup_elapsed = cleanup_started.elapsed();

    if let Err(error) = protocol_result {
        cleanup_result?;
        interface_cleanup_result?;
        return Err(error);
    }
    cleanup_result?;
    interface_cleanup_result?;
    if cleanup_elapsed > Duration::from_secs(5) {
        return Err(
            format!("route cleanup took {cleanup_elapsed:?}, exceeding five seconds").into(),
        );
    }
    if !route_removed {
        return Err(format!("route {overlay} still points to {interface} after cleanup").into());
    }
    Ok(())
}

async fn exercise_native_protocols(
    device: Box<dyn PacketDevice>,
    overlay: Ipv4Net,
    local: Ipv4Addr,
    remote: Ipv4Addr,
    interface: &str,
) -> (Option<Box<dyn PacketDevice>>, TestResult) {
    match route_points_to_interface(overlay, remote, interface).await {
        Ok(true) => {}
        Ok(false) => {
            return (
                Some(device),
                Err(format!(
                    "installed route {overlay} does not point to native interface {interface}"
                )
                .into()),
            );
        }
        Err(error) => return (Some(device), Err(error)),
    }

    let (stop_sender, stop_receiver) = oneshot::channel();
    let mut responder = tokio::spawn(run_packet_responder(device, local, remote, stop_receiver));
    let probes = run_native_protocol_probes(local, remote).await;
    let _ = stop_sender.send(());
    match timeout(NATIVE_TEST_TIMEOUT, &mut responder).await {
        Ok(Ok((device, responder_result))) => {
            let result = probes.and(responder_result);
            (Some(device), result)
        }
        Ok(Err(error)) => (None, Err(error.into())),
        Err(_) => {
            responder.abort();
            let _ = responder.await;
            (
                None,
                Err("native packet responder did not stop after protocol probes".into()),
            )
        }
    }
}

fn native_overlay() -> TestResult<Ipv4Net> {
    std::env::var("FUSEN_REAL_TUN_NATIVE_OVERLAY")
        .unwrap_or_else(|_| "198.19.253.0/24".to_owned())
        .parse()
        .map_err(Into::into)
}

fn nth_address(overlay: Ipv4Net, host: u32) -> TestResult<Ipv4Addr> {
    let address = u32::from(overlay.network())
        .checked_add(host)
        .map(Ipv4Addr::from)
        .ok_or("overlay address overflow")?;
    if !overlay.contains(&address) || address == overlay.broadcast() {
        return Err(format!("{overlay} has no usable host address at offset {host}").into());
    }
    Ok(address)
}

fn native_test_interface_name() -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let identifier = Uuid::new_v4().simple().to_string();
        Some(format!("fne2e{}", &identifier[..8]))
    }
    #[cfg(target_os = "macos")]
    {
        // utun selects an unused platform-valid unit and exposes the actual
        // name through PacketDevice::name.
        None
    }
}

async fn wait_for_native_interface_removal(interface: &str) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if !native_interface_exists(interface).await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                format!("native interface {interface} remained after device shutdown").into(),
            );
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn native_interface_exists(interface: &str) -> TestResult<bool> {
    #[cfg(target_os = "linux")]
    let output = Command::new("ip")
        .args(["-o", "link", "show"])
        .output()
        .await?;

    #[cfg(target_os = "macos")]
    let output = Command::new("ifconfig").arg("-l").output().await?;

    #[cfg(target_os = "windows")]
    let output = {
        let script = "& { param($alias) @(Get-NetAdapter -Name $alias -IncludeHidden -ErrorAction SilentlyContinue).Count }";
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
                interface,
            ])
            .output()
            .await?
    };

    require_success(output.clone(), "inspect native interface")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    #[cfg(target_os = "linux")]
    return Ok(stdout.lines().any(|line| {
        line.split(':')
            .nth(1)
            .and_then(|name| name.trim().split('@').next())
            .is_some_and(|name| name == interface)
    }));
    #[cfg(target_os = "macos")]
    return Ok(stdout.split_whitespace().any(|name| name == interface));
    #[cfg(target_os = "windows")]
    return stdout
        .trim()
        .parse::<usize>()
        .map(|count| count > 0)
        .map_err(Into::into);
}

async fn run_native_protocol_probes(local: Ipv4Addr, remote: Ipv4Addr) -> TestResult {
    run_native_ping(remote).await?;

    let udp = UdpSocket::bind(SocketAddr::from((local, 0))).await?;
    udp.send_to(ECHO_PAYLOAD, SocketAddr::from((remote, 39001)))
        .await?;
    let mut udp_response = [0_u8; 128];
    let (received, source) = timeout(NATIVE_TEST_TIMEOUT, udp.recv_from(&mut udp_response))
        .await
        .map_err(|_| "UDP probe timed out")??;
    if source.ip() != remote || &udp_response[..received] != ECHO_PAYLOAD {
        return Err(format!("unexpected UDP echo from {source}").into());
    }

    let mut tcp = timeout(
        NATIVE_TEST_TIMEOUT,
        TcpStream::connect(SocketAddr::from((remote, 39002))),
    )
    .await
    .map_err(|_| "TCP connect timed out")??;
    tcp.write_all(ECHO_PAYLOAD).await?;
    let mut tcp_response = vec![0_u8; ECHO_PAYLOAD.len()];
    timeout(NATIVE_TEST_TIMEOUT, tcp.read_exact(&mut tcp_response))
        .await
        .map_err(|_| "TCP echo timed out")??;
    if tcp_response != ECHO_PAYLOAD {
        return Err("unexpected TCP echo payload".into());
    }
    Ok(())
}

async fn run_native_ping(remote: Ipv4Addr) -> TestResult {
    let mut command = if cfg!(target_os = "windows") {
        let mut command = Command::new("ping.exe");
        command.args(["-n", "1", "-w", "2000", &remote.to_string()]);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("ping");
        command.args(["-n", "-c", "1", "-W", "2000", &remote.to_string()]);
        command
    } else {
        let mut command = Command::new("ping");
        command.args(["-n", "-c", "1", "-W", "2", &remote.to_string()]);
        command
    };
    let output = timeout(NATIVE_TEST_TIMEOUT, command.output())
        .await
        .map_err(|_| "ping probe timed out")??;
    require_success(output, "native overlay ping")
}

async fn run_packet_responder(
    mut device: Box<dyn PacketDevice>,
    local: Ipv4Addr,
    remote: Ipv4Addr,
    mut stop: oneshot::Receiver<()>,
) -> (Box<dyn PacketDevice>, TestResult) {
    let result = async {
        let mut progress = ProtocolProgress::default();
        let server_sequence = 0x51f0_0001_u32;
        while !progress.complete() {
            let packet = tokio::select! {
                result = timeout(NATIVE_TEST_TIMEOUT, device.read_packet()) => {
                    result.map_err(|_| "timed out waiting for a packet from the host stack")??
                }
                _ = &mut stop => return Ok(()),
            };
            let Some(ipv4) = ParsedIpv4::parse(&packet) else {
                continue;
            };
            if ipv4.source != local || ipv4.destination != remote {
                continue;
            }
            let response = match ipv4.protocol {
                1 => icmp_echo_response(&ipv4).inspect(|_| {
                    progress.icmp = true;
                }),
                17 => udp_echo_response(&ipv4).inspect(|_| {
                    progress.udp = true;
                }),
                6 => tcp_response(&ipv4, server_sequence).map(|(packet, echoed)| {
                    progress.tcp |= echoed;
                    packet
                }),
                _ => None,
            };
            if let Some(response) = response {
                tokio::select! {
                    result = device.write_packet(response) => result?,
                    _ = &mut stop => return Ok(()),
                }
            }
        }
        Ok(())
    }
    .await;
    (device, result)
}

struct ParsedIpv4<'a> {
    source: Ipv4Addr,
    destination: Ipv4Addr,
    protocol: u8,
    payload: &'a [u8],
}

impl<'a> ParsedIpv4<'a> {
    fn parse(packet: &'a [u8]) -> Option<Self> {
        if packet.len() < 20 || packet[0] >> 4 != 4 {
            return None;
        }
        let header_length = usize::from(packet[0] & 0x0f) * 4;
        let total_length = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
        if header_length < 20 || total_length < header_length || total_length > packet.len() {
            return None;
        }
        Some(Self {
            source: Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]),
            destination: Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]),
            protocol: packet[9],
            payload: &packet[header_length..total_length],
        })
    }
}

fn icmp_echo_response(request: &ParsedIpv4<'_>) -> Option<Bytes> {
    if request.payload.len() < 8 || request.payload[0] != 8 || request.payload[1] != 0 {
        return None;
    }
    let mut icmp = request.payload.to_vec();
    icmp[0] = 0;
    icmp[2..4].fill(0);
    let checksum = internet_checksum(&icmp);
    icmp[2..4].copy_from_slice(&checksum.to_be_bytes());
    Some(ipv4_packet(request.destination, request.source, 1, &icmp))
}

fn udp_echo_response(request: &ParsedIpv4<'_>) -> Option<Bytes> {
    if request.payload.len() < 8 {
        return None;
    }
    let udp_length = usize::from(u16::from_be_bytes([request.payload[4], request.payload[5]]));
    if udp_length < 8 || udp_length > request.payload.len() {
        return None;
    }
    let mut udp = request.payload[..udp_length].to_vec();
    let source_port = [udp[0], udp[1]];
    let destination_port = [udp[2], udp[3]];
    udp[0..2].copy_from_slice(&destination_port);
    udp[2..4].copy_from_slice(&source_port);
    // A zero UDP checksum is valid for IPv4 and avoids depending on host
    // checksum-offload metadata at the TUN boundary.
    udp[6..8].fill(0);
    Some(ipv4_packet(request.destination, request.source, 17, &udp))
}

fn tcp_response(request: &ParsedIpv4<'_>, server_sequence: u32) -> Option<(Bytes, bool)> {
    if request.payload.len() < 20 {
        return None;
    }
    let tcp_header_length = usize::from(request.payload[12] >> 4) * 4;
    if tcp_header_length < 20 || tcp_header_length > request.payload.len() {
        return None;
    }
    let flags = request.payload[13];
    let client_sequence = u32::from_be_bytes(request.payload[4..8].try_into().ok()?);
    let data = &request.payload[tcp_header_length..];
    let source_port = u16::from_be_bytes(request.payload[0..2].try_into().ok()?);
    let destination_port = u16::from_be_bytes(request.payload[2..4].try_into().ok()?);

    if flags & 0x02 != 0 && flags & 0x10 == 0 {
        let segment = tcp_segment(
            request.destination,
            request.source,
            destination_port,
            source_port,
            server_sequence,
            client_sequence.wrapping_add(1),
            0x12,
            &[],
        );
        return Some((
            ipv4_packet(request.destination, request.source, 6, &segment),
            false,
        ));
    }

    if !data.is_empty() {
        let segment = tcp_segment(
            request.destination,
            request.source,
            destination_port,
            source_port,
            server_sequence.wrapping_add(1),
            client_sequence.wrapping_add(data.len() as u32),
            0x18,
            data,
        );
        return Some((
            ipv4_packet(request.destination, request.source, 6, &segment),
            true,
        ));
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn tcp_segment(
    source: Ipv4Addr,
    destination: Ipv4Addr,
    source_port: u16,
    destination_port: u16,
    sequence: u32,
    acknowledgement: u32,
    flags: u8,
    payload: &[u8],
) -> Vec<u8> {
    let mut segment = vec![0_u8; 20 + payload.len()];
    segment[0..2].copy_from_slice(&source_port.to_be_bytes());
    segment[2..4].copy_from_slice(&destination_port.to_be_bytes());
    segment[4..8].copy_from_slice(&sequence.to_be_bytes());
    segment[8..12].copy_from_slice(&acknowledgement.to_be_bytes());
    segment[12] = 5 << 4;
    segment[13] = flags;
    segment[14..16].copy_from_slice(&u16::MAX.to_be_bytes());
    segment[20..].copy_from_slice(payload);

    let mut pseudo_header = Vec::with_capacity(12 + segment.len());
    pseudo_header.extend_from_slice(&source.octets());
    pseudo_header.extend_from_slice(&destination.octets());
    pseudo_header.push(0);
    pseudo_header.push(6);
    pseudo_header.extend_from_slice(&(segment.len() as u16).to_be_bytes());
    pseudo_header.extend_from_slice(&segment);
    let checksum = internet_checksum(&pseudo_header);
    segment[16..18].copy_from_slice(&checksum.to_be_bytes());
    segment
}

fn ipv4_packet(source: Ipv4Addr, destination: Ipv4Addr, protocol: u8, payload: &[u8]) -> Bytes {
    let total_length = 20 + payload.len();
    let mut packet = BytesMut::with_capacity(total_length);
    packet.put_u8(0x45);
    packet.put_u8(0);
    packet.put_u16(total_length as u16);
    packet.put_u16(0x4242);
    packet.put_u16(0);
    packet.put_u8(64);
    packet.put_u8(protocol);
    packet.put_u16(0);
    packet.extend_from_slice(&source.octets());
    packet.extend_from_slice(&destination.octets());
    packet.extend_from_slice(payload);
    let checksum = internet_checksum(&packet[..20]);
    packet[10..12].copy_from_slice(&checksum.to_be_bytes());
    packet.freeze()
}

fn internet_checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0_u32;
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    if let [last] = chunks.remainder() {
        sum += u32::from(*last) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

async fn route_points_to_interface(
    #[cfg_attr(target_os = "macos", allow(unused_variables))] overlay: Ipv4Net,
    #[cfg_attr(
        any(target_os = "linux", target_os = "windows"),
        allow(unused_variables)
    )]
    remote: Ipv4Addr,
    interface: &str,
) -> TestResult<bool> {
    #[cfg(target_os = "linux")]
    let output = Command::new("ip")
        .args(["-4", "route", "show", "exact", &overlay.to_string()])
        .output()
        .await?;

    #[cfg(target_os = "macos")]
    let output = Command::new("route")
        .args(["-n", "get", "-inet", &remote.to_string()])
        .output()
        .await?;

    #[cfg(target_os = "windows")]
    let output = {
        let script = "& { param($prefix, $alias) @(Get-NetRoute -DestinationPrefix $prefix -InterfaceAlias $alias -PolicyStore ActiveStore -ErrorAction SilentlyContinue).Count }";
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
                &overlay.to_string(),
                interface,
            ])
            .output()
            .await?
    };

    #[cfg(not(target_os = "macos"))]
    require_success(output.clone(), "inspect native overlay route")?;
    #[cfg(target_os = "macos")]
    if !output.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    #[cfg(target_os = "linux")]
    return Ok(stdout.lines().any(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        fields
            .windows(2)
            .any(|pair| pair[0] == "dev" && pair[1] == interface)
    }));
    #[cfg(target_os = "macos")]
    return Ok(stdout.lines().any(|line| {
        line.trim_start()
            .strip_prefix("interface:")
            .is_some_and(|value| value.trim() == interface)
    }));
    #[cfg(target_os = "windows")]
    return Ok(stdout.trim().parse::<usize>().unwrap_or(0) > 0);
}

fn require_success(output: Output, operation: &str) -> TestResult {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{operation} failed with {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
    .into())
}

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[path = "real_tun/linux_namespace.rs"]
mod linux_namespace;

#[cfg(target_os = "linux")]
use linux_namespace::{NamespaceScenario, run_child_role, run_namespace_scenario};

#[cfg(target_os = "linux")]
async fn dispatch_linux_test(scenario: NamespaceScenario, test_name: &'static str) {
    let result = match std::env::var("FUSEN_REAL_TUN_ROLE") {
        Ok(role) => run_child_role(&role).await,
        Err(_) => run_namespace_scenario(scenario, test_name).await,
    };
    if let Err(error) = result {
        panic!("Linux namespace {scenario:?} gate failed: {error}");
    }
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires root, iproute2, ping, and Linux network namespaces"]
async fn linux_namespace_overlay_e2e() {
    dispatch_linux_test(NamespaceScenario::Standard, "linux_namespace_overlay_e2e").await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires root, iproute2, ping, and Linux network namespaces"]
async fn linux_namespace_relay_restart() {
    dispatch_linux_test(
        NamespaceScenario::RelayRestart,
        "linux_namespace_relay_restart",
    )
    .await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires root, iproute2 tc, ping, and Linux network namespaces"]
async fn linux_namespace_fault_injection() {
    dispatch_linux_test(
        NamespaceScenario::FaultInjection,
        "linux_namespace_fault_injection",
    )
    .await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "release-only 30 minute privileged Linux soak"]
async fn linux_namespace_soak() {
    dispatch_linux_test(NamespaceScenario::Soak, "linux_namespace_soak").await;
}
