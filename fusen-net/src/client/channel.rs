use crate::{
    buffer::{Buffer, QuicBuffer, DEFAULT_BUF_SIZE},
    frame::{Frame, RegisterInfo},
    quic::Connection,
};
use fusen_common::BoxError;
use tokio::net::TcpStream;
use tracing::{debug, error};

pub async fn register(connection: impl Connection, info: RegisterInfo) -> Result<(), BoxError> {
    let (send_stream, recv_stram) = connection.open_bi().await?;
    let mut quic_buffer = QuicBuffer::new(send_stream, recv_stram, DEFAULT_BUF_SIZE);
    let _ = quic_buffer
        .write_frame(&crate::frame::Frame::Register(info.clone()))
        .await;
    loop {
        let result = quic_buffer.read_frame().await;
        let frame = match result {
            Ok(frame) => frame,
            Err(error) => {
                error!("recv frame error : {:?}", error);
                break;
            }
        };
        let result = do_frame(frame, &mut quic_buffer, &connection).await;
        if let Err(error) = result {
            error!("do_frame error {:?}", error);
            break;
        }
    }
    Ok(())
}

async fn do_frame(
    frame: Frame,
    quic_buffer: &mut QuicBuffer,
    connect: &impl Connection,
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
                let mut quic_buffer = QuicBuffer::new(send_stream, recv_stream, DEFAULT_BUF_SIZE);
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
                    let _ = quic_buffer
                        .write_frame(&Frame::TargetConnection(connection_info))
                        .await;
                    let quic_buffer = quic_buffer.split();
                    let result = crate::buffer::connect(quic_buffer, tcp_buffer).await;
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
