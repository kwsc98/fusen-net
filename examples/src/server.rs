use examples::{CERT_PEM, KEY_PEM, init_log};
use fusen_net::{
    quic::{gm_quic::GmQuicEndpoint, quin::QuinnEndpoint, s2n::S2nEndpoint},
    server::NetServer,
};

#[tokio::main]
async fn main() {
    init_log();
    tokio::spawn(async move {
        let result = NetServer
            .run(S2nEndpoint::make_server_endpoint(8087, CERT_PEM, KEY_PEM).unwrap())
            .await;
        println!("{:?}", result);
    });
    tokio::spawn(async move {
        let result = NetServer
            .run(GmQuicEndpoint::make_server_endpoint(8088, CERT_PEM, KEY_PEM).unwrap())
            .await;
        println!("{:?}", result);
    });
    let result = NetServer
        .run(QuinnEndpoint::make_server_endpoint(8089, CERT_PEM, KEY_PEM).unwrap())
        .await;
    println!("{:?}", result);
}
