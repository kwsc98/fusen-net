use crate::buffer::QuicBuffer;
use quinn::{ClientConfig, Endpoint};
use std::{
    net::{SocketAddr, UdpSocket},
    sync::Arc,
};
use support::make_client_endpoint;
pub mod support;

pub async fn connect(target_host: SocketAddr) -> Result<(QuicBuffer, SocketAddr), crate::Error> {
    let mut endpoint = make_client_endpoint("0.0.0.0:0".parse()?, &[])?;
    let local_addr = endpoint.local_addr().unwrap();
    endpoint.set_default_client_config(get_config());
    let connection = endpoint.connect(target_host, "fusen-net")?.await?;
    let (send_stream, recv_stream) = connection.open_bi().await?;
    let buffer = QuicBuffer::new(send_stream, recv_stream);
    Ok((buffer, local_addr))
}
pub async fn connect_reuse(
    endpoint: &mut Endpoint,
    target_host: SocketAddr,
) -> Result<(QuicBuffer, SocketAddr), crate::Error> {
    let local_addr = endpoint.local_addr().unwrap();
    endpoint.set_default_client_config(get_config());
    let connection = endpoint.connect(target_host, "fusen-net")?.await?;
    let (send_stream, recv_stream) = connection.open_bi().await?;
    let buffer = QuicBuffer::new(send_stream, recv_stream);
    Ok((buffer, local_addr))
}

struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

pub fn get_config() -> ClientConfig {
    let crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();
    ClientConfig::new(Arc::new(crypto))
}
