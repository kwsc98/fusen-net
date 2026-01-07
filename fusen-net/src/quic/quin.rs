use super::{Connection, Endpoint, StreamStop};
use crate::{common, error::FusenNetError};
use bytes::Bytes;
use futures::future::BoxFuture;
use gm_quic::qrecovery::streams::error;
use quinn::{ClientConfig, Endpoint as QuicEndpoint, RecvStream, SendStream, ServerConfig, VarInt};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, pem, pem::PemObject};
use std::{net::SocketAddr, sync::Arc, time::Duration};

#[allow(unused)]
fn make_client_endpoint(
    bind_addr: SocketAddr,
    server_certs: &[&str],
) -> Result<QuicEndpoint, FusenNetError> {
    let client_cfg = configure_client(server_certs)?;
    let mut endpoint = QuicEndpoint::client(bind_addr)
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}

fn configure_client(server_certs: &[&str]) -> Result<ClientConfig, FusenNetError> {
    let mut certs = rustls::RootCertStore::empty();
    for cert in server_certs {
        certs
            .add(
                CertificateDer::from_pem_reader(cert.as_bytes())
                    .map_err(|error| FusenNetError::BoxError(Box::new(error)))?,
            )
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    }
    Ok(ClientConfig::with_root_certificates(Arc::new(certs))
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?)
}

#[allow(unused)]
fn make_server_endpoint(
    bind_addr: SocketAddr,
    cert: CertifiedKeyV2<'static>,
) -> Result<QuicEndpoint, FusenNetError> {
    let CertifiedKeyV2 { priv_key, cert } = cert;
    let mut server_config = ServerConfig::with_single_cert(vec![cert.clone()], priv_key.into())
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.keep_alive_interval(Some(Duration::from_millis(1000)));
    transport_config.max_idle_timeout(Some(
        Duration::from_millis(60000)
            .try_into()
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))?,
    ));
    transport_config.max_concurrent_bidi_streams(1000u32.into());
    let endpoint = QuicEndpoint::server(server_config, bind_addr)
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
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
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>> {
        let connect = self.connect.clone();
        Box::pin(async move {
            let (send_stream, recv_stream) = connect.open_bi().await?;
            Ok((recv_stream, send_stream))
        })
    }

    fn accept_bi(
        &mut self,
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>> {
        let connect = self.connect.clone();
        Box::pin(async move {
            let (send_stream, recv_stream) = connect.accept_bi().await?;
            Ok((recv_stream, send_stream))
        })
    }

    fn send_datagram(&self, bytes: bytes::Bytes) -> Result<(), FusenNetError> {
        self.connect
            .send_datagram(bytes)
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))
    }

    fn recv_datagram(&self) -> BoxFuture<Result<Bytes, FusenNetError>> {
        Box::pin(async move {
            self.connect
                .read_datagram()
                .await
                .map_err(|error| FusenNetError::QuinnConnectionError(error))
        })
    }

    fn remote_address(&self) -> SocketAddr {
        self.connect.remote_address()
    }

    fn closed(&self) -> BoxFuture<FusenNetError> {
        let connect = self.connect.clone();
        Box::pin(async move {
            connect.closed().await;
            FusenNetError::ConnectClose
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
        let bind_addr = format!("0.0.0.0:{}", bind_port)
            .parse()
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
        let endpoint = make_server_endpoint(
            bind_addr,
            generate_signed(prik, cert)
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?,
        )?;
        let endpoint = QuinnEndpoint {
            endpoint: Arc::new(endpoint),
        };
        Ok(endpoint)
    }

    pub fn make_client_endpoint(cert: &str) -> Result<impl Endpoint + 'static, FusenNetError> {
        let endpoint = make_client_endpoint("0.0.0.0:0".parse().unwrap(), vec![cert].as_slice())?;
        let endpoint = QuinnEndpoint {
            endpoint: Arc::new(endpoint),
        };
        Ok(endpoint)
    }
}

impl Endpoint for QuinnEndpoint {
    fn accept(&self) -> BoxFuture<Result<impl Connection, FusenNetError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let connect = endpoint
                .accept()
                .await
                .ok_or(FusenNetError::EndpointClose)?;
            let connect = connect.await?;
            Ok(QuinnConnect { connect })
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<Result<impl Connection, FusenNetError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let connect = endpoint.connect(addr, &server_name)?.await?;
            Ok(QuinnConnect { connect })
        })
    }
}
