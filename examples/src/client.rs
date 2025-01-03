use examples::CERT_PEM;
use fusen_common::logs::LogConfig;
use fusen_net::{
    client::{self},
    frame::RegisterInfo,
    quic::quin::QuinnEndpoint,
};
use structopt::StructOpt;
use tracing::info;

#[tokio::main]
async fn main() {
    let log_config = LogConfig::default()
        .devmode(Some(true))
        .env_filter(Some("client=debug,hyper=debug".to_owned()));
    let _log_work = fusen_common::logs::init_log(&log_config, "suanleme-agent");
    let _cli = Cli::from_args();
    info!("start");
    let agent = client::Agent::new("127.0.0.1:7099", "localhost");
    let result = agent
        .register(
            RegisterInfo::default()
                .protocol(0)
                .target_host("127.0.0.1:7099".to_owned())
                .remote_port(Some(1026)),
            QuinnEndpoint::make_client_endpoint(CERT_PEM).unwrap(),
        )
        .await;
    info!("{:?}", result);
}

#[derive(StructOpt)]
struct Cli {
    #[structopt(short = "p", long = "port")]
    _port: Option<String>,
}
