// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Object-safe transport boundaries used by role-neutral node runtimes.

use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{control::ALPN, error::FusenNetError};

pub type BoxReadStream = Pin<Box<dyn AsyncRead + Send + Unpin + 'static>>;
pub type BoxWriteStream = Pin<Box<dyn AsyncWrite + Send + Unpin + 'static>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportBackend {
    Quinn,
    S2n,
    GmQuic,
}

#[async_trait]
pub trait TransportConnection: Send + Sync + 'static {
    async fn open_bi(&self) -> Result<(BoxReadStream, BoxWriteStream), TransportError>;

    async fn accept_bi(&mut self) -> Result<(BoxReadStream, BoxWriteStream), TransportError>;

    fn send_datagram(&self, packet: Bytes) -> Result<(), TransportError>;

    async fn recv_datagram(&mut self) -> Result<Bytes, TransportError>;

    fn dropped_incoming_datagrams(&self) -> Result<u64, TransportError> {
        Ok(0)
    }

    fn remote_address(&self) -> SocketAddr;

    async fn closed(&self) -> TransportError;
}

#[async_trait]
pub trait TransportEndpoint: Send + Sync + 'static {
    async fn accept(&self) -> Result<Box<dyn TransportConnection>, TransportError>;

    async fn connect(
        &self,
        address: SocketAddr,
        server_name: &str,
    ) -> Result<Box<dyn TransportConnection>, TransportError>;
}

#[derive(Clone)]
pub struct ServerTransportConfig {
    pub bind_address: SocketAddr,
    pub server_name: String,
    pub certificate_pem: String,
    pub private_key_pem: String,
    pub alpn: Vec<u8>,
}

impl ServerTransportConfig {
    pub fn new(
        bind_address: SocketAddr,
        server_name: impl Into<String>,
        certificate_pem: impl Into<String>,
        private_key_pem: impl Into<String>,
    ) -> Self {
        Self {
            bind_address,
            server_name: server_name.into(),
            certificate_pem: certificate_pem.into(),
            private_key_pem: private_key_pem.into(),
            alpn: ALPN.to_vec(),
        }
    }
}

impl fmt::Debug for ServerTransportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerTransportConfig")
            .field("bind_address", &self.bind_address)
            .field("server_name", &self.server_name)
            .field("certificate_pem", &"[PEM REDACTED]")
            .field("private_key_pem", &"[REDACTED]")
            .field("alpn", &String::from_utf8_lossy(&self.alpn))
            .finish()
    }
}

#[derive(Clone)]
pub struct ClientTransportConfig {
    pub bind_address: SocketAddr,
    pub ca_certificate_pem: String,
    pub alpn: Vec<u8>,
}

impl ClientTransportConfig {
    pub fn new(bind_address: SocketAddr, ca_certificate_pem: impl Into<String>) -> Self {
        Self {
            bind_address,
            ca_certificate_pem: ca_certificate_pem.into(),
            alpn: ALPN.to_vec(),
        }
    }
}

impl fmt::Debug for ClientTransportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientTransportConfig")
            .field("bind_address", &self.bind_address)
            .field("ca_certificate_pem", &"[PEM REDACTED]")
            .field("alpn", &String::from_utf8_lossy(&self.alpn))
            .finish()
    }
}

#[async_trait]
pub trait TransportFactory: Send + Sync + 'static {
    fn backend(&self) -> TransportBackend;

    async fn server_endpoint(
        &self,
        config: ServerTransportConfig,
    ) -> Result<Arc<dyn TransportEndpoint>, TransportError>;

    async fn client_endpoint(
        &self,
        config: ClientTransportConfig,
    ) -> Result<Arc<dyn TransportEndpoint>, TransportError>;
}

#[derive(Clone, Copy, Debug)]
pub struct ConfiguredTransportFactory {
    backend: TransportBackend,
}

