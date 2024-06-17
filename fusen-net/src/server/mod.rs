use crate::quic::support::make_server_endpoint;
use crate::shutdown::Shutdown;
use crate::ChannelInfo;
use cache::AsyncCache;
use channel::Channel;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::broadcast::Sender;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info};
pub mod cache;
mod channel;

pub struct Server {
    port: String,
}

impl Server {
    pub fn new(port: &str) -> Self {
        Self { port: port.into() }
    }
    pub async fn start(self) -> Result<(), crate::Error> {
        let bind_addr = format!("0.0.0.0:{}", self.port).parse()?;
        let endpoint = make_server_endpoint(bind_addr)?.0;
        let async_cache = AsyncCache::<String, Arc<ChannelInfo>>::new();
        let (shutdown_complete_tx, mut shutdown_complete_rx) = mpsc::channel(1);
        let notify_shutdown: Sender<()> = broadcast::channel(1).0;
        info!("server start");
        loop {
            let udp_stream = tokio::select! {
                _ = signal::ctrl_c() => {
                    drop(shutdown_complete_tx);
                    drop(notify_shutdown);
                    shutdown_complete_rx.recv().await;
                    return Ok(());
                },
                res = endpoint.accept() => res
            };
            match udp_stream {
                Some(incoming) => {
                    let async_cache_clone = async_cache.clone();
                    let shutdown_complete_tx_clone = shutdown_complete_tx.clone();
                    let notify_shutdown = notify_shutdown.subscribe();
                    tokio::spawn(async move {
                        debug!("connect udpStream : {:?}", incoming);
                        let connection = match incoming.await {
                            Ok(connection) => connection,
                            Err(error) => {
                                info!("erro : {:?}", error);
                                return;
                            }
                        };
                        let socket_addr = connection.remote_address();
                        let channel = Channel::new(
                            connection,
                            socket_addr,
                            async_cache_clone,
                            shutdown_complete_tx_clone,
                            Shutdown::new(notify_shutdown),
                        );
                        let error = channel.run().await;
                        debug!("udp_stream end : {:?}", error);
                    });
                }
                None => info!("udp connect, err"),
            }
        }
    }
}
