use examples::init_log;
use fusen_net::client;
use structopt::StructOpt;
use tracing::error;

#[tokio::main(worker_threads = 2)]
async fn main() {
    init_log();
    let cli = Cli::from_args();
    // let Some(register_addr) = cli.register_addr else {
    //     error!("register_addr must set");
    //     return;
    // };
    // let Some(tag) = cli.tag else {
    //     error!("tag must set");
    //     return;
    // };
    // let Some(port) = cli.port else {
    //     error!("port must set");
    //     return;
    // };
    let _ = client::Client::register("127.0.0.1:8089".to_owned(), "testtest".to_owned(), 8078).await;
}

#[derive(StructOpt)]
struct Cli {
    #[structopt(short, long)]
    register_addr: Option<String>,
    #[structopt(short, long)]
    tag: Option<String>,
    #[structopt(short, long)]
    port: Option<u16>,
}
