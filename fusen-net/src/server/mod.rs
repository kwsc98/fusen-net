use crate::authentication::Authentication;
use crate::common::get_uuid;
use crate::quic::{Connection, Endpoint};
use crate::shutdown::Shutdown;
use crate::ChannelInfo;
use channel::Channel;
use fusen_common::utils::map::AsyncMap;
use tokio::signal;
use tokio::sync::broadcast::Sender;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info};
mod channel;
mod register;

pub struct Server;

impl Server {
    pub async fn start(
        endpoint: impl Endpoint,
        authentication: impl Authentication,
    ) -> Result<(), crate::Error> {
        let (shutdown_complete_tx, mut shutdown_complete_rx) = mpsc::channel(1);
        let channel_info: AsyncMap<String, ChannelInfo> = AsyncMap::new();
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
                Ok(connection) => {
                    let shutdown_complete_tx_clone = shutdown_complete_tx.clone();
                    let notify_shutdown = notify_shutdown.subscribe();
                    let channel_info_clone = channel_info.clone();
                    let uuid = get_uuid();
                    tokio::spawn(async move {
                        debug!("connect udpStream : {:?}", connection);
                        let socket_addr = connection.remote_address();
                        let channel = Channel::new(
                            uuid.clone(),
                            socket_addr,
                            channel_info_clone.clone(),
                            shutdown_complete_tx_clone,
                            Shutdown::new(notify_shutdown),
                        );
                        let error = channel.run(connection, authentication).await;
                        let _ = channel_info_clone.remove(uuid).await;
                        debug!("udp_stream end : {:?}", error);
                    });
                }
                Err(error) => info!("udp connect error : {:?}", error),
            }
        }
    }
}
