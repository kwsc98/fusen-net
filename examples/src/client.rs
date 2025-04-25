use examples::{CERT_PEM, init_log};
use fusen_net::{
    client::{self},
    frame::Register,
    quic::gm_quic::GmQuicEndpoint,
};
use tracing::info;

#[tokio::main]
async fn main() {
    init_log();
    info!("start");

    let agent = client::Agent {
        register: "127.0.0.1:8088".to_owned(),
        server_name: "localhost".to_string(),
    };
    let result = agent
        .register(
            Register {
                target: "127.0.0.1:8082".to_owned(),
                remote_port: 9088,
                tag: Default::default(),
                token: Default::default(),
                info: Default::default(),
            },
            GmQuicEndpoint::make_client_endpoint(CERT_PEM).unwrap(),
        )
        .await;
    info!("gm_quic {:?}", result);
}
