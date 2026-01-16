use examples::{CERT_PEM, init_log};
use fusen_net::agent::{Agent, AgentConfig};
use tracing::info;

#[tokio::main]
async fn main() {
    init_log();
    info!("start");
    let de = Agent
        .run(AgentConfig {
            quic_lib: fusen_net::quic::Quiclib::S2n,
            server_addr: "82.156.175.196:8087".to_string(),
            server_name: "localhost".to_string(),
            cert_pem: CERT_PEM.to_string(),
            authentication: "10.0.0.3".to_string(),
        })
        .await;
    println!("{de:?}");
}
