// SPDX-License-Identifier: Apache-2.0 OR MIT

use bytes::{BufMut as _, Bytes, BytesMut};
use ipnet::Ipv4Net;

use super::{
    ROUTE_COMMAND_TIMEOUT, ROUTE_INSTALL_COMMAND_TIMEOUT, TunError, command::run_route_command,
};

pub(super) const PACKET_OVERHEAD: usize = 4;

pub(super) const fn configure_device(
    _configuration: &mut ::tun::Configuration,
) -> Result<(), TunError> {
    Ok(())
}

pub(super) fn decode_packet(packet: &[u8]) -> Result<Bytes, TunError> {
    if packet.len() < PACKET_OVERHEAD || packet[..4] != [0, 0, 0, 2] {
        return Err(TunError::InvalidPacket(
            "macOS utun frame does not contain an AF_INET header".to_owned(),
        ));
    }
    Ok(Bytes::copy_from_slice(&packet[PACKET_OVERHEAD..]))
}

pub(super) fn encode_packet(packet: Bytes) -> Bytes {
    let mut framed = BytesMut::with_capacity(PACKET_OVERHEAD + packet.len());
    framed.put_u32(2);
    framed.extend_from_slice(&packet);
    framed.freeze()
}

pub(super) async fn install_route(overlay: Ipv4Net, interface: &str) -> Result<(), TunError> {
    let overlay = overlay.to_string();
    run_route_command(
        "route",
        &["-n", "add", "-net", &overlay, "-interface", interface],
        ROUTE_INSTALL_COMMAND_TIMEOUT,
    )
    .await
}

pub(super) async fn remove_route(overlay: Ipv4Net, interface: &str) -> Result<(), TunError> {
    let overlay = overlay.to_string();
    run_route_command(
        "route",
        &["-n", "delete", "-net", &overlay, "-interface", interface],
        ROUTE_COMMAND_TIMEOUT,
    )
    .await
}
