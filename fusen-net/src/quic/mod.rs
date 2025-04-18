use crate::common::ConnectError;
use futures::future::BoxFuture;
use std::net::SocketAddr;
use tokio::io::{AsyncRead, AsyncWrite};

pub mod quin;
pub mod s2n;

pub trait Endpoint: 'static {
    fn accept(&self) -> BoxFuture<Result<impl Connection, ConnectError>>;

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<Result<impl Connection, ConnectError>>;
}

pub trait Connection: Send + Sync + 'static {
    fn open_bi(
        &self,
    ) -> BoxFuture<
        Result<
            (
                impl AsyncRead + 'static + Send + Sync,
                impl AsyncWrite + 'static + Send + Sync,
            ),
            ConnectError,
        >,
    >;

    fn accept_bi(
        &self,
    ) -> BoxFuture<
        Result<
            (
                impl AsyncRead + 'static + Send + Sync,
                impl AsyncWrite + 'static + Send + Sync,
            ),
            ConnectError,
        >,
    >;

    fn remote_address(&self) -> SocketAddr;

    fn closed(&self) -> BoxFuture<ConnectError>;
}
