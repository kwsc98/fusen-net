use examples::{CERT_PEM, KEY_PEM, init_log};
use fusen_net::server::{Server, ServerConfig};

#[tokio::main]
async fn main() {
    init_log();
    tokio::spawn(async move {
        let result = Server::run(ServerConfig {
            port: 8087,
            quic_lib: fusen_net::quic::Quiclib::S2n,
            cert_pem: CERT_PEM.to_string(),
            priv_key_pem: KEY_PEM.to_string(),
        })
        .await;
        println!("{:?}", result);
    });
    tokio::spawn(async move {
        let result = Server::run(ServerConfig {
            port: 8088,
            quic_lib: fusen_net::quic::Quiclib::GmQuic,
            cert_pem: CERT_PEM.to_string(),
            priv_key_pem: KEY_PEM.to_string(),
        })
        .await;
        println!("{:?}", result);
    });
    let result = Server::run(ServerConfig {
        port: 8089,
        quic_lib: fusen_net::quic::Quiclib::Quin,
        cert_pem: CERT_PEM.to_string(),
        priv_key_pem: KEY_PEM.to_string(),
    })
    .await;
    println!("{:?}", result);
}
