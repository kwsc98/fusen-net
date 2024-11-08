use crate::buffer::QuicBuffer;
use quinn::{ClientConfig, Endpoint};
use std::{net::SocketAddr, sync::Arc};
use support::make_client_endpoint;
pub mod support;

pub async fn connect(target_host: SocketAddr) -> Result<(QuicBuffer, SocketAddr), crate::Error> {
    let mut endpoint = make_client_endpoint("0.0.0.0:0".parse()?, &[])?;
    let local_addr = endpoint.local_addr().unwrap();
    let connection = endpoint.connect(target_host, "fusen-net")?.await?;
    let (send_stream, recv_stream) = connection.open_bi().await?;
    let buffer = QuicBuffer::new(send_stream, recv_stream);
    Ok((buffer, local_addr))
}

pub async fn connect_reuse(
    endpoint: &mut Endpoint,
    target_host: SocketAddr,
) -> Result<(QuicBuffer, SocketAddr), crate::Error> {
    let local_addr = endpoint.local_addr().unwrap();
    let connection = endpoint.connect(target_host, "fusen-net")?.await?;
    let (send_stream, recv_stream) = connection.open_bi().await?;
    let buffer = QuicBuffer::new(send_stream, recv_stream);
    Ok((buffer, local_addr))
}