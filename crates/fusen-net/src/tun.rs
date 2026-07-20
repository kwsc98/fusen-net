// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Platform-neutral TUN and route-management boundaries.

use std::net::Ipv4Addr;

use async_trait::async_trait;
use bytes::Bytes;
use ipnet::Ipv4Net;
use tun::AbstractDevice as _;
use uuid::Uuid;

use crate::data_plane::{MAX_OVERLAY_MTU, MIN_OVERLAY_MTU};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod command;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
use self::macos as platform;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use self::unsupported as platform;
#[cfg(target_os = "windows")]
use self::windows as platform;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunConfig {
    pub name: Option<String>,
    pub address: Ipv4Addr,
    pub overlay: Ipv4Net,
    pub mtu: u16,
}

#[async_trait]
pub trait PacketDevice: Send + 'static {
    fn name(&self) -> &str;

    fn mtu(&self) -> u16;

    async fn read_packet(&mut self) -> Result<Bytes, TunError>;

    async fn write_packet(&mut self, packet: Bytes) -> Result<(), TunError>;
}

#[async_trait]
pub trait TunFactory: Send + Sync + 'static {
    async fn create(&self, config: &TunConfig) -> Result<Box<dyn PacketDevice>, TunError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteLease {
    id: Uuid,
    pub interface_name: String,
    pub overlay: Ipv4Net,
}

impl RouteLease {
    pub fn new(interface_name: impl Into<String>, overlay: Ipv4Net) -> Self {
        Self {
            id: Uuid::new_v4(),
            interface_name: interface_name.into(),
            overlay,
        }
    }

    pub const fn id(&self) -> Uuid {
        self.id
    }
}

#[async_trait]
pub trait RouteManager: Send + Sync + 'static {
    /// Installs the route and returns its cleanup lease. Implementations must
    /// bound this operation and compensate internally if a timeout leaves the
    /// underlying system state uncertain.
    async fn install_overlay_route(
        &self,
        config: &TunConfig,
        interface_name: &str,
    ) -> Result<RouteLease, TunError>;

    async fn remove_overlay_route(&self, lease: RouteLease) -> Result<(), TunError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeTunFactory;

#[async_trait]
impl TunFactory for NativeTunFactory {
    async fn create(&self, config: &TunConfig) -> Result<Box<dyn PacketDevice>, TunError> {
        if !(MIN_OVERLAY_MTU..=MAX_OVERLAY_MTU).contains(&usize::from(config.mtu)) {
            return Err(TunError::InvalidConfiguration(format!(
                "IPv4 overlay MTU must be between {MIN_OVERLAY_MTU} and {MAX_OVERLAY_MTU}"
            )));
        }
        if !config.overlay.contains(&config.address)
            || config.address == config.overlay.network()
            || config.address == config.overlay.broadcast()
            || config.address.is_unspecified()
            || config.address.is_multicast()
            || config.address.is_broadcast()
        {
            return Err(TunError::InvalidConfiguration(
                "TUN address is not a usable overlay unicast address".to_owned(),
            ));
        }

        let mut native = tun::Configuration::default();
        native
            .address(config.address)
            // A host route prevents the TUN library from implicitly taking
            // ownership of anything broader than the assigned address. The
            // RouteManager installs the exact overlay route separately.
            .netmask(Ipv4Addr::new(255, 255, 255, 255))
            .mtu(config.mtu)
            .up();
        if let Some(name) = config.name.as_deref() {
            native.tun_name(name);
        }
        platform::configure_device(&mut native)?;

        let device = tun::create_as_async(&native).map_err(TunError::Create)?;
        let name = device.tun_name().map_err(TunError::Create)?;
        Ok(Box::new(NativePacketDevice {
            device,
            name,
            mtu: config.mtu,
        }))
    }
}

struct NativePacketDevice {
    device: tun::AsyncDevice,
    name: String,
    mtu: u16,
}

