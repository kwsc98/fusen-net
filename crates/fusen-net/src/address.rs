// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Overlay address allocation and node authentication.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::Ipv4Addr,
    str::FromStr,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ipnet::Ipv4Net;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const TOKEN_BYTES: usize = 32;
pub const TOKEN_DIGEST_BYTES: usize = 32;
pub const MAX_NODE_ID_LEN: usize = 63;
pub const TOKEN_PREFIX: &str = "fsn1_";
pub const TOKEN_DIGEST_PREFIX: &str = "sha256:";
const DUMMY_TOKEN_DIGEST: TokenDigest = TokenDigest([0_u8; TOKEN_DIGEST_BYTES]);

#[derive(Clone, Eq, PartialEq)]
pub struct NodeToken([u8; TOKEN_BYTES]);

impl NodeToken {
    pub fn parse(encoded: &str) -> Result<Self, AddressError> {
        let encoded = encoded
            .strip_prefix(TOKEN_PREFIX)
            .ok_or(AddressError::InvalidTokenFormat)?;
        let decoded = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| AddressError::InvalidTokenFormat)?;
        let bytes: [u8; TOKEN_BYTES] = decoded
            .try_into()
            .map_err(|_| AddressError::InvalidTokenFormat)?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn encode(&self) -> String {
        format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for NodeToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NodeToken([REDACTED])")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct TokenDigest([u8; TOKEN_DIGEST_BYTES]);

impl TokenDigest {
    pub fn from_token(token: &NodeToken) -> Self {
        // The persisted digest covers the complete textual token, including
        // its version prefix. This is the same byte sequence stored by the
        // agent and sent in Register.
        let digest: [u8; TOKEN_DIGEST_BYTES] = Sha256::digest(token.encode().as_bytes()).into();
        Self(digest)
    }

    pub fn from_token_text(encoded: &str) -> Result<Self, AddressError> {
        Ok(Self::from_token(&NodeToken::parse(encoded)?))
    }

    pub fn to_hex(&self) -> String {
        let mut output = String::with_capacity(TOKEN_DIGEST_BYTES * 2);
        for byte in self.0 {
            use fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
        }
        output
    }

    fn matches_digest(&self, candidate: &Self) -> bool {
        bool::from(self.0.ct_eq(&candidate.0))
    }
}

impl fmt::Debug for TokenDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TokenDigest([REDACTED])")
    }
}

impl fmt::Display for TokenDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{TOKEN_DIGEST_PREFIX}{}", self.to_hex())
    }
}

impl FromStr for TokenDigest {
    type Err = AddressError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value
            .strip_prefix(TOKEN_DIGEST_PREFIX)
            .ok_or(AddressError::InvalidTokenDigest)?;
        if value.len() != TOKEN_DIGEST_BYTES * 2
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AddressError::InvalidTokenDigest);
        }
        let mut result = [0_u8; TOKEN_DIGEST_BYTES];
        for (index, byte) in result.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = u8::from_str_radix(&value[offset..offset + 2], 16)
                .map_err(|_| AddressError::InvalidTokenDigest)?;
        }
        Ok(Self(result))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaticBinding {
    pub node_id: String,
    pub token_sha256: TokenDigest,
    pub overlay_ip: Ipv4Addr,
}

