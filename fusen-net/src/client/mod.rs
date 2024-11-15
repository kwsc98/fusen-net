use std::collections::HashMap;

use fusen_common::{utils::map::AsyncMap, BoxError};
use tokio::sync::Mutex;
use tracing::error;

use crate::{frame::RegisterInfo, quic::support::make_client_endpoint, ChannelInfo};

pub struct Agent {
    register: String,
    channel_info: Mutex<HashMap<String, RegisterInfo>>,
}

impl Agent {
    pub fn new(register: String) -> Self {
        let quic_client = make_client_endpoint(register);
        Agent {
            register,
            channel_info: Mutex::new(HashMap::new()),
        }
    }

    pub async fn register(&mut self, info: RegisterInfo) -> Result<(), BoxError> {
        let map = self.channel_info.lock().await;
        if map.contains_key(info.get_target_host()) {
            let info = format!("RegisterInfo Already exist : {:?}", info);
            error!(info);
            return Err(info.into());
        }


        Ok(())
    }
}
