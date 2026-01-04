use tokio::io::{AsyncRead, AsyncWrite};

use crate::quic::StreamStop;

pub mod shutdown;
pub mod token;

pub trait ReadStream: AsyncRead + Send + Sync + StreamStop + std::marker::Unpin + 'static {}
pub trait WriteStream:
    AsyncWrite + Send + Sync + StreamStop + std::marker::Unpin + 'static
{
}