impl StaticBinding {
    pub fn new(
        node_id: impl Into<String>,
        token_sha256: TokenDigest,
        overlay_ip: Ipv4Addr,
    ) -> Self {
        Self {
            node_id: node_id.into(),
            token_sha256,
            overlay_ip,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddressLease {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
}

/// Authentication and allocation are intentionally one operation. A relay
/// must not reveal whether a node ID exists when a credential is rejected.
pub trait AddressAllocator: Send + Sync + 'static {
    fn allocate(&self, node_id: &str, encoded_token: &str) -> Result<AddressLease, AddressError>;

    fn release(&self, _lease: &AddressLease) -> Result<(), AddressError> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct StaticAddressAllocator {
    overlay: Ipv4Net,
    bindings: HashMap<String, StaticBinding>,
}

impl StaticAddressAllocator {
    pub fn new(
        overlay: Ipv4Net,
        bindings: impl IntoIterator<Item = StaticBinding>,
    ) -> Result<Self, AddressError> {
        let mut by_node = HashMap::new();
        let mut addresses = HashSet::new();

        for binding in bindings {
            validate_node_id(&binding.node_id)?;
            validate_overlay_address(overlay, binding.overlay_ip)?;
            if !addresses.insert(binding.overlay_ip) {
                return Err(AddressError::DuplicateAddress(binding.overlay_ip));
            }
            let node_id = binding.node_id.clone();
            if by_node.insert(node_id.clone(), binding).is_some() {
                return Err(AddressError::DuplicateNode(node_id));
            }
        }

        Ok(Self {
            overlay,
            bindings: by_node,
        })
    }

    pub const fn overlay(&self) -> Ipv4Net {
        self.overlay
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

impl AddressAllocator for StaticAddressAllocator {
    fn allocate(&self, node_id: &str, encoded_token: &str) -> Result<AddressLease, AddressError> {
        // Parsing is performed even for an unknown node, keeping the public
        // result identical for malformed, unknown, and incorrect credentials.
        let token =
            NodeToken::parse(encoded_token).map_err(|_| AddressError::AuthenticationFailed)?;
        let candidate_digest = TokenDigest::from_token(&token);
        let binding = self.bindings.get(node_id);
        let expected_digest = binding
            .map(|binding| &binding.token_sha256)
            .unwrap_or(&DUMMY_TOKEN_DIGEST);
        let digest_matches = expected_digest.matches_digest(&candidate_digest);
        let Some(binding) = binding else {
            return Err(AddressError::AuthenticationFailed);
        };
        if !digest_matches {
            return Err(AddressError::AuthenticationFailed);
        }
        Ok(AddressLease {
            node_id: binding.node_id.clone(),
            overlay_ip: binding.overlay_ip,
        })
    }
}

pub fn validate_node_id(node_id: &str) -> Result<(), AddressError> {
    let mut bytes = node_id.bytes();
    let Some(first) = bytes.next() else {
        return Err(AddressError::InvalidNodeId);
    };
    if node_id.len() > MAX_NODE_ID_LEN
        || !first.is_ascii_alphanumeric()
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(AddressError::InvalidNodeId);
    }
    Ok(())
}

pub fn validate_overlay_address(overlay: Ipv4Net, address: Ipv4Addr) -> Result<(), AddressError> {
    if !overlay.contains(&address)
        || address == overlay.network()
        || address == overlay.broadcast()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_broadcast()
    {
        return Err(AddressError::InvalidOverlayAddress(address));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum AddressError {
    #[error("node ID must be 1-{MAX_NODE_ID_LEN} ASCII letters, digits, '.', '_' or '-'")]
    InvalidNodeId,
    #[error("token must be fsn1_ followed by unpadded URL-safe base64 for exactly 32 bytes")]
    InvalidTokenFormat,
    #[error("token digest must be sha256: followed by exactly 64 lowercase hexadecimal characters")]
    InvalidTokenDigest,
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("duplicate node ID {0}")]
    DuplicateNode(String),
    #[error("duplicate overlay address {0}")]
    DuplicateAddress(Ipv4Addr),
    #[error("{0} is not a usable host address in the overlay")]
    InvalidOverlayAddress(Ipv4Addr),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(fill: u8) -> NodeToken {
        NodeToken::from_bytes([fill; TOKEN_BYTES])
    }

    fn allocator() -> StaticAddressAllocator {
        let token = token(7);
        StaticAddressAllocator::new(
            "10.42.0.0/24".parse().expect("network"),
            [StaticBinding::new(
                "edge-a",
                TokenDigest::from_token(&token),
                "10.42.0.2".parse().expect("address"),
            )],
        )
        .expect("allocator")
    }

    #[test]
    fn valid_credentials_return_static_lease() {
        let lease = allocator()
            .allocate("edge-a", &token(7).encode())
            .expect("allocate");
        assert_eq!(lease.node_id, "edge-a");
        assert_eq!(
            lease.overlay_ip,
            "10.42.0.2".parse::<Ipv4Addr>().expect("address")
        );
    }

    #[test]
    fn all_invalid_credentials_have_same_public_error() {
        let allocator = allocator();
        for result in [
            allocator.allocate("unknown", &token(7).encode()),
            allocator.allocate("edge-a", &token(8).encode()),
            allocator.allocate("edge-a", "not-base64"),
        ] {
            assert_eq!(result, Err(AddressError::AuthenticationFailed));
        }
    }

    #[test]
    fn digest_hex_round_trips() {
        let digest = TokenDigest::from_token(&token(3));
        assert_eq!(
            digest.to_string().parse::<TokenDigest>().expect("digest"),
            digest
        );
        assert!("abcd".parse::<TokenDigest>().is_err());
    }

    #[test]
    fn duplicate_nodes_and_addresses_are_rejected() {
        let network = "10.42.0.0/24".parse().expect("network");
        let digest = TokenDigest::from_token(&token(1));
        let duplicate_node = StaticAddressAllocator::new(
            network,
            [
                StaticBinding::new("edge", digest.clone(), "10.42.0.2".parse().expect("ip")),
                StaticBinding::new("edge", digest.clone(), "10.42.0.3".parse().expect("ip")),
            ],
        );
        assert!(matches!(
            duplicate_node,
            Err(AddressError::DuplicateNode(_))
        ));

        let duplicate_address = StaticAddressAllocator::new(
            network,
            [
                StaticBinding::new("edge-a", digest.clone(), "10.42.0.2".parse().expect("ip")),
                StaticBinding::new("edge-b", digest, "10.42.0.2".parse().expect("ip")),
            ],
        );
        assert!(matches!(
            duplicate_address,
            Err(AddressError::DuplicateAddress(_))
        ));
    }

    #[test]
    fn unusable_overlay_addresses_are_rejected() {
        let network = "10.42.0.0/24".parse().expect("network");
        let digest = TokenDigest::from_token(&token(1));
        for address in ["10.42.0.0", "10.42.0.255", "10.43.0.2"] {
            let result = StaticAddressAllocator::new(
                network,
                [StaticBinding::new(
                    "edge",
                    digest.clone(),
                    address.parse().expect("address"),
                )],
            );
            assert!(matches!(
                result,
                Err(AddressError::InvalidOverlayAddress(_))
            ));
        }
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let token = token(9);
        assert_eq!(format!("{token:?}"), "NodeToken([REDACTED])");
        assert_eq!(
            format!("{:?}", TokenDigest::from_token(&token)),
            "TokenDigest([REDACTED])"
        );
    }

    #[test]
    fn node_id_contract_matches_configuration_format() {
        for valid in ["a", "A0", "edge-a", "edge_a", "edge.a"] {
            assert!(validate_node_id(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "-edge", ".edge", "_edge", "edge/a"] {
            assert!(validate_node_id(invalid).is_err(), "{invalid}");
        }
        assert!(validate_node_id(&"a".repeat(63)).is_ok());
        assert!(validate_node_id(&"a".repeat(64)).is_err());
    }
}
