// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Persistent node keys and coordinator certificate issuance for protocol v2.
//!
//! Persistence is intentionally Unix-only for now. Secret files are opened
//! without following symlinks and must not grant access to group or other
//! users. Windows callers receive [`IdentityError::UnsupportedPlatform`]
//! until the project has a reviewed ACL implementation for these files.

use std::{
    fmt, fs, io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
};

use ipnet::Ipv4Net;
use rand::{Rng, RngCore, rngs::OsRng};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, PublicKeyData,
    SanType, SerialNumber, SubjectPublicKeyInfo,
};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use x509_parser::{
    certificate::X509Certificate,
    certification_request::X509CertificationRequest,
    extensions::{GeneralName, ParsedExtension},
    oid_registry::OID_SIG_ECDSA_WITH_SHA256,
    parse_x509_certificate,
    prelude::FromDer,
    x509::X509Version,
};

use crate::{
    registry::{validate_node_id, validate_overlay_address},
    transport::AuthenticatedConnection,
};

pub const NODE_ID_URI_PREFIX: &str = "urn:stellaris:node:";
pub const NODE_CERTIFICATE_TTL: Duration = Duration::hours(24);
pub const CERTIFICATE_RENEWAL_JITTER: Duration = Duration::minutes(30);

const CA_COMMON_NAME: &str = "Stellaris Coordination CA";
const CA_VALIDITY: Duration = Duration::days(3650);
const CA_CLOCK_SKEW: Duration = Duration::minutes(5);
const MAX_PRIVATE_KEY_BYTES: u64 = 64 * 1024;
const MAX_CERTIFICATE_BYTES: u64 = 256 * 1024;
const MAX_CSR_BYTES: usize = 64 * 1024;
const TEMP_FILE_ATTEMPTS: usize = 16;
const CA_KEY_USAGE_FLAGS: u16 = (1 << 5) | (1 << 6);
const CA_EXTENSION_COUNT: usize = 4;
const NODE_EXTENSION_COUNT: usize = 6;

pub struct NodeKey {
    key_pair: KeyPair,
}

impl NodeKey {
    /// Loads an existing P-256 PKCS#8 key or atomically creates one.
    ///
    /// An existing path is never replaced. Invalid contents, insecure Unix
    /// permissions, symlinks, and non-regular files all fail closed.
    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self, IdentityError> {
        ensure_persistence_supported("load or create a node key")?;
        let path = path.as_ref();

        if path_exists(path)? {
            return load_node_key(path);
        }

        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|source| {
            IdentityError::Crypto {
                context: "generate node key",
                source,
            }
        })?;
        match write_atomic_new(path, key_pair.serialize_pem().as_bytes(), 0o600)? {
            InstallOutcome::Installed | InstallOutcome::Existing => load_node_key(path),
        }
    }

    /// Creates a minimal PKCS#10 request proving possession of this key.
    ///
    /// The coordinator does not trust request attributes; the request is kept
    /// minimal to avoid suggesting that they influence the issued identity.
    pub fn create_csr_der(&self) -> Result<Vec<u8>, IdentityError> {
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        let request =
            params
                .serialize_request(&self.key_pair)
                .map_err(|source| IdentityError::Crypto {
                    context: "create node certificate request",
                    source,
                })?;
        Ok(request.der().as_ref().to_vec())
    }

    pub fn public_key_der(&self) -> Vec<u8> {
        self.key_pair.public_key_der()
    }
}

impl fmt::Debug for NodeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NodeKey([REDACTED])")
    }
}

pub struct CertificateAuthority {
    signer_certificate: Certificate,
    key_pair: KeyPair,
    certificate_der: Vec<u8>,
    certificate_pem: String,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
}

impl CertificateAuthority {
    /// Creates and persists a new trust root only when both target files are
    /// absent. Use [`Self::load`] on every subsequent start.
    pub fn create(
        certificate_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, IdentityError> {
        ensure_persistence_supported("create a certificate authority")?;
        let certificate_path = certificate_path.as_ref();
        let key_path = key_path.as_ref();
        if certificate_path == key_path {
            return Err(IdentityError::ConflictingPaths(
                certificate_path.to_path_buf(),
            ));
        }

        match (path_exists(certificate_path)?, path_exists(key_path)?) {
            (false, false) => {}
            (true, true) => return Err(IdentityError::CaAlreadyInitialized),
            (certificate_exists, key_exists) => {
                return Err(IdentityError::IncompleteCaState {
                    certificate_exists,
                    key_exists,
                });
            }
        }

        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|source| {
            IdentityError::Crypto {
                context: "generate certificate authority key",
                source,
            }
        })?;
        let now = whole_seconds(OffsetDateTime::now_utc());
        let mut params = CertificateParams::default();
        params.not_before = now - CA_CLOCK_SKEW;
        params.not_after = now + CA_VALIDITY;
        params.serial_number = Some(random_serial_number());
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, CA_COMMON_NAME);
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.use_authority_key_identifier_extension = true;

        let certificate =
            params
                .self_signed(&key_pair)
                .map_err(|source| IdentityError::Crypto {
                    context: "create certificate authority certificate",
                    source,
                })?;
        let certificate_pem = certificate.pem();
        let key_pem = key_pair.serialize_pem();

        if write_atomic_new(key_path, key_pem.as_bytes(), 0o600)? == InstallOutcome::Existing {
            return Err(IdentityError::CaAlreadyInitialized);
        }
        let certificate_install =
            write_atomic_new(certificate_path, certificate_pem.as_bytes(), 0o644);
        match certificate_install {
            Ok(InstallOutcome::Installed) => {}
            Ok(InstallOutcome::Existing) => {
                rollback_created_identity_files(&[key_path])?;
                return Err(IdentityError::CaAlreadyInitialized);
            }
            Err(error) => {
                rollback_created_identity_files(&[key_path])?;
                return Err(error);
            }
        }

