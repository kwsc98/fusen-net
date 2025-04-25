use crate::common::{self, ConnectError};
use futures::future::BoxFuture;
use std::net::SocketAddr;

pub mod gm_quic;
pub mod quin;
pub mod s2n;

pub trait StreamStop {
    fn stop(&mut self);
}

pub trait Endpoint: 'static {
    fn accept(&self) -> BoxFuture<Result<impl Connection, ConnectError>>;

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<Result<impl Connection, ConnectError>>;
}

pub trait Connection: Sync + Send + 'static {
    fn open_bi(
        &self,
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), ConnectError>>;

    fn accept_bi(
        &self,
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), ConnectError>>;

    fn remote_address(&self) -> SocketAddr;

    fn closed(&self) -> BoxFuture<ConnectError>;
}
