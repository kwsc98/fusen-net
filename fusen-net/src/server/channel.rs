use super::register;
use crate::authentication::Authentication;
use crate::buffer::{Buffer, QuicBuffer, DEFAULT_BUF_SIZE};
use crate::frame::Frame;
use crate::quic::Connection;
use crate::shutdown::Shutdown;
use crate::utils::map::AsyncQuicBufferMap;
use crate::ChannelInfo;
use fusen_common::utils::map::AsyncMap;
use fusen_common::BoxError;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self};
use tracing::error;
use tracing::{debug, info};

pub struct Channel {
    uuid: String,
    _socket_addr: SocketAddr,
    channel_info: AsyncMap<String, ChannelInfo>,
    _shutdown_complete_tx: mpsc::Sender<()>,
    shutdown: Shutdown,
}

impl Channel {
    pub fn new(
        uuid: String,
        _socket_addr: SocketAddr,
        channel_info: AsyncMap<String, ChannelInfo>,
        _shutdown_complete_tx: mpsc::Sender<()>,
        shutdown: Shutdown,
    ) -> Self {
        Self {
            uuid,
            _socket_addr,
            channel_info,
            _shutdown_complete_tx,
            shutdown,
        }
    }

    pub async fn run(
        self,
        connection: impl Connection,
        authentication: impl Authentication,
    ) -> Result<(), crate::Error> {
        let Channel {
            uuid,
            _socket_addr,
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
            let uuid = uuid.clone();
            tokio::spawn(async move {
                let buffer = QuicBuffer::new(send_stream, recv_stream, DEFAULT_BUF_SIZE);
                let result = handler(uuid, buffer, async_cache, channel_info, authentication).await;
                if let Err(error) = result {
                    error!("handler error : {:?}", error);
                }
            });
        }
    }
}

async fn handler(
    uuid: String,
    mut buffer: QuicBuffer,
    async_cache: AsyncQuicBufferMap,
    channel_cache: AsyncMap<String, ChannelInfo>,
    authentication: impl Authentication,
) -> Result<(), BoxError> {
    let (sender, mut recv) = mpsc::unbounded_channel::<Frame>();
    loop {
        let frame = tokio::select! {
            frame = buffer.read_frame() => frame?,
            frame = recv.recv() => frame.ok_or::<BoxError>("recv frame error".into())?,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                let result = buffer.write_frame(&Frame::Ping).await;
                if let Err(error) = result {
                    info!("keep alive error : {:?}",error);
                }
                continue;
            }
        };
        match frame {
            Frame::Ping => {
                if let Err(info) = buffer.write_frame(&Frame::Ack("ok".to_string())).await {
                    error!("Send Ack Error : {:?}", info);
                }
            }
            Frame::Ack(msg) => {
                debug!("Recv Ack : {:?}", msg);
            }
            Frame::Register(register_info) => {
                if let Some(_channel_info) = channel_cache.get(uuid.clone()).await {
                    let info = "connection repeat registered".to_string();
                    info!(info);
                    buffer.write_frame(&Frame::Ack(info)).await?;
                    continue;
                }
                let register_info = Arc::new(register_info);
                let result = authentication.authentication(register_info.clone()).await;
                if !result.as_ref().is_ok_and(|e| *e) {
                    let info = format!("authentication error : {:?}", result);
                    info!(info);
                    buffer.write_frame(&Frame::Ack(info)).await?;
                    continue;
                }
                let result =
                    register::register(sender.clone(), register_info.clone(), async_cache.clone())
                        .await;
                match result {
                    Ok(channel_info) => {
                        channel_cache.insert(uuid.clone(), channel_info).await;
                        buffer.write_frame(&Frame::Ack("ok".to_string())).await?;
                    }
                    Err(error) => {
                        debug!("register error : {:?}", error);
                        buffer.write_frame(&Frame::Ack(error.to_string())).await?;
                    }
                }
            }
            Frame::TargetConnection(connection_info) => {
                let sender = async_cache
                    .remove(connection_info.get_uuid().to_owned())
                    .await;
                let Some(sender) = sender else {
                    return Err("recv connection time out".into());
                };
                let _ = sender.send(buffer);
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
