use std::{collections::HashMap, sync::Arc, time::Duration};

use crate::{
    buffer::{DEFAULT_BUF_SIZE, StreamBuffer},
    common::ConnectError,
    frame::{Frame, RegisterResponse},
    listener::listener,
    quic::{Connection, Endpoint},
};
use tokio::sync::{Mutex, oneshot};
use tracing::error;
pub struct NetServer;

impl NetServer {
    pub async fn run(&self, endpoint: impl Endpoint) -> Result<(), ConnectError> {
        while let Ok(connect) = endpoint.accept().await {
            tokio::spawn(async move {
                let mut init = None;
                let async_map = Arc::new(Mutex::new(HashMap::<
                    String,
                    oneshot::Sender<StreamBuffer>,
                >::default()));
                while let Ok((read_stream, write_stream)) = connect.accept_bi().await {
                    let mut buffer = StreamBuffer::new(read_stream, write_stream, DEFAULT_BUF_SIZE);
                    if init.is_none() {
                        let _ = init.insert(String::new());
                        if let Ok(crate::frame::Frame::Register(registry)) =
                            buffer.read_frame().await
                        {
                            let (socker_addr, mut recv) = match listener(registry).await {
                                Ok(recv) => recv,
                                Err(error) => {
                                    error!("register listener error : {:?}", error);
                                    return;
                                }
                            };
                            if let Err(error) = buffer
                                .write_frame(&crate::frame::Frame::RegisterResponse(
                                    RegisterResponse {
                                        local_addr: format!("{:?}", socker_addr),
                                    },
                                ))
                                .await
                            {
                                error!("send RegisterResponse error : {error}");
                                return;
                            };
                            let async_map = async_map.clone();
                            tokio::spawn(async move {
                                while let Some(connect_request) = recv.recv().await {
                                    let mut w_map = async_map.lock().await;
                                    w_map.insert(
                                        connect_request.token.clone(),
                                        connect_request.one_sender,
                                    );
                                    drop(w_map);
                                    let async_map = async_map.clone();
                                    let token = connect_request.token.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(Duration::from_millis(5000)).await;
                                        let mut w_map = async_map.lock().await;
                                        w_map.remove(&token);
                                        drop(w_map);
                                    });
                                    if let Err(error) = buffer
                                        .write_frame(&crate::frame::Frame::Connection(
                                            crate::frame::Connection {
                                                token: connect_request.token.clone(),
                                            },
                                        ))
                                        .await
                                    {
                                        error!("send Connection error ! : {:?}", error);
                                    }
                                }
                            });
                        }
                    } else {
                        let async_map = async_map.clone();
                        tokio::spawn(async move {
                            if let Ok(Frame::ConnectionResponse(token)) = buffer.read_frame().await
                            {
                                //获取sender
                                let mut w_map = async_map.lock().await;
                                let option = w_map.remove(&token);
                                drop(w_map);
                                if let Some(send) = option {
                                    let _ = send.send(buffer);
                                }
                            }
                        });
                    }
                }
            });
        }
        Ok(())
    }
}
