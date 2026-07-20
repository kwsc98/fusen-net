// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{
    common::{self},
    error::FusenNetError,
};
use bytes::Bytes;
use futures::future::BoxFuture;
use std::net::SocketAddr;

#[cfg(feature = "backend-gm-quic")]
pub mod gm_quic;
#[cfg(feature = "backend-quinn")]
pub mod quin;
#[cfg(feature = "backend-s2n")]
pub mod s2n;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Quiclib {
    GmQuic,
    Quin,
    S2n,
}

pub trait StreamStop {
    fn steam_stop(&mut self);
}

pub trait Endpoint: 'static {
    fn accept(&self) -> BoxFuture<'_, Result<impl Connection, FusenNetError>>;

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<'_, Result<impl Connection, FusenNetError>>;
}

pub trait Connection: Sync + Send + 'static {
    fn open_bi(
        &self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>;

    fn accept_bi(
        &mut self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>;

    fn send_datagram(&self, bytes: Bytes) -> Result<(), FusenNetError>;

    fn recv_datagram(&mut self) -> BoxFuture<'_, Result<Bytes, FusenNetError>>;

    fn dropped_incoming_datagrams(&self) -> Result<u64, FusenNetError> {
        Ok(0)
    }

    fn remote_address(&self) -> SocketAddr;

    fn closed(&self) -> BoxFuture<'_, FusenNetError>;
}
