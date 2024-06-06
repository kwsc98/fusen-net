use super::cache::AsyncCache;
use crate::buffer::Buffer;
use crate::frame::Frame;
use crate::shutdown::{self, Shutdown};
use crate::{connection, frame, ChannelInfo};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::info;

pub struct Channel {
    stream: TcpStream,
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
        stream: TcpStream,
        socket_addr: SocketAddr,
        async_cache: AsyncCache<String, Arc<ChannelInfo>>,
        _shutdown_complete_tx: mpsc::Sender<()>,
        shutdown: Shutdown,
    ) -> Self {
        Self {
            stream,
            socket_addr,
            async_cache,
            _shutdown_complete_tx,
            shutdown,
        }
    }

    pub async fn run(self) -> Result<(), crate::Error> {
        let Channel {
            stream,
            socket_addr,
            async_cache,
            _shutdown_complete_tx,
            mut shutdown,
        } = self;
        let mut buffer = Buffer::new(stream);
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
                                net_addr: socket_addr.clone(),
                                register_info,
                                sender: sender.clone(),
                            });
                            let _ = async_cache
                                .insert(
                                    channel_info.register_info.get_tag().to_owned(),
                                    channel_info.clone(),
                                )
                                .await;
                            let _ = buffer.write_frame(&frame::Frame::Ack).await;
                            let async_cache_clone = async_cache.clone();
                            //KeepAlive
                            tokio::spawn(async move {
                                loop {
                                    tokio::time::sleep(Duration::from_secs(5)).await;
                                    if let Err(_) = channel_info.sender.send(Frame::Ping) {
                                        info!("register conn close : {:?}", channel_info);
                                        break;
                                    }
                                }
                                let _ = async_cache_clone
                                    .remove(channel_info.register_info.get_tag().to_owned())
                                    .await;
                            });
                        }
                        frame::Frame::Connection(connection_info) => {
                            let target_channel_info = async_cache
                                .get(connection_info.get_target_tag().to_owned())
                                .await?
                                .ok_or(format!("not find : {:?}", connection_info))?;
                            let channel_info = Arc::new(ChannelInfo {
                                net_addr: socket_addr.clone(),
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
                                let _ = connection::handler(
                                    connection_info,
                                    buffer,
                                    target_channel_info,
                                    receiver,
                                )
                                .await;
                                let _ = async_cache.remove(tag).await;
                            });
                            return Ok(());
                        }
                        frame::Frame::TargetConnection(connection_info) => {
                            println!("---{:?}", connection_info);
                            let source_channel_info = async_cache
                                .get(connection_info.get_source_tag().to_owned())
                                .await?
                                .ok_or(format!("not find : {:?}", connection_info))?;
                            let _ = buffer.write_frame(&frame::Frame::Ack).await;
                            let _ = source_channel_info.sender.send(Frame::TargetBuffer(buffer));
                            let _ = async_cache
                                .remove(connection_info.get_source_tag().to_owned())
                                .await;
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
