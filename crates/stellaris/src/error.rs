// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::error::Error;

#[derive(Debug, thiserror::Error)]
pub enum StellarisError {
    #[error("box_error : {0}")]
    BoxError(Box<dyn Error + 'static + Sync + Send>),

    #[cfg(feature = "backend-quinn")]
    #[error("quinn_connection_error : {0}")]
    QuinnConnectionError(quinn::ConnectionError),

    #[cfg(feature = "backend-quinn")]
    #[error("quinn_connect_error : {0}")]
    QuinnConnecError(quinn::ConnectError),

    #[error("endpoint close !")]
    EndpointClose,

    #[error("connect close !")]
    ConnectClose,

    #[error("QUIC backend '{0}' is not enabled")]
    BackendUnavailable(&'static str),

    #[error("operation is not valid for {0} endpoint")]
    InvalidEndpointRole(&'static str),

    #[error("datagram send queue is full")]
    DatagramQueueFull,

    #[error("datagram exceeds the peer transport limit")]
    DatagramTooLarge,
}

#[cfg(feature = "backend-quinn")]
impl From<quinn::ConnectionError> for StellarisError {
    fn from(value: quinn::ConnectionError) -> Self {
        StellarisError::QuinnConnectionError(value)
    }
}

#[cfg(feature = "backend-quinn")]
impl From<quinn::ConnectError> for StellarisError {
    fn from(value: quinn::ConnectError) -> Self {
        StellarisError::QuinnConnecError(value)
    }
}
