use examples::init_log;
use fusen_net::client;
use structopt::StructOpt;
use tokio::sync::mpsc;
use tracing::{error, info};

#[tokio::main(worker_threads = 512)]
async fn main() {
    init_log();
    let cli = Cli::from_args();
    let Some(server_host) = cli.server_host else {
        error!("server_host must set");
        return;
    };
    let (send, mut recv) = mpsc::channel(1);
    if let Some(tag) = cli.tag {
        let server_host = server_host.clone();
        let send = send.clone();
        tokio::spawn(async move {
            let err = client::Client::register(server_host, tag).await;
            info!("{:?}", err);
            drop(send)
        });
    }

    for item in cli.agent {
        let server_host = server_host.clone();
        let send = send.clone();
        tokio::spawn(async move {
            let agent_info: Vec<&str> = item.split(':').collect();
            info!(
                "start agent target_tag : {}  target_host : {} , local_port : {}",
                agent_info[0], agent_info[1], agent_info[2]
            );
            let err = client::Client::agent(
                server_host,
                agent_info[0].to_owned(),
                agent_info[1].to_owned(),
                agent_info[2].to_owned(),
            )
            .await;
            info!("{:?}", err);
            drop(send)
        });
    }
    let _: Option<i32> = recv.recv().await;
}

#[derive(StructOpt)]
struct Cli {
    #[structopt(short = "s", long = "server_host")]
    server_host: Option<String>,
    #[structopt(short = "t", long = "tag")]
    tag: Option<String>,
    #[structopt(short = "a", long = "agent")]
    agent: Vec<String>,
}
