// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transactional protocol-v2 coordinator persistence.
//!
//! Live sessions are intentionally absent. Every mutation builds and validates
//! a complete next snapshot, persists it with file and directory durability,
//! and only then commits the in-memory copy. An I/O failure poisons the open
//! instance because a failed directory sync can leave disk state uncertain.

use std::{
    collections::HashSet,
    fmt, fs, io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Mutex, MutexGuard},
};

#[cfg(unix)]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
};

use ipnet::Ipv4Net;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    coordination::{
        DIRECTORY_SNAPSHOT_VERSION, DirectoryNodeSnapshot, DirectoryRevocationSnapshot,
        DirectorySnapshot,
    },
    registry::{
        EnrollmentTokenDigest, MAX_STATIC_NODES, StaticNodeRegistry, validate_node_id,
        validate_overlay_address,
    },
};

pub const COORDINATOR_STORE_VERSION: u16 = 2;
pub const MAX_COORDINATOR_STATE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_ENROLLMENT_RESULT_PEM_BYTES: usize = 256 * 1024;
pub const MAX_CONSUMED_TOKENS_PER_NODE: usize = 64;

const SHA256_PREFIX: &str = "sha256:";
const SHA256_BYTES: usize = 32;
const TEMP_FILE_ATTEMPTS: usize = 32;

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct Sha256Fingerprint([u8; SHA256_BYTES]);

impl Sha256Fingerprint {
    pub fn digest(value: &[u8]) -> Self {
        Self(Sha256::digest(value).into())
    }

    pub fn as_bytes(&self) -> &[u8; SHA256_BYTES] {
        &self.0
    }

    pub fn constant_time_eq(&self, other: &Self) -> bool {
        bool::from(self.0.ct_eq(&other.0))
    }

    fn to_hex(&self) -> String {
        let mut encoded = String::with_capacity(SHA256_BYTES * 2);
        for byte in self.0 {
            use fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }
}

impl fmt::Debug for Sha256Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Sha256Fingerprint(")?;
        fmt::Display::fmt(self, formatter)?;
        formatter.write_str(")")
    }
}

impl fmt::Display for Sha256Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{SHA256_PREFIX}{}", self.to_hex())
    }
}

impl FromStr for Sha256Fingerprint {
    type Err = CoordinatorStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value
            .strip_prefix(SHA256_PREFIX)
            .ok_or(CoordinatorStoreError::InvalidFingerprint)?;
        if hex.len() != SHA256_BYTES * 2
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CoordinatorStoreError::InvalidFingerprint);
        }
        let mut result = [0_u8; SHA256_BYTES];
        for (index, byte) in result.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = u8::from_str_radix(&hex[offset..offset + 2], 16)
                .map_err(|_| CoordinatorStoreError::InvalidFingerprint)?;
        }
        Ok(Self(result))
    }
}