        // Loading through the same validation path guarantees newly persisted
        // and restarted authorities have identical acceptance rules.
        match Self::load(certificate_path, key_path) {
            Ok(authority) => Ok(authority),
            Err(error) => {
                rollback_created_identity_files(&[certificate_path, key_path])?;
                Err(error)
            }
        }
    }

    /// Loads a complete persisted authority. This function never creates a
    /// key or certificate and never replaces invalid material.
    pub fn load(
        certificate_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, IdentityError> {
        ensure_persistence_supported("load a certificate authority")?;
        let certificate_path = certificate_path.as_ref();
        let key_path = key_path.as_ref();
        if certificate_path == key_path {
            return Err(IdentityError::ConflictingPaths(
                certificate_path.to_path_buf(),
            ));
        }

        match (path_exists(certificate_path)?, path_exists(key_path)?) {
            (false, false) => return Err(IdentityError::CaNotInitialized),
            (true, true) => {}
            (certificate_exists, key_exists) => {
                return Err(IdentityError::IncompleteCaState {
                    certificate_exists,
                    key_exists,
                });
            }
        }

        let certificate_bytes = read_regular_file(certificate_path, false, MAX_CERTIFICATE_BYTES)?;
        let certificate_pem = std::str::from_utf8(&certificate_bytes)
            .map_err(|_| IdentityError::InvalidCaCertificate)?
            .to_owned();
        let certificate_der = parse_ca_pem(&certificate_bytes)?;

        let key_bytes = read_regular_file(key_path, true, MAX_PRIVATE_KEY_BYTES)?;
        let key_pair = parse_p256_key(&key_bytes, key_path)?;
        let (not_before, not_after) = validate_ca_certificate(&certificate_der, &key_pair)?;

        let params = CertificateParams::from_ca_cert_pem(&certificate_pem)
            .map_err(|_| IdentityError::InvalidCaCertificate)?;
        let signer_certificate = params
            .self_signed(&key_pair)
            .map_err(|_| IdentityError::InvalidCaCertificate)?;

        Ok(Self {
            signer_certificate,
            key_pair,
            certificate_der,
            certificate_pem,
            not_before,
            not_after,
        })
    }

    /// Removes a CA pair created by the current, not-yet-committed server
    /// initialization transaction and durably records the directory changes.
    pub(crate) fn rollback_new(
        certificate_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<(), IdentityError> {
        rollback_created_identity_files(&[certificate_path.as_ref(), key_path.as_ref()])
    }

    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    pub fn certificate_pem(&self) -> &str {
        &self.certificate_pem
    }

    /// Verifies CSR proof-of-possession and issues a coordinator-controlled
    /// 24-hour node certificate.
    pub fn issue_node_certificate(
        &self,
        csr_der: &[u8],
        node_id: &str,
        overlay: Ipv4Net,
        overlay_ip: Ipv4Addr,
    ) -> Result<IssuedCertificate, IdentityError> {
        validate_node_id(node_id).map_err(|_| IdentityError::InvalidNodeId)?;
        validate_overlay_address(overlay, overlay_ip)
            .map_err(|_| IdentityError::InvalidOverlayAddress(overlay_ip))?;
        let public_key = verified_csr_public_key(csr_der)?;

        let not_before = whole_seconds(OffsetDateTime::now_utc());
        let not_after = not_before + NODE_CERTIFICATE_TTL;
        if not_before < self.not_before || not_after > self.not_after {
            return Err(IdentityError::CaOutsideIssuanceWindow);
        }

        let node_uri = format!("{NODE_ID_URI_PREFIX}{node_id}");
        let mut params = CertificateParams::default();
        params.not_before = not_before;
        params.not_after = not_after;
        params.serial_number = Some(random_serial_number());
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, node_id);
        params.subject_alt_names = vec![
            SanType::URI(
                node_uri
                    .try_into()
                    .map_err(|_| IdentityError::InvalidNodeId)?,
            ),
            SanType::IpAddress(overlay_ip.into()),
        ];
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ClientAuth,
            ExtendedKeyUsagePurpose::ServerAuth,
        ];
        params.use_authority_key_identifier_extension = true;

        let certificate = params
            .signed_by(&public_key, &self.signer_certificate, &self.key_pair)
            .map_err(|source| IdentityError::Crypto {
                context: "issue node certificate",
                source,
            })?;
        let der = certificate.der().as_ref().to_vec();
        let fingerprint = certificate_fingerprint(&der);
        let renew_after = randomized_renewal_time(not_before, not_after);

        Ok(IssuedCertificate {
            pem: certificate.pem(),
            der,
            fingerprint,
            not_before,
            not_after,
            renew_after,
        })
    }
}

impl fmt::Debug for CertificateAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertificateAuthority")
            .field(
                "certificate_fingerprint",
                &certificate_fingerprint(&self.certificate_der),
            )
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct IssuedCertificate {
    der: Vec<u8>,
    pem: String,
    fingerprint: String,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
    renew_after: OffsetDateTime,
}

impl fmt::Debug for IssuedCertificate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedCertificate")
            .field("fingerprint", &self.fingerprint)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .field("renew_after", &self.renew_after)
            .finish_non_exhaustive()
    }
}

impl IssuedCertificate {
    pub fn der(&self) -> &[u8] {
        &self.der
    }

    pub fn pem(&self) -> &str {
        &self.pem
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub const fn not_before(&self) -> OffsetDateTime {
        self.not_before
    }

    pub const fn not_after(&self) -> OffsetDateTime {
        self.not_after
    }

    pub const fn renew_after(&self) -> OffsetDateTime {
        self.renew_after
    }
}

/// Application identity extracted from a leaf certificate whose TLS chain has
/// already been verified by the transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedNodeCertificate {
    node_id: String,
    overlay_ip: Ipv4Addr,
    fingerprint: String,
    spki_fingerprint: String,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
}

impl VerifiedNodeCertificate {
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub const fn overlay_ip(&self) -> Ipv4Addr {
        self.overlay_ip
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn spki_fingerprint(&self) -> &str {
        &self.spki_fingerprint
    }

    pub const fn not_before(&self) -> OffsetDateTime {
        self.not_before
    }

    pub const fn not_after(&self) -> OffsetDateTime {
        self.not_after
    }
}

/// Binds the peer authenticated by a completed v2 TLS handshake to the static
/// node registry.
///
/// The strong transport wrapper has already fixed the ALPN and captured the
/// peer chain. This function independently verifies that the leaf was signed
/// directly by the caller-provided Stellaris CA and matches the expected node.
pub fn authenticate_verified_node_certificate(
    connection: &AuthenticatedConnection,
    trusted_ca_der: &[u8],
    expected_node_id: &str,
    expected_overlay_ip: Ipv4Addr,
    overlay: Ipv4Net,
    now: OffsetDateTime,
) -> Result<VerifiedNodeCertificate, IdentityError> {
    let verified = extract_verified_node_certificate(connection, trusted_ca_der, overlay, now)?;
    if verified.node_id != expected_node_id || verified.overlay_ip != expected_overlay_ip {
        return Err(IdentityError::NodeCertificateBindingMismatch);
    }
    Ok(verified)
}

/// Verifies the TLS-authenticated leaf and extracts its signed Stellaris
/// identity before consulting any registry or application message.
pub fn extract_verified_node_certificate(
    connection: &AuthenticatedConnection,
    trusted_ca_der: &[u8],
    overlay: Ipv4Net,
    now: OffsetDateTime,
) -> Result<VerifiedNodeCertificate, IdentityError> {
    let leaf_der = connection
        .certificate_chain_der()
        .first()
        .ok_or(IdentityError::MissingVerifiedPeerCertificate)?;

    verify_node_certificate_der(leaf_der, trusted_ca_der, overlay, now)
}

pub fn verify_node_certificate_der(
    leaf_der: &[u8],
    trusted_ca_der: &[u8],
    overlay: Ipv4Net,
    now: OffsetDateTime,
) -> Result<VerifiedNodeCertificate, IdentityError> {
    verify_node_certificate_der_with_policy(
        leaf_der,
        trusted_ca_der,
        overlay,
        now,
        LeafValidityPolicy::RequireCurrent,
    )
}

/// Verifies a certificate retained in a durable enrollment result.
///
/// An expired leaf remains valid history for an idempotent enrollment replay,
/// but a leaf whose validity period has not started is rejected. All other
/// profile, identity, lifetime, and signature checks are identical to live
/// connection authentication.
pub fn verify_persisted_node_certificate_der(
    leaf_der: &[u8],
    trusted_ca_der: &[u8],
    overlay: Ipv4Net,
    now: OffsetDateTime,
) -> Result<VerifiedNodeCertificate, IdentityError> {
    verify_node_certificate_der_with_policy(
        leaf_der,
        trusted_ca_der,
        overlay,
        now,
        LeafValidityPolicy::AllowExpired,
    )
}

#[derive(Clone, Copy)]
enum LeafValidityPolicy {
    RequireCurrent,
    AllowExpired,
}

fn verify_node_certificate_der_with_policy(
    leaf_der: &[u8],
    trusted_ca_der: &[u8],
    overlay: Ipv4Net,
    now: OffsetDateTime,
    validity_policy: LeafValidityPolicy,
) -> Result<VerifiedNodeCertificate, IdentityError> {
    let authority = parse_strict_ca_certificate(trusted_ca_der, now)?;
    if leaf_der.is_empty() || leaf_der.len() > MAX_CERTIFICATE_BYTES as usize {
        return Err(IdentityError::InvalidNodeCertificate);
    }

    let (remainder, certificate) =
        parse_x509_certificate(leaf_der).map_err(|_| IdentityError::InvalidNodeCertificate)?;
    if !remainder.is_empty() || certificate.version() != X509Version::V3 {
        return Err(IdentityError::InvalidNodeCertificate);
    }

    let not_before = certificate.validity().not_before.to_datetime();
    let not_after = certificate.validity().not_after.to_datetime();
    if now < not_before
        || matches!(validity_policy, LeafValidityPolicy::RequireCurrent) && now >= not_after
    {
        return Err(IdentityError::NodeCertificateOutsideValidity);
    }
    if certificate.signature_algorithm.algorithm != OID_SIG_ECDSA_WITH_SHA256
        || certificate.tbs_certificate.signature.algorithm != OID_SIG_ECDSA_WITH_SHA256
        || certificate.tbs_certificate.signature != certificate.signature_algorithm
    {
        return Err(IdentityError::InvalidNodeCertificateProfile);
    }
    if not_after - not_before != NODE_CERTIFICATE_TTL {
        return Err(IdentityError::InvalidNodeCertificateProfile);
    }
    let public_key = SubjectPublicKeyInfo::from_der(certificate.public_key().raw)
        .map_err(|_| IdentityError::InvalidNodeCertificateProfile)?;
    if public_key.algorithm() != &PKCS_ECDSA_P256_SHA256 {
        return Err(IdentityError::InvalidNodeCertificateProfile);
    }

    let basic = certificate
        .basic_constraints()
        .map_err(|_| IdentityError::InvalidNodeCertificateProfile)?
        .ok_or(IdentityError::InvalidNodeCertificateProfile)?;
    let key_usage = certificate
        .key_usage()
        .map_err(|_| IdentityError::InvalidNodeCertificateProfile)?
        .ok_or(IdentityError::InvalidNodeCertificateProfile)?;
    let extended = certificate
        .extended_key_usage()
        .map_err(|_| IdentityError::InvalidNodeCertificateProfile)?
        .ok_or(IdentityError::InvalidNodeCertificateProfile)?;
    if !basic.critical
        || basic.value.ca
        || basic.value.path_len_constraint.is_some()
        || !key_usage.critical
        || key_usage.value.flags != 1
        || extended.critical
        || !extended.value.client_auth
        || !extended.value.server_auth
        || extended.value.any
        || extended.value.code_signing
        || extended.value.email_protection
        || extended.value.time_stamping
        || extended.value.ocsp_signing
        || !extended.value.other.is_empty()
    {
        return Err(IdentityError::InvalidNodeCertificateProfile);
    }

    let common_names = certificate
        .subject()
        .iter_common_name()
        .map(|name| name.as_str())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| IdentityError::InvalidNodeCertificateIdentity)?;
    let san = certificate
        .subject_alternative_name()
        .map_err(|_| IdentityError::InvalidNodeCertificateIdentity)?
        .ok_or(IdentityError::InvalidNodeCertificateIdentity)?;
    let node_id = common_names
        .first()
        .copied()
        .ok_or(IdentityError::InvalidNodeCertificateIdentity)?;
    validate_node_id(node_id).map_err(|_| IdentityError::InvalidNodeCertificateIdentity)?;
    let expected_uri = format!("{NODE_ID_URI_PREFIX}{node_id}");
    let mut uri_matches = 0usize;
    let mut overlay_ip = None;
    for name in &san.value.general_names {
        match name {
            GeneralName::URI(uri) if *uri == expected_uri => uri_matches += 1,
            GeneralName::IPAddress(bytes) if bytes.len() == 4 => {
                overlay_ip = Some(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]));
            }
            _ => {}
        }
    }
    let overlay_ip = overlay_ip.ok_or(IdentityError::InvalidNodeCertificateIdentity)?;
    validate_overlay_address(overlay, overlay_ip)
        .map_err(|_| IdentityError::InvalidOverlayAddress(overlay_ip))?;
    if certificate.subject().iter_attributes().count() != 1
        || common_names != [node_id]
        || san.critical
        || san.value.general_names.len() != 2
        || uri_matches != 1
    {
        return Err(IdentityError::NodeCertificateBindingMismatch);
    }

    let authority_key_identifier = validate_node_extensions(&certificate)?;
    let authority_subject_key_identifier = ca_subject_key_identifier(&authority)?;
    if certificate.issuer() != authority.subject()
        || not_before < authority.validity().not_before.to_datetime()
        || not_after > authority.validity().not_after.to_datetime()
        || authority_key_identifier != authority_subject_key_identifier
        || certificate
            .verify_signature(Some(authority.public_key()))
            .is_err()
    {
        return Err(IdentityError::UntrustedNodeCertificate);
    }

    Ok(VerifiedNodeCertificate {
        node_id: node_id.to_owned(),
        overlay_ip,
        fingerprint: certificate_fingerprint(leaf_der),
        spki_fingerprint: certificate_fingerprint(certificate.public_key().raw),
        not_before,
        not_after,
    })
}

