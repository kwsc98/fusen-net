use std::{net::SocketAddr, sync::Arc};

use crate::{
    buffer::{self, DEFAULT_BUF_SIZE, StreamBuffer},
    frame::{Frame, Register},
    quic::{Connection, Endpoint},
};
use tokio::{io, net::TcpStream};
use tracing::{debug, error, info};

#[derive(Clone)]
pub struct Agent {
    pub register: String,
    pub server_name: String,
}

impl Agent {
    pub async fn register(
        &self,
        register: Register,
        endpoint: impl Endpoint,
    ) -> Result<(), io::Error> {
        let target = register
            .target
            .parse::<SocketAddr>()
            .map_err(|_error| io::Error::other("target socket error !"))?;
        let connection = endpoint
            .connect(
                self.register
                    .parse()
                    .map_err(|error| io::Error::other(format!("{:?}", error)))?,
                self.server_name.to_owned(),
            )
            .await
            .map_err(|error| io::Error::other(format!("{:?}", error)))?;
        let connection = Arc::new(connection);
        let (read_stream, write_stream) = match connection.open_bi().await {
            Ok(stream) => stream,
            Err(error) => return Err(io::Error::other(format!("{:?}", error))),
        };
        let mut buffer = StreamBuffer::new(read_stream, write_stream, DEFAULT_BUF_SIZE);
        let result = buffer
            .write_frame(&crate::frame::Frame::Register(register))
            .await;
        if let Err(error) = result {
            return Err(io::Error::other(format!(
                "send frame register error : {:?}",
                error
            )));
        }
        let result = buffer.read_frame().await;
        let Ok(Frame::RegisterResponse(response)) = result else {
            return Err(io::Error::other(format!(
                "recv frame register_response error : {:?}",
                result
            )));
        };
        info!("register success : {:?}", response);
        while let Ok(frame) = buffer.read_frame().await {
            if let Frame::Connection(connect) = frame {
                let connection = connection.clone();
                tokio::spawn(async move {
                    let result = TcpStream::connect(target).await;
                    let tcp_stream = match result {
                        Ok(stream) => stream,
                        Err(error) => {
                            error!("connect target socket_addr error : {:?}", error);
                            return;
                        }
                    };
                    let (read_stream, write_stream) = match connection.open_bi().await {
                        Ok(stream) => stream,
                        Err(error) => {
                            error!("connection open_bi error : {:?}", error);
                            return;
                        }
                    };
                    let mut buffer = StreamBuffer::new(read_stream, write_stream, DEFAULT_BUF_SIZE);
                    let result = buffer
                        .write_frame(&Frame::ConnectionResponse(connect))
                        .await;
                    if let Err(error) = result {
                        error!("send frame ConnectionResponse error : {:?}", error);
                        return;
                    }
                    let quic_stream = buffer.split();
                    let tcp_stream = tcp_stream.into_split();
                    let result = buffer::connect(quic_stream, tcp_stream).await;
                    debug!("connect close ~ : {:?}", result);
                });
            }
        }
        Ok(())
    }
}
