use super::{Connection, EndPoint};
use base64::{prelude::BASE64_STANDARD, Engine};
use fusen_common::BoxError;
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::{error::Error, net::SocketAddr, sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};

#[allow(unused)]
pub fn make_client_endpoint(
    bind_addr: SocketAddr,
    server_certs: &[&[u8]],
) -> Result<Endpoint, crate::Error> {
    let client_cfg = configure_client(server_certs)?;
    let mut endpoint = Endpoint::client(bind_addr)?;
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}

fn configure_client(
    server_certs: &[&[u8]],
) -> Result<ClientConfig, Box<dyn Error + Send + Sync + 'static>> {
    let mut certs = rustls::RootCertStore::empty();
    for cert in server_certs {
        certs.add(CertificateDer::from(*cert))?;
    }
    Ok(ClientConfig::with_root_certificates(Arc::new(certs))?)
}

#[allow(unused)]
pub fn make_server_endpoint(
    bind_addr: SocketAddr,
    cert: CertifiedKeyV2<'static>,
) -> Result<Endpoint, crate::Error> {
    let CertifiedKeyV2 { priv_key, cert } = cert;
    let mut server_config = ServerConfig::with_single_cert(vec![cert.clone()], priv_key.into())?;
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.keep_alive_interval(Some(Duration::from_millis(1000)));
    transport_config.max_idle_timeout(Some(Duration::from_millis(2000).try_into()?));
    transport_config.max_concurrent_bidi_streams(1000u32.into());
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    Ok(endpoint)
}

pub struct CertifiedKeyV2<'a> {
    priv_key: PrivatePkcs8KeyDer<'a>,
    cert: CertificateDer<'a>,
}

pub fn generate_signed<'a>(priv_key: &str, cert: &str) -> Result<CertifiedKeyV2<'a>, BoxError> {
    let priv_key = PrivatePkcs8KeyDer::from(BASE64_STANDARD.decode(priv_key)?);
    let cert = CertificateDer::from(BASE64_STANDARD.decode(cert)?);
    Ok(CertifiedKeyV2 { priv_key, cert })
}

#[derive(Debug)]
pub struct QuinnConnect {
    connect: quinn::Connection,
}

impl Connection for QuinnConnect {
    fn open_bi(
        &self,
    ) -> fusen_common::FusenFuture<
        Result<(impl AsyncRead + 'static, impl AsyncWrite + 'static), BoxError>,
    > {
        let connect = self.connect.clone();
        Box::pin(async move {
            let (send_stream, recv_stream) = connect.open_bi().await?;
            Ok((recv_stream, send_stream))
        })
    }

    fn accept_bi(
        &self,
    ) -> fusen_common::FusenFuture<
        Result<(impl AsyncRead + 'static, impl AsyncWrite + 'static), BoxError>,
    > {
        let connect = self.connect.clone();
        Box::pin(async move {
            let (send_stream, recv_stream) = connect.accept_bi().await?;
            Ok((recv_stream, send_stream))
        })
    }

    fn closed(&self) -> fusen_common::FusenFuture<BoxError> {
        let connect = self.connect.clone();
        Box::pin(async move { connect.closed().await.into() })
    }

    fn remote_address(&self) -> SocketAddr {
        self.connect.remote_address()
    }
}

pub struct QuinnEndPoint {
    pub endpoint: Arc<quinn::Endpoint>,
}

impl EndPoint for QuinnEndPoint {
    fn accept(&self) -> fusen_common::FusenFuture<Result<impl Connection, BoxError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let connect = endpoint.accept().await.ok_or("incoming is none !")?;
            let connect = connect.await?;
            Ok(QuinnConnect { connect })
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> fusen_common::FusenFuture<Result<impl Connection, BoxError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let connect = endpoint.connect(addr, &server_name)?.await?;
            Ok(QuinnConnect { connect })
        })
    }
}