#[async_trait]
impl PacketDevice for NativePacketDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn mtu(&self) -> u16 {
        self.mtu
    }

    async fn read_packet(&mut self) -> Result<Bytes, TunError> {
        use tokio::io::AsyncReadExt as _;

        let mut buffer = vec![0_u8; usize::from(self.mtu) + platform::PACKET_OVERHEAD];
        let size = self
            .device
            .read(&mut buffer)
            .await
            .map_err(TunError::Native)?;
        decode_native_packet(&buffer[..size])
    }

    async fn write_packet(&mut self, packet: Bytes) -> Result<(), TunError> {
        use tokio::io::AsyncWriteExt as _;

        if packet.len() > usize::from(self.mtu) {
            return Err(TunError::InvalidPacket(
                "packet exceeds configured TUN MTU".to_owned(),
            ));
        }
        let framed = encode_native_packet(packet);
        self.device
            .write_all(&framed)
            .await
            .map_err(TunError::Native)
    }
}

fn decode_native_packet(packet: &[u8]) -> Result<Bytes, TunError> {
    platform::decode_packet(packet)
}

fn encode_native_packet(packet: Bytes) -> Bytes {
    platform::encode_packet(packet)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeRouteManager;

const ROUTE_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const ROUTE_INSTALL_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[async_trait]
impl RouteManager for NativeRouteManager {
    async fn install_overlay_route(
        &self,
        config: &TunConfig,
        interface_name: &str,
    ) -> Result<RouteLease, TunError> {
        let started = std::time::Instant::now();
        match platform::install_route(config.overlay, interface_name).await {
            Ok(()) => Ok(RouteLease::new(interface_name, config.overlay)),
            Err(error @ TunError::RouteCommandTimeout { .. }) => {
                let remaining = ROUTE_COMMAND_TIMEOUT.saturating_sub(started.elapsed());
                let rollback = tokio::time::timeout(
                    remaining,
                    platform::remove_route(config.overlay, interface_name),
                )
                .await;
                match rollback {
                    Ok(Ok(())) => Err(error),
                    Ok(Err(rollback)) => Err(TunError::RouteInstallRollbackFailed {
                        install: Box::new(error),
                        rollback: Box::new(rollback),
                    }),
                    Err(_) => Err(TunError::RouteInstallRollbackTimeout {
                        install: Box::new(error),
                    }),
                }
            }
            Err(error) => Err(error),
        }
    }

    async fn remove_overlay_route(&self, lease: RouteLease) -> Result<(), TunError> {
        platform::remove_route(lease.overlay, &lease.interface_name).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TunError {
    #[error("TUN device is closed")]
    Closed,
    #[error("invalid TUN configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid TUN packet: {0}")]
    InvalidPacket(String),
    #[error("native TUN operation failed: {0}")]
    Native(#[source] std::io::Error),
    #[error("native TUN creation or configuration failed: {0}")]
    Create(#[source] tun::Error),
    #[error("route command {program} failed with status {status:?}: {stderr}")]
    RouteCommand {
        program: String,
        status: Option<i32>,
        stderr: String,
    },
    #[error("route command {program} did not finish within its deadline")]
    RouteCommandTimeout { program: String },
    #[error("route installation failed ({install}); compensating cleanup failed ({rollback})")]
    RouteInstallRollbackFailed {
        install: Box<TunError>,
        rollback: Box<TunError>,
    },
    #[error(
        "route installation failed ({install}) and compensating cleanup exceeded the shared five-second budget"
    )]
    RouteInstallRollbackTimeout { install: Box<TunError> },
    #[error("native route management is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("TUN operation failed: {0}")]
    Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_packet_framing_round_trips_ipv4() {
        let packet = Bytes::from_static(&[
            0x45, 0, 0, 20, 0, 0, 0, 0, 64, 17, 0, 0, 10, 42, 0, 2, 10, 42, 0, 3,
        ]);
        let framed = encode_native_packet(packet.clone());
        assert_eq!(decode_native_packet(&framed).expect("decode"), packet);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn native_packet_framing_rejects_wrong_platform_header() {
        assert!(decode_native_packet(&[0, 0, 0, 0, 0x45]).is_err());
    }
}
