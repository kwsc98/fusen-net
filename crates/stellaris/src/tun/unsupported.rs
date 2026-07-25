// SPDX-License-Identifier: Apache-2.0 OR MIT

use bytes::Bytes;
use ipnet::Ipv4Net;

use super::TunError;

pub(super) const PACKET_OVERHEAD: usize = 0;

pub(super) const fn configure_device(
    _configuration: &mut ::tun::Configuration,
) -> Result<(), TunError> {
    Ok(())
}

pub(super) fn decode_packet(packet: &[u8]) -> Result<Bytes, TunError> {
    Ok(Bytes::copy_from_slice(packet))
}

pub(super) const fn encode_packet(packet: Bytes) -> Bytes {
    packet
}

pub(super) async fn install_route(_overlay: Ipv4Net, _interface: &str) -> Result<(), TunError> {
    Err(TunError::UnsupportedPlatform)
}

pub(super) async fn remove_route(_overlay: Ipv4Net, _interface: &str) -> Result<(), TunError> {
    Err(TunError::UnsupportedPlatform)
}
