use std::collections::HashMap;

use ::packet::ip;
use bytes::Bytes;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::error::FusenNetError;

pub struct Register {
    pub tun_ip: String,
    pub pack_send: tokio::sync::mpsc::UnboundedSender<Bytes>,
}

pub async fn init_gateway()
-> Result<UnboundedSender<(Register, UnboundedReceiver<Bytes>)>, FusenNetError> {
    let (send, mut recv) =
        tokio::sync::mpsc::unbounded_channel::<(Register, UnboundedReceiver<Bytes>)>();
    let (packet_sender, packet_recv) = tokio::sync::mpsc::unbounded_channel::<Packet>();
    let register_sender = router(packet_recv).await?;
    tokio::spawn(async move {
        loop {
            if let Some((register, mut recv)) = recv.recv().await {
                register_sender.send(register);
                let packet_sender_clone = packet_sender.clone();
                tokio::spawn(async move {
                    while let Some(pack) = recv.recv().await {
                        //解析目标ip的地址
                        if let Ok(packet) = ip::v4::Packet::new(pack.as_ref()) {
                            packet_sender_clone.send(Packet {
                                tun_ip: packet.destination().to_string(),
                                bytes: pack,
                            });
                        }
                    }
                });
            }
        }
    });
    Ok(send)
}

struct Packet {
    tun_ip: String,
    bytes: Bytes,
}

enum Frame {
    Packet(Option<Packet>),
    Register(Option<Register>),
}

async fn router(
    mut recv: UnboundedReceiver<Packet>,
) -> Result<UnboundedSender<Register>, FusenNetError> {
    let (register_sender, mut registry_recv) = tokio::sync::mpsc::unbounded_channel::<Register>();
    tokio::spawn(async move {
        let mut hash_map: HashMap<String, UnboundedSender<Bytes>> = HashMap::new();
        loop {
            let frame = tokio::select! {
                packet = recv.recv() => {
                    Frame::Packet(packet)
                }
                register = registry_recv.recv() => {
                    Frame::Register(register)
                }
            };
            if let Frame::Packet(Some(packet)) = frame {
                if let Some(sender) = hash_map.get_mut(&packet.tun_ip) {
                    if let Err(_error) = sender.send(packet.bytes) {
                        let _ = hash_map.remove(&packet.tun_ip);
                    }
                }
            } else if let Frame::Register(Some(register)) = frame {
                let _ = hash_map.insert(register.tun_ip, register.pack_send);
            }
        }
    });
    Ok(register_sender)
}