pub fn csr_spki_fingerprint(csr_der: &[u8]) -> Result<String, IdentityError> {
    let _ = verified_csr_public_key(csr_der)?;
    let (_, csr) =
        X509CertificationRequest::from_der(csr_der).map_err(|_| IdentityError::InvalidCsr)?;
    Ok(certificate_fingerprint(
        csr.certification_request_info.subject_pki.raw,
    ))
}

#[cfg(test)]
fn authenticate_verified_node_certificate_der(
    leaf_der: &[u8],
    trusted_ca_der: &[u8],
    expected_node_id: &str,
    expected_overlay_ip: Ipv4Addr,
    overlay: Ipv4Net,
    now: OffsetDateTime,
) -> Result<VerifiedNodeCertificate, IdentityError> {
    let verified = verify_node_certificate_der(leaf_der, trusted_ca_der, overlay, now)?;
    if verified.node_id != expected_node_id || verified.overlay_ip != expected_overlay_ip {
        return Err(IdentityError::NodeCertificateBindingMismatch);
    }
    Ok(verified)
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error(
        "{operation} is unsupported on this platform until secret-file ACL handling is implemented"
    )]
    UnsupportedPlatform { operation: &'static str },
    #[error("identity paths resolve to the same target: {0}")]
    ConflictingPaths(PathBuf),
    #[error("certificate authority is not initialized")]
    CaNotInitialized,
    #[error("certificate authority is already initialized")]
    CaAlreadyInitialized,
    #[error(
        "incomplete certificate authority state (certificate exists: {certificate_exists}, key exists: {key_exists})"
    )]
    IncompleteCaState {
        certificate_exists: bool,
        key_exists: bool,
    },
    #[error("identity file is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("secret file {path} grants group or other permissions (mode {mode:#o})")]
    InsecurePermissions { path: PathBuf, mode: u32 },
    #[error("secret file {path} is owned by uid {owner}, expected effective uid {expected}")]
    UnexpectedOwner {
        path: PathBuf,
        owner: u32,
        expected: u32,
    },
    #[error("identity file {path} exceeds the {maximum}-byte limit")]
    FileTooLarge { path: PathBuf, maximum: u64 },
    #[error("invalid P-256 PKCS#8 private key: {0}")]
    InvalidPrivateKey(PathBuf),
    #[error("invalid certificate authority certificate")]
    InvalidCaCertificate,
    #[error("certificate authority key does not match its certificate")]
    CaKeyMismatch,
    #[error("certificate authority validity does not cover the requested leaf lifetime")]
    CaOutsideIssuanceWindow,
    #[error("invalid certificate signing request")]
    InvalidCsr,
    #[error("certificate signing request must use P-256 with ECDSA-SHA256")]
    UnsupportedCsrAlgorithm,
    #[error("invalid node ID")]
    InvalidNodeId,
    #[error("invalid overlay IPv4 address {0}")]
    InvalidOverlayAddress(Ipv4Addr),
    #[error("invalid node certificate DER")]
    InvalidNodeCertificate,
    #[error("node certificate is not currently valid")]
    NodeCertificateOutsideValidity,
    #[error("node certificate does not use the required P-256 mTLS profile")]
    InvalidNodeCertificateProfile,
    #[error("node certificate contains an invalid identity extension")]
    InvalidNodeCertificateIdentity,
    #[error("node certificate does not match the static node/address binding")]
    NodeCertificateBindingMismatch,
    #[error("node certificate was not directly signed by the trusted coordination CA")]
    UntrustedNodeCertificate,
    #[error("TLS did not provide a verified peer certificate")]
    MissingVerifiedPeerCertificate,
    #[error("{operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cryptographic operation failed while attempting to {context}: {source}")]
    Crypto {
        context: &'static str,
        #[source]
        source: rcgen::Error,
    },
}

