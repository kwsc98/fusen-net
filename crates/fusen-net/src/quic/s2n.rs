// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{Connection, Endpoint as MyEndpoint, StreamStop};
use crate::common::{self};
use crate::control::ALPN;
use crate::error::FusenNetError;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::lock::Mutex;
use s2n_quic::application::Error;
use s2n_quic::provider::datagram::{
    ConnectionInfo, Endpoint as DatagramEndpoint, PreConnectionInfo, ReceiveContext,
    Receiver as DatagramReceiver, default::Endpoint as DefaultDatagramEndpoint, default::Sender,
};
use s2n_quic::provider::limits::Limits;
use s2n_quic::provider::tls::default::{Client as TlsClient, Server as TlsServer};
use s2n_quic::stream::{ReceiveStream, SendStream};
use s2n_quic::{Client, Server, client::Connect};
use std::task::Poll;
use std::time::Duration;
use std::{
    collections::VecDeque,
    fmt::{Debug, Display},
    io,
    net::SocketAddr,
    sync::Arc,
    task::{Context, Waker},
};

const DATAGRAM_QUEUE_CAPACITY: usize = 256;

#[derive(Debug)]
struct DropNewDatagramEndpoint {
    sender: DefaultDatagramEndpoint,
}

impl DropNewDatagramEndpoint {
    fn new() -> Result<Self, Box<dyn std::error::Error + 'static + Sync + Send>> {
        let sender = DefaultDatagramEndpoint::builder()
            .with_send_capacity(DATAGRAM_QUEUE_CAPACITY)?
            .with_recv_capacity(1)?
            .build()?;
        Ok(Self { sender })
    }
}

impl DatagramEndpoint for DropNewDatagramEndpoint {
    type Sender = Sender;
    type Receiver = DropNewDatagramReceiver;

    fn create_connection(&mut self, info: &ConnectionInfo) -> (Self::Sender, Self::Receiver) {
        let (sender, _) = self.sender.create_connection(info);
        (sender, DropNewDatagramReceiver::new())
    }

    fn max_datagram_frame_size(&self, info: &PreConnectionInfo) -> u64 {
        self.sender.max_datagram_frame_size(info)
    }
}

#[derive(Debug)]
struct DropNewDatagramReceiver {
    queue: VecDeque<Bytes>,
    dropped_queue_full: u64,
    waker: Option<Waker>,
    closed: bool,
}

impl DropNewDatagramReceiver {
    fn new() -> Self {
        Self {
            queue: VecDeque::with_capacity(DATAGRAM_QUEUE_CAPACITY),
            dropped_queue_full: 0,
            waker: None,
            closed: false,
        }
    }

    fn enqueue(&mut self, datagram: &[u8]) {
        if self.queue.len() >= DATAGRAM_QUEUE_CAPACITY {
            self.dropped_queue_full = self.dropped_queue_full.saturating_add(1);
            return;
        }
        self.queue.push_back(Bytes::copy_from_slice(datagram));
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    fn poll_recv(&mut self, context: &mut Context<'_>) -> Poll<Result<Bytes, io::Error>> {
        if let Some(datagram) = self.queue.pop_front() {
            Poll::Ready(Ok(datagram))
        } else if self.closed {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "s2n-quic Datagram receiver is closed",
            )))
        } else {
            self.waker = Some(context.waker().clone());
            Poll::Pending
        }
    }
}

impl DatagramReceiver for DropNewDatagramReceiver {
    fn on_datagram(&mut self, _context: &ReceiveContext<'_>, datagram: &[u8]) {
        self.enqueue(datagram);
    }

