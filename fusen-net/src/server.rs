use crate::{
    common::ConnectError,
    quic::{Connection, Endpoint},
};

pub struct NetServer;

impl NetServer {
    pub async fn run(&self, endpoint: impl Endpoint) -> Result<(), ConnectError> {
        while let Ok(connect) = endpoint.accept().await {
            tokio::spawn(async move {
                if let Ok((read_stream, write_stream)) = connect.accept_bi().await {
                    
                }
            });
        }
        todo!()
    }
}
