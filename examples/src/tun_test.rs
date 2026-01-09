//            DO WHAT THE FUCK YOU WANT TO PUBLIC LICENSE
//                    Version 2, December 2004
//
// Copyleft (ↄ) meh. <meh@schizofreni.co> | http://meh.schizofreni.co
//
// Everyone is permitted to copy and distribute verbatim or modified
// copies of this license document, and changing it is allowed as long
// as the name is changed.
//
//            DO WHAT THE FUCK YOU WANT TO PUBLIC LICENSE
//   TERMS AND CONDITIONS FOR COPYING, DISTRIBUTION AND MODIFICATION
//
//  0. You just DO WHAT THE FUCK YOU WANT TO.

use std::net::Ipv4Addr;

use packet::{PacketMut, ip::Packet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tun::{AbstractDevice, BoxError, DEFAULT_MTU, PACKET_INFORMATION_LENGTH};

pub struct IPPacketCodec;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    main_entry().await?;
    Ok(())
}

async fn main_entry() -> Result<(), BoxError> {
    let mut config = tun::Configuration::default();

    config
        .address((10, 0, 0, 9))
        .netmask((255, 255, 255, 0))
        .destination((10, 0, 0, 1))
        .mtu(DEFAULT_MTU)
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

    let mut dev = tun::create_as_async(&config)?;
    let mut data = [0; (1200 as usize) + PACKET_INFORMATION_LENGTH];
    loop {
        let size = dev.read(&mut data).await?;
        let _ = dev.write(&data[..size]).await;
    }
    
    Ok(())
}