fn load_node_key(path: &Path) -> Result<NodeKey, IdentityError> {
    let bytes = read_regular_file(path, true, MAX_PRIVATE_KEY_BYTES)?;
    Ok(NodeKey {
        key_pair: parse_p256_key(&bytes, path)?,
    })
}

fn parse_p256_key(bytes: &[u8], path: &Path) -> Result<KeyPair, IdentityError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| IdentityError::InvalidPrivateKey(path.to_path_buf()))?;
    let blocks =
        pem::parse_many(bytes).map_err(|_| IdentityError::InvalidPrivateKey(path.to_path_buf()))?;
    if blocks.len() != 1 || blocks[0].tag() != "PRIVATE KEY" {
        return Err(IdentityError::InvalidPrivateKey(path.to_path_buf()));
    }
    KeyPair::from_pem_and_sign_algo(text, &PKCS_ECDSA_P256_SHA256)
        .map_err(|_| IdentityError::InvalidPrivateKey(path.to_path_buf()))
}

fn parse_ca_pem(bytes: &[u8]) -> Result<Vec<u8>, IdentityError> {
    let blocks = match pem::parse_many(bytes) {
        Ok(blocks) => blocks,
        Err(_) => return Err(IdentityError::InvalidCaCertificate),
    };
    if blocks.len() != 1 || blocks[0].tag() != "CERTIFICATE" {
        return Err(IdentityError::InvalidCaCertificate);
    }
    Ok(blocks[0].contents().to_vec())
}

fn parse_strict_ca_certificate(
    certificate_der: &[u8],
    now: OffsetDateTime,
) -> Result<X509Certificate<'_>, IdentityError> {
    if certificate_der.is_empty() || certificate_der.len() > MAX_CERTIFICATE_BYTES as usize {
        return Err(IdentityError::InvalidCaCertificate);
    }
    let (remainder, certificate) =
        parse_x509_certificate(certificate_der).map_err(|_| IdentityError::InvalidCaCertificate)?;
    if !remainder.is_empty()
        || certificate.version() != X509Version::V3
        || certificate.subject() != certificate.issuer()
        || certificate.tbs_certificate.issuer_uid.is_some()
        || certificate.tbs_certificate.subject_uid.is_some()
    {
        return Err(IdentityError::InvalidCaCertificate);
    }

    let not_before = certificate.validity().not_before.to_datetime();
    let not_after = certificate.validity().not_after.to_datetime();
    if now < not_before || now >= not_after {
        return Err(IdentityError::InvalidCaCertificate);
    }
    if certificate.signature_algorithm.algorithm != OID_SIG_ECDSA_WITH_SHA256
        || certificate.tbs_certificate.signature.algorithm != OID_SIG_ECDSA_WITH_SHA256
        || certificate.tbs_certificate.signature != certificate.signature_algorithm
    {
        return Err(IdentityError::InvalidCaCertificate);
    }
    let public_key = SubjectPublicKeyInfo::from_der(certificate.public_key().raw)
        .map_err(|_| IdentityError::InvalidCaCertificate)?;
    if public_key.algorithm() != &PKCS_ECDSA_P256_SHA256 {
        return Err(IdentityError::InvalidCaCertificate);
    }

    let common_names = certificate
        .subject()
        .iter_common_name()
        .map(|name| name.as_str())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| IdentityError::InvalidCaCertificate)?;
    if certificate.subject().iter_attributes().count() != 1 || common_names != [CA_COMMON_NAME] {
        return Err(IdentityError::InvalidCaCertificate);
    }

    let basic_constraints = certificate
        .basic_constraints()
        .map_err(|_| IdentityError::InvalidCaCertificate)?
        .ok_or(IdentityError::InvalidCaCertificate)?;
    let key_usage = certificate
        .key_usage()
        .map_err(|_| IdentityError::InvalidCaCertificate)?
        .ok_or(IdentityError::InvalidCaCertificate)?;
    if !basic_constraints.critical
        || !basic_constraints.value.ca
        || basic_constraints.value.path_len_constraint != Some(0)
        || !key_usage.critical
        || key_usage.value.flags != CA_KEY_USAGE_FLAGS
    {
        return Err(IdentityError::InvalidCaCertificate);
    }

    let _ = ca_subject_key_identifier(&certificate)?;
    certificate
        .verify_signature(None)
        .map_err(|_| IdentityError::InvalidCaCertificate)?;
    Ok(certificate)
}

fn ca_subject_key_identifier(certificate: &X509Certificate<'_>) -> Result<Vec<u8>, IdentityError> {
    certificate
        .extensions_map()
        .map_err(|_| IdentityError::InvalidCaCertificate)?;
    if certificate.extensions().len() != CA_EXTENSION_COUNT {
        return Err(IdentityError::InvalidCaCertificate);
    }

    let mut authority_key_identifier = None;
    let mut subject_key_identifier = None;
    for extension in certificate.extensions() {
        match extension.parsed_extension() {
            ParsedExtension::AuthorityKeyIdentifier(identifier)
                if !extension.critical
                    && identifier.authority_cert_issuer.is_none()
                    && identifier.authority_cert_serial.is_none() =>
            {
                authority_key_identifier = identifier
                    .key_identifier
                    .as_ref()
                    .map(|identifier| identifier.0.to_vec());
            }
            ParsedExtension::SubjectKeyIdentifier(identifier) if !extension.critical => {
                subject_key_identifier = Some(identifier.0.to_vec());
            }
            ParsedExtension::KeyUsage(_) | ParsedExtension::BasicConstraints(_)
                if extension.critical => {}
            _ => return Err(IdentityError::InvalidCaCertificate),
        }
    }

    let authority_key_identifier =
        authority_key_identifier.ok_or(IdentityError::InvalidCaCertificate)?;
    let subject_key_identifier =
        subject_key_identifier.ok_or(IdentityError::InvalidCaCertificate)?;
    if authority_key_identifier.is_empty() || authority_key_identifier != subject_key_identifier {
        return Err(IdentityError::InvalidCaCertificate);
    }
    Ok(subject_key_identifier)
}

fn validate_node_extensions(certificate: &X509Certificate<'_>) -> Result<Vec<u8>, IdentityError> {
    certificate
        .extensions_map()
        .map_err(|_| IdentityError::InvalidNodeCertificateProfile)?;
    if certificate.extensions().len() != NODE_EXTENSION_COUNT {
        return Err(IdentityError::InvalidNodeCertificateProfile);
    }

    let mut authority_key_identifier = None;
    let mut subject_key_identifier = None;
    for extension in certificate.extensions() {
        match extension.parsed_extension() {
            ParsedExtension::AuthorityKeyIdentifier(identifier)
                if !extension.critical
                    && identifier.authority_cert_issuer.is_none()
                    && identifier.authority_cert_serial.is_none() =>
            {
                authority_key_identifier = identifier
                    .key_identifier
                    .as_ref()
                    .map(|identifier| identifier.0.to_vec());
            }
            ParsedExtension::SubjectKeyIdentifier(identifier) if !extension.critical => {
                subject_key_identifier = Some(identifier.0.to_vec());
            }
            ParsedExtension::KeyUsage(_) | ParsedExtension::BasicConstraints(_)
                if extension.critical => {}
            ParsedExtension::ExtendedKeyUsage(_) | ParsedExtension::SubjectAlternativeName(_)
                if !extension.critical => {}
            _ => return Err(IdentityError::InvalidNodeCertificateProfile),
        }
    }

    let authority_key_identifier =
        authority_key_identifier.ok_or(IdentityError::InvalidNodeCertificateProfile)?;
    let subject_key_identifier =
        subject_key_identifier.ok_or(IdentityError::InvalidNodeCertificateProfile)?;
    if authority_key_identifier.is_empty() || subject_key_identifier.is_empty() {
        return Err(IdentityError::InvalidNodeCertificateProfile);
    }
    Ok(authority_key_identifier)
}

fn validate_ca_certificate(
    certificate_der: &[u8],
    key_pair: &KeyPair,
) -> Result<(OffsetDateTime, OffsetDateTime), IdentityError> {
    let certificate = parse_strict_ca_certificate(certificate_der, OffsetDateTime::now_utc())?;
    if certificate.public_key().raw != key_pair.public_key_der().as_slice() {
        return Err(IdentityError::CaKeyMismatch);
    }

    Ok((
        certificate.validity().not_before.to_datetime(),
        certificate.validity().not_after.to_datetime(),
    ))
}