impl ConfiguredTransportFactory {
    pub const fn new(backend: TransportBackend) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl TransportFactory for ConfiguredTransportFactory {
    fn backend(&self) -> TransportBackend {
        self.backend
    }

    async fn server_endpoint(
        &self,
        config: ServerTransportConfig,
    ) -> Result<Arc<dyn TransportEndpoint>, TransportError> {
        make_server_endpoint(self.backend, config)
    }

    async fn client_endpoint(
        &self,
        config: ClientTransportConfig,
    ) -> Result<Arc<dyn TransportEndpoint>, TransportError> {
        make_client_endpoint(self.backend, config)
    }
}

pub fn make_server_endpoint(
    backend: TransportBackend,
    config: ServerTransportConfig,
) -> Result<Arc<dyn TransportEndpoint>, TransportError> {
    validate_alpn(&config.alpn)?;
    validate_server_name(&config.server_name)?;
    validate_bind_address(config.bind_address, false)?;
    match backend {
        TransportBackend::Quinn => {
            #[cfg(feature = "backend-quinn")]
            {
                let endpoint = crate::quic::quin::QuinnEndpoint::make_server_endpoint_at(
                    config.bind_address,
                    &config.certificate_pem,
                    &config.private_key_pem,
                )?;
                Ok(Arc::new(LegacyEndpoint::new(endpoint)))
            }
            #[cfg(not(feature = "backend-quinn"))]
            {
                Err(TransportError::BackendUnavailable("quinn"))
            }
        }
        TransportBackend::S2n => {
            #[cfg(feature = "backend-s2n")]
            {
                let endpoint = crate::quic::s2n::S2nEndpoint::make_server_endpoint_at(
                    config.bind_address,
                    &config.certificate_pem,
                    &config.private_key_pem,
                )?;
                Ok(Arc::new(LegacyEndpoint::new(endpoint)))
            }
            #[cfg(not(feature = "backend-s2n"))]
            {
                Err(TransportError::BackendUnavailable("s2n"))
            }
        }
        TransportBackend::GmQuic => {
            #[cfg(feature = "backend-gm-quic")]
            {
                let endpoint =
                    crate::quic::gm_quic::GmQuicEndpoint::make_server_endpoint_at_with_name(
                        config.bind_address,
                        &config.server_name,
                        &config.certificate_pem,
                        &config.private_key_pem,
                    )?;
                Ok(Arc::new(LegacyEndpoint::new(endpoint)))
            }
            #[cfg(not(feature = "backend-gm-quic"))]
            {
                Err(TransportError::BackendUnavailable("gm-quic"))
            }
        }
    }
}

pub fn make_client_endpoint(
    backend: TransportBackend,
    config: ClientTransportConfig,
) -> Result<Arc<dyn TransportEndpoint>, TransportError> {
    validate_alpn(&config.alpn)?;
    validate_bind_address(config.bind_address, true)?;
    match backend {
        TransportBackend::Quinn => {
            #[cfg(feature = "backend-quinn")]
            {
                let endpoint = crate::quic::quin::QuinnEndpoint::make_client_endpoint_at(
                    config.bind_address,
                    &config.ca_certificate_pem,
                )?;
                Ok(Arc::new(LegacyEndpoint::new(endpoint)))
            }
            #[cfg(not(feature = "backend-quinn"))]
            {
                Err(TransportError::BackendUnavailable("quinn"))
            }
        }
        TransportBackend::S2n => {
            #[cfg(feature = "backend-s2n")]
            {
                let endpoint = crate::quic::s2n::S2nEndpoint::make_client_endpoint_at(
                    config.bind_address,
                    &config.ca_certificate_pem,
                )?;
                Ok(Arc::new(LegacyEndpoint::new(endpoint)))
            }
            #[cfg(not(feature = "backend-s2n"))]
            {
                Err(TransportError::BackendUnavailable("s2n"))
            }
        }
        TransportBackend::GmQuic => {
            #[cfg(feature = "backend-gm-quic")]
            {
                let endpoint = crate::quic::gm_quic::GmQuicEndpoint::make_client_endpoint_at(
                    config.bind_address,
                    &config.ca_certificate_pem,
                )?;
                Ok(Arc::new(LegacyEndpoint::new(endpoint)))
            }
            #[cfg(not(feature = "backend-gm-quic"))]
            {
                Err(TransportError::BackendUnavailable("gm-quic"))
            }
        }
    }
}

fn validate_alpn(alpn: &[u8]) -> Result<(), TransportError> {
    if alpn != ALPN {
        return Err(TransportError::InvalidConfiguration(
            "protocol v1 requires ALPN fusen-net/1".to_owned(),
        ));
    }
    Ok(())
}

fn validate_bind_address(address: SocketAddr, allow_zero_port: bool) -> Result<(), TransportError> {
    if (!allow_zero_port && address.port() == 0)
        || address.ip().is_multicast()
        || matches!(address.ip(), IpAddr::V4(ip) if ip.is_broadcast())
    {
        return Err(TransportError::InvalidConfiguration(format!(
            "invalid QUIC bind address {address}"
        )));
    }
    Ok(())
}

fn validate_server_name(server_name: &str) -> Result<(), TransportError> {
    let valid_name = match server_name.parse::<IpAddr>() {
        Ok(ip) => {
            !ip.is_unspecified()
                && !ip.is_multicast()
                && !matches!(ip, IpAddr::V4(address) if address.is_broadcast())
        }
        Err(_) => server_name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        }),
    };
    let valid =
        !server_name.is_empty() && server_name.len() <= 253 && server_name.is_ascii() && valid_name;
    if !valid {
        return Err(TransportError::InvalidConfiguration(
            "TLS server name must be an ASCII DNS name or IP address".to_owned(),
        ));
    }
    Ok(())
}

