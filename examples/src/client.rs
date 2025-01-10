use std::{sync::Arc, time::Duration};

use bytes::BytesMut;
use examples::CERT_PEM;
use fusen_common::log::LogConfig;
use fusen_net::{
    client::{self},
    frame::RegisterInfo,
    quic::{quin::QuinnEndpoint, s2n::S2nEndpoint},
};
use structopt::StructOpt;
use tokio::net::UdpSocket;
use tracing::info;

// #[tokio::main]
async fn mai2() {
    let log_config = LogConfig::default()
        .devmode(Some(true))
        .env_filter(Some("client=debug,hyper=debug".to_owned()));
    let _log_work = fusen_common::log::init_log(&log_config, "suanleme-agent");
    let _cli = Cli::from_args();
    info!("start");
    let agent = client::Agent::new("120.46.75.13:8089", "localhost");
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

#[tokio::main]
async fn main() {
    let udp_socket = std::net::UdpSocket::bind("0.0.0.0:1111").unwrap();
    // udp_socket.set_nonblocking(true).unwrap();
    let udp_socket = UdpSocket::from_std(udp_socket).unwrap();
    let udp_socket_1 = Arc::new(udp_socket);
    let udp_socket_2 = udp_socket_1.clone();
    let udp_socket_3 = udp_socket_1.clone();
    let udp_socket_4 = udp_socket_1.clone();

    tokio::spawn(async move {
        // let udp_socket = std::net::UdpSocket::bind("0.0.0.0:1111").unwrap();
        // udp_socket.set_nonblocking(true).unwrap();
        // let udp_socket = UdpSocket::from_std(udp_socket).unwrap();
        loop {
            let mut bytes = BytesMut::new();
            let data = udp_socket_1.recv_buf_from(&mut bytes).await;
            println!("22{:?}-{:?}", data, bytes);
        }
    });
    tokio::spawn(async move {
        // let udp_socket = std::net::UdpSocket::bind("0.0.0.0:1111").unwrap();
        // udp_socket.set_nonblocking(true).unwrap();
        // let udp_socket = UdpSocket::from_std(udp_socket).unwrap();
        loop {
            let mut bytes = BytesMut::new();
            let data = udp_socket_2.recv_buf_from(&mut bytes).await;
            println!("12{:?}-{:?}", data, bytes);
        }
    });
    tokio::spawn(async move {
        // let udp_socket = std::net::UdpSocket::bind("0.0.0.0:1111").unwrap();
        // udp_socket.set_nonblocking(true).unwrap();
        // let udp_socket = UdpSocket::from_std(udp_socket).unwrap();
        loop {
            let mut bytes = BytesMut::new();
            let data = udp_socket_3.recv_buf_from(&mut bytes).await;
            println!("22{:?}-{:?}", data, bytes);
        }
    });

    tokio::spawn(async move {
        // let udp_socket = std::net::UdpSocket::bind("0.0.0.0:1111").unwrap();
        // udp_socket.set_nonblocking(true).unwrap();
        // let udp_socket = UdpSocket::from_std(udp_socket).unwrap();
        loop {
            let mut bytes = BytesMut::new();
            let data = udp_socket_4.recv_buf_from(&mut bytes).await;
            // udp_socket_2.connect("0.0.0.0:2222").await;
            println!("11{:?}-{:?}", data, bytes);
        }
    });

    let udp_socket = UdpSocket::bind("0.0.0.0:2222").await.unwrap();
    for i in 0..2000000 {
        let _ = udp_socket.send_to(b"11", "127.0.0.1:1111").await;
    }
    tokio::signal::ctrl_c().await;
}
