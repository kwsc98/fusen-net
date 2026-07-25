// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Read-only protocol-v2 node registry.
//!
//! Enrollment authentication deliberately has one public failure result. A
//! lookup for an unknown or disabled node still hashes and compares the
//! supplied token against a dummy digest so callers do not accidentally turn
//! the registry into a node-enumeration oracle.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::Ipv4Addr,
    str::FromStr,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ipnet::Ipv4Net;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const STATIC_NODE_REGISTRY_VERSION: u16 = 2;
pub const MAX_STATIC_NODES: usize = 256;
pub const MAX_STATIC_NODE_REGISTRY_BYTES: usize = 128 * 1024;
pub const ENROLLMENT_TOKEN_BYTES: usize = 32;
pub const ENROLLMENT_TOKEN_PREFIX: &str = "stl2_";
pub const MAX_NODE_ID_LEN: usize = 63;

const TOKEN_DIGEST_PREFIX: &str = "sha256:";
const DUMMY_TOKEN_DIGEST: EnrollmentTokenDigest =
    EnrollmentTokenDigest([0_u8; ENROLLMENT_TOKEN_BYTES]);

pub fn validate_node_id(node_id: &str) -> Result<(), RegistryError> {
    let mut bytes = node_id.bytes();
    let Some(first) = bytes.next() else {
        return Err(RegistryError::InvalidNodeId);
    };
    if node_id.len() > MAX_NODE_ID_LEN
        || !first.is_ascii_alphanumeric()
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(RegistryError::InvalidNodeId);
    }
    Ok(())
}

pub fn validate_overlay_address(overlay: Ipv4Net, address: Ipv4Addr) -> Result<(), RegistryError> {
    if !overlay.contains(&address)
        || address == overlay.network()
        || address == overlay.broadcast()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_broadcast()
    {
        return Err(RegistryError::InvalidOverlayAddress(address));
    }
    Ok(())
}

/// A protocol-v2 one-time enrollment token.
#[derive(Clone, Eq, PartialEq)]
pub struct EnrollmentToken([u8; ENROLLMENT_TOKEN_BYTES]);

impl EnrollmentToken {
    pub fn generate() -> Self {
        let mut bytes = [0_u8; ENROLLMENT_TOKEN_BYTES];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; ENROLLMENT_TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn parse(encoded: &str) -> Result<Self, RegistryError> {
        let payload = encoded
            .strip_prefix(ENROLLMENT_TOKEN_PREFIX)
            .ok_or(RegistryError::InvalidEnrollmentToken)?;
        let decoded = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| RegistryError::InvalidEnrollmentToken)?;
        let bytes = decoded
            .try_into()
            .map_err(|_| RegistryError::InvalidEnrollmentToken)?;
        Ok(Self(bytes))
    }

    pub fn encode(&self) -> String {
        format!(
            "{ENROLLMENT_TOKEN_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(self.0)
        )
    }
}

impl fmt::Debug for EnrollmentToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentToken([REDACTED])")
    }
}

/// A SHA-256 digest of the complete textual enrollment token.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct EnrollmentTokenDigest([u8; ENROLLMENT_TOKEN_BYTES]);

impl EnrollmentTokenDigest {
    pub fn from_token(token: &EnrollmentToken) -> Self {
        Self(Sha256::digest(token.encode().as_bytes()).into())
    }

    pub fn from_token_text(token: &str) -> Result<Self, RegistryError> {
        let token = EnrollmentToken::parse(token).map_err(|_| RegistryError::InvalidTokenDigest)?;
        Ok(Self::from_token(&token))
    }

    pub fn as_bytes(&self) -> &[u8; ENROLLMENT_TOKEN_BYTES] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        let mut encoded = String::with_capacity(ENROLLMENT_TOKEN_BYTES * 2);
        for byte in self.0 {
            use fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }

    fn matches(&self, candidate: &Self) -> bool {
        bool::from(self.0.ct_eq(&candidate.0))
    }
}

