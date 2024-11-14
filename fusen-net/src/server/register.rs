use crate::{
    buffer::QuicBuffer,
    frame::{Frame, RegisterInfo},
};
use fusen_common::{shutdown::Shutdown, utils::map::AsyncMap, BoxError};
use tokio::sync::{
    broadcast,
    mpsc::{self, UnboundedSender},
};
use tracing::info;

pub async fn register(
    sender: UnboundedSender<Frame>,
    register_info: RegisterInfo,
    async_map: AsyncMap<String, mpsc::Sender<QuicBuffer>>,
) -> Result<(), BoxError> {
    //监听一个协议端口
    match *register_info.get_protocol() {
        0 => tcp_listener(sender, register_info, async_map).await,
        1 => udp_listener(sender, register_info, async_map).await,
        _ => Err("not support protocol".into())
    }
}

async fn tcp_listener(
    sender: UnboundedSender<Frame>,
    register_info: RegisterInfo,
    async_map: AsyncMap<String, mpsc::Sender<QuicBuffer>>,
) -> Result<(), BoxError> {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let (s, r) = broadcast::channel::<()>(1);
    let mut shutdown = Shutdown::new(r);
    tokio::spawn(async move {
        loop {
            let stream = tokio::select! {
                stream = listener.accept() => stream,
                _ = shutdown.recv() => {
                    info!("tcp listener close ~");
                    return ;
                }
            };
            let Ok((tcp_stream, socket_addr)) = stream else {
                continue;
            };
            //接收到tcp连接先客户端先发送一个连接请求
        }
    });
    todo!()
}

async fn udp_listener(
    sender: UnboundedSender<Frame>,
    register_info: RegisterInfo,
    async_map: AsyncMap<String, mpsc::Sender<QuicBuffer>>,
) -> Result<(), BoxError> {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let (s, r) = broadcast::channel::<()>(1);
    let mut shutdown = Shutdown::new(r);
    tokio::spawn(async move {
        loop {
            let stream = tokio::select! {
                stream = listener.accept() => stream,
                _ = shutdown.recv() => {
                    info!("tcp listener close ~");
                    return ;
                }
            };
            let Ok((tcp_stream, socket_addr)) = stream else {
                continue;
            };
            //接收到tcp连接先客户端先发送一个连接请求
        }
    });
    todo!()
}
