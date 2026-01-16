use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use dashmap::DashMap;
use tokio::sync::mpsc::UnboundedSender;
use tracing::info;

use crate::error::FusenNetError;

#[derive(Clone)]
pub struct Register {
    pub tun_ip: String,
    pub pack_tun_send: tokio::sync::mpsc::UnboundedSender<Bytes>,
}

pub async fn init_gateway() -> Result<
    (
        UnboundedSender<Register>,
        Arc<DashMap<String, UnboundedSender<Bytes>>>,
    ),
    FusenNetError,
> {
    let (send, mut recv) = tokio::sync::mpsc::unbounded_channel::<Register>();
    let map = Arc::new(DashMap::new());
    let map_clone = map.clone();
    let map_clone_3 = map.clone();
    tokio::spawn(async move {
        loop {
            if let Some(register) = recv.recv().await {
                let _ = map_clone.insert(register.tun_ip.clone(), register.pack_tun_send.clone());
                info!("{} 已注册", register.tun_ip);
                let map_clone_2 = map_clone.clone();
                tokio::spawn(async move {
                    register.pack_tun_send.closed().await;
                    let _ = map_clone_2.remove(&register.tun_ip);
                    info!("{} 已断开连接", register.tun_ip);
                });
            }
        }
    });
    tokio::spawn(async move {
        loop {
            loop {
                let _ = tokio::time::sleep(Duration::from_secs(10)).await;
                info!("存活的agent {map_clone_3:?}");
            }
        }
    });
    Ok((send, map))
}
