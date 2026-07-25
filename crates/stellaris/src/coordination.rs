// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounded, session-owned coordinator state for protocol v2.
//!
//! TLS verification remains the responsibility of the transport and control
//! session. This module starts at the post-authentication boundary: callers
//! register the node ID, overlay address, and certificate fingerprint already
//! extracted from one verified peer certificate.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
    sync::Mutex,
};

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};

use crate::{
    protocol::{Candidate, CandidateKind, PeerRecord, PeerRevoked},
    registry::{validate_node_id, validate_overlay_address},
    routing::SessionId,
};

pub const DEFAULT_MAX_DIRECTORY_PEERS: usize = 256;
pub const MAX_DIRECTORY_PEERS: usize = 256;
pub const MAX_CANDIDATES_PER_PEER: usize = 16;
pub const DIRECTORY_SNAPSHOT_VERSION: u16 = 2;
pub const MAX_DIRECTORY_SNAPSHOT_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectorySnapshot {
    pub version: u16,
    pub overlay: Ipv4Net,
    pub nodes: Vec<DirectoryNodeSnapshot>,
    pub revocations: Vec<DirectoryRevocationSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryNodeSnapshot {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    pub incarnation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRevocationSnapshot {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    pub epoch: u64,
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct CertificateFingerprint(String);

impl CertificateFingerprint {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CertificateFingerprint {
    type Err = DirectoryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(DirectoryError::InvalidCertificateFingerprint);
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DirectoryError::InvalidCertificateFingerprint);
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Debug for CertificateFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CertificateFingerprint")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for CertificateFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerRegistration {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    pub certificate_fingerprint: CertificateFingerprint,
    pub certificate_not_after_unix_seconds: u64,
    pub session_id: SessionId,
}

#[derive(Clone, Debug)]
struct PeerEntry {
    node_id: String,
    overlay_ip: Ipv4Addr,
    incarnation: u64,
    certificate_fingerprint: CertificateFingerprint,
    certificate_not_after_unix_seconds: u64,
    session_id: SessionId,
    candidate_epoch: u64,
    candidates: Vec<Candidate>,
}

impl PeerEntry {
    fn record(&self, request_id: u64) -> PeerRecord {
        PeerRecord {
            request_id,
            node_id: self.node_id.clone(),
            overlay_ip: self.overlay_ip,
            incarnation: self.incarnation,
            session_id: self.session_id.to_string(),
            certificate_fingerprint: self.certificate_fingerprint.to_string(),
            certificate_not_after_unix_seconds: self.certificate_not_after_unix_seconds,
            epoch: self.candidate_epoch,
            candidates: self.candidates.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Revocation {
    overlay_ip: Ipv4Addr,
    epoch: u64,
}

#[derive(Clone, Copy, Debug)]
struct IncarnationState {
    overlay_ip: Ipv4Addr,
    current: u64,
}

#[derive(Debug, Default)]
struct DirectoryState {
    peers: HashMap<Ipv4Addr, PeerEntry>,
    by_node: HashMap<String, Ipv4Addr>,
    by_fingerprint: HashMap<CertificateFingerprint, Ipv4Addr>,
    incarnations: HashMap<String, IncarnationState>,
    incarnation_nodes_by_overlay: HashMap<Ipv4Addr, String>,
    revocations: HashMap<String, Revocation>,
    revoked_overlays: HashMap<Ipv4Addr, String>,
}

#[derive(Debug)]
pub struct PeerDirectory {
    overlay: Ipv4Net,
    max_peers: usize,
    state: Mutex<DirectoryState>,
}

impl PeerDirectory {
    pub fn new(overlay: Ipv4Net, max_peers: usize) -> Result<Self, DirectoryError> {
        if max_peers == 0 || max_peers > MAX_DIRECTORY_PEERS {
            return Err(DirectoryError::InvalidCapacity(max_peers));
        }
        Ok(Self {
            overlay,
            max_peers,
            state: Mutex::new(DirectoryState::default()),
        })
    }

    pub fn with_default_capacity(overlay: Ipv4Net) -> Self {
        Self {
            overlay,
            max_peers: DEFAULT_MAX_DIRECTORY_PEERS,
            state: Mutex::new(DirectoryState::default()),
        }
    }

    pub const fn overlay(&self) -> Ipv4Net {
        self.overlay
    }

    pub const fn max_peers(&self) -> usize {
        self.max_peers
    }

    /// Captures only durable coordinator state, not live control sessions.
    ///
    /// The caller is responsible for atomically persisting the returned value
    /// before acknowledging mutations. This module intentionally performs no
    /// file I/O while holding the directory lock.
    pub fn export_snapshot(&self) -> DirectorySnapshot {
        let state = self.lock_state();
        let mut nodes: Vec<_> = state
            .incarnations
            .iter()
            .map(|(node_id, history)| DirectoryNodeSnapshot {
                node_id: node_id.clone(),
                overlay_ip: history.overlay_ip,
                incarnation: history.current,
            })
            .collect();
        let mut revocations: Vec<_> = state
            .revocations
            .iter()
            .map(|(node_id, revocation)| DirectoryRevocationSnapshot {
                node_id: node_id.clone(),
                overlay_ip: revocation.overlay_ip,
                epoch: revocation.epoch,
            })
            .collect();
        nodes.sort_unstable_by(|left, right| left.node_id.cmp(&right.node_id));
        revocations.sort_unstable_by(|left, right| left.node_id.cmp(&right.node_id));
        DirectorySnapshot {
            version: DIRECTORY_SNAPSHOT_VERSION,
            overlay: self.overlay,
            nodes,
            revocations,
        }
    }

    /// Serializes a bounded snapshot after releasing the directory lock.
    pub fn export_snapshot_json(&self) -> Result<Vec<u8>, DirectoryError> {
        let snapshot = self.export_snapshot();
        let encoded = serde_json::to_vec(&snapshot).map_err(|_| DirectoryError::InvalidSnapshot)?;
        validate_snapshot_size(encoded.len())?;
        Ok(encoded)
    }

    /// Builds a restarted directory from validated durable state.
    ///
    /// Active peers are deliberately not restored. They must authenticate and
    /// register again, at which point their incarnation advances.
    pub fn from_snapshot(
        overlay: Ipv4Net,
        max_peers: usize,
        snapshot: DirectorySnapshot,
    ) -> Result<Self, DirectoryError> {
        if max_peers == 0 || max_peers > MAX_DIRECTORY_PEERS {
            return Err(DirectoryError::InvalidCapacity(max_peers));
        }
        let state = validate_snapshot(overlay, snapshot)?;
        Ok(Self {
            overlay,
            max_peers,
            state: Mutex::new(state),
        })
    }

    /// Parses a bounded JSON snapshot without performing file I/O.
    pub fn from_snapshot_json(
        overlay: Ipv4Net,
        max_peers: usize,
        encoded: &[u8],
    ) -> Result<Self, DirectoryError> {
        validate_snapshot_size(encoded.len())?;
        let snapshot =
            serde_json::from_slice(encoded).map_err(|_| DirectoryError::InvalidSnapshot)?;
        Self::from_snapshot(overlay, max_peers, snapshot)
    }

    pub fn register(&self, registration: PeerRegistration) -> Result<(), DirectoryError> {
        self.register_inner(registration, false).map(|_| ())
    }

    /// Registers a session and atomically replaces the currently active session
    /// for the same static node/address binding, if one exists.
    pub(crate) fn register_replacing(
        &self,
        registration: PeerRegistration,
    ) -> Result<Option<SessionId>, DirectoryError> {
        self.register_inner(registration, true)
    }

    fn register_inner(
        &self,
        registration: PeerRegistration,
        replace_active: bool,
    ) -> Result<Option<SessionId>, DirectoryError> {
        validate_node_id(&registration.node_id).map_err(|_| DirectoryError::InvalidNodeId)?;
        validate_overlay_address(self.overlay, registration.overlay_ip)
            .map_err(|_| DirectoryError::InvalidOverlayAddress(registration.overlay_ip))?;
        validate_session_id(registration.session_id)?;
        if registration.certificate_not_after_unix_seconds == 0 {
            return Err(DirectoryError::InvalidCertificateExpiry);
        }

        let mut state = self.lock_state();
        if state.revocations.contains_key(&registration.node_id) {
            return Err(DirectoryError::NodeRevoked(registration.node_id));
        }
        if state
            .revoked_overlays
            .contains_key(&registration.overlay_ip)
        {
            return Err(DirectoryError::OverlayAddressRevoked(
                registration.overlay_ip,
            ));
        }
        let replaced_entry = match state.by_node.get(&registration.node_id).copied() {
            Some(active_ip) => {
                if !replace_active {
                    return Err(DirectoryError::DuplicateNode(registration.node_id));
                }
                if active_ip != registration.overlay_ip {
                    return Err(DirectoryError::DuplicateNode(registration.node_id));
                }
                let entry = state
                    .peers
                    .get(&active_ip)
                    .expect("node index and peer entry are updated atomically");
                if entry.session_id == registration.session_id {
                    return Err(DirectoryError::DuplicateSessionId);
                }
                Some(entry.clone())
            }
            None => {
                if state.peers.contains_key(&registration.overlay_ip) {
                    return Err(DirectoryError::DuplicateOverlayAddress(
                        registration.overlay_ip,
                    ));
                }
                None
            }
        };
        if replaced_entry.is_none() && state.peers.len() >= self.max_peers {
            return Err(DirectoryError::CapacityReached(self.max_peers));
        }
        if let Some(bound_ip) = state
            .by_fingerprint
            .get(&registration.certificate_fingerprint)
        {
            let belongs_to_replaced_entry = replaced_entry.as_ref().is_some_and(|entry| {
                *bound_ip == registration.overlay_ip
                    && entry.certificate_fingerprint == registration.certificate_fingerprint
            });
            if !belongs_to_replaced_entry {
                return Err(DirectoryError::DuplicateCertificateFingerprint);
            }
        }
        if state
            .incarnations
            .get(&registration.node_id)
            .is_some_and(|history| history.overlay_ip != registration.overlay_ip)
            || state
                .incarnation_nodes_by_overlay
                .get(&registration.overlay_ip)
                .is_some_and(|bound_node| bound_node != &registration.node_id)
        {
            return Err(DirectoryError::HistoricalBindingMismatch);
        }

        let incarnation = match state.incarnations.get(&registration.node_id).copied() {
            Some(history) => history
                .current
                .checked_add(1)
                .ok_or(DirectoryError::IncarnationExhausted)?,
            None => {
                if state.incarnations.len() >= MAX_DIRECTORY_PEERS {
                    return Err(DirectoryError::IncarnationCapacityReached(
                        MAX_DIRECTORY_PEERS,
                    ));
                }
                1
            }
        };

        let entry = PeerEntry {
            node_id: registration.node_id.clone(),
            overlay_ip: registration.overlay_ip,
            incarnation,
            certificate_fingerprint: registration.certificate_fingerprint.clone(),
            certificate_not_after_unix_seconds: registration.certificate_not_after_unix_seconds,
            session_id: registration.session_id,
            candidate_epoch: 0,
            candidates: Vec::new(),
        };
        if let Some(previous) = &replaced_entry
            && previous.certificate_fingerprint != registration.certificate_fingerprint
            && state.by_fingerprint.get(&previous.certificate_fingerprint)
                == Some(&registration.overlay_ip)
        {
            state
                .by_fingerprint
                .remove(&previous.certificate_fingerprint);
        }
        state
            .by_node
            .insert(registration.node_id.clone(), registration.overlay_ip);
        state.incarnations.insert(
            registration.node_id.clone(),
            IncarnationState {
                overlay_ip: registration.overlay_ip,
                current: incarnation,
            },
        );
        state
            .incarnation_nodes_by_overlay
            .insert(registration.overlay_ip, registration.node_id);
        state.by_fingerprint.insert(
            registration.certificate_fingerprint,
            registration.overlay_ip,
        );
        state.peers.insert(registration.overlay_ip, entry);
        Ok(replaced_entry.map(|entry| entry.session_id))
    }

    /// Applies one authenticated Agent announcement.
    ///
    /// Agent announcements may contain host candidates only. A
    /// server-reflexive address must be produced by an observation made from
    /// the P2P Hybrid socket, not copied from the control connection.
    pub fn announce_candidates(
        &self,
        overlay_ip: Ipv4Addr,
        session_id: SessionId,
        epoch: u64,
        candidates: Vec<Candidate>,
    ) -> Result<(), DirectoryError> {
        if epoch == 0 {
            return Err(DirectoryError::InvalidEpoch);
        }
        validate_announced_candidates(&candidates)?;

        let mut state = self.lock_state();
        let entry = state
            .peers
            .get_mut(&overlay_ip)
            .ok_or(DirectoryError::PeerUnavailable(overlay_ip))?;
        if entry.session_id != session_id {
            return Err(DirectoryError::StaleSession);
        }
        if epoch <= entry.candidate_epoch {
            return Err(DirectoryError::StaleEpoch {
                current: entry.candidate_epoch,
                received: epoch,
            });
        }
        entry.candidate_epoch = epoch;
        entry.candidates = candidates;
        Ok(())
    }

    pub fn lookup(
        &self,
        overlay_ip: Ipv4Addr,
        request_id: u64,
    ) -> Result<PeerRecord, DirectoryError> {
        if request_id == 0 {
            return Err(DirectoryError::InvalidRequestId);
        }
        let state = self.lock_state();
        let entry = state
            .peers
            .get(&overlay_ip)
            .ok_or(DirectoryError::PeerUnavailable(overlay_ip))?;
        if state.revocations.contains_key(&entry.node_id) {
            return Err(DirectoryError::NodeRevoked(entry.node_id.clone()));
        }
        Ok(entry.record(request_id))
    }

    /// Removes only the entry owned by `session_id`.
    pub fn remove_session(&self, overlay_ip: Ipv4Addr, session_id: SessionId) -> bool {
        let mut state = self.lock_state();
        let Some(entry) = state.peers.get(&overlay_ip) else {
            return false;
        };
        if entry.session_id != session_id {
            return false;
        }
        let entry = state
            .peers
            .remove(&overlay_ip)
            .expect("entry was checked while holding directory lock");
        if state.by_node.get(&entry.node_id) == Some(&overlay_ip) {
            state.by_node.remove(&entry.node_id);
        }
        if state.by_fingerprint.get(&entry.certificate_fingerprint) == Some(&overlay_ip) {
            state.by_fingerprint.remove(&entry.certificate_fingerprint);
        }
        true
    }

    /// Records a monotonic revocation and removes an active directory session.
    ///
    /// The returned session ID must be used by the runtime to cancel that
    /// session's control and data tasks. This module has no file store; callers
    /// must export and atomically persist a snapshot before acknowledging the
    /// mutation when crash durability is required.
    pub fn revoke(
        &self,
        node_id: &str,
        overlay_ip: Ipv4Addr,
        epoch: u64,
    ) -> Result<RevocationOutcome, DirectoryError> {
        validate_node_id(node_id).map_err(|_| DirectoryError::InvalidNodeId)?;
        validate_overlay_address(self.overlay, overlay_ip)
            .map_err(|_| DirectoryError::InvalidOverlayAddress(overlay_ip))?;
        if epoch == 0 {
            return Err(DirectoryError::InvalidEpoch);
        }

        let mut state = self.lock_state();
        if state
            .incarnations
            .get(node_id)
            .is_some_and(|history| history.overlay_ip != overlay_ip)
            || state
                .incarnation_nodes_by_overlay
                .get(&overlay_ip)
                .is_some_and(|bound_node| bound_node != node_id)
        {
            return Err(DirectoryError::RevocationBindingMismatch);
        }
        if let Some(current) = state.revocations.get(node_id) {
            if current.overlay_ip != overlay_ip {
                return Err(DirectoryError::RevocationBindingMismatch);
            }
            if epoch <= current.epoch {
                return Err(DirectoryError::StaleEpoch {
                    current: current.epoch,
                    received: epoch,
                });
            }
        }
        if state
            .revoked_overlays
            .get(&overlay_ip)
            .is_some_and(|revoked_node| revoked_node != node_id)
            || state
                .peers
                .get(&overlay_ip)
                .is_some_and(|entry| entry.node_id != node_id)
        {
            return Err(DirectoryError::RevocationBindingMismatch);
        }
        if !state.revocations.contains_key(node_id)
            && state.revocations.len() >= MAX_DIRECTORY_PEERS
        {
            return Err(DirectoryError::RevocationCapacityReached(
                MAX_DIRECTORY_PEERS,
            ));
        }
        let mut revoked_session = None;
        if let Some(active_ip) = state.by_node.get(node_id).copied() {
            if active_ip != overlay_ip {
                return Err(DirectoryError::RevocationBindingMismatch);
            }
            let entry = state
                .peers
                .remove(&active_ip)
                .expect("node index and peer entry are updated atomically");
            state.by_node.remove(node_id);
            state.by_fingerprint.remove(&entry.certificate_fingerprint);
            revoked_session = Some(entry.session_id);
        }
        state
            .revocations
            .insert(node_id.to_owned(), Revocation { overlay_ip, epoch });
        state
            .revoked_overlays
            .insert(overlay_ip, node_id.to_owned());
        Ok(RevocationOutcome {
            notification: PeerRevoked {
                node_id: node_id.to_owned(),
                overlay_ip,
                epoch,
            },
            revoked_session,
        })
    }

    pub fn is_revoked(&self, node_id: &str) -> bool {
        self.lock_state().revocations.contains_key(node_id)
    }

    /// Returns the durable revocation set as a bounded, deterministic wire
    /// notification list for control-session catch-up.
    pub fn revocation_notifications(&self) -> Vec<PeerRevoked> {
        let state = self.lock_state();
        let mut notifications: Vec<_> = state
            .revocations
            .iter()
            .map(|(node_id, revocation)| PeerRevoked {
                node_id: node_id.clone(),
                overlay_ip: revocation.overlay_ip,
                epoch: revocation.epoch,
            })
            .collect();
        notifications.sort_unstable_by(|left, right| left.node_id.cmp(&right.node_id));
        notifications
    }

    pub fn len(&self) -> usize {
        self.lock_state().peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, DirectoryState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevocationOutcome {
    pub notification: PeerRevoked,
    pub revoked_session: Option<SessionId>,
}

fn validate_snapshot_size(actual: usize) -> Result<(), DirectoryError> {
    if actual > MAX_DIRECTORY_SNAPSHOT_BYTES {
        return Err(DirectoryError::SnapshotTooLarge {
            actual,
            max: MAX_DIRECTORY_SNAPSHOT_BYTES,
        });
    }
    Ok(())
}

fn validate_snapshot(
    overlay: Ipv4Net,
    snapshot: DirectorySnapshot,
) -> Result<DirectoryState, DirectoryError> {
    if snapshot.version != DIRECTORY_SNAPSHOT_VERSION {
        return Err(DirectoryError::UnsupportedSnapshotVersion(snapshot.version));
    }
    if snapshot.overlay != overlay {
        return Err(DirectoryError::SnapshotOverlayMismatch {
            expected: overlay,
            received: snapshot.overlay,
        });
    }
    if snapshot.nodes.len() > MAX_DIRECTORY_PEERS
        || snapshot.revocations.len() > MAX_DIRECTORY_PEERS
    {
        return Err(DirectoryError::InvalidSnapshot);
    }

    let mut state = DirectoryState::default();
    for node in snapshot.nodes {
        validate_node_id(&node.node_id).map_err(|_| DirectoryError::InvalidSnapshot)?;
        validate_overlay_address(overlay, node.overlay_ip)
            .map_err(|_| DirectoryError::InvalidSnapshot)?;
        if node.incarnation == 0
            || state.incarnations.contains_key(&node.node_id)
            || state
                .incarnation_nodes_by_overlay
                .contains_key(&node.overlay_ip)
        {
            return Err(DirectoryError::InvalidSnapshot);
        }
        state.incarnations.insert(
            node.node_id.clone(),
            IncarnationState {
                overlay_ip: node.overlay_ip,
                current: node.incarnation,
            },
        );
        state
            .incarnation_nodes_by_overlay
            .insert(node.overlay_ip, node.node_id);
    }

    for revocation in snapshot.revocations {
        validate_node_id(&revocation.node_id).map_err(|_| DirectoryError::InvalidSnapshot)?;
        validate_overlay_address(overlay, revocation.overlay_ip)
            .map_err(|_| DirectoryError::InvalidSnapshot)?;
        if revocation.epoch == 0
            || state.revocations.contains_key(&revocation.node_id)
            || state.revoked_overlays.contains_key(&revocation.overlay_ip)
            || state
                .incarnations
                .get(&revocation.node_id)
                .is_some_and(|history| history.overlay_ip != revocation.overlay_ip)
            || state
                .incarnation_nodes_by_overlay
                .get(&revocation.overlay_ip)
                .is_some_and(|node_id| node_id != &revocation.node_id)
        {
            return Err(DirectoryError::InvalidSnapshot);
        }
        state.revocations.insert(
            revocation.node_id.clone(),
            Revocation {
                overlay_ip: revocation.overlay_ip,
                epoch: revocation.epoch,
            },
        );
        state
            .revoked_overlays
            .insert(revocation.overlay_ip, revocation.node_id);
    }

    Ok(state)
}

fn validate_announced_candidates(candidates: &[Candidate]) -> Result<(), DirectoryError> {
    if candidates.len() > MAX_CANDIDATES_PER_PEER {
        return Err(DirectoryError::TooManyCandidates {
            actual: candidates.len(),
            max: MAX_CANDIDATES_PER_PEER,
        });
    }
    let mut addresses = HashSet::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.kind != CandidateKind::Host {
            return Err(DirectoryError::UntrustedReflexiveCandidate);
        }
        let address = candidate.address;
        if address.port() == 0
            || address.is_ipv6()
            || address.ip().is_unspecified()
            || address.ip().is_multicast()
            || matches!(address.ip(), IpAddr::V4(ip) if ip.is_broadcast())
            || matches!(address.ip(), IpAddr::V4(ip) if ip.octets()[0] == 0 || ip.is_loopback())
        {
            return Err(DirectoryError::InvalidCandidate(address));
        }
        if !addresses.insert(address) {
            return Err(DirectoryError::DuplicateCandidate(address));
        }
    }
    Ok(())
}

fn validate_session_id(session_id: SessionId) -> Result<(), DirectoryError> {
    let session_id = uuid::Uuid::parse_str(&session_id.to_string())
        .map_err(|_| DirectoryError::InvalidSessionId)?;
    if session_id.get_version() != Some(uuid::Version::Random) {
        return Err(DirectoryError::InvalidSessionId);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum DirectoryError {
    #[error("directory capacity must be between 1 and {MAX_DIRECTORY_PEERS}, got {0}")]
    InvalidCapacity(usize),
    #[error("directory reached its configured capacity of {0} peers")]
    CapacityReached(usize),
    #[error("directory snapshot is {actual} bytes; maximum is {max}")]
    SnapshotTooLarge { actual: usize, max: usize },
    #[error("unsupported directory snapshot version {0}")]
    UnsupportedSnapshotVersion(u16),
    #[error("directory snapshot overlay {received} does not match configured overlay {expected}")]
    SnapshotOverlayMismatch {
        expected: Ipv4Net,
        received: Ipv4Net,
    },
    #[error("invalid directory snapshot")]
    InvalidSnapshot,
    #[error("invalid node ID")]
    InvalidNodeId,
    #[error("invalid certificate SHA-256 fingerprint")]
    InvalidCertificateFingerprint,
    #[error("certificate expiry timestamp must be non-zero")]
    InvalidCertificateExpiry,
    #[error("session ID must be a UUID v4")]
    InvalidSessionId,
    #[error("session ID already owns the active node binding")]
    DuplicateSessionId,
    #[error("{0} is not a usable address in the configured overlay")]
    InvalidOverlayAddress(Ipv4Addr),
    #[error("node {0} already has an active coordinator session")]
    DuplicateNode(String),
    #[error("overlay address {0} already has an active coordinator session")]
    DuplicateOverlayAddress(Ipv4Addr),
    #[error("certificate fingerprint already belongs to an active node")]
    DuplicateCertificateFingerprint,
    #[error("peer incarnation counter is exhausted")]
    IncarnationExhausted,
    #[error("node and overlay address do not match their historical static binding")]
    HistoricalBindingMismatch,
    #[error("incarnation directory reached its capacity of {0} node bindings")]
    IncarnationCapacityReached(usize),
    #[error("node {0} is revoked")]
    NodeRevoked(String),
    #[error("overlay address {0} belongs to a revoked node binding")]
    OverlayAddressRevoked(Ipv4Addr),
    #[error("revocation directory reached its capacity of {0} bindings")]
    RevocationCapacityReached(usize),
    #[error("peer {0} is not available")]
    PeerUnavailable(Ipv4Addr),
    #[error("candidate epoch must be non-zero")]
    InvalidEpoch,
    #[error("candidate epoch {received} is not newer than current epoch {current}")]
    StaleEpoch { current: u64, received: u64 },
    #[error("request ID must be non-zero")]
    InvalidRequestId,
    #[error("coordinator session no longer owns this peer entry")]
    StaleSession,
    #[error("candidate list contains {actual} entries; maximum is {max}")]
    TooManyCandidates { actual: usize, max: usize },
    #[error("invalid P2P candidate {0}")]
    InvalidCandidate(std::net::SocketAddr),
    #[error("duplicate P2P candidate {0}")]
    DuplicateCandidate(std::net::SocketAddr),
    #[error("Agent announcements cannot assert server-reflexive candidates")]
    UntrustedReflexiveCandidate,
    #[error("revocation does not match the registered node/address binding")]
    RevocationBindingMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn ip(value: &str) -> Ipv4Addr {
        value.parse().expect("IPv4 address")
    }

    fn fingerprint(fill: char) -> CertificateFingerprint {
        format!("sha256:{}", fill.to_string().repeat(64))
            .parse()
            .expect("fingerprint")
    }

    fn registration(
        node_id: &str,
        overlay_ip: &str,
        fill: char,
        session_id: SessionId,
    ) -> PeerRegistration {
        PeerRegistration {
            node_id: node_id.to_owned(),
            overlay_ip: ip(overlay_ip),
            certificate_fingerprint: fingerprint(fill),
            certificate_not_after_unix_seconds: 2_000_000_000,
            session_id,
        }
    }

    fn candidate(address: &str) -> Candidate {
        Candidate {
            address: address.parse::<SocketAddr>().expect("socket address"),
            kind: CandidateKind::Host,
            priority: 100,
        }
    }

    fn directory(max_peers: usize) -> PeerDirectory {
        PeerDirectory::new("10.42.0.0/24".parse().expect("overlay"), max_peers).expect("directory")
    }

    #[test]
    fn fingerprint_format_is_strict() {
        assert!(
            format!("sha256:{}", "a".repeat(64))
                .parse::<CertificateFingerprint>()
                .is_ok()
        );
        for value in [
            "a",
            "sha256:abc",
            &format!("sha256:{}", "A".repeat(64)),
            &format!("sha512:{}", "a".repeat(64)),
        ] {
            assert!(value.parse::<CertificateFingerprint>().is_err(), "{value}");
        }
    }

    #[test]
    fn capacity_is_bounded_to_product_limit() {
        assert!(matches!(
            PeerDirectory::new("10.42.0.0/24".parse().expect("overlay"), 0),
            Err(DirectoryError::InvalidCapacity(0))
        ));
        assert!(matches!(
            PeerDirectory::new(
                "10.42.0.0/24".parse().expect("overlay"),
                MAX_DIRECTORY_PEERS + 1
            ),
            Err(DirectoryError::InvalidCapacity(_))
        ));

        let directory = directory(1);
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', SessionId::new()))
            .expect("first registration");
        assert!(matches!(
            directory.register(registration("edge-b", "10.42.0.3", 'b', SessionId::new())),
            Err(DirectoryError::CapacityReached(1))
        ));
    }

    #[test]
    fn durable_snapshot_restores_incarnations_and_revocations_after_restart() {
        let overlay: Ipv4Net = "10.42.0.0/24".parse().expect("overlay");
        let directory = PeerDirectory::new(overlay, 4).expect("directory");
        let first = SessionId::new();
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', first))
            .expect("first registration");
        assert!(directory.remove_session(ip("10.42.0.2"), first));
        directory
            .register(registration("edge-a", "10.42.0.2", 'b', SessionId::new()))
            .expect("second registration");
        directory
            .revoke("edge-b", ip("10.42.0.3"), 7)
            .expect("durable revocation");

        let encoded = directory.export_snapshot_json().expect("snapshot JSON");
        assert_eq!(
            encoded,
            directory
                .export_snapshot_json()
                .expect("deterministic JSON")
        );
        let restored =
            PeerDirectory::from_snapshot_json(overlay, 4, &encoded).expect("restore snapshot");
        assert!(restored.is_empty());
        assert!(restored.is_revoked("edge-b"));
        assert_eq!(
            restored.revocation_notifications(),
            vec![PeerRevoked {
                node_id: "edge-b".to_owned(),
                overlay_ip: ip("10.42.0.3"),
                epoch: 7,
            }]
        );

        restored
            .register(registration("edge-a", "10.42.0.2", 'c', SessionId::new()))
            .expect("post-restart registration");
        assert_eq!(
            restored
                .lookup(ip("10.42.0.2"), 1)
                .expect("post-restart record")
                .incarnation,
            3
        );
        assert!(matches!(
            restored.register(registration("edge-b", "10.42.0.3", 'd', SessionId::new(),)),
            Err(DirectoryError::NodeRevoked(_))
        ));
    }

    #[test]
    fn snapshot_import_rejects_wrong_version_overlay_and_bindings() {
        let overlay: Ipv4Net = "10.42.0.0/24".parse().expect("overlay");
        let directory = PeerDirectory::new(overlay, 4).expect("directory");
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', SessionId::new()))
            .expect("registration");
        let snapshot = directory.export_snapshot();

        let mut legacy = snapshot.clone();
        legacy.version = 1;
        assert_eq!(
            PeerDirectory::from_snapshot(overlay, 4, legacy).unwrap_err(),
            DirectoryError::UnsupportedSnapshotVersion(1)
        );

        let mut future = snapshot.clone();
        future.version = DIRECTORY_SNAPSHOT_VERSION + 1;
        assert_eq!(
            PeerDirectory::from_snapshot(overlay, 4, future).unwrap_err(),
            DirectoryError::UnsupportedSnapshotVersion(DIRECTORY_SNAPSHOT_VERSION + 1)
        );
        assert!(matches!(
            PeerDirectory::from_snapshot(
                "10.43.0.0/24".parse().expect("other overlay"),
                4,
                snapshot.clone(),
            ),
            Err(DirectoryError::SnapshotOverlayMismatch { .. })
        ));

        let mut invalid = snapshot.clone();
        invalid.nodes.push(DirectoryNodeSnapshot {
            node_id: "edge-b".to_owned(),
            overlay_ip: ip("10.42.0.2"),
            incarnation: 1,
        });
        assert_eq!(
            PeerDirectory::from_snapshot(overlay, 4, invalid).unwrap_err(),
            DirectoryError::InvalidSnapshot
        );

        let mut invalid = snapshot;
        invalid.revocations.push(DirectoryRevocationSnapshot {
            node_id: "edge-a".to_owned(),
            overlay_ip: ip("10.42.0.3"),
            epoch: 1,
        });
        assert_eq!(
            PeerDirectory::from_snapshot(overlay, 4, invalid).unwrap_err(),
            DirectoryError::InvalidSnapshot
        );
        assert!(matches!(
            PeerDirectory::from_snapshot_json(
                overlay,
                4,
                &vec![b' '; MAX_DIRECTORY_SNAPSHOT_BYTES + 1],
            ),
            Err(DirectoryError::SnapshotTooLarge { .. })
        ));
    }

    #[test]
    fn snapshot_import_preserves_exhausted_monotonic_counters() {
        let overlay: Ipv4Net = "10.42.0.0/24".parse().expect("overlay");
        let snapshot = DirectorySnapshot {
            version: DIRECTORY_SNAPSHOT_VERSION,
            overlay,
            nodes: vec![DirectoryNodeSnapshot {
                node_id: "edge-a".to_owned(),
                overlay_ip: ip("10.42.0.2"),
                incarnation: u64::MAX,
            }],
            revocations: vec![DirectoryRevocationSnapshot {
                node_id: "edge-b".to_owned(),
                overlay_ip: ip("10.42.0.3"),
                epoch: u64::MAX,
            }],
        };
        let directory = PeerDirectory::from_snapshot(overlay, 4, snapshot).expect("snapshot");

        assert_eq!(
            directory.register(registration("edge-a", "10.42.0.2", 'a', SessionId::new(),)),
            Err(DirectoryError::IncarnationExhausted)
        );
        assert!(directory.is_empty());
        assert_eq!(
            directory.revoke("edge-b", ip("10.42.0.3"), u64::MAX),
            Err(DirectoryError::StaleEpoch {
                current: u64::MAX,
                received: u64::MAX,
            })
        );
        let exported = directory.export_snapshot();
        assert_eq!(exported.nodes[0].incarnation, u64::MAX);
        assert_eq!(exported.revocations[0].epoch, u64::MAX);
    }

    #[test]
    fn announcement_and_lookup_round_trip() {
        let directory = directory(4);
        let session = SessionId::new();
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', session))
            .expect("registration");
        directory
            .announce_candidates(
                ip("10.42.0.2"),
                session,
                7,
                vec![candidate("192.0.2.10:7000")],
            )
            .expect("announcement");

        let record = directory.lookup(ip("10.42.0.2"), 42).expect("lookup");
        assert_eq!(record.request_id, 42);
        assert_eq!(record.node_id, "edge-a");
        assert_eq!(record.overlay_ip, ip("10.42.0.2"));
        assert_eq!(record.incarnation, 1);
        assert_eq!(record.session_id, session.to_string());
        assert_eq!(record.certificate_fingerprint, fingerprint('a').as_str());
        assert_eq!(record.epoch, 7);
        assert_eq!(record.candidates, vec![candidate("192.0.2.10:7000")]);
        crate::protocol::ProtocolCodec::encode(&crate::protocol::ControlMessage::PeerRecord(
            record,
        ))
        .expect("directory record must satisfy the wire protocol");
    }

    #[test]
    fn registered_peer_without_candidates_is_wire_encodable() {
        let directory = directory(4);
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', SessionId::new()))
            .expect("registration");

        let record = directory.lookup(ip("10.42.0.2"), 1).expect("lookup");
        assert_eq!(record.epoch, 0);
        assert!(record.candidates.is_empty());
        crate::protocol::ProtocolCodec::encode(&crate::protocol::ControlMessage::PeerRecord(
            record,
        ))
        .expect("initial directory record must satisfy the wire protocol");
    }

    #[test]
    fn stale_and_replayed_epochs_are_rejected_without_mutation() {
        let directory = directory(4);
        let session = SessionId::new();
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', session))
            .expect("registration");
        let original = vec![candidate("192.0.2.10:7000")];
        directory
            .announce_candidates(ip("10.42.0.2"), session, 3, original.clone())
            .expect("initial announcement");

        for epoch in [2, 3] {
            assert!(matches!(
                directory.announce_candidates(
                    ip("10.42.0.2"),
                    session,
                    epoch,
                    vec![candidate("192.0.2.11:7000")]
                ),
                Err(DirectoryError::StaleEpoch {
                    current: 3,
                    received
                }) if received == epoch
            ));
        }
        assert_eq!(
            directory
                .lookup(ip("10.42.0.2"), 1)
                .expect("lookup")
                .candidates,
            original
        );
    }

    #[test]
    fn session_ownership_protects_updates_and_cleanup() {
        let directory = directory(4);
        let active = SessionId::new();
        let stale = SessionId::new();
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', active))
            .expect("registration");

        assert!(matches!(
            directory.announce_candidates(
                ip("10.42.0.2"),
                stale,
                1,
                vec![candidate("192.0.2.10:7000")]
            ),
            Err(DirectoryError::StaleSession)
        ));
        assert!(!directory.remove_session(ip("10.42.0.2"), stale));
        assert_eq!(directory.len(), 1);
        assert!(directory.remove_session(ip("10.42.0.2"), active));
        assert!(directory.is_empty());
    }

    #[test]
    fn replacement_session_advances_the_node_incarnation() {
        let directory = directory(4);
        let first = SessionId::new();
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', first))
            .expect("first registration");
        assert_eq!(
            directory
                .lookup(ip("10.42.0.2"), 1)
                .expect("first record")
                .incarnation,
            1
        );
        assert!(directory.remove_session(ip("10.42.0.2"), first));

        directory
            .register(registration("edge-a", "10.42.0.2", 'b', SessionId::new()))
            .expect("replacement registration");
        assert_eq!(
            directory
                .lookup(ip("10.42.0.2"), 2)
                .expect("replacement record")
                .incarnation,
            2
        );
    }

    #[test]
    fn disconnected_nodes_cannot_change_historical_static_bindings() {
        let directory = directory(4);
        let first = SessionId::new();
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', first))
            .expect("first registration");
        assert!(directory.remove_session(ip("10.42.0.2"), first));

        assert_eq!(
            directory.register(registration("edge-a", "10.42.0.3", 'b', SessionId::new())),
            Err(DirectoryError::HistoricalBindingMismatch)
        );
        assert_eq!(
            directory.register(registration("edge-b", "10.42.0.2", 'b', SessionId::new())),
            Err(DirectoryError::HistoricalBindingMismatch)
        );
        assert_eq!(
            directory.revoke("edge-a", ip("10.42.0.3"), 1),
            Err(DirectoryError::RevocationBindingMismatch)
        );
        assert_eq!(
            directory.revoke("edge-b", ip("10.42.0.2"), 1),
            Err(DirectoryError::RevocationBindingMismatch)
        );
        assert!(!directory.is_revoked("edge-a"));
        assert!(!directory.is_revoked("edge-b"));

        directory
            .register(registration("edge-a", "10.42.0.2", 'b', SessionId::new()))
            .expect("same static binding may reconnect");
        assert_eq!(
            directory
                .lookup(ip("10.42.0.2"), 1)
                .expect("replacement record")
                .incarnation,
            2
        );
    }

    #[test]
    fn registration_rejects_non_v4_session_ids() {
        let directory = directory(4);
        let invalid_session = uuid::Uuid::nil()
            .to_string()
            .parse::<SessionId>()
            .expect("routing SessionId accepts UUID syntax");
        assert_eq!(
            directory.register(registration("edge-a", "10.42.0.2", 'a', invalid_session,)),
            Err(DirectoryError::InvalidSessionId)
        );
        assert!(directory.is_empty());
    }

    #[test]
    fn node_address_and_fingerprint_are_unique() {
        let directory = directory(8);
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', SessionId::new()))
            .expect("first registration");
        assert!(matches!(
            directory.register(registration("edge-a", "10.42.0.3", 'b', SessionId::new())),
            Err(DirectoryError::DuplicateNode(_))
        ));
        assert!(matches!(
            directory.register(registration("edge-b", "10.42.0.2", 'b', SessionId::new())),
            Err(DirectoryError::DuplicateOverlayAddress(_))
        ));
        assert!(matches!(
            directory.register(registration("edge-b", "10.42.0.3", 'a', SessionId::new())),
            Err(DirectoryError::DuplicateCertificateFingerprint)
        ));
    }

    #[test]
    fn candidate_input_is_bounded_and_cannot_assert_observation() {
        let directory = directory(4);
        let session = SessionId::new();
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', session))
            .expect("registration");

        let duplicate = candidate("192.0.2.10:7000");
        assert!(matches!(
            directory.announce_candidates(
                ip("10.42.0.2"),
                session,
                1,
                vec![duplicate.clone(), duplicate]
            ),
            Err(DirectoryError::DuplicateCandidate(_))
        ));
        let mut observed = candidate("198.51.100.2:7000");
        observed.kind = CandidateKind::ServerReflexive;
        assert!(matches!(
            directory.announce_candidates(ip("10.42.0.2"), session, 1, vec![observed]),
            Err(DirectoryError::UntrustedReflexiveCandidate)
        ));
        assert!(matches!(
            directory.announce_candidates(
                ip("10.42.0.2"),
                session,
                1,
                vec![candidate("192.0.2.10:7000"); MAX_CANDIDATES_PER_PEER + 1]
            ),
            Err(DirectoryError::TooManyCandidates { .. })
        ));
        assert!(matches!(
            directory.announce_candidates(
                ip("10.42.0.2"),
                session,
                1,
                vec![candidate("[2001:db8::1]:7000")]
            ),
            Err(DirectoryError::InvalidCandidate(_))
        ));
        for address in ["0.1.2.3:7000", "127.0.0.1:7000"] {
            assert!(matches!(
                directory.announce_candidates(
                    ip("10.42.0.2"),
                    session,
                    1,
                    vec![candidate(address)],
                ),
                Err(DirectoryError::InvalidCandidate(_))
            ));
        }
    }

    #[test]
    fn revocation_removes_active_peer_and_rejects_replay_and_reregistration() {
        let directory = directory(4);
        let active_session = SessionId::new();
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', active_session))
            .expect("registration");
        let revoked = directory
            .revoke("edge-a", ip("10.42.0.2"), 9)
            .expect("revocation");
        assert_eq!(revoked.notification.node_id, "edge-a");
        assert_eq!(revoked.revoked_session, Some(active_session));
        assert!(directory.is_empty());
        assert!(directory.is_revoked("edge-a"));
        assert!(matches!(
            directory.revoke("edge-a", ip("10.42.0.2"), 9),
            Err(DirectoryError::StaleEpoch { .. })
        ));
        assert!(matches!(
            directory.register(registration("edge-a", "10.42.0.2", 'b', SessionId::new())),
            Err(DirectoryError::NodeRevoked(_))
        ));
        assert!(matches!(
            directory.register(registration("edge-b", "10.42.0.2", 'b', SessionId::new())),
            Err(DirectoryError::OverlayAddressRevoked(_))
        ));
        assert!(matches!(
            directory.revoke("edge-b", ip("10.42.0.2"), 10),
            Err(DirectoryError::RevocationBindingMismatch)
        ));
    }

    #[test]
    fn revocation_cannot_target_another_nodes_active_address() {
        let directory = directory(4);
        directory
            .register(registration("edge-a", "10.42.0.2", 'a', SessionId::new()))
            .expect("registration");

        assert!(matches!(
            directory.revoke("edge-b", ip("10.42.0.2"), 1),
            Err(DirectoryError::RevocationBindingMismatch)
        ));
        assert_eq!(
            directory
                .lookup(ip("10.42.0.2"), 1)
                .expect("active binding remains")
                .node_id,
            "edge-a"
        );
        assert!(!directory.is_revoked("edge-b"));
    }

    #[test]
    fn revocation_state_is_bounded() {
        let directory = PeerDirectory::new(
            "10.42.0.0/22".parse().expect("overlay"),
            MAX_DIRECTORY_PEERS,
        )
        .expect("directory");

        for index in 0..MAX_DIRECTORY_PEERS {
            let offset = index + 1;
            let address = Ipv4Addr::new(10, 42, (offset / 256) as u8, (offset % 256) as u8);
            directory
                .revoke(&format!("node-{index}"), address, 1)
                .expect("revocation within capacity");
        }

        let notifications = directory.revocation_notifications();
        assert_eq!(notifications.len(), MAX_DIRECTORY_PEERS);
        assert!(
            notifications
                .windows(2)
                .all(|pair| pair[0].node_id < pair[1].node_id)
        );
        for notification in notifications {
            crate::protocol::ControlMessage::PeerRevoked(notification)
                .validate()
                .expect("directory revocation must be wire-valid");
        }

        assert!(matches!(
            directory.revoke("over-capacity", ip("10.42.2.1"), 1),
            Err(DirectoryError::RevocationCapacityReached(
                MAX_DIRECTORY_PEERS
            ))
        ));
    }

    #[test]
    fn incarnation_state_is_bounded() {
        let overlay: Ipv4Net = "10.42.0.0/22".parse().expect("overlay");
        let directory = PeerDirectory::new(overlay, MAX_DIRECTORY_PEERS).expect("directory");

        for index in 0..MAX_DIRECTORY_PEERS {
            let offset = index + 1;
            let address = Ipv4Addr::new(10, 42, (offset / 256) as u8, (offset % 256) as u8);
            let session = SessionId::new();
            directory
                .register(PeerRegistration {
                    node_id: format!("node-{index}"),
                    overlay_ip: address,
                    certificate_fingerprint: fingerprint(if index % 2 == 0 { 'a' } else { 'b' }),
                    certificate_not_after_unix_seconds: 2_000_000_000,
                    session_id: session,
                })
                .expect("registration within capacity");
            assert!(directory.remove_session(address, session));
        }

        assert!(matches!(
            directory.register(PeerRegistration {
                node_id: "over-capacity".to_owned(),
                overlay_ip: ip("10.42.2.1"),
                certificate_fingerprint: fingerprint('c'),
                certificate_not_after_unix_seconds: 2_000_000_000,
                session_id: SessionId::new(),
            }),
            Err(DirectoryError::IncarnationCapacityReached(
                MAX_DIRECTORY_PEERS
            ))
        ));
    }

    #[test]
    fn invalid_overlay_bindings_and_request_ids_are_rejected() {
        let directory = directory(4);
        assert!(matches!(
            directory.register(registration("edge-a", "10.43.0.2", 'a', SessionId::new())),
            Err(DirectoryError::InvalidOverlayAddress(_))
        ));
        assert!(matches!(
            directory.lookup(ip("10.42.0.2"), 0),
            Err(DirectoryError::InvalidRequestId)
        ));
    }
}
