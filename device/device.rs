use futures::{SinkExt, StreamExt};
use packet::{builder::Builder, icmp, ip, Packet};
use tokio::sync::mpsc::Receiver;
use tun2::{self, BoxError, Configuration};

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let mut config = tun2::Configuration::default();
    config
        .address((10, 0, 0, 2))
        .netmask((255, 255, 0, 0))
        .destination((10, 0, 0, 1))
        .up();

    #[cfg(target_os = "linux")]
    config.platform_config(|config| {
        #[allow(deprecated)]
        config.packet_information(true);
        config.ensure_root_privileges(true);
    });

    #[cfg(target_os = "windows")]
    config.platform_config(|config| {
        config.device_guid(9099482345783245345345_u128);
    });

    let dev = tun2::create_as_async(&config)?;
    let mut framed = dev.into_framed();
    loop {
        let packet = framed.next().await;
        let pkt = packet.unwrap().unwrap();
        match ip::Packet::new(pkt) {
            Ok(ip::Packet::V4(pkt)) => {
                if let Ok(icmp) = icmp::Packet::new(pkt.payload()) {
                    if let Ok(icmp) = icmp.echo() {
                        println!("{:?} - {:?}", icmp.sequence(), pkt.destination());
                        let reply = ip::v4::Builder::default()
                            .id(0x42)?
                            .ttl(64)?
                            .source(pkt.destination())?
                            .destination(pkt.source())?
                            .icmp()?
                            .echo()?
                            .reply()?
                            .identifier(icmp.identifier())?
                            .sequence(icmp.sequence())?
                            .payload(icmp.payload())?
                            .build()?;
                        framed.send(reply).await?;
                    }
                }
            }
            Err(err) => println!("Received an invalid packet: {:?}", err),
            _ => {}
        }
    }
}
