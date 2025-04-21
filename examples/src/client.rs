use std::time::Duration;

use examples::{CERT_PEM, init_log};
use fusen_net::{
    client::{self},
    frame::Register,
    quic::{quin::QuinnEndpoint, s2n::S2nEndpoint},
};
use tracing::info;

#[tokio::main]
async fn main() {
    init_log();
    info!("start");
    tokio::spawn(async move {
        let agent = client::Agent {
            register: "127.0.0.1:8089".to_owned(),
            server_name: "localhost".to_string(),
        };
        let result = agent
            .register(
                Register {
                    target: "127.0.0.1:8082".to_owned(),
                    tag: Default::default(),
                    token: Default::default(),
                    info: Default::default(),
                },
                QuinnEndpoint::make_client_endpoint(CERT_PEM).unwrap(),
            )
            .await;
        info!("{:?}", result);
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let agent = client::Agent {
        register: "127.0.0.1:8088".to_owned(),
        server_name: "localhost".to_string(),
    };
    let result = agent
        .register(
            Register {
                target: "127.0.0.1:8082".to_owned(),
                tag: Default::default(),
                token: Default::default(),
                info: Default::default(),
            },
            S2nEndpoint::make_client_endpoint(CERT_PEM).unwrap(),
        )
        .await;
    info!("{:?}", result);
}
