use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use crate::frame::Frame;
use crate::{buffer::Buffer, frame, ChannelInfo};
use tokio::net::TcpSocket;
use tracing::error;
use tracing::info;

pub struct Client;

impl Client {
    pub async fn register(
        register_addr: String,
        tag: String,
        port: u16,
    ) -> Result<(), crate::Error> {
        let host: Vec<&str> = register_addr.split(":").collect();
        let ip: Vec<&str> = host[0].split(".").collect();
        let ip = ip.iter().enumerate().fold([0_u8; 4], |mut res, e| {
            res[e.0] = e.1.parse().unwrap();
            res
        });
        let server_port: u16 = host[1].parse()?;
        let server_addr = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]),
            server_port,
        ));
        let local_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), port));
        let server_sock = TcpSocket::new_v4().unwrap();
        #[cfg(target_family = "unix")]
        {
            server_sock.set_reuseport(true).unwrap();
        }
        server_sock.set_reuseaddr(true).unwrap();
        server_sock.bind(local_addr).unwrap();
        let tcp_stream = server_sock.connect(server_addr).await?;
        let mut info = ChannelInfo::new(crate::Protocol::V4);
        info.set_tag(tag);
        let frame = frame::Frame::Register(info);
        let mut buffer = Buffer::new(tcp_stream);
        let _ = buffer.write_frame(&frame).await?;
        match buffer.read_frame_block().await {
            Ok(frame) => {
                info!("receiver server frame : {:?}", frame);
                match frame {
                    Frame::ACK => loop {
                        match buffer.read_frame().await {
                            Ok(frame) => {
                                let _ = tokio::time::sleep(Duration::from_secs(1));
                                if let Some(frame) = frame {
                                    info!("receiver server frame : {:?}", frame);
                                }
                            }
                            Err(error) => error!("{:?}", error.to_string()),
                        }
                    },
                    _ => error!("receiver error frame"),
                }
            }
            Err(error) => error!("read server timeout : {:?}", error),
        };
        Ok(())
    }
}
