use fusen_common::BoxError;
use futures::lock::Mutex;
use s2n_quic::{
    client::Connect,
    connection::{Handle, StreamAcceptor},
    Client, Server,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::{error, info};

use super::{Connection, EndPoint};

const CERT_PEM: &str = "-----BEGIN CERTIFICATE-----
MIICRTCCAeugAwIBAgIUC989yXgvAxWhnaTdCsk8JgYpvzkwCgYIKoZIzj0EAwIw
gYExCzAJBgNVBAYTAkpQMQ4wDAYDVQQIDAVDaGliYTETMBEGA1UEBwwKQ2hpYmEg
Q2l0eTEYMBYGA1UECgwPVGVzc2llci1Bc2hwb29sMRAwDgYDVQQDDAdsb2NhbGNh
MSEwHwYJKoZIhvcNAQkBFhJjYUBkZXZlbG9wLmxvY2FsY2EwIBcNMjQwMzIzMDAz
NDMxWhgPMjIwMzA4MjkwMDM0MzFaMIGBMQswCQYDVQQGEwJKUDEOMAwGA1UECAwF
Q2hpYmExEzARBgNVBAcMCkNoaWJhIENpdHkxGDAWBgNVBAoMD1Rlc3NpZXItQXNo
cG9vbDEQMA4GA1UEAwwHbG9jYWxjYTEhMB8GCSqGSIb3DQEJARYSY2FAZGV2ZWxv
cC5sb2NhbGNhMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEbrmwtR2bEj/hit5i
7Vkh1wl3UqAykQFN801EYZC93qUp7XW9OB0U9kMk5K67Qb7239oL678jwtgJdBeo
DHa6C6M9MDswOQYDVR0RBDIwMIIJbG9jYWxob3N0ggtxbGF3cy5xbGF3c4cEfwAA
AYcQAAAAAAAAAAAAAAAAAAAAATAKBggqhkjOPQQDAgNIADBFAiAFj6aDZVkJm5v+
/f1MW9JCaWSdgzREF8wXRy4cWqZp3gIhAKprkqZOpfU4m1PLMuOqoRvnqz/r77uN
6nK1RbKK1pbF
-----END CERTIFICATE-----";

const KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgRlCQqSxQrvgT3BU7
xHp9ymk5r0RY2jccZOom+64gEv6hRANCAARuubC1HZsSP+GK3mLtWSHXCXdSoDKR
AU3zTURhkL3epSntdb04HRT2QyTkrrtBvvbf2gvrvyPC2Al0F6gMdroL
-----END PRIVATE KEY-----";

pub fn get_server() -> Result<Server, BoxError> {
    let server = Server::builder()
        .with_tls((CERT_PEM, KEY_PEM))?
        .with_io("0.0.0.0:8089")?
        .start()?;
    Ok(server)
}

pub fn get_client() -> Result<Client, BoxError> {
    let client = Client::builder()
        .with_tls(CERT_PEM)?
        .with_io("0.0.0.0:0")?
        .start()?;
    Ok(client)
}

#[derive(Debug)]
pub enum S2nEndPointInfo {
    Server(Mutex<Server>),
    Client(Client),
}

#[derive(Debug)]
pub struct S2nEndPoint {
    pub endpoint: Arc<S2nEndPointInfo>,
}

impl EndPoint for S2nEndPoint {
    fn accept(&self) -> fusen_common::FusenFuture<Result<impl Connection, BoxError>> {
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            match endpoint.as_ref() {
                S2nEndPointInfo::Server(server) => {
                    let mut server = server.lock().await;
                    let mut connection = server.accept().await.ok_or("Connection is none !")?;
                    let _ = connection.keep_alive(true);
                    let (handle, acceptor) = connection.split();
                    Ok(S2nConnect {
                        handle,
                        acceptor: Arc::new(Mutex::new(acceptor)),
                    })
                }
                S2nEndPointInfo::Client(_client) => {
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
                S2nEndPointInfo::Server(_server) => {
                    let info: &str = "Server cant connect !";
                    error!(info);
                    Err(info.into())
                }
                S2nEndPointInfo::Client(client) => {
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
