use std::net::SocketAddr;
use std::sync::Arc;

use cache::AsyncCache;
use channel::Channel;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::mpsc;
use tracing::error;
mod cache;
mod channel;
mod buffer;

pub struct Server {
    port: String,
}

impl Server {
    pub fn new(port: &str) -> Self {
        Self { port: port.into() }
    }
    pub async fn start(self) -> Result<(), crate::Error> {
        let listener = TcpListener::bind(&format!("0.0.0.0:{}", self.port)).await?;
        let async_cache = AsyncCache::<String, Arc<SocketAddr>>::new();
        let (shutdown_complete_tx, mut shutdown_complete_rx) = mpsc::channel(1);

        loop {
            let tcp_stream = tokio::select! {
                _ = signal::ctrl_c() => {
                    drop(shutdown_complete_tx);
                    shutdown_complete_rx.recv().await;
                    return Ok(());
                },
                res = listener.accept() => res
            };
            match tcp_stream {
                Ok((stream, socket_addr)) => {
                    let channel = Channel::new(
                        stream,
                        socket_addr,
                        async_cache.clone(),
                        shutdown_complete_tx.clone(),
                    );
                }
                Err(err) => error!("tcp connect, err: {:?}", err),
            }
        }
    }
}
