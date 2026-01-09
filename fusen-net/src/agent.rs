use std::{net::SocketAddr, sync::Arc};

use crate::{
    buffer::{self, DEFAULT_BUF_SIZE, StreamBuffer},
    error::FusenNetError,
    frame::{Frame, Register},
    quic::{Connection, Endpoint, Quiclib, quin::QuinnEndpoint, s2n::S2nEndpoint},
};
use bytes::Bytes;
use gm_quic::qinterface::local;
use s2n_quic::{client::Connect, provider::connection_id};
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tracing::{debug, error, info};
use tun::{DEFAULT_MTU, PACKET_INFORMATION_LENGTH};

#[derive(Clone)]
pub struct Agent;

pub struct AgentConfig {
    pub quic_lib: Quiclib,
    pub server_addr: String,
    pub server_name: String,
    pub cert_pem: String,
    pub authentication: String,
}

impl Agent {
    pub async fn run(&self, config: AgentConfig) -> Result<(), FusenNetError> {
        let cert_pem = config.cert_pem.clone();
        match &config.quic_lib {
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
                Self::handler(
                    config,
                    QuinnEndpoint::make_client_endpoint(cert_pem.as_str())?,
                )
                .await
            }
            Quiclib::S2n => {
                Self::handler(
                    config,
                    S2nEndpoint::make_client_endpoint(cert_pem.as_str())?,
                )
                .await
            }
        }
    }

    async fn handler(config: AgentConfig, endpoint: impl Endpoint) -> Result<(), FusenNetError> {
        //初始化流量网关
        let connection = endpoint
            .connect(
                config
                    .server_addr
                    .parse()
                    .map_err(|error| FusenNetError::BoxError(Box::new(error)))?,
                config.server_name,
            )
            .await?;
        let connection = connection;
        let (recv_stream, send_stream) = connection.open_bi().await?;
        let mut buffer = StreamBuffer::new(recv_stream, send_stream, DEFAULT_BUF_SIZE);
        let _ = buffer
            .write_frame(&Frame::Register(Register {
                authentication: config.authentication,
            }))
            .await
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
        let frame = buffer
            .read_frame()
            .await
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
        let Frame::RegisterResponse(response) = frame else {
            return Err(FusenNetError::ConnectClose);
        };
        let Some(local_addr) = response.local_addr else {
            //开启run网卡
            let _ = buffer
                .write_frame(&Frame::Ack)
                .await
                .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
            return Err(FusenNetError::ConnectClose);
        };
        let (connect_sender, connect_recv) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
        let tun_sender = license_tun(local_addr, connect_sender)?;
        //开启run网卡
        let _ = buffer
            .write_frame(&Frame::Ack)
            .await
            .map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
        router(connection, connect_recv, tun_sender).await
    }
}

enum TunFrame {
    Tun(Option<Bytes>),
    Read(Result<usize, io::Error>),
}

fn license_tun(
    local_addr: String,
    connect_sender: tokio::sync::mpsc::UnboundedSender<Bytes>,
) -> Result<tokio::sync::mpsc::UnboundedSender<Bytes>, FusenNetError> {
    let (tun_sender, mut tun_recv) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let mut config = tun::Configuration::default();
    config
        .address(local_addr)
        .netmask((255, 255, 255, 0))
        .destination((10, 0, 0, 1))
        .mtu(DEFAULT_MTU)
        .up();
    #[cfg(target_os = "linux")]
    config.platform_config(|config| {
        #[allow(deprecated)]
        config.packet_information(true);
        config.ensure_root_privileges(true);
    });
    #[cfg(target_os = "windows")]
    config.platform_config(|config| {
        config.device_guid(9099482345783245345345_u128);
    });
    let mut dev =
        tun::create_as_async(&config).map_err(|error| FusenNetError::BoxError(Box::new(error)))?;
    tokio::spawn(async move {
        let mut data = [0; (DEFAULT_MTU as usize) + PACKET_INFORMATION_LENGTH];
        loop {
            let frame = tokio::select! {
                recv = tun_recv.recv() => {
                    TunFrame::Tun(recv)
                },
                size = dev.read(&mut data) => {
                    TunFrame::Read(size)
                }
            };
            if let TunFrame::Read(Ok(size)) = frame {
                let bytes = Bytes::copy_from_slice(&data[..size]);
                let _ = connect_sender.send(bytes);
            }
            if let TunFrame::Tun(Some(bytes)) = frame {
                let _ = dev.write(&bytes).await;
            }
        }
    });
    Ok(tun_sender)
}

enum RouterFrame {
    QuicConnect(Result<Bytes, FusenNetError>),
    Read(Option<Bytes>),
}

async fn router(
    connect: impl Connection,
    mut connect_recv: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
    tun_sender: tokio::sync::mpsc::UnboundedSender<Bytes>,
) -> Result<(), FusenNetError> {
    loop {
        let frame = tokio::select! {
            recv = connect.recv_datagram() => {
                RouterFrame::QuicConnect(recv)
            },
            bytes = connect_recv.recv() => {
                RouterFrame::Read(bytes)
            }
        };
        if let RouterFrame::QuicConnect(Ok(bytes)) = frame {
            let _ = tun_sender.send(bytes);
        } else if let RouterFrame::Read(Some(bytes)) = frame {
            let _ = connect.send_datagram(bytes);
        }
    }
}
