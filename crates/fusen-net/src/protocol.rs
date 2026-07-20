// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fusen Net control-plane protocol version 1.
//!
//! A control frame consists of a fixed 12-byte header followed by a JSON
//! payload. Datagram payloads are not framed by this module: one QUIC
//! datagram carries exactly one IPv4 packet.

use std::{fmt, io, net::Ipv4Addr};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAGIC: [u8; 4] = *b"FNET";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 12;
pub const MAX_JSON_PAYLOAD: usize = 16 * 1024;
pub const ALPN: &[u8] = b"fusen-net/1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MessageType {
    Register = 1,
    RegisterAccepted = 2,
    Ready = 3,
    Error = 4,
}

impl TryFrom<u8> for MessageType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Register),
            2 => Ok(Self::RegisterAccepted),
            3 => Ok(Self::Ready),
            4 => Ok(Self::Error),
            other => Err(ProtocolError::UnknownMessageType(other)),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Register {
    pub node_id: String,
    pub token: String,
}

impl fmt::Debug for Register {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Register")
            .field("node_id", &self.node_id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RegisterAccepted {
    pub overlay_ip: Ipv4Addr,
    pub overlay: Ipv4Net,
    pub mtu: u16,
    pub session_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Ready {}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    AuthenticationFailed,
    DuplicateNode,
    InvalidRequest,
    ProtocolViolation,
    ServerBusy,
    Internal,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ErrorMessage {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlMessage {
    Register(Register),
    RegisterAccepted(RegisterAccepted),
    Ready(Ready),
    Error(ErrorMessage),
}

impl ControlMessage {
    pub const fn message_type(&self) -> MessageType {
        match self {
            Self::Register(_) => MessageType::Register,
            Self::RegisterAccepted(_) => MessageType::RegisterAccepted,
            Self::Ready(_) => MessageType::Ready,
            Self::Error(_) => MessageType::Error,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("incomplete control frame")]
    Incomplete,
    #[error("invalid control frame magic")]
    InvalidMagic,
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("unsupported frame flags 0x{0:02x}")]
    UnsupportedFlags(u8),
    #[error("unknown control message type {0}")]
    UnknownMessageType(u8),
    #[error("JSON payload is {actual} bytes; maximum is {max}")]
    PayloadTooLarge { actual: usize, max: usize },
    #[error("invalid control payload: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("control stream I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProtocolCodec;

impl ProtocolCodec {
    pub fn encode(message: &ControlMessage) -> Result<Bytes, ProtocolError> {
        let payload = match message {
            ControlMessage::Register(value) => serde_json::to_vec(value)?,
            ControlMessage::RegisterAccepted(value) => serde_json::to_vec(value)?,
            ControlMessage::Ready(value) => serde_json::to_vec(value)?,
            ControlMessage::Error(value) => serde_json::to_vec(value)?,
        };
        validate_payload_len(payload.len())?;

        let mut frame = BytesMut::with_capacity(HEADER_LEN + payload.len());
        frame.extend_from_slice(&MAGIC);
        frame.put_u16(VERSION);
        frame.put_u8(message.message_type() as u8);
        frame.put_u8(0);
        frame.put_u32(payload.len() as u32);
        frame.extend_from_slice(&payload);
        Ok(frame.freeze())
    }

    /// Decodes one frame and leaves any following frame in `buffer`.
    pub fn decode(buffer: &mut BytesMut) -> Result<Option<ControlMessage>, ProtocolError> {
        if buffer.len() < HEADER_LEN {
            return Ok(None);
        }

        let header = parse_header(&buffer[..HEADER_LEN])?;
        let frame_len =
            HEADER_LEN
                .checked_add(header.payload_len)
                .ok_or(ProtocolError::PayloadTooLarge {
                    actual: usize::MAX,
                    max: MAX_JSON_PAYLOAD,
                })?;
        if buffer.len() < frame_len {
            return Ok(None);
        }

        let message = decode_payload(header.message_type, &buffer[HEADER_LEN..frame_len])?;
        buffer.advance(frame_len);
        Ok(Some(message))
    }
}

#[derive(Clone, Copy, Debug)]
struct Header {
    message_type: MessageType,
    payload_len: usize,
}

fn parse_header(header: &[u8]) -> Result<Header, ProtocolError> {
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
    let message_type = MessageType::try_from(header[6])?;
    let flags = header[7];
    if flags != 0 {
        return Err(ProtocolError::UnsupportedFlags(flags));
    }
    let payload_len = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
    validate_payload_len(payload_len)?;
    Ok(Header {
        message_type,
        payload_len,
    })
}

fn validate_payload_len(payload_len: usize) -> Result<(), ProtocolError> {
    if payload_len > MAX_JSON_PAYLOAD {
        return Err(ProtocolError::PayloadTooLarge {
            actual: payload_len,
            max: MAX_JSON_PAYLOAD,
        });
    }
    Ok(())
}

fn decode_payload(
    message_type: MessageType,
    payload: &[u8],
) -> Result<ControlMessage, ProtocolError> {
    fn json<T: DeserializeOwned>(payload: &[u8]) -> Result<T, ProtocolError> {
        Ok(serde_json::from_slice(payload)?)
    }

    Ok(match message_type {
        MessageType::Register => ControlMessage::Register(json(payload)?),
        MessageType::RegisterAccepted => ControlMessage::RegisterAccepted(json(payload)?),
        MessageType::Ready => ControlMessage::Ready(json(payload)?),
        MessageType::Error => ControlMessage::Error(json(payload)?),
    })
}

pub async fn read_message<R>(reader: &mut R) -> Result<ControlMessage, ProtocolError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    let mut header_bytes = [0_u8; HEADER_LEN];
    reader.read_exact(&mut header_bytes).await?;
    let header = parse_header(&header_bytes)?;
    let mut payload = vec![0_u8; header.payload_len];
    reader.read_exact(&mut payload).await?;
    decode_payload(header.message_type, &payload)
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

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, RngCore, SeedableRng, rngs::StdRng};

    fn register() -> ControlMessage {
        ControlMessage::Register(Register {
            node_id: "edge-a".to_owned(),
            token: "a-secret-that-must-not-be-logged".to_owned(),
        })
    }

    #[test]
    fn header_is_exactly_twelve_bytes_and_network_ordered() {
        let frame = ProtocolCodec::encode(&register()).expect("encode");
        assert_eq!(&frame[..4], b"FNET");
        assert_eq!(&frame[4..6], &VERSION.to_be_bytes());
        assert_eq!(frame[6], MessageType::Register as u8);
        assert_eq!(frame[7], 0);
        assert_eq!(
            u32::from_be_bytes(frame[8..12].try_into().expect("length header")) as usize,
            frame.len() - HEADER_LEN
        );
    }

    #[test]
    fn fragmented_frame_is_not_consumed() {
        let encoded = ProtocolCodec::encode(&register()).expect("encode");
        for split_at in 0..encoded.len() {
            let mut partial = BytesMut::from(&encoded[..split_at]);
            assert_eq!(ProtocolCodec::decode(&mut partial).expect("decode"), None);
            assert_eq!(&partial[..], &encoded[..split_at]);
        }
    }

    #[test]
    fn coalesced_frames_are_decoded_one_at_a_time() {
        let first = register();
        let second = ControlMessage::Ready(Ready {});
        let first_bytes = ProtocolCodec::encode(&first).expect("first");
        let second_bytes = ProtocolCodec::encode(&second).expect("second");
        let mut buffer = BytesMut::from(first_bytes.as_ref());
        buffer.extend_from_slice(&second_bytes);

        assert_eq!(
            ProtocolCodec::decode(&mut buffer).expect("decode"),
            Some(first)
        );
        assert_eq!(
            ProtocolCodec::decode(&mut buffer).expect("decode"),
            Some(second)
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn all_message_types_round_trip() {
        let messages = [
            register(),
            ControlMessage::RegisterAccepted(RegisterAccepted {
                overlay_ip: "10.42.0.2".parse().expect("address"),
                overlay: "10.42.0.0/24".parse().expect("network"),
                mtu: 1100,
                session_id: "session-1".to_owned(),
            }),
            ControlMessage::Ready(Ready {}),
            ControlMessage::Error(ErrorMessage {
                code: ErrorCode::AuthenticationFailed,
                message: "credentials rejected".to_owned(),
                retryable: false,
            }),
        ];

        for expected in messages {
            let encoded = ProtocolCodec::encode(&expected).expect("encode");
            let mut buffer = BytesMut::from(encoded.as_ref());
            assert_eq!(
                ProtocolCodec::decode(&mut buffer).expect("decode"),
                Some(expected)
            );
        }
    }

    #[test]
    fn rejects_bad_header_fields_and_oversized_payload_before_allocation() {
        let encoded = ProtocolCodec::encode(&register()).expect("encode");

        let mut bad_magic = BytesMut::from(encoded.as_ref());
        bad_magic[0] = b'X';
        assert!(matches!(
            ProtocolCodec::decode(&mut bad_magic),
            Err(ProtocolError::InvalidMagic)
        ));

        let mut bad_version = BytesMut::from(encoded.as_ref());
        bad_version[4..6].copy_from_slice(&2_u16.to_be_bytes());
        assert!(matches!(
            ProtocolCodec::decode(&mut bad_version),
            Err(ProtocolError::UnsupportedVersion(2))
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
        oversized[6] = MessageType::Register as u8;
        oversized[8..12].copy_from_slice(&((MAX_JSON_PAYLOAD + 1) as u32).to_be_bytes());
        assert!(matches!(
            ProtocolCodec::decode(&mut oversized),
            Err(ProtocolError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_unknown_json_fields() {
        let payload = br#"{"node_id":"edge-a","token":"secret","extra":true}"#;
        let mut frame = BytesMut::with_capacity(HEADER_LEN + payload.len());
        frame.extend_from_slice(&MAGIC);
        frame.put_u16(VERSION);
        frame.put_u8(MessageType::Register as u8);
        frame.put_u8(0);
        frame.put_u32(payload.len() as u32);
        frame.extend_from_slice(payload);
        assert!(matches!(
            ProtocolCodec::decode(&mut frame),
            Err(ProtocolError::InvalidJson(_))
        ));
    }

    #[test]
    fn registration_debug_output_redacts_token() {
        let output = format!("{:?}", register());
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("a-secret-that-must-not-be-logged"));
    }

    #[test]
    fn arbitrary_input_is_handled_without_panicking() {
        let mut random = StdRng::seed_from_u64(0xf05e_0e7a_0000_0001);
        for _ in 0..4096 {
            let length = random.gen_range(0..=(HEADER_LEN + MAX_JSON_PAYLOAD + 128));
            let mut input = vec![0_u8; length];
            random.fill_bytes(&mut input);
            let mut bytes = BytesMut::from(input.as_slice());
            let _ = ProtocolCodec::decode(&mut bytes);
        }
    }

    #[test]
    fn encoder_enforces_json_limit() {
        let message = ControlMessage::Register(Register {
            node_id: "edge-a".to_owned(),
            token: "x".repeat(MAX_JSON_PAYLOAD),
        });
        assert!(matches!(
            ProtocolCodec::encode(&message),
            Err(ProtocolError::PayloadTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn async_helpers_round_trip() {
        let expected = register();
        let (mut client, mut server) = tokio::io::duplex(1024);
        write_message(&mut client, &expected).await.expect("write");
        let actual = read_message(&mut server).await.expect("read");
        assert_eq!(actual, expected);
    }
}
