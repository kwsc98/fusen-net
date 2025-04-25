use std::{error::Error, io};

use tokio::io::{AsyncRead, AsyncWrite};

use crate::quic::StreamStop;

pub mod token;

pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("quinn_connection_error : {0}")]
    QuinnConnectionError(quinn::ConnectionError),

    #[error("quinn_connect_error : {0}")]
    QuinnConnecError(quinn::ConnectError),

    #[error("s2n_quic_connect_error : {0}")]
    S2nConnectError(s2n_quic::connection::Error),

    #[error("gm_quic_connect_error : {0}")]
    GmQuicConnectError(io::Error),

    #[error("endpoint close !")]
    EndpointClose,

    #[error("connect close !")]
    ConnectClose,
}

impl From<quinn::ConnectionError> for ConnectError {
    fn from(value: quinn::ConnectionError) -> Self {
        ConnectError::QuinnConnectionError(value)
    }
}

impl From<quinn::ConnectError> for ConnectError {
    fn from(value: quinn::ConnectError) -> Self {
        ConnectError::QuinnConnecError(value)
    }
}

impl From<s2n_quic::connection::Error> for ConnectError {
    fn from(value: s2n_quic::connection::Error) -> Self {
        ConnectError::S2nConnectError(value)
    }
}

pub trait ReadStream: AsyncRead + Send + Sync + StreamStop + std::marker::Unpin + 'static {}
pub trait WriteStream:
    AsyncWrite + Send + Sync + StreamStop + std::marker::Unpin + 'static
{
}
