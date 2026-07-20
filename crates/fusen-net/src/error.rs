// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{error::Error, io};

#[derive(Debug, thiserror::Error)]
pub enum FusenNetError {
    #[error("box_error : {0}")]
    BoxError(Box<dyn Error + 'static + Sync + Send>),

    #[cfg(feature = "backend-quinn")]
    #[error("quinn_connection_error : {0}")]
    QuinnConnectionError(quinn::ConnectionError),

    #[cfg(feature = "backend-quinn")]
    #[error("quinn_connect_error : {0}")]
    QuinnConnecError(quinn::ConnectError),

    #[cfg(feature = "backend-s2n")]
    #[error("s2n_quic_connect_error : {0}")]
    S2nConnectError(s2n_quic::connection::Error),

    #[error("gm_quic_connect_error : {0}")]
    GmQuicConnectError(io::Error),

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
impl From<quinn::ConnectionError> for FusenNetError {
    fn from(value: quinn::ConnectionError) -> Self {
        FusenNetError::QuinnConnectionError(value)
    }
}

#[cfg(feature = "backend-quinn")]
impl From<quinn::ConnectError> for FusenNetError {
    fn from(value: quinn::ConnectError) -> Self {
        FusenNetError::QuinnConnecError(value)
    }
}

#[cfg(feature = "backend-s2n")]
impl From<s2n_quic::connection::Error> for FusenNetError {
    fn from(value: s2n_quic::connection::Error) -> Self {
        FusenNetError::S2nConnectError(value)
    }
}
