use crate::buffer::{Buffer, QuicBuffer};
use crate::common::get_uuid;
use crate::connection::connect_quic_to_quic;
use crate::frame::{ConnectionInfo, Frame};
use crate::shutdown::Shutdown;
use crate::{frame, ChannelInfo};
use fusen_common::utils::map::AsyncMap;
use fusen_common::BoxError;
use quinn::Connection;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tracing::debug;
use tracing::{error, info};

use super::register;

pub struct Channel {
    connection: Connection,
    socket_addr: SocketAddr,
    async_cache: AsyncMap<String, Arc<ChannelInfo>>,
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
        async_cache: AsyncMap<String, mpsc::Sender<QuicBuffer>>,
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
        let async_cache = AsyncMap::new();
        loop {
            let (send_stream, recv_stream) = tokio::select! {
                stream = connection.accept_bi() => stream?,
                _ = shutdown.recv() => {
                    return Ok(());
                }
            };
            let async_cache = async_cache.clone();
            tokio::spawn(async move {
                let buffer = QuicBuffer::new(send_stream, recv_stream, 1 * 1024 * 1024);
                let result = handler(buffer, async_cache).await;
                if let Err(error) = result {
                    error!("handler error : {:?}", error);
                }
            });
        }
    }
}

async fn handler(
    mut buffer: QuicBuffer,
    async_cache: AsyncMap<String, mpsc::Sender<QuicBuffer>>,
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
                    register::register(sender.clone(), register_info, async_cache.clone()).await;
                match result {
                    Ok(_) => buffer.write_frame(&Frame::Ack).await?,
                    Err(error) => error!("register error : {:?}", error),
                }
            }
            Frame::TargetConnection(connection_info) => {
                //根据uuid获取sender
                let sender = async_cache
                    .remove(connection_info.get_uuid().to_owned())
                    .await;
                let Some(sender) = sender else {
                    return Err("recv connection time out".into());
                };
                sender.send(buffer).await;
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
