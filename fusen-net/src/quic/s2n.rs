use super::{Connection, Endpoint as MyEndpoint, StreamStop};
use crate::common::{self};
use crate::error::FusenNetError;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::lock::Mutex;
use s2n_quic::application::Error;
use s2n_quic::provider::datagram::default::{Endpoint, Receiver, Sender};
use s2n_quic::provider::limits::Limits;
use s2n_quic::stream::{ReceiveStream, SendStream};
use s2n_quic::{Client, Server, client::Connect};
use std::task::Poll;
use std::time::Duration;
use std::{net::SocketAddr, sync::Arc};

fn get_server(
    cert: &str,
    priv_key: &str,
    port: u16,
) -> Result<Server, Box<dyn std::error::Error + 'static + Sync + Send>> {
    let limits = Limits::new()
        .with_max_open_local_bidirectional_streams(1000)?
        .with_max_open_remote_bidirectional_streams(1000)?
        .with_max_idle_timeout(Duration::from_secs(60))?
        .with_max_keep_alive_period(Duration::from_secs(1))?;
    let datagram_provider = Endpoint::builder()
        .with_send_capacity(32 * 1024 * 10)?
        .with_recv_capacity(32 * 1024 * 10)?
        .build()
        .unwrap();
    let server = Server::builder()
        .with_tls((cert, priv_key))?
        .with_io(format!("0.0.0.0:{}", port).as_str())?
        .with_limits(limits)?
        .with_datagram(datagram_provider)?
        .start()?;
    Ok(server)
}

fn get_client(cert: &str) -> Result<Client, Box<dyn std::error::Error + 'static + Sync + Send>> {
    let limits = Limits::new()
        .with_max_open_local_bidirectional_streams(1000)?
        .with_max_open_remote_bidirectional_streams(1000)?
        .with_max_idle_timeout(Duration::from_secs(60))?
        .with_max_keep_alive_period(Duration::from_secs(1))?;
    let datagram_provider = Endpoint::builder()
        .with_send_capacity(32 * 1024 * 10)?
        .with_recv_capacity(32 * 1024 * 10)?
        .build()
        .unwrap();
    let client = Client::builder()
        .with_tls(cert)?
        .with_io("0.0.0.0:0")?
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
        let server =
            get_server(cert, prik, bind_port).map_err(|error| FusenNetError::BoxError(error))?;
        let endpoint = S2nEndpoint {
            endpoint: Arc::new(S2nEndpointInfo::Server(Mutex::new(server))),
        };
        Ok(endpoint)
    }

    pub fn make_client_endpoint(cert: &str) -> Result<impl MyEndpoint + 'static, FusenNetError> {
        let client = get_client(cert).map_err(|error| FusenNetError::BoxError(error))?;
        let endpoint = S2nEndpoint {
            endpoint: Arc::new(S2nEndpointInfo::Client(client)),
        };
        Ok(endpoint)
    }
}

impl MyEndpoint for S2nEndpoint {
    fn accept(&self) -> BoxFuture<Result<impl Connection, FusenNetError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            match endpoint.as_ref() {
                S2nEndpointInfo::Server(server) => {
                    let mut server = server.lock().await;
                    let mut connection =
                        server.accept().await.ok_or(FusenNetError::EndpointClose)?;
                    let _ = connection.keep_alive(true);
                    Ok(S2nConnect { connection })
                }
                S2nEndpointInfo::Client(_client) => {
                    panic!("client cant accept !")
                }
            }
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> BoxFuture<Result<impl Connection, FusenNetError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            match endpoint.as_ref() {
                S2nEndpointInfo::Server(_server) => {
                    panic!("server cant connect !")
                }
                S2nEndpointInfo::Client(client) => {
                    let mut connection = client
                        .connect(Connect::new(addr).with_server_name(server_name))
                        .await?;
                    let _ = connection.keep_alive(true);
                    Ok(S2nConnect { connection })
                }
            }
        })
    }
}

#[derive(Debug)]
pub struct S2nConnect {
    connection: s2n_quic::connection::Connection,
}

impl StreamStop for ReceiveStream {
    fn steam_stop(&mut self) {
        let _ = self.stop_sending(s2n_quic::application::Error::new(0x100).unwrap());
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
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>> {
        let mut handle = self.connection.handle();
        Box::pin(async move {
            let (recv_stream, send_stream) = handle.open_bidirectional_stream().await?.split();
            Ok((recv_stream, send_stream))
        })
    }

    fn accept_bi(
        &mut self,
    ) -> BoxFuture<Result<(impl common::ReadStream, impl common::WriteStream), FusenNetError>> {
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
        let send_func = |x: &mut Sender| match x.send_datagram(bytes) {
            Ok(_) => {}
            Err(_err) => {}
        };
        self.connection
            .datagram_mut(send_func)
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))
    }

    fn recv_datagram(&mut self) -> BoxFuture<Result<Bytes, FusenNetError>> {
        Box::pin(async move {
            let recv_result = futures::future::poll_fn(|cx| {
                // datagram_mut takes a closure which calls the requested datagram function. The type
                // of the closure parameter should be either the datagram Sender type or the
                // datagram Receiver type. The datagram_mut function will check this type against
                // its stored datagram Sender and Receiver, and if the type matches, the requested
                // function will execute. Here, that requested function is poll_recv_datagram.
                match self
                    .connection
                    .datagram_mut(|recv: &mut Receiver| recv.poll_recv_datagram(cx))
                {
                    // If the function is successfully called on the provider, it will return Poll<Bytes>.
                    // Here we send an Ok() to wrap around the Bytes so the poll_fn doesn't complain.
                    Ok(poll_value) => poll_value.map(Ok),
                    // The datagram_mut function may return a query error if it can't find the type
                    // referenced in the closure. Here we wrap the error in a Poll::Ready enum so the
                    // poll_fn doesn't complain.
                    Err(query_err) => Poll::Ready(Err(query_err)),
                }
            })
            .await;
            match recv_result {
                Ok(result) => result.map_err(|error| FusenNetError::ConnectClose),
                Err(error) => Err(FusenNetError::BoxError(Box::new(error))),
            }
        })
    }

    fn remote_address(&self) -> SocketAddr {
        self.connection.remote_addr().unwrap()
    }

    fn closed(&self) -> BoxFuture<FusenNetError> {
        Box::pin(async move {
            self.connection.close(Error::new(0).unwrap());
            FusenNetError::ConnectClose
        })
    }
}
