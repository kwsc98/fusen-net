use std::{collections::HashMap, sync::Arc};

use crate::{
    buffer::{Buffer, QuicBuffer},
    frame::{Frame, RegisterInfo},
    quic::support::make_client_endpoint,
};
use base64::Engine;
use fusen_common::BoxError;
use quinn::Connection;
use tokio::sync::Mutex;
use tracing::{debug, error};

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
        let mut quic_buffer = QuicBuffer::new(send_stream, recv_stram, 1 * 1024 * 1024);
        let _ = quic_buffer
            .write_frame(&crate::frame::Frame::Register(info.clone()))
            .await;
        let _ack = quic_buffer.read_frame().await?;
        let target_host = info.get_target_host().to_owned();
        map.insert(info.get_target_host().to_owned(), info);
        drop(map);
        let map = self.channel_info.clone();
        let connect = self.connection.clone();
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
                let result = match frame {
                    crate::frame::Frame::Ping => {
                        let _ = quic_buffer.write_frame(&crate::frame::Frame::Ack).await;
                    }
                    crate::frame::Frame::Ack => todo!(),
                    crate::frame::Frame::Register(register_info) => todo!(),
                    crate::frame::Frame::UnRegister(register_info) => todo!(),
                    crate::frame::Frame::Connection(connection_info) => todo!(),
                    crate::frame::Frame::TargetConnection(connection_info) => todo!(),
                };
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
    match frame {
        crate::frame::Frame::Ping => {
            let _ = quic_buffer.write_frame(&crate::frame::Frame::Ack).await?;
        }
        crate::frame::Frame::Ack => {
            debug!("recv Ack")
        }
        crate::frame::Frame::Connection(connection_info) => {

        }
        frame => {
            let info = format!("recv error frame : {:?}", frame);
            error!(info);
            return Err(info.into());
        }
    }
    Ok(())
}
