use std::time::Duration;

use examples::init_log;
use fusen_net::{
    client::{self, AgentInfo},
    shutdown::ShutdownV2,
};
use structopt::StructOpt;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

#[tokio::main(worker_threads = 512)]
async fn main() {
    init_log();
    let cli = Cli::from_args();
    let Some(server_host) = cli.server_host else {
        error!("server_host must set");
        return;
    };
    let Some(tag) = cli.tag else {
        error!("tag must set");
        return;
    };
    let (send, mut recv) = mpsc::channel(1);
    let server_host_clone = server_host.clone();
    let mut shutdown = ShutdownV2::default();
    let send_clone = send.clone();
    tokio::spawn(async move {
        loop {
            let res = tokio::select! {
                res = client::register(server_host_clone.clone(), tag.clone()) => res,
                _ = shutdown.recv() => {
                    debug!("shutdown");
                    break;
                }
            };
            let _ = tokio::time::sleep(Duration::from_secs(10)).await;
            info!("error : {:?} , try register again", res);
        }
        drop(send_clone);
    });
    for item in cli.agent {
        let server_host = server_host.clone();
        tokio::spawn(async move {
            let agent_info: Vec<&str> = item.split('-').collect();
            info!(
                "start agent mode {} target_tag : {}  target_host : {} , local_port : {}",
                agent_info[0], agent_info[1], agent_info[2], agent_info[3]
            );
            let err = client::agent(server_host, AgentInfo::from(item.as_str())).await;
            info!("{:?}", err);
        });
    }
    drop(send);
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
