// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stellaris enrollment, coordination, relay, and peer protocol v2.
//!
//! Each protocol is isolated by a dedicated ALPN and message enum. Messages
//! sent after TLS authentication deliberately omit claims that are already
//! bound to the connection identity. In particular, candidate announcements,
//! relay binds, and peer handshakes cannot override the authenticated node ID
//! or overlay address.

use std::{
    collections::HashSet,
    fmt, io,
    net::{Ipv4Addr, SocketAddr},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::registry::{EnrollmentToken, validate_node_id, validate_overlay_address};

pub const MAGIC: [u8; 4] = *b"STLR";
pub const VERSION: u16 = 2;
pub const HEADER_LEN: usize = 12;
pub const MAX_JSON_PAYLOAD: usize = 16 * 1024;
pub const MAX_HANDSHAKE_PAYLOAD: usize = 4 * 1024;
pub const MAX_CANDIDATES: usize = 16;
pub const MAX_ERROR_MESSAGE_LEN: usize = 512;
pub const MAX_CSR_DER_BYTES: usize = 4 * 1024;
pub const MAX_CERTIFICATE_DER_BYTES: usize = 6 * 1024;
pub const MAX_CERTIFICATE_CHAIN_LEN: usize = 3;
pub const MIN_TUN_MTU: u16 = 576;
pub const MAX_TUN_MTU: u16 = 1100;
pub const MIN_OVERLAY_PREFIX_LEN: u8 = 8;
pub const MAX_OVERLAY_PREFIX_LEN: u8 = 30;

pub const ENROLLMENT_ALPN: &[u8] = b"stellaris/enroll/2";
pub const CONTROL_ALPN: &[u8] = b"stellaris/control/2";
pub const RELAY_ALPN: &[u8] = b"stellaris/relay/2";
pub const P2P_ALPN: &[u8] = b"stellaris/p2p/2";

const CERTIFICATE_FINGERPRINT_PREFIX: &str = "sha256:";
const SHA256_HEX_LEN: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MessageType {
    AnnounceCandidates = 1,
    LookupPeer = 2,
    PeerRecord = 3,
    PeerRevoked = 4,
    Error = 5,
    ControlWelcome = 6,
    ConnectRequest = 7,
    ConnectPlan = 8,
    RenewCertificate = 9,
    CertificateIssued = 10,
}

impl TryFrom<u8> for MessageType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::AnnounceCandidates),
            2 => Ok(Self::LookupPeer),
            3 => Ok(Self::PeerRecord),
            4 => Ok(Self::PeerRevoked),
            5 => Ok(Self::Error),
            6 => Ok(Self::ControlWelcome),
            7 => Ok(Self::ConnectRequest),
            8 => Ok(Self::ConnectPlan),
            9 => Ok(Self::RenewCertificate),
            10 => Ok(Self::CertificateIssued),
            other => Err(ProtocolError::UnknownMessageType(other)),
        }
    }
}

