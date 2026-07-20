// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{Connection, Endpoint, StreamStop};
use crate::{common, control::ALPN, error::FusenNetError};
use bytes::Bytes;
use futures::future::BoxFuture;
use quinn::{
    ClientConfig, Endpoint as QuicEndpoint, RecvStream, SendDatagramError, SendStream,
    ServerConfig, VarInt,
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::{Mutex, OnceCell};

const DATAGRAM_BUFFER_SIZE: usize = 128 * 1024;
const MAX_CONTROL_STREAMS: u32 = 1;

fn transport_config() -> Result<Arc<quinn::TransportConfig>, FusenNetError> {
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.keep_alive_interval(Some(Duration::from_secs(1)));
    transport_config.max_idle_timeout(Some(
        Duration::from_secs(60)
            .try_into()
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))?,
    ));
    transport_config.datagram_receive_buffer_size(Some(DATAGRAM_BUFFER_SIZE));
    transport_config.datagram_send_buffer_size(DATAGRAM_BUFFER_SIZE);
    transport_config.max_concurrent_bidi_streams(MAX_CONTROL_STREAMS.into());
    transport_config.max_concurrent_uni_streams(0u32.into());
    Ok(Arc::new(transport_config))
}

#[allow(unused)]
fn make_client_endpoint(
    bind_addr: SocketAddr,
    server_certs: &[&str],
) -> Result<QuicEndpoint, FusenNetError> {
    let mut client_cfg = configure_client(server_certs)?;
    let mut endpoint = QuicEndpoint::client(bind_addr)
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    client_cfg.transport_config(transport_config()?);
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}

fn configure_client(server_certs: &[&str]) -> Result<ClientConfig, FusenNetError> {
    let mut certs = rustls::RootCertStore::empty();
    for pem in server_certs {
        for cert in parse_certificates(pem)? {
            certs
                .add(cert)
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
        }
    }
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|error| FusenNetError::BoxError(Box::new(error)))?
    .with_root_certificates(certs)
    .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicClientConfig::try_from(crypto)
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    Ok(ClientConfig::new(Arc::new(crypto)))
}

#[allow(unused)]
fn make_server_endpoint(
    bind_addr: SocketAddr,
    cert: CertifiedKeyV2,
) -> Result<QuicEndpoint, FusenNetError> {
    let CertifiedKeyV2 {
        priv_key,
        cert_chain,
    } = cert;
    let mut crypto = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|error| FusenNetError::BoxError(Box::new(error)))?
    .with_no_client_auth()
    .with_single_cert(cert_chain, priv_key)
    .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicServerConfig::try_from(crypto)
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    let mut server_config = ServerConfig::with_crypto(Arc::new(crypto));
    server_config.transport_config(transport_config()?);
    let endpoint = QuicEndpoint::server(server_config, bind_addr)
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    Ok(endpoint)
}

pub struct CertifiedKeyV2 {
    priv_key: PrivateKeyDer<'static>,
    cert_chain: Vec<CertificateDer<'static>>,
}

pub fn generate_signed(priv_key: &str, cert: &str) -> Result<CertifiedKeyV2, FusenNetError> {
    Ok(CertifiedKeyV2 {
        priv_key: parse_private_key(priv_key)?,
        cert_chain: parse_certificates(cert)?,
    })
}

fn parse_certificates(pem: &str) -> Result<Vec<CertificateDer<'static>>, FusenNetError> {
    let certificates = CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    if certificates.is_empty() {
        return Err(FusenNetError::BoxError(Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            "certificate PEM does not contain a certificate",
        ))));
    }
    Ok(certificates)
}

fn parse_private_key(pem: &str) -> Result<PrivateKeyDer<'static>, FusenNetError> {
    PrivateKeyDer::from_pem_slice(pem.as_bytes())
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))
}

#[derive(Clone, Debug)]
enum QuinnInitializationError {
    Handshake(quinn::ConnectionError),
    StateUnavailable,
}

