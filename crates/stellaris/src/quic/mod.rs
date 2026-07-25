// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{
    common::{self},
    error::StellarisError,
};
use bytes::Bytes;
use futures::future::BoxFuture;
use std::net::SocketAddr;

#[cfg(feature = "backend-quinn")]
pub mod quin;

pub trait StreamStop {
    fn steam_stop(&mut self);
}

pub trait Endpoint: 'static {
    fn accept(&self) -> BoxFuture<'_, Result<impl Connection, StellarisError>>;

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<'_, Result<impl Connection, StellarisError>>;
}

pub trait Connection: Sync + Send + 'static {
    fn open_bi(
        &self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), StellarisError>>;

    fn accept_bi(
        &mut self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), StellarisError>>;

    fn send_datagram(&self, bytes: Bytes) -> Result<(), StellarisError>;

    fn recv_datagram(&mut self) -> BoxFuture<'_, Result<Bytes, StellarisError>>;

    fn dropped_incoming_datagrams(&self) -> Result<u64, StellarisError> {
        Ok(0)
    }

    fn remote_address(&self) -> SocketAddr;

    fn closed(&self) -> BoxFuture<'_, StellarisError>;
}