/// Direction of a framed v2 message on its authenticated QUIC stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageDirection {
    AgentToCoordinator,
    CoordinatorToAgent,
    InitiatorToResponder,
    ResponderToInitiator,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Host,
    ServerReflexive,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub address: SocketAddr,
    pub kind: CandidateKind,
    pub priority: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnnounceCandidates {
    /// Monotonically increasing within one authenticated control session.
    /// Receivers must reject an epoch that is not greater than the last
    /// accepted epoch for that session.
    pub epoch: u64,
    /// Agents may announce only [`CandidateKind::Host`] candidates. The
    /// coordinator derives server-reflexive candidates from authenticated
    /// observations rather than trusting an agent assertion.
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlWelcome {
    /// Canonical UUID v4 for this authenticated control session.
    pub session_id: String,
    /// Persisted, monotonically increasing generation for this node.
    pub incarnation: u64,
    pub overlay_ip: Ipv4Addr,
    pub overlay_cidr: Ipv4Net,
    pub mtu: u16,
    pub certificate_not_after_unix_seconds: u64,
    pub coordinator_time_unix_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LookupPeer {
    /// Non-zero identifier scoped to the authenticated control session.
    pub request_id: u64,
    pub overlay_ip: Ipv4Addr,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRecord {
    pub request_id: u64,
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    /// Coordinator-assigned, non-zero registration generation for this node.
    /// It increases whenever the node establishes a replacement session.
    pub incarnation: u64,
    /// Canonical UUID for the target's authenticated coordinator session.
    /// Candidate epochs are comparable only while this value is unchanged.
    pub session_id: String,
    /// SHA-256 of the authenticated leaf certificate DER, encoded as
    /// `sha256:` followed by 64 lowercase hexadecimal characters.
    pub certificate_fingerprint: String,
    pub certificate_not_after_unix_seconds: u64,
    /// Candidate epoch advertised by the peer. Zero is the initial
    /// "registered but not yet announced" state and is valid only when
    /// `candidates` is empty.
    pub epoch: u64,
    pub candidates: Vec<Candidate>,
}

/// Authenticated peer binding carried in a connection plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerDescriptor {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    pub incarnation: u64,
    pub session_id: String,
    pub certificate_fingerprint: String,
    pub certificate_not_after_unix_seconds: u64,
    /// Zero is valid only when no candidates have been announced.
    pub candidate_epoch: u64,
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectRequest {
    pub request_id: u64,
    pub overlay_ip: Ipv4Addr,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionRole {
    Initiator,
    Responder,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectPlan {
    pub request_id: u64,
    /// Coordinator-generated UUID v4 shared by both plan recipients.
    pub connection_id: String,
    pub role: ConnectionRole,
    pub peer: PeerDescriptor,
    pub expires_at_unix_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenewCertificate {
    pub request_id: u64,
    pub csr_der_base64: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateIssued {
    pub request_id: u64,
    pub certificate_chain_der_base64: Vec<String>,
    pub not_before_unix_seconds: u64,
    pub not_after_unix_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRevoked {
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    /// Monotonically increasing revocation revision for this node. This value
    /// is compared only with earlier revocation notifications, never with a
    /// session-scoped candidate epoch.
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    ProtocolViolation,
    ReplayDetected,
    PeerNotFound,
    Revoked,
    ServerBusy,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorMessage {
    pub request_id: Option<u64>,
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollRequest {
    /// Stable UUID v4 persisted by the agent until enrollment completes.
    pub enrollment_id: String,
    pub node_id: String,
    pub enrollment_token: String,
    pub csr_der_base64: String,
}

impl fmt::Debug for EnrollRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollRequest")
            .field("enrollment_id", &self.enrollment_id)
            .field("node_id", &self.node_id)
            .field("enrollment_token", &"[REDACTED]")
            .field("csr_der_base64", &self.csr_der_base64)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollAccepted {
    pub enrollment_id: String,
    pub node_id: String,
    pub overlay_ip: Ipv4Addr,
    pub overlay_cidr: Ipv4Net,
    pub mtu: u16,
    pub certificate_chain_der_base64: Vec<String>,
    pub node_ca_certificate_der_base64: String,
    pub not_before_unix_seconds: u64,
    pub not_after_unix_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelayBind {
    pub control_session_id: String,
    pub incarnation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelayAccepted {
    pub relay_session_id: String,
    pub mtu: u16,
    pub max_datagram_size: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelayReady {
    pub relay_session_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct P2pHello {
    pub connection_id: String,
    pub control_session_id: String,
    pub incarnation: u64,
    pub certificate_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct P2pReady {
    pub connection_id: String,
    pub mtu: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlMessage {
    AnnounceCandidates(AnnounceCandidates),
    LookupPeer(LookupPeer),
    PeerRecord(PeerRecord),
    PeerRevoked(PeerRevoked),
    Error(ErrorMessage),
    ControlWelcome(ControlWelcome),
    ConnectRequest(ConnectRequest),
    ConnectPlan(ConnectPlan),
    RenewCertificate(RenewCertificate),
    CertificateIssued(CertificateIssued),
}

impl ControlMessage {
    pub const fn message_type(&self) -> MessageType {
        match self {
            Self::AnnounceCandidates(_) => MessageType::AnnounceCandidates,
            Self::LookupPeer(_) => MessageType::LookupPeer,
            Self::PeerRecord(_) => MessageType::PeerRecord,
            Self::PeerRevoked(_) => MessageType::PeerRevoked,
            Self::Error(_) => MessageType::Error,
            Self::ControlWelcome(_) => MessageType::ControlWelcome,
            Self::ConnectRequest(_) => MessageType::ConnectRequest,
            Self::ConnectPlan(_) => MessageType::ConnectPlan,
            Self::RenewCertificate(_) => MessageType::RenewCertificate,
            Self::CertificateIssued(_) => MessageType::CertificateIssued,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::AnnounceCandidates(message) => validate_announcement(message),
            Self::LookupPeer(message) => {
                validate_request_id(message.request_id)?;
                validate_overlay_ip(message.overlay_ip)
            }
            Self::PeerRecord(message) => validate_peer_record(message),
            Self::PeerRevoked(message) => {
                validate_node_id(&message.node_id).map_err(|_| ProtocolError::InvalidNodeId)?;
                validate_overlay_ip(message.overlay_ip)?;
                validate_epoch(message.epoch)
            }
            Self::Error(message) => validate_error(message),
            Self::ControlWelcome(message) => validate_control_welcome(message),
            Self::ConnectRequest(message) => {
                validate_request_id(message.request_id)?;
                validate_overlay_ip(message.overlay_ip)
            }
            Self::ConnectPlan(message) => validate_connect_plan(message),
            Self::RenewCertificate(message) => {
                validate_request_id(message.request_id)?;
                validate_der_base64(&message.csr_der_base64, MAX_CSR_DER_BYTES, "CSR")
            }
            Self::CertificateIssued(message) => validate_certificate_issued(message),
        }
    }

    pub fn validate_direction(&self, direction: MessageDirection) -> Result<(), ProtocolError> {
        let valid = match self {
            Self::AnnounceCandidates(_)
            | Self::LookupPeer(_)
            | Self::ConnectRequest(_)
            | Self::RenewCertificate(_) => direction == MessageDirection::AgentToCoordinator,
            Self::PeerRecord(_)
            | Self::PeerRevoked(_)
            | Self::ControlWelcome(_)
            | Self::ConnectPlan(_)
            | Self::CertificateIssued(_) => direction == MessageDirection::CoordinatorToAgent,
            Self::Error(_) => matches!(
                direction,
                MessageDirection::AgentToCoordinator | MessageDirection::CoordinatorToAgent
            ),
        };
        if valid {
            Ok(())
        } else {
            Err(ProtocolError::WrongDirection {
                message_type: self.message_type() as u8,
                direction,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EnrollmentMessageType {
    EnrollRequest = 1,
    EnrollAccepted = 2,
    Error = 255,
}

impl TryFrom<u8> for EnrollmentMessageType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::EnrollRequest),
            2 => Ok(Self::EnrollAccepted),
            255 => Ok(Self::Error),
            other => Err(ProtocolError::UnknownMessageType(other)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnrollmentMessage {
    EnrollRequest(EnrollRequest),
    EnrollAccepted(EnrollAccepted),
    Error(ErrorMessage),
}

impl EnrollmentMessage {
    pub const fn message_type(&self) -> EnrollmentMessageType {
        match self {
            Self::EnrollRequest(_) => EnrollmentMessageType::EnrollRequest,
            Self::EnrollAccepted(_) => EnrollmentMessageType::EnrollAccepted,
            Self::Error(_) => EnrollmentMessageType::Error,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::EnrollRequest(message) => validate_enroll_request(message),
            Self::EnrollAccepted(message) => validate_enroll_accepted(message),
            Self::Error(message) => validate_error(message),
        }
    }

    pub fn validate_direction(&self, direction: MessageDirection) -> Result<(), ProtocolError> {
        let valid = match self {
            Self::EnrollRequest(_) => direction == MessageDirection::AgentToCoordinator,
            Self::EnrollAccepted(_) => direction == MessageDirection::CoordinatorToAgent,
            Self::Error(_) => matches!(
                direction,
                MessageDirection::AgentToCoordinator | MessageDirection::CoordinatorToAgent
            ),
        };
        if valid {
            Ok(())
        } else {
            Err(ProtocolError::WrongDirection {
                message_type: self.message_type() as u8,
                direction,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RelayMessageType {
    RelayBind = 1,
    RelayAccepted = 2,
    RelayReady = 3,
    Error = 255,
}

impl TryFrom<u8> for RelayMessageType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::RelayBind),
            2 => Ok(Self::RelayAccepted),
            3 => Ok(Self::RelayReady),
            255 => Ok(Self::Error),
            other => Err(ProtocolError::UnknownMessageType(other)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelayMessage {
    RelayBind(RelayBind),
    RelayAccepted(RelayAccepted),
    RelayReady(RelayReady),
    Error(ErrorMessage),
}

impl RelayMessage {
    pub const fn message_type(&self) -> RelayMessageType {
        match self {
            Self::RelayBind(_) => RelayMessageType::RelayBind,
            Self::RelayAccepted(_) => RelayMessageType::RelayAccepted,
            Self::RelayReady(_) => RelayMessageType::RelayReady,
            Self::Error(_) => RelayMessageType::Error,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::RelayBind(message) => {
                validate_session_id(&message.control_session_id)?;
                validate_incarnation(message.incarnation)
            }
            Self::RelayAccepted(message) => validate_relay_accepted(message),
            Self::RelayReady(message) => validate_session_id(&message.relay_session_id),
            Self::Error(message) => validate_error(message),
        }
    }

    pub fn validate_direction(&self, direction: MessageDirection) -> Result<(), ProtocolError> {
        let valid = match self {
            Self::RelayBind(_) => direction == MessageDirection::AgentToCoordinator,
            Self::RelayAccepted(_) => direction == MessageDirection::CoordinatorToAgent,
            Self::RelayReady(_) => matches!(
                direction,
                MessageDirection::AgentToCoordinator | MessageDirection::CoordinatorToAgent
            ),
            Self::Error(_) => matches!(
                direction,
                MessageDirection::AgentToCoordinator | MessageDirection::CoordinatorToAgent
            ),
        };
        if valid {
            Ok(())
        } else {
            Err(ProtocolError::WrongDirection {
                message_type: self.message_type() as u8,
                direction,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum P2pMessageType {
    P2pHello = 1,
    P2pReady = 2,
    Error = 255,
}

impl TryFrom<u8> for P2pMessageType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::P2pHello),
            2 => Ok(Self::P2pReady),
            255 => Ok(Self::Error),
            other => Err(ProtocolError::UnknownMessageType(other)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum P2pMessage {
    P2pHello(P2pHello),
    P2pReady(P2pReady),
    Error(ErrorMessage),
}

impl P2pMessage {
    pub const fn message_type(&self) -> P2pMessageType {
        match self {
            Self::P2pHello(_) => P2pMessageType::P2pHello,
            Self::P2pReady(_) => P2pMessageType::P2pReady,
            Self::Error(_) => P2pMessageType::Error,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::P2pHello(message) => validate_p2p_hello(message),
            Self::P2pReady(message) => {
                validate_session_id(&message.connection_id)?;
                validate_mtu(message.mtu)
            }
            Self::Error(message) => validate_error(message),
        }
    }

    pub fn validate_direction(&self, direction: MessageDirection) -> Result<(), ProtocolError> {
        let valid = match self {
            Self::P2pHello(_) => direction == MessageDirection::InitiatorToResponder,
            Self::P2pReady(_) => direction == MessageDirection::ResponderToInitiator,
            Self::Error(_) => matches!(
                direction,
                MessageDirection::InitiatorToResponder | MessageDirection::ResponderToInitiator
            ),
        };
        if valid {
            Ok(())
        } else {
            Err(ProtocolError::WrongDirection {
                message_type: self.message_type() as u8,
                direction,
            })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("incomplete v2 frame")]
    Incomplete,
    #[error("invalid v2 frame magic")]
    InvalidMagic,
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("unsupported frame flags 0x{0:02x}")]
    UnsupportedFlags(u8),
    #[error("unknown v2 message type {0}")]
    UnknownMessageType(u8),
    #[error("message type {message_type} is invalid in direction {direction:?}")]
    WrongDirection {
        message_type: u8,
        direction: MessageDirection,
    },
    #[error("JSON payload is {actual} bytes; maximum is {max}")]
    PayloadTooLarge { actual: usize, max: usize },
    #[error("candidate list has {actual} entries; maximum is {max}")]
    TooManyCandidates { actual: usize, max: usize },
    #[error("candidate address {0} is not a usable unicast socket address")]
    InvalidCandidateAddress(SocketAddr),
    #[error("candidate address {0} appears more than once")]
    DuplicateCandidate(SocketAddr),
    #[error("agents may announce only host candidates")]
    InvalidAnnouncedCandidateKind,
    #[error("request ID must be non-zero")]
    InvalidRequestId,
    #[error("epoch must be non-zero")]
    InvalidEpoch,
    #[error("invalid node ID")]
    InvalidNodeId,
    #[error("session ID must be a canonical hyphenated UUID v4")]
    InvalidSessionId,
    #[error("peer incarnation must be non-zero")]
    InvalidIncarnation,
    #[error("{0} is not a usable overlay IPv4 address")]
    InvalidOverlayIp(Ipv4Addr),
    #[error("{address} is not a usable host address in overlay {overlay}")]
    InvalidOverlayBinding { address: Ipv4Addr, overlay: Ipv4Net },
    #[error(
        "overlay prefix must be in the range /{MIN_OVERLAY_PREFIX_LEN}..=/{MAX_OVERLAY_PREFIX_LEN}"
    )]
    InvalidOverlayCidr,
    #[error("invalid certificate fingerprint")]
    InvalidCertificateFingerprint,
    #[error("invalid enrollment token")]
    InvalidEnrollmentToken,
    #[error("{field} is not canonical base64 DER or exceeds {max} decoded bytes")]
    InvalidDerBase64 { field: &'static str, max: usize },
    #[error("certificate chain must contain 1-{MAX_CERTIFICATE_CHAIN_LEN} certificates")]
    InvalidCertificateChain,
    #[error("certificate validity interval is invalid")]
    InvalidCertificateValidity,
    #[error("timestamp must be non-zero")]
    InvalidTimestamp,
    #[error("MTU must be in the range {MIN_TUN_MTU}..={MAX_TUN_MTU}")]
    InvalidMtu,
    #[error("maximum QUIC datagram size must be at least the negotiated MTU")]
    InvalidDatagramSize,
    #[error("error message must be 1-{MAX_ERROR_MESSAGE_LEN} bytes without control characters")]
    InvalidErrorMessage,
    #[error("invalid v2 JSON payload: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("v2 stream I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProtocolCodec;

impl ProtocolCodec {
    pub fn encode(message: &ControlMessage) -> Result<Bytes, ProtocolError> {
        message.validate()?;
        encode_frame(
            message.message_type() as u8,
            &encode_control_payload(message)?,
            MAX_JSON_PAYLOAD,
        )
    }

    pub fn encode_for(
        message: &ControlMessage,
        direction: MessageDirection,
    ) -> Result<Bytes, ProtocolError> {
        message.validate_direction(direction)?;
        Self::encode(message)
    }

    /// Decodes one frame and leaves any following frame in `buffer`.
    pub fn decode(buffer: &mut BytesMut) -> Result<Option<ControlMessage>, ProtocolError> {
        decode_next(
            buffer,
            MAX_JSON_PAYLOAD,
            decode_control_payload,
            |message| message.validate(),
        )
    }

    pub fn decode_from(
        buffer: &mut BytesMut,
        direction: MessageDirection,
    ) -> Result<Option<ControlMessage>, ProtocolError> {
        decode_next(
            buffer,
            MAX_JSON_PAYLOAD,
            decode_control_payload,
            |message| {
                message.validate()?;
                message.validate_direction(direction)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EnrollmentCodec;

impl EnrollmentCodec {
    pub fn encode(
        message: &EnrollmentMessage,
        direction: MessageDirection,
    ) -> Result<Bytes, ProtocolError> {
        message.validate()?;
        message.validate_direction(direction)?;
        encode_frame(
            message.message_type() as u8,
            &encode_enrollment_payload(message)?,
            MAX_JSON_PAYLOAD,
        )
    }

    pub fn decode(
        buffer: &mut BytesMut,
        direction: MessageDirection,
    ) -> Result<Option<EnrollmentMessage>, ProtocolError> {
        decode_next(
            buffer,
            MAX_JSON_PAYLOAD,
            decode_enrollment_payload,
            |message| {
                message.validate()?;
                message.validate_direction(direction)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RelayCodec;

impl RelayCodec {
    pub fn encode(
        message: &RelayMessage,
        direction: MessageDirection,
    ) -> Result<Bytes, ProtocolError> {
        message.validate()?;
        message.validate_direction(direction)?;
        encode_frame(
            message.message_type() as u8,
            &encode_relay_payload(message)?,
            MAX_HANDSHAKE_PAYLOAD,
        )
    }

    pub fn decode(
        buffer: &mut BytesMut,
        direction: MessageDirection,
    ) -> Result<Option<RelayMessage>, ProtocolError> {
        decode_next(
            buffer,
            MAX_HANDSHAKE_PAYLOAD,
            decode_relay_payload,
            |message| {
                message.validate()?;
                message.validate_direction(direction)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct P2pCodec;

impl P2pCodec {
    pub fn encode(
        message: &P2pMessage,
        direction: MessageDirection,
    ) -> Result<Bytes, ProtocolError> {
        message.validate()?;
        message.validate_direction(direction)?;
        encode_frame(
            message.message_type() as u8,
            &encode_p2p_payload(message)?,
            MAX_HANDSHAKE_PAYLOAD,
        )
    }

    pub fn decode(
        buffer: &mut BytesMut,
        direction: MessageDirection,
    ) -> Result<Option<P2pMessage>, ProtocolError> {
        decode_next(
            buffer,
            MAX_HANDSHAKE_PAYLOAD,
            decode_p2p_payload,
            |message| {
                message.validate()?;
                message.validate_direction(direction)
            },
        )
    }
}

fn encode_frame(
    message_type: u8,
    payload: &[u8],
    max_payload: usize,
) -> Result<Bytes, ProtocolError> {
    validate_payload_len(payload.len(), max_payload)?;
    let payload_len = u32::try_from(payload.len()).map_err(|_| ProtocolError::PayloadTooLarge {
        actual: payload.len(),
        max: max_payload,
    })?;

    let mut frame = BytesMut::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&MAGIC);
    frame.put_u16(VERSION);
    frame.put_u8(message_type);
    frame.put_u8(0);
    frame.put_u32(payload_len);
    frame.extend_from_slice(payload);
    Ok(frame.freeze())
}

fn decode_next<T>(
    buffer: &mut BytesMut,
    max_payload: usize,
    decode: fn(u8, &[u8]) -> Result<T, ProtocolError>,
    validate: impl FnOnce(&T) -> Result<(), ProtocolError>,
) -> Result<Option<T>, ProtocolError> {
    if buffer.len() < HEADER_LEN {
        return Ok(None);
    }

    let header = parse_header(&buffer[..HEADER_LEN], max_payload)?;
    let frame_len =
        HEADER_LEN
            .checked_add(header.payload_len)
            .ok_or(ProtocolError::PayloadTooLarge {
                actual: usize::MAX,
                max: max_payload,
            })?;
    if buffer.len() < frame_len {
        return Ok(None);
    }

    let message = decode(header.message_type, &buffer[HEADER_LEN..frame_len])?;
    validate(&message)?;
    buffer.advance(frame_len);
    Ok(Some(message))
}

#[derive(Clone, Copy, Debug)]
struct Header {
    message_type: u8,
    payload_len: usize,
}

fn parse_header(header: &[u8], max_payload: usize) -> Result<Header, ProtocolError> {
    if header.len() < HEADER_LEN {
        return Err(ProtocolError::Incomplete);
    }
    if header[..4] != MAGIC {
        return Err(ProtocolError::InvalidMagic);
    }

    let version = u16::from_be_bytes([header[4], header[5]]);
    if version != VERSION {
        return Err(ProtocolError::UnsupportedVersion(version));
    }
    let message_type = header[6];
    let flags = header[7];
    if flags != 0 {
        return Err(ProtocolError::UnsupportedFlags(flags));
    }
    let payload_len = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
    validate_payload_len(payload_len, max_payload)?;
    Ok(Header {
        message_type,
        payload_len,
    })
}

fn validate_payload_len(payload_len: usize, max_payload: usize) -> Result<(), ProtocolError> {
    if payload_len > max_payload {
        return Err(ProtocolError::PayloadTooLarge {
            actual: payload_len,
            max: max_payload,
        });
    }
    Ok(())
}

fn json<T: DeserializeOwned>(payload: &[u8]) -> Result<T, ProtocolError> {
    Ok(serde_json::from_slice(payload)?)
}

fn encode_control_payload(message: &ControlMessage) -> Result<Vec<u8>, ProtocolError> {
    Ok(match message {
        ControlMessage::AnnounceCandidates(value) => serde_json::to_vec(value)?,
        ControlMessage::LookupPeer(value) => serde_json::to_vec(value)?,
        ControlMessage::PeerRecord(value) => serde_json::to_vec(value)?,
        ControlMessage::PeerRevoked(value) => serde_json::to_vec(value)?,
        ControlMessage::Error(value) => serde_json::to_vec(value)?,
        ControlMessage::ControlWelcome(value) => serde_json::to_vec(value)?,
        ControlMessage::ConnectRequest(value) => serde_json::to_vec(value)?,
        ControlMessage::ConnectPlan(value) => serde_json::to_vec(value)?,
        ControlMessage::RenewCertificate(value) => serde_json::to_vec(value)?,
        ControlMessage::CertificateIssued(value) => serde_json::to_vec(value)?,
    })
}

fn decode_control_payload(
    message_type: u8,
    payload: &[u8],
) -> Result<ControlMessage, ProtocolError> {
    Ok(match MessageType::try_from(message_type)? {
        MessageType::AnnounceCandidates => ControlMessage::AnnounceCandidates(json(payload)?),
        MessageType::LookupPeer => ControlMessage::LookupPeer(json(payload)?),
        MessageType::PeerRecord => ControlMessage::PeerRecord(json(payload)?),
        MessageType::PeerRevoked => ControlMessage::PeerRevoked(json(payload)?),
        MessageType::Error => ControlMessage::Error(json(payload)?),
        MessageType::ControlWelcome => ControlMessage::ControlWelcome(json(payload)?),
        MessageType::ConnectRequest => ControlMessage::ConnectRequest(json(payload)?),
        MessageType::ConnectPlan => ControlMessage::ConnectPlan(json(payload)?),
        MessageType::RenewCertificate => ControlMessage::RenewCertificate(json(payload)?),
        MessageType::CertificateIssued => ControlMessage::CertificateIssued(json(payload)?),
    })
}

fn encode_enrollment_payload(message: &EnrollmentMessage) -> Result<Vec<u8>, ProtocolError> {
    Ok(match message {
        EnrollmentMessage::EnrollRequest(value) => serde_json::to_vec(value)?,
        EnrollmentMessage::EnrollAccepted(value) => serde_json::to_vec(value)?,
        EnrollmentMessage::Error(value) => serde_json::to_vec(value)?,
    })
}

fn decode_enrollment_payload(
    message_type: u8,
    payload: &[u8],
) -> Result<EnrollmentMessage, ProtocolError> {
    Ok(match EnrollmentMessageType::try_from(message_type)? {
        EnrollmentMessageType::EnrollRequest => EnrollmentMessage::EnrollRequest(json(payload)?),
        EnrollmentMessageType::EnrollAccepted => EnrollmentMessage::EnrollAccepted(json(payload)?),
        EnrollmentMessageType::Error => EnrollmentMessage::Error(json(payload)?),
    })
}

fn encode_relay_payload(message: &RelayMessage) -> Result<Vec<u8>, ProtocolError> {
    Ok(match message {
        RelayMessage::RelayBind(value) => serde_json::to_vec(value)?,
        RelayMessage::RelayAccepted(value) => serde_json::to_vec(value)?,
        RelayMessage::RelayReady(value) => serde_json::to_vec(value)?,
        RelayMessage::Error(value) => serde_json::to_vec(value)?,
    })
}

fn decode_relay_payload(message_type: u8, payload: &[u8]) -> Result<RelayMessage, ProtocolError> {
    Ok(match RelayMessageType::try_from(message_type)? {
        RelayMessageType::RelayBind => RelayMessage::RelayBind(json(payload)?),
        RelayMessageType::RelayAccepted => RelayMessage::RelayAccepted(json(payload)?),
        RelayMessageType::RelayReady => RelayMessage::RelayReady(json(payload)?),
        RelayMessageType::Error => RelayMessage::Error(json(payload)?),
    })
}

fn encode_p2p_payload(message: &P2pMessage) -> Result<Vec<u8>, ProtocolError> {
    Ok(match message {
        P2pMessage::P2pHello(value) => serde_json::to_vec(value)?,
        P2pMessage::P2pReady(value) => serde_json::to_vec(value)?,
        P2pMessage::Error(value) => serde_json::to_vec(value)?,
    })
}

fn decode_p2p_payload(message_type: u8, payload: &[u8]) -> Result<P2pMessage, ProtocolError> {
    Ok(match P2pMessageType::try_from(message_type)? {
        P2pMessageType::P2pHello => P2pMessage::P2pHello(json(payload)?),
        P2pMessageType::P2pReady => P2pMessage::P2pReady(json(payload)?),
        P2pMessageType::Error => P2pMessage::Error(json(payload)?),
    })
}

fn validate_announcement(message: &AnnounceCandidates) -> Result<(), ProtocolError> {
    validate_epoch(message.epoch)?;
    validate_candidates(&message.candidates)?;
    if message
        .candidates
        .iter()
        .any(|candidate| candidate.kind != CandidateKind::Host)
    {
        return Err(ProtocolError::InvalidAnnouncedCandidateKind);
    }
    Ok(())
}

fn validate_peer_record(message: &PeerRecord) -> Result<(), ProtocolError> {
    validate_request_id(message.request_id)?;
    validate_node_id(&message.node_id).map_err(|_| ProtocolError::InvalidNodeId)?;
    validate_overlay_ip(message.overlay_ip)?;
    if message.incarnation == 0 {
        return Err(ProtocolError::InvalidIncarnation);
    }
    validate_session_id(&message.session_id)?;
    validate_certificate_fingerprint(&message.certificate_fingerprint)?;
    validate_timestamp(message.certificate_not_after_unix_seconds)?;
    if message.epoch == 0 && !message.candidates.is_empty() {
        return Err(ProtocolError::InvalidEpoch);
    }
    validate_candidates(&message.candidates)
}

fn validate_control_welcome(message: &ControlWelcome) -> Result<(), ProtocolError> {
    validate_session_id(&message.session_id)?;
    validate_incarnation(message.incarnation)?;
    validate_overlay_binding(message.overlay_cidr, message.overlay_ip)?;
    validate_mtu(message.mtu)?;
    validate_timestamp(message.coordinator_time_unix_seconds)?;
    validate_timestamp(message.certificate_not_after_unix_seconds)?;
    if message.certificate_not_after_unix_seconds <= message.coordinator_time_unix_seconds {
        return Err(ProtocolError::InvalidCertificateValidity);
    }
    Ok(())
}

fn validate_peer_descriptor(message: &PeerDescriptor) -> Result<(), ProtocolError> {
    validate_node_id(&message.node_id).map_err(|_| ProtocolError::InvalidNodeId)?;
    validate_overlay_ip(message.overlay_ip)?;
    validate_incarnation(message.incarnation)?;
    validate_session_id(&message.session_id)?;
    validate_certificate_fingerprint(&message.certificate_fingerprint)?;
    validate_timestamp(message.certificate_not_after_unix_seconds)?;
    if message.candidate_epoch == 0 && !message.candidates.is_empty() {
        return Err(ProtocolError::InvalidEpoch);
    }
    validate_candidates(&message.candidates)
}

fn validate_connect_plan(message: &ConnectPlan) -> Result<(), ProtocolError> {
    validate_request_id(message.request_id)?;
    validate_session_id(&message.connection_id)?;
    validate_peer_descriptor(&message.peer)?;
    validate_timestamp(message.expires_at_unix_seconds)
}

fn validate_certificate_issued(message: &CertificateIssued) -> Result<(), ProtocolError> {
    validate_request_id(message.request_id)?;
    validate_certificate_chain(&message.certificate_chain_der_base64)?;
    validate_certificate_validity(
        message.not_before_unix_seconds,
        message.not_after_unix_seconds,
    )
}

fn validate_enroll_request(message: &EnrollRequest) -> Result<(), ProtocolError> {
    validate_session_id(&message.enrollment_id)?;
    validate_node_id(&message.node_id).map_err(|_| ProtocolError::InvalidNodeId)?;
    EnrollmentToken::parse(&message.enrollment_token)
        .map_err(|_| ProtocolError::InvalidEnrollmentToken)?;
    validate_der_base64(&message.csr_der_base64, MAX_CSR_DER_BYTES, "CSR")
}

fn validate_enroll_accepted(message: &EnrollAccepted) -> Result<(), ProtocolError> {
    validate_session_id(&message.enrollment_id)?;
    validate_node_id(&message.node_id).map_err(|_| ProtocolError::InvalidNodeId)?;
    validate_overlay_binding(message.overlay_cidr, message.overlay_ip)?;
    validate_mtu(message.mtu)?;
    validate_certificate_chain(&message.certificate_chain_der_base64)?;
    validate_der_base64(
        &message.node_ca_certificate_der_base64,
        MAX_CERTIFICATE_DER_BYTES,
        "node CA certificate",
    )?;
    validate_certificate_validity(
        message.not_before_unix_seconds,
        message.not_after_unix_seconds,
    )
}

fn validate_relay_accepted(message: &RelayAccepted) -> Result<(), ProtocolError> {
    validate_session_id(&message.relay_session_id)?;
    validate_mtu(message.mtu)?;
    if message.max_datagram_size < message.mtu {
        return Err(ProtocolError::InvalidDatagramSize);
    }
    Ok(())
}

fn validate_p2p_hello(message: &P2pHello) -> Result<(), ProtocolError> {
    validate_session_id(&message.connection_id)?;
    validate_session_id(&message.control_session_id)?;
    validate_incarnation(message.incarnation)?;
    validate_certificate_fingerprint(&message.certificate_fingerprint)
}

fn validate_certificate_chain(chain: &[String]) -> Result<(), ProtocolError> {
    if chain.is_empty() || chain.len() > MAX_CERTIFICATE_CHAIN_LEN {
        return Err(ProtocolError::InvalidCertificateChain);
    }
    for certificate in chain {
        validate_der_base64(certificate, MAX_CERTIFICATE_DER_BYTES, "certificate")?;
    }
    Ok(())
}

fn validate_der_base64(
    value: &str,
    max_decoded_len: usize,
    field: &'static str,
) -> Result<(), ProtocolError> {
    let decoded = STANDARD
        .decode(value)
        .map_err(|_| ProtocolError::InvalidDerBase64 {
            field,
            max: max_decoded_len,
        })?;
    if decoded.is_empty() || decoded.len() > max_decoded_len || STANDARD.encode(&decoded) != value {
        return Err(ProtocolError::InvalidDerBase64 {
            field,
            max: max_decoded_len,
        });
    }
    Ok(())
}

fn validate_certificate_validity(
    not_before_unix_seconds: u64,
    not_after_unix_seconds: u64,
) -> Result<(), ProtocolError> {
    validate_timestamp(not_before_unix_seconds)?;
    validate_timestamp(not_after_unix_seconds)?;
    if not_after_unix_seconds <= not_before_unix_seconds {
        return Err(ProtocolError::InvalidCertificateValidity);
    }
    Ok(())
}

fn validate_timestamp(timestamp: u64) -> Result<(), ProtocolError> {
    if timestamp == 0 {
        Err(ProtocolError::InvalidTimestamp)
    } else {
        Ok(())
    }
}

fn validate_mtu(mtu: u16) -> Result<(), ProtocolError> {
    if !(MIN_TUN_MTU..=MAX_TUN_MTU).contains(&mtu) {
        Err(ProtocolError::InvalidMtu)
    } else {
        Ok(())
    }
}

fn validate_incarnation(incarnation: u64) -> Result<(), ProtocolError> {
    if incarnation == 0 {
        Err(ProtocolError::InvalidIncarnation)
    } else {
        Ok(())
    }
}

fn validate_candidates(candidates: &[Candidate]) -> Result<(), ProtocolError> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(ProtocolError::TooManyCandidates {
            actual: candidates.len(),
            max: MAX_CANDIDATES,
        });
    }

    let mut addresses = HashSet::with_capacity(candidates.len());
    for candidate in candidates {
        if !is_usable_candidate_address(candidate.address) {
            return Err(ProtocolError::InvalidCandidateAddress(candidate.address));
        }
        if !addresses.insert(candidate.address) {
            return Err(ProtocolError::DuplicateCandidate(candidate.address));
        }
    }
    Ok(())
}

fn is_usable_candidate_address(address: SocketAddr) -> bool {
    let SocketAddr::V4(address) = address else {
        return false;
    };
    let ip = address.ip();
    address.port() != 0
        && ip.octets()[0] != 0
        && !ip.is_loopback()
        && !ip.is_multicast()
        && !ip.is_broadcast()
}

fn validate_overlay_ip(address: Ipv4Addr) -> Result<(), ProtocolError> {
    if address.is_unspecified() || address.is_multicast() || address.is_broadcast() {
        Err(ProtocolError::InvalidOverlayIp(address))
    } else {
        Ok(())
    }
}

fn validate_overlay_binding(overlay: Ipv4Net, address: Ipv4Addr) -> Result<(), ProtocolError> {
    if !(MIN_OVERLAY_PREFIX_LEN..=MAX_OVERLAY_PREFIX_LEN).contains(&overlay.prefix_len()) {
        return Err(ProtocolError::InvalidOverlayCidr);
    }
    validate_overlay_address(overlay, address)
        .map_err(|_| ProtocolError::InvalidOverlayBinding { address, overlay })
}

fn validate_certificate_fingerprint(value: &str) -> Result<(), ProtocolError> {
    let Some(hex) = value.strip_prefix(CERTIFICATE_FINGERPRINT_PREFIX) else {
        return Err(ProtocolError::InvalidCertificateFingerprint);
    };
    if hex.len() != SHA256_HEX_LEN
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProtocolError::InvalidCertificateFingerprint);
    }
    Ok(())
}

fn validate_session_id(value: &str) -> Result<(), ProtocolError> {
    let session_id = uuid::Uuid::parse_str(value).map_err(|_| ProtocolError::InvalidSessionId)?;
    if session_id.to_string() != value || session_id.get_version() != Some(uuid::Version::Random) {
        return Err(ProtocolError::InvalidSessionId);
    }
    Ok(())
}

fn validate_request_id(request_id: u64) -> Result<(), ProtocolError> {
    if request_id == 0 {
        Err(ProtocolError::InvalidRequestId)
    } else {
        Ok(())
    }
}

fn validate_epoch(epoch: u64) -> Result<(), ProtocolError> {
    if epoch == 0 {
        Err(ProtocolError::InvalidEpoch)
    } else {
        Ok(())
    }
}

fn validate_error(message: &ErrorMessage) -> Result<(), ProtocolError> {
    if let Some(request_id) = message.request_id {
        validate_request_id(request_id)?;
    }
    if message.message.is_empty()
        || message.message.len() > MAX_ERROR_MESSAGE_LEN
        || message.message.chars().any(char::is_control)
    {
        return Err(ProtocolError::InvalidErrorMessage);
    }
    Ok(())
}

pub async fn read_message<R>(reader: &mut R) -> Result<ControlMessage, ProtocolError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    read_stream_message(
        reader,
        MAX_JSON_PAYLOAD,
        decode_control_payload,
        |message| message.validate(),
    )
    .await
}

pub async fn read_message_from<R>(
    reader: &mut R,
    direction: MessageDirection,
) -> Result<ControlMessage, ProtocolError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    read_stream_message(
        reader,
        MAX_JSON_PAYLOAD,
        decode_control_payload,
        |message| {
            message.validate()?;
            message.validate_direction(direction)
        },
    )
    .await
}

async fn read_stream_message<R, T>(
    reader: &mut R,
    max_payload: usize,
    decode: fn(u8, &[u8]) -> Result<T, ProtocolError>,
    validate: impl FnOnce(&T) -> Result<(), ProtocolError>,
) -> Result<T, ProtocolError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    let mut header_bytes = [0_u8; HEADER_LEN];
    reader.read_exact(&mut header_bytes).await?;
    let header = parse_header(&header_bytes, max_payload)?;
    let mut payload = vec![0_u8; header.payload_len];
    reader.read_exact(&mut payload).await?;
    let message = decode(header.message_type, &payload)?;
    validate(&message)?;
    Ok(message)
}

pub async fn write_message<W>(writer: &mut W, message: &ControlMessage) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    let bytes = ProtocolCodec::encode(message)?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn write_message_for<W>(
    writer: &mut W,
    message: &ControlMessage,
    direction: MessageDirection,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    write_stream_bytes(writer, ProtocolCodec::encode_for(message, direction)?).await
}

pub async fn read_enrollment_message<R>(
    reader: &mut R,
    direction: MessageDirection,
) -> Result<EnrollmentMessage, ProtocolError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    read_stream_message(
        reader,
        MAX_JSON_PAYLOAD,
        decode_enrollment_payload,
        |message| {
            message.validate()?;
            message.validate_direction(direction)
        },
    )
    .await
}

pub async fn write_enrollment_message<W>(
    writer: &mut W,
    message: &EnrollmentMessage,
    direction: MessageDirection,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    write_stream_bytes(writer, EnrollmentCodec::encode(message, direction)?).await
}

pub async fn read_relay_message<R>(
    reader: &mut R,
    direction: MessageDirection,
) -> Result<RelayMessage, ProtocolError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    read_stream_message(
        reader,
        MAX_HANDSHAKE_PAYLOAD,
        decode_relay_payload,
        |message| {
            message.validate()?;
            message.validate_direction(direction)
        },
    )
    .await
}

pub async fn write_relay_message<W>(
    writer: &mut W,
    message: &RelayMessage,
    direction: MessageDirection,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    write_stream_bytes(writer, RelayCodec::encode(message, direction)?).await
}

pub async fn read_p2p_message<R>(
    reader: &mut R,
    direction: MessageDirection,
) -> Result<P2pMessage, ProtocolError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    read_stream_message(
        reader,
        MAX_HANDSHAKE_PAYLOAD,
        decode_p2p_payload,
        |message| {
            message.validate()?;
            message.validate_direction(direction)
        },
    )
    .await
}

pub async fn write_p2p_message<W>(
    writer: &mut W,
    message: &P2pMessage,
    direction: MessageDirection,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    write_stream_bytes(writer, P2pCodec::encode(message, direction)?).await
}

async fn write_stream_bytes<W>(writer: &mut W, bytes: Bytes) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, RngCore, SeedableRng, rngs::StdRng};

    fn host_candidate(last_octet: u8) -> Candidate {
        Candidate {
            address: SocketAddr::from(([192, 0, 2, last_octet], 4000 + u16::from(last_octet))),
            kind: CandidateKind::Host,
            priority: u32::from(last_octet),
        }
    }

    fn fingerprint() -> String {
        format!("sha256:{}", "ab".repeat(32))
    }

    fn uuid_a() -> String {
        "550e8400-e29b-41d4-a716-446655440000".to_owned()
    }

    fn uuid_b() -> String {
        "550e8400-e29b-41d4-a716-446655440001".to_owned()
    }

    fn der_base64(fill: u8) -> String {
        STANDARD.encode([0x30, 0x03, 0x02, 0x01, fill])
    }

    fn descriptor() -> PeerDescriptor {
        PeerDescriptor {
            node_id: "edge-b".to_owned(),
            overlay_ip: "10.42.0.3".parse().expect("overlay IP"),
            incarnation: 3,
            session_id: uuid_a(),
            certificate_fingerprint: fingerprint(),
            certificate_not_after_unix_seconds: 2_000_000_000,
            candidate_epoch: 9,
            candidates: vec![host_candidate(2)],
        }
    }

    fn messages() -> Vec<ControlMessage> {
        vec![
            ControlMessage::AnnounceCandidates(AnnounceCandidates {
                epoch: 1,
                candidates: vec![host_candidate(1)],
            }),
            ControlMessage::LookupPeer(LookupPeer {
                request_id: 7,
                overlay_ip: "10.42.0.3".parse().expect("overlay IP"),
            }),
            ControlMessage::PeerRecord(PeerRecord {
                request_id: 7,
                node_id: "edge-b".to_owned(),
                overlay_ip: "10.42.0.3".parse().expect("overlay IP"),
                incarnation: 3,
                session_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
                certificate_fingerprint: fingerprint(),
                certificate_not_after_unix_seconds: 2_000_000_000,
                epoch: 9,
                candidates: vec![
                    host_candidate(2),
                    Candidate {
                        address: "198.51.100.8:42000".parse().expect("candidate"),
                        kind: CandidateKind::ServerReflexive,
                        priority: 100,
                    },
                ],
            }),
            ControlMessage::PeerRevoked(PeerRevoked {
                node_id: "edge-b".to_owned(),
                overlay_ip: "10.42.0.3".parse().expect("overlay IP"),
                epoch: 10,
            }),
            ControlMessage::Error(ErrorMessage {
                request_id: Some(7),
                code: ErrorCode::PeerNotFound,
                message: "peer is not online".to_owned(),
                retryable: true,
            }),
            ControlMessage::ControlWelcome(ControlWelcome {
                session_id: uuid_a(),
                incarnation: 3,
                overlay_ip: "10.42.0.2".parse().expect("overlay IP"),
                overlay_cidr: "10.42.0.0/24".parse().expect("overlay CIDR"),
                mtu: 1100,
                certificate_not_after_unix_seconds: 2_000_000_000,
                coordinator_time_unix_seconds: 1_900_000_000,
            }),
            ControlMessage::ConnectRequest(ConnectRequest {
                request_id: 8,
                overlay_ip: "10.42.0.3".parse().expect("overlay IP"),
            }),
            ControlMessage::ConnectPlan(ConnectPlan {
                request_id: 8,
                connection_id: uuid_b(),
                role: ConnectionRole::Initiator,
                peer: descriptor(),
                expires_at_unix_seconds: 1_900_000_030,
            }),
            ControlMessage::RenewCertificate(RenewCertificate {
                request_id: 9,
                csr_der_base64: der_base64(1),
            }),
            ControlMessage::CertificateIssued(CertificateIssued {
                request_id: 9,
                certificate_chain_der_base64: vec![der_base64(2), der_base64(3)],
                not_before_unix_seconds: 1_900_000_000,
                not_after_unix_seconds: 2_000_000_000,
            }),
        ]
    }

    fn raw_frame(message_type: MessageType, payload: &[u8]) -> BytesMut {
        raw_frame_type(message_type as u8, payload)
    }

    fn raw_frame_type(message_type: u8, payload: &[u8]) -> BytesMut {
        let mut frame = BytesMut::with_capacity(HEADER_LEN + payload.len());
        frame.extend_from_slice(&MAGIC);
        frame.put_u16(VERSION);
        frame.put_u8(message_type);
        frame.put_u8(0);
        frame.put_u32(payload.len() as u32);
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn v2_alpns_are_role_specific_and_distinct() {
        assert_eq!(ENROLLMENT_ALPN, b"stellaris/enroll/2");
        assert_eq!(CONTROL_ALPN, b"stellaris/control/2");
        assert_eq!(RELAY_ALPN, b"stellaris/relay/2");
        assert_eq!(P2P_ALPN, b"stellaris/p2p/2");
        let alpns = [ENROLLMENT_ALPN, CONTROL_ALPN, RELAY_ALPN, P2P_ALPN];
        for (index, alpn) in alpns.iter().enumerate() {
            assert!(alpns[..index].iter().all(|earlier| earlier != alpn));
        }
    }

    #[test]
    fn all_message_types_round_trip() {
        for expected in messages() {
            let encoded = ProtocolCodec::encode(&expected).expect("encode");
            assert_eq!(&encoded[..4], b"STLR");
            assert_eq!(&encoded[4..6], &VERSION.to_be_bytes());
            assert_eq!(encoded[6], expected.message_type() as u8);
            assert_eq!(encoded[7], 0);

            let mut buffer = BytesMut::from(encoded.as_ref());
            assert_eq!(
                ProtocolCodec::decode(&mut buffer).expect("decode"),
                Some(expected)
            );
            assert!(buffer.is_empty());
        }
    }

    #[test]
    fn channel_codecs_round_trip_and_reject_wrong_directions() {
        let enrollment = [
            (
                EnrollmentMessage::EnrollRequest(EnrollRequest {
                    enrollment_id: uuid_a(),
                    node_id: "edge-a".to_owned(),
                    enrollment_token: EnrollmentToken::from_bytes([7; 32]).encode(),
                    csr_der_base64: der_base64(1),
                }),
                MessageDirection::AgentToCoordinator,
            ),
            (
                EnrollmentMessage::EnrollAccepted(EnrollAccepted {
                    enrollment_id: uuid_a(),
                    node_id: "edge-a".to_owned(),
                    overlay_ip: "10.42.0.2".parse().expect("overlay IP"),
                    overlay_cidr: "10.42.0.0/24".parse().expect("overlay CIDR"),
                    mtu: 1100,
                    certificate_chain_der_base64: vec![der_base64(2)],
                    node_ca_certificate_der_base64: der_base64(3),
                    not_before_unix_seconds: 1_900_000_000,
                    not_after_unix_seconds: 2_000_000_000,
                }),
                MessageDirection::CoordinatorToAgent,
            ),
        ];
        for (expected, direction) in enrollment {
            let encoded = EnrollmentCodec::encode(&expected, direction).expect("encode enrollment");
            let mut buffer = BytesMut::from(encoded.as_ref());
            assert_eq!(
                EnrollmentCodec::decode(&mut buffer, direction).expect("decode enrollment"),
                Some(expected.clone())
            );
            assert!(buffer.is_empty());

            let wrong = match direction {
                MessageDirection::AgentToCoordinator => MessageDirection::CoordinatorToAgent,
                MessageDirection::CoordinatorToAgent => MessageDirection::AgentToCoordinator,
                _ => unreachable!(),
            };
            assert!(matches!(
                EnrollmentCodec::encode(&expected, wrong),
                Err(ProtocolError::WrongDirection { .. })
            ));
        }

        let relay = [
            (
                RelayMessage::RelayBind(RelayBind {
                    control_session_id: uuid_a(),
                    incarnation: 3,
                }),
                MessageDirection::AgentToCoordinator,
            ),
            (
                RelayMessage::RelayAccepted(RelayAccepted {
                    relay_session_id: uuid_b(),
                    mtu: 1100,
                    max_datagram_size: 1200,
                }),
                MessageDirection::CoordinatorToAgent,
            ),
            (
                RelayMessage::RelayReady(RelayReady {
                    relay_session_id: uuid_b(),
                }),
                MessageDirection::AgentToCoordinator,
            ),
        ];
        for (expected, direction) in relay {
            let encoded = RelayCodec::encode(&expected, direction).expect("encode relay");
            let mut buffer = BytesMut::from(encoded.as_ref());
            assert_eq!(
                RelayCodec::decode(&mut buffer, direction).expect("decode relay"),
                Some(expected)
            );
        }

        let p2p = [
            (
                P2pMessage::P2pHello(P2pHello {
                    connection_id: uuid_b(),
                    control_session_id: uuid_a(),
                    incarnation: 3,
                    certificate_fingerprint: fingerprint(),
                }),
                MessageDirection::InitiatorToResponder,
            ),
            (
                P2pMessage::P2pReady(P2pReady {
                    connection_id: uuid_b(),
                    mtu: 1100,
                }),
                MessageDirection::ResponderToInitiator,
            ),
        ];
        for (expected, direction) in p2p {
            let encoded = P2pCodec::encode(&expected, direction).expect("encode P2P");
            let mut buffer = BytesMut::from(encoded.as_ref());
            assert_eq!(
                P2pCodec::decode(&mut buffer, direction).expect("decode P2P"),
                Some(expected)
            );
        }
    }

    #[test]
    fn directed_control_codec_enforces_message_origin() {
        for message in messages() {
            let direction = match message {
                ControlMessage::AnnounceCandidates(_)
                | ControlMessage::LookupPeer(_)
                | ControlMessage::ConnectRequest(_)
                | ControlMessage::RenewCertificate(_)
                | ControlMessage::Error(_) => MessageDirection::AgentToCoordinator,
                ControlMessage::PeerRecord(_)
                | ControlMessage::PeerRevoked(_)
                | ControlMessage::ControlWelcome(_)
                | ControlMessage::ConnectPlan(_)
                | ControlMessage::CertificateIssued(_) => MessageDirection::CoordinatorToAgent,
            };
            let encoded = ProtocolCodec::encode_for(&message, direction).expect("directed encode");
            let mut buffer = BytesMut::from(encoded.as_ref());
            assert_eq!(
                ProtocolCodec::decode_from(&mut buffer, direction).expect("directed decode"),
                Some(message)
            );
        }
    }

    #[test]
    fn decoder_preserves_partial_and_following_frames() {
        let first = ProtocolCodec::encode(&messages()[0]).expect("first frame");
        let second_message = messages()[1].clone();
        let second = ProtocolCodec::encode(&second_message).expect("second frame");

        let mut partial = BytesMut::from(&first[..HEADER_LEN + 1]);
        assert_eq!(ProtocolCodec::decode(&mut partial).expect("partial"), None);
        assert_eq!(partial.len(), HEADER_LEN + 1);

        let mut combined = BytesMut::from(first.as_ref());
        combined.extend_from_slice(&second);
        assert_eq!(
            ProtocolCodec::decode(&mut combined).expect("first decode"),
            Some(messages()[0].clone())
        );
        assert_eq!(
            ProtocolCodec::decode(&mut combined).expect("second decode"),
            Some(second_message)
        );
        assert!(combined.is_empty());
    }

    #[test]
    fn rejects_invalid_header_before_payload_allocation() {
        let encoded = ProtocolCodec::encode(&messages()[0]).expect("encode");

        let mut bad_magic = BytesMut::from(encoded.as_ref());
        bad_magic[0] = b'X';
        assert!(matches!(
            ProtocolCodec::decode(&mut bad_magic),
            Err(ProtocolError::InvalidMagic)
        ));

        let mut v1 = BytesMut::from(encoded.as_ref());
        v1[4..6].copy_from_slice(&1_u16.to_be_bytes());
        assert!(matches!(
            ProtocolCodec::decode(&mut v1),
            Err(ProtocolError::UnsupportedVersion(1))
        ));

        let mut bad_flags = BytesMut::from(encoded.as_ref());
        bad_flags[7] = 1;
        assert!(matches!(
            ProtocolCodec::decode(&mut bad_flags),
            Err(ProtocolError::UnsupportedFlags(1))
        ));

        let mut oversized = BytesMut::zeroed(HEADER_LEN);
        oversized[..4].copy_from_slice(&MAGIC);
        oversized[4..6].copy_from_slice(&VERSION.to_be_bytes());
        oversized[6] = MessageType::LookupPeer as u8;
        oversized[8..12].copy_from_slice(&((MAX_JSON_PAYLOAD + 1) as u32).to_be_bytes());
        assert!(matches!(
            ProtocolCodec::decode(&mut oversized),
            Err(ProtocolError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_unknown_and_duplicate_json_fields() {
        let mut unknown = raw_frame(
            MessageType::LookupPeer,
            br#"{"request_id":1,"overlay_ip":"10.42.0.2","extra":true}"#,
        );
        assert!(matches!(
            ProtocolCodec::decode(&mut unknown),
            Err(ProtocolError::InvalidJson(_))
        ));

        let mut duplicate = raw_frame(
            MessageType::LookupPeer,
            br#"{"request_id":1,"request_id":2,"overlay_ip":"10.42.0.2"}"#,
        );
        assert!(matches!(
            ProtocolCodec::decode(&mut duplicate),
            Err(ProtocolError::InvalidJson(_))
        ));

        let mut enrollment_unknown = raw_frame_type(
            EnrollmentMessageType::EnrollRequest as u8,
            br#"{"enrollment_id":"550e8400-e29b-41d4-a716-446655440000","node_id":"edge-a","enrollment_token":"redacted","csr_der_base64":"MAA=","extra":true}"#,
        );
        assert!(matches!(
            EnrollmentCodec::decode(
                &mut enrollment_unknown,
                MessageDirection::AgentToCoordinator
            ),
            Err(ProtocolError::InvalidJson(_))
        ));

        let mut enrollment_duplicate = raw_frame_type(
            EnrollmentMessageType::EnrollRequest as u8,
            br#"{"enrollment_id":"550e8400-e29b-41d4-a716-446655440000","node_id":"edge-a","node_id":"edge-b","enrollment_token":"redacted","csr_der_base64":"MAA="}"#,
        );
        assert!(matches!(
            EnrollmentCodec::decode(
                &mut enrollment_duplicate,
                MessageDirection::AgentToCoordinator
            ),
            Err(ProtocolError::InvalidJson(_))
        ));
    }

    #[test]
    fn enrollment_bounds_uuid_token_csr_and_certificates() {
        let valid = EnrollRequest {
            enrollment_id: uuid_a(),
            node_id: "edge-a".to_owned(),
            enrollment_token: EnrollmentToken::from_bytes([9; 32]).encode(),
            csr_der_base64: der_base64(1),
        };
        EnrollmentMessage::EnrollRequest(valid.clone())
            .validate()
            .expect("valid enrollment");
        assert!(!format!("{valid:?}").contains(&valid.enrollment_token));

        let mut invalid = valid.clone();
        invalid.enrollment_id = uuid::Uuid::new_v4().to_string().to_uppercase();
        assert!(matches!(
            EnrollmentMessage::EnrollRequest(invalid).validate(),
            Err(ProtocolError::InvalidSessionId)
        ));

        let mut invalid = valid.clone();
        invalid.enrollment_token = "stl3_not-base64".to_owned();
        assert!(matches!(
            EnrollmentMessage::EnrollRequest(invalid).validate(),
            Err(ProtocolError::InvalidEnrollmentToken)
        ));

        let mut invalid = valid.clone();
        invalid.csr_der_base64 = STANDARD.encode(vec![0_u8; MAX_CSR_DER_BYTES + 1]);
        assert!(matches!(
            EnrollmentMessage::EnrollRequest(invalid).validate(),
            Err(ProtocolError::InvalidDerBase64 { .. })
        ));

        let issued = CertificateIssued {
            request_id: 1,
            certificate_chain_der_base64: vec![der_base64(1)],
            not_before_unix_seconds: 10,
            not_after_unix_seconds: 20,
        };
        ControlMessage::CertificateIssued(issued.clone())
            .validate()
            .expect("valid certificate response");

        let mut invalid = issued.clone();
        invalid.certificate_chain_der_base64.clear();
        assert!(matches!(
            ControlMessage::CertificateIssued(invalid).validate(),
            Err(ProtocolError::InvalidCertificateChain)
        ));

        let mut invalid = issued;
        invalid.not_after_unix_seconds = invalid.not_before_unix_seconds;
        assert!(matches!(
            ControlMessage::CertificateIssued(invalid).validate(),
            Err(ProtocolError::InvalidCertificateValidity)
        ));
    }

    #[test]
    fn relay_and_p2p_handshakes_enforce_session_mtu_and_fingerprint_bounds() {
        let invalid_relay = RelayMessage::RelayAccepted(RelayAccepted {
            relay_session_id: uuid_a(),
            mtu: 1100,
            max_datagram_size: 1000,
        });
        assert!(matches!(
            invalid_relay.validate(),
            Err(ProtocolError::InvalidDatagramSize)
        ));

        let invalid_p2p = P2pMessage::P2pHello(P2pHello {
            connection_id: uuid_a(),
            control_session_id: uuid_b(),
            incarnation: 1,
            certificate_fingerprint: "sha256:ABC".to_owned(),
        });
        assert!(matches!(
            invalid_p2p.validate(),
            Err(ProtocolError::InvalidCertificateFingerprint)
        ));

        let invalid_ready = P2pMessage::P2pReady(P2pReady {
            connection_id: uuid_a(),
            mtu: MIN_TUN_MTU - 1,
        });
        assert!(matches!(
            invalid_ready.validate(),
            Err(ProtocolError::InvalidMtu)
        ));

        let invalid_ready = P2pMessage::P2pReady(P2pReady {
            connection_id: uuid_a(),
            mtu: MAX_TUN_MTU + 1,
        });
        assert!(matches!(
            invalid_ready.validate(),
            Err(ProtocolError::InvalidMtu)
        ));
    }

    #[test]
    fn announcement_enforces_epoch_kind_count_and_unique_usable_addresses() {
        let valid = AnnounceCandidates {
            epoch: 1,
            candidates: (1..=MAX_CANDIDATES as u8).map(host_candidate).collect(),
        };
        ControlMessage::AnnounceCandidates(valid.clone())
            .validate()
            .expect("maximum candidate count is valid");

        let mut invalid = valid.clone();
        invalid.epoch = 0;
        assert!(matches!(
            ControlMessage::AnnounceCandidates(invalid).validate(),
            Err(ProtocolError::InvalidEpoch)
        ));

        let mut invalid = valid.clone();
        invalid.candidates.push(host_candidate(17));
        assert!(matches!(
            ControlMessage::AnnounceCandidates(invalid).validate(),
            Err(ProtocolError::TooManyCandidates { .. })
        ));

        let mut invalid = valid.clone();
        invalid.candidates[0].kind = CandidateKind::ServerReflexive;
        assert!(matches!(
            ControlMessage::AnnounceCandidates(invalid).validate(),
            Err(ProtocolError::InvalidAnnouncedCandidateKind)
        ));

        let mut invalid = valid.clone();
        invalid.candidates[1].address = invalid.candidates[0].address;
        assert!(matches!(
            ControlMessage::AnnounceCandidates(invalid).validate(),
            Err(ProtocolError::DuplicateCandidate(_))
        ));

        for address in [
            "0.0.0.0:4000",
            "0.1.2.3:4000",
            "127.0.0.1:4000",
            "224.0.0.1:4000",
            "255.255.255.255:4000",
            "192.0.2.1:0",
            "[2001:db8::1]:4000",
        ] {
            let invalid = ControlMessage::AnnounceCandidates(AnnounceCandidates {
                epoch: 1,
                candidates: vec![Candidate {
                    address: address.parse().expect("socket address"),
                    kind: CandidateKind::Host,
                    priority: 1,
                }],
            });
            assert!(matches!(
                invalid.validate(),
                Err(ProtocolError::InvalidCandidateAddress(_))
            ));
        }
    }

    #[test]
    fn request_records_revocations_and_errors_enforce_semantic_bounds() {
        let mut lookup = match messages()[1].clone() {
            ControlMessage::LookupPeer(message) => message,
            _ => unreachable!(),
        };
        lookup.request_id = 0;
        assert!(matches!(
            ControlMessage::LookupPeer(lookup).validate(),
            Err(ProtocolError::InvalidRequestId)
        ));

        let mut record = match messages()[2].clone() {
            ControlMessage::PeerRecord(message) => message,
            _ => unreachable!(),
        };
        record.certificate_fingerprint = "SHA256:not-canonical".to_owned();
        assert!(matches!(
            ControlMessage::PeerRecord(record).validate(),
            Err(ProtocolError::InvalidCertificateFingerprint)
        ));

        for invalid_session_id in [
            "550E8400-E29B-41D4-A716-446655440000",
            "550e8400-e29b-11d4-a716-446655440000",
            "00000000-0000-0000-0000-000000000000",
        ] {
            let mut record = match messages()[2].clone() {
                ControlMessage::PeerRecord(message) => message,
                _ => unreachable!(),
            };
            record.session_id = invalid_session_id.to_owned();
            assert!(matches!(
                ControlMessage::PeerRecord(record).validate(),
                Err(ProtocolError::InvalidSessionId)
            ));
        }

        let mut record = match messages()[2].clone() {
            ControlMessage::PeerRecord(message) => message,
            _ => unreachable!(),
        };
        record.incarnation = 0;
        assert!(matches!(
            ControlMessage::PeerRecord(record).validate(),
            Err(ProtocolError::InvalidIncarnation)
        ));

        let mut record = match messages()[2].clone() {
            ControlMessage::PeerRecord(message) => message,
            _ => unreachable!(),
        };
        record.session_id = uuid::Uuid::nil().to_string();
        assert!(matches!(
            ControlMessage::PeerRecord(record).validate(),
            Err(ProtocolError::InvalidSessionId)
        ));

        let mut initial_record = match messages()[2].clone() {
            ControlMessage::PeerRecord(message) => message,
            _ => unreachable!(),
        };
        initial_record.epoch = 0;
        initial_record.candidates.clear();
        ControlMessage::PeerRecord(initial_record.clone())
            .validate()
            .expect("an empty initial peer record may use epoch zero");
        initial_record.candidates.push(host_candidate(8));
        assert!(matches!(
            ControlMessage::PeerRecord(initial_record).validate(),
            Err(ProtocolError::InvalidEpoch)
        ));

        let mut revoked = match messages()[3].clone() {
            ControlMessage::PeerRevoked(message) => message,
            _ => unreachable!(),
        };
        revoked.node_id = "-invalid".to_owned();
        assert!(matches!(
            ControlMessage::PeerRevoked(revoked).validate(),
            Err(ProtocolError::InvalidNodeId)
        ));

        let invalid_overlay = ControlMessage::LookupPeer(LookupPeer {
            request_id: 1,
            overlay_ip: Ipv4Addr::BROADCAST,
        });
        assert!(matches!(
            invalid_overlay.validate(),
            Err(ProtocolError::InvalidOverlayIp(_))
        ));

        let invalid_error = ControlMessage::Error(ErrorMessage {
            request_id: Some(0),
            code: ErrorCode::InvalidRequest,
            message: "invalid".to_owned(),
            retryable: false,
        });
        assert!(matches!(
            invalid_error.validate(),
            Err(ProtocolError::InvalidRequestId)
        ));

        let invalid_error = ControlMessage::Error(ErrorMessage {
            request_id: None,
            code: ErrorCode::Internal,
            message: "line one\nline two".to_owned(),
            retryable: false,
        });
        assert!(matches!(
            invalid_error.validate(),
            Err(ProtocolError::InvalidErrorMessage)
        ));
    }

    #[test]
    fn decoder_applies_semantic_validation() {
        let mut frame = raw_frame(
            MessageType::AnnounceCandidates,
            br#"{"epoch":0,"candidates":[]}"#,
        );
        assert!(matches!(
            ProtocolCodec::decode(&mut frame),
            Err(ProtocolError::InvalidEpoch)
        ));
    }

    #[test]
    fn arbitrary_input_is_handled_without_panicking() {
        let mut random = StdRng::seed_from_u64(0x57e1_1a21_0000_0002);
        for _ in 0..4096 {
            let length = random.gen_range(0..=(HEADER_LEN + MAX_JSON_PAYLOAD + 128));
            let mut input = vec![0_u8; length];
            random.fill_bytes(&mut input);
            let mut bytes = BytesMut::from(input.as_slice());
            let _ = ProtocolCodec::decode(&mut bytes);
        }
    }

    #[tokio::test]
    async fn async_helpers_round_trip() {
        let expected = messages()[2].clone();
        let (mut client, mut server) = tokio::io::duplex(MAX_JSON_PAYLOAD + HEADER_LEN);
        write_message(&mut client, &expected).await.expect("write");
        let actual = read_message(&mut server).await.expect("read");
        assert_eq!(actual, expected);
    }
}