    fn on_connection_error(&mut self, _error: s2n_quic::connection::Error) {
        self.closed = true;
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

fn get_server(
    cert: &str,
    priv_key: &str,
    bind_address: SocketAddr,
) -> Result<Server, Box<dyn std::error::Error + 'static + Sync + Send>> {
    let tls = TlsServer::builder()
        .with_application_protocols([ALPN])?
        .with_certificate(cert, priv_key)?
        .build()?;
    let limits = Limits::new()
        .with_max_open_local_bidirectional_streams(1)?
        .with_max_open_remote_bidirectional_streams(1)?
        .with_max_open_local_unidirectional_streams(0)?
        .with_max_open_remote_unidirectional_streams(0)?
        .with_max_idle_timeout(Duration::from_secs(60))?
        .with_max_keep_alive_period(Duration::from_secs(1))?;
    let datagram_provider = DropNewDatagramEndpoint::new()?;
    let server = Server::builder()
        .with_tls(tls)?
        .with_io(bind_address)?
        .with_limits(limits)?
        .with_datagram(datagram_provider)?
        .start()?;
    Ok(server)
}

fn get_client(
    cert: &str,
    bind_address: SocketAddr,
) -> Result<Client, Box<dyn std::error::Error + 'static + Sync + Send>> {
    let tls = TlsClient::builder()
        .with_application_protocols([ALPN])?
        .with_empty_trust_store()?
        .with_certificate(cert)?
        .build()?;
    let limits = Limits::new()
        .with_max_open_local_bidirectional_streams(1)?
        .with_max_open_remote_bidirectional_streams(1)?
        .with_max_open_local_unidirectional_streams(0)?
        .with_max_open_remote_unidirectional_streams(0)?
        .with_max_idle_timeout(Duration::from_secs(60))?
        .with_max_keep_alive_period(Duration::from_secs(1))?;
    let datagram_provider = DropNewDatagramEndpoint::new()?;
    let client = Client::builder()
        .with_tls(tls)?
        .with_io(bind_address)?
        .with_limits(limits)?
        .with_datagram(datagram_provider)?
        .start()?;
    Ok(client)
}

#[derive(Debug)]
pub enum S2nEndpointInfo {
    Server(Mutex<Server>),
    Client(Client),
}

#[derive(Debug)]
pub struct S2nEndpoint {
    pub endpoint: Arc<S2nEndpointInfo>,
}

impl S2nEndpoint {
    pub fn make_server_endpoint(
        bind_port: u16,
        cert: &str,
        prik: &str,
    ) -> Result<impl MyEndpoint, FusenNetError> {
        Self::make_server_endpoint_at(SocketAddr::from(([0, 0, 0, 0], bind_port)), cert, prik)
    }

    pub fn make_server_endpoint_at(
        bind_address: SocketAddr,
        cert: &str,
        prik: &str,
    ) -> Result<impl MyEndpoint, FusenNetError> {
        let server = get_server(cert, prik, bind_address).map_err(FusenNetError::BoxError)?;
        let endpoint = S2nEndpoint {
            endpoint: Arc::new(S2nEndpointInfo::Server(Mutex::new(server))),
        };
        Ok(endpoint)
    }

    pub fn make_client_endpoint(cert: &str) -> Result<impl MyEndpoint + 'static, FusenNetError> {
        Self::make_client_endpoint_at(SocketAddr::from(([0, 0, 0, 0], 0)), cert)
    }

    pub fn make_client_endpoint_at(
        bind_address: SocketAddr,
        cert: &str,
    ) -> Result<impl MyEndpoint + 'static, FusenNetError> {
        let client = get_client(cert, bind_address).map_err(FusenNetError::BoxError)?;
        let endpoint = S2nEndpoint {
            endpoint: Arc::new(S2nEndpointInfo::Client(client)),
        };
        Ok(endpoint)
    }
}

impl MyEndpoint for S2nEndpoint {
    fn accept(&self) -> BoxFuture<'_, Result<impl Connection, FusenNetError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            match endpoint.as_ref() {
                S2nEndpointInfo::Server(server) => {
                    let mut server = server.lock().await;
                    let mut connection =
                        server.accept().await.ok_or(FusenNetError::EndpointClose)?;
                    connection.keep_alive(true)?;
                    let remote_address = connection
                        .remote_addr()
                        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
                    Ok(S2nConnect {
                        connection,
                        remote_address,
                    })
                }
                S2nEndpointInfo::Client(_client) => {
                    Err(FusenNetError::InvalidEndpointRole("client"))
                }
            }
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<'_, Result<impl Connection, FusenNetError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            match endpoint.as_ref() {
                S2nEndpointInfo::Server(_server) => {
                    Err(FusenNetError::InvalidEndpointRole("server"))
                }
                S2nEndpointInfo::Client(client) => {
                    let mut connection = client
                        .connect(Connect::new(addr).with_server_name(server_name))
                        .await?;
                    connection.keep_alive(true)?;
                    let remote_address = connection
                        .remote_addr()
                        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
                    Ok(S2nConnect {
                        connection,
                        remote_address,
                    })
                }
            }
        })
    }
}

#[derive(Debug)]
pub struct S2nConnect {
    connection: s2n_quic::connection::Connection,
    remote_address: SocketAddr,
}

