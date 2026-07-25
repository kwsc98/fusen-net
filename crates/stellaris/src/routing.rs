// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Session-aware, bounded IPv4 overlay routing.

use std::{
    collections::HashMap,
    fmt,
    net::Ipv4Addr,
    str::FromStr,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use bytes::Bytes;
use ipnet::Ipv4Net;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::registry::{validate_node_id, validate_overlay_address};

pub const DEFAULT_ROUTE_QUEUE_CAPACITY: usize = 256;
pub const MIN_IPV4_PACKET_LEN: usize = 20;
pub const MIN_OVERLAY_MTU: usize = 576;
pub const MAX_OVERLAY_MTU: usize = 1100;
pub const MIN_OVERLAY_PREFIX_LEN: u8 = 8;
pub const MAX_OVERLAY_PREFIX_LEN: u8 = 30;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for SessionId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteRegistration {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    pub session_id: SessionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketMetadata {
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub header_len: usize,
    pub total_len: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct PacketValidator {
    overlay: Ipv4Net,
    mtu: usize,
}

impl PacketValidator {
    pub fn new(overlay: Ipv4Net, mtu: usize) -> Result<Self, RouteError> {
        if !(MIN_OVERLAY_PREFIX_LEN..=MAX_OVERLAY_PREFIX_LEN).contains(&overlay.prefix_len()) {
            return Err(RouteError::InvalidOverlay(overlay));
        }
        if !(MIN_OVERLAY_MTU..=MAX_OVERLAY_MTU).contains(&mtu) {
            return Err(RouteError::InvalidMtu(mtu));
        }
        Ok(Self { overlay, mtu })
    }

    pub const fn overlay(&self) -> Ipv4Net {
        self.overlay
    }

    pub const fn mtu(&self) -> usize {
        self.mtu
    }

    pub fn validate(
        &self,
        packet: &[u8],
        expected_source: Ipv4Addr,
    ) -> Result<PacketMetadata, RouteError> {
        let metadata = self.parse_ipv4(packet)?;
        if metadata.source != expected_source {
            return Err(RouteError::SourceSpoofed {
                expected: expected_source,
                actual: metadata.source,
            });
        }
        self.validate_destination(metadata.destination)?;
        Ok(metadata)
    }

    pub fn validate_inbound(
        &self,
        packet: &[u8],
        expected_destination: Ipv4Addr,
    ) -> Result<PacketMetadata, RouteError> {
        let metadata = self.parse_ipv4(packet)?;
        if metadata.destination != expected_destination {
            return Err(RouteError::WrongDestination {
                expected: expected_destination,
                actual: metadata.destination,
            });
        }
        if !is_overlay_unicast(self.overlay, metadata.source) {
            return Err(RouteError::SourceNotOverlayUnicast(metadata.source));
        }
        Ok(metadata)
    }

    /// Validates a packet received from an authenticated P2P connection.
    ///
    /// Relay traffic may originate from any usable overlay address, while a
    /// P2P connection is bound to exactly one certificate identity. Checking
    /// both endpoints prevents one authenticated peer from injecting packets
    /// on behalf of another node.
    pub fn validate_peer_inbound(
        &self,
        packet: &[u8],
        expected_source: Ipv4Addr,
        expected_destination: Ipv4Addr,
    ) -> Result<PacketMetadata, RouteError> {
        let metadata = self.validate_inbound(packet, expected_destination)?;
        if metadata.source != expected_source {
            return Err(RouteError::SourceSpoofed {
                expected: expected_source,
                actual: metadata.source,
            });
        }
        Ok(metadata)
    }

    fn parse_ipv4(&self, packet: &[u8]) -> Result<PacketMetadata, RouteError> {
        if packet.len() > self.mtu {
            return Err(RouteError::PacketExceedsMtu {
                actual: packet.len(),
                mtu: self.mtu,
            });
        }
        if packet.len() < MIN_IPV4_PACKET_LEN {
            return Err(RouteError::MalformedIpv4(
                "packet is shorter than the IPv4 header",
            ));
        }
        if packet[0] >> 4 != 4 {
            return Err(RouteError::UnsupportedIpVersion(packet[0] >> 4));
        }

        let header_len = usize::from(packet[0] & 0x0f) * 4;
        if header_len < MIN_IPV4_PACKET_LEN || header_len > packet.len() {
            return Err(RouteError::MalformedIpv4("invalid IPv4 header length"));
        }
        let total_len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
        if total_len < header_len || total_len != packet.len() {
            return Err(RouteError::MalformedIpv4(
                "IPv4 total length must match the complete datagram",
            ));
        }

        let source = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
        let destination = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
        Ok(PacketMetadata {
            source,
            destination,
            header_len,
            total_len,
        })
    }

    fn validate_destination(&self, destination: Ipv4Addr) -> Result<(), RouteError> {
        if destination.is_unspecified()
            || destination.is_multicast()
            || destination.is_broadcast()
            || destination == self.overlay.network()
            || destination == self.overlay.broadcast()
        {
            return Err(RouteError::DestinationNotUnicast(destination));
        }
        if !self.overlay.contains(&destination) {
            return Err(RouteError::DestinationOutsideOverlay(destination));
        }
        Ok(())
    }
}

fn is_overlay_unicast(overlay: Ipv4Net, address: Ipv4Addr) -> bool {
    overlay.contains(&address)
        && address != overlay.network()
        && address != overlay.broadcast()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_broadcast()
}

#[derive(Debug)]
struct RouteEntry {
    node_id: String,
    session_id: SessionId,
    sender: mpsc::Sender<Bytes>,
    active: bool,
}

#[derive(Debug, Default)]
struct RouteState {
    routes: HashMap<Ipv4Addr, RouteEntry>,
    nodes: HashMap<String, Ipv4Addr>,
}

#[derive(Debug)]
pub struct RouteTable {
    overlay: Ipv4Net,
    queue_capacity: usize,
    state: Mutex<RouteState>,
    dropped_queue_full: AtomicU64,
    dropped_transport: AtomicU64,
}

impl RouteTable {
    pub fn new(overlay: Ipv4Net, queue_capacity: usize) -> Result<Self, RouteError> {
        if queue_capacity == 0 {
            return Err(RouteError::InvalidQueueCapacity);
        }
        Ok(Self {
            overlay,
            queue_capacity,
            state: Mutex::new(RouteState::default()),
            dropped_queue_full: AtomicU64::new(0),
            dropped_transport: AtomicU64::new(0),
        })
    }

    pub fn with_default_capacity(overlay: Ipv4Net) -> Self {
        Self {
            overlay,
            queue_capacity: DEFAULT_ROUTE_QUEUE_CAPACITY,
            state: Mutex::new(RouteState::default()),
            dropped_queue_full: AtomicU64::new(0),
            dropped_transport: AtomicU64::new(0),
        }
    }

    pub const fn overlay(&self) -> Ipv4Net {
        self.overlay
    }

    pub fn register(
        &self,
        registration: RouteRegistration,
    ) -> Result<mpsc::Receiver<Bytes>, RouteError> {
        let address = registration.overlay_ip;
        let session_id = registration.session_id;
        let receiver = self.reserve(registration)?;
        if !self.activate_session(address, session_id) {
            self.remove_session(address, session_id);
            return Err(RouteError::StaleSession);
        }
        Ok(receiver)
    }

    /// Atomically installs an active replacement for the same node and
    /// overlay address. Validation completes before the route-table lock is
    /// mutated, so a rejected replacement leaves the existing active route
    /// untouched.
    pub fn replace_active(
        &self,
        registration: RouteRegistration,
    ) -> Result<mpsc::Receiver<Bytes>, RouteError> {
        validate_node_id(&registration.node_id).map_err(|_| RouteError::InvalidNodeId)?;
        validate_overlay_address(self.overlay, registration.overlay_ip)
            .map_err(|_| RouteError::InvalidRouteAddress(registration.overlay_ip))?;

        let (sender, receiver) = mpsc::channel(self.queue_capacity);
        let mut state = self.lock_state();
        if let Some(existing_address) = state.nodes.get(&registration.node_id)
            && *existing_address != registration.overlay_ip
        {
            return Err(RouteError::DuplicateNode(registration.node_id));
        }
        if let Some(existing) = state.routes.get(&registration.overlay_ip)
            && existing.node_id != registration.node_id
        {
            return Err(RouteError::DuplicateAddress(registration.overlay_ip));
        }

        state
            .nodes
            .insert(registration.node_id.clone(), registration.overlay_ip);
        state.routes.insert(
            registration.overlay_ip,
            RouteEntry {
                node_id: registration.node_id,
                session_id: registration.session_id,
                sender,
                active: true,
            },
        );
        Ok(receiver)
    }

    /// Reserves a node ID and address without making the route visible to the
    /// data plane. The relay activates it only after receiving `Ready`.
    pub fn reserve(
        &self,
        registration: RouteRegistration,
    ) -> Result<mpsc::Receiver<Bytes>, RouteError> {
        validate_node_id(&registration.node_id).map_err(|_| RouteError::InvalidNodeId)?;
        validate_overlay_address(self.overlay, registration.overlay_ip)
            .map_err(|_| RouteError::InvalidRouteAddress(registration.overlay_ip))?;

        let mut state = self.lock_state();
        if state.nodes.contains_key(&registration.node_id) {
            return Err(RouteError::DuplicateNode(registration.node_id));
        }
        if state.routes.contains_key(&registration.overlay_ip) {
            return Err(RouteError::DuplicateAddress(registration.overlay_ip));
        }

        let (sender, receiver) = mpsc::channel(self.queue_capacity);
        state
            .nodes
            .insert(registration.node_id.clone(), registration.overlay_ip);
        state.routes.insert(
            registration.overlay_ip,
            RouteEntry {
                node_id: registration.node_id,
                session_id: registration.session_id,
                sender,
                active: false,
            },
        );
        Ok(receiver)
    }

    pub fn activate_session(&self, overlay_ip: Ipv4Addr, session_id: SessionId) -> bool {
        let mut state = self.lock_state();
        let Some(entry) = state.routes.get_mut(&overlay_ip) else {
            return false;
        };
        if entry.session_id != session_id {
            return false;
        }
        entry.active = true;
        true
    }

    /// Removes a route only when it still belongs to the supplied session.
    /// A delayed cleanup from an old connection therefore cannot remove a
    /// newer route that reused the same node ID and address.
    pub fn remove_session(&self, overlay_ip: Ipv4Addr, session_id: SessionId) -> bool {
        let mut state = self.lock_state();
        let Some(entry) = state.routes.get(&overlay_ip) else {
            return false;
        };
        if entry.session_id != session_id {
            return false;
        }
        let node_id = entry.node_id.clone();
        state.routes.remove(&overlay_ip);
        if state.nodes.get(&node_id) == Some(&overlay_ip) {
            state.nodes.remove(&node_id);
        }
        true
    }

    pub fn is_current_session(&self, overlay_ip: Ipv4Addr, session_id: SessionId) -> bool {
        self.lock_state()
            .routes
            .get(&overlay_ip)
            .is_some_and(|entry| entry.session_id == session_id)
    }

    pub fn len(&self) -> usize {
        self.lock_state().routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn dropped_queue_full(&self) -> u64 {
        self.dropped_queue_full.load(Ordering::Relaxed)
    }

    pub fn record_transport_drop(&self) {
        self.record_transport_drops(1);
    }

    pub fn record_transport_drops(&self, count: u64) {
        self.dropped_transport.fetch_add(count, Ordering::Relaxed);
    }

    pub fn dropped_transport(&self) -> u64 {
        self.dropped_transport.load(Ordering::Relaxed)
    }

    pub fn route_from(
        &self,
        source_session: SessionId,
        source_address: Ipv4Addr,
        packet: Bytes,
        validator: &PacketValidator,
    ) -> Result<RouteOutcome, RouteError> {
        let metadata = validator.validate(&packet, source_address)?;
        let destination_sender = {
            let state = self.lock_state();
            let Some(source) = state.routes.get(&source_address) else {
                return Err(RouteError::StaleSession);
            };
            if source.session_id != source_session || !source.active {
                return Err(RouteError::StaleSession);
            }
            state
                .routes
                .get(&metadata.destination)
                .filter(|entry| entry.active)
                .map(|entry| entry.sender.clone())
                .ok_or(RouteError::DestinationOffline(metadata.destination))?
        };

        match destination_sender.try_send(packet) {
            Ok(()) => Ok(RouteOutcome::Forwarded),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_queue_full.fetch_add(1, Ordering::Relaxed);
                Ok(RouteOutcome::DroppedQueueFull)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(RouteError::DestinationOffline(metadata.destination))
            }
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, RouteState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteOutcome {
    Forwarded,
    DroppedQueueFull,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum RouteError {
    #[error(
        "overlay {0} must have a prefix length between /{MIN_OVERLAY_PREFIX_LEN} and /{MAX_OVERLAY_PREFIX_LEN}"
    )]
    InvalidOverlay(Ipv4Net),
    #[error(
        "MTU {0} is outside the supported IPv4 overlay range {MIN_OVERLAY_MTU}..={MAX_OVERLAY_MTU}"
    )]
    InvalidMtu(usize),
    #[error("route queue capacity must be greater than zero")]
    InvalidQueueCapacity,
    #[error("invalid node ID")]
    InvalidNodeId,
    #[error("{0} is not a usable route address in the overlay")]
    InvalidRouteAddress(Ipv4Addr),
    #[error("node {0} already has an active session")]
    DuplicateNode(String),
    #[error("overlay address {0} already has an active session")]
    DuplicateAddress(Ipv4Addr),
    #[error("malformed IPv4 packet: {0}")]
    MalformedIpv4(&'static str),
    #[error("IP version {0} is not supported")]
    UnsupportedIpVersion(u8),
    #[error("packet length {actual} exceeds MTU {mtu}")]
    PacketExceedsMtu { actual: usize, mtu: usize },
    #[error("packet source {actual} does not match assigned address {expected}")]
    SourceSpoofed {
        expected: Ipv4Addr,
        actual: Ipv4Addr,
    },
    #[error("packet source {0} is not an overlay unicast address")]
    SourceNotOverlayUnicast(Ipv4Addr),
    #[error("packet destination {actual} does not match local address {expected}")]
    WrongDestination {
        expected: Ipv4Addr,
        actual: Ipv4Addr,
    },
    #[error("destination {0} is not an overlay unicast address")]
    DestinationNotUnicast(Ipv4Addr),
    #[error("destination {0} is outside the overlay")]
    DestinationOutsideOverlay(Ipv4Addr),
    #[error("destination {0} is offline")]
    DestinationOffline(Ipv4Addr),
    #[error("source session is no longer active")]
    StaleSession,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(value: &str) -> Ipv4Addr {
        value.parse().expect("IPv4 address")
    }

    fn packet(source: Ipv4Addr, destination: Ipv4Addr, payload_len: usize) -> Bytes {
        let total_len = MIN_IPV4_PACKET_LEN + payload_len;
        let mut packet = vec![0_u8; total_len];
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        packet[8] = 64;
        packet[9] = 17;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        Bytes::from(packet)
    }

    fn registration(node: &str, address: &str, session_id: SessionId) -> RouteRegistration {
        RouteRegistration {
            node_id: node.to_owned(),
            overlay_ip: ip(address),
            session_id,
        }
    }

    #[tokio::test]
    async fn routes_one_complete_ipv4_packet() {
        let overlay = "10.42.0.0/24".parse().expect("overlay");
        let table = RouteTable::new(overlay, 1).expect("table");
        let source_session = SessionId::new();
        let destination_session = SessionId::new();
        let _source = table
            .register(registration("edge-a", "10.42.0.2", source_session))
            .expect("source");
        let mut destination = table
            .register(registration("edge-b", "10.42.0.3", destination_session))
            .expect("destination");
        let validator = PacketValidator::new(overlay, 1100).expect("validator");
        let expected = packet(ip("10.42.0.2"), ip("10.42.0.3"), 8);

        assert_eq!(
            table
                .route_from(
                    source_session,
                    ip("10.42.0.2"),
                    expected.clone(),
                    &validator
                )
                .expect("route"),
            RouteOutcome::Forwarded
        );
        assert_eq!(destination.recv().await, Some(expected));
    }

    #[test]
    fn full_destination_queue_drops_new_packet_and_counts_it() {
        let overlay = "10.42.0.0/24".parse().expect("overlay");
        let table = RouteTable::new(overlay, 1).expect("table");
        let source_session = SessionId::new();
        let _source = table
            .register(registration("edge-a", "10.42.0.2", source_session))
            .expect("source");
        let _destination = table
            .register(registration("edge-b", "10.42.0.3", SessionId::new()))
            .expect("destination");
        let validator = PacketValidator::new(overlay, 1100).expect("validator");
        let packet = packet(ip("10.42.0.2"), ip("10.42.0.3"), 0);

        assert_eq!(
            table
                .route_from(source_session, ip("10.42.0.2"), packet.clone(), &validator)
                .expect("first"),
            RouteOutcome::Forwarded
        );
        assert_eq!(
            table
                .route_from(source_session, ip("10.42.0.2"), packet, &validator)
                .expect("second"),
            RouteOutcome::DroppedQueueFull
        );
        assert_eq!(table.dropped_queue_full(), 1);
    }

    #[test]
    fn delayed_cleanup_cannot_remove_new_session() {
        let table = RouteTable::new("10.42.0.0/24".parse().expect("overlay"), 1).expect("table");
        let old = SessionId::new();
        let new = SessionId::new();
        let address = ip("10.42.0.2");
        let _old_rx = table
            .register(registration("edge-a", "10.42.0.2", old))
            .expect("old");
        assert!(table.remove_session(address, old));
        let _new_rx = table
            .register(registration("edge-a", "10.42.0.2", new))
            .expect("new");

        assert!(!table.remove_session(address, old));
        assert!(table.is_current_session(address, new));
    }

    #[test]
    fn active_replacement_is_atomic_and_rejects_a_different_node() {
        let table = RouteTable::new("10.42.0.0/24".parse().expect("overlay"), 1).expect("table");
        let old = SessionId::new();
        let replacement = SessionId::new();
        let address = ip("10.42.0.2");
        let _old_rx = table
            .register(registration("edge-a", "10.42.0.2", old))
            .expect("old route");

        assert!(matches!(
            table.replace_active(registration("edge-b", "10.42.0.2", SessionId::new())),
            Err(RouteError::DuplicateAddress(candidate)) if candidate == address
        ));
        assert!(table.is_current_session(address, old));

        let _replacement_rx = table
            .replace_active(registration("edge-a", "10.42.0.2", replacement))
            .expect("atomic replacement");
        assert!(!table.is_current_session(address, old));
        assert!(table.is_current_session(address, replacement));
        assert!(!table.remove_session(address, old));
    }

    #[test]
    fn duplicate_node_and_address_use_reject_new_policy() {
        let table = RouteTable::new("10.42.0.0/24".parse().expect("overlay"), 1).expect("table");
        let _receiver = table
            .register(registration("edge-a", "10.42.0.2", SessionId::new()))
            .expect("first");
        assert!(matches!(
            table.register(registration("edge-a", "10.42.0.3", SessionId::new())),
            Err(RouteError::DuplicateNode(_))
        ));
        assert!(matches!(
            table.register(registration("edge-b", "10.42.0.2", SessionId::new())),
            Err(RouteError::DuplicateAddress(_))
        ));
    }

    #[test]
    fn reserved_route_is_inactive_until_ready_transition() {
        let overlay = "10.42.0.0/24".parse().expect("overlay");
        let table = RouteTable::new(overlay, 1).expect("table");
        let source_session = SessionId::new();
        let destination_session = SessionId::new();
        let _source = table
            .reserve(registration("edge-a", "10.42.0.2", source_session))
            .expect("source reservation");
        let _destination = table
            .register(registration("edge-b", "10.42.0.3", destination_session))
            .expect("destination");
        let validator = PacketValidator::new(overlay, 1100).expect("validator");
        let datagram = packet(ip("10.42.0.2"), ip("10.42.0.3"), 0);

        assert_eq!(
            table.route_from(
                source_session,
                ip("10.42.0.2"),
                datagram.clone(),
                &validator,
            ),
            Err(RouteError::StaleSession)
        );
        assert!(table.activate_session(ip("10.42.0.2"), source_session));
        assert_eq!(
            table
                .route_from(source_session, ip("10.42.0.2"), datagram, &validator,)
                .expect("active route"),
            RouteOutcome::Forwarded
        );
    }

    #[test]
    fn validator_rejects_spoofing_non_unicast_outside_overlay_and_ipv6() {
        let validator =
            PacketValidator::new("10.0.0.0/8".parse().expect("overlay"), 1100).expect("validator");
        let assigned = ip("10.42.0.2");
        assert!(matches!(
            validator.validate(&packet(ip("10.42.0.9"), ip("10.42.0.3"), 0), assigned),
            Err(RouteError::SourceSpoofed { .. })
        ));
        assert!(matches!(
            validator.validate(&packet(assigned, ip("224.0.0.1"), 0), assigned),
            Err(RouteError::DestinationNotUnicast(_))
        ));

        let narrow = PacketValidator::new("10.42.0.0/24".parse().expect("overlay"), 1100)
            .expect("validator");
        assert!(matches!(
            narrow.validate(&packet(assigned, ip("10.43.0.3"), 0), assigned),
            Err(RouteError::DestinationOutsideOverlay(_))
        ));

        let mut ipv6 = packet(assigned, ip("10.42.0.3"), 0).to_vec();
        ipv6[0] = 0x65;
        assert_eq!(
            narrow.validate(&ipv6, assigned),
            Err(RouteError::UnsupportedIpVersion(6))
        );
    }

    #[test]
    fn validator_rejects_default_route_overlay() {
        assert!(matches!(
            PacketValidator::new("0.0.0.0/0".parse().expect("overlay"), 1100),
            Err(RouteError::InvalidOverlay(_))
        ));
    }

    #[test]
    fn validator_rejects_malformed_and_oversized_datagrams() {
        let validator =
            PacketValidator::new("10.42.0.0/24".parse().expect("overlay"), 576).expect("validator");
        let source = ip("10.42.0.2");
        assert!(matches!(
            validator.validate(&[0_u8; 10], source),
            Err(RouteError::MalformedIpv4(_))
        ));

        let oversized = packet(source, ip("10.42.0.3"), 557);
        assert!(matches!(
            validator.validate(&oversized, source),
            Err(RouteError::PacketExceedsMtu { .. })
        ));

        let mut trailing = packet(source, ip("10.42.0.3"), 0).to_vec();
        trailing.push(0);
        assert!(matches!(
            validator.validate(&trailing, source),
            Err(RouteError::MalformedIpv4(_))
        ));
    }

    #[test]
    fn validator_rejects_mtu_above_overlay_transport_limit() {
        assert!(matches!(
            PacketValidator::new(
                "10.42.0.0/24".parse().expect("overlay"),
                MAX_OVERLAY_MTU + 1,
            ),
            Err(RouteError::InvalidMtu(value)) if value == MAX_OVERLAY_MTU + 1
        ));
    }

    #[test]
    fn stale_source_session_cannot_route() {
        let overlay = "10.42.0.0/24".parse().expect("overlay");
        let table = RouteTable::new(overlay, 1).expect("table");
        let active = SessionId::new();
        let _source = table
            .register(registration("edge-a", "10.42.0.2", active))
            .expect("source");
        let _destination = table
            .register(registration("edge-b", "10.42.0.3", SessionId::new()))
            .expect("destination");
        let validator = PacketValidator::new(overlay, 1100).expect("validator");
        assert_eq!(
            table.route_from(
                SessionId::new(),
                ip("10.42.0.2"),
                packet(ip("10.42.0.2"), ip("10.42.0.3"), 0),
                &validator,
            ),
            Err(RouteError::StaleSession)
        );
    }
}
