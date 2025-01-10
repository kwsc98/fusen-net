use std::{sync::Arc, time::Duration};

use crate::{
    buffer::{connect, Buffer, QuicBuffer},
    common::get_uuid,
    frame::{ConnectionInfo, Frame, RegisterInfo},
    utils::map::AsyncQuicBufferMap,
};
use fusen_common::{shutdown::Shutdown, BoxError};
use tokio::sync::{
    broadcast::{self, Sender},
    mpsc::{self, UnboundedSender},
    oneshot,
};
use tracing::info;
use tracing::{debug, error};

pub async fn register(
    sender: UnboundedSender<Frame>,
    register_info: Arc<RegisterInfo>,
    async_map: AsyncQuicBufferMap,
) -> Result<Sender<()>, BoxError> {
    //监听一个协议端口
    match *register_info.get_protocol() {
        0 => tcp_listener(sender, register_info, async_map).await,
        // 1 => udp_listener(sender, register_info, async_map).await,
        _ => Err("not support protocol".into()),
    }
}

async fn tcp_listener(
    sender: UnboundedSender<Frame>,
    register_info: Arc<RegisterInfo>,
    async_map: AsyncQuicBufferMap,
) -> Result<Sender<()>, BoxError> {
    let port = register_info.get_remote_port().unwrap_or(0);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    info!("{:?}", listener);
    let (s, _r) = broadcast::channel::<()>(1);
    let mut shutdown = Shutdown::new(s.subscribe());
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
    Ok(s)
}

// async fn udp_listener(
//     sender: UnboundedSender<Frame>,
//     register_info: Arc<RegisterInfo>,
//     async_map: AsyncQuicBufferMap,
// ) -> Result<Sender<()>, BoxError> {
//     let port = register_info.get_remote_port().unwrap_or(0);
//     let udp_socket = tokio::net::UdpSocket::bind(format!("0.0.0.0:{}", port)).await?;
//     let udp_socket = Arc::new(udp_socket);
//     info!("{:?}", udp_socket);
//     let (s, _r) = broadcast::channel::<()>(1);
//     let mut shutdown = Shutdown::new(s.subscribe());
//     tokio::spawn(async move {
//         let (m_s, _) = broadcast::channel::<()>(1);
//         let mut bytes = bytes::BytesMut::new();
//         let mut udp_cache = HashMap::<String, Sender<()>>::new();
//         loop {
//             let stream = tokio::select! {
//                 stream = listener.accept() => stream,
//                 _ = shutdown.recv() => {
//                     drop(m_s);
//                     info!("tcp listener close ~");
//                     return;
//                 }
//             };
//             let Ok((tcp_stream, _socket_addr)) = stream else {
//                 continue;
//             };
//             let async_map = async_map.clone();
//             let sender = sender.clone();
//             let _shutdown = Shutdown::new(m_s.subscribe());
//             let target_host = register_info.get_target_host().to_owned();
//             tokio::spawn(async move {
//                 let uuid = get_uuid();
//                 let (sendr, recv) = oneshot::channel::<QuicBuffer>();
//                 async_map.insert(uuid.clone(), sendr).await;
//                 let result: Result<(), mpsc::error::SendError<Frame>> =
//                     sender.send(Frame::Connection(
//                         ConnectionInfo::default()
//                             .uuid(uuid.clone())
//                             .target_host(target_host),
//                     ));
//                 if let Err(error) = result {
//                     error!("connection error : {:?}", error);
//                 }
//                 let quci_buffer = tokio::select! {
//                     quic_buffer = recv => quic_buffer,
//                     _ = tokio::time::sleep(Duration::from_millis(10000)) => {
//                         error!("connection timeout : 10000");
//                         let _ = async_map.remove(uuid).await;
//                         return;
//                     }
//                 };
//                 let quci_buffer = match quci_buffer {
//                     Ok(quci_buffer) => quci_buffer,
//                     Err(error) => {
//                         error!("recv quci_buffer error : {:?}", error);
//                         return;
//                     }
//                 };
//                 let tcp_buffer = tcp_stream.into_split();
//                 let quic_buffer = quci_buffer.split();
//                 let result = connect(quic_buffer, tcp_buffer).await;
//                 debug!("connect close ~ : {:?}", result);
//             });
//         }
//     });
//     Ok(s)
// }

// pub struct UdpConnect {
//     from_addr: SocketAddr,
//     udp_socket: Arc<UdpSocket>,
// }

// impl UdpConnect {
//     pub fn new(udp_socket: Arc<UdpSocket>, from_addr: SocketAddr) -> Result<Self, BoxError> {
//         let udp_connect = Self {
//             from_addr,
//             udp_socket,
//         };
//         let (sender, recv) = tokio::sync::mpsc::unbounded_channel::<()>();
//         tokio::spawn(async move {

//         });
//         Ok(udp_connect)
//     }
// }
