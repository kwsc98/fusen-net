use bytes::Bytes;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::{
    buffer::{DEFAULT_BUF_SIZE, StreamBuffer},
    error::FusenNetError,
    frame::{Frame, Register, RegisterResponse},
    gateway::{self, init_gateway},
    quic::{Connection, Endpoint, Quiclib, quin::QuinnEndpoint, s2n::S2nEndpoint},
};

pub struct Server;

pub struct ServerConfig {
    pub port: u16,
    pub quic_lib: Quiclib,
    pub cert_pem: String,
    pub priv_key_pem: String,
}

impl Server {
    pub async fn run(config: ServerConfig) -> Result<(), FusenNetError> {
        let port = config.port;
        let cert_pem = config.cert_pem;
        let priv_key_pem = config.priv_key_pem;
        match config.quic_lib {
            Quiclib::GmQuic => {
                todo!()
                // Self::handler(GmQuicEndpoint::make_server_endpoint(
                //     port,
                //     cert_pem.as_str(),
                //     priv_key_pem.as_str(),
                // )?)
                // .await
            }
            Quiclib::Quin => {
                Self::handler(QuinnEndpoint::make_server_endpoint(
                    port,
                    cert_pem.as_str(),
                    priv_key_pem.as_str(),
                )?)
                .await
            }
            Quiclib::S2n => {
                Self::handler(S2nEndpoint::make_server_endpoint(
                    port,
                    cert_pem.as_str(),
                    priv_key_pem.as_str(),
                )?)
                .await
            }
        }
    }

    async fn handler(endpoint: impl Endpoint) -> Result<(), FusenNetError> {
        //初始化流量网关
        let register_sender = init_gateway().await?;
        while let Ok(connect) = endpoint.accept().await {
            let register_sender_clone = register_sender.clone();
            tokio::spawn(async move {
                let _ = connect_handler(connect, register_sender_clone).await;
            });
        }
        Ok(())
    }
}

async fn connect_handler(
    mut connect: impl Connection,
    sender: UnboundedSender<(gateway::Register, UnboundedReceiver<Bytes>)>,
) -> Result<(), FusenNetError> {
    let (read_stream, write_stream) = connect.accept_bi().await?;
    let mut buffer = StreamBuffer::new(read_stream, write_stream, DEFAULT_BUF_SIZE);
    let frame = buffer
        .read_frame()
        .await
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    let Frame::Register(registry) = frame else {
        return Err(FusenNetError::ConnectClose);
    };
    let tun_ip = registry.authentication.clone();
    let result = registry_handler(registry).await?;
    let register_response = RegisterResponse {
        local_addr: result.clone(),
    };
    buffer
        .write_frame(&Frame::RegisterResponse(register_response))
        .await
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    let frame = buffer
        .read_frame()
        .await
        .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    let Frame::Ack = frame else {
        return Err(FusenNetError::ConnectClose);
    };
    if result.is_none() {
        return Ok(());
    }
    handler(tun_ip, connect, sender).await
}

async fn registry_handler(registry: Register) -> Result<Option<String>, FusenNetError> {
    Ok(Some(registry.authentication))
}

enum BytesFrame {
    Connect(Result<bytes::Bytes, FusenNetError>),
    Router(Option<bytes::Bytes>),
}

async fn handler(
    tun_ip: String,
    connect: impl Connection,
    sender: UnboundedSender<(gateway::Register, UnboundedReceiver<Bytes>)>,
) -> Result<(), FusenNetError> {
    let (sender1, mut recv1) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let (sender2, recv2) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    sender.send((
        gateway::Register {
            tun_ip,
            pack_send: sender1,
        },
        recv2,
    ));
    loop {
        let frame = tokio::select! {
            bytes = connect.recv_datagram() => {
               BytesFrame::Connect(bytes)
            },
            bytes = recv1.recv() => {
               BytesFrame::Router(bytes)
            }
        };
        match frame {
            BytesFrame::Connect(bytes) => {
                if let Ok(bytes) = bytes {
                    sender2.send(bytes);
                } else {
                    break;
                }
            }
            BytesFrame::Router(bytes) => {
                if let Some(bytes) = bytes {
                    connect.send_datagram(bytes);
                } else {
                    break;
                }
            }
        }
    }
    Ok(())
}
