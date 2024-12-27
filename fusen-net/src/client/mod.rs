use crate::{frame::RegisterInfo, quic::Endpoint};
use fusen_common::BoxError;
mod channel;

pub struct Agent {
    register: String,
    server_name: String,
}

impl Agent {
    pub fn new(register: &str, server_name: &str) -> Self {
        Agent {
            register: register.to_owned(),
            server_name: server_name.to_owned(),
        }
    }

    pub async fn register(
        &self,
        register: RegisterInfo,
        endpoint: impl Endpoint,
    ) -> Result<(), BoxError> {
        let connection = endpoint
            .connect(self.register.parse()?, self.server_name.to_owned())
            .await?;
        channel::register(connection, register).await
    }
}
