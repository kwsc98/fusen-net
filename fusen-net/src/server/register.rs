use std::time::Duration;

use crate::{
    buffer::{Buffer, QuicBuffer, TcpBuffer},
    common::get_uuid,
    frame::{ConnectionInfo, Frame, RegisterInfo},
    utils::map::AsyncQuicBufferMap,
};
use fusen_common::{shutdown::Shutdown, BoxError};
use tokio::sync::{
    broadcast::{self, Sender},
    mpsc::{self, UnboundedSender},
    oneshot,
};
use tracing::info;
use tracing::{debug, error};

pub async fn register(
    sender: UnboundedSender<Frame>,
    register_info: RegisterInfo,
    async_map: AsyncQuicBufferMap,
) -> Result<Sender<()>, BoxError> {
    //监听一个协议端口
    match *register_info.get_protocol() {
        0 => tcp_listener(sender, register_info, async_map).await,
        1 => udp_listener(sender, register_info, async_map).await,
        _ => Err("not support protocol".into()),
    }
}

async fn tcp_listener(
    sender: UnboundedSender<Frame>,
    register_info: RegisterInfo,
    async_map: AsyncQuicBufferMap,
) -> Result<Sender<()>, BoxError> {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let (s, _r) = broadcast::channel::<()>(1);
    let mut shutdown = Shutdown::new(s.subscribe());
    tokio::spawn(async move {
        let (m_s, _) = broadcast::channel::<()>(1);
        loop {
            let stream = tokio::select! {
                stream = listener.accept() => stream,
                _ = shutdown.recv() => {
                    drop(m_s);
                    info!("tcp listener close ~");
                    return;
                }
            };
            let Ok((tcp_stream, _socket_addr)) = stream else {
                continue;
            };
            let async_map = async_map.clone();
            let sender = sender.clone();
            let shutdown = Shutdown::new(m_s.subscribe());
            let target_host = register_info.get_target_host().to_owned();
            tokio::spawn(async move {
                let uuid = get_uuid();
                let (sendr, recv) = oneshot::channel::<QuicBuffer>();
                async_map.insert(uuid.clone(), sendr);
                let result: Result<(), mpsc::error::SendError<Frame>> =
                    sender.send(Frame::Connection(
                        ConnectionInfo::default()
                            .uuid(uuid.clone())
                            .target_host(target_host),
                    ));
                if let Err(error) = result {
                    error!("connection error : {:?}", error);
                }
                let quci_buffer = tokio::select! {
                    quic_buffer = recv => quic_buffer,
                    _ = tokio::time::sleep(Duration::from_millis(1000)) => {
                        error!("connection timeout : 1000");
                        let _ = async_map.remove(uuid).await;
                        return;
                    }
                };
                let quci_buffer = match quci_buffer {
                    Ok(quci_buffer) => quci_buffer,
                    Err(error) => {
                        error!("recv quci_buffer error : {:?}", error);
                        return;
                    }
                };
                let tcp_buffer = TcpBuffer::new(tcp_stream, 1 * 1024 * 1024);
                let result = connect(tcp_buffer, quci_buffer, shutdown).await;
                debug!("connect close ~ : {:?}", result);
            });
        }
    });
    Ok(s)
}

async fn udp_listener(
    sender: UnboundedSender<Frame>,
    register_info: RegisterInfo,
    async_map: AsyncQuicBufferMap,
) -> Result<Sender<()>, BoxError> {
    todo!()
}

async fn connect(
    mut s1: impl Buffer,
    mut s2: impl Buffer,
    mut shutdown: Shutdown,
) -> Result<(), BoxError> {
    loop {
        tokio::select! {
            buf = s1.read_buf() => s2.write_buf(buf?).await?,
            buf = s2.read_buf() => s1.write_buf(buf?).await?,
            _ = shutdown.recv() => {
                info!("connect shutdown");
                return Ok(());
            }
        };
    }
}