impl QuinnInitializationError {
    fn to_fusen_error(&self) -> FusenNetError {
        match self {
            Self::Handshake(error) => FusenNetError::QuinnConnectionError(error.clone()),
            Self::StateUnavailable => FusenNetError::BoxError(Box::new(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "QUIC handshake state is unavailable",
            ))),
        }
    }
}

#[derive(Debug)]
struct QuinnConnectionState {
    connection: OnceCell<Result<quinn::Connection, QuinnInitializationError>>,
    pending: Mutex<Option<std::pin::Pin<Box<quinn::Connecting>>>>,
}

impl QuinnConnectionState {
    fn established(connection: quinn::Connection) -> Self {
        Self {
            connection: OnceCell::new_with(Some(Ok(connection))),
            pending: Mutex::new(None),
        }
    }

    fn pending(connecting: Result<quinn::Connecting, quinn::ConnectionError>) -> Self {
        match connecting {
            Ok(connecting) => Self {
                connection: OnceCell::new(),
                pending: Mutex::new(Some(Box::pin(connecting))),
            },
            Err(error) => Self {
                connection: OnceCell::new_with(Some(Err(QuinnInitializationError::Handshake(
                    error,
                )))),
                pending: Mutex::new(None),
            },
        }
    }

    async fn connection(&self) -> Result<quinn::Connection, FusenNetError> {
        match self
            .connection
            .get_or_init(|| self.complete_handshake())
            .await
        {
            Ok(connection) => Ok(connection.clone()),
            Err(error) => Err(error.to_fusen_error()),
        }
    }

    fn ready_connection(&self) -> Result<&quinn::Connection, FusenNetError> {
        match self.connection.get() {
            Some(Ok(connection)) => Ok(connection),
            Some(Err(error)) => Err(error.to_fusen_error()),
            None => Err(FusenNetError::BoxError(Box::new(io::Error::new(
                io::ErrorKind::WouldBlock,
                "QUIC handshake is not complete; Datagram send is unavailable",
            )))),
        }
    }

    async fn complete_handshake(&self) -> Result<quinn::Connection, QuinnInitializationError> {
        let mut pending = self.pending.lock().await;
        // Await in place so cancellation leaves Connecting available for a retry;
        // dropping the owning QuinnConnect still closes it immediately.
        let result = match pending.as_mut() {
            Some(connecting) => connecting.as_mut().await,
            None => return Err(QuinnInitializationError::StateUnavailable),
        };
        *pending = None;
        result.map_err(QuinnInitializationError::Handshake)
    }
}

#[derive(Debug)]
pub struct QuinnConnect {
    remote_address: SocketAddr,
    state: Arc<QuinnConnectionState>,
}

impl QuinnConnect {
    fn established(connection: quinn::Connection) -> Self {
        Self {
            remote_address: connection.remote_address(),
            state: Arc::new(QuinnConnectionState::established(connection)),
        }
    }

    fn pending(incoming: quinn::Incoming) -> Self {
        let remote_address = incoming.remote_address();
        // This starts Quinn's internal driver but does not await TLS, allowing
        // the listener to accept another Initial while the handshake proceeds.
        Self {
            remote_address,
            state: Arc::new(QuinnConnectionState::pending(incoming.accept())),
        }
    }
}

impl StreamStop for RecvStream {
    fn steam_stop(&mut self) {
        let _ = self.stop(VarInt::from_u32(0x100));
    }
}

impl StreamStop for SendStream {
    fn steam_stop(&mut self) {
        let _ = self.finish();
    }
}

impl common::ReadStream for RecvStream {}
impl common::WriteStream for SendStream {}

