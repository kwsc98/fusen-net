use super::cache::AsyncCache;
use crate::buffer::QuicBuffer;
use crate::connection::connect_quic_to_quic;
use crate::frame::{ConnectionInfo, Frame};
use crate::quic::{self, support};
use crate::shutdown::Shutdown;
use crate::{frame, ChannelInfo};
use quinn::Connection;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tracing::info;

pub struct Channel {
    connection: Connection,
    socket_addr: SocketAddr,
    async_cache: AsyncCache<String, Arc<ChannelInfo>>,
    _shutdown_complete_tx: mpsc::Sender<()>,
    shutdown: Shutdown,
}

#[derive(Debug)]
pub enum FrameType {
    Socket(Frame),
    Handler(Frame),
}

impl Channel {
    pub fn new(
        connection: Connection,
        socket_addr: SocketAddr,
        async_cache: AsyncCache<String, Arc<ChannelInfo>>,
        _shutdown_complete_tx: mpsc::Sender<()>,
        shutdown: Shutdown,
    ) -> Self {
        Self {
            connection,
            socket_addr,
            async_cache,
            _shutdown_complete_tx,
            shutdown,
        }
    }

    pub async fn run(self) -> Result<(), crate::Error> {
        let Channel {
            connection,
            socket_addr,
            async_cache,
            _shutdown_complete_tx,
            mut shutdown,
        } = self;
        let (send_stream, recv_stream) = connection.accept_bi().await?;
        let mut buffer = QuicBuffer::new(send_stream, recv_stream);
        let (sender, mut receiver) = mpsc::unbounded_channel();
        loop {
            let frame = tokio::select! {
                res = buffer.read_frame() => FrameType::Socket(res?),
                res1 = receiver.recv() => FrameType::Handler(res1.ok_or::<crate::Error>("receiver error".into())?),
                _ = shutdown.recv() => {
                    return Ok(());
                }
            };
            match frame {
                FrameType::Socket(frame) => {
                    match frame {
                        frame::Frame::Register(register_info) => {
                            let channel_info = Arc::new(ChannelInfo {
                                net_addr: socket_addr,
                                register_info,
                                sender: sender.clone(),
                            });
                            let _ = buffer.write_frame(&frame::Frame::Ack).await;
                            let _ = async_cache
                                .insert(
                                    channel_info.register_info.get_tag().to_owned(),
                                    channel_info.clone(),
                                )
                                .await;
                            // let Ok((quic_buffer, _addr)) =
                            //     quic::connect(channel_info.net_addr.clone()).await
                            // else {
                            //     return Err("connect error".into());
                            // };
                            // buffer = quic_buffer;
                            let async_cache_clone = async_cache.clone();
                            //KeepAlive
                            tokio::spawn(async move {
                                loop {
                                    tokio::time::sleep(Duration::from_secs(3)).await;
                                    if channel_info.sender.send(Frame::Ping).is_err() {
                                        break;
                                    }
                                }
                                info!("register conn close : {:?}", channel_info);
                                let _ = async_cache_clone
                                    .remove(channel_info.register_info.get_tag().to_owned())
                                    .await;
                            });
                        }
                        frame::Frame::Connection(connection_info) => {
                            let target_channel_info = async_cache
                                .get(connection_info.get_target_tag().to_owned())
                                .await?
                                .ok_or(format!("not find connection : {:?}", connection_info))?;
                            let channel_info = Arc::new(ChannelInfo {
                                net_addr: socket_addr,
                                register_info: Default::default(),
                                sender: sender.clone(),
                            });
                            let _ = async_cache
                                .insert(
                                    connection_info.get_source_tag().to_owned(),
                                    channel_info.clone(),
                                )
                                .await;
                            tokio::spawn(async move {
                                let tag = connection_info.get_source_tag().to_owned();
                                let _ =
                                    handler(connection_info, buffer, target_channel_info, receiver)
                                        .await;
                                let _ = async_cache.remove(tag).await;
                            });
                            return Ok(());
                        }
                        frame::Frame::TargetConnection(connection_info) => {
                            let source_channel_info = async_cache
                                .get(connection_info.get_source_tag().to_owned())
                                .await?
                                .ok_or(format!(
                                    "not find targetConnection: {:?}",
                                    connection_info
                                ))?;
                            let _ = buffer.write_frame(&frame::Frame::Ack).await;
                            let _ = source_channel_info.sender.send(Frame::TargetBuffer(buffer));
                            let _ = async_cache
                                .remove(connection_info.get_source_tag().to_owned())
                                .await;
                            return Ok(());
                        }
                        frame::Frame::Subscribe(mut subscribe_info) => {
                            let async_cache_clone = async_cache.clone();
                            //KeepAlive
                            tokio::spawn(async move {
                                loop {
                                    let addr = async_cache_clone
                                        .get(subscribe_info.get_target_tag().to_owned())
                                        .await
                                        .unwrap();
                                    let target_sockeraddr = match addr {
                                        Some(channel) => Some(channel.net_addr.to_string()),
                                        None => None,
                                    };
                                    subscribe_info.set_target_sockeraddr(target_sockeraddr);
                                    let _ = buffer
                                        .write_frame(&Frame::Subscribe(subscribe_info.clone()))
                                        .await;
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                }
                            });
                            return Ok(());
                        }
                        frame::Frame::Ping => {
                            let _ = buffer.write_frame(&frame::Frame::Ack).await;
                        }
                        _ => (),
                    }
                }
                FrameType::Handler(frame) => {
                    let _ = buffer.write_frame(&frame).await;
                }
            }
        }
    }
}

async fn handler(
    connection_info: ConnectionInfo,
    mut buffer1: QuicBuffer,
    channel_info: Arc<ChannelInfo>,
    mut receiver: UnboundedReceiver<Frame>,
) -> Result<(), crate::Error> {
    channel_info
        .sender
        .send(Frame::Connection(connection_info))?;
    let frame = receiver.recv().await.ok_or("receive error")?;
    let Frame::TargetBuffer(buffer2) = frame else {
        return Err("receive error frame".into());
    };
    let _ = buffer1.write_frame(&Frame::Ack).await;
    connect_quic_to_quic(buffer1, buffer2).await
}
