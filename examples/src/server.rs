use examples::init_log;
use fusen_net::server;
use structopt::StructOpt;

#[tokio::main(worker_threads = 2)]
async fn main() {
    init_log();
    let cli = Cli::from_args();
    let port = cli.port.as_deref().unwrap_or("8089");
    let server = server::Server::new(port);
    let _ = server.start().await;
}

#[derive(StructOpt)]
struct Cli {
    #[structopt(short = "p", long = "port")]
    port: Option<String>,
}
