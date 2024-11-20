use std::{collections::HashMap, sync::Arc};

use crate::{
    buffer::{Buffer, QuicBuffer, TcpBuffer, DEFAULT_BUF_SIZE},
    frame::{Frame, RegisterInfo},
    quic::support::make_client_endpoint,
};
use base64::Engine;
use fusen_common::{shutdown::Shutdown, BoxError};
use quinn::Connection;
use tokio::{
    net::TcpStream,
    sync::{broadcast, Mutex},
};
use tracing::{debug, error, info};

pub struct Agent {
    connection: Connection,
    channel_info: Arc<Mutex<HashMap<String, RegisterInfo>>>,
}

impl Agent {
    pub async fn new(
        register: &str,
        server_cert: &str,
        server_name: &str,
    ) -> Result<Self, BoxError> {
        let endpoint = make_client_endpoint(
            "0.0.0.0:0".parse().unwrap(),
            vec![base64::prelude::BASE64_STANDARD
                .decode(server_cert)?
                .as_slice()]
            .as_slice(),
        )?;
        let connection = endpoint.connect(register.parse()?, server_name)?.await?;
        Ok(Agent {
            connection,
            channel_info: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn register(&mut self, info: RegisterInfo) -> Result<(), BoxError> {
        let mut map = self.channel_info.lock().await;
        if map.contains_key(info.get_target_host()) {
            let info = format!("RegisterInfo Already exist : {:?}", info);
            error!(info);
            return Err(info.into());
        }
        let (send_stream, recv_stram) = self.connection.open_bi().await?;
        let mut quic_buffer = QuicBuffer::new(send_stream, recv_stram, DEFAULT_BUF_SIZE);
        let _ = quic_buffer
            .write_frame(&crate::frame::Frame::Register(info.clone()))
            .await;
        let target_host = info.get_target_host().to_owned();
        map.insert(info.get_target_host().to_owned(), info);
        drop(map);
        let map = self.channel_info.clone();
        let mut connect = self.connection.clone();
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
                let result = do_frame(frame, &mut quic_buffer, &mut connect).await;
                if let Err(error) = result {
                    error!("do_frame error {:?}", error);
                    break;
                }
            }
            let mut map = map.lock().await;
            let _ = map.remove(&target_host);
            drop(map);
        });
        Ok(())
    }
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
