use std::collections::HashMap;

use serde::{Deserialize, Serialize};
pub mod buffer;
pub mod client;
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelInfo {
    tag: String,
    protocol: Protocol,
    net_ip: String,
    net_port: u16,
    mate_data: MetaData,
}

impl ChannelInfo {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            tag: Default::default(),
            protocol,
            net_ip: Default::default(),
            net_port: Default::default(),
            mate_data: Default::default(),
        }
    }

    pub fn set_tag(&mut self, tag: String) {
        self.tag = tag;
    }
}
