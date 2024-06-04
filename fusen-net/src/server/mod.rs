use cache::AsyncCache;
use channel::Channel;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::broadcast::Sender;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info};

use crate::shutdown::Shutdown;
use crate::ChannelInfo;
mod cache;
mod channel;

pub struct Server {
    port: String,
}

impl Server {
    pub fn new(port: &str) -> Self {
        Self { port: port.into() }
    }
    pub async fn start(self) -> Result<(), crate::Error> {
        let listener = TcpListener::bind(&format!("0.0.0.0:{}", self.port)).await?;
        let async_cache = AsyncCache::<String, Arc<ChannelInfo>>::new();
        let (shutdown_complete_tx, mut shutdown_complete_rx) = mpsc::channel(1);
        let notify_shutdown: Sender<()> = broadcast::channel(1).0;
        info!("server start");
        loop {
            let tcp_stream = tokio::select! {
                _ = signal::ctrl_c() => {
                    drop(shutdown_complete_tx);
                    drop(notify_shutdown);
                    shutdown_complete_rx.recv().await;
                    return Ok(());
                },
                res = listener.accept() => res
            };
            match tcp_stream {
                Ok((stream, socket_addr)) => {
                    info!("connect tcpStream : {:?}", socket_addr);
                    let channel = Channel::new(
                        stream,
                        socket_addr,
                        async_cache.clone(),
                        shutdown_complete_tx.clone(),
                        Shutdown::new(notify_shutdown.subscribe()),
                    );
                    tokio::spawn(async move { channel.run() });
                }
                Err(err) => error!("tcp connect, err: {:?}", err),
            }
        }
    }
}
