use examples::init_log;
use fusen_net::{
    client::{self},
    frame::RegisterInfo,
};
use structopt::StructOpt;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    init_log();
    let _cli = Cli::from_args();
    let cert = "MIIBXjCCAQSgAwIBAgIUe5OtmYuog9ozO8SLCzIuweYKCC8wCgYIKoZIzj0EAwIwITEfMB0GA1UEAwwWcmNnZW4gc2VsZiBzaWduZWQgY2VydDAgFw03NTAxMDEwMDAwMDBaGA80MDk2MDEwMTAwMDAwMFowITEfMB0GA1UEAwwWcmNnZW4gc2VsZiBzaWduZWQgY2VydDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABOnAhz99k/QwGKDGZnd/dSLlL1HjznJBfUH4tmoptsbCGnepbKCe7tIr58RcYpq2zx8nvjCG5Tluj08FB9NTMJGjGDAWMBQGA1UdEQQNMAuCCWZ1c2VuLW5ldDAKBggqhkjOPQQDAgNIADBFAiBdldLdBl7pN/pIlnETjd4lbZYo/SUU/95K+yABYD1fnwIhAKQCPafzFvETqISrld1yde3rV7BJ/rEj+GUtU2IvifEs";
    let agent = client::Agent::new("127.0.0.1:8089", cert, "fusen-net")
        .await
        .unwrap();
    let _ = agent
        .register(
            RegisterInfo::default()
                .protocol(0)
                .target_host("127.0.0.1:8080".to_owned()),
        )
        .await;
    let (_s, mut r) = mpsc::channel::<()>(1);
    r.recv().await;
}

#[derive(StructOpt)]
struct Cli {
    #[structopt(short = "p", long = "port")]
    _port: Option<String>,
}
