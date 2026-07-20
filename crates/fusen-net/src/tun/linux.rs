// SPDX-License-Identifier: Apache-2.0 OR MIT

use bytes::{BufMut as _, Bytes, BytesMut};
use ipnet::Ipv4Net;

use super::{
    ROUTE_COMMAND_TIMEOUT, ROUTE_INSTALL_COMMAND_TIMEOUT, TunError, command::run_route_command,
};

pub(super) const PACKET_OVERHEAD: usize = 4;

pub(super) fn configure_device(configuration: &mut ::tun::Configuration) -> Result<(), TunError> {
    configuration.platform_config(|platform| {
        #[allow(deprecated)]
        platform.packet_information(true);
        platform.ensure_root_privileges(true);
    });
    Ok(())
}

pub(super) fn decode_packet(packet: &[u8]) -> Result<Bytes, TunError> {
    if packet.len() < PACKET_OVERHEAD || packet[2..4] != [0x08, 0x00] {
        return Err(TunError::InvalidPacket(
            "Linux TUN frame does not contain an IPv4 protocol header".to_owned(),
        ));
    }
    Ok(Bytes::copy_from_slice(&packet[PACKET_OVERHEAD..]))
}

pub(super) fn encode_packet(packet: Bytes) -> Bytes {
    let mut framed = BytesMut::with_capacity(PACKET_OVERHEAD + packet.len());
    framed.put_u16(0);
    framed.put_u16(0x0800);
    framed.extend_from_slice(&packet);
    framed.freeze()
}

pub(super) async fn install_route(overlay: Ipv4Net, interface: &str) -> Result<(), TunError> {
    let overlay = overlay.to_string();
    run_route_command(
        "ip",
        &["route", "add", &overlay, "dev", interface],
        ROUTE_INSTALL_COMMAND_TIMEOUT,
    )
    .await
}

pub(super) async fn remove_route(overlay: Ipv4Net, interface: &str) -> Result<(), TunError> {
    let overlay = overlay.to_string();
    run_route_command(
        "ip",
        &["route", "del", &overlay, "dev", interface],
        ROUTE_COMMAND_TIMEOUT,
    )
    .await
}
