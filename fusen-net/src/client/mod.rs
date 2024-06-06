use crate::buffer::Buffer;
use crate::common::get_uuid;
use crate::connection;
use crate::frame::{ConnectionInfo, Frame, RegisterInfo};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

pub struct Client;

impl Client {
    pub async fn register(register_addr: String, tag: String) -> Result<(), crate::Error> {
        let host: SocketAddr = register_addr.parse().unwrap();
        let tcp_stream = TcpStream::connect(host).await?;
        let mut buffer = Buffer::new(tcp_stream);
        let _ = buffer
            .write_frame(&Frame::Register(RegisterInfo::new(tag)))
            .await;
        let _frame = tokio::select! {
            res = buffer.read_frame() => res?,
            _ = tokio::time::sleep(Duration::from_secs(3)) => return Err("register time out".into()),
        };
        loop {
            match buffer.read_frame().await? {
                Frame::Ping => {
                    let _ = buffer.write_frame(&Frame::Ack).await;
                }
                Frame::Connection(connection) => {
                    let host = host.clone();
                    tokio::spawn(async move {
                        let tcp_stream = TcpStream::connect(connection.get_target_host())
                            .await
                            .unwrap();
                        let buffer = Buffer::new(tcp_stream);
                        let tcp_stream = TcpStream::connect(host).await.unwrap();
                        let mut buffer2 = Buffer::new(tcp_stream);
                        let _ = buffer2
                            .write_frame(&Frame::TargetConnection(connection))
                            .await;
                        let _frame = tokio::select! {
                            res = buffer2.read_frame() => res.unwrap(),
                            _ = tokio::time::sleep(Duration::from_secs(3)) => panic!("connect time out"),
                        };
                        let _ = connection::connect(buffer, buffer2).await;
                    });
                }
                frame => info!("rev not support frame : {:?}", frame),
            }
        }
    }

    pub async fn agent(
        register_addr: String,
        target_tag: String,
        target_host: String,
        agent_port: String,
    ) -> Result<(), crate::Error> {
        let listener = TcpListener::bind(&format!("0.0.0.0:{}", agent_port)).await?;
        while let Ok(tcp_stream) = listener.accept().await {
            let register_addr: String = register_addr.clone();
            let target_tag: String = target_tag.clone();
            let target_host: String = target_host.clone();
            let buffer2 = Buffer::new(tcp_stream.0);
            tokio::spawn(async move {
                let host: SocketAddr = register_addr.parse().unwrap();
                let tcp_stream = TcpStream::connect(host).await.unwrap();
                let mut buffer = Buffer::new(tcp_stream);
                let _ = buffer
                    .write_frame(&Frame::Connection(ConnectionInfo::new(
                        get_uuid(),
                        target_tag,
                        target_host,
                    )))
                    .await;
                let _frame = tokio::select! {
                    res = buffer.read_frame() => res.unwrap(),
                    _ = tokio::time::sleep(Duration::from_secs(3)) => panic!("agent time out"),
                };
                let _ = connection::connect(buffer, buffer2).await;
            });
        }
        Ok(())
    }
}
