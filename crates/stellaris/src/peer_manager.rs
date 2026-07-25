// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounded, transport-independent P2P path selection state.
//!
//! This module does not create, authenticate, or close connections. Callers
//! carry incarnation-bound probe generations into asynchronous work and own
//! the transport connections identified by [`PeerConnectionId`].

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::Mutex,
    time::{Duration, Instant},
};

use ipnet::Ipv4Net;

use crate::{
    coordination::CertificateFingerprint,
    protocol::{
        Candidate, CandidateKind, ConnectPlan, ConnectionRole, MAX_CANDIDATES, PeerDescriptor,
        PeerRecord, PeerRevoked,
    },
    registry::{validate_node_id, validate_overlay_address},
    routing::SessionId,
};

pub const DEFAULT_MAX_MANAGED_PEERS: usize = 256;
pub const MAX_MANAGED_PEERS: usize = 256;
pub const MIN_P2P_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_P2P_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
pub const DEFAULT_P2P_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub const INITIAL_DIAL_BACKOFF: Duration = Duration::from_secs(1);
pub const MAX_DIAL_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProbeGeneration {
    generation: u64,
    incarnation: u64,
    session_id: SessionId,
}

impl ProbeGeneration {
    pub const fn get(self) -> u64 {
        self.generation
    }

    pub const fn session_id(self) -> SessionId {
        self.session_id
    }

