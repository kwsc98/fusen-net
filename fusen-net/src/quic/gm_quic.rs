use super::{Connection, Endpoint};
use crate::common::{BoxError, ConnectError};
use futures::future::BoxFuture;
use gm_quic::QuicServer;
use quinn::{ClientConfig, Endpoint as QuicEndpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, pem, pem::PemObject};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};

#[allow(unused)]
fn make_client_endpoint(
    bind_addr: SocketAddr,
    server_certs: &[&str],
) -> Result<QuicEndpoint, BoxError> {
    let client_cfg = configure_client(server_certs)?;
    let mut endpoint = QuicEndpoint::client(bind_addr)?;
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}

fn configure_client(server_certs: &[&str]) -> Result<ClientConfig, BoxError> {
    let mut certs = rustls::RootCertStore::empty();
    for cert in server_certs {
        certs.add(CertificateDer::from_pem_reader(cert.as_bytes())?)?;
    }
    Ok(ClientConfig::with_root_certificates(Arc::new(certs))?)
}

#[allow(unused)]
fn make_server_endpoint(
    bind_addr: SocketAddr,
    cert: CertifiedKeyV2<'static>,
) -> Result<QuicEndpoint, BoxError> {
    let CertifiedKeyV2 { priv_key, cert } = cert;
    let mut server_config = ServerConfig::with_single_cert(vec![cert.clone()], priv_key.into())?;
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.keep_alive_interval(Some(Duration::from_millis(1000)));
    transport_config.max_idle_timeout(Some(Duration::from_millis(5000).try_into()?));
    transport_config.max_concurrent_bidi_streams(1000u32.into());
    let endpoint = QuicEndpoint::server(server_config, bind_addr)?;
    Ok(endpoint)
}

pub struct CertifiedKeyV2<'a> {
    priv_key: PrivatePkcs8KeyDer<'a>,
    cert: CertificateDer<'a>,
}

pub fn generate_signed<'a>(priv_key: &str, cert: &str) -> Result<CertifiedKeyV2<'a>, pem::Error> {
    let priv_key = PrivatePkcs8KeyDer::from_pem_reader(priv_key.as_bytes())?;
    let cert = CertificateDer::from_pem_reader(cert.as_bytes())?;
    Ok(CertifiedKeyV2 { priv_key, cert })
}

#[derive(Debug)]
pub struct QuinnConnect {
    connect: quinn::Connection,
}

impl Connection for QuinnConnect {
    fn open_bi(
        &self,
    ) -> BoxFuture<Result<(impl AsyncRead + 'static, impl AsyncWrite + 'static), ConnectError>>
    {
        let connect = self.connect.clone();
        Box::pin(async move {
            let (send_stream, recv_stream) = connect.open_bi().await?;
            Ok((recv_stream, send_stream))
        })
    }

    fn accept_bi(
        &self,
    ) -> BoxFuture<Result<(impl AsyncRead + 'static, impl AsyncWrite + 'static), ConnectError>>
    {
        let connect = self.connect.clone();
        Box::pin(async move {
            let (send_stream, recv_stream) = connect.accept_bi().await?;
            Ok((recv_stream, send_stream))
        })
    }

    fn remote_address(&self) -> SocketAddr {
        self.connect.remote_address()
    }

    fn closed(&self) -> BoxFuture<ConnectError> {
        let connect = self.connect.clone();
        Box::pin(async move {
            connect.closed().await;
            ConnectError::ConnectClose
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
    ) -> Result<impl Endpoint, BoxError> {
        let bind_addr = format!("0.0.0.0:{}", bind_port).parse()?;
        let quic = QuicServer::builder().without_client_cert_verifier().with_single_cert(cert_chain, key_der);
        let endpoint = make_server_endpoint(bind_addr, generate_signed(prik, cert)?)?;
        let endpoint = QuinnEndpoint {
            endpoint: Arc::new(endpoint),
        };
        Ok(endpoint)
    }

    pub fn make_client_endpoint(cert: &str) -> Result<impl Endpoint + 'static, BoxError> {
        let endpoint = make_client_endpoint("0.0.0.0:0".parse().unwrap(), vec![cert].as_slice())?;
        let endpoint = QuinnEndpoint {
            endpoint: Arc::new(endpoint),
        };
        Ok(endpoint)
    }
}

impl Endpoint for QuinnEndpoint {
    fn accept(&self) -> BoxFuture<Result<impl Connection, ConnectError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let connect = endpoint.accept().await.ok_or(ConnectError::EndpointClose)?;
            let connect = connect.await?;
            Ok(QuinnConnect { connect })
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<Result<impl Connection, ConnectError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let connect = endpoint.connect(addr, &server_name)?.await?;
            Ok(QuinnConnect { connect })
        })
    }
}
