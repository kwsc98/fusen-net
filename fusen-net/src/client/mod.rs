use crate::{
    frame::RegisterInfo,
    quic::{make_client_endpoint, EndPoint},
};
use fusen_common::BoxError;
mod channel;

pub struct Agent {
    register: String,
    server_cert: String,
    server_name: String,
}

impl Agent {
    pub fn new(register: &str, server_cert: &str, server_name: &str) -> Self {
        Agent {
            register: register.to_owned(),
            server_cert: server_cert.to_owned(),
            server_name: server_name.to_owned(),
        }
    }

    pub async fn register(&self, register: RegisterInfo) -> Result<(), BoxError> {
        let endpoint = make_client_endpoint(&self.server_cert)?;
        let connection = endpoint
            .connect(self.register.parse()?, self.server_name.to_owned())
            .await?;
        channel::register(connection, register).await
    }
}
