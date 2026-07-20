// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{Connection, Endpoint, StreamStop};
use crate::common::{self};
use crate::control::ALPN;
use crate::error::FusenNetError;
use bytes::Bytes;
use futures::future::BoxFuture;
use gm_quic::prelude::{
    BindUri, BuildListenersError, CancelStream, Connection as GmConnect, DatagramReader,
    DatagramWriter, EndpointAddr, ParameterId, ParseBindUriError, QuicClient, QuicListeners,
    ServerError, SocketEndpointAddr, StopSending, StreamReader, StreamWriter, handy,
};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::io;
use std::str::FromStr;
use std::{net::SocketAddr, sync::Arc, time::Duration};

const MAX_DATAGRAM_FRAME_SIZE: u32 = u16::MAX as u32;
const KEEPALIVE_LIFETIME: Duration = Duration::MAX;

fn ensure_crypto_provider() {
    // With all backends enabled, Quinn also enables rustls' ring feature and
    // rustls can no longer infer a process default for gm-quic's builders.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

pub struct CertifiedKeyV2 {
    priv_key: PrivateKeyDer<'static>,
    cert_chain: Vec<CertificateDer<'static>>,
}

pub fn generate_signed(priv_key: &str, cert: &str) -> Result<CertifiedKeyV2, FusenNetError> {
    Ok(CertifiedKeyV2 {
        priv_key: PrivateKeyDer::from_pem_slice(priv_key.as_bytes())
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))?,
        cert_chain: parse_certificates(cert)?,
    })
}

fn parse_certificates(pem: &str) -> Result<Vec<CertificateDer<'static>>, FusenNetError> {
    let certificates = CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    if certificates.is_empty() {
        return Err(FusenNetError::GmQuicConnectError(io::Error::new(
            io::ErrorKind::InvalidData,
            "certificate PEM does not contain a certificate",
        )));
    }
    Ok(certificates)
}

pub struct GmQuicConnect {
    connect: GmConnect,
    datagram_writer: DatagramWriter,
    datagram_reader: DatagramReader,
    remote_address: SocketAddr,
}

impl StreamStop for StreamReader {
    fn steam_stop(&mut self) {
        self.stop(0x100);
    }
}

impl StreamStop for StreamWriter {
    fn steam_stop(&mut self) {
        self.cancel(0x100);
    }
}

impl common::ReadStream for StreamReader {}
impl common::WriteStream for StreamWriter {}

impl Connection for GmQuicConnect {
    fn open_bi(
        &self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>
    {
        Box::pin(async move {
            let stream = match self.connect.open_bi_stream().await {
                Ok(stream) => stream,
                Err(error) => {
                    return Err(FusenNetError::GmQuicConnectError(io::Error::other(
                        error.to_string(),
                    )));
                }
            };
            let Some((_stream_id, (recv_stream, send_stream))) = stream else {
                return Err(FusenNetError::GmQuicConnectError(io::Error::other(
                    "open_bi error !",
                )));
            };
            Ok((recv_stream, send_stream))
        })
    }

    fn accept_bi(
        &mut self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>
    {
        Box::pin(async move {
            let (_stream_id, (recv_stream, send_stream)) =
                match self.connect.accept_bi_stream().await {
                    Ok(stream) => stream,
                    Err(error) => {
                        return Err(FusenNetError::GmQuicConnectError(io::Error::other(
                            error.to_string(),
                        )));
                    }
                };

            Ok((recv_stream, send_stream))
        })
    }

    fn send_datagram(&self, bytes: bytes::Bytes) -> Result<(), FusenNetError> {
        self.datagram_writer
            .send_bytes(bytes)
            .map_err(map_datagram_send_error)
    }

    fn recv_datagram(&mut self) -> BoxFuture<'_, Result<Bytes, FusenNetError>> {
        Box::pin(async move {
            self.datagram_reader
                .recv()
                .await
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))
        })
    }

    fn dropped_incoming_datagrams(&self) -> Result<u64, FusenNetError> {
        self.datagram_reader
            .dropped_queue_full()
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))
    }

    fn remote_address(&self) -> SocketAddr {
        self.remote_address
    }

    fn closed(&self) -> BoxFuture<'_, FusenNetError> {
        Box::pin(async move {
            let error = self.connect.terminated().await;
            FusenNetError::GmQuicConnectError(io::Error::other(error.to_string()))
        })
    }
}

fn map_datagram_send_error(error: io::Error) -> FusenNetError {
    match error.kind() {
        io::ErrorKind::WouldBlock => FusenNetError::DatagramQueueFull,
        io::ErrorKind::InvalidInput => FusenNetError::DatagramTooLarge,
        _ => FusenNetError::GmQuicConnectError(error),
    }
}

pub struct GmQuicEndpoint {
    pub server: Option<Arc<QuicListeners>>,
    pub client: Option<Arc<QuicClient>>,
}

impl From<BuildListenersError> for FusenNetError {
    fn from(value: BuildListenersError) -> Self {
        FusenNetError::BoxError(Box::new(value))
    }
}

impl From<ParseBindUriError> for FusenNetError {
    fn from(value: ParseBindUriError) -> Self {
        FusenNetError::BoxError(Box::new(value))
    }
}

impl From<ServerError> for FusenNetError {
    fn from(value: ServerError) -> Self {
        FusenNetError::BoxError(Box::new(value))
    }
}

impl GmQuicEndpoint {
    pub fn make_server_endpoint(
        bind_port: u16,
        cert: &str,
        prik: &str,
    ) -> Result<impl Endpoint, FusenNetError> {
        Self::make_server_endpoint_at(SocketAddr::from(([0, 0, 0, 0], bind_port)), cert, prik)
    }

