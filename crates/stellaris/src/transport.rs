// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Version 2 transport boundaries.

use std::{
    fmt,
    net::SocketAddr,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::Arc,
};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(feature = "backend-quinn")]
use crate::error::StellarisError;
use crate::protocol::{CONTROL_ALPN, ENROLLMENT_ALPN, P2P_ALPN, RELAY_ALPN};

pub type BoxReadStream = Pin<Box<dyn AsyncRead + Send + Unpin + 'static>>;
pub type BoxWriteStream = Pin<Box<dyn AsyncWrite + Send + Unpin + 'static>>;

pub(crate) mod sealed {
    pub trait Sealed {}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportBackend {
    Quinn,
    S2n,
    GmQuic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolPurpose {
    Enrollment,
    Control,
    Relay,
    P2p,
}

impl ProtocolPurpose {
    pub const fn alpn(self) -> &'static [u8] {
        match self {
            Self::Enrollment => ENROLLMENT_ALPN,
            Self::Control => CONTROL_ALPN,
            Self::Relay => RELAY_ALPN,
            Self::P2p => P2P_ALPN,
        }
    }

    pub const fn requires_node_identity(self) -> bool {
        !matches!(self, Self::Enrollment)
    }
}

#[async_trait]
pub trait TransportConnection: sealed::Sealed + Send + Sync + 'static {
    async fn open_bi(&self) -> Result<(BoxReadStream, BoxWriteStream), TransportError>;
    async fn accept_bi(&mut self) -> Result<(BoxReadStream, BoxWriteStream), TransportError>;
    fn send_datagram(&self, packet: Bytes) -> Result<(), TransportError>;
    async fn recv_datagram(&mut self) -> Result<Bytes, TransportError>;
    fn dropped_incoming_datagrams(&self) -> Result<u64, TransportError> {
        Ok(0)
    }
    fn remote_address(&self) -> SocketAddr;
    async fn negotiated_alpn(&self) -> Result<Vec<u8>, TransportError>;
    async fn peer_certificate_chain_der(&self) -> Result<Vec<Vec<u8>>, TransportError>;
    fn close(&self, code: u32, reason: &[u8]);
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
    async fn local_address(&self) -> Result<SocketAddr, TransportError>;
}

/// Connection whose TLS metadata has been checked against one fixed v2 role.
pub struct AuthenticatedConnection {
    purpose: ProtocolPurpose,
    certificate_chain_der: Vec<Vec<u8>>,
    inner: Box<dyn TransportConnection>,
}

pub(crate) struct AuthenticatedConnectionMetadata {
    purpose: ProtocolPurpose,
    certificate_chain_der: Vec<Vec<u8>>,
}

impl AuthenticatedConnection {
    pub fn purpose(&self) -> ProtocolPurpose {
        self.purpose
    }

    pub fn certificate_chain_der(&self) -> &[Vec<u8>] {
        &self.certificate_chain_der
    }

    pub fn connection(&self) -> &dyn TransportConnection {
        self.inner.as_ref()
    }

    pub fn connection_mut(&mut self) -> &mut dyn TransportConnection {
        self.inner.as_mut()
    }

    pub fn into_inner(self) -> Box<dyn TransportConnection> {
        self.inner
    }

    pub(crate) fn from_metadata(
        inner: Box<dyn TransportConnection>,
        metadata: AuthenticatedConnectionMetadata,
    ) -> Self {
        Self {
            purpose: metadata.purpose,
            certificate_chain_der: metadata.certificate_chain_der,
            inner,
        }
    }
}

impl Deref for AuthenticatedConnection {
    type Target = dyn TransportConnection;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}

impl DerefMut for AuthenticatedConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut()
    }
}

impl AsRef<dyn TransportConnection> for AuthenticatedConnection {
    fn as_ref(&self) -> &(dyn TransportConnection + 'static) {
        self.inner.as_ref()
    }
}

pub async fn authenticate_connection(
    connection: Box<dyn TransportConnection>,
    purpose: ProtocolPurpose,
) -> Result<AuthenticatedConnection, TransportError> {
    let metadata = authenticate_connection_metadata(connection.as_ref(), purpose).await?;
    Ok(AuthenticatedConnection::from_metadata(connection, metadata))
}

pub(crate) async fn authenticate_connection_metadata(
    connection: &dyn TransportConnection,
    purpose: ProtocolPurpose,
) -> Result<AuthenticatedConnectionMetadata, TransportError> {
    if !purpose.requires_node_identity() {
        return Err(TransportError::InvalidConfiguration(
            "enrollment connections do not carry a node identity".to_owned(),
        ));
    }
    let negotiated = connection.negotiated_alpn().await?;
    if negotiated.as_slice() != purpose.alpn() {
        return Err(TransportError::AlpnMismatch);
    }
    let certificate_chain_der = connection.peer_certificate_chain_der().await?;
    if certificate_chain_der.is_empty() || certificate_chain_der[0].is_empty() {
        return Err(TransportError::MissingPeerCertificate);
    }
    Ok(AuthenticatedConnectionMetadata {
        purpose,
        certificate_chain_der,
    })
}

