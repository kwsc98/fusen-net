use super::{Connection, Endpoint};
use fusen_common::BoxError;
use futures::lock::Mutex;
use s2n_quic::provider::limits::Limits;
use s2n_quic::{
    client::Connect,
    connection::{Handle, StreamAcceptor},
    Client, Server,
};
use std::time::Duration;
use std::{net::SocketAddr, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::{error, info};

pub fn get_server(cert: &str, priv_key: &str, port: u16) -> Result<Server, BoxError> {
    let limits = Limits::new()
        .with_max_open_local_bidirectional_streams(1000)?
        .with_max_open_remote_bidirectional_streams(1000)?
        .with_max_idle_timeout(Duration::from_secs(5))?
        .with_max_keep_alive_period(Duration::from_secs(1))?;
    let server = Server::builder()
        .with_tls((cert, priv_key))?
        .with_io(format!("0.0.0.0:{}", port).as_str())?
        .with_limits(limits)?
        .start()?;
    Ok(server)
}

pub fn get_client(cert: &str) -> Result<Client, BoxError> {
    let limits = Limits::new()
        .with_max_open_local_bidirectional_streams(1000)?
        .with_max_open_remote_bidirectional_streams(1000)?
        .with_max_idle_timeout(Duration::from_secs(5))?
        .with_max_keep_alive_period(Duration::from_secs(1))?;
    let client = Client::builder()
        .with_tls(cert)?
        .with_io("0.0.0.0:0")?
        .with_limits(limits)?
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
    ) -> Result<impl Endpoint, BoxError> {
        let server = get_server(cert, prik, bind_port)?;
        let endpoint = S2nEndpoint {
            endpoint: Arc::new(S2nEndpointInfo::Server(Mutex::new(server))),
        };
        Ok(endpoint)
    }

    pub fn make_client_endpoint(cert: &str) -> Result<impl Endpoint + 'static, BoxError> {
        let client = get_client(cert)?;
        let endpoint = S2nEndpoint {
            endpoint: Arc::new(S2nEndpointInfo::Client(client)),
        };
        Ok(endpoint)
    }
}

impl Endpoint for S2nEndpoint {
    fn accept(&self) -> fusen_common::FusenFuture<Result<impl Connection, BoxError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            match endpoint.as_ref() {
                S2nEndpointInfo::Server(server) => {
                    let mut server = server.lock().await;
                    let mut connection = server.accept().await.ok_or("Connection is none !")?;
                    let _ = connection.keep_alive(true);
                    let (handle, acceptor) = connection.split();
                    Ok(S2nConnect {
                        handle,
                        acceptor: Arc::new(Mutex::new(acceptor)),
                    })
                }
                S2nEndpointInfo::Client(_client) => {
                    let info: &str = "Client cant accept !";
                    error!(info);
                    Err(info.into())
                }
            }
        })
    }

    fn connect(
        &self,
        addr: SocketAddr,
        server_name: String,
    ) -> fusen_common::FusenFuture<Result<impl Connection, BoxError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            match endpoint.as_ref() {
                S2nEndpointInfo::Server(_server) => {
                    let info: &str = "Server cant connect !";
                    error!(info);
                    Err(info.into())
                }
                S2nEndpointInfo::Client(client) => {
                    let mut connection = client
                        .connect(Connect::new(addr).with_server_name(server_name))
                        .await?;
                    let _ = connection.keep_alive(true);
                    let (handle, acceptor) = connection.split();
                    Ok(S2nConnect {
                        handle,
                        acceptor: Arc::new(Mutex::new(acceptor)),
                    })
                }
            }
        })
    }
}

#[derive(Debug)]
pub struct S2nConnect {
    handle: Handle,
    acceptor: Arc<Mutex<StreamAcceptor>>,
}

impl Connection for S2nConnect {
    fn open_bi(
        &self,
    ) -> fusen_common::FusenFuture<
        Result<(impl AsyncRead + 'static, impl AsyncWrite + 'static), BoxError>,
    > {
        let mut connect = self.handle.clone();
        Box::pin(async move {
            let (recv_stream, send_stream) = connect.open_bidirectional_stream().await?.split();
            Ok((recv_stream, send_stream))
        })
    }

    fn accept_bi(
        &self,
    ) -> fusen_common::FusenFuture<
        Result<(impl AsyncRead + 'static, impl AsyncWrite + 'static), BoxError>,
    > {
        let acceptor = self.acceptor.clone();
        Box::pin(async move {
            let mut connect = acceptor.lock().await;
            let (recv_stream, send_stream) = connect
                .accept_bidirectional_stream()
                .await?
                .ok_or("bistream is none !")?
                .split();
            Ok((recv_stream, send_stream))
        })
    }

    fn remote_address(&self) -> SocketAddr {
        self.handle.remote_addr().unwrap()
    }

    fn closed(&self) -> fusen_common::FusenFuture<BoxError> {
        let mut connect = self.handle.clone();
        Box::pin(async move {
            if let Ok(stream) = connect.open_bidirectional_stream().await {
                let (mut recv_stream, _send_stream) = stream.split();
                let result = recv_stream.receive().await;
                info!("closed result : {:?}", result);
            }
            "connect close !".into()
        })
    }
}
