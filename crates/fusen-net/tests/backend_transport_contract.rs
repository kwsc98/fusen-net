// SPDX-License-Identifier: Apache-2.0 OR MIT

#![cfg(any(
    feature = "backend-quinn",
    feature = "backend-s2n",
    feature = "backend-gm-quic"
))]

use std::{
    future::Future,
    io,
    net::SocketAddr,
    sync::{Arc, OnceLock},
    time::Duration,
};

use bytes::Bytes;
use fusen_net::transport::{
    BoxReadStream, BoxWriteStream, ClientTransportConfig, ServerTransportConfig, TransportBackend,
    TransportConnection, TransportEndpoint, TransportError, make_client_endpoint,
    make_server_endpoint,
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
    time::timeout,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const STEP_TIMEOUT: Duration = Duration::from_secs(5);
const SERVER_NAME: &str = "localhost";

fn contract_lock() -> &'static Mutex<()> {
    static CONTRACT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    CONTRACT_LOCK.get_or_init(|| Mutex::new(()))
}

struct TestCertificate {
    certificate_pem: String,
    private_key_pem: String,
}

impl TestCertificate {
    fn generate() -> Self {
        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec![SERVER_NAME.to_owned()])
                .expect("localhost test certificate generation");
        Self {
            certificate_pem: cert.pem(),
            private_key_pem: key_pair.serialize_pem(),
        }
    }
}

async fn within<T>(
    backend: TransportBackend,
    operation: &str,
    future: impl Future<Output = T>,
) -> T {
    timeout(STEP_TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("{backend:?} {operation} timed out after {STEP_TIMEOUT:?}"))
}

fn temporary_udp_address() -> SocketAddr {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("reserve local UDP port");
    socket.local_addr().expect("reserved UDP address")
}

fn endpoints(
    backend: TransportBackend,
    server_address: SocketAddr,
    certificate: &TestCertificate,
) -> (Arc<dyn TransportEndpoint>, Arc<dyn TransportEndpoint>) {
    let server = make_server_endpoint(
        backend,
        ServerTransportConfig::new(
            server_address,
            SERVER_NAME,
            certificate.certificate_pem.clone(),
            certificate.private_key_pem.clone(),
        ),
    )
    .unwrap_or_else(|error| panic!("{backend:?} server endpoint creation failed: {error}"));
    let client = make_client_endpoint(
        backend,
        ClientTransportConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            certificate.certificate_pem.clone(),
        ),
    )
    .unwrap_or_else(|error| panic!("{backend:?} client endpoint creation failed: {error}"));
    (server, client)
}

fn assert_fixed_alpn_is_enforced(backend: TransportBackend, certificate: &TestCertificate) {
    let mut server_config = ServerTransportConfig::new(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        SERVER_NAME,
        certificate.certificate_pem.clone(),
        certificate.private_key_pem.clone(),
    );
    server_config.alpn = b"not-fusen-net".to_vec();
    match make_server_endpoint(backend, server_config) {
        Err(TransportError::InvalidConfiguration(message)) => {
            assert!(message.contains("ALPN"), "unexpected ALPN error: {message}");
        }
        Err(error) => panic!("{backend:?} returned the wrong invalid-ALPN error: {error}"),
        Ok(_) => panic!("{backend:?} accepted a server endpoint with the wrong ALPN"),
    }

    let mut client_config = ClientTransportConfig::new(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        certificate.certificate_pem.clone(),
    );
    client_config.alpn = b"not-fusen-net".to_vec();
    match make_client_endpoint(backend, client_config) {
        Err(TransportError::InvalidConfiguration(message)) => {
            assert!(message.contains("ALPN"), "unexpected ALPN error: {message}");
        }
        Err(error) => panic!("{backend:?} returned the wrong invalid-ALPN error: {error}"),
        Ok(_) => panic!("{backend:?} accepted a client endpoint with the wrong ALPN"),
    }
}

async fn assert_wrong_sni_is_rejected(
    backend: TransportBackend,
    server_address: SocketAddr,
    server: Arc<dyn TransportEndpoint>,
    client: Arc<dyn TransportEndpoint>,
) {
    let accept_task = tokio::spawn(async move { server.accept().await });

    let result = timeout(
        CONNECT_TIMEOUT,
        client.connect(server_address, "wrong-name.invalid"),
    )
    .await
    .unwrap_or_else(|_| {
        panic!("{backend:?} wrong-SNI connection did not fail within {CONNECT_TIMEOUT:?}")
    });
    accept_task.abort();
    let _ = accept_task.await;

    match result {
        Err(TransportError::Legacy(error)) => {
            assert!(
                !error.to_string().is_empty(),
                "{backend:?} swallowed the TLS/SNI failure"
            );
        }
        Err(error) => panic!("{backend:?} returned the wrong SNI error variant: {error}"),
        Ok(_) => panic!("{backend:?} accepted a certificate for the wrong SNI"),
    }
}