#[derive(Clone)]
pub struct ServerTransportConfig {
    pub bind_address: SocketAddr,
    pub server_name: String,
    pub certificate_pem: String,
    pub private_key_pem: String,
    pub purpose: ProtocolPurpose,
    pub client_ca_certificate_pem: Option<String>,
}

impl ServerTransportConfig {
    pub fn new(
        bind_address: SocketAddr,
        server_name: impl Into<String>,
        certificate_pem: impl Into<String>,
        private_key_pem: impl Into<String>,
        purpose: ProtocolPurpose,
    ) -> Self {
        Self {
            bind_address,
            server_name: server_name.into(),
            certificate_pem: certificate_pem.into(),
            private_key_pem: private_key_pem.into(),
            purpose,
            client_ca_certificate_pem: None,
        }
    }

    pub fn with_client_ca_certificate(mut self, certificate_pem: impl Into<String>) -> Self {
        self.client_ca_certificate_pem = Some(certificate_pem.into());
        self
    }
}

impl fmt::Debug for ServerTransportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerTransportConfig")
            .field("bind_address", &self.bind_address)
            .field("server_name", &self.server_name)
            .field("purpose", &self.purpose)
            .field("certificate_pem", &"[PEM REDACTED]")
            .field("private_key_pem", &"[REDACTED]")
            .field(
                "client_ca_certificate_pem",
                &self
                    .client_ca_certificate_pem
                    .as_ref()
                    .map(|_| "[PEM REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone)]
pub struct ClientCertificateIdentity {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

impl ClientCertificateIdentity {
    pub fn new(certificate_pem: impl Into<String>, private_key_pem: impl Into<String>) -> Self {
        Self {
            certificate_pem: certificate_pem.into(),
            private_key_pem: private_key_pem.into(),
        }
    }
}

impl fmt::Debug for ClientCertificateIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientCertificateIdentity")
            .field("certificate_pem", &"[PEM REDACTED]")
            .field("private_key_pem", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone)]
pub struct ClientTransportConfig {
    pub bind_address: SocketAddr,
    pub ca_certificate_pem: String,
    pub purpose: ProtocolPurpose,
    pub client_identity: Option<ClientCertificateIdentity>,
}

impl ClientTransportConfig {
    pub fn new(
        bind_address: SocketAddr,
        ca_certificate_pem: impl Into<String>,
        purpose: ProtocolPurpose,
    ) -> Self {
        Self {
            bind_address,
            ca_certificate_pem: ca_certificate_pem.into(),
            purpose,
            client_identity: None,
        }
    }

    pub fn with_client_identity(
        mut self,
        certificate_pem: impl Into<String>,
        private_key_pem: impl Into<String>,
    ) -> Self {
        self.client_identity = Some(ClientCertificateIdentity::new(
            certificate_pem,
            private_key_pem,
        ));
        self
    }
}

impl fmt::Debug for ClientTransportConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientTransportConfig")
            .field("bind_address", &self.bind_address)
            .field("purpose", &self.purpose)
            .field("ca_certificate_pem", &"[PEM REDACTED]")
            .field("client_identity", &self.client_identity)
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
    validate_server_config(&config)?;
    if backend != TransportBackend::Quinn {
        return Err(TransportError::UnsupportedRuntimeBackend(backend));
    }
    #[cfg(feature = "backend-quinn")]
    {
        let endpoint = crate::quic::quin::QuinnEndpoint::make_server_endpoint_at_with_alpn(
            config.bind_address,
            &config.certificate_pem,
            &config.private_key_pem,
            config.purpose.alpn(),
            config.client_ca_certificate_pem.as_deref(),
        )?;
        Ok(Arc::new(endpoint))
    }
    #[cfg(not(feature = "backend-quinn"))]
    {
        Err(TransportError::BackendUnavailable("quinn"))
    }
}

pub fn make_client_endpoint(
    backend: TransportBackend,
    config: ClientTransportConfig,
) -> Result<Arc<dyn TransportEndpoint>, TransportError> {
    validate_client_config(&config)?;
    if backend != TransportBackend::Quinn {
        return Err(TransportError::UnsupportedRuntimeBackend(backend));
    }
    #[cfg(feature = "backend-quinn")]
    {
        let endpoint = crate::quic::quin::QuinnEndpoint::make_client_endpoint_at_with_alpn(
            config.bind_address,
            &config.ca_certificate_pem,
            config.purpose.alpn(),
            config.client_identity.as_ref().map(|identity| {
                (
                    identity.certificate_pem.as_str(),
                    identity.private_key_pem.as_str(),
                )
            }),
        )?;
        Ok(Arc::new(endpoint))
    }
    #[cfg(not(feature = "backend-quinn"))]
    {
        Err(TransportError::BackendUnavailable("quinn"))
    }
}

#[cfg(feature = "backend-quinn")]
pub struct HybridQuinnEndpoint {
    inner: crate::quic::quin::HybridQuinnEndpoint,
}

#[cfg(feature = "backend-quinn")]
impl HybridQuinnEndpoint {
    pub fn bind(
        bind_address: SocketAddr,
        certificate_pem: &str,
        private_key_pem: &str,
        trusted_peer_ca_pem: &str,
    ) -> Result<Self, TransportError> {
        validate_bind_address(bind_address, true)?;
        Ok(Self {
            inner: crate::quic::quin::HybridQuinnEndpoint::bind(
                bind_address,
                certificate_pem,
                private_key_pem,
                trusted_peer_ca_pem,
                P2P_ALPN,
            )?,
        })
    }

    pub async fn rotate_identity(
        &self,
        certificate_pem: &str,
        private_key_pem: &str,
    ) -> Result<(), TransportError> {
        self.inner
            .rotate_identity(certificate_pem, private_key_pem)
            .await?;
        Ok(())
    }

    pub async fn close(&self) {
        self.inner.close().await;
    }
}

fn validate_server_config(config: &ServerTransportConfig) -> Result<(), TransportError> {
    validate_bind_address(config.bind_address, false)?;
    validate_server_name(&config.server_name)?;
    if config.purpose.requires_node_identity() && config.client_ca_certificate_pem.is_none() {
        return Err(TransportError::InvalidConfiguration(
            "control, relay, and P2P listeners require the node CA".to_owned(),
        ));
    }
    if !config.purpose.requires_node_identity() && config.client_ca_certificate_pem.is_some() {
        return Err(TransportError::InvalidConfiguration(
            "the enrollment listener uses server-only TLS".to_owned(),
        ));
    }
    Ok(())
}

fn validate_client_config(config: &ClientTransportConfig) -> Result<(), TransportError> {
    validate_bind_address(config.bind_address, true)?;
    if config.purpose.requires_node_identity() != config.client_identity.is_some() {
        return Err(TransportError::InvalidConfiguration(
            "client certificate presence does not match the protocol role".to_owned(),
        ));
    }
    Ok(())
}

fn validate_bind_address(address: SocketAddr, allow_zero_port: bool) -> Result<(), TransportError> {
    if (!allow_zero_port && address.port() == 0)
        || address.ip().is_multicast()
        || matches!(address.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast())
    {
        return Err(TransportError::InvalidConfiguration(format!(
            "invalid QUIC bind address {address}"
        )));
    }
    Ok(())
}

fn validate_server_name(server_name: &str) -> Result<(), TransportError> {
    if server_name.is_empty() || server_name.len() > 253 || !server_name.is_ascii() {
        return Err(TransportError::InvalidConfiguration(
            "TLS server name must be a non-empty ASCII name".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(feature = "backend-quinn")]
#[async_trait]
impl TransportEndpoint for crate::quic::quin::QuinnEndpoint {
    async fn accept(&self) -> Result<Box<dyn TransportConnection>, TransportError> {
        Ok(Box::new(self.accept_connection().await?))
    }

    async fn connect(
        &self,
        address: SocketAddr,
        server_name: &str,
    ) -> Result<Box<dyn TransportConnection>, TransportError> {
        Ok(Box::new(
            self.connect_connection(address, server_name).await?,
        ))
    }

    async fn local_address(&self) -> Result<SocketAddr, TransportError> {
        self.local_address().map_err(TransportError::from)
    }
}

#[cfg(feature = "backend-quinn")]
#[async_trait]
impl TransportEndpoint for HybridQuinnEndpoint {
    async fn accept(&self) -> Result<Box<dyn TransportConnection>, TransportError> {
        Ok(Box::new(self.inner.accept_connection().await?))
    }

    async fn connect(
        &self,
        address: SocketAddr,
        server_name: &str,
    ) -> Result<Box<dyn TransportConnection>, TransportError> {
        Ok(Box::new(
            self.inner.connect_connection(address, server_name).await?,
        ))
    }

    async fn local_address(&self) -> Result<SocketAddr, TransportError> {
        Ok(self.inner.local_address().await?)
    }
}

#[cfg(feature = "backend-quinn")]
impl sealed::Sealed for crate::quic::quin::QuinnConnect {}

#[cfg(feature = "backend-quinn")]
#[async_trait]
impl TransportConnection for crate::quic::quin::QuinnConnect {
    async fn open_bi(&self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
        let (reader, writer) = crate::quic::Connection::open_bi(self).await?;
        Ok((Box::pin(reader), Box::pin(writer)))
    }

    async fn accept_bi(&mut self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
        let (reader, writer) = crate::quic::Connection::accept_bi(self).await?;
        Ok((Box::pin(reader), Box::pin(writer)))
    }

    fn send_datagram(&self, packet: Bytes) -> Result<(), TransportError> {
        crate::quic::Connection::send_datagram(self, packet).map_err(map_send_error)
    }

    async fn recv_datagram(&mut self) -> Result<Bytes, TransportError> {
        Ok(crate::quic::Connection::recv_datagram(self).await?)
    }

    fn dropped_incoming_datagrams(&self) -> Result<u64, TransportError> {
        crate::quic::Connection::dropped_incoming_datagrams(self).map_err(TransportError::from)
    }

    fn remote_address(&self) -> SocketAddr {
        crate::quic::Connection::remote_address(self)
    }

    async fn negotiated_alpn(&self) -> Result<Vec<u8>, TransportError> {
        crate::quic::quin::QuinnConnect::negotiated_alpn(self)
            .await?
            .ok_or(TransportError::MissingHandshakeMetadata("ALPN"))
    }

    async fn peer_certificate_chain_der(&self) -> Result<Vec<Vec<u8>>, TransportError> {
        crate::quic::quin::QuinnConnect::peer_certificate_chain_der(self)
            .await?
            .ok_or(TransportError::MissingPeerCertificate)
    }

    fn close(&self, code: u32, reason: &[u8]) {
        crate::quic::quin::QuinnConnect::close(self, code, reason);
    }

    async fn closed(&self) -> TransportError {
        crate::quic::Connection::closed(self).await.into()
    }
}

#[cfg(feature = "backend-quinn")]
fn map_send_error(error: StellarisError) -> TransportError {
    match error {
        StellarisError::DatagramQueueFull => TransportError::DatagramQueueFull,
        StellarisError::DatagramTooLarge => TransportError::DatagramTooLarge,
        error => TransportError::Quinn(error),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport endpoint is closed")]
    EndpointClosed,
    #[error("transport connection is closed")]
    ConnectionClosed,
    #[error("datagram send queue is full")]
    DatagramQueueFull,
    #[error("datagram exceeds the peer transport limit")]
    DatagramTooLarge,
    #[error("QUIC backend '{0}' is not enabled")]
    BackendUnavailable(&'static str),
    #[error("QUIC backend {0:?} has no Stellaris v2 implementation")]
    UnsupportedRuntimeBackend(TransportBackend),
    #[error("invalid transport configuration: {0}")]
    InvalidConfiguration(String),
    #[error("TLS did not negotiate the expected ALPN")]
    AlpnMismatch,
    #[error("TLS peer certificate is missing")]
    MissingPeerCertificate,
    #[error("TLS handshake metadata is missing: {0}")]
    MissingHandshakeMetadata(&'static str),
    #[cfg(feature = "backend-quinn")]
    #[error("Quinn transport failed: {0}")]
    Quinn(#[from] StellarisError),
    #[error("transport failed: {0}")]
    Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_roles_have_distinct_alpns() {
        let values = [
            ProtocolPurpose::Enrollment.alpn(),
            ProtocolPurpose::Control.alpn(),
            ProtocolPurpose::Relay.alpn(),
            ProtocolPurpose::P2p.alpn(),
        ];
        for (index, value) in values.iter().enumerate() {
            assert!(
                values
                    .iter()
                    .enumerate()
                    .all(|(other, candidate)| index == other || value != candidate)
            );
        }
    }

    #[test]
    fn only_enrollment_allows_server_only_tls() {
        let bind = SocketAddr::from(([127, 0, 0, 1], 7000));
        let enrollment = ServerTransportConfig::new(
            bind,
            "localhost",
            "cert",
            "key",
            ProtocolPurpose::Enrollment,
        );
        assert!(validate_server_config(&enrollment).is_ok());
        let control =
            ServerTransportConfig::new(bind, "localhost", "cert", "key", ProtocolPurpose::Control);
        assert!(validate_server_config(&control).is_err());
    }

    #[test]
    fn non_quinn_backends_are_not_v2_runtime_backends() {
        let config = ClientTransportConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            "CA",
            ProtocolPurpose::Enrollment,
        );
        assert!(matches!(
            make_client_endpoint(TransportBackend::S2n, config),
            Err(TransportError::UnsupportedRuntimeBackend(
                TransportBackend::S2n
            ))
        ));
    }
}
