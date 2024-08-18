use futures::{SinkExt, StreamExt};
use packet::{builder::Builder, icmp, ip, Packet};
use pnet::{packet::Packet as _, transport::ipv4_packet_iter};
use tokio::sync::mpsc::Receiver;
use tun2::{self, BoxError, Configuration};

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let mut config = tun2::Configuration::default();
    config
        .address((10, 1, 0, 2))
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
        let mut pkt = packet.unwrap().unwrap();
        let mut d1 = pnet::packet::ipv4::MutableIpv4Packet::new(&mut pkt).unwrap();
        println!("{:?} - {:?}", d1.get_destination(), d1.get_source());
        d1.set_source(d1.get_destination());
        d1.set_destination(d1.get_source());
        let _ = framed.send(d1.packet().to_vec()).await;
    }
}