impl Serialize for Sha256Fingerprint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Sha256Fingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = <&str>::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedEnrollmentResult {
    pub node_certificate_pem: String,
    pub node_ca_pem: String,
    pub certificate_fingerprint: Sha256Fingerprint,
    pub certificate_not_after_unix: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentReplay {
    pub enrollment_id: String,
    pub request_sha256: Sha256Fingerprint,
    pub token_sha256: EnrollmentTokenDigest,
    pub result: PersistedEnrollmentResult,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinatorNodeState {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    pub incarnation: u64,
    pub revocation_epoch: Option<u64>,
    pub consumed_token_sha256: Vec<EnrollmentTokenDigest>,
    pub authorized_spki_sha256: Option<Sha256Fingerprint>,
    pub enrollment: Option<EnrollmentReplay>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinatorState {
    pub version: u16,
    pub overlay: Ipv4Net,
    pub generation: u64,
    pub nodes: Vec<CoordinatorNodeState>,
}

#[derive(Clone, Debug)]
pub struct EnrollmentCommit {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    pub enrollment_id: String,
    pub request_sha256: Sha256Fingerprint,
    pub token_sha256: EnrollmentTokenDigest,
    pub authorized_spki_sha256: Sha256Fingerprint,
    pub result: PersistedEnrollmentResult,
}

impl EnrollmentCommit {
    pub fn from_material(
        node_id: impl Into<String>,
        overlay_ip: Ipv4Addr,
        enrollment_id: impl Into<String>,
        request_bytes: &[u8],
        token_sha256: EnrollmentTokenDigest,
        authorized_spki_der: &[u8],
        result: PersistedEnrollmentResult,
    ) -> Self {
        Self {
            node_id: node_id.into(),
            overlay_ip,
            enrollment_id: enrollment_id.into(),
            request_sha256: Sha256Fingerprint::digest(request_bytes),
            token_sha256,
            authorized_spki_sha256: Sha256Fingerprint::digest(authorized_spki_der),
            result,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnrollmentCommitOutcome {
    Committed(PersistedEnrollmentResult),
    Replayed(PersistedEnrollmentResult),
}

#[derive(Debug)]
struct OpenState {
    snapshot: CoordinatorState,
    poisoned: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersistenceStage {
    Write,
    FileSync,
    Rename,
    DirectorySync,
}

#[derive(Debug)]
pub struct CoordinatorStore {
    path: PathBuf,
    max_nodes: usize,
    state: Mutex<OpenState>,
    injected_failure: Mutex<Option<PersistenceStage>>,
}

impl CoordinatorStore {
    /// Creates a version-2 state file. Existing state is never accepted as an
    /// initialization success; normal startup must use [`Self::open`].
    pub fn initialize(
        path: impl AsRef<Path>,
        overlay: Ipv4Net,
        max_nodes: usize,
    ) -> Result<Self, CoordinatorStoreError> {
        Self::initialize_with_failure(path, overlay, max_nodes, None)
    }

    fn initialize_with_failure(
        path: impl AsRef<Path>,
        overlay: Ipv4Net,
        max_nodes: usize,
        injected_failure: Option<PersistenceStage>,
    ) -> Result<Self, CoordinatorStoreError> {
        ensure_supported()?;
        validate_capacity(max_nodes)?;
        let path = path.as_ref().to_path_buf();
        ensure_secure_parent(&path, true)?;
        if path_exists(&path)? {
            return Err(CoordinatorStoreError::AlreadyInitialized);
        }

        let snapshot = CoordinatorState {
            version: COORDINATOR_STORE_VERSION,
            overlay,
            generation: 0,
            nodes: Vec::new(),
        };
        let encoded = encode_and_validate(&snapshot, overlay, max_nodes)?;
        atomic_create(&path, &encoded, injected_failure)?;
        Ok(Self {
            path,
            max_nodes,
            state: Mutex::new(OpenState {
                snapshot,
                poisoned: false,
            }),
            injected_failure: Mutex::new(None),
        })
    }

    /// Opens existing durable state. Missing, v1, oversized, symlinked, or
    /// group/other-readable state fails closed.
    pub fn open(
        path: impl AsRef<Path>,
        overlay: Ipv4Net,
        max_nodes: usize,
    ) -> Result<Self, CoordinatorStoreError> {
        ensure_supported()?;
        validate_capacity(max_nodes)?;
        let path = path.as_ref().to_path_buf();
        ensure_secure_parent(&path, false)?;
        let encoded = read_state_file(&path)?;
        let snapshot: CoordinatorState =
            serde_json::from_slice(&encoded).map_err(|_| CoordinatorStoreError::InvalidJson)?;
        validate_state(&snapshot, overlay, max_nodes)?;
        Ok(Self {
            path,
            max_nodes,
            state: Mutex::new(OpenState {
                snapshot,
                poisoned: false,
            }),
            injected_failure: Mutex::new(None),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn snapshot(&self) -> CoordinatorState {
        self.lock_state().snapshot.clone()
    }

    pub fn is_poisoned(&self) -> bool {
        self.lock_state().poisoned
    }

    pub fn node(&self, node_id: &str) -> Option<CoordinatorNodeState> {
        self.lock_state()
            .snapshot
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .cloned()
    }

    /// Ensures all durable bindings still exist in the startup-loaded static
    /// registry. Disabled nodes remain valid durable history but cannot create
    /// new authenticated sessions.
    pub fn validate_registry(
        &self,
        registry: &StaticNodeRegistry,
    ) -> Result<(), CoordinatorStoreError> {
        let state = self.lock_state();
        if state.snapshot.overlay != registry.overlay() {
            return Err(CoordinatorStoreError::OverlayMismatch);
        }
        for node in &state.snapshot.nodes {
            let binding = registry
                .binding(&node.node_id)
                .ok_or(CoordinatorStoreError::RegistryBindingMismatch)?;
            if binding.overlay_ip != node.overlay_ip {
                return Err(CoordinatorStoreError::RegistryBindingMismatch);
            }
        }
        Ok(())
    }

    /// Checks for an idempotent response before certificate issuance. A
    /// repeated token under another enrollment ID is rejected.
    pub fn lookup_enrollment(
        &self,
        node_id: &str,
        overlay_ip: Ipv4Addr,
        enrollment_id: &str,
        request_sha256: &Sha256Fingerprint,
        token_sha256: &EnrollmentTokenDigest,
    ) -> Result<Option<PersistedEnrollmentResult>, CoordinatorStoreError> {
        let state = self.lock_state();
        ensure_usable(&state)?;
        let Some(node) = state
            .snapshot
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
        else {
            return Ok(None);
        };
        ensure_binding(node, overlay_ip)?;
        if node.revocation_epoch.is_some() {
            return Err(CoordinatorStoreError::NodeRevoked);
        }
        if let Some(replay) = &node.enrollment
            && replay.enrollment_id == enrollment_id
        {
            if replay.request_sha256 == *request_sha256 && replay.token_sha256 == *token_sha256 {
                return Ok(Some(replay.result.clone()));
            }
            return Err(CoordinatorStoreError::EnrollmentConflict);
        }
        if state
            .snapshot
            .nodes
            .iter()
            .any(|candidate| candidate.consumed_token_sha256.contains(token_sha256))
        {
            return Err(CoordinatorStoreError::EnrollmentTokenConsumed);
        }
        Ok(None)
    }

    /// Consumes the token, replaces the authorized SPKI, and persists the
    /// exact response used for idempotent retries in one transaction.
    pub fn commit_enrollment(
        &self,
        commit: EnrollmentCommit,
    ) -> Result<EnrollmentCommitOutcome, CoordinatorStoreError> {
        validate_enrollment_commit(&commit, self.lock_state().snapshot.overlay)?;
        self.mutate(|snapshot| {
            let node_index =
                find_or_insert_node(snapshot, self.max_nodes, &commit.node_id, commit.overlay_ip)?;
            {
                let node = &snapshot.nodes[node_index];
                if node.revocation_epoch.is_some() {
                    return Err(CoordinatorStoreError::NodeRevoked);
                }
                if let Some(replay) = &node.enrollment
                    && replay.enrollment_id == commit.enrollment_id
                {
                    if replay.request_sha256 == commit.request_sha256
                        && replay.token_sha256 == commit.token_sha256
                    {
                        return Ok(Mutation::Unchanged(EnrollmentCommitOutcome::Replayed(
                            replay.result.clone(),
                        )));
                    }
                    return Err(CoordinatorStoreError::EnrollmentConflict);
                }
            }
            if snapshot.nodes.iter().any(|candidate| {
                candidate
                    .consumed_token_sha256
                    .contains(&commit.token_sha256)
            }) {
                return Err(CoordinatorStoreError::EnrollmentTokenConsumed);
            }
            let node = &mut snapshot.nodes[node_index];
            if node.consumed_token_sha256.len() == MAX_CONSUMED_TOKENS_PER_NODE {
                return Err(CoordinatorStoreError::ConsumedTokenHistoryFull);
            }

            node.consumed_token_sha256.push(commit.token_sha256.clone());
            node.authorized_spki_sha256 = Some(commit.authorized_spki_sha256.clone());
            node.enrollment = Some(EnrollmentReplay {
                enrollment_id: commit.enrollment_id,
                request_sha256: commit.request_sha256,
                token_sha256: commit.token_sha256,
                result: commit.result.clone(),
            });
            Ok(Mutation::Changed(EnrollmentCommitOutcome::Committed(
                commit.result,
            )))
        })
    }

    pub fn is_authorized_spki(&self, node_id: &str, spki_der: &[u8]) -> bool {
        let candidate = Sha256Fingerprint::digest(spki_der);
        self.lock_state()
            .snapshot
            .nodes
            .iter()
            .find(|node| node.node_id == node_id && node.revocation_epoch.is_none())
            .and_then(|node| node.authorized_spki_sha256.as_ref())
            .is_some_and(|expected| expected.constant_time_eq(&candidate))
    }

    pub fn advance_incarnation(
        &self,
        node_id: &str,
        overlay_ip: Ipv4Addr,
    ) -> Result<u64, CoordinatorStoreError> {
        validate_node_id(node_id).map_err(|_| CoordinatorStoreError::InvalidNodeId)?;
        self.mutate(|snapshot| {
            validate_overlay_address(snapshot.overlay, overlay_ip)
                .map_err(|_| CoordinatorStoreError::InvalidOverlayAddress(overlay_ip))?;
            let index = find_or_insert_node(snapshot, self.max_nodes, node_id, overlay_ip)?;
            let node = &mut snapshot.nodes[index];
            if node.revocation_epoch.is_some() {
                return Err(CoordinatorStoreError::NodeRevoked);
            }
            if node.authorized_spki_sha256.is_none() {
                return Err(CoordinatorStoreError::NodeNotEnrolled);
            }
            node.incarnation = node
                .incarnation
                .checked_add(1)
                .ok_or(CoordinatorStoreError::CounterExhausted)?;
            Ok(Mutation::Changed(node.incarnation))
        })
    }

    pub fn revoke(
        &self,
        node_id: &str,
        overlay_ip: Ipv4Addr,
        epoch: u64,
    ) -> Result<(), CoordinatorStoreError> {
        validate_node_id(node_id).map_err(|_| CoordinatorStoreError::InvalidNodeId)?;
        if epoch == 0 {
            return Err(CoordinatorStoreError::InvalidRevocationEpoch);
        }
        self.mutate(|snapshot| {
            validate_overlay_address(snapshot.overlay, overlay_ip)
                .map_err(|_| CoordinatorStoreError::InvalidOverlayAddress(overlay_ip))?;
            let index = find_or_insert_node(snapshot, self.max_nodes, node_id, overlay_ip)?;
            let node = &mut snapshot.nodes[index];
            if node
                .revocation_epoch
                .is_some_and(|current| epoch <= current)
            {
                return Err(CoordinatorStoreError::StaleRevocationEpoch);
            }
            node.revocation_epoch = Some(epoch);
            Ok(Mutation::Changed(()))
        })
    }

    /// Produces the existing directory snapshot shape without restoring any
    /// live sessions after restart.
    pub fn directory_snapshot(&self) -> DirectorySnapshot {
        let state = self.lock_state();
        let mut nodes = Vec::new();
        let mut revocations = Vec::new();
        for node in &state.snapshot.nodes {
            if node.incarnation != 0 {
                nodes.push(DirectoryNodeSnapshot {
                    node_id: node.node_id.clone(),
                    overlay_ip: node.overlay_ip,
                    incarnation: node.incarnation,
                });
            }
            if let Some(epoch) = node.revocation_epoch {
                revocations.push(DirectoryRevocationSnapshot {
                    node_id: node.node_id.clone(),
                    overlay_ip: node.overlay_ip,
                    epoch,
                });
            }
        }
        nodes.sort_unstable_by(|left, right| left.node_id.cmp(&right.node_id));
        revocations.sort_unstable_by(|left, right| left.node_id.cmp(&right.node_id));
        DirectorySnapshot {
            version: DIRECTORY_SNAPSHOT_VERSION,
            overlay: state.snapshot.overlay,
            nodes,
            revocations,
        }
    }

    fn mutate<T>(
        &self,
        operation: impl FnOnce(&mut CoordinatorState) -> Result<Mutation<T>, CoordinatorStoreError>,
    ) -> Result<T, CoordinatorStoreError> {
        let mut state = self.lock_state();
        ensure_usable(&state)?;
        let mut next = state.snapshot.clone();
        let result = match operation(&mut next)? {
            Mutation::Changed(result) => result,
            Mutation::Unchanged(result) => return Ok(result),
        };
        next.generation = next
            .generation
            .checked_add(1)
            .ok_or(CoordinatorStoreError::CounterExhausted)?;
        let encoded = encode_and_validate(&next, state.snapshot.overlay, self.max_nodes)?;
        let persistence = atomic_replace(&self.path, &encoded, || self.take_injected_failure());
        if let Err(error) = persistence {
            state.poisoned = true;
            return Err(error);
        }
        state.snapshot = next;
        Ok(result)
    }

    fn lock_state(&self) -> MutexGuard<'_, OpenState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn take_injected_failure(&self) -> Option<PersistenceStage> {
        self.injected_failure
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }

    #[cfg(test)]
    fn inject_failure(&self, stage: PersistenceStage) {
        *self
            .injected_failure
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(stage);
    }
}

enum Mutation<T> {
    Changed(T),
    Unchanged(T),
}

fn find_or_insert_node(
    state: &mut CoordinatorState,
    max_nodes: usize,
    node_id: &str,
    overlay_ip: Ipv4Addr,
) -> Result<usize, CoordinatorStoreError> {
    if let Some(index) = state.nodes.iter().position(|node| node.node_id == node_id) {
        ensure_binding(&state.nodes[index], overlay_ip)?;
        return Ok(index);
    }
    if state.nodes.iter().any(|node| node.overlay_ip == overlay_ip) {
        return Err(CoordinatorStoreError::BindingConflict);
    }
    if state.nodes.len() >= max_nodes {
        return Err(CoordinatorStoreError::CapacityExceeded(max_nodes));
    }
    state.nodes.push(CoordinatorNodeState {
        node_id: node_id.to_owned(),
        overlay_ip,
        incarnation: 0,
        revocation_epoch: None,
        consumed_token_sha256: Vec::new(),
        authorized_spki_sha256: None,
        enrollment: None,
    });
    Ok(state.nodes.len() - 1)
}

fn ensure_binding(
    node: &CoordinatorNodeState,
    overlay_ip: Ipv4Addr,
) -> Result<(), CoordinatorStoreError> {
    if node.overlay_ip != overlay_ip {
        return Err(CoordinatorStoreError::BindingConflict);
    }
    Ok(())
}

fn validate_enrollment_commit(
    commit: &EnrollmentCommit,
    overlay: Ipv4Net,
) -> Result<(), CoordinatorStoreError> {
    validate_node_id(&commit.node_id).map_err(|_| CoordinatorStoreError::InvalidNodeId)?;
    validate_overlay_address(overlay, commit.overlay_ip)
        .map_err(|_| CoordinatorStoreError::InvalidOverlayAddress(commit.overlay_ip))?;
    Uuid::parse_str(&commit.enrollment_id)
        .map_err(|_| CoordinatorStoreError::InvalidEnrollmentId)?;
    validate_result(&commit.result)
}

fn validate_result(result: &PersistedEnrollmentResult) -> Result<(), CoordinatorStoreError> {
    if result.node_certificate_pem.is_empty()
        || result.node_ca_pem.is_empty()
        || result.node_certificate_pem.len() > MAX_ENROLLMENT_RESULT_PEM_BYTES
        || result.node_ca_pem.len() > MAX_ENROLLMENT_RESULT_PEM_BYTES
        || result.certificate_not_after_unix <= 0
    {
        return Err(CoordinatorStoreError::InvalidEnrollmentResult);
    }
    for certificate in [&result.node_certificate_pem, &result.node_ca_pem] {
        let blocks = pem::parse_many(certificate.as_bytes())
            .map_err(|_| CoordinatorStoreError::InvalidEnrollmentResult)?;
        if blocks.len() != 1 || blocks[0].tag() != "CERTIFICATE" {
            return Err(CoordinatorStoreError::InvalidEnrollmentResult);
        }
    }
    Ok(())
}

fn encode_and_validate(
    state: &CoordinatorState,
    overlay: Ipv4Net,
    max_nodes: usize,
) -> Result<Vec<u8>, CoordinatorStoreError> {
    validate_state(state, overlay, max_nodes)?;
    let mut encoded =
        serde_json::to_vec_pretty(state).map_err(|_| CoordinatorStoreError::InvalidJson)?;
    encoded.push(b'\n');
    if encoded.len() as u64 > MAX_COORDINATOR_STATE_BYTES {
        return Err(CoordinatorStoreError::StateTooLarge);
    }
    Ok(encoded)
}

fn validate_state(
    state: &CoordinatorState,
    expected_overlay: Ipv4Net,
    max_nodes: usize,
) -> Result<(), CoordinatorStoreError> {
    if state.version != COORDINATOR_STORE_VERSION {
        return Err(CoordinatorStoreError::UnsupportedVersion(state.version));
    }
    if state.overlay != expected_overlay {
        return Err(CoordinatorStoreError::OverlayMismatch);
    }
    if state.nodes.len() > max_nodes {
        return Err(CoordinatorStoreError::CapacityExceeded(max_nodes));
    }

    let mut node_ids = HashSet::new();
    let mut overlay_ips = HashSet::new();
    let mut consumed_tokens = HashSet::new();
    for node in &state.nodes {
        validate_node_id(&node.node_id).map_err(|_| CoordinatorStoreError::InvalidState)?;
        validate_overlay_address(state.overlay, node.overlay_ip)
            .map_err(|_| CoordinatorStoreError::InvalidState)?;
        if !node_ids.insert(&node.node_id) || !overlay_ips.insert(node.overlay_ip) {
            return Err(CoordinatorStoreError::InvalidState);
        }
        if node.revocation_epoch == Some(0) {
            return Err(CoordinatorStoreError::InvalidState);
        }
        if node.consumed_token_sha256.len() > MAX_CONSUMED_TOKENS_PER_NODE
            || node
                .consumed_token_sha256
                .iter()
                .any(|digest| !consumed_tokens.insert(digest))
        {
            return Err(CoordinatorStoreError::InvalidState);
        }
        if let Some(enrollment) = &node.enrollment {
            Uuid::parse_str(&enrollment.enrollment_id)
                .map_err(|_| CoordinatorStoreError::InvalidState)?;
            if !node
                .consumed_token_sha256
                .contains(&enrollment.token_sha256)
                || node.authorized_spki_sha256.is_none()
            {
                return Err(CoordinatorStoreError::InvalidState);
            }
            validate_result(&enrollment.result)?;
        } else if !node.consumed_token_sha256.is_empty() || node.authorized_spki_sha256.is_some() {
            return Err(CoordinatorStoreError::InvalidState);
        }
    }
    Ok(())
}

fn validate_capacity(max_nodes: usize) -> Result<(), CoordinatorStoreError> {
    if max_nodes == 0 || max_nodes > MAX_STATIC_NODES {
        return Err(CoordinatorStoreError::InvalidCapacity(max_nodes));
    }
    Ok(())
}

fn ensure_usable(state: &OpenState) -> Result<(), CoordinatorStoreError> {
    if state.poisoned {
        Err(CoordinatorStoreError::Poisoned)
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn ensure_secure_parent(path: &Path, create: bool) -> Result<(), CoordinatorStoreError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| CoordinatorStoreError::InsecurePath(path.to_path_buf()))?;
    if create && !parent.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|source| CoordinatorStoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
    }
    let metadata = fs::symlink_metadata(parent).map_err(|source| CoordinatorStoreError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(CoordinatorStoreError::InsecurePermissions(
            parent.to_path_buf(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_secure_parent(_path: &Path, _create: bool) -> Result<(), CoordinatorStoreError> {
    Err(CoordinatorStoreError::UnsupportedPlatform)
}

#[cfg(unix)]
fn path_exists(path: &Path) -> Result<bool, CoordinatorStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(CoordinatorStoreError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(not(unix))]
fn path_exists(_path: &Path) -> Result<bool, CoordinatorStoreError> {
    Err(CoordinatorStoreError::UnsupportedPlatform)
}

#[cfg(unix)]
fn read_state_file(path: &Path) -> Result<Vec<u8>, CoordinatorStoreError> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            CoordinatorStoreError::NotInitialized
        } else {
            CoordinatorStoreError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|source| CoordinatorStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(CoordinatorStoreError::InsecurePermissions(
            path.to_path_buf(),
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_COORDINATOR_STATE_BYTES {
        return Err(CoordinatorStoreError::StateTooLarge);
    }
    let mut encoded = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut encoded)
        .map_err(|source| CoordinatorStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(encoded)
}

#[cfg(not(unix))]
fn read_state_file(_path: &Path) -> Result<Vec<u8>, CoordinatorStoreError> {
    Err(CoordinatorStoreError::UnsupportedPlatform)
}

#[cfg(unix)]
fn atomic_create(
    path: &Path,
    contents: &[u8],
    injected_failure: Option<PersistenceStage>,
) -> Result<(), CoordinatorStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| CoordinatorStoreError::InsecurePath(path.to_path_buf()))?;
    let mut random = OsRng;
    for _ in 0..TEMP_FILE_ATTEMPTS {
        let temporary = parent.join(format!(
            ".{}.tmp-{:016x}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("state"),
            random.next_u64()
        ));
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(CoordinatorStoreError::Io {
                    path: temporary,
                    source,
                });
            }
        };
        let mut installed = false;
        let result = (|| {
            fail_at(injected_failure, PersistenceStage::Write)?;
            file.write_all(contents)?;
            fail_at(injected_failure, PersistenceStage::FileSync)?;
            file.sync_all()?;
            drop(file);
            fail_at(injected_failure, PersistenceStage::Rename)?;
            fs::hard_link(&temporary, path)?;
            installed = true;
            fs::remove_file(&temporary)?;
            fail_at(injected_failure, PersistenceStage::DirectorySync)?;
            File::open(parent)?.sync_all()?;
            Ok::<_, io::Error>(())
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temporary);
            if installed {
                let _ = fs::remove_file(path);
                let _ = File::open(parent).and_then(|directory| directory.sync_all());
            }
            if source.kind() == io::ErrorKind::AlreadyExists {
                return Err(CoordinatorStoreError::AlreadyInitialized);
            }
            return Err(CoordinatorStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
        return Ok(());
    }
    Err(CoordinatorStoreError::TemporaryFileExhausted)
}

#[cfg(not(unix))]
fn atomic_create(
    _path: &Path,
    _contents: &[u8],
    _injected_failure: Option<PersistenceStage>,
) -> Result<(), CoordinatorStoreError> {
    Err(CoordinatorStoreError::UnsupportedPlatform)
}

#[cfg(unix)]
fn atomic_replace(
    path: &Path,
    contents: &[u8],
    injected_failure: impl Fn() -> Option<PersistenceStage>,
) -> Result<(), CoordinatorStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| CoordinatorStoreError::InsecurePath(path.to_path_buf()))?;
    let failure = injected_failure();
    let mut random = OsRng;
    for _ in 0..TEMP_FILE_ATTEMPTS {
        let temporary = parent.join(format!(
            ".{}.tmp-{:016x}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("state"),
            random.next_u64()
        ));
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(CoordinatorStoreError::Io {
                    path: temporary,
                    source,
                });
            }
        };
        let result = (|| {
            fail_at(failure, PersistenceStage::Write)?;
            file.write_all(contents)?;
            fail_at(failure, PersistenceStage::FileSync)?;
            file.sync_all()?;
            fail_at(failure, PersistenceStage::Rename)?;
            fs::rename(&temporary, path)?;
            fail_at(failure, PersistenceStage::DirectorySync)?;
            File::open(parent)?.sync_all()?;
            Ok::<_, io::Error>(())
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temporary);
            return Err(CoordinatorStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
        return Ok(());
    }
    Err(CoordinatorStoreError::TemporaryFileExhausted)
}

#[cfg(not(unix))]
fn atomic_replace(
    _path: &Path,
    _contents: &[u8],
    _injected_failure: impl Fn() -> Option<PersistenceStage>,
) -> Result<(), CoordinatorStoreError> {
    Err(CoordinatorStoreError::UnsupportedPlatform)
}

fn fail_at(injected: Option<PersistenceStage>, current: PersistenceStage) -> Result<(), io::Error> {
    if injected == Some(current) {
        Err(io::Error::other(format!(
            "injected persistence failure at {current:?}"
        )))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn ensure_supported() -> Result<(), CoordinatorStoreError> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_supported() -> Result<(), CoordinatorStoreError> {
    Err(CoordinatorStoreError::UnsupportedPlatform)
}

#[derive(Debug, thiserror::Error)]
pub enum CoordinatorStoreError {
    #[error("coordinator persistence is not supported on this platform")]
    UnsupportedPlatform,
    #[error("coordinator state version {0} is unsupported; expected version 2")]
    UnsupportedVersion(u16),
    #[error("coordinator state is not initialized")]
    NotInitialized,
    #[error("coordinator state is already initialized")]
    AlreadyInitialized,
    #[error("coordinator store instance is poisoned; reopen it before another mutation")]
    Poisoned,
    #[error("coordinator state contains invalid JSON")]
    InvalidJson,
    #[error("coordinator state is invalid")]
    InvalidState,
    #[error("coordinator state is empty or exceeds its size limit")]
    StateTooLarge,
    #[error("configured overlay does not match persisted state")]
    OverlayMismatch,
    #[error("coordinator capacity {0} must be between 1 and 256")]
    InvalidCapacity(usize),
    #[error("coordinator state reached its capacity of {0} nodes")]
    CapacityExceeded(usize),
    #[error("node ID is invalid")]
    InvalidNodeId,
    #[error("{0} is not a usable overlay address")]
    InvalidOverlayAddress(Ipv4Addr),
    #[error("node/address binding conflicts with durable state")]
    BindingConflict,
    #[error("durable node/address binding does not match the static registry")]
    RegistryBindingMismatch,
    #[error("SHA-256 fingerprint must contain exactly 64 lowercase hex characters")]
    InvalidFingerprint,
    #[error("enrollment ID is invalid")]
    InvalidEnrollmentId,
    #[error("enrollment retry conflicts with the persisted request")]
    EnrollmentConflict,
    #[error("enrollment token was already consumed")]
    EnrollmentTokenConsumed,
    #[error("node reached its bounded history of 64 consumed enrollment tokens")]
    ConsumedTokenHistoryFull,
    #[error("enrollment result is invalid or exceeds its bounds")]
    InvalidEnrollmentResult,
    #[error("node is not enrolled")]
    NodeNotEnrolled,
    #[error("node is revoked")]
    NodeRevoked,
    #[error("revocation epoch must be non-zero")]
    InvalidRevocationEpoch,
    #[error("revocation epoch is not newer than durable state")]
    StaleRevocationEpoch,
    #[error("durable counter is exhausted")]
    CounterExhausted,
    #[error("coordinator path is insecure: {0}")]
    InsecurePath(PathBuf),
    #[error("coordinator path has an unexpected owner or unsafe permissions: {0}")]
    InsecurePermissions(PathBuf),
    #[error("could not allocate a unique temporary coordinator state file")]
    TemporaryFileExhausted,
    #[error("I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::registry::{ENROLLMENT_TOKEN_BYTES, EnrollmentToken};
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "stellaris-coordinator-store-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create fixture directory");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("secure fixture directory");
            Self(path)
        }

        fn state_path(&self) -> PathBuf {
            self.0.join("coordinator.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn overlay() -> Ipv4Net {
        "10.88.0.0/24".parse().unwrap()
    }

    fn token_digest(fill: u8) -> EnrollmentTokenDigest {
        EnrollmentTokenDigest::from_token(&EnrollmentToken::from_bytes(
            [fill; ENROLLMENT_TOKEN_BYTES],
        ))
    }

    fn result(fill: u8) -> PersistedEnrollmentResult {
        PersistedEnrollmentResult {
            node_certificate_pem: pem::encode(&pem::Pem::new("CERTIFICATE", vec![fill])),
            node_ca_pem: pem::encode(&pem::Pem::new("CERTIFICATE", vec![9, fill])),
            certificate_fingerprint: Sha256Fingerprint::digest(&[fill]),
            certificate_not_after_unix: 2_000_000_000,
        }
    }

    fn commit(fill: u8) -> EnrollmentCommit {
        EnrollmentCommit::from_material(
            "edge-a",
            "10.88.0.2".parse().unwrap(),
            Uuid::from_bytes([fill; 16]).to_string(),
            &[fill, 1],
            token_digest(fill),
            &[fill, 2],
            result(fill),
        )
    }

    #[test]
    fn initialization_is_explicit_and_v1_is_rejected() {
        let directory = TestDirectory::new("initialize");
        let path = directory.state_path();
        assert!(matches!(
            CoordinatorStore::open(&path, overlay(), 256),
            Err(CoordinatorStoreError::NotInitialized)
        ));
        let store = CoordinatorStore::initialize(&path, overlay(), 256).expect("initialize");
        assert_eq!(store.snapshot().version, 2);
        assert!(matches!(
            CoordinatorStore::initialize(&path, overlay(), 256),
            Err(CoordinatorStoreError::AlreadyInitialized)
        ));

        let mut state = store.snapshot();
        state.version = 1;
        let encoded = serde_json::to_vec(&state).unwrap();
        atomic_replace(&path, &encoded, || None).unwrap();
        assert!(matches!(
            CoordinatorStore::open(&path, overlay(), 256),
            Err(CoordinatorStoreError::UnsupportedVersion(1))
        ));
    }

    #[test]
    fn enrollment_is_idempotent_and_token_rotation_replaces_spki() {
        let directory = TestDirectory::new("enrollment");
        let store = CoordinatorStore::initialize(directory.state_path(), overlay(), 256).unwrap();
        let first = commit(1);
        assert!(matches!(
            store.commit_enrollment(first.clone()).unwrap(),
            EnrollmentCommitOutcome::Committed(_)
        ));
        assert!(store.is_authorized_spki("edge-a", &[1, 2]));
        assert!(matches!(
            store.commit_enrollment(first.clone()).unwrap(),
            EnrollmentCommitOutcome::Replayed(_)
        ));

        let mut conflicting = first.clone();
        conflicting.request_sha256 = Sha256Fingerprint::digest(b"different");
        assert!(matches!(
            store.commit_enrollment(conflicting),
            Err(CoordinatorStoreError::EnrollmentConflict)
        ));
        let mut reused_token = commit(2);
        reused_token.node_id = "edge-b".to_owned();
        reused_token.overlay_ip = "10.88.0.3".parse().unwrap();
        reused_token.token_sha256 = first.token_sha256;
        assert!(matches!(
            store.commit_enrollment(reused_token),
            Err(CoordinatorStoreError::EnrollmentTokenConsumed)
        ));

        store.commit_enrollment(commit(2)).expect("rotate token");
        assert!(!store.is_authorized_spki("edge-a", &[1, 2]));
        assert!(store.is_authorized_spki("edge-a", &[2, 2]));
    }

    #[test]
    fn incarnation_and_revocation_survive_restart_without_sessions() {
        let directory = TestDirectory::new("restart");
        let path = directory.state_path();
        let store = CoordinatorStore::initialize(&path, overlay(), 256).unwrap();
        store.commit_enrollment(commit(1)).unwrap();
        assert_eq!(
            store
                .advance_incarnation("edge-a", "10.88.0.2".parse().unwrap())
                .unwrap(),
            1
        );
        store
            .revoke("edge-a", "10.88.0.2".parse().unwrap(), 7)
            .unwrap();
        drop(store);

        let restored = CoordinatorStore::open(&path, overlay(), 256).unwrap();
        let directory = restored.directory_snapshot();
        assert_eq!(directory.nodes[0].incarnation, 1);
        assert_eq!(directory.revocations[0].epoch, 7);
        assert!(matches!(
            restored.advance_incarnation("edge-a", "10.88.0.2".parse().unwrap()),
            Err(CoordinatorStoreError::NodeRevoked)
        ));
    }

    #[test]
    fn every_persistence_failure_is_unacknowledged_and_poisons_the_instance() {
        for stage in [
            PersistenceStage::Write,
            PersistenceStage::FileSync,
            PersistenceStage::Rename,
            PersistenceStage::DirectorySync,
        ] {
            let directory = TestDirectory::new("fault");
            let store =
                CoordinatorStore::initialize(directory.state_path(), overlay(), 256).unwrap();
            store.inject_failure(stage);
            assert!(matches!(
                store.commit_enrollment(commit(1)),
                Err(CoordinatorStoreError::Io { .. })
            ));
            assert_eq!(store.snapshot().generation, 0);
            assert!(store.snapshot().nodes.is_empty());
            assert!(store.is_poisoned());
            assert!(matches!(
                store.commit_enrollment(commit(2)),
                Err(CoordinatorStoreError::Poisoned)
            ));
        }
    }

    #[test]
    fn initialization_failures_leave_no_state_and_can_be_retried() {
        for stage in [
            PersistenceStage::Write,
            PersistenceStage::FileSync,
            PersistenceStage::Rename,
            PersistenceStage::DirectorySync,
        ] {
            let directory = TestDirectory::new("initialization-fault");
            let path = directory.state_path();
            assert!(matches!(
                CoordinatorStore::initialize_with_failure(&path, overlay(), 256, Some(stage)),
                Err(CoordinatorStoreError::Io { .. })
            ));
            assert!(!path.exists(), "{stage:?} left a final state file");

            let store = CoordinatorStore::initialize(&path, overlay(), 256)
                .expect("retry initialization after injected failure");
            assert_eq!(store.snapshot().generation, 0);
        }
    }

    #[test]
    fn state_permissions_and_capacity_fail_closed() {
        let directory = TestDirectory::new("bounds");
        let path = directory.state_path();
        drop(CoordinatorStore::initialize(&path, overlay(), 1).unwrap());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            CoordinatorStore::open(&path, overlay(), 1),
            Err(CoordinatorStoreError::InsecurePermissions(_))
        ));
        assert!(matches!(
            CoordinatorStore::initialize(directory.0.join("other.json"), overlay(), 257),
            Err(CoordinatorStoreError::InvalidCapacity(257))
        ));
    }
}
