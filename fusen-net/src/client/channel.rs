use std::collections::HashMap;

use fusen_common::{shutdown::Shutdown, BoxError};
use quinn::Connection;
use tokio::{
    net::TcpStream,
    sync::{
        broadcast::{self, Sender},
        mpsc::{UnboundedReceiver, UnboundedSender},
        oneshot,
    },
};
use tracing::{debug, error, info};

use crate::{
    buffer::{Buffer, QuicBuffer, DEFAULT_BUF_SIZE},
    frame::{Frame, RegisterInfo},
};

use super::get_connection;

#[derive(Debug)]
pub struct ChannelInfo {
    _register_info: RegisterInfo,
    _shutdown: Sender<()>,
}

pub async fn handler(
    mut recv: UnboundedReceiver<(Frame, oneshot::Sender<Result<(), BoxError>>)>,
    connect_sender: UnboundedSender<oneshot::Sender<Result<Connection, BoxError>>>,
) -> Result<(), BoxError> {
    let mut channel_info = HashMap::<String, ChannelInfo>::new();
    loop {
        let (frame, sender) = recv.recv().await.unwrap();
        match frame {
            Frame::Register(register_info) => {
                if channel_info.contains_key(register_info.get_target_host()) {
                    let info = format!("RegisterInfo Already exist : {:?}", register_info);
                    error!(info);
                    let _ = sender.send(Err(info.into()));
                } else {
                    let result = register(connect_sender.clone(), register_info.clone()).await;
                    match result {
                        Ok(shutdown) => {
                            channel_info.insert(
                                register_info.get_target_host().to_owned(),
                                ChannelInfo {
                                    _register_info: register_info,
                                    _shutdown: shutdown,
                                },
                            );
                            let _ = sender.send(Ok(()));
                        }
                        Err(error) => {
                            let _ = sender.send(Err(error));
                        }
                    };
                }
            }
            Frame::UnRegister(register_info) => {
                let _result = channel_info.remove(register_info.get_target_host());
                let _ = sender.send(Ok(()));
            }
            frame => {
                let info = format!("not support frame : {:?}", frame);
                error!(info);
                let _ = sender.send(Err(info.into()));
            }
        }
    }
}

pub async fn register(
    connect_sender: UnboundedSender<oneshot::Sender<Result<Connection, BoxError>>>,
    info: RegisterInfo,
) -> Result<Sender<()>, BoxError> {
    let connection = get_connection(&connect_sender).await?;
    let (send_stream, recv_stram) = connection.open_bi().await?;
    let mut quic_buffer = QuicBuffer::new(send_stream, recv_stram, DEFAULT_BUF_SIZE);
    let _ = quic_buffer
        .write_frame(&crate::frame::Frame::Register(info.clone()))
        .await;
    let (s, _) = broadcast::channel::<()>(1);
    let mut shutdown = Shutdown::new(s.subscribe());
    let mut connection: Connection = connection.clone();
    tokio::spawn(async move {
        loop {
            let result = tokio::select! {
                frame = quic_buffer.read_frame() => frame,
                _ = shutdown.recv() => {
                    info!("unregister : {:?}",info);
                    return;
                }
            };
            let frame = match result {
                Ok(frame) => frame,
                Err(error) => {
                    error!("recv frame error : {:?}", error);
                    break;
                }
            };
            let result = do_frame(frame, &mut quic_buffer, &mut connection).await;
            if let Err(error) = result {
                error!("do_frame error {:?}", error);
                break;
            }
        }
    });
    Ok(s)
}

async fn do_frame(
    frame: Frame,
    quic_buffer: &mut QuicBuffer,
    connect: &mut Connection,
) -> Result<(), BoxError> {
    match frame {
        crate::frame::Frame::Ping => {
            quic_buffer.write_frame(&crate::frame::Frame::Ack).await?;
        }
        crate::frame::Frame::Ack => {
            debug!("recv Ack")
        }
        crate::frame::Frame::Connection(connection_info) => match connect.open_bi().await {
            Ok((send_stream, recv_stream)) => {
                tokio::spawn(async move {
                    let result = TcpStream::connect(connection_info.get_target_host()).await;
                    let tcp_stream = match result {
                        Ok(stream) => stream,
                        Err(error) => {
                            error!("connect target_host error : {:?}", error);
                            return;
                        }
                    };
                    let tcp_buffer = tcp_stream.into_split();
                    let mut quic_buffer =
                        QuicBuffer::new(send_stream, recv_stream, DEFAULT_BUF_SIZE);
                    let _ = quic_buffer
                        .write_frame(&Frame::TargetConnection(connection_info))
                        .await;
                    let quic_buffer = quic_buffer.split();
                    let (s, _r) = broadcast::channel::<()>(1);
                    let shutdown = Shutdown::new(s.subscribe());
                    let result = crate::buffer::connect(tcp_buffer, quic_buffer, shutdown).await;
                    debug!("connect close ~ : {:?}", result);
                });
            }
            Err(error) => error!("connect open_bi error : {:?}", error),
        },
        frame => {
            let info = format!("recv error frame : {:?}", frame);
            error!(info);
            return Err(info.into());
        }
    }
    Ok(())
}
