use std::collections::HashMap;

use crate::Channel;

pub struct Server {
    addr: String,
    cache: HashMap<String, Channel>,
}

impl Server {
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.into(),
            cache: Default::default(),
        }
    }
    pub fn start(self) {}
}
