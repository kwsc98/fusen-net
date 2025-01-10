use chrono::{DateTime, Local};
use frame::RegisterInfo;
use fusen_common::fusen_procedural_macro::Data;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::broadcast;
pub mod authentication;
pub mod buffer;
pub mod client;
pub mod common;
pub mod frame;
pub mod quic;
pub mod server;
pub mod shutdown;
pub mod socket;
pub mod utils;
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type FusenFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct MetaData {
    pub inner: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Protocol {
    V4,
    V6,
}

#[derive(Clone, Data, Debug)]
pub struct ChannelInfo {
    register_info: Arc<RegisterInfo>,
    remote_port: u16,
    begin_time: DateTime<Local>,
    sender: broadcast::Sender<()>,
}

impl ChannelInfo {
    pub fn new(
        remote_port: u16,
        register_info: Arc<RegisterInfo>,
        sender: broadcast::Sender<()>,
    ) -> Self {
        Self {
            remote_port,
            register_info,
            begin_time: Local::now(),
            sender,
        }
    }
}
