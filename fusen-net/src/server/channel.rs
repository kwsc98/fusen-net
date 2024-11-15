use crate::buffer::{Buffer, QuicBuffer};
use crate::frame::Frame;
use crate::shutdown::Shutdown;
use crate::utils::map::AsyncQuicBufferMap;
use crate::ChannelInfo;
use fusen_common::utils::map::AsyncMap;
use fusen_common::BoxError;
use quinn::Connection;
use std::net::SocketAddr;
use tokio::sync::mpsc::{self};
use tracing::debug;
use tracing::error;

use super::register;

pub struct Channel {
    connection: Connection,
    socket_addr: SocketAddr,
    channel_info: AsyncMap<String, ChannelInfo>,
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
        channel_info: AsyncMap<String, ChannelInfo>,
        _shutdown_complete_tx: mpsc::Sender<()>,
        shutdown: Shutdown,
    ) -> Self {
        Self {
            connection,
            socket_addr,
            channel_info,
            _shutdown_complete_tx,
            shutdown,
        }
    }

    pub async fn run(self) -> Result<(), crate::Error> {
        let Channel {
            connection,
            socket_addr,
            channel_info,
            _shutdown_complete_tx,
            mut shutdown,
        } = self;
        let async_cache = AsyncQuicBufferMap::new();
        loop {
            let (send_stream, recv_stream) = tokio::select! {
                stream = connection.accept_bi() => stream?,
                _ = shutdown.recv() => {
                    return Ok(());
                }
            };
            let async_cache = async_cache.clone();
            let channel_info = channel_info.clone();
            tokio::spawn(async move {
                let buffer = QuicBuffer::new(send_stream, recv_stream, 1 * 1024 * 1024);
                let result = handler(buffer, async_cache, channel_info).await;
                if let Err(error) = result {
                    error!("handler error : {:?}", error);
                }
            });
        }
    }
}

async fn handler(
    mut buffer: QuicBuffer,
    async_cache: AsyncQuicBufferMap,
    channel_info: AsyncMap<String, ChannelInfo>,
) -> Result<(), BoxError> {
    let (sender, mut recv) = mpsc::unbounded_channel::<Frame>();
    loop {
        let frame = tokio::select! {
            frame = buffer.read_frame() => frame?,
            frame = recv.recv() => frame.ok_or::<BoxError>("recv frame error".into())?,
        };
        match frame {
            Frame::Ping => {
                if let Err(info) = buffer.write_frame(&Frame::Ack).await {
                    error!("Send Ack Error : {:?}", info);
                }
            }
            Frame::Ack => {
                debug!("Recv Ack");
            }
            Frame::Register(register_info) => {
                let result =
                    register::register(sender.clone(), register_info.clone(), async_cache.clone())
                        .await;
                match result {
                    Ok(sender) => {
                        channel_info
                            .insert(
                                register_info.get_target_host().to_owned(),
                                ChannelInfo::new(register_info, sender),
                            )
                            .await;
                        buffer.write_frame(&Frame::Ack).await?
                    }
                    Err(error) => error!("register error : {:?}", error),
                }
            }
            Frame::UnRegister(register_info) => {
                let _ = channel_info
                    .remove(register_info.get_target_host().to_owned())
                    .await;
                buffer.write_frame(&Frame::Ack).await?
            }
            Frame::TargetConnection(connection_info) => {
                //根据uuid获取sender
                let sender = async_cache
                    .remove(connection_info.get_uuid().to_owned())
                    .await;
                let Some(sender) = sender else {
                    return Err("recv connection time out".into());
                };
                sender.send(buffer);
                return Ok(());
            }
            Frame::Connection(connection_info) => {
                let _result = buffer
                    .write_frame(&Frame::Connection(connection_info))
                    .await;
            }
        }
    }
}
