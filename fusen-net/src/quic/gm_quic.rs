use super::{Connection, Endpoint};
use crate::common::{BoxError, ConnectError};
use futures::future::BoxFuture;
use gm_quic::{ClientParameters, Connection as QuicConnect, HeartbeatConfig, QuicClient};
use gm_quic::{QuicServer, ServerParameters};
use rustls::RootCertStore;
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer,
    pem::{self, PemObject},
};
use std::io;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};

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
    connect: Arc<QuicConnect>,
    remote_address: SocketAddr,
}

impl Connection for GmQuicConnect {
    fn open_bi(
        &self,
    ) -> BoxFuture<Result<(impl AsyncRead + 'static, impl AsyncWrite + 'static), ConnectError>>
    {
        let connect = self.connect.clone();
        Box::pin(async move {
            let stream = match connect.open_bi_stream().await {
                Ok(stream) => stream,
                Err(error) => return Err(ConnectError::GmQuicConnectError(error)),
            };
            let Some((_stream_id, (recv_stream, send_stream))) = stream else {
                return Err(ConnectError::GmQuicConnectError(io::Error::other(
                    "open_bi error !",
                )));
            };
            Ok((recv_stream, send_stream))
        })
    }

    fn accept_bi(
        &self,
    ) -> BoxFuture<Result<(impl AsyncRead + 'static, impl AsyncWrite + 'static), ConnectError>>
    {
        let connect = self.connect.clone();
        Box::pin(async move {
            let stream = match connect.accept_bi_stream().await {
                Ok(stream) => stream,
                Err(error) => return Err(ConnectError::GmQuicConnectError(error)),
            };
            let Some((_stream_id, (recv_stream, send_stream))) = stream else {
                return Err(ConnectError::GmQuicConnectError(io::Error::other(
                    "open_bi error !",
                )));
            };
            Ok((recv_stream, send_stream))
        })
    }

    fn remote_address(&self) -> SocketAddr {
        self.remote_address
    }

    fn closed(&self) -> BoxFuture<ConnectError> {
        let connect = self.connect.clone();
        Box::pin(async move {
            connect.close("done".into(), 0);
            ConnectError::ConnectClose
        })
    }
}

pub struct GmQuicEndpoint {
    pub server: Option<Arc<QuicServer>>,
    pub client: Option<Arc<QuicClient>>,
}

impl GmQuicEndpoint {
    pub fn make_server_endpoint(
        bind_port: u16,
        cert: &str,
        prik: &str,
    ) -> Result<impl Endpoint, BoxError> {
        let _ = default_provider().install_default();
        let certifie_key = generate_signed(prik, cert)?;
        let endpoint: Arc<QuicServer> = QuicServer::builder()
            .defer_idle_timeout(HeartbeatConfig::new_with_interval(
                Duration::from_millis(5000),
                Duration::from_millis(1000),
            ))
            .without_client_cert_verifier()
            .with_single_cert(
                vec![certifie_key.cert],
                PrivateKeyDer::Pkcs8(certifie_key.priv_key),
            )
            .with_parameters(server_parameters())
            .listen(format!("0.0.0.0:{}", bind_port).parse::<SocketAddr>()?)?;
        Ok(GmQuicEndpoint {
            server: Some(endpoint),
            client: None,
        })
    }

    pub fn make_client_endpoint(cert: &str) -> Result<impl Endpoint + 'static, BoxError> {
        let _ = default_provider().install_default();
        let cert = CertificateDer::from_pem_reader(cert.as_bytes())?;
        let mut roots = RootCertStore::empty();
        roots.add_parsable_certificates(vec![cert]);
        let client = QuicClient::builder()
            .defer_idle_timeout(HeartbeatConfig::new_with_interval(
                Duration::from_millis(5000),
                Duration::from_millis(1000),
            ))
            .with_root_certificates(roots)
            .without_cert()
            .with_parameters(client_parameters())
            .reuse_connection()
            .build();
        Ok(GmQuicEndpoint {
            server: None,
            client: Some(Arc::new(client)),
        })
    }
}

impl Endpoint for GmQuicEndpoint {
    fn accept(&self) -> BoxFuture<Result<impl Connection, ConnectError>> {
        let server = self.server.clone();
        Box::pin(async move {
            let Some(server) = server else {
                return Err(ConnectError::EndpointClose);
            };
            let (connect, addr) = server
                .accept()
                .await
                .map_err(ConnectError::GmQuicConnectError)?;
            Ok(GmQuicConnect {
                connect,
                remote_address: addr.remote().addr(),
            })
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<Result<impl Connection, ConnectError>> {
        let client = self.client.clone();
        Box::pin(async move {
            let Some(client) = client else {
                return Err(ConnectError::EndpointClose);
            };
            let connect = client
                .connect(&server_name, addr)
                .map_err(ConnectError::GmQuicConnectError)?;
            Ok(GmQuicConnect {
                connect,
                remote_address: addr,
            })
        })
    }
}

pub fn server_parameters() -> ServerParameters {
    let mut params = ServerParameters::default();
    params.set_initial_max_streams_bidi(1000u32);
    params.set_initial_max_streams_uni(1000u32);
    params.set_initial_max_data(1u32 << 20);
    params.set_initial_max_stream_data_uni(1u32 << 20);
    params.set_initial_max_stream_data_bidi_local(1u32 << 20);
    params.set_initial_max_stream_data_bidi_remote(1u32 << 20);
    params.set_max_idle_timeout(Duration::from_secs(30));
    params
}

pub fn client_parameters() -> ClientParameters {
    let mut params = ClientParameters::default();
    params.set_initial_max_streams_bidi(1000u32);
    params.set_initial_max_streams_uni(1000u32);
    params.set_initial_max_data(1u32 << 20);
    params.set_initial_max_stream_data_uni(1u32 << 20);
    params.set_initial_max_stream_data_bidi_local(1u32 << 20);
    params.set_initial_max_stream_data_bidi_remote(1u32 << 20);
    params
}