impl Connection for QuinnConnect {
    fn open_bi(
        &self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>
    {
        let state = self.state.clone();
        Box::pin(async move {
            let connect = state.connection().await?;
            let (send_stream, recv_stream) = connect.open_bi().await?;
            Ok((recv_stream, send_stream))
        })
    }

    fn accept_bi(
        &mut self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>
    {
        let state = self.state.clone();
        Box::pin(async move {
            let connect = state.connection().await?;
            let (send_stream, recv_stream) = connect.accept_bi().await?;
            Ok((recv_stream, send_stream))
        })
    }

    fn send_datagram(&self, bytes: bytes::Bytes) -> Result<(), FusenNetError> {
        let connect = self.state.ready_connection()?;
        if let Some(error) = connect.close_reason() {
            return Err(FusenNetError::QuinnConnectionError(error));
        }
        if connect.max_datagram_size().is_some()
            && connect.datagram_send_buffer_space() < bytes.len()
        {
            return Err(FusenNetError::DatagramQueueFull);
        }
        connect.send_datagram(bytes).map_err(|error| match error {
            SendDatagramError::TooLarge => FusenNetError::DatagramTooLarge,
            SendDatagramError::ConnectionLost(error) => FusenNetError::QuinnConnectionError(error),
            error => FusenNetError::BoxError(Box::new(error)),
        })
    }

    fn recv_datagram(&mut self) -> BoxFuture<'_, Result<Bytes, FusenNetError>> {
        let state = self.state.clone();
        Box::pin(async move {
            state
                .connection()
                .await?
                .read_datagram()
                .await
                .map_err(FusenNetError::QuinnConnectionError)
        })
    }

    fn remote_address(&self) -> SocketAddr {
        self.remote_address
    }

    fn closed(&self) -> BoxFuture<'_, FusenNetError> {
        let state = self.state.clone();
        Box::pin(async move {
            match state.connection().await {
                Ok(connect) => FusenNetError::QuinnConnectionError(connect.closed().await),
                Err(error) => error,
            }
        })
    }
}

pub struct QuinnEndpoint {
    pub endpoint: Arc<quinn::Endpoint>,
}

impl QuinnEndpoint {
    pub fn make_server_endpoint(
        bind_port: u16,
        cert: &str,
        prik: &str,
    ) -> Result<impl Endpoint, FusenNetError> {
        Self::make_server_endpoint_at(SocketAddr::from(([0, 0, 0, 0], bind_port)), cert, prik)
    }

    pub fn make_server_endpoint_at(
        bind_addr: SocketAddr,
        cert: &str,
        prik: &str,
    ) -> Result<impl Endpoint, FusenNetError> {
        let endpoint = make_server_endpoint(bind_addr, generate_signed(prik, cert)?)?;
        let endpoint = QuinnEndpoint {
            endpoint: Arc::new(endpoint),
        };
        Ok(endpoint)
    }

    pub fn make_client_endpoint(cert: &str) -> Result<impl Endpoint + 'static, FusenNetError> {
        Self::make_client_endpoint_at(SocketAddr::from(([0, 0, 0, 0], 0)), cert)
    }

    pub fn make_client_endpoint_at(
        bind_address: SocketAddr,
        cert: &str,
    ) -> Result<impl Endpoint + 'static, FusenNetError> {
        let endpoint = make_client_endpoint(bind_address, vec![cert].as_slice())?;
        let endpoint = QuinnEndpoint {
            endpoint: Arc::new(endpoint),
        };
        Ok(endpoint)
    }
}

