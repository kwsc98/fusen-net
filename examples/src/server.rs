use examples::{init_log, CERT_PEM, KEY_PEM};
use fusen_net::{
    authentication::AuthenticationDefault,
    quic::{quin::QuinnEndpoint, s2n::S2nEndpoint},
    server::Server,
};

#[tokio::main]
async fn main() {
    init_log();
    tokio::spawn(async move {
        let result = Server::start(
            S2nEndpoint::make_server_endpoint(8088, CERT_PEM, KEY_PEM).unwrap(),
            AuthenticationDefault,
            Default::default(),
        )
        .await;
        println!("{:?}", result);
    });
    let result = Server::start(
        QuinnEndpoint::make_server_endpoint(8089, CERT_PEM, KEY_PEM).unwrap(),
        AuthenticationDefault,
        Default::default(),
    )
    .await;
    println!("{:?}", result);
}
