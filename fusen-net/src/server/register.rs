use std::{any, collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use crate::{
    buffer::{connect, Buffer, QuicBuffer},
    common::get_uuid,
    frame::{ConnectionInfo, Frame, RegisterInfo},
    utils::map::AsyncQuicBufferMap,
    ChannelInfo,
};
use bytes::BytesMut;
use fusen_common::{shutdown::Shutdown, BoxError};
use tokio::{
    net::UdpSocket,
    sync::{
        broadcast::{self, Sender},
        mpsc::{self, UnboundedSender},
        oneshot,
    },
};
use tracing::info;
use tracing::{debug, error};

pub async fn register(
    sender: UnboundedSender<Frame>,
    register_info: Arc<RegisterInfo>,
    async_map: AsyncQuicBufferMap,
) -> Result<ChannelInfo, BoxError> {
    //监听一个协议端口
    match *register_info.get_protocol() {
        0 => tcp_listener(sender, register_info, async_map).await,
        1 => udp_listener(sender, register_info, async_map).await,
        _ => Err("not support protocol".into()),
    }
}

async fn tcp_listener(
    sender: UnboundedSender<Frame>,
    register_info: Arc<RegisterInfo>,
    async_map: AsyncQuicBufferMap,
) -> Result<ChannelInfo, BoxError> {
    let port = register_info.get_remote_port().unwrap_or(0);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    let (s, _r) = broadcast::channel::<()>(1);
    let mut shutdown = Shutdown::new(s.subscribe());
    if let Err(error) = listener.local_addr() {
        return Err(format!("listener error : {:?}", error).into());
    }
    let channel_info = ChannelInfo::new(
        listener.local_addr().unwrap().port(),
        register_info.clone(),
        s,
    );
    tokio::spawn(async move {
        let (m_s, _) = broadcast::channel::<()>(1);
        loop {
            let stream = tokio::select! {
                stream = listener.accept() => stream,
                _ = shutdown.recv() => {
                    drop(m_s);
                    info!("tcp listener close ~");
                    return;
                }
            };
            let Ok((tcp_stream, _socket_addr)) = stream else {
                continue;
            };
            let async_map = async_map.clone();
            let sender = sender.clone();
            let _shutdown = Shutdown::new(m_s.subscribe());
            let target_host = register_info.get_target_host().to_owned();
            tokio::spawn(async move {
                let uuid = get_uuid();
                let (sendr, recv) = oneshot::channel::<QuicBuffer>();
                async_map.insert(uuid.clone(), sendr).await;
                let result: Result<(), mpsc::error::SendError<Frame>> =
                    sender.send(Frame::Connection(
                        ConnectionInfo::default()
                            .uuid(uuid.clone())
                            .target_host(target_host),
                    ));
                if let Err(error) = result {
                    error!("connection error : {:?}", error);
                }
                let quci_buffer = tokio::select! {
                    quic_buffer = recv => quic_buffer,
                    _ = tokio::time::sleep(Duration::from_millis(10000)) => {
                        error!("connection timeout : 10000");
                        let _ = async_map.remove(uuid).await;
                        return;
                    }
                };
                let quci_buffer = match quci_buffer {
                    Ok(quci_buffer) => quci_buffer,
                    Err(error) => {
                        error!("recv quci_buffer error : {:?}", error);
                        return;
                    }
                };
                let tcp_buffer = tcp_stream.into_split();
                let quic_buffer = quci_buffer.split();
                let result = connect(quic_buffer, tcp_buffer).await;
                debug!("connect close ~ : {:?}", result);
            });
        }
    });
    Ok(channel_info)
}

async fn udp_listener(
    sender: UnboundedSender<Frame>,
    register_info: Arc<RegisterInfo>,
    async_map: AsyncQuicBufferMap,
) -> Result<ChannelInfo, BoxError> {
    let port = register_info.get_remote_port().unwrap_or(0);
    let udp_socket = tokio::net::UdpSocket::bind(format!("0.0.0.0:{}", port)).await?;
    let (s, _r) = broadcast::channel::<()>(1);
    let mut shutdown = Shutdown::new(s.subscribe());
    if let Err(error) = udp_socket.local_addr() {
        return Err(format!("listener error : {:?}", error).into());
    }
    let channel_info = ChannelInfo::new(
        udp_socket.local_addr().unwrap().port(),
        register_info.clone(),
        s,
    );
    tokio::spawn(async move {
        let (m_s, _) = broadcast::channel::<()>(1);
        let socket_map = HashMap::<SocketAddr, Sender<BytesMut>>::new();
        loop {
            let mut mut_bytes = BytesMut::new();
            let stream = tokio::select! {
                stream = udp_socket.recv_buf_from(&mut mut_bytes) => stream,
                _ = shutdown.recv() => {
                    drop(m_s);
                    info!("udp listener close ~");
                    return;
                }
            };
            let Ok((tcp_stream, socket_addr)) = stream else {
                continue;
            };
            match socket_map.get(&socket_addr) {
                Some(sender) => sender.send(mut_bytes),
                None => todo!(),
            };

            let async_map = async_map.clone();
            let sender = sender.clone();
            let _shutdown = Shutdown::new(m_s.subscribe());
            let target_host = register_info.get_target_host().to_owned();
            tokio::spawn(async move {
                let uuid = get_uuid();
                let (sendr, recv) = oneshot::channel::<QuicBuffer>();
                async_map.insert(uuid.clone(), sendr).await;
                let result: Result<(), mpsc::error::SendError<Frame>> =
                    sender.send(Frame::Connection(
                        ConnectionInfo::default()
                            .uuid(uuid.clone())
                            .target_host(target_host),
                    ));
                if let Err(error) = result {
                    error!("connection error : {:?}", error);
                }
                let quci_buffer = tokio::select! {
                    quic_buffer = recv => quic_buffer,
                    _ = tokio::time::sleep(Duration::from_millis(10000)) => {
                        error!("connection timeout : 10000");
                        let _ = async_map.remove(uuid).await;
                        return;
                    }
                };
                let quci_buffer = match quci_buffer {
                    Ok(quci_buffer) => quci_buffer,
                    Err(error) => {
                        error!("recv quci_buffer error : {:?}", error);
                        return;
                    }
                };
                // let tcp_buffer = tcp_stream.into_split();
                // let quic_buffer = quci_buffer.split();
                // let result = connect(quic_buffer, tcp_buffer).await;
                // debug!("connect close ~ : {:?}", result);
            });
        }
    });
    Ok(channel_info)
}

pub fn udp_connect(
    bytes: BytesMut,
    from_addr: SocketAddr,
    udp_socket: Arc<UdpSocket>,
) -> Result<Sender<BytesMut>, BoxError> {
    let (sender, recv) = broadcast::channel::<BytesMut>(1);
    tokio::spawn(async move {
        
    });
    Ok(sender)
}

#[tokio::test]
async fn test() {
    tokio::spawn(async move {
        let udp_socket = std::net::UdpSocket::bind("0.0.0.0:1111").unwrap();
        udp_socket.set_nonblocking(true).unwrap();
        let udp_socket = UdpSocket::from_std(udp_socket).unwrap();
        loop {
            let mut bytes = BytesMut::new();
            let data = udp_socket.recv_from(&mut bytes).await;
            println!("11{:?}-{:?}", data, bytes);
        }
    });

    tokio::spawn(async move {
        let udp_socket = std::net::UdpSocket::bind("0.0.0.0:1111").unwrap();
        udp_socket.set_nonblocking(true).unwrap();
        let udp_socket = UdpSocket::from_std(udp_socket).unwrap();
        loop {
            let mut bytes = BytesMut::new();
            let data = udp_socket.recv_from(&mut bytes).await;
            println!("22{:?}-{:?}", data, bytes);
        }
    });
    tokio::signal::ctrl_c().await;
}