    pub const fn incarnation(self) -> u64 {
        self.incarnation
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PeerConnectionId(u64);

impl PeerConnectionId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionDirection {
    Inbound,
    Outbound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectedPath {
    Relay,
    P2p(PeerConnectionId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathDecision {
    pub selected: SelectedPath,
    pub action: Option<ConnectionAction>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionCloseReason {
    CandidateChanged,
    PeerReplaced,
    CertificateExpired,
    Idle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionAction {
    Dial {
        overlay_ip: Ipv4Addr,
        generation: ProbeGeneration,
        plan_connection_id: SessionId,
        role: ConnectionRole,
        candidate: Candidate,
    },
    AwaitInbound {
        overlay_ip: Ipv4Addr,
        generation: ProbeGeneration,
        plan_connection_id: SessionId,
    },
    Close {
        overlay_ip: Ipv4Addr,
        connection_id: PeerConnectionId,
        reason: ConnectionCloseReason,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectPlanStart {
    pub generation: ProbeGeneration,
    pub superseded_generation: Option<ProbeGeneration>,
    pub actions: Vec<ConnectionAction>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbePlanSnapshot {
    pub plan_connection_id: SessionId,
    pub role: ConnectionRole,
    pub expires_at_unix_seconds: u64,
    pub next_candidate: usize,
    pub completed_rounds: u32,
    pub retry_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyOutcome {
    Activated {
        connection_id: PeerConnectionId,
    },
    Replaced {
        connection_id: PeerConnectionId,
        connection_to_close: PeerConnectionId,
    },
    KeptExisting {
        connection_id: PeerConnectionId,
        connection_to_close: PeerConnectionId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerPathState {
    RelayOnly,
    Probing {
        generation: ProbeGeneration,
    },
    P2pReady {
        generation: ProbeGeneration,
        connection_id: PeerConnectionId,
        direction: ConnectionDirection,
        last_used: Instant,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerSnapshot {
    pub overlay_ip: Ipv4Addr,
    pub node_id: String,
    pub incarnation: u64,
    pub session_id: SessionId,
    pub certificate_fingerprint: CertificateFingerprint,
    pub certificate_not_after_unix_seconds: u64,
    pub candidate_epoch: u64,
    pub candidates: Vec<Candidate>,
    pub path: PeerPathState,
    pub probe_plan: Option<ProbePlanSnapshot>,
}

#[derive(Clone, Debug)]
struct PeerEntry {
    node_id: String,
    incarnation: u64,
    session_id: SessionId,
    certificate_fingerprint: CertificateFingerprint,
    certificate_not_after_unix_seconds: u64,
    candidate_epoch: u64,
    candidates: Vec<Candidate>,
    path: PeerPathState,
    probe_plan: Option<ProbePlanState>,
}

#[derive(Clone, Debug)]
struct ProbePlanState {
    plan_connection_id: SessionId,
    role: ConnectionRole,
    expires_at_unix_seconds: u64,
    next_candidate: usize,
    completed_rounds: u32,
    retry_at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct RevocationTombstone {
    overlay_ip: Ipv4Addr,
    epoch: u64,
}

#[derive(Clone, Debug)]
struct PeerFreshness {
    incarnation: u64,
    session_id: SessionId,
    certificate_fingerprint: CertificateFingerprint,
    certificate_not_after_unix_seconds: u64,
    candidate_epoch: u64,
    candidates: Vec<Candidate>,
}

#[derive(Clone, Debug)]
struct HistoricalBinding {
    overlay_ip: Ipv4Addr,
    freshness: Option<PeerFreshness>,
}

impl PeerEntry {
    fn snapshot(&self, overlay_ip: Ipv4Addr) -> PeerSnapshot {
        PeerSnapshot {
            overlay_ip,
            node_id: self.node_id.clone(),
            incarnation: self.incarnation,
            session_id: self.session_id,
            certificate_fingerprint: self.certificate_fingerprint.clone(),
            certificate_not_after_unix_seconds: self.certificate_not_after_unix_seconds,
            candidate_epoch: self.candidate_epoch,
            candidates: self.candidates.clone(),
            path: self.path,
            probe_plan: self.probe_plan.as_ref().map(|plan| ProbePlanSnapshot {
                plan_connection_id: plan.plan_connection_id,
                role: plan.role,
                expires_at_unix_seconds: plan.expires_at_unix_seconds,
                next_candidate: plan.next_candidate,
                completed_rounds: plan.completed_rounds,
                retry_at: plan.retry_at,
            }),
        }
    }
}

#[derive(Debug, Default)]
struct ManagerState {
    peers: HashMap<Ipv4Addr, PeerEntry>,
    by_node: HashMap<String, Ipv4Addr>,
    historical_by_node: HashMap<String, HistoricalBinding>,
    historical_by_overlay: HashMap<Ipv4Addr, String>,
    revocations: HashMap<String, RevocationTombstone>,
    last_probe_generation: u64,
    last_connection_id: u64,
}

#[derive(Debug)]
pub struct PeerManager {
    local_node_id: String,
    overlay: Ipv4Net,
    max_peers: usize,
    idle_timeout: Duration,
    state: Mutex<ManagerState>,
}

impl PeerManager {
    pub fn with_defaults(
        local_node_id: impl Into<String>,
        overlay: Ipv4Net,
    ) -> Result<Self, PeerManagerError> {
        Self::new(
            local_node_id,
            overlay,
            DEFAULT_MAX_MANAGED_PEERS,
            DEFAULT_P2P_IDLE_TIMEOUT,
        )
    }

    pub fn new(
        local_node_id: impl Into<String>,
        overlay: Ipv4Net,
        max_peers: usize,
        idle_timeout: Duration,
    ) -> Result<Self, PeerManagerError> {
        let local_node_id = local_node_id.into();
        validate_node_id(&local_node_id).map_err(|_| PeerManagerError::InvalidNodeId)?;
        if max_peers == 0 || max_peers > MAX_MANAGED_PEERS {
            return Err(PeerManagerError::InvalidCapacity(max_peers));
        }
        if !(MIN_P2P_IDLE_TIMEOUT..=MAX_P2P_IDLE_TIMEOUT).contains(&idle_timeout) {
            return Err(PeerManagerError::InvalidIdleTimeout(idle_timeout));
        }
        Ok(Self {
            local_node_id,
            overlay,
            max_peers,
            idle_timeout,
            state: Mutex::new(ManagerState::default()),
        })
    }

    pub fn local_node_id(&self) -> &str {
        &self.local_node_id
    }

    pub const fn overlay(&self) -> Ipv4Net {
        self.overlay
    }

    pub const fn max_peers(&self) -> usize {
        self.max_peers
    }

    pub const fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    /// Installs an authenticated candidate snapshot for one node incarnation.
    ///
    /// The coordinator-assigned incarnation must not decrease. Within one
    /// incarnation, the session is immutable and epochs must not decrease; an
    /// equal epoch is an idempotent directory lookup result. A newer
    /// incarnation may restart at epoch zero. Candidate changes invalidate
    /// in-flight probes and ready paths. A newer incarnation also invalidates
    /// a ready path because its session and certificate identity have changed.
    pub fn update_candidates(
        &self,
        overlay_ip: Ipv4Addr,
        node_id: impl Into<String>,
        incarnation: u64,
        session_id: SessionId,
        certificate_fingerprint: CertificateFingerprint,
        epoch: u64,
    ) -> Result<Option<PeerConnectionId>, PeerManagerError> {
        self.update_peer(
            overlay_ip,
            node_id.into(),
            incarnation,
            session_id,
            certificate_fingerprint,
            u64::MAX,
            epoch,
            Vec::new(),
        )
    }

    pub fn update_peer_record(
        &self,
        record: &PeerRecord,
    ) -> Result<Option<PeerConnectionId>, PeerManagerError> {
        if record.request_id == 0 {
            return Err(PeerManagerError::InvalidRequestId);
        }
        self.update_peer(
            record.overlay_ip,
            record.node_id.clone(),
            record.incarnation,
            parse_session_id(&record.session_id)?,
            CertificateFingerprint::from_str(&record.certificate_fingerprint)
                .map_err(|_| PeerManagerError::InvalidCertificateFingerprint)?,
            record.certificate_not_after_unix_seconds,
            record.epoch,
            validate_candidates(record.epoch, &record.candidates)?,
        )
    }

    pub fn update_peer_descriptor(
        &self,
        descriptor: &PeerDescriptor,
    ) -> Result<Option<PeerConnectionId>, PeerManagerError> {
        self.update_peer(
            descriptor.overlay_ip,
            descriptor.node_id.clone(),
            descriptor.incarnation,
            parse_session_id(&descriptor.session_id)?,
            CertificateFingerprint::from_str(&descriptor.certificate_fingerprint)
                .map_err(|_| PeerManagerError::InvalidCertificateFingerprint)?,
            descriptor.certificate_not_after_unix_seconds,
            descriptor.candidate_epoch,
            validate_candidates(descriptor.candidate_epoch, &descriptor.candidates)?,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update_peer(
        &self,
        overlay_ip: Ipv4Addr,
        node_id: String,
        incarnation: u64,
        session_id: SessionId,
        certificate_fingerprint: CertificateFingerprint,
        certificate_not_after_unix_seconds: u64,
        epoch: u64,
        candidates: Vec<Candidate>,
    ) -> Result<Option<PeerConnectionId>, PeerManagerError> {
        validate_overlay_address(self.overlay, overlay_ip)
            .map_err(|_| PeerManagerError::InvalidOverlayAddress(overlay_ip))?;
        validate_node_id(&node_id).map_err(|_| PeerManagerError::InvalidNodeId)?;
        if node_id == self.local_node_id {
            return Err(PeerManagerError::LocalPeerBinding);
        }
        if incarnation == 0 {
            return Err(PeerManagerError::InvalidIncarnation);
        }
        if certificate_not_after_unix_seconds == 0 {
            return Err(PeerManagerError::InvalidCertificateExpiry);
        }
        validate_session_id(session_id)?;
        let incoming_freshness = PeerFreshness {
            incarnation,
            session_id,
            certificate_fingerprint: certificate_fingerprint.clone(),
            certificate_not_after_unix_seconds,
            candidate_epoch: epoch,
            candidates: candidates.clone(),
        };

        let mut state = self.lock_state();
        if let Some(existing_overlay_ip) = state.historical_by_node.get(&node_id)
            && existing_overlay_ip.overlay_ip != overlay_ip
        {
            return Err(PeerManagerError::NodeAlreadyBound {
                node_id,
                existing_overlay_ip: existing_overlay_ip.overlay_ip,
                received_overlay_ip: overlay_ip,
            });
        }
        if let Some(existing_node_id) = state.historical_by_overlay.get(&overlay_ip)
            && existing_node_id != &node_id
        {
            return Err(PeerManagerError::PeerBindingMismatch {
                expected: existing_node_id.clone(),
                received: node_id,
            });
        }
        if let Some(revocation) = state.revocations.get(&node_id) {
            return Err(PeerManagerError::PeerRevoked {
                node_id,
                epoch: revocation.epoch,
            });
        }

        if state.peers.contains_key(&overlay_ip) {
            let entry = state
                .peers
                .get_mut(&overlay_ip)
                .expect("peer was checked while holding the manager lock");
            if entry.node_id != node_id {
                return Err(PeerManagerError::PeerBindingMismatch {
                    expected: entry.node_id.clone(),
                    received: node_id,
                });
            }

            if incarnation < entry.incarnation {
                return Err(PeerManagerError::StaleIncarnation {
                    current: entry.incarnation,
                    received: incarnation,
                });
            }

            if incarnation == entry.incarnation {
                if session_id != entry.session_id {
                    return Err(PeerManagerError::IncarnationSessionMismatch {
                        incarnation,
                        expected: entry.session_id,
                        received: session_id,
                    });
                }
                if certificate_fingerprint != entry.certificate_fingerprint {
                    return Err(PeerManagerError::IncarnationFingerprintMismatch { incarnation });
                }
                if certificate_not_after_unix_seconds != entry.certificate_not_after_unix_seconds {
                    return Err(PeerManagerError::IncarnationCertificateExpiryMismatch {
                        incarnation,
                    });
                }
                if epoch < entry.candidate_epoch {
                    return Err(PeerManagerError::StaleCandidateEpoch {
                        current: entry.candidate_epoch,
                        received: epoch,
                    });
                }
                if epoch == entry.candidate_epoch {
                    if candidates != entry.candidates {
                        return Err(PeerManagerError::CandidateSnapshotMismatch { epoch });
                    }
                    return Ok(None);
                }
            }

            let connection_to_close = if incarnation > entry.incarnation {
                entry.incarnation = incarnation;
                entry.session_id = session_id;
                entry.certificate_fingerprint = certificate_fingerprint;
                entry.certificate_not_after_unix_seconds = certificate_not_after_unix_seconds;
                let connection_to_close = match entry.path {
                    PeerPathState::P2pReady { connection_id, .. } => Some(connection_id),
                    PeerPathState::RelayOnly | PeerPathState::Probing { .. } => None,
                };
                entry.candidate_epoch = epoch;
                entry.candidates = candidates;
                entry.path = PeerPathState::RelayOnly;
                entry.probe_plan = None;
                connection_to_close
            } else {
                entry.candidate_epoch = epoch;
                entry.candidates = candidates;
                let connection_to_close = match entry.path {
                    PeerPathState::P2pReady { connection_id, .. } => Some(connection_id),
                    PeerPathState::RelayOnly | PeerPathState::Probing { .. } => None,
                };
                entry.path = PeerPathState::RelayOnly;
                entry.probe_plan = None;
                connection_to_close
            };
            state
                .historical_by_node
                .get_mut(&node_id)
                .expect("active peers retain their historical binding")
                .freshness = Some(incoming_freshness);
            return Ok(connection_to_close);
        }

        if let Some(current) = state
            .historical_by_node
            .get(&node_id)
            .and_then(|binding| binding.freshness.as_ref())
        {
            validate_historical_freshness(current, &incoming_freshness)?;
        }

        if let Some(existing_overlay_ip) = state.by_node.get(&node_id) {
            return Err(PeerManagerError::NodeAlreadyBound {
                node_id,
                existing_overlay_ip: *existing_overlay_ip,
                received_overlay_ip: overlay_ip,
            });
        }
        if state.peers.len() >= self.max_peers {
            return Err(PeerManagerError::CapacityReached(self.max_peers));
        }
        if let Some(binding) = state.historical_by_node.get_mut(&node_id) {
            binding.freshness = Some(incoming_freshness);
        } else {
            if state.historical_by_node.len() >= MAX_MANAGED_PEERS {
                return Err(PeerManagerError::HistoricalBindingCapacityReached(
                    MAX_MANAGED_PEERS,
                ));
            }
            state.historical_by_node.insert(
                node_id.clone(),
                HistoricalBinding {
                    overlay_ip,
                    freshness: Some(incoming_freshness),
                },
            );
            state
                .historical_by_overlay
                .insert(overlay_ip, node_id.clone());
        }
        state.by_node.insert(node_id.clone(), overlay_ip);
        state.peers.insert(
            overlay_ip,
            PeerEntry {
                node_id,
                incarnation,
                session_id,
                certificate_fingerprint,
                certificate_not_after_unix_seconds,
                candidate_epoch: epoch,
                candidates,
                path: PeerPathState::RelayOnly,
                probe_plan: None,
            },
        );
        Ok(None)
    }

    /// Applies a coordinator-authenticated revocation notification.
    ///
    /// Tombstones and their node/address bindings survive [`Self::forget_peer`]
    /// so a delayed directory response cannot recreate a revoked peer. A newer
    /// revision is accepted idempotently with respect to path cleanup, while an
    /// equal or older revision is rejected as replay.
    pub fn apply_revocation(
        &self,
        revocation: PeerRevoked,
    ) -> Result<Option<PeerConnectionId>, PeerManagerError> {
        validate_node_id(&revocation.node_id).map_err(|_| PeerManagerError::InvalidNodeId)?;
        validate_overlay_address(self.overlay, revocation.overlay_ip)
            .map_err(|_| PeerManagerError::InvalidOverlayAddress(revocation.overlay_ip))?;
        if revocation.node_id == self.local_node_id {
            return Err(PeerManagerError::LocalPeerBinding);
        }
        if revocation.epoch == 0 {
            return Err(PeerManagerError::InvalidRevocationEpoch);
        }

        let mut state = self.lock_state();
        if state
            .historical_by_node
            .get(&revocation.node_id)
            .is_some_and(|binding| binding.overlay_ip != revocation.overlay_ip)
            || state
                .historical_by_overlay
                .get(&revocation.overlay_ip)
                .is_some_and(|node_id| node_id != &revocation.node_id)
            || state
                .by_node
                .get(&revocation.node_id)
                .is_some_and(|overlay_ip| *overlay_ip != revocation.overlay_ip)
            || state
                .peers
                .get(&revocation.overlay_ip)
                .is_some_and(|entry| entry.node_id != revocation.node_id)
        {
            return Err(PeerManagerError::RevocationBindingMismatch);
        }

        if let Some(current) = state.revocations.get(&revocation.node_id) {
            if current.overlay_ip != revocation.overlay_ip {
                return Err(PeerManagerError::RevocationBindingMismatch);
            }
            if revocation.epoch <= current.epoch {
                return Err(PeerManagerError::StaleRevocationEpoch {
                    current: current.epoch,
                    received: revocation.epoch,
                });
            }
        } else if state.revocations.len() >= MAX_MANAGED_PEERS {
            return Err(PeerManagerError::RevocationCapacityReached(
                MAX_MANAGED_PEERS,
            ));
        }

        if !state.historical_by_node.contains_key(&revocation.node_id) {
            if state.historical_by_node.len() >= MAX_MANAGED_PEERS {
                return Err(PeerManagerError::HistoricalBindingCapacityReached(
                    MAX_MANAGED_PEERS,
                ));
            }
            state.historical_by_node.insert(
                revocation.node_id.clone(),
                HistoricalBinding {
                    overlay_ip: revocation.overlay_ip,
                    freshness: None,
                },
            );
            state
                .historical_by_overlay
                .insert(revocation.overlay_ip, revocation.node_id.clone());
        }

        let connection_to_close = state
            .peers
            .remove(&revocation.overlay_ip)
            .and_then(|entry| match entry.path {
                PeerPathState::P2pReady { connection_id, .. } => Some(connection_id),
                PeerPathState::RelayOnly | PeerPathState::Probing { .. } => None,
            });
        if state.by_node.get(&revocation.node_id) == Some(&revocation.overlay_ip) {
            state.by_node.remove(&revocation.node_id);
        }
        state.revocations.insert(
            revocation.node_id,
            RevocationTombstone {
                overlay_ip: revocation.overlay_ip,
                epoch: revocation.epoch,
            },
        );
        Ok(connection_to_close)
    }

    /// Starts or supersedes a probe for the current candidate epoch.
    pub fn begin_probe(&self, overlay_ip: Ipv4Addr) -> Result<ProbeGeneration, PeerManagerError> {
        let mut state = self.lock_state();
        let entry = state
            .peers
            .get(&overlay_ip)
            .ok_or(PeerManagerError::PeerNotFound(overlay_ip))?;
        if entry.candidate_epoch == 0 {
            return Err(PeerManagerError::CandidatesNotAnnounced(overlay_ip));
        }
        if matches!(entry.path, PeerPathState::P2pReady { .. }) {
            return Err(PeerManagerError::P2pAlreadyReady(overlay_ip));
        }

        let generation = ProbeGeneration {
            generation: state
                .last_probe_generation
                .checked_add(1)
                .ok_or(PeerManagerError::ProbeGenerationExhausted)?,
            incarnation: entry.incarnation,
            session_id: entry.session_id,
        };
        state.last_probe_generation = generation.get();
        state
            .peers
            .get_mut(&overlay_ip)
            .expect("peer was checked while holding the manager lock")
            .path = PeerPathState::Probing { generation };
        Ok(generation)
    }

    /// Installs a coordinator-authenticated connection plan and returns the
    /// transport work that can be executed without consulting manager state
    /// again. Data remains on Relay until [`Self::mark_ready_for_plan`].
    pub fn start_connect_plan(
        &self,
        plan: &ConnectPlan,
        now: Instant,
        now_unix_seconds: u64,
    ) -> Result<ConnectPlanStart, PeerManagerError> {
        if plan.request_id == 0 {
            return Err(PeerManagerError::InvalidRequestId);
        }
        if plan.expires_at_unix_seconds <= now_unix_seconds {
            return Err(PeerManagerError::ConnectPlanExpired);
        }
        if plan.peer.certificate_not_after_unix_seconds <= now_unix_seconds {
            return Err(PeerManagerError::PeerCertificateExpired);
        }
        if plan.peer.candidates.is_empty() {
            return Err(PeerManagerError::NoUsableCandidates(plan.peer.overlay_ip));
        }
        let plan_connection_id = parse_session_id(&plan.connection_id)?;
        let expected_session_id = parse_session_id(&plan.peer.session_id)?;
        let expected_fingerprint =
            CertificateFingerprint::from_str(&plan.peer.certificate_fingerprint)
                .map_err(|_| PeerManagerError::InvalidCertificateFingerprint)?;
        let expected_candidates =
            validate_candidates(plan.peer.candidate_epoch, &plan.peer.candidates)?;
        let previous = self.snapshot(plan.peer.overlay_ip);
        let connection_to_close = self.update_peer_descriptor(&plan.peer)?;

        let mut state = self.lock_state();
        let entry = state
            .peers
            .get(&plan.peer.overlay_ip)
            .ok_or(PeerManagerError::PeerNotFound(plan.peer.overlay_ip))?;
        if entry.candidates.is_empty() {
            return Err(PeerManagerError::NoUsableCandidates(plan.peer.overlay_ip));
        }
        if entry.node_id != plan.peer.node_id
            || entry.incarnation != plan.peer.incarnation
            || entry.session_id != expected_session_id
            || entry.certificate_fingerprint != expected_fingerprint
            || entry.certificate_not_after_unix_seconds
                != plan.peer.certificate_not_after_unix_seconds
            || entry.candidate_epoch != plan.peer.candidate_epoch
            || entry.candidates != expected_candidates
        {
            return Err(PeerManagerError::ConnectPlanPeerChanged);
        }
        if matches!(entry.path, PeerPathState::P2pReady { .. }) {
            return Err(PeerManagerError::P2pAlreadyReady(plan.peer.overlay_ip));
        }
        let superseded_generation = previous.as_ref().and_then(|peer| match peer.path {
            PeerPathState::Probing { generation } => Some(generation),
            PeerPathState::RelayOnly | PeerPathState::P2pReady { .. } => None,
        });

        let generation = ProbeGeneration {
            generation: state
                .last_probe_generation
                .checked_add(1)
                .ok_or(PeerManagerError::ProbeGenerationExhausted)?,
            incarnation: entry.incarnation,
            session_id: entry.session_id,
        };
        state.last_probe_generation = generation.get();
        let entry = state
            .peers
            .get_mut(&plan.peer.overlay_ip)
            .expect("peer was checked while holding the manager lock");
        entry.path = PeerPathState::Probing { generation };
        entry.probe_plan = Some(ProbePlanState {
            plan_connection_id,
            role: plan.role,
            expires_at_unix_seconds: plan.expires_at_unix_seconds,
            next_candidate: 0,
            completed_rounds: 0,
            retry_at: now,
        });

        let mut actions = Vec::with_capacity(3);
        if let Some(connection_id) = connection_to_close {
            actions.push(ConnectionAction::Close {
                overlay_ip: plan.peer.overlay_ip,
                connection_id,
                reason: if previous
                    .as_ref()
                    .is_some_and(|peer| peer.incarnation == plan.peer.incarnation)
                {
                    ConnectionCloseReason::CandidateChanged
                } else {
                    ConnectionCloseReason::PeerReplaced
                },
            });
        }
        if plan.role == ConnectionRole::Responder {
            actions.push(ConnectionAction::AwaitInbound {
                overlay_ip: plan.peer.overlay_ip,
                generation,
                plan_connection_id,
            });
        }
        let candidate = entry
            .candidates
            .first()
            .expect("non-empty candidates were checked")
            .clone();
        entry
            .probe_plan
            .as_mut()
            .expect("connection plan was installed")
            .next_candidate = 1;
        actions.push(ConnectionAction::Dial {
            overlay_ip: plan.peer.overlay_ip,
            generation,
            plan_connection_id,
            role: plan.role,
            candidate,
        });
        Ok(ConnectPlanStart {
            generation,
            superseded_generation,
            actions,
        })
    }

    /// Returns the next due candidate dial. A caller reports failures through
    /// [`Self::dial_failed_at`], which advances to the next candidate or starts
    /// a bounded exponential backoff after a complete round.
    pub fn next_dial_action(
        &self,
        overlay_ip: Ipv4Addr,
        now: Instant,
        now_unix_seconds: u64,
    ) -> Result<Option<ConnectionAction>, PeerManagerError> {
        let mut state = self.lock_state();
        let entry = state
            .peers
            .get_mut(&overlay_ip)
            .ok_or(PeerManagerError::PeerNotFound(overlay_ip))?;
        ensure_peer_and_plan_current(entry, now_unix_seconds)?;
        let generation = match entry.path {
            PeerPathState::Probing { generation } => generation,
            PeerPathState::P2pReady { .. } => return Ok(None),
            PeerPathState::RelayOnly => return Err(PeerManagerError::NoActiveProbe),
        };
        let plan = entry
            .probe_plan
            .as_mut()
            .ok_or(PeerManagerError::NoConnectionPlan)?;
        if now < plan.retry_at {
            return Ok(None);
        }
        let Some(candidate) = entry.candidates.get(plan.next_candidate).cloned() else {
            return Ok(None);
        };
        plan.next_candidate += 1;
        let role = plan.role;
        Ok(Some(ConnectionAction::Dial {
            overlay_ip,
            generation,
            plan_connection_id: plan.plan_connection_id,
            role,
            candidate,
        }))
    }

    pub fn dial_failed_at(
        &self,
        overlay_ip: Ipv4Addr,
        generation: ProbeGeneration,
        now: Instant,
        now_unix_seconds: u64,
    ) -> Result<bool, PeerManagerError> {
        let mut state = self.lock_state();
        let entry = state
            .peers
            .get_mut(&overlay_ip)
            .ok_or(PeerManagerError::PeerNotFound(overlay_ip))?;
        ensure_peer_and_plan_current(entry, now_unix_seconds)?;
        if entry.path != (PeerPathState::Probing { generation }) {
            return Ok(false);
        }
        let plan = entry
            .probe_plan
            .as_mut()
            .ok_or(PeerManagerError::NoConnectionPlan)?;
        if plan.next_candidate < entry.candidates.len() {
            plan.retry_at = now;
            return Ok(true);
        }
        plan.completed_rounds = plan
            .completed_rounds
            .checked_add(1)
            .ok_or(PeerManagerError::DialFailureCounterExhausted)?;
        plan.next_candidate = 0;
        plan.retry_at = now
            .checked_add(dial_backoff(plan.completed_rounds))
            .ok_or(PeerManagerError::DeadlineOverflow)?;
        Ok(true)
    }

    pub fn generation_for_plan(
        &self,
        overlay_ip: Ipv4Addr,
        plan_connection_id: SessionId,
        now_unix_seconds: u64,
    ) -> Result<ProbeGeneration, PeerManagerError> {
        let state = self.lock_state();
        let entry = state
            .peers
            .get(&overlay_ip)
            .ok_or(PeerManagerError::PeerNotFound(overlay_ip))?;
        ensure_peer_and_plan_current(entry, now_unix_seconds)?;
        let plan = entry
            .probe_plan
            .as_ref()
            .ok_or(PeerManagerError::NoConnectionPlan)?;
        if plan.plan_connection_id != plan_connection_id {
            return Err(PeerManagerError::ConnectionPlanMismatch);
        }
        match entry.path {
            PeerPathState::Probing { generation } | PeerPathState::P2pReady { generation, .. } => {
                Ok(generation)
            }
            PeerPathState::RelayOnly => Err(PeerManagerError::NoActiveProbe),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mark_ready_for_plan(
        &self,
        overlay_ip: Ipv4Addr,
        plan_connection_id: SessionId,
        authenticated_fingerprint: &CertificateFingerprint,
        direction: ConnectionDirection,
        now: Instant,
        now_unix_seconds: u64,
    ) -> Result<ReadyOutcome, PeerManagerError> {
        let generation =
            self.generation_for_plan(overlay_ip, plan_connection_id, now_unix_seconds)?;
        self.mark_ready(
            overlay_ip,
            generation,
            authenticated_fingerprint,
            direction,
            now,
        )
    }

    /// Activates or arbitrates an authenticated connection for one probe.
    ///
    /// The first authenticated connection is always usable, regardless of its
    /// direction. Direction is considered only when a duplicate connection for
    /// the same generation arrives.
    pub fn mark_ready(
        &self,
        overlay_ip: Ipv4Addr,
        generation: ProbeGeneration,
        authenticated_fingerprint: &CertificateFingerprint,
        direction: ConnectionDirection,
        now: Instant,
    ) -> Result<ReadyOutcome, PeerManagerError> {
        let mut state = self.lock_state();
        let (path, peer_node_id, expected_fingerprint) = state
            .peers
            .get(&overlay_ip)
            .map(|entry| {
                (
                    entry.path,
                    entry.node_id.clone(),
                    entry.certificate_fingerprint.clone(),
                )
            })
            .ok_or(PeerManagerError::PeerNotFound(overlay_ip))?;

        match path {
            PeerPathState::Probing {
                generation: current_generation,
            }
            | PeerPathState::P2pReady {
                generation: current_generation,
                ..
            } if current_generation == generation => {}
            PeerPathState::RelayOnly
            | PeerPathState::Probing { .. }
            | PeerPathState::P2pReady { .. } => {
                return Err(PeerManagerError::ProbeNoLongerCurrent(generation));
            }
        }
        if authenticated_fingerprint != &expected_fingerprint {
            return Err(PeerManagerError::PeerCertificateMismatch);
        }

        let preferred = match path {
            PeerPathState::P2pReady {
                direction: current_direction,
                ..
            } if current_direction != direction => {
                Some(preferred_direction(&self.local_node_id, &peer_node_id)?)
            }
            PeerPathState::RelayOnly
            | PeerPathState::Probing { .. }
            | PeerPathState::P2pReady { .. } => None,
        };
        let new_connection_id = PeerConnectionId(
            state
                .last_connection_id
                .checked_add(1)
                .ok_or(PeerManagerError::ConnectionIdExhausted)?,
        );
        state.last_connection_id = new_connection_id.get();
        let entry = state
            .peers
            .get_mut(&overlay_ip)
            .expect("peer was checked while holding the manager lock");

        match path {
            PeerPathState::Probing { .. } => {
                entry.path = PeerPathState::P2pReady {
                    generation,
                    connection_id: new_connection_id,
                    direction,
                    last_used: now,
                };
                Ok(ReadyOutcome::Activated {
                    connection_id: new_connection_id,
                })
            }
            PeerPathState::P2pReady {
                connection_id: current_connection_id,
                last_used,
                ..
            } if preferred == Some(direction) => {
                entry.path = PeerPathState::P2pReady {
                    generation,
                    connection_id: new_connection_id,
                    direction,
                    last_used: last_used.max(now),
                };
                Ok(ReadyOutcome::Replaced {
                    connection_id: new_connection_id,
                    connection_to_close: current_connection_id,
                })
            }
            PeerPathState::P2pReady {
                connection_id: current_connection_id,
                ..
            } => Ok(ReadyOutcome::KeptExisting {
                connection_id: current_connection_id,
                connection_to_close: new_connection_id,
            }),
            PeerPathState::RelayOnly => unreachable!("current generation was validated above"),
        }
    }

    /// Returns `true` only when `generation` is the current in-flight probe.
    pub fn probe_failed(&self, overlay_ip: Ipv4Addr, generation: ProbeGeneration) -> bool {
        let mut state = self.lock_state();
        let Some(entry) = state.peers.get_mut(&overlay_ip) else {
            return false;
        };
        if entry.path != (PeerPathState::Probing { generation }) {
            return false;
        }
        entry.path = PeerPathState::RelayOnly;
        entry.probe_plan = None;
        true
    }

    /// Returns `true` only when the event owns the current ready connection.
    pub fn connection_closed(
        &self,
        overlay_ip: Ipv4Addr,
        generation: ProbeGeneration,
        connection_id: PeerConnectionId,
    ) -> bool {
        let mut state = self.lock_state();
        let Some(entry) = state.peers.get_mut(&overlay_ip) else {
            return false;
        };
        match entry.path {
            PeerPathState::P2pReady {
                generation: current_generation,
                connection_id: current_connection,
                ..
            } if current_generation == generation && current_connection == connection_id => {
                entry.path = PeerPathState::RelayOnly;
                entry.probe_plan = None;
                true
            }
            PeerPathState::RelayOnly
            | PeerPathState::Probing { .. }
            | PeerPathState::P2pReady { .. } => false,
        }
    }

    /// Returns whether an actor still owns the current authenticated P2P path.
    ///
    /// Connection actors must re-check ownership immediately before processing
    /// a datagram because replacement and invalidation can race with their
    /// close notification.
    pub fn is_current_connection(
        &self,
        overlay_ip: Ipv4Addr,
        generation: ProbeGeneration,
        connection_id: PeerConnectionId,
    ) -> bool {
        let state = self.lock_state();
        matches!(
            state.peers.get(&overlay_ip).map(|entry| entry.path),
            Some(PeerPathState::P2pReady {
                generation: current_generation,
                connection_id: current_connection,
                ..
            }) if current_generation == generation && current_connection == connection_id
        )
    }

    /// Selects exactly one path and refreshes P2P idle activity for data traffic.
    ///
    /// Transport keepalives must not call this method solely to keep a path
    /// alive; only application packet selection resets the idle deadline.
    pub fn select_path(&self, overlay_ip: Ipv4Addr, now: Instant) -> SelectedPath {
        let mut state = self.lock_state();
        let Some(entry) = state.peers.get_mut(&overlay_ip) else {
            return SelectedPath::Relay;
        };
        match entry.path {
            PeerPathState::P2pReady {
                generation,
                connection_id,
                direction,
                last_used,
            } => {
                entry.path = PeerPathState::P2pReady {
                    generation,
                    connection_id,
                    direction,
                    last_used: last_used.max(now),
                };
                SelectedPath::P2p(connection_id)
            }
            PeerPathState::RelayOnly | PeerPathState::Probing { .. } => SelectedPath::Relay,
        }
    }

    /// Expiry-aware path selection for the v2 runtime. The returned close
    /// action must be executed, but the selected path is already Relay, so the
    /// triggering packet is never sent on both paths.
    pub fn select_path_at(
        &self,
        overlay_ip: Ipv4Addr,
        now: Instant,
        now_unix_seconds: u64,
    ) -> PathDecision {
        let mut state = self.lock_state();
        let Some(entry) = state.peers.get_mut(&overlay_ip) else {
            return PathDecision {
                selected: SelectedPath::Relay,
                action: None,
            };
        };
        if entry.certificate_not_after_unix_seconds <= now_unix_seconds {
            let action = match entry.path {
                PeerPathState::P2pReady { connection_id, .. } => Some(ConnectionAction::Close {
                    overlay_ip,
                    connection_id,
                    reason: ConnectionCloseReason::CertificateExpired,
                }),
                PeerPathState::RelayOnly | PeerPathState::Probing { .. } => None,
            };
            entry.path = PeerPathState::RelayOnly;
            entry.probe_plan = None;
            return PathDecision {
                selected: SelectedPath::Relay,
                action,
            };
        }
        match entry.path {
            PeerPathState::P2pReady {
                generation,
                connection_id,
                direction,
                last_used,
            } => {
                entry.path = PeerPathState::P2pReady {
                    generation,
                    connection_id,
                    direction,
                    last_used: last_used.max(now),
                };
                PathDecision {
                    selected: SelectedPath::P2p(connection_id),
                    action: None,
                }
            }
            PeerPathState::RelayOnly | PeerPathState::Probing { .. } => PathDecision {
                selected: SelectedPath::Relay,
                action: None,
            },
        }
    }

    /// Applies certificate, connection-plan, and idle deadlines in one
    /// deterministic pass and returns every transport connection to close.
    pub fn maintenance(&self, now: Instant, now_unix_seconds: u64) -> Vec<ConnectionAction> {
        let mut state = self.lock_state();
        let mut overlay_ips: Vec<_> = state.peers.keys().copied().collect();
        overlay_ips.sort_unstable();
        let mut actions = Vec::new();
        for overlay_ip in overlay_ips {
            let entry = state
                .peers
                .get_mut(&overlay_ip)
                .expect("address came from the peer map");
            if entry.certificate_not_after_unix_seconds <= now_unix_seconds {
                if let PeerPathState::P2pReady { connection_id, .. } = entry.path {
                    actions.push(ConnectionAction::Close {
                        overlay_ip,
                        connection_id,
                        reason: ConnectionCloseReason::CertificateExpired,
                    });
                }
                entry.path = PeerPathState::RelayOnly;
                entry.probe_plan = None;
                continue;
            }

            if entry
                .probe_plan
                .as_ref()
                .is_some_and(|plan| plan.expires_at_unix_seconds <= now_unix_seconds)
            {
                if matches!(entry.path, PeerPathState::Probing { .. }) {
                    entry.path = PeerPathState::RelayOnly;
                }
                entry.probe_plan = None;
            }

            let PeerPathState::P2pReady {
                connection_id,
                last_used,
                ..
            } = entry.path
            else {
                continue;
            };
            if now.saturating_duration_since(last_used) >= self.idle_timeout {
                entry.path = PeerPathState::RelayOnly;
                entry.probe_plan = None;
                actions.push(ConnectionAction::Close {
                    overlay_ip,
                    connection_id,
                    reason: ConnectionCloseReason::Idle,
                });
            }
        }
        actions
    }

    /// Falls idle ready paths back to Relay and returns connections to close.
    pub fn reap_idle(&self, now: Instant) -> Vec<PeerConnectionId> {
        let mut state = self.lock_state();
        let mut connections = Vec::with_capacity(state.peers.len());
        for entry in state.peers.values_mut() {
            let PeerPathState::P2pReady {
                connection_id,
                last_used,
                ..
            } = entry.path
            else {
                continue;
            };
            if now.saturating_duration_since(last_used) >= self.idle_timeout {
                entry.path = PeerPathState::RelayOnly;
                entry.probe_plan = None;
                connections.push(connection_id);
            }
        }
        connections.sort_unstable();
        connections
    }

    /// Removes one active peer and returns its connection, if any, to close.
    ///
    /// The lifetime node/address binding, last accepted freshness tuple, and
    /// any revocation tombstone remain, so a delayed record cannot roll state
    /// backward or install a different static binding.
    pub fn forget_peer(&self, overlay_ip: Ipv4Addr) -> Option<PeerConnectionId> {
        let mut state = self.lock_state();
        let entry = state.peers.remove(&overlay_ip)?;
        if state.by_node.get(&entry.node_id) == Some(&overlay_ip) {
            state.by_node.remove(&entry.node_id);
        }
        match entry.path {
            PeerPathState::P2pReady { connection_id, .. } => Some(connection_id),
            PeerPathState::RelayOnly | PeerPathState::Probing { .. } => None,
        }
    }

    pub fn preferred_direction(
        &self,
        peer_node_id: &str,
    ) -> Result<ConnectionDirection, PeerManagerError> {
        preferred_direction(&self.local_node_id, peer_node_id)
    }

    pub fn snapshot(&self, overlay_ip: Ipv4Addr) -> Option<PeerSnapshot> {
        self.lock_state()
            .peers
            .get(&overlay_ip)
            .map(|entry| entry.snapshot(overlay_ip))
    }

    /// Returns a bounded, address-sorted point-in-time view.
    pub fn snapshots(&self) -> Vec<PeerSnapshot> {
        let state = self.lock_state();
        let mut snapshots: Vec<_> = state
            .peers
            .iter()
            .map(|(overlay_ip, entry)| entry.snapshot(*overlay_ip))
            .collect();
        snapshots.sort_unstable_by_key(|snapshot| snapshot.overlay_ip);
        snapshots
    }

    pub fn len(&self) -> usize {
        self.lock_state().peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ManagerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub fn preferred_direction(
    local_node_id: &str,
    peer_node_id: &str,
) -> Result<ConnectionDirection, PeerManagerError> {
    validate_node_id(local_node_id).map_err(|_| PeerManagerError::InvalidNodeId)?;
    validate_node_id(peer_node_id).map_err(|_| PeerManagerError::InvalidNodeId)?;
    match local_node_id.cmp(peer_node_id) {
        Ordering::Less => Ok(ConnectionDirection::Outbound),
        Ordering::Greater => Ok(ConnectionDirection::Inbound),
        Ordering::Equal => Err(PeerManagerError::LocalPeerBinding),
    }
}

fn parse_session_id(value: &str) -> Result<SessionId, PeerManagerError> {
    let session_id = SessionId::from_str(value).map_err(|_| PeerManagerError::InvalidSessionId)?;
    validate_session_id(session_id)?;
    if session_id.to_string() != value {
        return Err(PeerManagerError::InvalidSessionId);
    }
    Ok(session_id)
}

fn validate_candidates(
    epoch: u64,
    candidates: &[Candidate],
) -> Result<Vec<Candidate>, PeerManagerError> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(PeerManagerError::TooManyCandidates {
            actual: candidates.len(),
            max: MAX_CANDIDATES,
        });
    }
    if epoch == 0 && !candidates.is_empty() {
        return Err(PeerManagerError::CandidatesWithoutEpoch);
    }
    let mut addresses = HashSet::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.kind != CandidateKind::Host {
            return Err(PeerManagerError::UnsupportedCandidateKind(candidate.kind));
        }
        if !is_usable_candidate(candidate.address) {
            return Err(PeerManagerError::InvalidCandidate(candidate.address));
        }
        if !addresses.insert(candidate.address) {
            return Err(PeerManagerError::DuplicateCandidate(candidate.address));
        }
    }
    let mut candidates = candidates.to_vec();
    candidates.sort_unstable_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.address.cmp(&right.address))
    });
    Ok(candidates)
}

fn is_usable_candidate(address: SocketAddr) -> bool {
    let IpAddr::V4(ip) = address.ip() else {
        return false;
    };
    address.port() != 0
        && !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && !ip.is_broadcast()
}

fn ensure_peer_and_plan_current(
    entry: &PeerEntry,
    now_unix_seconds: u64,
) -> Result<(), PeerManagerError> {
    if entry.certificate_not_after_unix_seconds <= now_unix_seconds {
        return Err(PeerManagerError::PeerCertificateExpired);
    }
    if entry
        .probe_plan
        .as_ref()
        .is_some_and(|plan| plan.expires_at_unix_seconds <= now_unix_seconds)
    {
        return Err(PeerManagerError::ConnectPlanExpired);
    }
    Ok(())
}

fn dial_backoff(completed_rounds: u32) -> Duration {
    let exponent = completed_rounds.saturating_sub(1).min(31);
    INITIAL_DIAL_BACKOFF
        .saturating_mul(1_u32 << exponent)
        .min(MAX_DIAL_BACKOFF)
}

fn validate_session_id(session_id: SessionId) -> Result<(), PeerManagerError> {
    let session_id = uuid::Uuid::parse_str(&session_id.to_string())
        .map_err(|_| PeerManagerError::InvalidSessionId)?;
    if session_id.get_version() != Some(uuid::Version::Random) {
        return Err(PeerManagerError::InvalidSessionId);
    }
    Ok(())
}

fn validate_historical_freshness(
    current: &PeerFreshness,
    incoming: &PeerFreshness,
) -> Result<(), PeerManagerError> {
    if incoming.incarnation < current.incarnation {
        return Err(PeerManagerError::StaleIncarnation {
            current: current.incarnation,
            received: incoming.incarnation,
        });
    }
    if incoming.incarnation == current.incarnation {
        if incoming.session_id != current.session_id {
            return Err(PeerManagerError::IncarnationSessionMismatch {
                incarnation: incoming.incarnation,
                expected: current.session_id,
                received: incoming.session_id,
            });
        }
        if incoming.certificate_fingerprint != current.certificate_fingerprint {
            return Err(PeerManagerError::IncarnationFingerprintMismatch {
                incarnation: incoming.incarnation,
            });
        }
        if incoming.certificate_not_after_unix_seconds != current.certificate_not_after_unix_seconds
        {
            return Err(PeerManagerError::IncarnationCertificateExpiryMismatch {
                incarnation: incoming.incarnation,
            });
        }
        if incoming.candidate_epoch < current.candidate_epoch {
            return Err(PeerManagerError::StaleCandidateEpoch {
                current: current.candidate_epoch,
                received: incoming.candidate_epoch,
            });
        }
        if incoming.candidate_epoch == current.candidate_epoch
            && incoming.candidates != current.candidates
        {
            return Err(PeerManagerError::CandidateSnapshotMismatch {
                epoch: incoming.candidate_epoch,
            });
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum PeerManagerError {
    #[error("invalid node ID")]
    InvalidNodeId,
    #[error("peer binding cannot use the local node ID")]
    LocalPeerBinding,
    #[error("{0} is not a usable target address in the overlay")]
    InvalidOverlayAddress(Ipv4Addr),
    #[error("peer manager capacity must be between 1 and {MAX_MANAGED_PEERS}, got {0}")]
    InvalidCapacity(usize),
    #[error(
        "P2P idle timeout must be between {MIN_P2P_IDLE_TIMEOUT:?} and {MAX_P2P_IDLE_TIMEOUT:?}, got {0:?}"
    )]
    InvalidIdleTimeout(Duration),
    #[error("peer manager reached its configured capacity of {0}")]
    CapacityReached(usize),
    #[error("peer binding history reached its capacity of {0}")]
    HistoricalBindingCapacityReached(usize),
    #[error("peer revocation tombstones reached their capacity of {0}")]
    RevocationCapacityReached(usize),
    #[error("peer {0} is not managed")]
    PeerNotFound(Ipv4Addr),
    #[error("peer binding changed from {expected} to {received}")]
    PeerBindingMismatch { expected: String, received: String },
    #[error(
        "node {node_id} is already bound to {existing_overlay_ip}, cannot bind it to {received_overlay_ip}"
    )]
    NodeAlreadyBound {
        node_id: String,
        existing_overlay_ip: Ipv4Addr,
        received_overlay_ip: Ipv4Addr,
    },
    #[error("node incarnation must be non-zero")]
    InvalidIncarnation,
    #[error("session ID must be a UUID v4")]
    InvalidSessionId,
    #[error("certificate fingerprint is invalid")]
    InvalidCertificateFingerprint,
    #[error("request ID must be non-zero")]
    InvalidRequestId,
    #[error("node incarnation {received} is older than current incarnation {current}")]
    StaleIncarnation { current: u64, received: u64 },
    #[error("node incarnation {incarnation} changed control session from {expected} to {received}")]
    IncarnationSessionMismatch {
        incarnation: u64,
        expected: SessionId,
        received: SessionId,
    },
    #[error("node incarnation {incarnation} changed certificate fingerprint")]
    IncarnationFingerprintMismatch { incarnation: u64 },
    #[error("node incarnation {incarnation} changed certificate expiry")]
    IncarnationCertificateExpiryMismatch { incarnation: u64 },
    #[error("candidate epoch {received} is older than current epoch {current}")]
    StaleCandidateEpoch { current: u64, received: u64 },
    #[error("candidate snapshot changed without increasing epoch {epoch}")]
    CandidateSnapshotMismatch { epoch: u64 },
    #[error("certificate expiry timestamp must be non-zero")]
    InvalidCertificateExpiry,
    #[error("peer certificate is expired")]
    PeerCertificateExpired,
    #[error("candidate snapshot contains {actual} candidates, maximum is {max}")]
    TooManyCandidates { actual: usize, max: usize },
    #[error("candidate epoch zero cannot carry candidates")]
    CandidatesWithoutEpoch,
    #[error("only host candidates are supported before NAT traversal")]
    UnsupportedCandidateKind(CandidateKind),
    #[error("candidate address {0} is not a usable IPv4 host endpoint")]
    InvalidCandidate(SocketAddr),
    #[error("candidate address {0} is duplicated")]
    DuplicateCandidate(SocketAddr),
    #[error("peer {0} has no usable host candidates")]
    NoUsableCandidates(Ipv4Addr),
    #[error("revocation epoch must be non-zero")]
    InvalidRevocationEpoch,
    #[error("revocation epoch {received} is not newer than current epoch {current}")]
    StaleRevocationEpoch { current: u64, received: u64 },
    #[error("revocation does not match the peer's historical node/address binding")]
    RevocationBindingMismatch,
    #[error("peer {node_id} is revoked at revision {epoch}")]
    PeerRevoked { node_id: String, epoch: u64 },
    #[error("peer {0} has not announced candidates")]
    CandidatesNotAnnounced(Ipv4Addr),
    #[error("connection plan has expired")]
    ConnectPlanExpired,
    #[error("peer descriptor changed while installing the connection plan")]
    ConnectPlanPeerChanged,
    #[error("peer has no active connection plan")]
    NoConnectionPlan,
    #[error("peer has no active probe")]
    NoActiveProbe,
    #[error("connection plan ID does not match the active probe")]
    ConnectionPlanMismatch,
    #[error("dial failure counter is exhausted")]
    DialFailureCounterExhausted,
    #[error("dial deadline exceeds the monotonic clock range")]
    DeadlineOverflow,
    #[error("peer {0} already has a ready P2P path")]
    P2pAlreadyReady(Ipv4Addr),
    #[error("probe generation counter is exhausted")]
    ProbeGenerationExhausted,
    #[error("connection ID counter is exhausted")]
    ConnectionIdExhausted,
    #[error("probe generation {0:?} is no longer current")]
    ProbeNoLongerCurrent(ProbeGeneration),
    #[error("authenticated peer certificate does not match the directory record")]
    PeerCertificateMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    const PEER_A: Ipv4Addr = Ipv4Addr::new(10, 42, 0, 2);
    const PEER_B: Ipv4Addr = Ipv4Addr::new(10, 42, 0, 3);

    fn overlay() -> Ipv4Net {
        "10.42.0.0/24".parse().expect("overlay")
    }

    fn manager() -> PeerManager {
        PeerManager::with_defaults("edge-a", overlay()).expect("peer manager")
    }

    fn session(last_octet: u8) -> SessionId {
        format!("00000000-0000-4000-8000-{last_octet:012x}")
            .parse()
            .expect("session ID")
    }

    fn fingerprint(fill: char) -> CertificateFingerprint {
        format!("sha256:{}", fill.to_string().repeat(64))
            .parse()
            .expect("certificate fingerprint")
    }

    fn revocation(node_id: &str, overlay_ip: Ipv4Addr, epoch: u64) -> PeerRevoked {
        PeerRevoked {
            node_id: node_id.to_owned(),
            overlay_ip,
            epoch,
        }
    }

    fn host_candidate(last_octet: u8, port: u16, priority: u32) -> Candidate {
        Candidate {
            address: SocketAddr::from(([192, 168, 1, last_octet], port)),
            kind: CandidateKind::Host,
            priority,
        }
    }

    fn descriptor(
        candidates: Vec<Candidate>,
        epoch: u64,
        certificate_expiry: u64,
    ) -> PeerDescriptor {
        PeerDescriptor {
            node_id: "edge-b".to_owned(),
            overlay_ip: PEER_B,
            incarnation: 1,
            session_id: session(1).to_string(),
            certificate_fingerprint: fingerprint('b').to_string(),
            certificate_not_after_unix_seconds: certificate_expiry,
            candidate_epoch: epoch,
            candidates,
        }
    }

    fn connect_plan(
        role: ConnectionRole,
        candidates: Vec<Candidate>,
        certificate_expiry: u64,
        plan_expiry: u64,
    ) -> ConnectPlan {
        ConnectPlan {
            request_id: 1,
            connection_id: session(9).to_string(),
            role,
            peer: descriptor(candidates, 1, certificate_expiry),
            expires_at_unix_seconds: plan_expiry,
        }
    }

    fn indexed_peer(index: usize) -> Ipv4Addr {
        let host = index + 1;
        Ipv4Addr::new(10, 42, (host / 254) as u8, (host % 254 + 1) as u8)
    }

    fn add_peer(manager: &PeerManager, address: Ipv4Addr, node_id: &str, epoch: u64) {
        assert_eq!(
            manager.update_candidates(address, node_id, 1, session(1), fingerprint('b'), epoch),
            Ok(None)
        );
    }

    fn ready_peer(
        manager: &PeerManager,
        address: Ipv4Addr,
        now: Instant,
    ) -> (ProbeGeneration, PeerConnectionId) {
        let generation = manager.begin_probe(address).expect("begin probe");
        let snapshot = manager.snapshot(address).expect("peer snapshot");
        let direction = manager
            .preferred_direction(&snapshot.node_id)
            .expect("preferred direction");
        let outcome = manager
            .mark_ready(
                address,
                generation,
                &snapshot.certificate_fingerprint,
                direction,
                now,
            )
            .expect("mark ready");
        let ReadyOutcome::Activated { connection_id } = outcome else {
            panic!("first ready connection must activate the path");
        };
        (generation, connection_id)
    }

    #[test]
    fn switches_to_one_ready_path_and_falls_back_on_close() {
        let manager = manager();
        let now = Instant::now();
        add_peer(&manager, PEER_B, "edge-b", 1);
        assert_eq!(manager.select_path(PEER_B, now), SelectedPath::Relay);

        let generation = manager.begin_probe(PEER_B).expect("probe");
        assert_eq!(manager.select_path(PEER_B, now), SelectedPath::Relay);
        let outcome = manager
            .mark_ready(
                PEER_B,
                generation,
                &fingerprint('b'),
                ConnectionDirection::Inbound,
                now,
            )
            .expect("ready");
        let ReadyOutcome::Activated { connection_id } = outcome else {
            panic!("first reverse-direction connection must be accepted");
        };
        assert_eq!(
            manager.select_path(PEER_B, now),
            SelectedPath::P2p(connection_id)
        );
        assert!(matches!(
            manager.snapshot(PEER_B).expect("ready snapshot").path,
            PeerPathState::P2pReady {
                direction: ConnectionDirection::Inbound,
                ..
            }
        ));
        assert!(manager.connection_closed(PEER_B, generation, connection_id));
        assert_eq!(manager.select_path(PEER_B, now), SelectedPath::Relay);
    }

    #[test]
    fn stale_probe_and_connection_events_cannot_replace_or_break_current() {
        let manager = manager();
        let now = Instant::now();
        add_peer(&manager, PEER_B, "edge-b", 1);

        let old_probe = manager.begin_probe(PEER_B).expect("old probe");
        let current_probe = manager.begin_probe(PEER_B).expect("current probe");
        assert!(matches!(
            manager.mark_ready(
                PEER_B,
                old_probe,
                &fingerprint('b'),
                ConnectionDirection::Outbound,
                now
            ),
            Err(PeerManagerError::ProbeNoLongerCurrent(generation)) if generation == old_probe
        ));
        assert!(!manager.probe_failed(PEER_B, old_probe));

        let old_outcome = manager
            .mark_ready(
                PEER_B,
                current_probe,
                &fingerprint('b'),
                ConnectionDirection::Outbound,
                now,
            )
            .expect("old connection becomes ready");
        let ReadyOutcome::Activated {
            connection_id: old_connection,
        } = old_outcome
        else {
            panic!("first current connection must activate");
        };
        assert!(manager.connection_closed(PEER_B, current_probe, old_connection));
        let new_probe = manager.begin_probe(PEER_B).expect("new probe");
        let new_outcome = manager
            .mark_ready(
                PEER_B,
                new_probe,
                &fingerprint('b'),
                ConnectionDirection::Outbound,
                now,
            )
            .expect("new ready connection");
        let ReadyOutcome::Activated {
            connection_id: new_connection,
        } = new_outcome
        else {
            panic!("replacement generation must activate");
        };

        assert!(!manager.connection_closed(PEER_B, current_probe, old_connection));
        assert_eq!(
            manager.select_path(PEER_B, now),
            SelectedPath::P2p(new_connection)
        );
    }

    #[test]
    fn zero_incarnation_is_rejected() {
        let manager = manager();
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 0, session(1), fingerprint('b'), 0),
            Err(PeerManagerError::InvalidIncarnation)
        );
        assert!(manager.is_empty());
    }

    #[test]
    fn non_v4_session_id_is_rejected() {
        let manager = manager();
        let invalid_session = uuid::Uuid::nil()
            .to_string()
            .parse::<SessionId>()
            .expect("routing SessionId accepts UUID syntax");
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 1, invalid_session, fingerprint('b'), 1,),
            Err(PeerManagerError::InvalidSessionId)
        );
        assert!(manager.is_empty());
    }

    #[test]
    fn ready_connection_must_match_directory_certificate() {
        let manager = manager();
        let now = Instant::now();
        add_peer(&manager, PEER_B, "edge-b", 1);
        let generation = manager.begin_probe(PEER_B).expect("probe");

        assert_eq!(
            manager.mark_ready(
                PEER_B,
                generation,
                &fingerprint('c'),
                ConnectionDirection::Outbound,
                now,
            ),
            Err(PeerManagerError::PeerCertificateMismatch)
        );
        assert_eq!(
            manager.snapshot(PEER_B).expect("unchanged probe").path,
            PeerPathState::Probing { generation }
        );
        assert!(matches!(
            manager.mark_ready(
                PEER_B,
                generation,
                &fingerprint('b'),
                ConnectionDirection::Outbound,
                now,
            ),
            Ok(ReadyOutcome::Activated { .. })
        ));
    }

    #[test]
    fn revocation_closes_ready_path_and_blocks_delayed_records() {
        let manager = manager();
        let now = Instant::now();
        add_peer(&manager, PEER_B, "edge-b", 1);
        let (generation, connection_id) = ready_peer(&manager, PEER_B, now);
        assert!(manager.is_current_connection(PEER_B, generation, connection_id));

        assert_eq!(
            manager.apply_revocation(revocation("edge-b", PEER_B, 7)),
            Ok(Some(connection_id))
        );
        assert!(!manager.is_current_connection(PEER_B, generation, connection_id));
        assert!(manager.snapshot(PEER_B).is_none());
        assert_eq!(manager.select_path(PEER_B, now), SelectedPath::Relay);
        assert!(!manager.connection_closed(PEER_B, generation, connection_id));

        for incarnation in [1, 2, u64::MAX] {
            assert_eq!(
                manager.update_candidates(
                    PEER_B,
                    "edge-b",
                    incarnation,
                    SessionId::new(),
                    fingerprint('b'),
                    1,
                ),
                Err(PeerManagerError::PeerRevoked {
                    node_id: "edge-b".to_owned(),
                    epoch: 7,
                })
            );
        }
        assert_eq!(manager.forget_peer(PEER_B), None);
        assert_eq!(
            manager.apply_revocation(revocation("edge-b", PEER_B, 7)),
            Err(PeerManagerError::StaleRevocationEpoch {
                current: 7,
                received: 7,
            })
        );
        assert_eq!(
            manager.apply_revocation(revocation("edge-b", PEER_B, 6)),
            Err(PeerManagerError::StaleRevocationEpoch {
                current: 7,
                received: 6,
            })
        );
        assert_eq!(
            manager.apply_revocation(revocation("edge-b", PEER_B, 8)),
            Ok(None)
        );
    }

    #[test]
    fn revocation_preserves_static_binding_and_validates_revision() {
        let manager = manager();
        assert_eq!(
            manager.apply_revocation(revocation("edge-b", PEER_B, 0)),
            Err(PeerManagerError::InvalidRevocationEpoch)
        );
        assert_eq!(
            manager.apply_revocation(revocation("edge-a", PEER_B, 1)),
            Err(PeerManagerError::LocalPeerBinding)
        );
        assert_eq!(
            manager.apply_revocation(revocation("edge-b", PEER_B, 1)),
            Ok(None)
        );
        assert_eq!(
            manager.apply_revocation(revocation("edge-b", PEER_A, 2)),
            Err(PeerManagerError::RevocationBindingMismatch)
        );
        assert_eq!(
            manager.apply_revocation(revocation("edge-c", PEER_B, 1)),
            Err(PeerManagerError::RevocationBindingMismatch)
        );
        assert_eq!(
            manager.update_candidates(PEER_A, "edge-b", 1, SessionId::new(), fingerprint('b'), 1,),
            Err(PeerManagerError::NodeAlreadyBound {
                node_id: "edge-b".to_owned(),
                existing_overlay_ip: PEER_B,
                received_overlay_ip: PEER_A,
            })
        );
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-c", 1, SessionId::new(), fingerprint('c'), 1,),
            Err(PeerManagerError::PeerBindingMismatch {
                expected: "edge-b".to_owned(),
                received: "edge-c".to_owned(),
            })
        );
    }

    #[test]
    fn candidate_epochs_are_monotonic_and_updates_invalidate_probes() {
        let manager = manager();
        add_peer(&manager, PEER_B, "edge-b", 0);
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 1, session(1), fingerprint('b'), 0),
            Ok(None)
        );
        assert!(matches!(
            manager.begin_probe(PEER_B),
            Err(PeerManagerError::CandidatesNotAnnounced(PEER_B))
        ));
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 1, session(1), fingerprint('b'), 4),
            Ok(None)
        );
        let generation = manager.begin_probe(PEER_B).expect("probe");

        assert!(matches!(
            manager.update_candidates(PEER_B, "edge-b", 1, session(1), fingerprint('b'), 3),
            Err(PeerManagerError::StaleCandidateEpoch {
                current: 4,
                received: 3
            })
        ));
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 1, session(1), fingerprint('b'), 4),
            Ok(None)
        );
        assert_eq!(
            manager.snapshot(PEER_B).expect("idempotent snapshot").path,
            PeerPathState::Probing { generation }
        );
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 1, session(1), fingerprint('b'), 5),
            Ok(None)
        );
        assert!(!manager.probe_failed(PEER_B, generation));
        assert_eq!(
            manager.snapshot(PEER_B).expect("snapshot").path,
            PeerPathState::RelayOnly
        );
    }

    #[test]
    fn newer_incarnation_restarts_epoch_and_rejects_late_records() {
        let manager = manager();
        let session_a = session(1);
        let session_b = session(2);
        let unexpected_session = session(3);
        add_peer(&manager, PEER_B, "edge-b", 4);
        let old_probe = manager.begin_probe(PEER_B).expect("old incarnation probe");
        assert_eq!(old_probe.incarnation(), 1);
        assert_eq!(old_probe.session_id(), session_a);

        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 2, session_b, fingerprint('c'), 0),
            Ok(None)
        );
        let snapshot = manager.snapshot(PEER_B).expect("new incarnation snapshot");
        assert_eq!(snapshot.incarnation, 2);
        assert_eq!(snapshot.session_id, session_b);
        assert_eq!(snapshot.certificate_fingerprint, fingerprint('c'));
        assert_eq!(snapshot.candidate_epoch, 0);
        assert_eq!(snapshot.path, PeerPathState::RelayOnly);
        assert!(matches!(
            manager.mark_ready(
                PEER_B,
                old_probe,
                &fingerprint('b'),
                ConnectionDirection::Outbound,
                Instant::now()
            ),
            Err(PeerManagerError::ProbeNoLongerCurrent(generation))
                if generation == old_probe
        ));
        assert!(!manager.probe_failed(PEER_B, old_probe));

        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 1, session_a, fingerprint('b'), 99),
            Err(PeerManagerError::StaleIncarnation {
                current: 2,
                received: 1,
            })
        );
        assert_eq!(
            manager
                .update_candidates(PEER_B, "edge-b", 2, unexpected_session, fingerprint('c'), 1,),
            Err(PeerManagerError::IncarnationSessionMismatch {
                incarnation: 2,
                expected: session_b,
                received: unexpected_session,
            })
        );
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 2, session_b, fingerprint('d'), 1),
            Err(PeerManagerError::IncarnationFingerprintMismatch { incarnation: 2 })
        );
        let snapshot = manager.snapshot(PEER_B).expect("unchanged snapshot");
        assert_eq!(snapshot.incarnation, 2);
        assert_eq!(snapshot.session_id, session_b);
        assert_eq!(snapshot.candidate_epoch, 0);

        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 2, session_b, fingerprint('c'), 1),
            Ok(None)
        );
        let current_probe = manager.begin_probe(PEER_B).expect("new incarnation probe");
        assert_eq!(current_probe.incarnation(), 2);
        assert_eq!(current_probe.session_id(), session_b);
    }

    #[test]
    fn candidate_update_and_new_incarnation_close_ready_paths() {
        let manager = manager();
        let now = Instant::now();
        add_peer(&manager, PEER_B, "edge-b", 1);
        let (ready_probe, connection_id) = ready_peer(&manager, PEER_B, now);
        assert!(manager.is_current_connection(PEER_B, ready_probe, connection_id));

        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 1, session(1), fingerprint('b'), 2),
            Ok(Some(connection_id))
        );
        assert!(!manager.is_current_connection(PEER_B, ready_probe, connection_id));
        assert_eq!(manager.select_path(PEER_B, now), SelectedPath::Relay);
        assert!(!manager.connection_closed(PEER_B, ready_probe, connection_id));

        let replacement_probe = manager.begin_probe(PEER_B).expect("replacement probe");
        let replacement = manager
            .mark_ready(
                PEER_B,
                replacement_probe,
                &fingerprint('b'),
                ConnectionDirection::Outbound,
                now,
            )
            .expect("replacement ready path");
        let ReadyOutcome::Activated {
            connection_id: replacement_connection,
        } = replacement
        else {
            panic!("replacement probe must activate");
        };
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 2, session(2), fingerprint('c'), 0),
            Ok(Some(replacement_connection))
        );
        assert!(!manager.is_current_connection(PEER_B, replacement_probe, replacement_connection,));
        assert_eq!(manager.select_path(PEER_B, now), SelectedPath::Relay);

        assert!(!manager.connection_closed(PEER_B, replacement_probe, replacement_connection));
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 2, session(2), fingerprint('c'), 1),
            Ok(None)
        );
        let next_probe = manager.begin_probe(PEER_B).expect("replacement probe");
        assert_eq!(next_probe.incarnation(), 2);
        assert_eq!(next_probe.session_id(), session(2));
    }

    #[test]
    fn idle_timeout_bounds_are_enforced() {
        for timeout in [
            MIN_P2P_IDLE_TIMEOUT - Duration::from_millis(1),
            MAX_P2P_IDLE_TIMEOUT + Duration::from_millis(1),
        ] {
            assert!(matches!(
                PeerManager::new("edge-a", overlay(), 1, timeout),
                Err(PeerManagerError::InvalidIdleTimeout(received)) if received == timeout
            ));
        }
        for timeout in [MIN_P2P_IDLE_TIMEOUT, MAX_P2P_IDLE_TIMEOUT] {
            assert!(PeerManager::new("edge-a", overlay(), 1, timeout).is_ok());
        }
    }

    #[test]
    fn capacity_and_snapshots_are_bounded() {
        assert!(matches!(
            PeerManager::new("edge-a", overlay(), 0, DEFAULT_P2P_IDLE_TIMEOUT),
            Err(PeerManagerError::InvalidCapacity(0))
        ));
        assert!(matches!(
            PeerManager::new(
                "edge-a",
                overlay(),
                MAX_MANAGED_PEERS + 1,
                DEFAULT_P2P_IDLE_TIMEOUT
            ),
            Err(PeerManagerError::InvalidCapacity(_))
        ));

        let manager = PeerManager::new("edge-a", overlay(), 1, DEFAULT_P2P_IDLE_TIMEOUT)
            .expect("bounded manager");
        add_peer(&manager, PEER_A, "edge-b", 1);
        assert!(matches!(
            manager.update_candidates(PEER_B, "edge-c", 1, session(2), fingerprint('c'), 1),
            Err(PeerManagerError::CapacityReached(1))
        ));
        assert_eq!(manager.len(), 1);
        assert_eq!(manager.snapshots().len(), 1);
        assert_eq!(
            manager.select_path(PEER_B, Instant::now()),
            SelectedPath::Relay
        );
    }

    #[test]
    fn historical_binding_and_revocation_state_are_bounded() {
        let large_overlay = "10.42.0.0/22".parse().expect("large overlay");
        let history = PeerManager::new("local", large_overlay, 1, DEFAULT_P2P_IDLE_TIMEOUT)
            .expect("history manager");
        for index in 0..MAX_MANAGED_PEERS {
            let overlay_ip = indexed_peer(index);
            let node_id = format!("edge-{index}");
            assert_eq!(
                history.update_candidates(
                    overlay_ip,
                    &node_id,
                    1,
                    SessionId::new(),
                    fingerprint('b'),
                    1,
                ),
                Ok(None)
            );
            assert_eq!(history.forget_peer(overlay_ip), None);
        }
        let overflow_index = MAX_MANAGED_PEERS;
        assert_eq!(
            history.update_candidates(
                indexed_peer(overflow_index),
                format!("edge-{overflow_index}"),
                1,
                SessionId::new(),
                fingerprint('b'),
                1,
            ),
            Err(PeerManagerError::HistoricalBindingCapacityReached(
                MAX_MANAGED_PEERS
            ))
        );

        let revocations = PeerManager::new("local", large_overlay, 1, DEFAULT_P2P_IDLE_TIMEOUT)
            .expect("revocation manager");
        for index in 0..MAX_MANAGED_PEERS {
            assert_eq!(
                revocations.apply_revocation(revocation(
                    &format!("edge-{index}"),
                    indexed_peer(index),
                    1,
                )),
                Ok(None)
            );
        }
        assert_eq!(
            revocations.apply_revocation(revocation(
                &format!("edge-{overflow_index}"),
                indexed_peer(overflow_index),
                1,
            )),
            Err(PeerManagerError::RevocationCapacityReached(
                MAX_MANAGED_PEERS
            ))
        );
    }

    #[test]
    fn forget_preserves_both_directions_of_the_historical_binding() {
        let manager = PeerManager::new("edge-a", overlay(), 2, DEFAULT_P2P_IDLE_TIMEOUT)
            .expect("peer manager");
        assert_eq!(
            manager.update_candidates(PEER_A, "edge-b", 3, session(3), fingerprint('b'), 5),
            Ok(None)
        );

        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 1, session(2), fingerprint('b'), 1),
            Err(PeerManagerError::NodeAlreadyBound {
                node_id: "edge-b".to_owned(),
                existing_overlay_ip: PEER_A,
                received_overlay_ip: PEER_B,
            })
        );
        assert_eq!(manager.forget_peer(PEER_A), None);
        assert_eq!(
            manager.update_candidates(PEER_B, "edge-b", 4, session(4), fingerprint('c'), 0),
            Err(PeerManagerError::NodeAlreadyBound {
                node_id: "edge-b".to_owned(),
                existing_overlay_ip: PEER_A,
                received_overlay_ip: PEER_B,
            })
        );
        assert_eq!(
            manager.update_candidates(PEER_A, "edge-c", 1, session(2), fingerprint('c'), 1),
            Err(PeerManagerError::PeerBindingMismatch {
                expected: "edge-b".to_owned(),
                received: "edge-c".to_owned(),
            })
        );
        assert_eq!(
            manager.update_candidates(PEER_A, "edge-b", 2, session(3), fingerprint('b'), 99),
            Err(PeerManagerError::StaleIncarnation {
                current: 3,
                received: 2,
            })
        );
        assert_eq!(
            manager.update_candidates(PEER_A, "edge-b", 3, session(4), fingerprint('b'), 6),
            Err(PeerManagerError::IncarnationSessionMismatch {
                incarnation: 3,
                expected: session(3),
                received: session(4),
            })
        );
        assert_eq!(
            manager.update_candidates(PEER_A, "edge-b", 3, session(3), fingerprint('c'), 6),
            Err(PeerManagerError::IncarnationFingerprintMismatch { incarnation: 3 })
        );
        assert_eq!(
            manager.update_candidates(PEER_A, "edge-b", 3, session(3), fingerprint('b'), 4),
            Err(PeerManagerError::StaleCandidateEpoch {
                current: 5,
                received: 4,
            })
        );
        assert_eq!(
            manager.update_candidates(PEER_A, "edge-b", 4, session(4), fingerprint('c'), 0),
            Ok(None)
        );
        assert!(manager.snapshot(PEER_B).is_none());
        assert_eq!(
            manager.snapshot(PEER_A).expect("rejoined peer"),
            PeerSnapshot {
                overlay_ip: PEER_A,
                node_id: "edge-b".to_owned(),
                incarnation: 4,
                session_id: session(4),
                certificate_fingerprint: fingerprint('c'),
                certificate_not_after_unix_seconds: u64::MAX,
                candidate_epoch: 0,
                candidates: Vec::new(),
                path: PeerPathState::RelayOnly,
                probe_plan: None,
            }
        );
    }

    #[test]
    fn idle_reaping_returns_only_connections_that_must_close() {
        let manager = manager();
        let started = Instant::now();
        add_peer(&manager, PEER_B, "edge-b", 1);
        let (_, connection_id) = ready_peer(&manager, PEER_B, started);

        assert!(
            manager
                .reap_idle(started + DEFAULT_P2P_IDLE_TIMEOUT - Duration::from_millis(1))
                .is_empty()
        );
        let refreshed = started + Duration::from_secs(4 * 60);
        assert_eq!(
            manager.select_path(PEER_B, refreshed),
            SelectedPath::P2p(connection_id)
        );
        assert!(
            manager
                .reap_idle(refreshed + DEFAULT_P2P_IDLE_TIMEOUT - Duration::from_millis(1))
                .is_empty()
        );
        assert_eq!(
            manager.reap_idle(refreshed + DEFAULT_P2P_IDLE_TIMEOUT),
            vec![connection_id]
        );
        assert_eq!(manager.select_path(PEER_B, refreshed), SelectedPath::Relay);
        assert!(
            manager
                .reap_idle(refreshed + DEFAULT_P2P_IDLE_TIMEOUT)
                .is_empty()
        );
    }

    #[test]
    fn node_order_deterministically_arbitrates_simultaneous_connections() {
        assert_eq!(
            preferred_direction("edge-a", "edge-b"),
            Ok(ConnectionDirection::Outbound)
        );
        assert_eq!(
            preferred_direction("edge-b", "edge-a"),
            Ok(ConnectionDirection::Inbound)
        );

        let manager = manager();
        let now = Instant::now();
        add_peer(&manager, PEER_B, "edge-b", 1);
        let generation = manager.begin_probe(PEER_B).expect("probe");
        let first = manager
            .mark_ready(
                PEER_B,
                generation,
                &fingerprint('b'),
                ConnectionDirection::Inbound,
                now,
            )
            .expect("first reverse connection");
        let ReadyOutcome::Activated {
            connection_id: first_connection,
        } = first
        else {
            panic!("first connection must activate regardless of direction");
        };
        assert!(manager.is_current_connection(PEER_B, generation, first_connection));

        let same_direction = manager
            .mark_ready(
                PEER_B,
                generation,
                &fingerprint('b'),
                ConnectionDirection::Inbound,
                now,
            )
            .expect("same-direction duplicate");
        let ReadyOutcome::KeptExisting {
            connection_id,
            connection_to_close: same_direction_to_close,
        } = same_direction
        else {
            panic!("same-direction duplicate must keep the first connection");
        };
        assert_eq!(connection_id, first_connection);
        assert_ne!(same_direction_to_close, first_connection);

        let preferred = manager
            .mark_ready(
                PEER_B,
                generation,
                &fingerprint('b'),
                ConnectionDirection::Outbound,
                now,
            )
            .expect("preferred-direction duplicate");
        let ReadyOutcome::Replaced {
            connection_id: preferred_connection,
            connection_to_close,
        } = preferred
        else {
            panic!("preferred-direction duplicate must replace the reverse connection");
        };
        assert_eq!(connection_to_close, first_connection);
        assert!(!manager.is_current_connection(PEER_B, generation, first_connection));
        assert!(manager.is_current_connection(PEER_B, generation, preferred_connection));

        let reverse_duplicate = manager
            .mark_ready(
                PEER_B,
                generation,
                &fingerprint('b'),
                ConnectionDirection::Inbound,
                now,
            )
            .expect("reverse duplicate after arbitration");
        assert!(matches!(
            reverse_duplicate,
            ReadyOutcome::KeptExisting {
                connection_id,
                connection_to_close,
            } if connection_id == preferred_connection
                && connection_to_close != preferred_connection
        ));
        assert_eq!(
            manager.select_path(PEER_B, now),
            SelectedPath::P2p(preferred_connection)
        );
        assert!(matches!(
            manager.snapshot(PEER_B).expect("arbitrated path").path,
            PeerPathState::P2pReady {
                connection_id,
                direction: ConnectionDirection::Outbound,
                ..
            } if connection_id == preferred_connection
        ));
        assert!(!manager.connection_closed(PEER_B, generation, first_connection));
    }

    #[test]
    fn concurrent_snapshots_are_coherent_and_bounded() {
        const LAST_EPOCH: u64 = 64;
        const READERS: usize = 4;

        let manager = Arc::new(manager());
        add_peer(&manager, PEER_B, "edge-b", 1);
        let (_, connection_id) = ready_peer(&manager, PEER_B, Instant::now());
        let barrier = Arc::new(Barrier::new(READERS + 1));
        let mut readers = Vec::new();
        for _ in 0..READERS {
            let manager = manager.clone();
            let barrier = barrier.clone();
            readers.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..512 {
                    let snapshots = manager.snapshots();
                    assert_eq!(snapshots.len(), 1);
                    let snapshot = &snapshots[0];
                    assert_eq!(snapshot.overlay_ip, PEER_B);
                    assert_eq!(snapshot.node_id, "edge-b");
                    assert_eq!(snapshot.session_id, session(1));
                    assert!((1..=LAST_EPOCH).contains(&snapshot.candidate_epoch));
                    match snapshot.path {
                        PeerPathState::RelayOnly => {}
                        PeerPathState::Probing { generation } => {
                            assert_ne!(generation.get(), 0);
                        }
                        PeerPathState::P2pReady {
                            generation,
                            connection_id,
                            ..
                        } => {
                            assert_ne!(generation.get(), 0);
                            assert_ne!(connection_id.get(), 0);
                        }
                    }
                }
            }));
        }

        barrier.wait();
        for epoch in 2..=LAST_EPOCH {
            let connection_to_close = manager
                .update_candidates(PEER_B, "edge-b", 1, session(1), fingerprint('b'), epoch)
                .expect("new epoch");
            assert_eq!(connection_to_close, (epoch == 2).then_some(connection_id));
            assert_eq!(
                manager.select_path(PEER_B, Instant::now()),
                SelectedPath::Relay
            );
        }
        for reader in readers {
            reader.join().expect("snapshot reader");
        }

        let snapshot = manager.snapshot(PEER_B).expect("final snapshot");
        assert_eq!(snapshot.candidate_epoch, LAST_EPOCH);
        assert_eq!(snapshot.path, PeerPathState::RelayOnly);
    }

    #[test]
    fn structured_updates_retain_canonical_candidates_and_certificate_expiry() {
        let manager = manager();
        let low = host_candidate(3, 4001, 10);
        let high = host_candidate(4, 4002, 20);
        let update = descriptor(vec![low.clone(), high.clone()], 1, 500);
        assert_eq!(manager.update_peer_descriptor(&update), Ok(None));

        let snapshot = manager.snapshot(PEER_B).expect("structured peer");
        assert_eq!(snapshot.certificate_not_after_unix_seconds, 500);
        assert_eq!(snapshot.candidates, vec![high.clone(), low.clone()]);

        let reordered = descriptor(vec![high.clone(), low], 1, 500);
        assert_eq!(manager.update_peer_descriptor(&reordered), Ok(None));
        let mut conflicting = reordered;
        conflicting.candidates[0].priority += 1;
        assert_eq!(
            manager.update_peer_descriptor(&conflicting),
            Err(PeerManagerError::CandidateSnapshotMismatch { epoch: 1 })
        );

        let mut unsupported = descriptor(vec![high], 2, 500);
        unsupported.candidates[0].kind = CandidateKind::ServerReflexive;
        assert_eq!(
            manager.update_peer_descriptor(&unsupported),
            Err(PeerManagerError::UnsupportedCandidateKind(
                CandidateKind::ServerReflexive
            ))
        );
    }

    #[test]
    fn initiator_dials_candidates_by_priority_then_backs_off_by_round() {
        let manager = manager();
        let now = Instant::now();
        let low = host_candidate(3, 4001, 10);
        let high = host_candidate(4, 4002, 20);
        let plan = connect_plan(
            ConnectionRole::Initiator,
            vec![low.clone(), high.clone()],
            500,
            200,
        );
        let started = manager
            .start_connect_plan(&plan, now, 100)
            .expect("start plan");
        assert_eq!(
            started.actions,
            vec![ConnectionAction::Dial {
                overlay_ip: PEER_B,
                generation: started.generation,
                plan_connection_id: session(9),
                role: ConnectionRole::Initiator,
                candidate: high.clone(),
            }]
        );

        assert!(
            manager
                .dial_failed_at(PEER_B, started.generation, now, 100)
                .unwrap()
        );
        assert_eq!(
            manager.next_dial_action(PEER_B, now, 100).unwrap(),
            Some(ConnectionAction::Dial {
                overlay_ip: PEER_B,
                generation: started.generation,
                plan_connection_id: session(9),
                role: ConnectionRole::Initiator,
                candidate: low,
            })
        );
        assert!(
            manager
                .dial_failed_at(PEER_B, started.generation, now, 100)
                .unwrap()
        );
        assert_eq!(
            manager
                .next_dial_action(PEER_B, now + Duration::from_millis(999), 100)
                .unwrap(),
            None
        );
        assert_eq!(
            manager
                .next_dial_action(PEER_B, now + INITIAL_DIAL_BACKOFF, 101)
                .unwrap(),
            Some(ConnectionAction::Dial {
                overlay_ip: PEER_B,
                generation: started.generation,
                plan_connection_id: session(9),
                role: ConnectionRole::Initiator,
                candidate: high,
            })
        );
        let probe = manager
            .snapshot(PEER_B)
            .expect("peer snapshot")
            .probe_plan
            .expect("probe plan");
        assert_eq!(probe.completed_rounds, 1);
    }

    #[test]
    fn replacement_plan_exposes_the_probe_generation_to_cancel() {
        let manager = manager();
        let now = Instant::now();
        let plan = connect_plan(
            ConnectionRole::Initiator,
            vec![host_candidate(3, 4001, 10)],
            500,
            200,
        );
        let first = manager
            .start_connect_plan(&plan, now, 100)
            .expect("first plan");
        let replacement = manager
            .start_connect_plan(&plan, now, 100)
            .expect("replacement plan");
        assert_eq!(replacement.superseded_generation, Some(first.generation));
        assert_ne!(replacement.generation, first.generation);
        assert!(
            !manager
                .dial_failed_at(PEER_B, first.generation, now, 100)
                .unwrap()
        );
    }

    #[test]
    fn responder_plan_authenticates_by_plan_id_and_arbitrates_duplicates() {
        let manager = manager();
        let now = Instant::now();
        let plan = connect_plan(
            ConnectionRole::Responder,
            vec![host_candidate(3, 4001, 10)],
            500,
            200,
        );
        let started = manager
            .start_connect_plan(&plan, now, 100)
            .expect("start responder plan");
        assert_eq!(
            started.actions,
            vec![
                ConnectionAction::AwaitInbound {
                    overlay_ip: PEER_B,
                    generation: started.generation,
                    plan_connection_id: session(9),
                },
                ConnectionAction::Dial {
                    overlay_ip: PEER_B,
                    generation: started.generation,
                    plan_connection_id: session(9),
                    role: ConnectionRole::Responder,
                    candidate: host_candidate(3, 4001, 10),
                },
            ]
        );
        assert_eq!(
            manager.generation_for_plan(PEER_B, session(8), 100),
            Err(PeerManagerError::ConnectionPlanMismatch)
        );

        let first = manager
            .mark_ready_for_plan(
                PEER_B,
                session(9),
                &fingerprint('b'),
                ConnectionDirection::Inbound,
                now,
                100,
            )
            .expect("activate inbound");
        let ReadyOutcome::Activated {
            connection_id: first_connection,
        } = first
        else {
            panic!("first connection should activate");
        };
        let duplicate = manager
            .mark_ready_for_plan(
                PEER_B,
                session(9),
                &fingerprint('b'),
                ConnectionDirection::Outbound,
                now,
                100,
            )
            .expect("arbitrate duplicate");
        assert!(matches!(
            duplicate,
            ReadyOutcome::Replaced {
                connection_to_close,
                ..
            } if connection_to_close == first_connection
        ));
    }

    #[test]
    fn expiry_switches_atomically_to_relay_and_emits_one_close_action() {
        let manager = manager();
        let now = Instant::now();
        let plan = connect_plan(
            ConnectionRole::Responder,
            vec![host_candidate(3, 4001, 10)],
            150,
            140,
        );
        let started = manager
            .start_connect_plan(&plan, now, 100)
            .expect("start plan");
        let ready = manager
            .mark_ready_for_plan(
                PEER_B,
                session(9),
                &fingerprint('b'),
                ConnectionDirection::Inbound,
                now,
                100,
            )
            .expect("ready path");
        let ReadyOutcome::Activated { connection_id } = ready else {
            panic!("ready path should activate");
        };
        assert!(manager.is_current_connection(PEER_B, started.generation, connection_id,));

        assert_eq!(
            manager.select_path_at(PEER_B, now, 150),
            PathDecision {
                selected: SelectedPath::Relay,
                action: Some(ConnectionAction::Close {
                    overlay_ip: PEER_B,
                    connection_id,
                    reason: ConnectionCloseReason::CertificateExpired,
                }),
            }
        );
        assert!(!manager.is_current_connection(PEER_B, started.generation, connection_id,));
        assert_eq!(
            manager.select_path_at(PEER_B, now, 150),
            PathDecision {
                selected: SelectedPath::Relay,
                action: None,
            }
        );
    }

    #[test]
    fn maintenance_emits_idle_close_and_cancels_expired_handshakes() {
        let ready_manager = manager();
        let now = Instant::now();
        let plan = connect_plan(
            ConnectionRole::Responder,
            vec![host_candidate(3, 4001, 10)],
            1_000,
            500,
        );
        let started = ready_manager
            .start_connect_plan(&plan, now, 100)
            .expect("start ready plan");
        let ready = ready_manager
            .mark_ready_for_plan(
                PEER_B,
                session(9),
                &fingerprint('b'),
                ConnectionDirection::Inbound,
                now,
                100,
            )
            .expect("ready path");
        let ReadyOutcome::Activated { connection_id } = ready else {
            panic!("ready path should activate");
        };
        assert!(ready_manager.is_current_connection(PEER_B, started.generation, connection_id,));
        assert_eq!(
            ready_manager.maintenance(now + DEFAULT_P2P_IDLE_TIMEOUT, 101),
            vec![ConnectionAction::Close {
                overlay_ip: PEER_B,
                connection_id,
                reason: ConnectionCloseReason::Idle,
            }]
        );
        assert!(!ready_manager.is_current_connection(PEER_B, started.generation, connection_id,));
        assert_eq!(
            ready_manager.select_path_at(PEER_B, now, 101).selected,
            SelectedPath::Relay
        );

        let probing = manager();
        let expiring_plan = connect_plan(
            ConnectionRole::Initiator,
            vec![host_candidate(3, 4001, 10)],
            1_000,
            110,
        );
        probing
            .start_connect_plan(&expiring_plan, now, 100)
            .expect("start expiring plan");
        assert!(probing.maintenance(now, 110).is_empty());
        let snapshot = probing.snapshot(PEER_B).expect("expired probe peer");
        assert_eq!(snapshot.path, PeerPathState::RelayOnly);
        assert_eq!(snapshot.probe_plan, None);
    }
}
