// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transport-independent coordinator control-session core for protocol v2.
//!
//! The authentication boundary is explicit: callers may register a session
//! only after mTLS and the static node registry have bound all fields in
//! [`AuthenticatedSession`]. This module performs no network I/O. A 64-bit
//! sliding window accepts limited out-of-order lookup IDs while permanently
//! rejecting duplicates and IDs that have fallen behind the window.

use std::{
    collections::{HashMap, VecDeque},
    net::Ipv4Addr,
    str::FromStr,
    sync::Mutex,
};

use ipnet::Ipv4Net;
use time::OffsetDateTime;

use crate::{
    coordination::{
        CertificateFingerprint, DirectoryError, DirectorySnapshot, MAX_DIRECTORY_PEERS,
        PeerDirectory, PeerRegistration,
    },
    identity::VerifiedNodeCertificate,
    protocol::{AnnounceCandidates, ControlMessage, ErrorCode, ErrorMessage, PeerRevoked},
    registry::validate_overlay_address,
    routing::SessionId,
};

pub const DEFAULT_MAX_CONTROL_SESSIONS: usize = 256;
pub const MAX_CONTROL_SESSIONS: usize = MAX_DIRECTORY_PEERS;
pub const DEFAULT_MAX_PENDING_EVENTS: usize = 512;
pub const MIN_PENDING_EVENTS: usize = 2;
pub const MAX_PENDING_EVENTS: usize = 1024;
pub const REQUEST_REPLAY_WINDOW_BITS: u64 = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedSession {
    node_id: String,
    overlay_ip: Ipv4Addr,
    certificate_fingerprint: String,
    session_id: SessionId,
    certificate_not_after: OffsetDateTime,
}

