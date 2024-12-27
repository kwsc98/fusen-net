use fusen_common::BoxError;
use std::{fmt::Debug, net::SocketAddr};
use tokio::io::{AsyncRead, AsyncWrite};

pub mod quin;
pub mod s2n;
pub mod tcp;


pub trait Endpoint: 'static {
    fn accept(&self) -> fusen_common::FusenFuture<Result<impl Connection, BoxError>>;

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> fusen_common::FusenFuture<Result<impl Connection, BoxError>>;
}

pub trait Connection: 'static + Debug + Send + Sync {
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