fn verified_csr_public_key(csr_der: &[u8]) -> Result<SubjectPublicKeyInfo, IdentityError> {
    if csr_der.is_empty() || csr_der.len() > MAX_CSR_BYTES {
        return Err(IdentityError::InvalidCsr);
    }
    let (remainder, csr) =
        X509CertificationRequest::from_der(csr_der).map_err(|_| IdentityError::InvalidCsr)?;
    if !remainder.is_empty() {
        return Err(IdentityError::InvalidCsr);
    }
    if csr.signature_algorithm.algorithm != OID_SIG_ECDSA_WITH_SHA256 {
        return Err(IdentityError::UnsupportedCsrAlgorithm);
    }

    let public_key = SubjectPublicKeyInfo::from_der(csr.certification_request_info.subject_pki.raw)
        .map_err(|_| IdentityError::UnsupportedCsrAlgorithm)?;
    if public_key.algorithm() != &PKCS_ECDSA_P256_SHA256 {
        return Err(IdentityError::UnsupportedCsrAlgorithm);
    }
    csr.verify_signature()
        .map_err(|_| IdentityError::InvalidCsr)?;
    Ok(public_key)
}

fn certificate_fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let mut output = String::with_capacity("sha256:".len() + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        use fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn random_serial_number() -> SerialNumber {
    let mut bytes = [0_u8; 20];
    OsRng.fill_bytes(&mut bytes);
    bytes[0] &= 0x7f;
    if bytes.iter().all(|byte| *byte == 0) {
        bytes[19] = 1;
    }
    SerialNumber::from_slice(&bytes)
}

fn randomized_renewal_time(
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
) -> OffsetDateTime {
    let midpoint = not_before + (not_after - not_before) / 2;
    let jitter_seconds = CERTIFICATE_RENEWAL_JITTER.whole_seconds();
    midpoint + Duration::seconds(OsRng.gen_range(-jitter_seconds..=jitter_seconds))
}

fn whole_seconds(value: OffsetDateTime) -> OffsetDateTime {
    value
        .replace_nanosecond(0)
        .expect("zero is a valid nanosecond")
}

fn ensure_persistence_supported(operation: &'static str) -> Result<(), IdentityError> {
    #[cfg(unix)]
    {
        let _ = operation;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err(IdentityError::UnsupportedPlatform { operation })
    }
}

fn path_exists(path: &Path) -> Result<bool, IdentityError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect", path, source)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallOutcome {
    Installed,
    Existing,
}

#[cfg(unix)]
fn write_atomic_new(
    path: &Path,
    contents: &[u8],
    mode: u32,
) -> Result<InstallOutcome, IdentityError> {
    let parent = normalized_parent(path)?;
    let mut temporary = None;
    for _ in 0..TEMP_FILE_ATTEMPTS {
        let mut random = [0_u8; 12];
        OsRng.fill_bytes(&mut random);
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let candidate = parent.join(format!(".stellaris-{}-{suffix}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(mode);
        match options.open(&candidate) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(io_error(
                    "create temporary identity file",
                    &candidate,
                    source,
                ));
            }
        }
    }
    let Some((temporary_path, mut file)) = temporary else {
        return Err(io_error(
            "create temporary identity file",
            path,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "temporary name attempts exhausted",
            ),
        ));
    };

    if let Err(source) = file
        .set_permissions(fs::Permissions::from_mode(mode))
        .and_then(|()| file.write_all(contents))
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&temporary_path);
        return Err(io_error("write temporary identity file", path, source));
    }
    drop(file);

    let outcome = match fs::hard_link(&temporary_path, path) {
        Ok(()) => InstallOutcome::Installed,
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => InstallOutcome::Existing,
        Err(source) => {
            let _ = fs::remove_file(&temporary_path);
            return Err(io_error("install identity file", path, source));
        }
    };
    if let Err(source) = fs::remove_file(&temporary_path) {
        if outcome == InstallOutcome::Installed {
            let _ = fs::remove_file(path);
            let _ = sync_directory(parent);
        }
        return Err(io_error(
            "remove temporary identity file",
            &temporary_path,
            source,
        ));
    }
    if outcome == InstallOutcome::Installed
        && let Err(error) = sync_directory(parent)
    {
        let _ = fs::remove_file(path);
        let _ = sync_directory(parent);
        return Err(error);
    }
    Ok(outcome)
}

#[cfg(not(unix))]
fn write_atomic_new(
    _path: &Path,
    _contents: &[u8],
    _mode: u32,
) -> Result<InstallOutcome, IdentityError> {
    Err(IdentityError::UnsupportedPlatform {
        operation: "persist identity material",
    })
}

#[cfg(unix)]
fn read_regular_file(path: &Path, secret: bool, maximum: u64) -> Result<Vec<u8>, IdentityError> {
    read_regular_file_for_uid(path, secret, maximum, effective_user_id())
}