impl Endpoint for QuinnEndpoint {
    fn accept(&self) -> BoxFuture<'_, Result<impl Connection, FusenNetError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let incoming = endpoint
                .accept()
                .await
                .ok_or(FusenNetError::EndpointClose)?;
            Ok(QuinnConnect::pending(incoming))
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<'_, Result<impl Connection, FusenNetError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let connect = endpoint.connect(addr, &server_name)?.await?;
            Ok(QuinnConnect::established(connect))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quinn::{
        AsyncUdpSocket, EndpointConfig, Runtime, TokioRuntime, UdpPoller,
        udp::{RecvMeta, Transmit},
    };
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use std::{
        io::IoSliceMut,
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::time::timeout;

    const SERVER_NAME: &str = "localhost";
    const TEST_TIMEOUT: Duration = Duration::from_secs(5);
    const PENDING_TIMEOUT: Duration = Duration::from_millis(200);

    #[derive(Debug)]
    struct SendOnlySocket {
        inner: Arc<dyn AsyncUdpSocket>,
    }

    impl AsyncUdpSocket for SendOnlySocket {
        fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
            self.inner.clone().create_io_poller()
        }

        fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
            self.inner.try_send(transmit)
        }

        fn poll_recv(
            &self,
            _context: &mut Context<'_>,
            _buffers: &mut [IoSliceMut<'_>],
            _metadata: &mut [RecvMeta],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.local_addr()
        }

        fn max_transmit_segments(&self) -> usize {
            self.inner.max_transmit_segments()
        }

        fn max_receive_segments(&self) -> usize {
            self.inner.max_receive_segments()
        }

        fn may_fragment(&self) -> bool {
            self.inner.may_fragment()
        }
    }

    fn make_send_only_client_endpoint(certificate_pem: &str) -> QuicEndpoint {
        let socket = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("bind send-only client socket");
        socket
            .set_nonblocking(true)
            .expect("make send-only client socket nonblocking");
        let runtime: Arc<dyn Runtime> = Arc::new(TokioRuntime);
        let socket = runtime
            .wrap_udp_socket(socket)
            .expect("wrap send-only client socket");
        let mut endpoint = QuicEndpoint::new_with_abstract_socket(
            EndpointConfig::default(),
            None,
            Arc::new(SendOnlySocket { inner: socket }),
            runtime,
        )
        .expect("create send-only client endpoint");
        let mut client_config = configure_client(&[certificate_pem]).expect("client TLS config");
        client_config.transport_config(transport_config().expect("client transport configuration"));
        endpoint.set_default_client_config(client_config);
        endpoint
    }

    #[tokio::test]
    async fn transport_allows_only_the_v1_control_stream() {
        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec![SERVER_NAME.to_owned()])
                .expect("generate test certificate");
        let server = make_server_endpoint(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            generate_signed(&key_pair.serialize_pem(), &cert.pem())
                .expect("parse test certificate"),
        )
        .expect("create server endpoint");
        let server_address = server.local_addr().expect("server address");
        let client = make_client_endpoint(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            &[cert.pem().as_str()],
        )
        .expect("create client endpoint");

        let (server_connection, client_connection) = timeout(TEST_TIMEOUT, async {
            tokio::join!(
                async {
                    server
                        .accept()
                        .await
                        .expect("server endpoint open")
                        .await
                        .expect("server handshake")
                },
                async {
                    client
                        .connect(server_address, SERVER_NAME)
                        .expect("start client handshake")
                        .await
                        .expect("client handshake")
                }
            )
        })
        .await
        .expect("QUIC handshake timeout");

        let (mut client_writer, client_reader) = client_connection
            .open_bi()
            .await
            .expect("open client control stream");
        client_writer
            .write_all(b"c")
            .await
            .expect("activate client control stream");
        let server_control = timeout(TEST_TIMEOUT, server_connection.accept_bi())
            .await
            .expect("server control stream timeout")
            .expect("accept client control stream");

        let (mut server_writer, server_reader) = server_connection
            .open_bi()
            .await
            .expect("open server control stream");
        server_writer
            .write_all(b"s")
            .await
            .expect("activate server control stream");
        let client_control = timeout(TEST_TIMEOUT, client_connection.accept_bi())
            .await
            .expect("client control stream timeout")
            .expect("accept server control stream");