/// Adapts the original generic QUIC traits to the object-safe runtime traits.
/// This preserves source compatibility while allowing heterogeneous listener
/// implementations to share one relay route table.
pub struct LegacyEndpoint<E> {
    endpoint: E,
}

impl<E> LegacyEndpoint<E> {
    pub const fn new(endpoint: E) -> Self {
        Self { endpoint }
    }
}

#[async_trait]
impl<E> TransportEndpoint for LegacyEndpoint<E>
where
    E: crate::quic::Endpoint + Send + Sync,
{
    async fn accept(&self) -> Result<Box<dyn TransportConnection>, TransportError> {
        let connection = self.endpoint.accept().await?;
        Ok(Box::new(LegacyConnection::new(connection)))
    }

    async fn connect(
        &self,
        address: SocketAddr,
        server_name: &str,
    ) -> Result<Box<dyn TransportConnection>, TransportError> {
        let connection = self
            .endpoint
            .connect(address, server_name.to_owned())
            .await?;
        Ok(Box::new(LegacyConnection::new(connection)))
    }
}

pub struct LegacyConnection<C> {
    connection: C,
}

impl<C> LegacyConnection<C> {
    pub const fn new(connection: C) -> Self {
        Self { connection }
    }
}

#[async_trait]
impl<C> TransportConnection for LegacyConnection<C>
where
    C: crate::quic::Connection,
{
    async fn open_bi(&self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
        let (reader, writer) = self.connection.open_bi().await?;
        Ok((Box::pin(reader), Box::pin(writer)))
    }

    async fn accept_bi(&mut self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
        let (reader, writer) = self.connection.accept_bi().await?;
        Ok((Box::pin(reader), Box::pin(writer)))
    }

    fn send_datagram(&self, packet: Bytes) -> Result<(), TransportError> {
        self.connection
            .send_datagram(packet)
            .map_err(|error| match error {
                FusenNetError::DatagramQueueFull => TransportError::DatagramQueueFull,
                FusenNetError::DatagramTooLarge => TransportError::DatagramTooLarge,
                error => TransportError::Legacy(error),
            })
    }

    async fn recv_datagram(&mut self) -> Result<Bytes, TransportError> {
        Ok(self.connection.recv_datagram().await?)
    }

    fn dropped_incoming_datagrams(&self) -> Result<u64, TransportError> {
        self.connection
            .dropped_incoming_datagrams()
            .map_err(TransportError::from)
    }

    fn remote_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    async fn closed(&self) -> TransportError {
        self.connection.closed().await.into()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport endpoint is closed")]
    EndpointClosed,
    #[error("transport connection is closed")]
    ConnectionClosed,
    #[error("transport operation is not supported for this endpoint role")]
    UnsupportedRole,
    #[error("datagram send queue is full")]
    DatagramQueueFull,
    #[error("datagram exceeds the peer transport limit")]
    DatagramTooLarge,
    #[error("QUIC backend '{0}' is not enabled")]
    BackendUnavailable(&'static str),
    #[error("invalid transport configuration: {0}")]
    InvalidConfiguration(String),
    #[error("transport failed: {0}")]
    Legacy(#[from] FusenNetError),
    #[error("transport failed: {0}")]
    Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_server_name_rejects_non_identity_addresses_and_invalid_dns() {
        assert!(validate_server_name("relay.example").is_ok());
        assert!(validate_server_name("127.0.0.1").is_ok());
        for invalid in [
            "",
            "bad/name",
            "bad..name",
            "-relay.example",
            "relay-.example",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::",
        ] {
            assert!(
                validate_server_name(invalid).is_err(),
                "accepted invalid TLS server name {invalid:?}"
            );
        }
    }
}
