use crate::{
    common::{self},
    error::FusenNetError,
};
use bytes::Bytes;
use futures::future::BoxFuture;
use std::net::SocketAddr;

pub mod gm_quic;
pub mod quin;
pub mod s2n;

pub enum Quiclib {
    GmQuic,
    Quin,
    S2n,
}

pub trait StreamStop {
    fn steam_stop(&mut self);
}

pub trait Endpoint: 'static {
    fn accept(&self) -> BoxFuture<Result<impl Connection, FusenNetError>>;

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<Result<impl Connection, FusenNetError>>;
}

pub trait Connection: Sync + Send + 'static {
    fn open_bi(
        &self,
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>;

    fn accept_bi(
        &mut self,
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>;

    fn send_datagram(&self, bytes: Bytes) -> Result<(), FusenNetError>;

    fn recv_datagram(&mut self) -> BoxFuture<Result<Bytes, FusenNetError>>;

    fn remote_address(&self) -> SocketAddr;

    fn closed(&self) -> BoxFuture<FusenNetError>;
}
