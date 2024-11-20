use examples::init_log;
use fusen_net::server;
use structopt::StructOpt;

#[tokio::main]
async fn main() {
    init_log();
    let cli = Cli::from_args();
    let port = cli.port.as_deref().unwrap_or("8089");
    let priv_key = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgZwVyc8l7gpw2udg8Glqzv4KSGOi9uVZkgImBfTEZMd6hRANCAATpwIc/fZP0MBigxmZ3f3Ui5S9R485yQX1B+LZqKbbGwhp3qWygnu7SK+fEXGKats8fJ74whuU5bo9PBQfTUzCR";
    let cert = "MIIBXjCCAQSgAwIBAgIUe5OtmYuog9ozO8SLCzIuweYKCC8wCgYIKoZIzj0EAwIwITEfMB0GA1UEAwwWcmNnZW4gc2VsZiBzaWduZWQgY2VydDAgFw03NTAxMDEwMDAwMDBaGA80MDk2MDEwMTAwMDAwMFowITEfMB0GA1UEAwwWcmNnZW4gc2VsZiBzaWduZWQgY2VydDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABOnAhz99k/QwGKDGZnd/dSLlL1HjznJBfUH4tmoptsbCGnepbKCe7tIr58RcYpq2zx8nvjCG5Tluj08FB9NTMJGjGDAWMBQGA1UdEQQNMAuCCWZ1c2VuLW5ldDAKBggqhkjOPQQDAgNIADBFAiBdldLdBl7pN/pIlnETjd4lbZYo/SUU/95K+yABYD1fnwIhAKQCPafzFvETqISrld1yde3rV7BJ/rEj+GUtU2IvifEs";
    let server = server::Server::default()
        .port(port.to_owned())
        .priv_key(priv_key.to_owned())
        .cert(cert.to_owned());
    let _ = server.start().await;
}

#[derive(StructOpt)]
struct Cli {
    #[structopt(short = "p", long = "port")]
    port: Option<String>,
}