impl AuthenticatedSession {
    pub fn from_verified_certificate(
        certificate: &VerifiedNodeCertificate,
        session_id: SessionId,
    ) -> Self {
        Self {
            node_id: certificate.node_id().to_owned(),
            overlay_ip: certificate.overlay_ip(),
            certificate_fingerprint: certificate.fingerprint().to_owned(),
            session_id,
            certificate_not_after: certificate.not_after(),
        }
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub const fn overlay_ip(&self) -> Ipv4Addr {
        self.overlay_ip
    }

    pub fn certificate_fingerprint(&self) -> &str {
        &self.certificate_fingerprint
    }

    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub const fn certificate_not_after(&self) -> OffsetDateTime {
        self.certificate_not_after
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionLease {
    session_id: SessionId,
    generation: u64,
    certificate_not_after: OffsetDateTime,
}

impl SessionLease {
    #[cfg(test)]
    pub(crate) const fn for_test(
        session_id: SessionId,
        generation: u64,
        certificate_not_after: OffsetDateTime,
    ) -> Self {
        Self {
            session_id,
            generation,
            certificate_not_after,
        }
    }

    pub const fn session_id(self) -> SessionId {
        self.session_id
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }

    pub const fn certificate_not_after(self) -> OffsetDateTime {
        self.certificate_not_after
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RequestReplayWindow {
    high_watermark: u64,
    seen: u64,
}

impl RequestReplayWindow {
    fn accept(&mut self, request_id: u64) -> bool {
        if self.high_watermark == 0 {
            self.high_watermark = request_id;
            self.seen = 1;
            return true;
        }
        if request_id > self.high_watermark {
            let shift = request_id - self.high_watermark;
            self.seen = if shift >= REQUEST_REPLAY_WINDOW_BITS {
                1
            } else {
                (self.seen << shift) | 1
            };
            self.high_watermark = request_id;
            return true;
        }

        let distance = self.high_watermark - request_id;
        if distance >= REQUEST_REPLAY_WINDOW_BITS {
            return false;
        }
        let bit = 1_u64 << distance;
        if self.seen & bit != 0 {
            return false;
        }
        self.seen |= bit;
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveSession {
    node_id: String,
    overlay_ip: Ipv4Addr,
    generation: u64,
    certificate_not_after: OffsetDateTime,
    lookup_replay: RequestReplayWindow,
}

impl ActiveSession {
    fn accept_lookup_request_id(&mut self, request_id: u64) -> bool {
        self.lookup_replay.accept(request_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionReply {
    Accepted,
    Message(ControlMessage),
    Close { message: Option<ControlMessage> },
}

impl SessionReply {
    pub fn message(&self) -> Option<&ControlMessage> {
        match self {
            Self::Accepted => None,
            Self::Message(message) => Some(message),
            Self::Close { message } => message.as_ref(),
        }
    }

    pub const fn should_close(&self) -> bool {
        matches!(self, Self::Close { .. })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionCancellationReason {
    Revoked,
    Replaced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoordinatorEvent {
    BroadcastRevocation(PeerRevoked),
    SendRevocation {
        lease: SessionLease,
        notification: PeerRevoked,
    },
    CancelSession {
        lease: SessionLease,
        reason: SessionCancellationReason,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevocationDispatch {
    pub notification: PeerRevoked,
    pub canceled_session: Option<SessionLease>,
}

#[derive(Debug, Default)]
struct CoordinatorState {
    sessions: HashMap<SessionId, ActiveSession>,
    events: VecDeque<CoordinatorEvent>,
    last_session_generation: u64,
}

#[derive(Debug)]
pub struct Coordinator {
    directory: PeerDirectory,
    max_sessions: usize,
    max_pending_events: usize,
    state: Mutex<CoordinatorState>,
}

impl Coordinator {
    pub fn new(
        overlay: Ipv4Net,
        max_sessions: usize,
        max_pending_events: usize,
    ) -> Result<Self, CoordinatorError> {
        validate_capacities(max_sessions, max_pending_events)?;
        Ok(Self {
            directory: PeerDirectory::new(overlay, max_sessions)?,
            max_sessions,
            max_pending_events,
            state: Mutex::new(CoordinatorState::default()),
        })
    }

    /// Restores durable incarnation and revocation state. Live sessions are
    /// intentionally not part of a snapshot and must authenticate again.
    pub fn from_snapshot(
        overlay: Ipv4Net,
        max_sessions: usize,
        max_pending_events: usize,
        snapshot: DirectorySnapshot,
    ) -> Result<Self, CoordinatorError> {
        validate_capacities(max_sessions, max_pending_events)?;
        Ok(Self {
            directory: PeerDirectory::from_snapshot(overlay, max_sessions, snapshot)?,
            max_sessions,
            max_pending_events,
            state: Mutex::new(CoordinatorState::default()),
        })
    }

    pub fn from_snapshot_json(
        overlay: Ipv4Net,
        max_sessions: usize,
        max_pending_events: usize,
        encoded: &[u8],
    ) -> Result<Self, CoordinatorError> {
        validate_capacities(max_sessions, max_pending_events)?;
        Ok(Self {
            directory: PeerDirectory::from_snapshot_json(overlay, max_sessions, encoded)?,
            max_sessions,
            max_pending_events,
            state: Mutex::new(CoordinatorState::default()),
        })
    }

    pub fn with_default_capacity(overlay: Ipv4Net) -> Self {
        Self {
            directory: PeerDirectory::with_default_capacity(overlay),
            max_sessions: DEFAULT_MAX_CONTROL_SESSIONS,
            max_pending_events: DEFAULT_MAX_PENDING_EVENTS,
            state: Mutex::new(CoordinatorState::default()),
        }
    }

    pub const fn overlay(&self) -> Ipv4Net {
        self.directory.overlay()
    }

    pub const fn max_sessions(&self) -> usize {
        self.max_sessions
    }

    pub const fn max_pending_events(&self) -> usize {
        self.max_pending_events
    }

    pub fn export_snapshot(&self) -> DirectorySnapshot {
        self.directory.export_snapshot()
    }

    pub fn export_snapshot_json(&self) -> Result<Vec<u8>, CoordinatorError> {
        Ok(self.directory.export_snapshot_json()?)
    }

    /// Registers identity fields previously authenticated by mTLS and the
    /// static node registry.
    pub fn register_authenticated(
        &self,
        authenticated: AuthenticatedSession,
    ) -> Result<SessionLease, CoordinatorError> {
        self.register_authenticated_at(authenticated, OffsetDateTime::now_utc())
    }

    pub fn register_authenticated_at(
        &self,
        authenticated: AuthenticatedSession,
        now: OffsetDateTime,
    ) -> Result<SessionLease, CoordinatorError> {
        if !is_v4_session_id(authenticated.session_id) {
            return Err(CoordinatorError::InvalidSessionId);
        }
        if now >= authenticated.certificate_not_after {
            return Err(CoordinatorError::CertificateExpired);
        }
        let fingerprint = CertificateFingerprint::from_str(&authenticated.certificate_fingerprint)?;
        let certificate_not_after_unix_seconds =
            u64::try_from(authenticated.certificate_not_after.unix_timestamp())
                .map_err(|_| CoordinatorError::CertificateTimestampOutOfRange)?;

        let mut state = self.lock_state();
        if state.sessions.contains_key(&authenticated.session_id) {
            return Err(CoordinatorError::DuplicateSessionId);
        }
        let replaced_lease = state.sessions.iter().find_map(|(session_id, session)| {
            (session.node_id == authenticated.node_id
                && session.overlay_ip == authenticated.overlay_ip)
                .then_some(SessionLease {
                    session_id: *session_id,
                    generation: session.generation,
                    certificate_not_after: session.certificate_not_after,
                })
        });
        if state.sessions.len() >= self.max_sessions && replaced_lease.is_none() {
            return Err(CoordinatorError::SessionCapacityReached(self.max_sessions));
        }
        let revocations = self.directory.revocation_notifications();
        let stale_targeted_events = replaced_lease
            .map(|lease| targeted_event_count(&state.events, lease))
            .unwrap_or(0);
        let retained_events = state.events.len() - stale_targeted_events;
        let required_events = revocations.len() + usize::from(replaced_lease.is_some());
        if retained_events.saturating_add(required_events) > self.max_pending_events {
            return Err(CoordinatorError::EventQueueFull(self.max_pending_events));
        }
        let generation = state
            .last_session_generation
            .checked_add(1)
            .ok_or(CoordinatorError::SessionGenerationExhausted)?;

        let replaced_session_id = self.directory.register_replacing(PeerRegistration {
            node_id: authenticated.node_id.clone(),
            overlay_ip: authenticated.overlay_ip,
            certificate_fingerprint: fingerprint,
            certificate_not_after_unix_seconds,
            session_id: authenticated.session_id,
        })?;
        debug_assert_eq!(
            replaced_session_id,
            replaced_lease.map(SessionLease::session_id)
        );
        if let Some(replaced) = replaced_lease {
            state.sessions.remove(&replaced.session_id);
            remove_targeted_events(&mut state.events, replaced);
            state.events.push_back(CoordinatorEvent::CancelSession {
                lease: replaced,
                reason: SessionCancellationReason::Replaced,
            });
        }
        state.last_session_generation = generation;
        state.sessions.insert(
            authenticated.session_id,
            ActiveSession {
                node_id: authenticated.node_id,
                overlay_ip: authenticated.overlay_ip,
                generation,
                certificate_not_after: authenticated.certificate_not_after,
                lookup_replay: RequestReplayWindow::default(),
            },
        );
        let lease = SessionLease {
            session_id: authenticated.session_id,
            generation,
            certificate_not_after: authenticated.certificate_not_after,
        };
        for notification in revocations {
            state.events.push_back(CoordinatorEvent::SendRevocation {
                lease,
                notification,
            });
        }
        Ok(lease)
    }

    /// Removes only the directory entry owned by this exact session.
    pub fn close_session(&self, lease: SessionLease) -> bool {
        let mut state = self.lock_state();
        self.remove_active_lease(&mut state, lease)
    }

    /// Removes an exact lease once its authenticated certificate has expired.
    /// Per-session runtimes should schedule this using
    /// [`SessionLease::certificate_not_after`].
    pub fn expire_session(&self, lease: SessionLease, now: OffsetDateTime) -> bool {
        let mut state = self.lock_state();
        let is_expired = state
            .sessions
            .get(&lease.session_id)
            .is_some_and(|session| {
                session.generation == lease.generation && now >= session.certificate_not_after
            });
        is_expired && self.remove_active_lease(&mut state, lease)
    }

    /// Removes every session whose certificate is expired at `now` and returns
    /// the exact leases the runtime must close. The result is bounded by
    /// [`MAX_CONTROL_SESSIONS`] and sorted by session generation.
    pub fn expire_sessions(&self, now: OffsetDateTime) -> Vec<SessionLease> {
        let mut state = self.lock_state();
        let mut expired: Vec<_> = state
            .sessions
            .iter()
            .filter_map(|(session_id, session)| {
                (now >= session.certificate_not_after).then_some(SessionLease {
                    session_id: *session_id,
                    generation: session.generation,
                    certificate_not_after: session.certificate_not_after,
                })
            })
            .collect();
        expired.sort_unstable_by_key(|lease| lease.generation);
        for lease in &expired {
            let removed = self.remove_active_lease(&mut state, *lease);
            debug_assert!(
                removed,
                "expired lease was collected while holding the state lock"
            );
        }
        expired
    }

    /// Handles one already-decoded client control message.
    ///
    /// Expected client mistakes and replay attempts are returned as protocol
    /// error messages. The `Accepted` result for an announcement deliberately
    /// does not create an acknowledgement queue.
    pub fn handle(&self, lease: SessionLease, message: ControlMessage) -> SessionReply {
        self.handle_at(lease, message, OffsetDateTime::now_utc())
    }

    pub fn handle_at(
        &self,
        lease: SessionLease,
        message: ControlMessage,
        now: OffsetDateTime,
    ) -> SessionReply {
        let request_id = message_request_id(&message);
        let mut state = self.lock_state();
        let certificate_not_after = state
            .sessions
            .get(&lease.session_id)
            .filter(|session| session.generation == lease.generation)
            .map(|session| session.certificate_not_after);
        let Some(certificate_not_after) = certificate_not_after else {
            return close_error_reply(
                request_id,
                ErrorCode::Revoked,
                "control session is not active",
            );
        };
        if now >= certificate_not_after {
            self.remove_active_lease(&mut state, lease);
            return close_error_reply(
                request_id,
                ErrorCode::Revoked,
                "control session certificate expired",
            );
        }

        match message {
            ControlMessage::AnnounceCandidates(announcement) => {
                if ControlMessage::AnnounceCandidates(announcement.clone())
                    .validate()
                    .is_err()
                {
                    return invalid_request_reply(None);
                }
                let overlay_ip = state
                    .sessions
                    .get(&lease.session_id)
                    .expect("active session was checked while holding the state lock")
                    .overlay_ip;
                match self.apply_announcement(lease.session_id, overlay_ip, announcement) {
                    Ok(()) => SessionReply::Accepted,
                    Err(error) => directory_error_reply(None, error),
                }
            }
            ControlMessage::LookupPeer(lookup) => {
                if ControlMessage::LookupPeer(lookup.clone())
                    .validate()
                    .is_err()
                    || validate_overlay_address(self.directory.overlay(), lookup.overlay_ip)
                        .is_err()
                {
                    return invalid_request_reply(nonzero_request_id(lookup.request_id));
                }
                let session = state
                    .sessions
                    .get_mut(&lease.session_id)
                    .expect("active session was checked while holding the state lock");
                if !session.accept_lookup_request_id(lookup.request_id) {
                    return error_reply(
                        Some(lookup.request_id),
                        ErrorCode::ReplayDetected,
                        "lookup request ID was recently used in this session",
                        false,
                    );
                }
                match self.directory.lookup(lookup.overlay_ip, lookup.request_id) {
                    Ok(record) => SessionReply::Message(ControlMessage::PeerRecord(record)),
                    Err(error) => directory_error_reply(Some(lookup.request_id), error),
                }
            }
            ControlMessage::Error(error) => {
                if ControlMessage::Error(error).validate().is_err() {
                    self.remove_active_lease(&mut state, lease);
                    SessionReply::Close { message: None }
                } else {
                    SessionReply::Accepted
                }
            }
            ControlMessage::ConnectRequest(request) => error_reply(
                Some(request.request_id),
                ErrorCode::Internal,
                "connection planning is unavailable in the coordinator state core",
                true,
            ),
            ControlMessage::RenewCertificate(request) => error_reply(
                Some(request.request_id),
                ErrorCode::Internal,
                "certificate renewal is unavailable in the coordinator state core",
                true,
            ),
            ControlMessage::PeerRecord(_)
            | ControlMessage::PeerRevoked(_)
            | ControlMessage::ControlWelcome(_)
            | ControlMessage::ConnectPlan(_)
            | ControlMessage::CertificateIssued(_) => {
                self.remove_active_lease(&mut state, lease);
                close_error_reply(
                    request_id,
                    ErrorCode::ProtocolViolation,
                    "client sent a coordinator-only control message",
                )
            }
        }
    }

    /// Applies an administrative revocation and emits bounded, high-level
    /// actions for the network runtime to dispatch.
    pub fn revoke(
        &self,
        node_id: &str,
        overlay_ip: Ipv4Addr,
        epoch: u64,
    ) -> Result<RevocationDispatch, CoordinatorError> {
        let mut state = self.lock_state();
        let expected_session = state.sessions.iter().find_map(|(session_id, session)| {
            (session.node_id == node_id && session.overlay_ip == overlay_ip).then_some(
                SessionLease {
                    session_id: *session_id,
                    generation: session.generation,
                    certificate_not_after: session.certificate_not_after,
                },
            )
        });
        let stale_targeted_events = expected_session
            .map(|lease| targeted_event_count(&state.events, lease))
            .unwrap_or(0);
        let retained_events = state.events.len() - stale_targeted_events;
        let required_events = 1 + usize::from(expected_session.is_some());
        if retained_events.saturating_add(required_events) > self.max_pending_events {
            return Err(CoordinatorError::EventQueueFull(self.max_pending_events));
        }

        let outcome = self.directory.revoke(node_id, overlay_ip, epoch)?;
        let canceled_session = outcome.revoked_session.and_then(|session_id| {
            state
                .sessions
                .remove(&session_id)
                .map(|session| SessionLease {
                    session_id,
                    generation: session.generation,
                    certificate_not_after: session.certificate_not_after,
                })
        });
        if let Some(lease) = canceled_session {
            remove_targeted_events(&mut state.events, lease);
        }
        state
            .events
            .push_back(CoordinatorEvent::BroadcastRevocation(
                outcome.notification.clone(),
            ));
        if let Some(lease) = canceled_session {
            state.events.push_back(CoordinatorEvent::CancelSession {
                lease,
                reason: SessionCancellationReason::Revoked,
            });
        }

        Ok(RevocationDispatch {
            notification: outcome.notification,
            canceled_session,
        })
    }

    pub fn pop_event(&self) -> Option<CoordinatorEvent> {
        self.lock_state().events.pop_front()
    }

    pub fn active_session_count(&self) -> usize {
        self.lock_state().sessions.len()
    }

    pub fn pending_event_count(&self) -> usize {
        self.lock_state().events.len()
    }

    pub fn is_revoked(&self, node_id: &str) -> bool {
        self.directory.is_revoked(node_id)
    }

    fn apply_announcement(
        &self,
        session_id: SessionId,
        overlay_ip: Ipv4Addr,
        announcement: AnnounceCandidates,
    ) -> Result<(), DirectoryError> {
        self.directory.announce_candidates(
            overlay_ip,
            session_id,
            announcement.epoch,
            announcement.candidates,
        )
    }

    fn remove_active_lease(&self, state: &mut CoordinatorState, lease: SessionLease) -> bool {
        let Some(session) = state
            .sessions
            .get(&lease.session_id)
            .filter(|session| session.generation == lease.generation)
        else {
            return false;
        };
        let overlay_ip = session.overlay_ip;
        state.sessions.remove(&lease.session_id);
        remove_targeted_events(&mut state.events, lease);
        self.directory.remove_session(overlay_ip, lease.session_id)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, CoordinatorState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum CoordinatorError {
    #[error("control session capacity must be between 1 and {MAX_CONTROL_SESSIONS}, got {0}")]
    InvalidSessionCapacity(usize),
    #[error(
        "pending event capacity must be between {MIN_PENDING_EVENTS} and {MAX_PENDING_EVENTS}, got {0}"
    )]
    InvalidEventCapacity(usize),
    #[error("control session ID must be a UUID v4")]
    InvalidSessionId,
    #[error("control session ID is already active")]
    DuplicateSessionId,
    #[error("control session certificate is already expired")]
    CertificateExpired,
    #[error("control session certificate timestamp cannot be represented by protocol v2")]
    CertificateTimestampOutOfRange,
    #[error("control session generation counter is exhausted")]
    SessionGenerationExhausted,
    #[error("coordinator reached its configured capacity of {0} sessions")]
    SessionCapacityReached(usize),
    #[error("coordinator event queue reached its configured capacity of {0}")]
    EventQueueFull(usize),
    #[error(transparent)]
    Directory(#[from] DirectoryError),
}

fn is_v4_session_id(session_id: SessionId) -> bool {
    uuid::Uuid::parse_str(&session_id.to_string())
        .is_ok_and(|value| value.get_version() == Some(uuid::Version::Random))
}

fn validate_capacities(
    max_sessions: usize,
    max_pending_events: usize,
) -> Result<(), CoordinatorError> {
    if max_sessions == 0 || max_sessions > MAX_CONTROL_SESSIONS {
        return Err(CoordinatorError::InvalidSessionCapacity(max_sessions));
    }
    if !(MIN_PENDING_EVENTS..=MAX_PENDING_EVENTS).contains(&max_pending_events) {
        return Err(CoordinatorError::InvalidEventCapacity(max_pending_events));
    }
    Ok(())
}

fn targeted_event_count(events: &VecDeque<CoordinatorEvent>, lease: SessionLease) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                CoordinatorEvent::SendRevocation {
                    lease: target,
                    ..
                } if *target == lease
            )
        })
        .count()
}

fn remove_targeted_events(events: &mut VecDeque<CoordinatorEvent>, lease: SessionLease) {
    events.retain(|event| {
        !matches!(
            event,
            CoordinatorEvent::SendRevocation {
                lease: target,
                ..
            } if *target == lease
        )
    });
}

fn message_request_id(message: &ControlMessage) -> Option<u64> {
    match message {
        ControlMessage::LookupPeer(lookup) => nonzero_request_id(lookup.request_id),
        ControlMessage::PeerRecord(record) => nonzero_request_id(record.request_id),
        ControlMessage::ConnectRequest(request) => nonzero_request_id(request.request_id),
        ControlMessage::ConnectPlan(plan) => nonzero_request_id(plan.request_id),
        ControlMessage::RenewCertificate(request) => nonzero_request_id(request.request_id),
        ControlMessage::CertificateIssued(certificate) => {
            nonzero_request_id(certificate.request_id)
        }
        ControlMessage::Error(error) => error.request_id.and_then(nonzero_request_id),
        _ => None,
    }
}

fn nonzero_request_id(request_id: u64) -> Option<u64> {
    (request_id != 0).then_some(request_id)
}

fn invalid_request_reply(request_id: Option<u64>) -> SessionReply {
    error_reply(
        request_id,
        ErrorCode::InvalidRequest,
        "invalid control request",
        false,
    )
}

fn directory_error_reply(request_id: Option<u64>, error: DirectoryError) -> SessionReply {
    let (code, message, retryable) = match error {
        DirectoryError::PeerUnavailable(_) => (ErrorCode::PeerNotFound, "peer is not online", true),
        DirectoryError::StaleEpoch { .. } => (
            ErrorCode::ReplayDetected,
            "candidate epoch is not newer than the accepted epoch",
            false,
        ),
        DirectoryError::StaleSession
        | DirectoryError::NodeRevoked(_)
        | DirectoryError::OverlayAddressRevoked(_) => (
            ErrorCode::Revoked,
            "control session or peer binding is revoked",
            false,
        ),
        DirectoryError::CapacityReached(_)
        | DirectoryError::IncarnationCapacityReached(_)
        | DirectoryError::RevocationCapacityReached(_) => {
            (ErrorCode::ServerBusy, "coordinator is at capacity", true)
        }
        DirectoryError::InvalidEpoch
        | DirectoryError::InvalidCertificateExpiry
        | DirectoryError::InvalidRequestId
        | DirectoryError::InvalidOverlayAddress(_)
        | DirectoryError::TooManyCandidates { .. }
        | DirectoryError::InvalidCandidate(_)
        | DirectoryError::DuplicateCandidate(_)
        | DirectoryError::UntrustedReflexiveCandidate => {
            (ErrorCode::InvalidRequest, "invalid control request", false)
        }
        DirectoryError::InvalidCapacity(_)
        | DirectoryError::SnapshotTooLarge { .. }
        | DirectoryError::UnsupportedSnapshotVersion(_)
        | DirectoryError::SnapshotOverlayMismatch { .. }
        | DirectoryError::InvalidSnapshot
        | DirectoryError::InvalidNodeId
        | DirectoryError::InvalidCertificateFingerprint
        | DirectoryError::InvalidSessionId
        | DirectoryError::DuplicateSessionId
        | DirectoryError::DuplicateNode(_)
        | DirectoryError::DuplicateOverlayAddress(_)
        | DirectoryError::DuplicateCertificateFingerprint
        | DirectoryError::IncarnationExhausted
        | DirectoryError::HistoricalBindingMismatch
        | DirectoryError::RevocationBindingMismatch => (
            ErrorCode::Internal,
            "coordinator state rejected the operation",
            true,
        ),
    };
    error_reply(request_id, code, message, retryable)
}

fn error_reply(
    request_id: Option<u64>,
    code: ErrorCode,
    message: &'static str,
    retryable: bool,
) -> SessionReply {
    SessionReply::Message(ControlMessage::Error(ErrorMessage {
        request_id,
        code,
        message: message.to_owned(),
        retryable,
    }))
}

fn close_error_reply(
    request_id: Option<u64>,
    code: ErrorCode,
    message: &'static str,
) -> SessionReply {
    SessionReply::Close {
        message: Some(ControlMessage::Error(ErrorMessage {
            request_id,
            code,
            message: message.to_owned(),
            retryable: false,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Candidate, CandidateKind, LookupPeer, PeerRecord};
    use std::net::SocketAddr;

    fn ip(value: &str) -> Ipv4Addr {
        value.parse().expect("valid IPv4 address")
    }

    fn overlay() -> Ipv4Net {
        "10.42.0.0/24".parse().expect("valid overlay")
    }

    fn coordinator(max_sessions: usize, max_events: usize) -> Coordinator {
        Coordinator::new(overlay(), max_sessions, max_events).expect("valid coordinator")
    }

    fn authenticated(
        node_id: &str,
        overlay_ip: &str,
        fingerprint_fill: char,
        session_id: SessionId,
    ) -> AuthenticatedSession {
        authenticated_until(
            node_id,
            overlay_ip,
            fingerprint_fill,
            session_id,
            OffsetDateTime::now_utc() + time::Duration::days(1),
        )
    }

    fn authenticated_until(
        node_id: &str,
        overlay_ip: &str,
        fingerprint_fill: char,
        session_id: SessionId,
        certificate_not_after: OffsetDateTime,
    ) -> AuthenticatedSession {
        AuthenticatedSession {
            node_id: node_id.to_owned(),
            overlay_ip: ip(overlay_ip),
            certificate_fingerprint: format!("sha256:{}", fingerprint_fill.to_string().repeat(64)),
            session_id,
            certificate_not_after,
        }
    }

    fn candidate(address: &str) -> Candidate {
        Candidate {
            address: SocketAddr::from_str(address).expect("valid socket address"),
            kind: CandidateKind::Host,
            priority: 10,
        }
    }

    fn lookup(request_id: u64, overlay_ip: &str) -> ControlMessage {
        ControlMessage::LookupPeer(LookupPeer {
            request_id,
            overlay_ip: ip(overlay_ip),
        })
    }

    fn error(reply: SessionReply) -> ErrorMessage {
        let error = match reply {
            SessionReply::Message(ControlMessage::Error(error))
            | SessionReply::Close {
                message: Some(ControlMessage::Error(error)),
            } => error,
            _ => panic!("expected structured error reply"),
        };
        ControlMessage::Error(error.clone())
            .validate()
            .expect("coordinator errors must be protocol-valid");
        error
    }

    fn record(reply: SessionReply) -> PeerRecord {
        let SessionReply::Message(ControlMessage::PeerRecord(record)) = reply else {
            panic!("expected peer record")
        };
        record
    }

    #[test]
    fn authenticated_sessions_announce_and_lookup_peer_records() {
        let coordinator = coordinator(4, 8);
        let edge_a_id = SessionId::new();
        let edge_b_id = SessionId::new();
        let edge_a = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', edge_a_id))
            .expect("register edge A");
        let edge_b = coordinator
            .register_authenticated(authenticated("edge-b", "10.42.0.3", 'b', edge_b_id))
            .expect("register edge B");

        assert_eq!(
            coordinator.handle(
                edge_a,
                ControlMessage::AnnounceCandidates(AnnounceCandidates {
                    epoch: 1,
                    candidates: vec![candidate("192.0.2.2:7000")],
                })
            ),
            SessionReply::Accepted
        );
        let record = record(coordinator.handle(edge_b, lookup(1, "10.42.0.2")));
        assert_eq!(record.request_id, 1);
        assert_eq!(record.node_id, "edge-a");
        assert_eq!(record.overlay_ip, ip("10.42.0.2"));
        assert_eq!(record.session_id, edge_a_id.to_string());
        assert_eq!(record.incarnation, 1);
        assert_eq!(
            record.certificate_fingerprint,
            format!("sha256:{}", "a".repeat(64))
        );
        assert_eq!(record.epoch, 1);
        assert_eq!(record.candidates, vec![candidate("192.0.2.2:7000")]);
        assert_eq!(coordinator.active_session_count(), 2);
    }

    #[test]
    fn recent_request_ids_reject_replay_but_allow_out_of_order_values() {
        let coordinator = coordinator(2, 4);
        let session_id = SessionId::new();
        let session = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', session_id))
            .expect("register session");

        let missing = error(coordinator.handle(session, lookup(7, "10.42.0.3")));
        assert_eq!(missing.code, ErrorCode::PeerNotFound);
        assert_eq!(missing.request_id, Some(7));
        assert!(missing.retryable);

        let replay = error(coordinator.handle(session, lookup(7, "10.42.0.3")));
        assert_eq!(replay.code, ErrorCode::ReplayDetected);
        assert_eq!(replay.request_id, Some(7));
        assert!(!replay.retryable);

        let out_of_order = error(coordinator.handle(session, lookup(6, "10.42.0.3")));
        assert_eq!(out_of_order.code, ErrorCode::PeerNotFound);
        let replay = error(coordinator.handle(session, lookup(6, "10.42.0.3")));
        assert_eq!(replay.code, ErrorCode::ReplayDetected);

        let zero = error(coordinator.handle(session, lookup(0, "10.42.0.3")));
        assert_eq!(zero.code, ErrorCode::InvalidRequest);
        assert_eq!(zero.request_id, None);

        assert_eq!(
            coordinator.handle(
                session,
                ControlMessage::Error(ErrorMessage {
                    request_id: Some(8),
                    code: ErrorCode::Internal,
                    message: "agent-side failure".to_owned(),
                    retryable: false,
                }),
            ),
            SessionReply::Accepted
        );

        let reflected_record = record(coordinator.handle(session, lookup(8, "10.42.0.2")));
        let fatal_reply = coordinator.handle(session, ControlMessage::PeerRecord(reflected_record));
        assert!(fatal_reply.should_close());
        let forbidden = error(fatal_reply);
        assert_eq!(forbidden.code, ErrorCode::ProtocolViolation);
        assert_eq!(forbidden.request_id, Some(8));
        assert_eq!(coordinator.active_session_count(), 0);
    }

    #[test]
    fn request_replay_window_rejects_duplicates_and_ids_that_fall_behind() {
        let coordinator = coordinator(1, 2);
        let session_id = SessionId::new();
        let session = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', session_id))
            .expect("register session");

        for request_id in 1..=REQUEST_REPLAY_WINDOW_BITS + 1 {
            let reply = error(coordinator.handle(session, lookup(request_id, "10.42.0.3")));
            assert_eq!(reply.code, ErrorCode::PeerNotFound);
        }
        let newest_replay =
            error(coordinator.handle(session, lookup(REQUEST_REPLAY_WINDOW_BITS + 1, "10.42.0.3")));
        assert_eq!(newest_replay.code, ErrorCode::ReplayDetected);

        let too_old = error(coordinator.handle(session, lookup(1, "10.42.0.3")));
        assert_eq!(too_old.code, ErrorCode::ReplayDetected);
    }

    #[test]
    fn candidate_epochs_and_session_ownership_reject_replay() {
        let coordinator = coordinator(2, 4);
        let first_id = SessionId::new();
        let first = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', first_id))
            .expect("register first session");
        let announcement = |epoch| {
            ControlMessage::AnnounceCandidates(AnnounceCandidates {
                epoch,
                candidates: vec![candidate("192.0.2.2:7000")],
            })
        };

        assert_eq!(
            coordinator.handle(first, announcement(2)),
            SessionReply::Accepted
        );
        for epoch in [2, 1] {
            let replay = error(coordinator.handle(first, announcement(epoch)));
            assert_eq!(replay.code, ErrorCode::ReplayDetected);
        }

        let unknown = SessionLease {
            session_id: SessionId::new(),
            generation: 999,
            certificate_not_after: OffsetDateTime::now_utc() + time::Duration::days(1),
        };
        let inactive = error(coordinator.handle(unknown, announcement(3)));
        assert_eq!(inactive.code, ErrorCode::Revoked);
        assert!(coordinator.close_session(first));

        let replacement_id = SessionId::new();
        let replacement = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'b', replacement_id))
            .expect("register replacement");
        assert_eq!(
            coordinator.handle(replacement, announcement(1)),
            SessionReply::Accepted
        );
        let stale = error(coordinator.handle(first, announcement(3)));
        assert_eq!(stale.code, ErrorCode::Revoked);
        let self_record = record(coordinator.handle(replacement, lookup(1, "10.42.0.2")));
        assert_eq!(self_record.incarnation, 2);
        assert_eq!(self_record.session_id, replacement_id.to_string());
        assert_eq!(self_record.epoch, 1);
    }

    #[test]
    fn stale_lease_cannot_close_or_drive_a_reused_session_id() {
        let coordinator = coordinator(1, 2);
        let reused_id = SessionId::new();
        let first = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', reused_id))
            .expect("register first lease");
        assert!(coordinator.close_session(first));

        let replacement = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'b', reused_id))
            .expect("register replacement with the same UUID");
        assert_ne!(first.generation(), replacement.generation());
        assert!(!coordinator.close_session(first));

        let stale_reply = coordinator.handle(first, lookup(1, "10.42.0.2"));
        assert!(stale_reply.should_close());
        assert_eq!(error(stale_reply).code, ErrorCode::Revoked);
        assert_eq!(coordinator.active_session_count(), 1);

        let current = record(coordinator.handle(replacement, lookup(1, "10.42.0.2")));
        assert_eq!(current.session_id, reused_id.to_string());
        assert_eq!(current.incarnation, 2);
    }

    #[test]
    fn invalid_candidate_and_overlay_inputs_return_bounded_errors() {
        let coordinator = coordinator(2, 4);
        let session_id = SessionId::new();
        let session = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', session_id))
            .expect("register session");

        let mut reflexive = candidate("198.51.100.2:7000");
        reflexive.kind = CandidateKind::ServerReflexive;
        let invalid = error(coordinator.handle(
            session,
            ControlMessage::AnnounceCandidates(AnnounceCandidates {
                epoch: 1,
                candidates: vec![reflexive],
            }),
        ));
        assert_eq!(invalid.code, ErrorCode::InvalidRequest);
        assert!(invalid.message.len() <= crate::protocol::MAX_ERROR_MESSAGE_LEN);

        let outside = error(coordinator.handle(session, lookup(1, "10.43.0.2")));
        assert_eq!(outside.code, ErrorCode::InvalidRequest);
        assert_eq!(outside.request_id, Some(1));

        let too_many = error(coordinator.handle(
            session,
            ControlMessage::AnnounceCandidates(AnnounceCandidates {
                epoch: 1,
                candidates: vec![candidate("192.0.2.2:7000"); 17],
            }),
        ));
        assert_eq!(too_many.code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn registration_validates_authenticated_bindings_and_capacity() {
        assert!(matches!(
            Coordinator::new(overlay(), 0, 1),
            Err(CoordinatorError::InvalidSessionCapacity(0))
        ));
        assert!(matches!(
            Coordinator::new(overlay(), 1, 0),
            Err(CoordinatorError::InvalidEventCapacity(0))
        ));
        assert!(matches!(
            Coordinator::new(overlay(), 1, 1),
            Err(CoordinatorError::InvalidEventCapacity(1))
        ));

        let coordinator = coordinator(1, 2);
        let nil_session =
            SessionId::from_str("00000000-0000-0000-0000-000000000000").expect("parse nil session");
        assert!(matches!(
            coordinator.register_authenticated(authenticated(
                "edge-a",
                "10.42.0.2",
                'a',
                nil_session
            )),
            Err(CoordinatorError::InvalidSessionId)
        ));

        let first = SessionId::new();
        let mut malformed = authenticated("edge-a", "10.42.0.2", 'a', first);
        malformed.certificate_fingerprint = "SHA256:not-canonical".to_owned();
        assert!(matches!(
            coordinator.register_authenticated(malformed),
            Err(CoordinatorError::Directory(
                DirectoryError::InvalidCertificateFingerprint
            ))
        ));
        coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', first))
            .expect("register first session");
        assert!(matches!(
            coordinator.register_authenticated(authenticated(
                "edge-b",
                "10.42.0.3",
                'b',
                SessionId::new()
            )),
            Err(CoordinatorError::SessionCapacityReached(1))
        ));
    }

    #[test]
    fn certificate_expiry_is_enforced_at_registration_and_message_boundaries() {
        let coordinator = coordinator(2, 4);
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);
        let expired_id = SessionId::new();
        assert_eq!(
            coordinator.register_authenticated_at(
                authenticated_until("edge-a", "10.42.0.2", 'a', expired_id, now),
                now,
            ),
            Err(CoordinatorError::CertificateExpired)
        );
        assert_eq!(coordinator.active_session_count(), 0);

        let not_after = now + time::Duration::seconds(10);
        let lease = coordinator
            .register_authenticated_at(
                authenticated_until("edge-a", "10.42.0.2", 'a', SessionId::new(), not_after),
                now,
            )
            .expect("unexpired certificate");
        assert_eq!(lease.certificate_not_after(), not_after);
        let before_expiry = record(coordinator.handle_at(
            lease,
            lookup(1, "10.42.0.2"),
            not_after - time::Duration::nanoseconds(1),
        ));
        assert_eq!(before_expiry.incarnation, 1);

        let expired = coordinator.handle_at(lease, lookup(2, "10.42.0.2"), not_after);
        assert!(expired.should_close());
        assert_eq!(error(expired).code, ErrorCode::Revoked);
        assert_eq!(coordinator.active_session_count(), 0);
        assert!(!coordinator.close_session(lease));
    }

    #[test]
    fn expiry_sweep_is_exact_bounded_and_aba_safe() {
        let coordinator = coordinator(3, 4);
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);
        let first_expiry = now + time::Duration::seconds(10);
        let later_expiry = now + time::Duration::seconds(20);
        let first = coordinator
            .register_authenticated_at(
                authenticated_until("edge-a", "10.42.0.2", 'a', SessionId::new(), first_expiry),
                now,
            )
            .expect("first session");
        let later = coordinator
            .register_authenticated_at(
                authenticated_until("edge-b", "10.42.0.3", 'b', SessionId::new(), later_expiry),
                now,
            )
            .expect("later session");
        let second = coordinator
            .register_authenticated_at(
                authenticated_until("edge-c", "10.42.0.4", 'c', SessionId::new(), first_expiry),
                now,
            )
            .expect("second expiring session");

        assert!(
            coordinator
                .expire_sessions(first_expiry - time::Duration::nanoseconds(1))
                .is_empty()
        );
        assert_eq!(
            coordinator.expire_sessions(first_expiry),
            vec![first, second]
        );
        assert_eq!(coordinator.active_session_count(), 1);
        assert!(coordinator.expire_sessions(first_expiry).is_empty());
        assert!(
            coordinator
                .handle_at(first, lookup(1, "10.42.0.3"), first_expiry)
                .should_close()
        );
        assert_eq!(
            error(coordinator.handle_at(later, lookup(1, "10.42.0.2"), first_expiry)).code,
            ErrorCode::PeerNotFound
        );
        assert_eq!(coordinator.expire_sessions(later_expiry), vec![later]);
        assert_eq!(coordinator.active_session_count(), 0);
    }

    #[test]
    fn active_session_replacement_works_at_capacity_and_cancels_old_lease() {
        let coordinator = coordinator(1, 2);
        let first = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', SessionId::new()))
            .expect("first session");
        let replacement = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'b', SessionId::new()))
            .expect("replacement session");

        assert_eq!(coordinator.active_session_count(), 1);
        assert_eq!(
            coordinator.pop_event(),
            Some(CoordinatorEvent::CancelSession {
                lease: first,
                reason: SessionCancellationReason::Replaced,
            })
        );
        assert!(
            coordinator
                .handle(first, lookup(1, "10.42.0.2"))
                .should_close()
        );
        assert_eq!(
            record(coordinator.handle(replacement, lookup(1, "10.42.0.2"))).incarnation,
            2
        );
    }

    #[test]
    fn revocation_emits_broadcast_and_active_session_cancellation() {
        let coordinator = coordinator(3, 4);
        let edge_a_id = SessionId::new();
        let edge_b_id = SessionId::new();
        let edge_a = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', edge_a_id))
            .expect("register edge A");
        let edge_b = coordinator
            .register_authenticated(authenticated("edge-b", "10.42.0.3", 'b', edge_b_id))
            .expect("register edge B");

        let dispatch = coordinator
            .revoke("edge-a", ip("10.42.0.2"), 9)
            .expect("revoke edge A");
        assert_eq!(dispatch.notification.node_id, "edge-a");
        assert_eq!(dispatch.notification.epoch, 9);
        assert_eq!(dispatch.canceled_session, Some(edge_a));
        assert_eq!(coordinator.active_session_count(), 1);
        assert!(coordinator.is_revoked("edge-a"));
        assert_eq!(
            coordinator.pop_event(),
            Some(CoordinatorEvent::BroadcastRevocation(
                dispatch.notification.clone()
            ))
        );
        assert_eq!(
            coordinator.pop_event(),
            Some(CoordinatorEvent::CancelSession {
                lease: edge_a,
                reason: SessionCancellationReason::Revoked,
            })
        );
        assert_eq!(coordinator.pop_event(), None);

        let inactive = error(coordinator.handle(edge_a, lookup(1, "10.42.0.3")));
        assert_eq!(inactive.code, ErrorCode::Revoked);
        let unavailable = error(coordinator.handle(edge_b, lookup(1, "10.42.0.2")));
        assert_eq!(unavailable.code, ErrorCode::PeerNotFound);
        assert!(matches!(
            coordinator.revoke("edge-a", ip("10.42.0.2"), 9),
            Err(CoordinatorError::Directory(
                DirectoryError::StaleEpoch { .. }
            ))
        ));
        assert!(matches!(
            coordinator.register_authenticated(authenticated(
                "edge-a",
                "10.42.0.2",
                'c',
                SessionId::new()
            )),
            Err(CoordinatorError::Directory(DirectoryError::NodeRevoked(_)))
        ));
    }

    #[test]
    fn event_capacity_preflight_keeps_revocation_atomic() {
        let coordinator = coordinator(2, 2);
        coordinator
            .revoke("offline-a", ip("10.42.0.10"), 1)
            .expect("first offline revocation");
        let active_id = SessionId::new();
        let active = coordinator
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', active_id))
            .expect("register active session");

        assert!(matches!(
            coordinator.revoke("edge-a", ip("10.42.0.2"), 1),
            Err(CoordinatorError::EventQueueFull(2))
        ));
        assert!(!coordinator.is_revoked("edge-a"));
        assert_eq!(coordinator.active_session_count(), 1);
        let self_record = record(coordinator.handle(active, lookup(1, "10.42.0.2")));
        assert_eq!(self_record.node_id, "edge-a");
        assert_eq!(coordinator.pending_event_count(), 2);
    }

    #[test]
    fn restored_revocations_are_targeted_to_each_new_session_in_sorted_order() {
        let source = coordinator(4, 8);
        let original = source
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'a', SessionId::new()))
            .expect("original session");
        assert!(source.close_session(original));
        source
            .revoke("revoked-z", ip("10.42.0.20"), 3)
            .expect("first revocation");
        source
            .revoke("revoked-a", ip("10.42.0.10"), 7)
            .expect("second revocation");
        let snapshot = source.export_snapshot();
        let restored_from_value = Coordinator::from_snapshot(overlay(), 4, 8, snapshot.clone())
            .expect("restore typed snapshot");
        assert_eq!(restored_from_value.export_snapshot(), snapshot);
        assert_eq!(restored_from_value.active_session_count(), 0);
        let encoded = source.export_snapshot_json().expect("snapshot JSON");

        let restored = Coordinator::from_snapshot_json(overlay(), 4, 8, &encoded)
            .expect("restored coordinator");
        assert_eq!(restored.active_session_count(), 0);
        assert!(restored.is_revoked("revoked-a"));
        assert_eq!(
            restored.export_snapshot_json().expect("restored JSON"),
            encoded
        );
        let lease = restored
            .register_authenticated(authenticated("edge-a", "10.42.0.2", 'b', SessionId::new()))
            .expect("post-restart session");
        assert_eq!(
            record(restored.handle(lease, lookup(1, "10.42.0.2"))).incarnation,
            2
        );
        assert_eq!(
            restored.pop_event(),
            Some(CoordinatorEvent::SendRevocation {
                lease,
                notification: PeerRevoked {
                    node_id: "revoked-a".to_owned(),
                    overlay_ip: ip("10.42.0.10"),
                    epoch: 7,
                },
            })
        );
        assert_eq!(
            restored.pop_event(),
            Some(CoordinatorEvent::SendRevocation {
                lease,
                notification: PeerRevoked {
                    node_id: "revoked-z".to_owned(),
                    overlay_ip: ip("10.42.0.20"),
                    epoch: 3,
                },
            })
        );
        assert_eq!(restored.pop_event(), None);
    }

    #[test]
    fn revocation_catch_up_capacity_is_preflighted_before_registration() {
        let coordinator = coordinator(2, 2);
        coordinator
            .revoke("revoked-a", ip("10.42.0.10"), 1)
            .expect("first revocation");
        coordinator
            .revoke("revoked-b", ip("10.42.0.11"), 1)
            .expect("second revocation");
        let attempted = authenticated("edge-a", "10.42.0.2", 'a', SessionId::new());
        assert!(matches!(
            coordinator.register_authenticated(attempted.clone()),
            Err(CoordinatorError::EventQueueFull(2))
        ));
        assert_eq!(coordinator.active_session_count(), 0);
        assert!(coordinator.export_snapshot().nodes.is_empty());

        assert!(matches!(
            coordinator.pop_event(),
            Some(CoordinatorEvent::BroadcastRevocation(_))
        ));
        assert!(matches!(
            coordinator.register_authenticated(attempted.clone()),
            Err(CoordinatorError::EventQueueFull(2))
        ));
        assert!(matches!(
            coordinator.pop_event(),
            Some(CoordinatorEvent::BroadcastRevocation(_))
        ));
        let lease = coordinator
            .register_authenticated(attempted)
            .expect("backlog exactly fits empty queue");
        assert_eq!(coordinator.pending_event_count(), 2);
        assert_eq!(coordinator.active_session_count(), 1);
        for expected_node in ["revoked-a", "revoked-b"] {
            assert!(matches!(
                coordinator.pop_event(),
                Some(CoordinatorEvent::SendRevocation {
                    lease: target,
                    notification,
                }) if target == lease && notification.node_id == expected_node
            ));
        }
    }
}
