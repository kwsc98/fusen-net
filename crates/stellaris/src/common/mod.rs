// SPDX-License-Identifier: Apache-2.0 OR MIT

use tokio::io::{AsyncRead, AsyncWrite};

use crate::quic::StreamStop;

pub trait ReadStream: AsyncRead + Send + Sync + StreamStop + std::marker::Unpin + 'static {}
pub trait WriteStream:
    AsyncWrite + Send + Sync + StreamStop + std::marker::Unpin + 'static
{
}