impl Drop for S2nConnect {
    fn drop(&mut self) {
        if let Ok(error) = Error::new(0) {
            self.connection.close(error);
        }
    }
}

impl StreamStop for ReceiveStream {
    fn steam_stop(&mut self) {
        if let Ok(error) = s2n_quic::application::Error::new(0x100) {
            let _ = self.stop_sending(error);
        }
    }
}

impl StreamStop for SendStream {
    fn steam_stop(&mut self) {
        let _ = self.finish();
    }
}

impl common::ReadStream for ReceiveStream {}
impl common::WriteStream for SendStream {}

impl Connection for S2nConnect {
    fn open_bi(
        &self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>
    {
        let mut handle = self.connection.handle();
        Box::pin(async move {
            let (recv_stream, send_stream) = handle.open_bidirectional_stream().await?.split();
            Ok((recv_stream, send_stream))
        })
    }

    fn accept_bi(
        &mut self,
    ) -> BoxFuture<'_, Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>>
    {
        Box::pin(async move {
            let (recv_stream, send_stream) = self
                .connection
                .accept_bidirectional_stream()
                .await?
                .ok_or(FusenNetError::EndpointClose)?
                .split();
            Ok((recv_stream, send_stream))
        })
    }

    fn send_datagram(&self, bytes: bytes::Bytes) -> Result<(), FusenNetError> {
        let result = self
            .connection
            .datagram_mut(|sender: &mut Sender| sender.send_datagram(bytes))
            .map_err(datagram_error)?;
        result.map_err(send_datagram_error)
    }

    fn recv_datagram(&mut self) -> BoxFuture<'_, Result<Bytes, FusenNetError>> {
        Box::pin(recv_s2n_datagram(&self.connection))
    }

    fn dropped_incoming_datagrams(&self) -> Result<u64, FusenNetError> {
        self.connection
            .datagram_mut(|receiver: &mut DropNewDatagramReceiver| receiver.dropped_queue_full)
            .map_err(datagram_error)
    }

    fn remote_address(&self) -> SocketAddr {
        self.remote_address
    }

    fn closed(&self) -> BoxFuture<'_, FusenNetError> {
        Box::pin(async move {
            loop {
                match recv_s2n_datagram(&self.connection).await {
                    Ok(_) => continue,
                    Err(error) => return error,
                }
            }
        })
    }
}

fn datagram_error(error: impl Debug) -> FusenNetError {
    FusenNetError::BoxError(Box::new(io::Error::other(format!("{error:?}"))))
}

async fn recv_s2n_datagram(
    connection: &s2n_quic::connection::Connection,
) -> Result<Bytes, FusenNetError> {
    let recv_result = futures::future::poll_fn(|cx| {
        match connection
            .datagram_mut(|receiver: &mut DropNewDatagramReceiver| receiver.poll_recv(cx))
        {
            Ok(poll_value) => poll_value.map(Ok),
            Err(query_error) => Poll::Ready(Err(query_error)),
        }
    })
    .await;
    match recv_result {
        Ok(result) => result.map_err(datagram_error),
        Err(error) => Err(datagram_error(error)),
    }
}

fn send_datagram_error<E: Debug + Display>(error: E) -> FusenNetError {
    // s2n-quic keeps DatagramError's non-exhaustive variants private across
    // the provider boundary; classify the stable Debug names without exposing
    // backend-specific error types to the transport contract.
    let debug = format!("{error:?}");
    if debug.contains("QueueAtCapacity") {
        FusenNetError::DatagramQueueFull
    } else if debug.contains("ExceedsPeerTransportLimits") {
        FusenNetError::DatagramTooLarge
    } else {
        datagram_error(error)
    }
}

#[cfg(test)]
mod datagram_queue_tests {
    use super::*;

    #[test]
    fn receive_queue_drops_new_datagram_and_counts_at_capacity() {
        let mut receiver = DropNewDatagramReceiver::new();
        for value in 0..DATAGRAM_QUEUE_CAPACITY {
            receiver.enqueue(&[value as u8]);
        }
        receiver.enqueue(b"new-datagram-must-drop");

        assert_eq!(receiver.queue.len(), DATAGRAM_QUEUE_CAPACITY);
        assert_eq!(receiver.queue.front(), Some(&Bytes::from_static(b"\0")));
        assert_eq!(receiver.dropped_queue_full, 1);
    }
}
