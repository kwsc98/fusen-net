use std::time::Duration;

use examples::init_log;
use fusen_net::{
    client::{self, AgentInfo},
    server,
};
use tokio::sync::mpsc;

#[tokio::main(worker_threads = 512)]
async fn main() {
    init_log();
    let server = server::Server::new("8089");
    tokio::spawn(async move {
        let _ = server.start().await;
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    tokio::spawn(async move {
        let error = client::register("127.0.0.1:8089".to_owned(), "agent1".to_owned()).await;
        println!("error1 -- {:?}", error);
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    tokio::spawn(async move {
        let error = client::agent(
            "127.0.0.1:8089".to_owned(),
            AgentInfo::from("RM-agent1-127.0.0.1:8081-8078"),
        )
        .await;
        println!("error2 -- {:?}", error);
    });

    let mut m = mpsc::channel(1);
    let _: Option<i32> = m.1.recv().await;
}
