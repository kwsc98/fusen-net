use crate::common::get_uuid;
use crate::quic::support::{generate_signed, make_server_endpoint};
use crate::shutdown::Shutdown;
use crate::ChannelInfo;
use channel::Channel;
use fusen_common::fusen_procedural_macro::Data;
use fusen_common::utils::map::AsyncMap;
use tokio::signal;
use tokio::sync::broadcast::Sender;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info};
mod channel;
mod register;

#[derive(Default, Data)]
pub struct Server {
    port: String,
    priv_key: String,
    cert: String,
}

impl Server {
    pub async fn start(self) -> Result<(), crate::Error> {
        let bind_addr = format!("0.0.0.0:{}", self.port).parse()?;
        let endpoint =
            make_server_endpoint(bind_addr, generate_signed(&self.priv_key, &self.cert)?)?;
        let (shutdown_complete_tx, mut shutdown_complete_rx) = mpsc::channel(1);
        let channel_info: AsyncMap<String, AsyncMap<String, ChannelInfo>> = AsyncMap::new();
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
                    let shutdown_complete_tx_clone = shutdown_complete_tx.clone();
                    let notify_shutdown = notify_shutdown.subscribe();
                    let channel_info = channel_info.clone();
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
                        let uuid = get_uuid();
                        let async_map = AsyncMap::new();
                        channel_info.insert(uuid.clone(), async_map.clone()).await;
                        let channel = Channel::new(
                            connection,
                            socket_addr,
                            async_map,
                            shutdown_complete_tx_clone,
                            Shutdown::new(notify_shutdown),
                        );
                        let error = channel.run().await;
                        let _ = channel_info.remove(uuid).await;
                        debug!("udp_stream end : {:?}", error);
                    });
                }
                None => info!("udp connect, err"),
            }
        }
    }
}
