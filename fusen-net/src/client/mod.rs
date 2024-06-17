use crate::buffer::{QuicBuffer, TcpBuffer};
use crate::common::get_uuid;
use crate::frame::{ConnectionInfo, Frame, RegisterInfo, SubscribeInfo};
use crate::quic::support::make_server_endpoint;
use crate::server::cache::AsyncCache;
use crate::{connection, quic};
use quinn::Endpoint;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

#[derive(Clone)]
pub struct AgentInfo {
    pub agent_mode: AgentMode,
    pub target_tag: String,
    pub target_host: String,
    pub agent_port: String,
}

impl From<&str> for AgentInfo {
    fn from(value: &str) -> Self {
        let agent_info: Vec<&str> = value.split('-').collect();
        AgentInfo {
            agent_mode: agent_info[0].into(),
            target_tag: agent_info[1].to_owned(),
            target_host: agent_info[2].to_owned(),
            agent_port: agent_info[3].to_owned(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AgentMode {
    DM,
    RM,
}

impl Into<AgentMode> for &str {
    fn into(self) -> AgentMode {
        if self.to_uppercase().contains("DM") {
            AgentMode::DM
        } else {
            AgentMode::RM
        }
    }
}

pub async fn register(register_addr: String, tag: String) -> Result<(), crate::Error> {
    let mut server_endpoint = make_server_endpoint(format!("0.0.0.0:0").parse().unwrap())
        .unwrap()
        .0;
    let host: SocketAddr = register_addr.parse().unwrap();
    let (mut quic_buffer, _local_addr) = quic::connect_reuse(&mut server_endpoint, host).await?;
    let _ = quic_buffer
        .write_frame(&Frame::Register(RegisterInfo::new(register_addr, tag)))
        .await;
    let _frame = tokio::select! {
        res = quic_buffer.read_frame() => res?,
        _ = tokio::time::sleep(Duration::from_secs(3)) => return Err("register time out".into()),
    };
    while let Some(connecting) = server_endpoint.accept().await {
        tokio::spawn(async move {
            let Ok(connection) = connecting.await else {
                return;
            };
            let (send_stream, recv_stream) = connection
                .accept_bi()
                .await
                .expect("connection accept_bi err");
            let mut quic_buffer = QuicBuffer::new(send_stream, recv_stream);
            while let Ok(frame) = quic_buffer.read_frame().await {
                match frame {
                    Frame::Ping => {
                        let _ = quic_buffer.write_frame(&Frame::Ack).await;
                    }
                    Frame::Connection(connection) => {
                        match connection.get_agent_mode() {
                            AgentMode::DM => {
                                info!("start dm connection : {:?}", connection);
                                let tcp_stream = TcpStream::connect(connection.get_target_host())
                                    .await
                                    .unwrap();
                                let tcp_buffer = TcpBuffer::new(tcp_stream);
                                let _ = quic_buffer
                                    .write_frame(&Frame::TargetConnection(connection))
                                    .await;
                                let _ =
                                    connection::connect_tcp_to_quic(tcp_buffer, quic_buffer).await;
                                return;
                            }
                            AgentMode::RM => tokio::spawn(async move {
                                info!("start rm connection : {:?}", connection);
                                let tcp_stream = TcpStream::connect(connection.get_target_host())
                                    .await
                                    .unwrap();
                                let buffer = TcpBuffer::new(tcp_stream);
                                let (mut quic_buffer, _) = quic::connect(host).await.unwrap();
                                let _ = quic_buffer
                                    .write_frame(&Frame::TargetConnection(connection))
                                    .await;
                                let _frame = tokio::select! {
                                    res = quic_buffer.read_frame() => res.unwrap(),
                                    _ = tokio::time::sleep(Duration::from_secs(3)) => panic!("connect time out"),
                                };
                                let _ = connection::connect_tcp_to_quic(buffer, quic_buffer).await;
                            }),
                        };
                    }
                    frame => info!("rev not support frame : {:?}", frame),
                }
            }
        });
    }
    Ok(())
}

pub async fn agent(register_addr: String, agent_info: AgentInfo) -> Result<(), crate::Error> {
    match &agent_info.agent_mode {
        AgentMode::DM => dm_handler(register_addr, agent_info).await,
        AgentMode::RM => rm_handler(register_addr, agent_info).await,
    }
}

async fn dm_handler(register_addr: String, agent_info: AgentInfo) -> Result<(), crate::Error> {
    let async_cache: AsyncCache<String, String> = AsyncCache::new();
    let register_addr_clone = register_addr.clone();
    let host: SocketAddr = register_addr_clone.parse().unwrap();
    let (mut quic_buffer, _) = quic::connect(host).await.expect("udp connect error");
    let _ = quic_buffer
        .write_frame(&&Frame::Subscribe(SubscribeInfo::new(
            agent_info.target_tag.clone(),
        )))
        .await;
    let Ok(Frame::Subscribe(subscribe_info)) =
        quic_buffer.read_frame_wait(Duration::from_secs(3)).await
    else {
        return Err("subscribe time out".into());
    };
    if let Some(addr) = subscribe_info.get_target_sockeraddr() {
        let _ = async_cache
            .insert(subscribe_info.get_target_tag().to_string(), addr.to_owned())
            .await;
    }
    let async_cache_clone = async_cache.clone();
    tokio::spawn(async move {
        while let Ok(frame) = quic_buffer.read_frame().await {
            if let Frame::Subscribe(subscribe_info) = frame {
                if let Some(addr) = subscribe_info.get_target_sockeraddr() {
                    let _ = async_cache_clone
                        .insert(subscribe_info.get_target_tag().to_string(), addr.to_owned())
                        .await;
                }
            }
        }
    });
    let listener = TcpListener::bind(&format!("0.0.0.0:{}", agent_info.agent_port)).await?;
    while let Ok(tcp_stream) = listener.accept().await {
        let agent_info = agent_info.clone();
        let async_cache_clone = async_cache.clone();
        tokio::spawn(async move {
            let tcp_buffer = TcpBuffer::new(tcp_stream.0);
            let Ok(Some(addr)) = async_cache_clone.get(agent_info.target_tag.clone()).await else {
                info!("not find addr");
                return;
            };
            let host: SocketAddr = addr.parse().unwrap();
            info!("{:?}", host);
            let (mut quic_buffer, _) = quic::connect(host).await.expect("udp connect error");
            let _ = quic_buffer
                .write_frame(&Frame::Connection(ConnectionInfo::new(
                    AgentMode::DM,
                    get_uuid(),
                    agent_info.target_tag,
                    agent_info.target_host,
                )))
                .await;
            if let Ok(Frame::TargetConnection(_connection)) = quic_buffer.read_frame().await {
                let _ = connection::connect_tcp_to_quic(tcp_buffer, quic_buffer).await;
            }
        });
    }
    Ok(())
}

async fn rm_handler(register_addr: String, agent_info: AgentInfo) -> Result<(), crate::Error> {
    let listener = TcpListener::bind(&format!("0.0.0.0:{}", agent_info.agent_port)).await?;
    while let Ok(tcp_stream) = listener.accept().await {
        let tcp_buffer = TcpBuffer::new(tcp_stream.0);
        let agent_info = agent_info.clone();
        let register_addr = register_addr.clone();
        tokio::spawn(async move {
            let host: SocketAddr = register_addr.parse().unwrap();
            let (mut quic_buffer, _) = quic::connect(host).await.expect("udp connect error");
            let _ = quic_buffer
                .write_frame(&Frame::Connection(ConnectionInfo::new(
                    AgentMode::RM,
                    get_uuid(),
                    agent_info.target_tag,
                    agent_info.target_host,
                )))
                .await;
            let _frame = tokio::select! {
                res = quic_buffer.read_frame() => res.unwrap(),
                _ = tokio::time::sleep(Duration::from_secs(3)) => panic!("agent time out"),
            };
            let _ = connection::connect_tcp_to_quic(tcp_buffer, quic_buffer).await;
        });
    }
    Ok(())
}
