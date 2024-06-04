use super::cache::AsyncCache;
use crate::buffer::Buffer;
use crate::shutdown::Shutdown;
use crate::socket::get_tcp_stream;
use crate::{frame, ChannelInfo, Protocol};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::error;

pub struct Channel {
    stream: TcpStream,
    socket_addr: SocketAddr,
    async_cache: AsyncCache<String, Arc<ChannelInfo>>,
    _shutdown_complete_tx: mpsc::Sender<()>,
    shutdown: Shutdown,
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
        let (protocol, net_ip, net_port) = match socket_addr {
            SocketAddr::V4(socke_v4) => (Protocol::V4, socke_v4.ip().to_string(), socke_v4.port()),
            SocketAddr::V6(socke_v6) => (Protocol::V6, socke_v6.ip().to_string(), socke_v6.port()),
        };
        loop {
            let frame = tokio::select! {
                res = buffer.read_frame() => res?,
                _ = shutdown.recv() => {
                    return Ok(());
                }
            };
            if let Some(frame) = frame {
                match frame {
                    frame::Frame::Register(mut channel_info) => {
                        channel_info.net_ip = net_ip.clone();
                        channel_info.net_port = net_port.clone();
                        channel_info.protocol = protocol.clone();
                        let channel_info = Arc::new(channel_info);
                        let _ = async_cache
                            .insert(channel_info.tag.clone(), channel_info.clone())
                            .await;
                        let async_cache_clone = async_cache.clone();
                        tokio::spawn(async move {
                            let channel_info = channel_info;
                            let async_cache = async_cache_clone;
                            match get_tcp_stream(channel_info.net_ip.clone(), channel_info.net_port)
                                .await
                            {
                                Ok(stream) => {
                                    let mut buffer = Buffer::new(stream);
                                    loop {
                                        match buffer.write_frame(&frame::Frame::PING).await {
                                            Ok(_) => (),
                                            Err(_) => {
                                                error!("ping send error : {:?}", channel_info);
                                                break;
                                            }
                                        };
                                        match buffer.read_frame_wait(Duration::from_secs(5)).await {
                                            Ok(_frame) => {
                                                tokio::time::sleep(Duration::from_secs(2)).await;
                                            }
                                            Err(_) => {
                                                error!("ack receive time out : {:?}", channel_info);
                                                break;
                                            }
                                        }
                                    }
                                    let _ = async_cache.remove(channel_info.tag.clone());
                                }
                                Err(error) => error!("get tcp stream error : {:?}", error),
                            };
                        });
                        let _ = buffer.write_frame(&frame::Frame::ACK).await;
                    }
                    frame::Frame::Subscribe(tag) => {
                        let channel_into = async_cache.get(tag).await;
                        match channel_into {
                            Ok(option) => match option {
                                Some(info) => {
                                    let _ = buffer
                                        .write_frame(&frame::Frame::Register(info.as_ref().clone()))
                                        .await;
                                }
                                None => {
                                    let _ = buffer.write_frame(&frame::Frame::NotFind).await;
                                }
                            },
                            Err(_err) => {
                                let _ = buffer.write_frame(&frame::Frame::ERROR).await;
                            }
                        }
                    }
                    frame::Frame::PING => {
                        let _ = buffer.write_frame(&frame::Frame::ACK).await;
                    }
                    _ => {
                        let _ = buffer.write_frame(&frame::Frame::ACK).await;
                    }
                };
            }
        }
    }
}
