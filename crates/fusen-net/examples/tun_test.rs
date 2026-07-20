// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal native TUN diagnostic. It reports packet sizes and never prints
//! packet contents.

use std::{env, error::Error, net::Ipv4Addr};

use fusen_net::tun::{NativeTunFactory, TunConfig, TunFactory};
use ipnet::Ipv4Net;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let address = env::var("FUSEN_TUN_ADDRESS")
        .unwrap_or_else(|_| "10.250.0.2".to_owned())
        .parse::<Ipv4Addr>()?;
    let overlay = env::var("FUSEN_TUN_OVERLAY")
        .unwrap_or_else(|_| "10.250.0.0/24".to_owned())
        .parse::<Ipv4Net>()?;
    let mtu = env::var("FUSEN_MTU")
        .unwrap_or_else(|_| "1100".to_owned())
        .parse::<u16>()?;
    let config = TunConfig {
        name: env::var("FUSEN_TUN_NAME").ok(),
        address,
        overlay,
        mtu,
    };
    let mut device = NativeTunFactory.create(&config).await?;
    println!(
        "TUN {} is up at {address} in {overlay} with MTU {mtu}",
        device.name()
    );

    loop {
        let packet = device.read_packet().await?;
        println!("received an IPv4 packet of {} bytes", packet.len());
    }
}