    pub fn make_server_endpoint_at(
        bind_address: SocketAddr,
        cert: &str,
        prik: &str,
    ) -> Result<impl Endpoint, FusenNetError> {
        Self::make_server_endpoint_at_with_name(bind_address, "localhost", cert, prik)
    }

    pub fn make_server_endpoint_at_with_name(
        bind_address: SocketAddr,
        server_name: &str,
        cert: &str,
        prik: &str,
    ) -> Result<impl Endpoint, FusenNetError> {
        ensure_crypto_provider();
        let certifie_key = generate_signed(prik, cert)?;
        let mut parameters = handy::server_parameters();
        for (id, value) in [
            (ParameterId::InitialMaxStreamsBidi, 1_u32),
            (ParameterId::InitialMaxStreamsUni, 0_u32),
            (ParameterId::MaxDatagramFrameSize, MAX_DATAGRAM_FRAME_SIZE),
        ] {
            parameters
                .set(id, value)
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
        }
        let endpoint = QuicListeners::builder()?
            .without_client_cert_verifier()
            .with_parameters(parameters)
            .defer_idle_timeout(KEEPALIVE_LIFETIME)
            .with_alpns([ALPN])
            .listen(4096);
        let bind_uris = vec![BindUri::from_str(&bind_address.to_string())?];
        endpoint.add_server(
            server_name,
            certifie_key.cert_chain,
            certifie_key.priv_key,
            bind_uris,
            None,
        )?;
        Ok(GmQuicEndpoint {
            server: Some(endpoint),
            client: None,
        })
    }

    pub fn make_client_endpoint(cert: &str) -> Result<impl Endpoint + 'static, FusenNetError> {
        Self::make_client_endpoint_at(SocketAddr::from(([0, 0, 0, 0], 0)), cert)
    }

    pub fn make_client_endpoint_at(
        bind_address: SocketAddr,
        cert: &str,
    ) -> Result<impl Endpoint + 'static, FusenNetError> {
        ensure_crypto_provider();
        let mut roots = RootCertStore::empty();
        for cert in parse_certificates(cert)? {
            roots
                .add(cert)
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
        }
        let mut parameters = handy::client_parameters();
        for (id, value) in [
            (ParameterId::InitialMaxStreamsBidi, 1_u32),
            (ParameterId::InitialMaxStreamsUni, 0_u32),
            (ParameterId::MaxDatagramFrameSize, MAX_DATAGRAM_FRAME_SIZE),
        ] {
            parameters
                .set(id, value)
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
        }
        let client = QuicClient::builder()
            .bind([BindUri::from_str(&bind_address.to_string())?])
            .defer_idle_timeout(KEEPALIVE_LIFETIME)
            .with_root_certificates(roots)
            .without_cert()
            .with_alpns([ALPN])
            .with_parameters(parameters)
            .build();
        Ok(GmQuicEndpoint {
            server: None,
            client: Some(Arc::new(client)),
        })
    }
}

impl Endpoint for GmQuicEndpoint {
    fn accept(&self) -> BoxFuture<'_, Result<impl Connection, FusenNetError>> {
        let server = self.server.clone();
        Box::pin(async move {
            let Some(server) = server else {
                return Err(FusenNetError::EndpointClose);
            };
            let (connect, _, addr, _) = server.accept().await.map_err(|error| {
                FusenNetError::GmQuicConnectError(io::Error::other(error.to_string()))
            })?;
            #[allow(deprecated)]
            let w = connect
                .unreliable_writer()
                .await
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?
                .map_err(FusenNetError::GmQuicConnectError)?;
            #[allow(deprecated)]
            let r = connect
                .unreliable_reader()
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?
                .map_err(FusenNetError::GmQuicConnectError)?;
            let remote_address = endpoint_addr_to_socket(&addr.remote()).ok_or_else(|| {
                FusenNetError::GmQuicConnectError(io::Error::other(
                    "remote endpoint is not an IP socket",
                ))
            })?;
            Ok(GmQuicConnect {
                connect,
                datagram_writer: w,
                datagram_reader: r,
                remote_address,
            })
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<'_, Result<impl Connection, FusenNetError>> {
        let client = self.client.clone();
        Box::pin(async move {
            let Some(client) = client else {
                return Err(FusenNetError::EndpointClose);
            };
            let remote_address = addr;
            let addr = EndpointAddr::Socket(SocketEndpointAddr::Direct { addr });
            let connect = client
                .connected_to(&server_name, vec![addr])
                .map_err(|error| {
                    FusenNetError::GmQuicConnectError(io::Error::other(error.to_string()))
                })?;
            #[allow(deprecated)]
            let w = connect
                .unreliable_writer()
                .await
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?
                .map_err(FusenNetError::GmQuicConnectError)?;
            #[allow(deprecated)]
            let r = connect
                .unreliable_reader()
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?
                .map_err(FusenNetError::GmQuicConnectError)?;
            Ok(GmQuicConnect {
                connect,
                datagram_writer: w,
                datagram_reader: r,
                remote_address,
            })
        })
    }
}

fn endpoint_addr_to_socket(address: &EndpointAddr) -> Option<SocketAddr> {
    match address {
        EndpointAddr::Socket(SocketEndpointAddr::Direct { addr }) => Some(*addr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagram_send_errors_map_to_transport_contract() {
        assert!(matches!(
            map_datagram_send_error(io::Error::from(io::ErrorKind::WouldBlock)),
            FusenNetError::DatagramQueueFull
        ));
        assert!(matches!(
            map_datagram_send_error(io::Error::from(io::ErrorKind::InvalidInput)),
            FusenNetError::DatagramTooLarge
        ));
        assert!(matches!(
            map_datagram_send_error(io::Error::from(io::ErrorKind::BrokenPipe)),
            FusenNetError::GmQuicConnectError(_)
        ));
    }
}
