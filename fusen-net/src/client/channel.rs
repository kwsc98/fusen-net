use std::collections::HashMap;

use fusen_common::{shutdown::Shutdown, BoxError};
use quinn::Connection;
use tokio::{
    net::TcpStream,
    sync::{broadcast, mpsc::UnboundedReceiver, oneshot},
};
use tracing::{debug, error, info};

use crate::{
    buffer::{Buffer, QuicBuffer, TcpBuffer, DEFAULT_BUF_SIZE},
    frame::{Frame, RegisterInfo},
};

pub async fn handler(
    mut recv: UnboundedReceiver<(Frame, oneshot::Sender<Result<(), BoxError>>)>,
    connection: Connection,
) -> Result<(), BoxError> {
    let mut channel_info = HashMap::<String, RegisterInfo>::new();
    let (send_stream, recv_stram) = connection.open_bi().await?;
    loop {
        let (frame, sender) = recv.recv().await.unwrap();
        match frame {
            Frame::Register(register_info) => {
                if channel_info.contains_key(register_info.get_target_host()) {
                    let info = format!("RegisterInfo Already exist : {:?}", register_info);
                    error!(info);
                    let _ = sender.send(Err(info.into()));
                } else {
                    let result = register(&connection, register_info.clone()).await;
                    if result.is_ok() {
                        channel_info
                            .insert(register_info.get_target_host().to_owned(), register_info);
                    }
                    let _ = sender.send(result);
                }
            }
            Frame::UnRegister(register_info) => todo!(),
            frame => {
                let info = format!("not support frame : {:?}", frame);
                error!(info);
                let _ = sender.send(Err(info.into()));
            }
        }
    }
}

pub async fn register(connection: &Connection, info: RegisterInfo) -> Result<(), BoxError> {
    let (send_stream, recv_stram) = connection.open_bi().await?;
    let mut quic_buffer = QuicBuffer::new(send_stream, recv_stram, DEFAULT_BUF_SIZE);
    let _ = quic_buffer
        .write_frame(&crate::frame::Frame::Register(info.clone()))
        .await;
    let mut connection: Connection = connection.clone();
    tokio::spawn(async move {
        loop {
            let result = tokio::select! {
                frame = quic_buffer.read_frame() => frame
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
    Ok(())
}

async fn do_frame(
    frame: Frame,
    quic_buffer: &mut QuicBuffer,
    connect: &mut Connection,
) -> Result<(), BoxError> {
    info!("{:?}", frame);
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
                    let tcp_buffer = TcpBuffer::new(tcp_stream, DEFAULT_BUF_SIZE);
                    let mut quic_buffer =
                        QuicBuffer::new(send_stream, recv_stream, DEFAULT_BUF_SIZE);
                    let _ = quic_buffer
                        .write_frame(&Frame::TargetConnection(connection_info))
                        .await;
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