async fn write_control_message(
    backend: TransportBackend,
    operation: &str,
    writer: &mut BoxWriteStream,
    payload: &[u8],
) {
    within(backend, operation, async {
        writer.write_all(payload).await?;
        writer.flush().await?;
        Ok::<(), io::Error>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{backend:?} {operation} failed: {error}"));
}

async fn read_control_message(
    backend: TransportBackend,
    operation: &str,
    reader: &mut BoxReadStream,
    payload: &[u8],
) {
    within(backend, operation, async {
        let mut received = vec![0_u8; payload.len()];
        reader.read_exact(&mut received).await?;
        if received != payload {
            return Err(io::Error::other(format!(
                "control payload mismatch: expected {payload:?}, received {received:?}"
            )));
        }
        Ok::<(), io::Error>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{backend:?} {operation} failed: {error}"));
}

async fn receive_datagram(
    backend: TransportBackend,
    operation: &str,
    connection: &mut Box<dyn TransportConnection>,
) -> Bytes {
    within(backend, operation, connection.recv_datagram())
        .await
        .unwrap_or_else(|error| panic!("{backend:?} {operation} failed: {error}"))
}

async fn run_transport_contract(backend: TransportBackend) {
    let _contract_guard = contract_lock().lock().await;
    let certificate = TestCertificate::generate();
    assert_fixed_alpn_is_enforced(backend, &certificate);

    let server_address = temporary_udp_address();
    let (server_endpoint, client_endpoint) = endpoints(backend, server_address, &certificate);
    let connected = timeout(CONNECT_TIMEOUT, async {
        tokio::try_join!(
            server_endpoint.accept(),
            client_endpoint.connect(server_address, SERVER_NAME)
        )
    })
    .await
    .unwrap_or_else(|_| {
        panic!("{backend:?} TLS connection did not complete within {CONNECT_TIMEOUT:?}")
    })
    .unwrap_or_else(|error| panic!("{backend:?} TLS connection failed: {error}"));
    let (mut server_connection, mut client_connection) = connected;

    assert_eq!(client_connection.remote_address(), server_address);
    assert!(server_connection.remote_address().ip().is_loopback());
    assert_ne!(server_connection.remote_address().port(), 0);

    let (mut client_reader, mut client_writer) = within(
        backend,
        "client bidirectional stream open",
        client_connection.open_bi(),
    )
    .await
    .unwrap_or_else(|error| panic!("{backend:?} client stream open failed: {error}"));
    write_control_message(
        backend,
        "client-to-server control write",
        &mut client_writer,
        b"control from client",
    )
    .await;
    let (mut server_reader, mut server_writer) = within(
        backend,
        "server bidirectional stream accept",
        server_connection.accept_bi(),
    )
    .await
    .unwrap_or_else(|error| panic!("{backend:?} server stream accept failed: {error}"));
    read_control_message(
        backend,
        "client-to-server control read",
        &mut server_reader,
        b"control from client",
    )
    .await;

    write_control_message(
        backend,
        "server-to-client control write",
        &mut server_writer,
        b"control from server",
    )
    .await;
    read_control_message(
        backend,
        "server-to-client control read",
        &mut client_reader,
        b"control from server",
    )
    .await;

    match client_connection.send_datagram(Bytes::from(vec![0_u8; usize::from(u16::MAX)])) {
        Err(TransportError::DatagramTooLarge) => {}
        Err(error) => panic!("{backend:?} returned the wrong oversized Datagram error: {error}"),
        Ok(()) => panic!("{backend:?} accepted an oversized Datagram"),
    }

    let client_datagram = Bytes::from_static(b"datagram from client");
    client_connection
        .send_datagram(client_datagram.clone())
        .unwrap_or_else(|error| panic!("{backend:?} client Datagram send failed: {error}"));
    assert_eq!(
        receive_datagram(
            backend,
            "client-to-server Datagram receive",
            &mut server_connection
        )
        .await,
        client_datagram
    );

    let server_datagram = Bytes::from_static(b"datagram from server");
    server_connection
        .send_datagram(server_datagram.clone())
        .unwrap_or_else(|error| panic!("{backend:?} server Datagram send failed: {error}"));
    assert_eq!(
        receive_datagram(
            backend,
            "server-to-client Datagram receive",
            &mut client_connection
        )
        .await,
        server_datagram
    );

    client_connection
        .send_datagram(Bytes::new())
        .unwrap_or_else(|error| panic!("{backend:?} empty Datagram send failed: {error}"));
    assert_eq!(
        receive_datagram(backend, "empty Datagram receive", &mut server_connection).await,
        Bytes::new()
    );

    let first_queued = Bytes::from_static(b"first queued Datagram");
    let second_queued = Bytes::from_static(b"second queued Datagram");
    client_connection
        .send_datagram(first_queued.clone())
        .unwrap_or_else(|error| panic!("{backend:?} first queued Datagram failed: {error}"));
    client_connection
        .send_datagram(second_queued.clone())
        .unwrap_or_else(|error| panic!("{backend:?} second queued Datagram failed: {error}"));
    assert_eq!(
        receive_datagram(
            backend,
            "first queued Datagram receive",
            &mut server_connection
        )
        .await,
        first_queued
    );
    assert_eq!(
        receive_datagram(
            backend,
            "second queued Datagram receive",
            &mut server_connection
        )
        .await,
        second_queued
    );

    let overlay_mtu_datagram = Bytes::from(vec![0x5a; 1100]);
    client_connection
        .send_datagram(overlay_mtu_datagram.clone())
        .unwrap_or_else(|error| panic!("{backend:?} 1100-byte Datagram send failed: {error}"));
    assert_eq!(
        receive_datagram(
            backend,
            "1100-byte Datagram receive",
            &mut server_connection
        )
        .await,
        overlay_mtu_datagram
    );

    let pressure_datagram = Bytes::from(vec![0xa5; 1100]);
    let mut accepted_under_pressure = 0_u64;
    let mut rejected_under_pressure = 0_u64;
    for _ in 0..4096 {
        match client_connection.send_datagram(pressure_datagram.clone()) {
            Ok(()) => accepted_under_pressure += 1,
            Err(TransportError::DatagramQueueFull) => rejected_under_pressure += 1,
            Err(error) => panic!("{backend:?} pressure send failed unexpectedly: {error}"),
        }
    }
    assert!(
        accepted_under_pressure > 0,
        "{backend:?} rejected every pressure Datagram"
    );
    assert!(
        rejected_under_pressure > 0,
        "{backend:?} did not expose its bounded send queue under pressure"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let receive_drops = server_connection
        .dropped_incoming_datagrams()
        .unwrap_or_else(|error| panic!("{backend:?} receive drop metric failed: {error}"));
    assert!(receive_drops <= accepted_under_pressure);

    assert_wrong_sni_is_rejected(
        backend,
        server_address,
        server_endpoint.clone(),
        client_endpoint.clone(),
    )
    .await;

    within(backend, "control stream shutdown", async {
        client_writer.shutdown().await?;
        server_writer.shutdown().await?;
        Ok::<(), io::Error>(())
    })
    .await
    .unwrap_or_else(|error| panic!("{backend:?} control stream shutdown failed: {error}"));
    drop(client_reader);
    drop(client_writer);
    drop(server_reader);
    drop(server_writer);
    drop(server_connection);
    drop(server_endpoint);

    let close_error = within(
        backend,
        "remote close notification",
        client_connection.closed(),
    )
    .await;
    assert!(
        matches!(close_error, TransportError::Legacy(_)),
        "{backend:?} returned the wrong close error variant: {close_error}"
    );
    assert!(
        !close_error.to_string().is_empty(),
        "{backend:?} swallowed the remote close reason"
    );

    match within(
        backend,
        "Datagram receive after remote close",
        client_connection.recv_datagram(),
    )
    .await
    {
        Err(TransportError::Legacy(error)) => {
            assert!(
                !error.to_string().is_empty(),
                "{backend:?} swallowed the post-close Datagram error"
            );
        }
        Err(error) => panic!("{backend:?} returned the wrong post-close error: {error}"),
        Ok(packet) => panic!("{backend:?} produced a Datagram after remote close: {packet:?}"),
    }
}

#[cfg(feature = "backend-quinn")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quinn_transport_contract() {
    run_transport_contract(TransportBackend::Quinn).await;
}

#[cfg(feature = "backend-s2n")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2n_transport_contract() {
    run_transport_contract(TransportBackend::S2n).await;
}

#[cfg(feature = "backend-gm-quic")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gm_quic_transport_contract() {
    run_transport_contract(TransportBackend::GmQuic).await;
}