#[cfg(unix)]
fn read_regular_file_for_uid(
    path: &Path,
    secret: bool,
    maximum: u64,
    effective_uid: u32,
) -> Result<Vec<u8>, IdentityError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(IdentityError::NotRegularFile(path.to_path_buf()));
    }

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options
        .open(path)
        .map_err(|source| io_error("open identity file", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect open identity file", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(IdentityError::NotRegularFile(path.to_path_buf()));
    }
    if secret {
        let owner = metadata.uid();
        if owner != effective_uid {
            return Err(IdentityError::UnexpectedOwner {
                path: path.to_path_buf(),
                owner,
                expected: effective_uid,
            });
        }
        let mode = metadata.permissions().mode() & 0o7777;
        if mode & 0o077 != 0 {
            return Err(IdentityError::InsecurePermissions {
                path: path.to_path_buf(),
                mode,
            });
        }
    }
    if metadata.len() > maximum {
        return Err(IdentityError::FileTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read identity file", path, source))?;
    if bytes.len() as u64 > maximum {
        return Err(IdentityError::FileTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    Ok(bytes)
}

#[cfg(unix)]
fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn read_regular_file(_path: &Path, _secret: bool, _maximum: u64) -> Result<Vec<u8>, IdentityError> {
    Err(IdentityError::UnsupportedPlatform {
        operation: "read identity material",
    })
}

#[cfg(unix)]
fn normalized_parent(path: &Path) -> Result<&Path, IdentityError> {
    if path.file_name().is_none() {
        return Err(io_error(
            "resolve identity path",
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"),
        ));
    }
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Ok(parent),
        Some(_) => Ok(Path::new(".")),
        None => Err(io_error(
            "resolve identity path",
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"),
        )),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), IdentityError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("sync identity directory", path, source))
}

#[cfg(unix)]
fn rollback_created_identity_files(paths: &[&Path]) -> Result<(), IdentityError> {
    let mut parents = Vec::new();
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error("roll back identity file", path, source)),
        }
        let parent = normalized_parent(path)?;
        if !parents.contains(&parent) {
            parents.push(parent);
        }
    }
    for parent in parents {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn rollback_created_identity_files(_paths: &[&Path]) -> Result<(), IdentityError> {
    Err(IdentityError::UnsupportedPlatform {
        operation: "roll back identity material",
    })
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> IdentityError {
    IdentityError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        protocol::{CONTROL_ALPN, RELAY_ALPN},
        transport::{
            BoxReadStream, BoxWriteStream, ProtocolPurpose, TransportConnection, TransportError,
            authenticate_connection,
        },
    };
    use bytes::Bytes;
    use std::{
        net::{IpAddr, SocketAddr},
        os::unix::fs::symlink,
        sync::{Arc, Barrier},
        thread,
    };
    use x509_parser::extensions::GeneralName;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let mut random = [0_u8; 8];
            OsRng.fill_bytes(&mut random);
            let path = std::env::temp_dir().join(format!(
                "stellaris-identity-{name}-{}-{}",
                std::process::id(),
                u64::from_ne_bytes(random)
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_with_mode(path: &Path, contents: impl AsRef<[u8]>, mode: u32) {
        fs::write(path, contents).expect("write fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set fixture mode");
    }

    fn create_ca(dir: &TestDir) -> (CertificateAuthority, PathBuf, PathBuf) {
        let certificate_path = dir.join("ca.pem");
        let key_path = dir.join("ca-key.pem");
        let authority = CertificateAuthority::create(&certificate_path, &key_path)
            .expect("create certificate authority");
        (authority, certificate_path, key_path)
    }

    fn test_overlay() -> Ipv4Net {
        "10.88.0.0/24".parse().expect("valid test overlay")
    }

    struct TestPeerConnection {
        alpn: Option<Vec<u8>>,
        chain: Option<Vec<Vec<u8>>>,
    }

    impl crate::transport::sealed::Sealed for TestPeerConnection {}

    #[async_trait::async_trait]
    impl TransportConnection for TestPeerConnection {
        async fn open_bi(&self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
            Err(TransportError::ConnectionClosed)
        }

        async fn accept_bi(&mut self) -> Result<(BoxReadStream, BoxWriteStream), TransportError> {
            Err(TransportError::ConnectionClosed)
        }

        fn send_datagram(&self, _packet: Bytes) -> Result<(), TransportError> {
            Err(TransportError::ConnectionClosed)
        }

        async fn recv_datagram(&mut self) -> Result<Bytes, TransportError> {
            Err(TransportError::ConnectionClosed)
        }

        fn remote_address(&self) -> SocketAddr {
            SocketAddr::from(([127, 0, 0, 1], 1))
        }

        async fn negotiated_alpn(&self) -> Result<Vec<u8>, TransportError> {
            self.alpn
                .clone()
                .ok_or(TransportError::MissingHandshakeMetadata("ALPN"))
        }

        async fn peer_certificate_chain_der(&self) -> Result<Vec<Vec<u8>>, TransportError> {
            Ok(self.chain.clone().unwrap_or_default())
        }

        fn close(&self, _code: u32, _reason: &[u8]) {}

        async fn closed(&self) -> TransportError {
            TransportError::ConnectionClosed
        }
    }

    #[test]
    fn node_key_is_private_and_reused() {
        let dir = TestDir::new("node-key-reuse");
        let path = dir.join("node-key.pem");
        let first = NodeKey::load_or_create(&path).expect("create key");
        let second = NodeKey::load_or_create(&path).expect("load key");

        assert_eq!(first.public_key_der(), second.public_key_der());
        assert_eq!(
            fs::metadata(path)
                .expect("key metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn concurrent_node_key_creation_converges_on_one_key() {
        let dir = TestDir::new("node-key-race");
        let path = Arc::new(dir.join("node-key.pem"));
        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    NodeKey::load_or_create(path.as_ref()).map(|key| key.public_key_der())
                })
            })
            .collect::<Vec<_>>();
        let keys = threads
            .into_iter()
            .map(|thread| thread.join().expect("thread completes").expect("load key"))
            .collect::<Vec<_>>();

        assert!(keys.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn invalid_existing_node_keys_fail_without_replacement() {
        let dir = TestDir::new("invalid-node-keys");

        let corrupt = dir.join("corrupt.pem");
        write_with_mode(&corrupt, b"not a private key", 0o600);
        let before = fs::read(&corrupt).expect("read corrupt fixture");
        assert!(matches!(
            NodeKey::load_or_create(&corrupt),
            Err(IdentityError::InvalidPrivateKey(_))
        ));
        assert_eq!(fs::read(&corrupt).expect("reread fixture"), before);

        let permissive = dir.join("permissive.pem");
        let key = KeyPair::generate().expect("generate fixture key");
        write_with_mode(&permissive, key.serialize_pem(), 0o644);
        assert!(matches!(
            NodeKey::load_or_create(&permissive),
            Err(IdentityError::InsecurePermissions { .. })
        ));

        let target = dir.join("target.pem");
        write_with_mode(&target, key.serialize_pem(), 0o600);
        let link = dir.join("link.pem");
        symlink(&target, &link).expect("create symlink fixture");
        assert!(matches!(
            NodeKey::load_or_create(&link),
            Err(IdentityError::NotRegularFile(_))
        ));

        let directory = dir.join("directory");
        fs::create_dir(&directory).expect("create directory fixture");
        assert!(matches!(
            NodeKey::load_or_create(&directory),
            Err(IdentityError::NotRegularFile(_))
        ));

        let wrong_owner = dir.join("wrong-owner.pem");
        write_with_mode(&wrong_owner, key.serialize_pem(), 0o600);
        let actual_uid = effective_user_id();
        let different_uid = actual_uid.wrapping_add(1);
        assert!(matches!(
            read_regular_file_for_uid(
                &wrong_owner,
                true,
                MAX_PRIVATE_KEY_BYTES,
                different_uid,
            ),
            Err(IdentityError::UnexpectedOwner {
                owner,
                expected,
                ..
            }) if owner == actual_uid && expected == different_uid
        ));
    }

    #[test]
    fn ca_create_then_load_preserves_the_trust_root() {
        let dir = TestDir::new("ca-reuse");
        let (created, certificate_path, key_path) = create_ca(&dir);
        let loaded = CertificateAuthority::load(&certificate_path, &key_path).expect("load CA");

        assert_eq!(created.certificate_der(), loaded.certificate_der());
        assert_eq!(created.certificate_pem(), loaded.certificate_pem());
        assert!(matches!(
            CertificateAuthority::create(&certificate_path, &key_path),
            Err(IdentityError::CaAlreadyInitialized)
        ));
        assert_eq!(
            fs::metadata(key_path)
                .expect("CA key metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn ca_creation_failure_rolls_back_the_first_installed_file() {
        let dir = TestDir::new("ca-transaction-rollback");
        let certificate_parent = dir.join("certificate-parent");
        let certificate_path = certificate_parent.join("ca.pem");
        let key_path = dir.join("ca-key.pem");

        assert!(matches!(
            CertificateAuthority::create(&certificate_path, &key_path),
            Err(IdentityError::Io { .. })
        ));
        assert!(!certificate_path.exists());
        assert!(!key_path.exists());

        fs::create_dir(&certificate_parent).expect("create certificate parent for retry");
        CertificateAuthority::create(&certificate_path, &key_path)
            .expect("retry CA creation after rollback");
        assert!(certificate_path.exists());
        assert!(key_path.exists());
    }

    #[test]
    fn ca_missing_and_partial_states_fail_closed() {
        let missing = TestDir::new("ca-missing");
        assert!(matches!(
            CertificateAuthority::load(missing.join("ca.pem"), missing.join("key.pem")),
            Err(IdentityError::CaNotInitialized)
        ));

        let key_only = TestDir::new("ca-key-only");
        let key = KeyPair::generate().expect("generate CA key fixture");
        write_with_mode(&key_only.join("key.pem"), key.serialize_pem(), 0o600);
        assert!(matches!(
            CertificateAuthority::load(key_only.join("ca.pem"), key_only.join("key.pem")),
            Err(IdentityError::IncompleteCaState {
                certificate_exists: false,
                key_exists: true
            })
        ));

        let certificate_only = TestDir::new("ca-certificate-only");
        let source = TestDir::new("ca-source");
        let (_, source_certificate, _) = create_ca(&source);
        fs::copy(source_certificate, certificate_only.join("ca.pem"))
            .expect("copy CA certificate fixture");
        assert!(matches!(
            CertificateAuthority::load(
                certificate_only.join("ca.pem"),
                certificate_only.join("key.pem")
            ),
            Err(IdentityError::IncompleteCaState {
                certificate_exists: true,
                key_exists: false
            })
        ));
    }

    #[test]
    fn ca_corruption_invalid_flags_and_key_mismatch_fail_closed() {
        let corrupt_certificate = TestDir::new("ca-corrupt-certificate");
        let (_, certificate_path, key_path) = create_ca(&corrupt_certificate);
        write_with_mode(&certificate_path, b"not a certificate", 0o644);
        assert!(matches!(
            CertificateAuthority::load(&certificate_path, &key_path),
            Err(IdentityError::InvalidCaCertificate)
        ));

        let corrupt_key = TestDir::new("ca-corrupt-key");
        let (_, certificate_path, key_path) = create_ca(&corrupt_key);
        write_with_mode(&key_path, b"not a key", 0o600);
        assert!(matches!(
            CertificateAuthority::load(&certificate_path, &key_path),
            Err(IdentityError::InvalidPrivateKey(_))
        ));

        let mismatch_a = TestDir::new("ca-mismatch-a");
        let mismatch_b = TestDir::new("ca-mismatch-b");
        let (_, certificate_a, key_a) = create_ca(&mismatch_a);
        let (_, _, key_b) = create_ca(&mismatch_b);
        fs::copy(key_b, &key_a).expect("replace CA key with a different key");
        assert!(matches!(
            CertificateAuthority::load(certificate_a, key_a),
            Err(IdentityError::CaKeyMismatch)
        ));

        let not_a_ca = TestDir::new("not-a-ca");
        let key = KeyPair::generate().expect("generate leaf key");
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let certificate = params.self_signed(&key).expect("create leaf fixture");
        write_with_mode(&not_a_ca.join("certificate.pem"), certificate.pem(), 0o644);
        write_with_mode(&not_a_ca.join("key.pem"), key.serialize_pem(), 0o600);
        assert!(matches!(
            CertificateAuthority::load(not_a_ca.join("certificate.pem"), not_a_ca.join("key.pem")),
            Err(IdentityError::InvalidCaCertificate)
        ));

        let unconstrained_ca = TestDir::new("unconstrained-ca");
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("generate CA key");
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, CA_COMMON_NAME);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.use_authority_key_identifier_extension = true;
        let certificate = params.self_signed(&key).expect("create loose CA fixture");
        write_with_mode(
            &unconstrained_ca.join("certificate.pem"),
            certificate.pem(),
            0o644,
        );
        write_with_mode(
            &unconstrained_ca.join("key.pem"),
            key.serialize_pem(),
            0o600,
        );
        assert!(matches!(
            CertificateAuthority::load(
                unconstrained_ca.join("certificate.pem"),
                unconstrained_ca.join("key.pem"),
            ),
            Err(IdentityError::InvalidCaCertificate)
        ));
    }

    #[test]
    fn csr_requires_valid_p256_proof_of_possession() {
        let dir = TestDir::new("csr-validation");
        let (authority, _, _) = create_ca(&dir);
        let key_path = dir.join("node-key.pem");
        let node_key = NodeKey::load_or_create(key_path).expect("create node key");
        let csr = node_key.create_csr_der().expect("create CSR");
        authority
            .issue_node_certificate(&csr, "edge-a", test_overlay(), Ipv4Addr::new(10, 88, 0, 2))
            .expect("issue from valid CSR");

        let mut tampered = csr;
        *tampered.last_mut().expect("CSR has signature bytes") ^= 1;
        assert!(matches!(
            authority.issue_node_certificate(
                &tampered,
                "edge-a",
                test_overlay(),
                Ipv4Addr::new(10, 88, 0, 2)
            ),
            Err(IdentityError::InvalidCsr)
        ));

        let p384 = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384)
            .expect("generate P-384 fixture key");
        let request = CertificateParams::default()
            .serialize_request(&p384)
            .expect("create P-384 CSR");
        assert!(matches!(
            authority.issue_node_certificate(
                request.der().as_ref(),
                "edge-a",
                test_overlay(),
                Ipv4Addr::new(10, 88, 0, 2)
            ),
            Err(IdentityError::UnsupportedCsrAlgorithm)
        ));

        for invalid_address in [
            Ipv4Addr::new(10, 88, 0, 0),
            Ipv4Addr::new(10, 88, 0, 255),
            Ipv4Addr::new(10, 99, 0, 2),
        ] {
            assert!(matches!(
                authority.issue_node_certificate(
                    node_key.create_csr_der().expect("create address-check CSR").as_ref(),
                    "edge-a",
                    test_overlay(),
                    invalid_address,
                ),
                Err(IdentityError::InvalidOverlayAddress(address)) if address == invalid_address
            ));
        }
    }

    #[test]
    fn issuer_overrides_csr_identity_and_privilege_requests() {
        let dir = TestDir::new("csr-override");
        let (authority, _, _) = create_ca(&dir);
        let subject_key = KeyPair::generate().expect("generate subject key");
        let mut request_params = CertificateParams::default();
        request_params.distinguished_name = DistinguishedName::new();
        request_params
            .distinguished_name
            .push(DnType::CommonName, "attacker-controlled-name");
        request_params.subject_alt_names = vec![
            SanType::DnsName("attacker.invalid".try_into().expect("valid DNS name")),
            SanType::URI("urn:stellaris:node:attacker".try_into().expect("valid URI")),
            SanType::IpAddress(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 99))),
        ];
        request_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        request_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::CodeSigning];
        request_params
            .custom_extensions
            .push(rcgen::CustomExtension::from_oid_content(
                &[2, 5, 29, 19],
                vec![0x30, 0x03, 0x01, 0x01, 0xff],
            ));
        let request = request_params
            .serialize_request(&subject_key)
            .expect("create privileged CSR fixture");

        let overlay_ip = Ipv4Addr::new(10, 88, 0, 42);
        let issued = authority
            .issue_node_certificate(
                request.der().as_ref(),
                "edge-42",
                test_overlay(),
                overlay_ip,
            )
            .expect("issue controlled certificate");
        let (remainder, certificate) =
            parse_x509_certificate(issued.der()).expect("parse issued certificate");
        assert!(remainder.is_empty());
        certificate
            .verify_signature(Some(
                parse_x509_certificate(authority.certificate_der())
                    .expect("parse CA certificate")
                    .1
                    .public_key(),
            ))
            .expect("verify issued certificate");

        let common_names = certificate
            .subject()
            .iter_common_name()
            .map(|name| name.as_str().expect("UTF-8 common name"))
            .collect::<Vec<_>>();
        assert_eq!(common_names, vec!["edge-42"]);

        let san = certificate
            .subject_alternative_name()
            .expect("valid SAN extension")
            .expect("SAN is present");
        assert_eq!(san.value.general_names.len(), 2);
        assert!(
            san.value
                .general_names
                .contains(&GeneralName::URI("urn:stellaris:node:edge-42"))
        );
        assert!(
            san.value
                .general_names
                .contains(&GeneralName::IPAddress(&overlay_ip.octets()))
        );

        let basic = certificate
            .basic_constraints()
            .expect("valid basic constraints")
            .expect("basic constraints are present");
        assert!(!basic.value.ca);

        let usage = certificate
            .key_usage()
            .expect("valid key usage")
            .expect("key usage is present");
        assert_eq!(usage.value.flags, 1);

        let extended = certificate
            .extended_key_usage()
            .expect("valid extended key usage")
            .expect("extended key usage is present");
        assert!(extended.value.client_auth);
        assert!(extended.value.server_auth);
        assert!(!extended.value.any);
        assert!(!extended.value.code_signing);
        assert!(!extended.value.email_protection);
        assert!(!extended.value.time_stamping);
        assert!(!extended.value.ocsp_signing);
        assert!(extended.value.other.is_empty());

        assert_eq!(
            certificate.validity().not_after.timestamp()
                - certificate.validity().not_before.timestamp(),
            NODE_CERTIFICATE_TTL.whole_seconds()
        );
        assert_eq!(
            issued.not_after() - issued.not_before(),
            NODE_CERTIFICATE_TTL
        );
        let renewal_midpoint = issued.not_before() + NODE_CERTIFICATE_TTL / 2;
        assert!(
            issued.renew_after() >= renewal_midpoint - CERTIFICATE_RENEWAL_JITTER
                && issued.renew_after() <= renewal_midpoint + CERTIFICATE_RENEWAL_JITTER
        );
        assert_eq!(
            issued.fingerprint(),
            format!("sha256:{:x}", Sha256::digest(issued.der()))
        );
        assert!(
            issued
                .fingerprint()
                .strip_prefix("sha256:")
                .is_some_and(|hex| hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        );
        let debug = format!("{issued:?}");
        assert!(debug.contains(issued.fingerprint()));
        assert!(!debug.contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn verified_leaf_identity_is_bound_to_the_static_registry() {
        let dir = TestDir::new("verified-leaf-binding");
        let (authority, _, _) = create_ca(&dir);
        let node_key = NodeKey::load_or_create(dir.join("node-key.pem")).expect("create node key");
        let overlay_ip = Ipv4Addr::new(10, 88, 0, 42);
        let issued = authority
            .issue_node_certificate(
                &node_key.create_csr_der().expect("create CSR"),
                "edge-42",
                test_overlay(),
                overlay_ip,
            )
            .expect("issue node certificate");
        let identity = authenticate_verified_node_certificate_der(
            issued.der(),
            authority.certificate_der(),
            "edge-42",
            overlay_ip,
            test_overlay(),
            issued.not_before() + Duration::seconds(1),
        )
        .expect("authenticate verified leaf");

        assert_eq!(identity.node_id(), "edge-42");
        assert_eq!(identity.overlay_ip(), overlay_ip);
        assert_eq!(identity.fingerprint(), issued.fingerprint());
        assert_eq!(identity.not_before(), issued.not_before());
        assert_eq!(identity.not_after(), issued.not_after());

        for (node_id, address) in [
            ("edge-other", overlay_ip),
            ("edge-42", Ipv4Addr::new(10, 88, 0, 43)),
        ] {
            assert!(matches!(
                authenticate_verified_node_certificate_der(
                    issued.der(),
                    authority.certificate_der(),
                    node_id,
                    address,
                    test_overlay(),
                    issued.not_before() + Duration::seconds(1),
                ),
                Err(IdentityError::NodeCertificateBindingMismatch)
            ));
        }
        assert!(matches!(
            authenticate_verified_node_certificate_der(
                issued.der(),
                authority.certificate_der(),
                "edge-42",
                overlay_ip,
                test_overlay(),
                issued.not_after(),
            ),
            Err(IdentityError::NodeCertificateOutsideValidity)
        ));

        let mut trailing_der = issued.der().to_vec();
        trailing_der.push(0);
        assert!(matches!(
            authenticate_verified_node_certificate_der(
                &trailing_der,
                authority.certificate_der(),
                "edge-42",
                overlay_ip,
                test_overlay(),
                issued.not_before() + Duration::seconds(1),
            ),
            Err(IdentityError::InvalidNodeCertificate)
        ));
    }

    #[test]
    fn persisted_leaf_validation_accepts_expired_history_but_not_future_certificates() {
        let dir = TestDir::new("persisted-leaf-validity");
        let (authority, _, _) = create_ca(&dir);
        let node_key = NodeKey::load_or_create(dir.join("node-key.pem")).expect("create node key");
        let issued = authority
            .issue_node_certificate(
                &node_key.create_csr_der().expect("create CSR"),
                "edge-42",
                test_overlay(),
                Ipv4Addr::new(10, 88, 0, 42),
            )
            .expect("issue node certificate");

        verify_persisted_node_certificate_der(
            issued.der(),
            authority.certificate_der(),
            test_overlay(),
            issued.not_after() + Duration::seconds(1),
        )
        .expect("expired enrollment result remains valid history");
        assert!(matches!(
            verify_persisted_node_certificate_der(
                issued.der(),
                authority.certificate_der(),
                test_overlay(),
                issued.not_before() - Duration::seconds(1),
            ),
            Err(IdentityError::NodeCertificateOutsideValidity)
        ));
    }

    #[tokio::test]
    async fn verified_peer_entry_point_requires_v2_tls_metadata() {
        let dir = TestDir::new("verified-peer-entry-point");
        let (authority, _, _) = create_ca(&dir);
        let node_key = NodeKey::load_or_create(dir.join("node-key.pem")).expect("create node key");
        let overlay_ip = Ipv4Addr::new(10, 88, 0, 42);
        let issued = authority
            .issue_node_certificate(
                &node_key.create_csr_der().expect("create CSR"),
                "edge-42",
                test_overlay(),
                overlay_ip,
            )
            .expect("issue node certificate");
        let now = issued.not_before() + Duration::seconds(1);

        let connection = authenticate_connection(
            Box::new(TestPeerConnection {
                alpn: Some(CONTROL_ALPN.to_vec()),
                chain: Some(vec![
                    issued.der().to_vec(),
                    authority.certificate_der().to_vec(),
                ]),
            }),
            ProtocolPurpose::Control,
        )
        .await
        .expect("authenticate v2 transport metadata");
        let identity = authenticate_verified_node_certificate(
            &connection,
            authority.certificate_der(),
            "edge-42",
            overlay_ip,
            test_overlay(),
            now,
        )
        .expect("authenticate TLS peer leaf");
        assert_eq!(identity.fingerprint(), issued.fingerprint());

        let other_dir = TestDir::new("verified-peer-wrong-ca");
        let (other_authority, _, _) = create_ca(&other_dir);
        assert!(matches!(
            authenticate_verified_node_certificate(
                &connection,
                other_authority.certificate_der(),
                "edge-42",
                overlay_ip,
                test_overlay(),
                now,
            ),
            Err(IdentityError::UntrustedNodeCertificate)
        ));

        assert!(matches!(
            authenticate_connection(
                Box::new(TestPeerConnection {
                    alpn: Some(CONTROL_ALPN.to_vec()),
                    chain: Some(Vec::new()),
                }),
                ProtocolPurpose::Control,
            )
            .await,
            Err(TransportError::MissingPeerCertificate)
        ));

        assert!(matches!(
            authenticate_connection(
                Box::new(TestPeerConnection {
                    alpn: Some(CONTROL_ALPN.to_vec()),
                    chain: None,
                }),
                ProtocolPurpose::Control,
            )
            .await,
            Err(TransportError::MissingPeerCertificate)
        ));

        assert!(matches!(
            authenticate_connection(
                Box::new(TestPeerConnection {
                    alpn: Some(CONTROL_ALPN.to_vec()),
                    chain: Some(vec![issued.der().to_vec()]),
                }),
                ProtocolPurpose::Relay,
            )
            .await,
            Err(TransportError::AlpnMismatch)
        ));

        assert!(matches!(
            authenticate_connection(
                Box::new(TestPeerConnection {
                    alpn: Some(RELAY_ALPN.to_vec()),
                    chain: Some(vec![issued.der().to_vec()]),
                }),
                ProtocolPurpose::Enrollment,
            )
            .await,
            Err(TransportError::InvalidConfiguration(_))
        ));
    }

    #[test]
    fn verified_leaf_rejects_certificates_outside_the_node_profile() {
        let dir = TestDir::new("verified-leaf-profile");
        let (authority, _, _) = create_ca(&dir);
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("generate leaf key");
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "edge-42");
        params.subject_alt_names = vec![
            SanType::URI(
                "urn:stellaris:node:edge-42"
                    .try_into()
                    .expect("valid node URI"),
            ),
            SanType::IpAddress(Ipv4Addr::new(10, 88, 0, 42).into()),
        ];
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let certificate = params.self_signed(&key).expect("create profile fixture");

        assert!(matches!(
            authenticate_verified_node_certificate_der(
                certificate.der().as_ref(),
                authority.certificate_der(),
                "edge-42",
                Ipv4Addr::new(10, 88, 0, 42),
                test_overlay(),
                OffsetDateTime::now_utc(),
            ),
            Err(IdentityError::InvalidNodeCertificateProfile)
        ));
    }
}

#[cfg(all(test, not(unix)))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn persistence_fails_instead_of_using_weak_secret_permissions() {
        assert!(matches!(
            NodeKey::load_or_create("node-key.pem"),
            Err(IdentityError::UnsupportedPlatform { .. })
        ));
        assert!(matches!(
            CertificateAuthority::create("ca.pem", "ca-key.pem"),
            Err(IdentityError::UnsupportedPlatform { .. })
        ));
    }
}