impl fmt::Debug for EnrollmentTokenDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentTokenDigest([REDACTED])")
    }
}

impl fmt::Display for EnrollmentTokenDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{TOKEN_DIGEST_PREFIX}{}", self.to_hex())
    }
}

impl FromStr for EnrollmentTokenDigest {
    type Err = RegistryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value
            .strip_prefix(TOKEN_DIGEST_PREFIX)
            .ok_or(RegistryError::InvalidTokenDigest)?;
        if hex.len() != ENROLLMENT_TOKEN_BYTES * 2
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(RegistryError::InvalidTokenDigest);
        }

        let mut digest = [0_u8; ENROLLMENT_TOKEN_BYTES];
        for (index, byte) in digest.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = u8::from_str_radix(&hex[offset..offset + 2], 16)
                .map_err(|_| RegistryError::InvalidTokenDigest)?;
        }
        Ok(Self(digest))
    }
}

impl Serialize for EnrollmentTokenDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for EnrollmentTokenDigest {
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
pub struct StaticNode {
    #[serde(rename = "id")]
    pub node_id: String,
    #[serde(rename = "ipv4")]
    pub overlay_ip: Ipv4Addr,
    pub enrollment_token_sha256: EnrollmentTokenDigest,
    pub enabled: bool,
}

impl StaticNode {
    pub fn new(
        node_id: impl Into<String>,
        overlay_ip: Ipv4Addr,
        enrollment_token_sha256: EnrollmentTokenDigest,
        enabled: bool,
    ) -> Self {
        Self {
            node_id: node_id.into(),
            overlay_ip,
            enrollment_token_sha256,
            enabled,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StaticNodeRegistryDocument {
    pub version: u16,
    pub nodes: Vec<StaticNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedStaticNode {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
}

#[derive(Debug)]
pub struct StaticNodeRegistry {
    overlay: Ipv4Net,
    by_node: HashMap<String, StaticNode>,
    by_overlay: HashMap<Ipv4Addr, String>,
}

impl StaticNodeRegistry {
    pub fn new(
        overlay: Ipv4Net,
        nodes: impl IntoIterator<Item = StaticNode>,
    ) -> Result<Self, RegistryError> {
        let nodes = nodes.into_iter();
        let (lower, upper) = nodes.size_hint();
        if lower > MAX_STATIC_NODES || upper.is_some_and(|upper| upper > MAX_STATIC_NODES) {
            return Err(RegistryError::CapacityExceeded(MAX_STATIC_NODES));
        }

        let mut by_node = HashMap::new();
        let mut by_overlay = HashMap::new();
        let mut token_digests = HashSet::new();
        for node in nodes {
            if by_node.len() == MAX_STATIC_NODES {
                return Err(RegistryError::CapacityExceeded(MAX_STATIC_NODES));
            }
            validate_node_id(&node.node_id).map_err(|_| RegistryError::InvalidNodeId)?;
            validate_overlay_address(overlay, node.overlay_ip)
                .map_err(|_| RegistryError::InvalidOverlayAddress(node.overlay_ip))?;
            if by_node.contains_key(&node.node_id) {
                return Err(RegistryError::DuplicateNode(node.node_id));
            }
            if by_overlay.contains_key(&node.overlay_ip) {
                return Err(RegistryError::DuplicateOverlayAddress(node.overlay_ip));
            }
            if !token_digests.insert(node.enrollment_token_sha256.clone()) {
                return Err(RegistryError::DuplicateEnrollmentTokenDigest);
            }

            by_overlay.insert(node.overlay_ip, node.node_id.clone());
            by_node.insert(node.node_id.clone(), node);
        }

        Ok(Self {
            overlay,
            by_node,
            by_overlay,
        })
    }

    pub fn from_document(
        overlay: Ipv4Net,
        document: StaticNodeRegistryDocument,
    ) -> Result<Self, RegistryError> {
        if document.version != STATIC_NODE_REGISTRY_VERSION {
            return Err(RegistryError::UnsupportedVersion(document.version));
        }
        Self::new(overlay, document.nodes)
    }

    pub fn from_json(overlay: Ipv4Net, encoded: &[u8]) -> Result<Self, RegistryError> {
        if encoded.is_empty() || encoded.len() > MAX_STATIC_NODE_REGISTRY_BYTES {
            return Err(RegistryError::InvalidDocumentSize(encoded.len()));
        }
        let document = serde_json::from_slice(encoded).map_err(|_| RegistryError::InvalidJson)?;
        Self::from_document(overlay, document)
    }

    pub const fn overlay(&self) -> Ipv4Net {
        self.overlay
    }

    pub fn len(&self) -> usize {
        self.by_node.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_node.is_empty()
    }

    pub fn binding(&self, node_id: &str) -> Option<&StaticNode> {
        self.by_node.get(node_id)
    }

    pub fn binding_by_overlay(&self, overlay_ip: Ipv4Addr) -> Option<&StaticNode> {
        self.by_overlay
            .get(&overlay_ip)
            .and_then(|node_id| self.by_node.get(node_id))
    }

    pub fn enabled_binding(&self, node_id: &str) -> Option<&StaticNode> {
        self.binding(node_id).filter(|node| node.enabled)
    }

    pub fn authenticate_enrollment(
        &self,
        node_id: &str,
        encoded_token: &str,
    ) -> Result<AuthenticatedStaticNode, RegistryError> {
        // Hash every input, including malformed values, before consulting the
        // result. Unknown nodes compare against a fixed dummy digest.
        let candidate = EnrollmentTokenDigest(Sha256::digest(encoded_token.as_bytes()).into());
        let token_is_well_formed = EnrollmentToken::parse(encoded_token).is_ok();
        let binding = self.by_node.get(node_id);
        let expected = binding
            .map(|node| &node.enrollment_token_sha256)
            .unwrap_or(&DUMMY_TOKEN_DIGEST);
        let digest_matches = expected.matches(&candidate);

        let Some(binding) = binding else {
            return Err(RegistryError::AuthenticationFailed);
        };
        if !token_is_well_formed || !binding.enabled || !digest_matches {
            return Err(RegistryError::AuthenticationFailed);
        }

        Ok(AuthenticatedStaticNode {
            node_id: binding.node_id.clone(),
            overlay_ip: binding.overlay_ip,
        })
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum RegistryError {
    #[error("static node registry version {0} is unsupported; expected version 2")]
    UnsupportedVersion(u16),
    #[error("static node registry contains invalid JSON")]
    InvalidJson,
    #[error("static node registry size {0} is outside its allowed bounds")]
    InvalidDocumentSize(usize),
    #[error("static node registry supports at most {0} nodes")]
    CapacityExceeded(usize),
    #[error("node ID is invalid")]
    InvalidNodeId,
    #[error("enrollment token digest must be sha256 followed by 64 lowercase hex characters")]
    InvalidTokenDigest,
    #[error("enrollment token must be stl2_ followed by 32 URL-safe base64 bytes")]
    InvalidEnrollmentToken,
    #[error("duplicate node ID {0}")]
    DuplicateNode(String),
    #[error("duplicate overlay address {0}")]
    DuplicateOverlayAddress(Ipv4Addr),
    #[error("duplicate enrollment token digest")]
    DuplicateEnrollmentTokenDigest,
    #[error("{0} is not a usable host address in the overlay")]
    InvalidOverlayAddress(Ipv4Addr),
    #[error("enrollment authentication failed")]
    AuthenticationFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn token(fill: u8) -> EnrollmentToken {
        EnrollmentToken::from_bytes([fill; ENROLLMENT_TOKEN_BYTES])
    }

    fn node(id: &str, ip: &str, token_fill: u8, enabled: bool) -> StaticNode {
        StaticNode::new(
            id,
            ip.parse().expect("valid fixture address"),
            EnrollmentTokenDigest::from_token(&token(token_fill)),
            enabled,
        )
    }

    fn registry() -> StaticNodeRegistry {
        StaticNodeRegistry::new(
            "10.88.0.0/24".parse().expect("valid fixture overlay"),
            [
                node("edge-a", "10.88.0.2", 1, true),
                node("edge-b", "10.88.0.3", 2, false),
            ],
        )
        .expect("valid fixture registry")
    }

    #[test]
    fn valid_token_authenticates_the_static_binding() {
        let authenticated = registry()
            .authenticate_enrollment("edge-a", &token(1).encode())
            .expect("valid enrollment");
        assert_eq!(authenticated.node_id, "edge-a");
        assert_eq!(
            authenticated.overlay_ip,
            "10.88.0.2".parse::<Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn all_invalid_credentials_share_one_public_error() {
        let registry = registry();
        for result in [
            registry.authenticate_enrollment("missing", &token(1).encode()),
            registry.authenticate_enrollment("edge-a", &token(9).encode()),
            registry.authenticate_enrollment("edge-a", "malformed"),
            registry.authenticate_enrollment(
                "edge-a",
                &format!(
                    "stl3_{}",
                    URL_SAFE_NO_PAD.encode([1; ENROLLMENT_TOKEN_BYTES])
                ),
            ),
            registry.authenticate_enrollment("edge-b", &token(2).encode()),
        ] {
            assert_eq!(result, Err(RegistryError::AuthenticationFailed));
        }
    }

    #[test]
    fn every_identity_dimension_is_unique() {
        let overlay = "10.88.0.0/24".parse().unwrap();
        assert!(matches!(
            StaticNodeRegistry::new(
                overlay,
                [
                    node("edge", "10.88.0.2", 1, true),
                    node("edge", "10.88.0.3", 2, true)
                ]
            ),
            Err(RegistryError::DuplicateNode(_))
        ));
        assert!(matches!(
            StaticNodeRegistry::new(
                overlay,
                [
                    node("edge-a", "10.88.0.2", 1, true),
                    node("edge-b", "10.88.0.2", 2, true)
                ]
            ),
            Err(RegistryError::DuplicateOverlayAddress(_))
        ));
        assert_eq!(
            StaticNodeRegistry::new(
                overlay,
                [
                    node("edge-a", "10.88.0.2", 1, true),
                    node("edge-b", "10.88.0.3", 1, true)
                ]
            )
            .unwrap_err(),
            RegistryError::DuplicateEnrollmentTokenDigest
        );
    }

    #[test]
    fn v1_unknown_fields_and_oversized_documents_are_rejected() {
        assert!(matches!(
            StaticNodeRegistry::from_json(
                "10.88.0.0/24".parse().unwrap(),
                br#"{"version":1,"nodes":[]}"#
            ),
            Err(RegistryError::UnsupportedVersion(1))
        ));
        assert_eq!(
            StaticNodeRegistry::from_json(
                "10.88.0.0/24".parse().unwrap(),
                br#"{"version":2,"nodes":[],"legacy":true}"#
            )
            .unwrap_err(),
            RegistryError::InvalidJson
        );
        assert!(matches!(
            StaticNodeRegistry::from_json(
                "10.88.0.0/24".parse().unwrap(),
                &vec![b' '; MAX_STATIC_NODE_REGISTRY_BYTES + 1]
            ),
            Err(RegistryError::InvalidDocumentSize(_))
        ));
    }

    #[test]
    fn digest_serialization_is_canonical_and_redacted() {
        let digest = EnrollmentTokenDigest::from_token(&token(3));
        let encoded = serde_json::to_string(&digest).expect("serialize digest");
        assert_eq!(
            serde_json::from_str::<EnrollmentTokenDigest>(&encoded).unwrap(),
            digest
        );
        assert_eq!(format!("{digest:?}"), "EnrollmentTokenDigest([REDACTED])");
        assert!("sha256:ABCDEF".parse::<EnrollmentTokenDigest>().is_err());
    }
}
