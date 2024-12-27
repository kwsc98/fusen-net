use examples::{init_log, CERT_PEM, KEY_PEM};
use fusen_net::{
    quic::{quin::QuinnEndpoint, s2n::S2nEndpoint},
    server::Server,
};
use structopt::StructOpt;

#[tokio::main]
async fn main() {
    init_log();
    let cli = Cli::from_args();
    let port = cli.port.as_deref().unwrap_or("8089");
    tokio::spawn(async move {
        let result =
            Server::start(S2nEndpoint::make_server_endpoint("8088", CERT_PEM, KEY_PEM).unwrap())
                .await;
        println!("{:?}", result);
    });
    let result =
        Server::start(QuinnEndpoint::make_server_endpoint(port, CERT_PEM, KEY_PEM).unwrap()).await;
    println!("{:?}", result);
}

#[derive(StructOpt)]
struct Cli {
    #[structopt(short = "p", long = "port")]
    port: Option<String>,
}
