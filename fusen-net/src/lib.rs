use std::{collections::HashMap, net::SocketAddr};

use frame::{Frame, RegisterInfo};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
pub mod buffer;
pub mod client;
pub mod common;
pub mod connection;
pub mod frame;
pub mod server;
pub mod shutdown;
pub mod socket;
pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct MetaData {
    pub inner: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Protocol {
    V4,
    V6,
}

#[derive(Clone, Debug)]
pub struct ChannelInfo {
    _net_addr: SocketAddr,
    register_info: RegisterInfo,
    sender: UnboundedSender<Frame>,
}

impl ChannelInfo {
    pub fn new(
        _net_addr: SocketAddr,
        register_info: RegisterInfo,
        sender: UnboundedSender<Frame>,
    ) -> Self {
        Self {
            _net_addr,
            register_info,
            sender,
        }
    }
}
