use super::{Connection, Endpoint, StreamStop};
use crate::common::{self};
use crate::error::FusenNetError;
use bytes::Bytes;
use futures::future::BoxFuture;
use gm_quic::prelude::{
    BindUri, BuildListenersError, CancelStream, Connection as GmConnect, EndpointAddr,
    ParseBindUriError, QuicClient, QuicListeners, ServerError, SocketEndpointAddr, StopSending,
    StreamReader, StreamWriter, handy,
};
use rustls::RootCertStore;
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer,
    pem::{self, PemObject},
};
use std::io;
use std::str::FromStr;
use std::{net::SocketAddr, sync::Arc, time::Duration};

pub struct CertifiedKeyV2<'a> {
    priv_key: PrivatePkcs8KeyDer<'a>,
    cert: CertificateDer<'a>,
}

pub fn generate_signed<'a>(priv_key: &str, cert: &str) -> Result<CertifiedKeyV2<'a>, pem::Error> {
    let priv_key = PrivatePkcs8KeyDer::from_pem_reader(priv_key.as_bytes())?;
    let cert = CertificateDer::from_pem_reader(cert.as_bytes())?;
    Ok(CertifiedKeyV2 { priv_key, cert })
}

pub struct GmQuicConnect {
    connect: GmConnect,
    remote_address: EndpointAddr,
}

impl StreamStop for StreamReader {
    fn steam_stop(&mut self) {
        self.stop(0x100);
    }
}

impl StreamStop for StreamWriter {
    fn steam_stop(&mut self) {
        let _ = self.cancel(0x100);
    }
}

impl common::ReadStream for StreamReader {}
impl common::WriteStream for StreamWriter {}

impl Connection for GmQuicConnect {
    fn open_bi(
        &self,
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>> {
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
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>> {
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
        todo!()
    }

    fn recv_datagram(&self) -> BoxFuture<Result<Bytes, FusenNetError>> {
        todo!()
    }

    fn remote_address(&self) -> SocketAddr {
        self.remote_address;
        todo!()
    }

    fn closed(&self) -> BoxFuture<FusenNetError> {
        Box::pin(async move {
            let _ = self.connect.close("done", 0);
            FusenNetError::ConnectClose
        })
    }
}

pub struct GmQuicEndpoint {
    pub server: Option<Arc<QuicListeners>>,
    pub client: Option<Arc<QuicClient>>,
}

impl From<pem::Error> for FusenNetError {
    fn from(value: pem::Error) -> Self {
        FusenNetError::BoxError(Box::new(value))
    }
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
        let _ = default_provider().install_default();
        let certifie_key = generate_signed(prik, cert)?;
        let mut parameters = handy::server_parameters();
        // let _ = parameters.set(ParameterId::InitialMaxStreamsBidi, 1000u32);
        // let _ = parameters.set(ParameterId::InitialMaxStreamsUni, 1000u32);
        let endpoint = QuicListeners::builder()?
            .without_client_cert_verifier()
            .with_parameters(parameters)
            .defer_idle_timeout(Duration::from_secs(60))
            .enable_0rtt()
            .listen(4096);
        let bind_uris = vec![BindUri::from_str(
            format!("0.0.0.0:{}", bind_port).as_str(),
        )?];
        endpoint.add_server(
            "localhost",
            certifie_key.cert,
            PrivateKeyDer::Pkcs8(certifie_key.priv_key),
            bind_uris,
            None,
        )?;
        Ok(GmQuicEndpoint {
            server: Some(endpoint),
            client: None,
        })
    }

    pub fn make_client_endpoint(cert: &str) -> Result<impl Endpoint + 'static, FusenNetError> {
        let _ = default_provider().install_default();
        let cert = CertificateDer::from_pem_reader(cert.as_bytes())?;
        let mut roots = RootCertStore::empty();
        roots.add_parsable_certificates(vec![cert]);
        let mut parameters = handy::client_parameters();
        // let _ = parameters.set(ParameterId::InitialMaxStreamsBidi, 1000u32);
        // let _ = parameters.set(ParameterId::InitialMaxStreamsUni, 1000u32);
        let client = QuicClient::builder()
            .defer_idle_timeout(Duration::from_secs(20))
            .with_root_certificates(roots)
            .without_cert()
            .with_parameters(parameters)
            .enable_sslkeylog()
            .enable_0rtt()
            .build();
        Ok(GmQuicEndpoint {
            server: None,
            client: Some(Arc::new(client)),
        })
    }
}

impl Endpoint for GmQuicEndpoint {
    fn accept(&self) -> BoxFuture<Result<impl Connection, FusenNetError>> {
        let server = self.server.clone();
        Box::pin(async move {
            let Some(server) = server else {
                return Err(FusenNetError::EndpointClose);
            };
            let (connect, _, addr, _) = server.accept().await.map_err(|error| {
                FusenNetError::GmQuicConnectError(io::Error::other(error.to_string()))
            })?;
            Ok(GmQuicConnect {
                connect,
                remote_address: addr.remote(),
            })
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<Result<impl Connection, FusenNetError>> {
        let client = self.client.clone();
        Box::pin(async move {
            let Some(client) = client else {
                return Err(FusenNetError::EndpointClose);
            };
            let addr = EndpointAddr::Socket(SocketEndpointAddr::Direct { addr });
            let connect = client
                .connected_to(&server_name, vec![addr.clone()])
                .map_err(|error| {
                    FusenNetError::GmQuicConnectError(io::Error::other(error.to_string()))
                })?;
            Ok(GmQuicConnect {
                connect,
                remote_address: addr,
            })
        })
    }
}