        assert!(
            timeout(PENDING_TIMEOUT, client_connection.open_bi())
                .await
                .is_err(),
            "server admitted a second bidirectional stream"
        );
        assert!(
            timeout(PENDING_TIMEOUT, server_connection.open_bi())
                .await
                .is_err(),
            "client admitted a second bidirectional stream"
        );
        assert!(
            timeout(PENDING_TIMEOUT, client_connection.open_uni())
                .await
                .is_err(),
            "server admitted a unidirectional stream"
        );
        assert!(
            timeout(PENDING_TIMEOUT, server_connection.open_uni())
                .await
                .is_err(),
            "client admitted a unidirectional stream"
        );

        drop((client_writer, client_reader, server_control));
        drop((server_writer, server_reader, client_control));
        client.close(VarInt::from_u32(0), b"test complete");
        let _close_reason = client_connection.closed().await;
        assert!(matches!(
            QuinnConnect::established(client_connection)
                .send_datagram(Bytes::from_static(b"after close")),
            Err(FusenNetError::QuinnConnectionError(_))
        ));
        server.close(VarInt::from_u32(0), b"test complete");
    }

    #[tokio::test]
    async fn pending_handshake_does_not_block_accepting_the_next_incoming() {
        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec![SERVER_NAME.to_owned()])
                .expect("generate test certificate");
        let certificate_pem = cert.pem();
        let server_endpoint = make_server_endpoint(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            generate_signed(&key_pair.serialize_pem(), &certificate_pem)
                .expect("parse test certificate"),
        )
        .expect("create server endpoint");
        let server_address = server_endpoint.local_addr().expect("server address");
        let server = QuinnEndpoint {
            endpoint: Arc::new(server_endpoint),
        };

        let stalled_client = make_send_only_client_endpoint(&certificate_pem);
        let stalled_client_address = stalled_client.local_addr().expect("stalled client address");
        let stalled_connecting = stalled_client
            .connect(server_address, SERVER_NAME)
            .expect("start stalled client handshake");
        let first = timeout(TEST_TIMEOUT, server.accept())
            .await
            .expect("first Incoming accept timed out")
            .expect("first Incoming accept failed");
        assert_eq!(first.remote_address(), stalled_client_address);
        match first.send_datagram(Bytes::from_static(b"before handshake")) {
            Err(FusenNetError::BoxError(error)) => assert_eq!(
                error.downcast_ref::<io::Error>().map(io::Error::kind),
                Some(io::ErrorKind::WouldBlock)
            ),
            Err(error) => panic!("unexpected pending Datagram error: {error}"),
            Ok(()) => panic!("pending connection accepted a Datagram"),
        }

        let first_handshake = tokio::spawn(async move { first.closed().await });
        tokio::time::sleep(PENDING_TIMEOUT).await;
        assert!(
            !first_handshake.is_finished(),
            "send-only client unexpectedly completed its handshake"
        );

        let second_client = make_client_endpoint(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            &[certificate_pem.as_str()],
        )
        .expect("create second client endpoint");
        let second_client_address = second_client.local_addr().expect("second client address");
        let second_connecting = second_client
            .connect(server_address, SERVER_NAME)
            .expect("start second client handshake");
        let second = timeout(TEST_TIMEOUT, server.accept())
            .await
            .expect("second Incoming was blocked by the first handshake")
            .expect("second Incoming accept failed");
        assert_eq!(second.remote_address(), second_client_address);

        drop(second);
        let rejection = timeout(TEST_TIMEOUT, second_connecting)
            .await
            .expect("dropped Pending connection did not reject the client");
        assert!(
            rejection.is_err(),
            "dropped Pending connection was accepted"
        );

        first_handshake.abort();
        let _ = first_handshake.await;
        drop(stalled_connecting);
        stalled_client.close(VarInt::from_u32(0), b"test complete");
        second_client.close(VarInt::from_u32(0), b"test complete");
        server.endpoint.close(VarInt::from_u32(0), b"test complete");
    }
}
