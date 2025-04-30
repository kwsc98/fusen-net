use examples::{CERT_PEM, init_log};
use fusen_net::{
    client::{self},
    frame::Register,
    quic::{gm_quic::GmQuicEndpoint, quin::QuinnEndpoint, s2n::S2nEndpoint},
};
use tracing::info;

#[tokio::main]
async fn main() {
    init_log();
    info!("start");

    tokio::spawn(async move {
        let agent = client::Agent {
            register: "47.93.39.219:8088".to_owned(),
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
    });
    tokio::spawn(async move {
        let agent = client::Agent {
            register: "47.93.39.219:8087".to_owned(),
            server_name: "localhost".to_string(),
        };
        let result = agent
            .register(
                Register {
                    target: "127.0.0.1:8082".to_owned(),
                    remote_port: 9087,
                    tag: Default::default(),
                    token: Default::default(),
                    info: Default::default(),
                },
                S2nEndpoint::make_client_endpoint(CERT_PEM).unwrap(),
            )
            .await;
        info!("s2n_quic {:?}", result);
    });
    let agent = client::Agent {
        register: "47.93.39.219:8089".to_owned(),
        server_name: "localhost".to_string(),
    };
    let result = agent
        .register(
            Register {
                target: "127.0.0.1:8082".to_owned(),
                remote_port: 9089,
                tag: Default::default(),
                token: Default::default(),
                info: Default::default(),
            },
            QuinnEndpoint::make_client_endpoint(CERT_PEM).unwrap(),
        )
        .await;
    info!("quinn_quic {:?}", result);
}
