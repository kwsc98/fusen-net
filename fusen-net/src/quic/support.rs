//! Commonly used code in most examples.

use quinn::{default_runtime, ClientConfig, Endpoint, EndpointConfig, ServerConfig};
use std::net::TcpListener;
use std::os::unix::io::AsRawFd;
use std::{net::SocketAddr, sync::Arc};
use tokio::io;

/// Constructs a QUIC endpoint configured for use a client only.
///
/// ## Args
///
/// - server_certs: list of trusted certificates.
#[allow(unused)]
pub fn make_client_endpoint(
    bind_addr: SocketAddr,
    server_certs: &[&[u8]],
) -> Result<Endpoint, crate::Error> {
    let mut socket = std::net::UdpSocket::bind(bind_addr)?;
    // 设置端口复用
    let socket_raw_fd = socket.as_raw_fd();
    let enable = 1;
    unsafe {
        // 调用系统API设置SO_REUSEPORT
        libc::setsockopt(
            socket_raw_fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            &enable as *const _ as *const libc::c_void,
            std::mem::size_of_val(&enable) as libc::socklen_t,
        );
    }
    let runtime = default_runtime()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no async runtime found"))?;
    Endpoint::new(EndpointConfig::default(), None, socket, runtime)
        .map_err(|error| format!("make_client_endpoint error : {:?}", error).into())
}

/// Constructs a QUIC endpoint configured to listen for incoming connections on a certain address
/// and port.
///
/// ## Returns
///
/// - a stream of incoming QUIC connections
/// - server certificate serialized into DER format
#[allow(unused)]
pub fn make_server_endpoint(bind_addr: SocketAddr) -> Result<(Endpoint, Vec<u8>), crate::Error> {
    let (server_config, server_cert) = configure_server()?;
    let mut socket = std::net::UdpSocket::bind(bind_addr)?;
    // 设置端口复用
    let socket_raw_fd = socket.as_raw_fd();
    let enable = 1;
    unsafe {
        // 调用系统API设置SO_REUSEPORT
        libc::setsockopt(
            socket_raw_fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            &enable as *const _ as *const libc::c_void,
            std::mem::size_of_val(&enable) as libc::socklen_t,
        );
    }
    let runtime = default_runtime()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "no async runtime found"))?;
    let endpoint = Endpoint::new(
        EndpointConfig::default(),
        Some(server_config),
        socket,
        runtime,
    )?;
    Ok((endpoint, server_cert))
}

/// Builds default quinn client config and trusts given certificates.
///
/// ## Args
///
/// - server_certs: a list of trusted certificates in DER format.
fn configure_client(server_certs: &[&[u8]]) -> Result<ClientConfig, crate::Error> {
    let mut certs = rustls::RootCertStore::empty();
    for cert in server_certs {
        certs.add(&rustls::Certificate(cert.to_vec()))?;
    }

    let client_config = ClientConfig::with_root_certificates(certs);
    Ok(client_config)
}

/// Returns default server configuration along with its certificate.
fn configure_server() -> Result<(ServerConfig, Vec<u8>), crate::Error> {
    let cert = rcgen::generate_simple_self_signed(vec!["fusen-net".into()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let priv_key = cert.serialize_private_key_der();
    let priv_key = rustls::PrivateKey(priv_key);
    let cert_chain = vec![rustls::Certificate(cert_der.clone())];
    let mut server_config = ServerConfig::with_single_cert(cert_chain, priv_key)?;
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_concurrent_uni_streams(0_u8.into());
    Ok((server_config, cert_der))
}

#[allow(unused)]
pub const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];
