use std::{io, net::SocketAddr, time::Duration};

use crate::{
    buffer::{self, StreamBuffer},
    common::{ReadStream, WriteStream, shutdown::Shutdown, token::get_uuid},
    frame::Register,
};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver},
    oneshot,
};
use tracing::{debug, error, info};

pub struct ConnectRequest<RS, WS>
where
    RS: ReadStream,
    WS: WriteStream,
{
    pub token: String,
    pub one_sender: oneshot::Sender<StreamBuffer<RS, WS>>,
}

pub async fn listener<RS: ReadStream, WS: WriteStream>(
    registry: Register,
    mut shutdown: Shutdown,
) -> Result<(SocketAddr, UnboundedReceiver<ConnectRequest<RS, WS>>), io::Error> {
    let (send, recv) = mpsc::unbounded_channel::<ConnectRequest<RS, WS>>();
    //监听tcp连接
    let listener =
        tokio::net::TcpListener::bind(format!("0.0.0.0:{}", registry.remote_port)).await?;
    let local_addr = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let result = tokio::select! {
                result = listener.accept() => result,
                _ = shutdown.recv() => {
                    info!("listener close ! : {:?}",registry);
                    return ;
                }
            };
            if let Ok((tcp_stream, _)) = result {
                let send = send.clone();
                tokio::spawn(async move {
                    let (one_send, one_recv) = oneshot::channel::<StreamBuffer<RS, WS>>();
                    if let Err(error) = send.send(ConnectRequest {
                        token: get_uuid(),
                        one_sender: one_send,
                    }) {
                        error!("send ConnectRequest error : {:?}", error);
                        return;
                    }
                    let result = tokio::select! {
                        result = one_recv => result,
                        _ = tokio::time::sleep(Duration::from_millis(5000)) => {
                            error!("connect time out");
                            return ;
                        }
                    };
                    let Ok(stream_buffer) = result else {
                        error!("recv stream error : {:?}", result);
                        return;
                    };
                    let quic_stream = stream_buffer.split();
                    let tcp_stream = tcp_stream.into_split();
                    let result = buffer::connect(quic_stream, tcp_stream).await;
                    debug!("connect close ~ : {:?}", result);
                });
            }
        }
    });
    Ok((local_addr, recv))
}
