use std::{fmt::Debug, net::SocketAddr, sync::Arc};

use base64::Engine;
use fusen_common::BoxError;
use futures::lock::Mutex;
use quin::{generate_signed, QuinnEndPoint};
use s2n::S2nEndPoint;
use tokio::io::{AsyncRead, AsyncWrite};

mod quin;
mod s2n;

pub trait EndPoint: 'static {
    fn accept(&self) -> fusen_common::FusenFuture<Result<impl Connection, BoxError>>;

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> fusen_common::FusenFuture<Result<impl Connection, BoxError>>;
}

pub trait Connection: 'static + Debug + Send {
    fn open_bi(
        &self,
    ) -> fusen_common::FusenFuture<
        Result<
            (
                impl AsyncRead + 'static + Send + Sync,
                impl AsyncWrite + 'static + Send + Sync,
            ),
            BoxError,
        >,
    >;

    fn accept_bi(
        &self,
    ) -> fusen_common::FusenFuture<
        Result<
            (
                impl AsyncRead + 'static + Send + Sync,
                impl AsyncWrite + 'static + Send + Sync,
            ),
            BoxError,
        >,
    >;

    fn remote_address(&self) -> SocketAddr;

    fn closed(&self) -> fusen_common::FusenFuture<BoxError>;
}

// pub fn make_server_endpoint(
//     bind_port: &str,
//     cert: &str,
//     prik: &str,
// ) -> Result<impl EndPoint, BoxError> {
//     let bind_addr = format!("0.0.0.0:{}", bind_port).parse()?;
//     let endpoint = quin::make_server_endpoint(bind_addr, generate_signed(prik, cert)?)?;
//     let endpoint = QuinnEndPoint {
//         endpoint: Arc::new(endpoint),
//     };
//     Ok(endpoint)
// }

// pub fn make_client_endpoint(cert: &str) -> Result<impl EndPoint + 'static, BoxError> {
//     let endpoint = quin::make_client_endpoint(
//         "0.0.0.0:0".parse().unwrap(),
//         vec![base64::prelude::BASE64_STANDARD.decode(cert)?.as_slice()].as_slice(),
//     )?;
//     let endpoint = QuinnEndPoint {
//         endpoint: Arc::new(endpoint),
//     };
//     Ok(endpoint)
// }

pub fn make_server_endpoint(
    bind_port: &str,
    cert: &str,
    prik: &str,
) -> Result<impl EndPoint, BoxError> {
    let server = s2n::get_server()?;
    let endpoint = S2nEndPoint {
        endpoint: Arc::new(s2n::S2nEndPointInfo::Server(Mutex::new(server))),
    };
    Ok(endpoint)
}

pub fn make_client_endpoint(cert: &str) -> Result<impl EndPoint + 'static, BoxError> {
    let client = s2n::get_client()?;
    let endpoint = S2nEndPoint {
        endpoint: Arc::new(s2n::S2nEndPointInfo::Client(client)),
    };
    Ok(endpoint)
}
