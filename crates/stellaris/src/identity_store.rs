// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Unix persistence for protocol-v2 agent identity material.
//!
//! The node certificate and node CA are stored in one atomically replaced JSON
//! document. This avoids exposing a mixed old/new pair during renewal. The
//! private key and pending enrollment request are always owner-only files.

use std::{
    fmt, fs, io,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

#[cfg(unix)]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use uuid::Uuid;
use x509_parser::{
    certification_request::X509CertificationRequest, extensions::GeneralName,
    parse_x509_certificate, prelude::FromDer,
};

use crate::{
    identity::{NODE_ID_URI_PREFIX, NodeKey},
    registry::{EnrollmentToken, validate_node_id},
};

pub const NODE_IDENTITY_STORE_VERSION: u16 = 2;
pub const NODE_KEY_FILE: &str = "node-key.pem";
pub const NODE_CREDENTIALS_FILE: &str = "credentials.json";
pub const ENROLLMENT_REQUEST_FILE: &str = "enrollment-request.json";

const MAX_CREDENTIALS_BYTES: u64 = 512 * 1024;
const MAX_ENROLLMENT_REQUEST_BYTES: u64 = 128 * 1024;
const MAX_CERTIFICATE_PEM_BYTES: usize = 256 * 1024;
const MAX_CSR_BYTES: usize = 64 * 1024;
const MAX_NODE_IDENTITY_FILE_BYTES: usize = 512 * 1024;
const TEMP_FILE_ATTEMPTS: usize = 32;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PendingEnrollmentRequest {
    pub version: u16,
    pub enrollment_id: String,
    pub node_id: String,
    enrollment_token: String,
    csr_der_base64: String,
}

impl PendingEnrollmentRequest {
    pub fn enrollment_token(&self) -> &str {
        &self.enrollment_token
    }

    pub fn csr_der(&self) -> Result<Vec<u8>, NodeIdentityStoreError> {
        decode_csr(&self.csr_der_base64)
    }
}

impl fmt::Debug for PendingEnrollmentRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingEnrollmentRequest")
            .field("version", &self.version)
            .field("enrollment_id", &self.enrollment_id)
            .field("node_id", &self.node_id)
            .field("enrollment_token", &"[REDACTED]")
            .field("csr_der_base64", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledNodeIdentity {
    pub version: u16,
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    node_ca_pem: String,
    node_certificate_pem: String,
    not_before_unix: i64,
    not_after_unix: i64,
    spki_sha256: String,
}

impl InstalledNodeIdentity {
    pub fn node_ca_pem(&self) -> &str {
        &self.node_ca_pem
    }

    pub fn node_certificate_pem(&self) -> &str {
        &self.node_certificate_pem
    }

    pub fn spki_sha256(&self) -> &str {
        &self.spki_sha256
    }

    pub fn not_before(&self) -> Result<OffsetDateTime, NodeIdentityStoreError> {
        OffsetDateTime::from_unix_timestamp(self.not_before_unix)
            .map_err(|_| NodeIdentityStoreError::InvalidCredentials)
    }

    pub fn not_after(&self) -> Result<OffsetDateTime, NodeIdentityStoreError> {
        OffsetDateTime::from_unix_timestamp(self.not_after_unix)
            .map_err(|_| NodeIdentityStoreError::InvalidCredentials)
    }

    pub fn is_valid_at(&self, now: OffsetDateTime) -> bool {
        self.not_before().is_ok_and(|not_before| now >= not_before)
            && self.not_after().is_ok_and(|not_after| now < not_after)
    }
}

impl fmt::Debug for InstalledNodeIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstalledNodeIdentity")
            .field("version", &self.version)
            .field("node_id", &self.node_id)
            .field("overlay_ip", &self.overlay_ip)
            .field("not_before_unix", &self.not_before_unix)
            .field("not_after_unix", &self.not_after_unix)
            .field("spki_sha256", &self.spki_sha256)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct NodeIdentityStore {
    root: PathBuf,
    key: NodeKey,
    pending: Mutex<Option<PendingEnrollmentRequest>>,
    installed: Mutex<Option<InstalledNodeIdentity>>,
}

impl NodeIdentityStore {
    /// Opens a secure identity directory, creating only the directory and node
    /// key when they do not yet exist.
    pub fn load_or_create(root: impl AsRef<Path>) -> Result<Self, NodeIdentityStoreError> {
        ensure_supported("open a node identity store")?;
        let root = root.as_ref().to_path_buf();
        ensure_secure_directory(&root)?;

        let key_path = root.join(NODE_KEY_FILE);
        let key = NodeKey::load_or_create(&key_path).map_err(NodeIdentityStoreError::Identity)?;
        let pending = load_optional_json::<PendingEnrollmentRequest>(
            &root.join(ENROLLMENT_REQUEST_FILE),
            MAX_ENROLLMENT_REQUEST_BYTES,
            true,
        )?;
        if let Some(request) = &pending {
            validate_pending_request(request, &key)?;
        }

        let installed = load_optional_json::<InstalledNodeIdentity>(
            &root.join(NODE_CREDENTIALS_FILE),
            MAX_CREDENTIALS_BYTES,
            true,
        )?;
        if let Some(identity) = &installed {
            validate_identity_document(identity, &key, None)?;
        }

        Ok(Self {
            root,
            key,
            pending: Mutex::new(pending),
            installed: Mutex::new(installed),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn key_path(&self) -> PathBuf {
        self.root.join(NODE_KEY_FILE)
    }

    pub fn public_key_der(&self) -> Vec<u8> {
        self.key.public_key_der()
    }

    pub fn pending_enrollment(&self) -> Option<PendingEnrollmentRequest> {
        self.lock_pending().clone()
    }

    pub fn installed_identity(&self) -> Option<InstalledNodeIdentity> {
        self.lock_installed().clone()
    }

    /// Returns an existing matching request or atomically persists a new one.
    /// A pending request must be explicitly cleared before changing node ID or
    /// enrollment token.
    pub fn prepare_enrollment(
        &self,
        node_id: &str,
        enrollment_token: &str,
    ) -> Result<PendingEnrollmentRequest, NodeIdentityStoreError> {
        validate_node_id(node_id).map_err(|_| NodeIdentityStoreError::InvalidNodeId)?;
        EnrollmentToken::parse(enrollment_token)
            .map_err(|_| NodeIdentityStoreError::InvalidEnrollmentToken)?;

        let mut pending = self.lock_pending();
        if let Some(existing) = pending.as_ref() {
            if existing.node_id != node_id
                || !constant_time_token_eq(&existing.enrollment_token, enrollment_token)
            {
                return Err(NodeIdentityStoreError::PendingEnrollmentConflict);
            }
            return Ok(existing.clone());
        }

        let csr_der = self
            .key
            .create_csr_der()
            .map_err(NodeIdentityStoreError::Identity)?;
        let request = PendingEnrollmentRequest {
            version: NODE_IDENTITY_STORE_VERSION,
            enrollment_id: Uuid::new_v4().to_string(),
            node_id: node_id.to_owned(),
            enrollment_token: enrollment_token.to_owned(),
            csr_der_base64: STANDARD.encode(csr_der),
        };
        let encoded = encode_bounded(&request, MAX_ENROLLMENT_REQUEST_BYTES as usize)?;
        atomic_replace(&self.root.join(ENROLLMENT_REQUEST_FILE), &encoded, 0o600)?;
        *pending = Some(request.clone());
        Ok(request)
    }

    /// Installs one coordinator-verified node certificate and its exact trust
    /// root as a single durable transaction.
    pub fn install_certificate(
        &self,
        node_id: &str,
        overlay_ip: Ipv4Addr,
        node_ca_pem: &str,
        node_certificate_pem: &str,
        now: OffsetDateTime,
    ) -> Result<InstalledNodeIdentity, NodeIdentityStoreError> {
        let identity = build_identity_document(
            node_id,
            overlay_ip,
            node_ca_pem,
            node_certificate_pem,
            &self.key,
            now,
        )?;
        let encoded = encode_bounded(&identity, MAX_CREDENTIALS_BYTES as usize)?;

        let mut installed = self.lock_installed();
        if let Some(current) = installed.as_ref()
            && (current.node_id != identity.node_id
                || current.overlay_ip != identity.overlay_ip
                || parse_one_certificate(&current.node_ca_pem)?
                    != parse_one_certificate(&identity.node_ca_pem)?)
        {
            return Err(NodeIdentityStoreError::TrustRootReplacement);
        }
        atomic_replace(&self.root.join(NODE_CREDENTIALS_FILE), &encoded, 0o600)?;
        *installed = Some(identity.clone());
        Ok(identity)
    }

    /// Removes a completed request only when its ID still matches, preventing
    /// stale completion callbacks from deleting a newer enrollment attempt.
    pub fn clear_pending_enrollment(
        &self,
        enrollment_id: &str,
    ) -> Result<bool, NodeIdentityStoreError> {
        let mut pending = self.lock_pending();
        let Some(current) = pending.as_ref() else {
            return Ok(false);
        };
        if current.enrollment_id != enrollment_id {
            return Ok(false);
        }

        remove_durable(&self.root.join(ENROLLMENT_REQUEST_FILE))?;
        *pending = None;
        Ok(true)
    }

    fn lock_pending(&self) -> MutexGuard<'_, Option<PendingEnrollmentRequest>> {
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn lock_installed(&self) -> MutexGuard<'_, Option<InstalledNodeIdentity>> {
        self.installed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn build_identity_document(
    node_id: &str,
    overlay_ip: Ipv4Addr,
    node_ca_pem: &str,
    node_certificate_pem: &str,
    key: &NodeKey,
    now: OffsetDateTime,
) -> Result<InstalledNodeIdentity, NodeIdentityStoreError> {
    validate_node_id(node_id).map_err(|_| NodeIdentityStoreError::InvalidNodeId)?;
    let (not_before_unix, not_after_unix, spki_sha256) = validate_certificate_pair(
        node_id,
        overlay_ip,
        node_ca_pem,
        node_certificate_pem,
        key,
        Some(now),
    )?;
    Ok(InstalledNodeIdentity {
        version: NODE_IDENTITY_STORE_VERSION,
        node_id: node_id.to_owned(),
        overlay_ip,
        node_ca_pem: node_ca_pem.to_owned(),
        node_certificate_pem: node_certificate_pem.to_owned(),
        not_before_unix,
        not_after_unix,
        spki_sha256,
    })
}

fn validate_identity_document(
    identity: &InstalledNodeIdentity,
    key: &NodeKey,
    now: Option<OffsetDateTime>,
) -> Result<(), NodeIdentityStoreError> {
    if identity.version != NODE_IDENTITY_STORE_VERSION {
        return Err(NodeIdentityStoreError::UnsupportedVersion(identity.version));
    }
    validate_node_id(&identity.node_id).map_err(|_| NodeIdentityStoreError::InvalidCredentials)?;
    let (not_before, not_after, spki) = validate_certificate_pair(
        &identity.node_id,
        identity.overlay_ip,
        &identity.node_ca_pem,
        &identity.node_certificate_pem,
        key,
        now,
    )?;
    if not_before != identity.not_before_unix
        || not_after != identity.not_after_unix
        || spki != identity.spki_sha256
    {
        return Err(NodeIdentityStoreError::InvalidCredentials);
    }
    Ok(())
}

fn validate_certificate_pair(
    node_id: &str,
    overlay_ip: Ipv4Addr,
    node_ca_pem: &str,
    node_certificate_pem: &str,
    key: &NodeKey,
    now: Option<OffsetDateTime>,
) -> Result<(i64, i64, String), NodeIdentityStoreError> {
    let ca_der = parse_one_certificate(node_ca_pem)?;
    let leaf_der = parse_one_certificate(node_certificate_pem)?;
    let (ca_remainder, ca) =
        parse_x509_certificate(&ca_der).map_err(|_| NodeIdentityStoreError::InvalidCredentials)?;
    let (leaf_remainder, leaf) = parse_x509_certificate(&leaf_der)
        .map_err(|_| NodeIdentityStoreError::InvalidCredentials)?;
    if !ca_remainder.is_empty()
        || !leaf_remainder.is_empty()
        || ca.subject() != ca.issuer()
        || leaf.issuer() != ca.subject()
        || ca
            .basic_constraints()
            .map_err(|_| NodeIdentityStoreError::InvalidCredentials)?
            .is_none_or(|constraints| !constraints.value.ca)
        || leaf
            .basic_constraints()
            .map_err(|_| NodeIdentityStoreError::InvalidCredentials)?
            .is_none_or(|constraints| constraints.value.ca)
    {
        return Err(NodeIdentityStoreError::InvalidCredentials);
    }
    ca.verify_signature(None)
        .map_err(|_| NodeIdentityStoreError::InvalidCredentials)?;
    leaf.verify_signature(Some(ca.public_key()))
        .map_err(|_| NodeIdentityStoreError::InvalidCredentials)?;
    if leaf.public_key().raw != key.public_key_der().as_slice() {
        return Err(NodeIdentityStoreError::CertificateKeyMismatch);
    }

    let subject_alternative_name = leaf
        .subject_alternative_name()
        .map_err(|_| NodeIdentityStoreError::InvalidCredentials)?
        .ok_or(NodeIdentityStoreError::InvalidCredentials)?;
    let expected_uri = format!("{NODE_ID_URI_PREFIX}{node_id}");
    let mut uris = Vec::new();
    let mut addresses = Vec::new();
    for name in &subject_alternative_name.value.general_names {
        match name {
            GeneralName::URI(uri) => uris.push(*uri),
            GeneralName::IPAddress(bytes) if bytes.len() == 4 => {
                addresses.push(IpAddr::V4(Ipv4Addr::new(
                    bytes[0], bytes[1], bytes[2], bytes[3],
                )));
            }
            _ => return Err(NodeIdentityStoreError::InvalidCredentials),
        }
    }
    if uris != [expected_uri.as_str()] || addresses != [IpAddr::V4(overlay_ip)] {
        return Err(NodeIdentityStoreError::CertificateIdentityMismatch);
    }

    let not_before = leaf.validity().not_before.to_datetime();
    let not_after = leaf.validity().not_after.to_datetime();
    let ca_not_before = ca.validity().not_before.to_datetime();
    let ca_not_after = ca.validity().not_after.to_datetime();
    if not_before >= not_after
        || not_before < ca_not_before
        || not_after > ca_not_after
        || now.is_some_and(|now| now < not_before || now >= not_after)
        || now.is_some_and(|now| now < ca_not_before || now >= ca_not_after)
    {
        return Err(NodeIdentityStoreError::CertificateNotCurrentlyValid);
    }

    let digest = Sha256::digest(leaf.public_key().raw);
    Ok((
        not_before.unix_timestamp(),
        not_after.unix_timestamp(),
        format_sha256(&digest),
    ))
}

fn validate_pending_request(
    request: &PendingEnrollmentRequest,
    key: &NodeKey,
) -> Result<(), NodeIdentityStoreError> {
    if request.version != NODE_IDENTITY_STORE_VERSION {
        return Err(NodeIdentityStoreError::UnsupportedVersion(request.version));
    }
    Uuid::parse_str(&request.enrollment_id)
        .map_err(|_| NodeIdentityStoreError::InvalidEnrollmentRequest)?;
    validate_node_id(&request.node_id)
        .map_err(|_| NodeIdentityStoreError::InvalidEnrollmentRequest)?;
    EnrollmentToken::parse(&request.enrollment_token)
        .map_err(|_| NodeIdentityStoreError::InvalidEnrollmentRequest)?;

    let csr_der = request.csr_der()?;
    let (remainder, csr) = X509CertificationRequest::from_der(&csr_der)
        .map_err(|_| NodeIdentityStoreError::InvalidEnrollmentRequest)?;
    if !remainder.is_empty()
        || csr.certification_request_info.subject_pki.raw != key.public_key_der().as_slice()
    {
        return Err(NodeIdentityStoreError::InvalidEnrollmentRequest);
    }
    csr.verify_signature()
        .map_err(|_| NodeIdentityStoreError::InvalidEnrollmentRequest)
}

fn decode_csr(encoded: &str) -> Result<Vec<u8>, NodeIdentityStoreError> {
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| NodeIdentityStoreError::InvalidEnrollmentRequest)?;
    if decoded.is_empty() || decoded.len() > MAX_CSR_BYTES {
        return Err(NodeIdentityStoreError::InvalidEnrollmentRequest);
    }
    Ok(decoded)
}

fn parse_one_certificate(pem_text: &str) -> Result<Vec<u8>, NodeIdentityStoreError> {
    if pem_text.is_empty() || pem_text.len() > MAX_CERTIFICATE_PEM_BYTES {
        return Err(NodeIdentityStoreError::InvalidCredentials);
    }
    let blocks = pem::parse_many(pem_text.as_bytes())
        .map_err(|_| NodeIdentityStoreError::InvalidCredentials)?;
    if blocks.len() != 1 || blocks[0].tag() != "CERTIFICATE" {
        return Err(NodeIdentityStoreError::InvalidCredentials);
    }
    Ok(blocks[0].contents().to_vec())
}

fn constant_time_token_eq(left: &str, right: &str) -> bool {
    let left: [u8; 32] = Sha256::digest(left.as_bytes()).into();
    let right: [u8; 32] = Sha256::digest(right.as_bytes()).into();
    bool::from(left.ct_eq(&right))
}

fn format_sha256(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    format!("sha256:{encoded}")
}

fn encode_bounded<T: Serialize>(
    value: &T,
    maximum: usize,
) -> Result<Vec<u8>, NodeIdentityStoreError> {
    let mut encoded =
        serde_json::to_vec_pretty(value).map_err(|_| NodeIdentityStoreError::InvalidJson)?;
    encoded.push(b'\n');
    if encoded.len() > maximum || encoded.len() > MAX_NODE_IDENTITY_FILE_BYTES {
        return Err(NodeIdentityStoreError::FileTooLarge);
    }
    Ok(encoded)
}

#[cfg(unix)]
fn ensure_secure_directory(path: &Path) -> Result<(), NodeIdentityStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|source| NodeIdentityStoreError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
            let metadata =
                fs::symlink_metadata(path).map_err(|source| NodeIdentityStoreError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
            validate_directory(path, &metadata)
        }
        Err(source) => Err(NodeIdentityStoreError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(unix)]
fn validate_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), NodeIdentityStoreError> {
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(NodeIdentityStoreError::InsecurePath(path.to_path_buf()));
    }
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.uid() != expected_uid || metadata.mode() & 0o077 != 0 {
        return Err(NodeIdentityStoreError::InsecurePermissions(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_secure_directory(_path: &Path) -> Result<(), NodeIdentityStoreError> {
    Err(NodeIdentityStoreError::UnsupportedPlatform)
}

#[cfg(unix)]
fn load_optional_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    maximum: u64,
    secret: bool,
) -> Result<Option<T>, NodeIdentityStoreError> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(NodeIdentityStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let metadata = file
        .metadata()
        .map_err(|source| NodeIdentityStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || (secret && metadata.mode() & 0o077 != 0)
    {
        return Err(NodeIdentityStoreError::InsecurePermissions(
            path.to_path_buf(),
        ));
    }
    if metadata.len() == 0 || metadata.len() > maximum {
        return Err(NodeIdentityStoreError::FileTooLarge);
    }

    let mut encoded = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut encoded)
        .map_err(|source| NodeIdentityStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    serde_json::from_slice(&encoded)
        .map(Some)
        .map_err(|_| NodeIdentityStoreError::InvalidJson)
}

#[cfg(not(unix))]
fn load_optional_json<T: for<'de> Deserialize<'de>>(
    _path: &Path,
    _maximum: u64,
    _secret: bool,
) -> Result<Option<T>, NodeIdentityStoreError> {
    Err(NodeIdentityStoreError::UnsupportedPlatform)
}

#[cfg(unix)]
fn atomic_replace(path: &Path, contents: &[u8], mode: u32) -> Result<(), NodeIdentityStoreError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| NodeIdentityStoreError::InsecurePath(path.to_path_buf()))?;
    let mut random = OsRng;
    for _ in 0..TEMP_FILE_ATTEMPTS {
        let temporary = parent.join(format!(
            ".{}.tmp-{:016x}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("identity"),
            random.next_u64()
        ));
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW);
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(NodeIdentityStoreError::Io {
                    path: temporary,
                    source,
                });
            }
        };
        let result = (|| {
            file.write_all(contents)?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            File::open(parent)?.sync_all()?;
            Ok::<_, io::Error>(())
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temporary);
            return Err(NodeIdentityStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
        return Ok(());
    }
    Err(NodeIdentityStoreError::TemporaryFileExhausted)
}

#[cfg(not(unix))]
fn atomic_replace(
    _path: &Path,
    _contents: &[u8],
    _mode: u32,
) -> Result<(), NodeIdentityStoreError> {
    Err(NodeIdentityStoreError::UnsupportedPlatform)
}

#[cfg(unix)]
fn remove_durable(path: &Path) -> Result<(), NodeIdentityStoreError> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(NodeIdentityStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| NodeIdentityStoreError::InsecurePath(path.to_path_buf()))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| NodeIdentityStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn remove_durable(_path: &Path) -> Result<(), NodeIdentityStoreError> {
    Err(NodeIdentityStoreError::UnsupportedPlatform)
}

#[cfg(unix)]
fn ensure_supported(_operation: &'static str) -> Result<(), NodeIdentityStoreError> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_supported(_operation: &'static str) -> Result<(), NodeIdentityStoreError> {
    Err(NodeIdentityStoreError::UnsupportedPlatform)
}

#[derive(Debug, thiserror::Error)]
pub enum NodeIdentityStoreError {
    #[error("identity persistence is not supported on this platform")]
    UnsupportedPlatform,
    #[error("identity store version {0} is unsupported; expected version 2")]
    UnsupportedVersion(u16),
    #[error("node ID is invalid")]
    InvalidNodeId,
    #[error("enrollment token is invalid")]
    InvalidEnrollmentToken,
    #[error("a different enrollment request is already pending")]
    PendingEnrollmentConflict,
    #[error("persisted enrollment request is invalid")]
    InvalidEnrollmentRequest,
    #[error("persisted credentials are invalid")]
    InvalidCredentials,
    #[error("node certificate does not identify the expected node and overlay address")]
    CertificateIdentityMismatch,
    #[error("node certificate public key does not match the local private key")]
    CertificateKeyMismatch,
    #[error("certificate renewal attempted to replace the installed node CA or node binding")]
    TrustRootReplacement,
    #[error("node certificate or its CA is not currently valid")]
    CertificateNotCurrentlyValid,
    #[error("identity file contains invalid JSON")]
    InvalidJson,
    #[error("identity file is empty or exceeds its size limit")]
    FileTooLarge,
    #[error("identity path is not a secure regular file or directory: {0}")]
    InsecurePath(PathBuf),
    #[error("identity path has an unexpected owner or unsafe permissions: {0}")]
    InsecurePermissions(PathBuf),
    #[error("could not allocate a unique temporary identity file")]
    TemporaryFileExhausted,
    #[error("identity cryptography failed: {0}")]
    Identity(#[source] crate::identity::IdentityError),
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
    use crate::{
        identity::CertificateAuthority,
        registry::{ENROLLMENT_TOKEN_BYTES, EnrollmentToken},
    };
    use ipnet::Ipv4Net;
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
                "stellaris-identity-store-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create fixture directory");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("secure fixture directory");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl AsRef<Path> for TestDirectory {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn enrollment_token() -> String {
        EnrollmentToken::from_bytes([7; ENROLLMENT_TOKEN_BYTES]).encode()
    }

    fn issue_for_store(
        store: &NodeIdentityStore,
        directory: &TestDirectory,
        node_id: &str,
        overlay_ip: Ipv4Addr,
    ) -> (String, String) {
        let ca =
            CertificateAuthority::create(directory.join("ca.pem"), directory.join("ca-key.pem"))
                .expect("create fixture CA");
        let request = store
            .prepare_enrollment(node_id, &enrollment_token())
            .expect("prepare enrollment");
        let issued = ca
            .issue_node_certificate(
                &request.csr_der().expect("decode CSR"),
                node_id,
                "10.88.0.0/24".parse::<Ipv4Net>().unwrap(),
                overlay_ip,
            )
            .expect("issue fixture certificate");
        (ca.certificate_pem().to_owned(), issued.pem().to_owned())
    }

    #[test]
    fn pending_enrollment_is_stable_across_restart_and_redacted() {
        let directory = TestDirectory::new("pending");
        let store = NodeIdentityStore::load_or_create(&directory).expect("open store");
        let first = store
            .prepare_enrollment("edge-a", &enrollment_token())
            .expect("prepare request");
        let second = store
            .prepare_enrollment("edge-a", &enrollment_token())
            .expect("reuse request");
        assert_eq!(first, second);
        assert!(!format!("{first:?}").contains(&enrollment_token()));

        drop(store);
        let reopened = NodeIdentityStore::load_or_create(&directory).expect("reopen store");
        assert_eq!(reopened.pending_enrollment(), Some(first));
        assert!(matches!(
            reopened.prepare_enrollment(
                "edge-b",
                &EnrollmentToken::from_bytes([8; ENROLLMENT_TOKEN_BYTES]).encode()
            ),
            Err(NodeIdentityStoreError::PendingEnrollmentConflict)
        ));
    }

    #[test]
    fn v1_pending_enrollment_is_rejected() {
        let directory = TestDirectory::new("pending-v1");
        let store = NodeIdentityStore::load_or_create(&directory).expect("open store");
        let mut request = store
            .prepare_enrollment("edge-a", &enrollment_token())
            .expect("prepare request");
        request.version = 1;
        drop(store);

        let encoded = encode_bounded(&request, MAX_ENROLLMENT_REQUEST_BYTES as usize).unwrap();
        atomic_replace(&directory.join(ENROLLMENT_REQUEST_FILE), &encoded, 0o600).unwrap();
        assert!(matches!(
            NodeIdentityStore::load_or_create(&directory),
            Err(NodeIdentityStoreError::UnsupportedVersion(1))
        ));
    }

    #[test]
    fn certificate_and_ca_install_atomically_and_survive_restart() {
        let directory = TestDirectory::new("install");
        let store = NodeIdentityStore::load_or_create(&directory).expect("open store");
        let overlay_ip = "10.88.0.2".parse().unwrap();
        let (ca, certificate) = issue_for_store(&store, &directory, "edge-a", overlay_ip);
        let installed = store
            .install_certificate(
                "edge-a",
                overlay_ip,
                &ca,
                &certificate,
                OffsetDateTime::now_utc(),
            )
            .expect("install identity");
        assert!(installed.is_valid_at(OffsetDateTime::now_utc()));
        assert_eq!(installed.node_ca_pem(), ca);

        drop(store);
        let reopened = NodeIdentityStore::load_or_create(&directory).expect("reopen store");
        assert_eq!(reopened.installed_identity(), Some(installed));
    }

    #[test]
    fn v1_installed_credentials_are_rejected() {
        let directory = TestDirectory::new("credentials-v1");
        let store = NodeIdentityStore::load_or_create(&directory).expect("open store");
        let overlay_ip = "10.88.0.2".parse().unwrap();
        let (ca, certificate) = issue_for_store(&store, &directory, "edge-a", overlay_ip);
        let mut installed = store
            .install_certificate(
                "edge-a",
                overlay_ip,
                &ca,
                &certificate,
                OffsetDateTime::now_utc(),
            )
            .expect("install identity");
        installed.version = 1;
        drop(store);

        let encoded = encode_bounded(&installed, MAX_CREDENTIALS_BYTES as usize).unwrap();
        atomic_replace(&directory.join(NODE_CREDENTIALS_FILE), &encoded, 0o600).unwrap();
        assert!(matches!(
            NodeIdentityStore::load_or_create(&directory),
            Err(NodeIdentityStoreError::UnsupportedVersion(1))
        ));
    }

    #[test]
    fn a_certificate_for_another_local_key_is_rejected() {
        let first = TestDirectory::new("key-a");
        let second = TestDirectory::new("key-b");
        let first_store = NodeIdentityStore::load_or_create(&first).expect("first store");
        let second_store = NodeIdentityStore::load_or_create(&second).expect("second store");
        let overlay_ip = "10.88.0.2".parse().unwrap();
        let (ca, certificate) = issue_for_store(&first_store, &first, "edge-a", overlay_ip);
        assert!(matches!(
            second_store.install_certificate(
                "edge-a",
                overlay_ip,
                &ca,
                &certificate,
                OffsetDateTime::now_utc()
            ),
            Err(NodeIdentityStoreError::CertificateKeyMismatch)
        ));
    }

    #[test]
    fn unsafe_store_permissions_fail_closed() {
        let directory = TestDirectory::new("permissions");
        fs::set_permissions(directory.as_ref(), fs::Permissions::from_mode(0o755))
            .expect("make fixture unsafe");
        assert!(matches!(
            NodeIdentityStore::load_or_create(&directory),
            Err(NodeIdentityStoreError::InsecurePermissions(_))
        ));
    }

    #[test]
    fn stale_completion_cannot_clear_another_request() {
        let directory = TestDirectory::new("clear");
        let store = NodeIdentityStore::load_or_create(&directory).expect("open store");
        let request = store
            .prepare_enrollment("edge-a", &enrollment_token())
            .expect("prepare request");
        assert!(!store.clear_pending_enrollment("different").unwrap());
        assert!(store.pending_enrollment().is_some());
        assert!(
            store
                .clear_pending_enrollment(&request.enrollment_id)
                .unwrap()
        );
        assert!(store.pending_enrollment().is_none());
        assert!(!directory.join(ENROLLMENT_REQUEST_FILE).exists());
    }
}
