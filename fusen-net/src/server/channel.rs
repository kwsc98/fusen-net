use crate::buffer::{Buffer, QuicBuffer};
use crate::common::get_uuid;
use crate::connection::connect_quic_to_quic;
use crate::frame::{ConnectionInfo, Frame};
use crate::shutdown::Shutdown;
use crate::{frame, ChannelInfo};
use fusen_common::utils::map::AsyncMap;
use quinn::Connection;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tracing::debug;
use tracing::{error, info};

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
        async_cache: AsyncMap<String, Arc<ChannelInfo>>,
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
        let mut buffer = QuicBuffer::new(send_stream, recv_stream, 1 * 1024 * 1024);
        let uuid = get_uuid();
        loop {
            let frame = tokio::select! {
                frame = buffer.read_frame() => frame?,
                _ = shutdown.recv() => {
                    return Ok(());
                }
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
                    //根据注册信息，暴露相应的端口
                }
                Frame::TargetConnection(connection_info) => {}
                other_frame => panic!("recv error frame : {:?}", other_frame),
            }
        }
    }
}
