// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr},
    str::FromStr,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::{BufMut, Bytes, BytesMut};
use ipnet::Ipv4Net;
use stellaris::{
    peer_manager::{
        ConnectionAction, ConnectionCloseReason, ConnectionDirection, PeerManager, ReadyOutcome,
        SelectedPath,
    },
    protocol::{
        CONTROL_ALPN, Candidate, CandidateKind, ConnectPlan, ConnectionRole, ENROLLMENT_ALPN,
        EnrollRequest, EnrollmentCodec, EnrollmentMessage, EnrollmentMessageType, MAGIC,
        MessageDirection, P2P_ALPN, PeerDescriptor, ProtocolError, RELAY_ALPN,
    },
    registry::{ENROLLMENT_TOKEN_BYTES, EnrollmentToken},
    routing::{
        PacketValidator, RouteError, RouteOutcome, RouteRegistration, RouteTable, SessionId,
    },
};
use uuid::Uuid;

#[cfg(unix)]
use stellaris::{
    coordinator_store::{
        CoordinatorStore, CoordinatorStoreError, EnrollmentCommit, EnrollmentCommitOutcome,
        PersistedEnrollmentResult, Sha256Fingerprint,
    },
    registry::{EnrollmentTokenDigest, StaticNode, StaticNodeRegistry},
};

const MTU: usize = 1_100;
const SOURCE_IP: Ipv4Addr = Ipv4Addr::new(10, 42, 0, 2);
const PEER_IP: Ipv4Addr = Ipv4Addr::new(10, 42, 0, 3);

fn overlay() -> Ipv4Net {
    "10.42.0.0/24".parse().expect("test overlay")
}

fn token(fill: u8) -> EnrollmentToken {
    EnrollmentToken::from_bytes([fill; ENROLLMENT_TOKEN_BYTES])
}

#[cfg(unix)]
fn registry(token: &EnrollmentToken) -> StaticNodeRegistry {
    StaticNodeRegistry::new(
        overlay(),
        [StaticNode::new(
            "edge-a",
            SOURCE_IP,
            EnrollmentTokenDigest::from_token(token),
            true,
        )],
    )
    .expect("v2 registry")
}

#[cfg(unix)]
fn enrollment_result(fill: u8) -> PersistedEnrollmentResult {
    let certificate = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        STANDARD.encode([fill])
    );
    PersistedEnrollmentResult {
        node_certificate_pem: certificate.clone(),
        node_ca_pem: certificate,
        certificate_fingerprint: Sha256Fingerprint::digest(&[fill]),
        certificate_not_after_unix: 4_102_444_800,
    }
}

#[cfg(unix)]
mod enrollment {
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        path::{Path, PathBuf},
    };

    use super::*;

    struct SecureTempDir(PathBuf);

    impl SecureTempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "stellaris-v2-acceptance-{}",
                Uuid::new_v4().hyphenated()
            ));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .expect("create secure test directory");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("secure test directory permissions");
            Self(path)
        }

        fn state_path(&self) -> impl AsRef<Path> {
            self.0.join("coordinator.json")
        }
    }

    impl Drop for SecureTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn enrollment_frame_registry_and_store_enforce_one_time_rotation() {
        let first_token = token(0x11);
        assert!(first_token.encode().starts_with("stl2_"));
        let first_registry = registry(&first_token);
        let enrollment_id = Uuid::new_v4().hyphenated().to_string();
        let request = EnrollmentMessage::EnrollRequest(EnrollRequest {
            enrollment_id: enrollment_id.clone(),
            node_id: "edge-a".to_owned(),
            enrollment_token: first_token.encode(),
            csr_der_base64: STANDARD.encode(b"csr-one"),
        });
        let encoded = EnrollmentCodec::encode(&request, MessageDirection::AgentToCoordinator)
            .expect("encode enrollment request");
        let mut frame = BytesMut::from(encoded.as_ref());
        let decoded = EnrollmentCodec::decode(&mut frame, MessageDirection::AgentToCoordinator)
            .expect("decode enrollment request")
            .expect("complete enrollment frame");
        assert!(frame.is_empty());
        let EnrollmentMessage::EnrollRequest(decoded) = decoded else {
            panic!("request frame changed message type");
        };

        let binding = first_registry
            .authenticate_enrollment(&decoded.node_id, &decoded.enrollment_token)
            .expect("static registry authenticates enrollment");
        assert_eq!(binding.overlay_ip, SOURCE_IP);

        let directory = SecureTempDir::new();
        let state_path = directory.state_path();
        let store = CoordinatorStore::initialize(&state_path, overlay(), 256)
            .expect("initialize v2 coordinator state");
        store
            .validate_registry(&first_registry)
            .expect("durable and static bindings agree");

        let first_request = STANDARD
            .decode(decoded.csr_der_base64.as_bytes())
            .expect("validated canonical CSR base64");
        let first_digest = EnrollmentTokenDigest::from_token(&first_token);
        let first_commit = EnrollmentCommit::from_material(
            binding.node_id,
            binding.overlay_ip,
            decoded.enrollment_id,
            &first_request,
            first_digest.clone(),
            b"spki-one",
            enrollment_result(1),
        );
        assert!(matches!(
            store
                .commit_enrollment(first_commit.clone())
                .expect("commit first enrollment"),
            EnrollmentCommitOutcome::Committed(_)
        ));
        assert!(matches!(
            store
                .commit_enrollment(first_commit)
                .expect("retry exact enrollment"),
            EnrollmentCommitOutcome::Replayed(_)
        ));

        let conflicting_request = Sha256Fingerprint::digest(b"different-csr");
        assert!(matches!(
            store.lookup_enrollment(
                "edge-a",
                SOURCE_IP,
                &enrollment_id,
                &conflicting_request,
                &first_digest,
            ),
            Err(CoordinatorStoreError::EnrollmentConflict)
        ));
        assert!(matches!(
            store.lookup_enrollment(
                "edge-a",
                SOURCE_IP,
                &Uuid::new_v4().hyphenated().to_string(),
                &Sha256Fingerprint::digest(&first_request),
                &first_digest,
            ),
            Err(CoordinatorStoreError::EnrollmentTokenConsumed)
        ));

        let replacement_token = token(0x22);
        let replacement_registry = registry(&replacement_token);
        assert!(
            first_registry
                .authenticate_enrollment("edge-a", &replacement_token.encode())
                .is_err()
        );
        let replacement_binding = replacement_registry
            .authenticate_enrollment("edge-a", &replacement_token.encode())
            .expect("replacement startup registry accepts the rotated token");
        store
            .validate_registry(&replacement_registry)
            .expect("token rotation preserves the static node binding");

        let replacement_commit = EnrollmentCommit::from_material(
            replacement_binding.node_id,
            replacement_binding.overlay_ip,
            Uuid::new_v4().hyphenated().to_string(),
            b"csr-two",
            EnrollmentTokenDigest::from_token(&replacement_token),
            b"spki-two",
            enrollment_result(2),
        );
        assert!(matches!(
            store
                .commit_enrollment(replacement_commit)
                .expect("commit token and key rotation"),
            EnrollmentCommitOutcome::Committed(_)
        ));
        assert!(!store.is_authorized_spki("edge-a", b"spki-one"));
        assert!(store.is_authorized_spki("edge-a", b"spki-two"));

        drop(store);
        let reopened =
            CoordinatorStore::open(&state_path, overlay(), 256).expect("reopen persisted v2 state");
        assert!(!reopened.is_authorized_spki("edge-a", b"spki-one"));
        assert!(reopened.is_authorized_spki("edge-a", b"spki-two"));
    }
}

fn raw_enrollment_frame(version: u16, payload: &[u8]) -> BytesMut {
    let mut frame = BytesMut::with_capacity(12 + payload.len());
    frame.extend_from_slice(&MAGIC);
    frame.put_u16(version);
    frame.put_u8(EnrollmentMessageType::EnrollRequest as u8);
    frame.put_u8(0);
    frame.put_u32(u32::try_from(payload.len()).expect("test payload length"));
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn public_v2_protocol_boundary_rejects_v1_direction_and_schema_extensions() {
    let alpns = [ENROLLMENT_ALPN, CONTROL_ALPN, RELAY_ALPN, P2P_ALPN];
    assert_eq!(alpns.into_iter().collect::<HashSet<_>>().len(), 4);
    assert!(alpns.iter().all(|alpn| alpn.ends_with(b"/2")));

    let request = EnrollmentMessage::EnrollRequest(EnrollRequest {
        enrollment_id: Uuid::new_v4().hyphenated().to_string(),
        node_id: "edge-a".to_owned(),
        enrollment_token: token(0x33).encode(),
        csr_der_base64: STANDARD.encode(b"csr"),
    });
    assert!(matches!(
        EnrollmentCodec::encode(&request, MessageDirection::CoordinatorToAgent),
        Err(ProtocolError::WrongDirection { .. })
    ));

    let valid = EnrollmentCodec::encode(&request, MessageDirection::AgentToCoordinator)
        .expect("valid v2 enrollment frame");
    let mut v1 = BytesMut::from(valid.as_ref());
    v1[4..6].copy_from_slice(&1_u16.to_be_bytes());
    assert!(matches!(
        EnrollmentCodec::decode(&mut v1, MessageDirection::AgentToCoordinator),
        Err(ProtocolError::UnsupportedVersion(1))
    ));

    let EnrollmentMessage::EnrollRequest(request) = request else {
        unreachable!("constructed enrollment request")
    };
    let payload = format!(
        "{{\"enrollment_id\":\"{}\",\"node_id\":\"{}\",\"enrollment_token\":\"{}\",\"csr_der_base64\":\"{}\",\"legacy_version\":1}}",
        request.enrollment_id, request.node_id, request.enrollment_token, request.csr_der_base64,
    );
    let mut unknown_field = raw_enrollment_frame(2, payload.as_bytes());
    assert!(matches!(
        EnrollmentCodec::decode(&mut unknown_field, MessageDirection::AgentToCoordinator,),
        Err(ProtocolError::InvalidJson(_))
    ));
}

fn session() -> SessionId {
    SessionId::from_str(&Uuid::new_v4().hyphenated().to_string()).expect("v4 session ID")
}

fn candidate() -> Candidate {
    Candidate {
        address: SocketAddr::from(([192, 168, 1, 3], 44_443)),
        kind: CandidateKind::Host,
        priority: 100,
    }
}

fn descriptor(
    incarnation: u64,
    peer_session: SessionId,
    certificate_expiry: u64,
) -> PeerDescriptor {
    PeerDescriptor {
        node_id: "edge-b".to_owned(),
        overlay_ip: PEER_IP,
        incarnation,
        session_id: peer_session.to_string(),
        certificate_fingerprint: format!("sha256:{}", "b".repeat(64)),
        certificate_not_after_unix_seconds: certificate_expiry,
        candidate_epoch: 1,
        candidates: vec![candidate()],
    }
}

fn connect_plan(peer: PeerDescriptor, role: ConnectionRole) -> ConnectPlan {
    ConnectPlan {
        request_id: 1,
        connection_id: session().to_string(),
        role,
        peer,
        expires_at_unix_seconds: 900,
    }
}

fn registration(node_id: &str, overlay_ip: Ipv4Addr, session_id: SessionId) -> RouteRegistration {
    RouteRegistration {
        node_id: node_id.to_owned(),
        overlay_ip,
        session_id,
    }
}

fn packet(packet_id: u64, source: Ipv4Addr, destination: Ipv4Addr) -> Bytes {
    let mut packet = vec![0_u8; 28];
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(&28_u16.to_be_bytes());
    packet[8] = 64;
    packet[9] = 17;
    packet[12..16].copy_from_slice(&source.octets());
    packet[16..20].copy_from_slice(&destination.octets());
    packet[20..28].copy_from_slice(&packet_id.to_be_bytes());
    Bytes::from(packet)
}

fn packet_id(packet: &[u8]) -> u64 {
    u64::from_be_bytes(packet[20..28].try_into().expect("packet ID payload"))
}

#[tokio::test]
async fn one_path_per_packet_falls_back_without_replaying_failed_p2p_datagram() {
    let manager =
        PeerManager::new("edge-a", overlay(), 8, Duration::from_secs(30)).expect("peer manager");
    let now = Instant::now();
    let plan = connect_plan(descriptor(1, session(), 1_000), ConnectionRole::Initiator);
    let plan_session = SessionId::from_str(&plan.connection_id).expect("plan session");
    let start = manager
        .start_connect_plan(&plan, now, 100)
        .expect("start LAN connection plan");
    assert!(matches!(
        start.actions.as_slice(),
        [ConnectionAction::Dial { .. }]
    ));

    let fingerprint = manager
        .snapshot(PEER_IP)
        .expect("planned peer")
        .certificate_fingerprint;
    let first = manager
        .mark_ready_for_plan(
            PEER_IP,
            plan_session,
            &fingerprint,
            ConnectionDirection::Inbound,
            now,
            100,
        )
        .expect("simultaneous inbound path");
    let ReadyOutcome::Activated {
        connection_id: reverse_connection,
    } = first
    else {
        panic!("first authenticated path must activate");
    };
    let preferred = manager
        .mark_ready_for_plan(
            PEER_IP,
            plan_session,
            &fingerprint,
            ConnectionDirection::Outbound,
            now,
            100,
        )
        .expect("simultaneous preferred path");
    let ReadyOutcome::Replaced {
        connection_id: p2p_connection,
        connection_to_close,
    } = preferred
    else {
        panic!("node ordering must select one simultaneous path");
    };
    assert_eq!(connection_to_close, reverse_connection);

    let routes = RouteTable::new(overlay(), 4).expect("relay route table");
    let validator = PacketValidator::new(overlay(), MTU).expect("packet validator");
    let source_session = session();
    let _source = routes
        .register(registration("edge-a", SOURCE_IP, source_session))
        .expect("source relay route");
    let stale_peer_session = session();
    let stale_peer_receiver = routes
        .reserve(registration("edge-b", PEER_IP, stale_peer_session))
        .expect("peer RelayBind reserves but does not activate");
    assert_eq!(
        routes.route_from(
            source_session,
            SOURCE_IP,
            packet(90, SOURCE_IP, PEER_IP),
            &validator,
        ),
        Err(RouteError::DestinationOffline(PEER_IP))
    );
    assert!(routes.activate_session(PEER_IP, stale_peer_session));
    drop(stale_peer_receiver);
    assert!(routes.remove_session(PEER_IP, stale_peer_session));

    let current_peer_session = session();
    let mut peer_receiver = routes
        .reserve(registration("edge-b", PEER_IP, current_peer_session))
        .expect("replacement RelayBind");
    assert!(routes.activate_session(PEER_IP, current_peer_session));
    assert!(!routes.remove_session(PEER_IP, stale_peer_session));
    assert!(routes.is_current_session(PEER_IP, current_peer_session));

    assert!(matches!(
        routes.route_from(
            source_session,
            SOURCE_IP,
            packet(91, Ipv4Addr::new(10, 42, 0, 99), PEER_IP),
            &validator,
        ),
        Err(RouteError::SourceSpoofed { .. })
    ));
    assert!(
        validator
            .validate_peer_inbound(&packet(92, SOURCE_IP, PEER_IP), SOURCE_IP, PEER_IP)
            .is_ok()
    );
    assert!(matches!(
        validator.validate_peer_inbound(
            &packet(92, Ipv4Addr::new(10, 42, 0, 99), PEER_IP),
            SOURCE_IP,
            PEER_IP,
        ),
        Err(RouteError::SourceSpoofed { .. })
    ));

    let mut selections = Vec::new();
    let mut p2p_deliveries = Vec::new();

    let first_packet = packet(101, SOURCE_IP, PEER_IP);
    let first_path = manager.select_path_at(PEER_IP, now, 100).selected;
    selections.push((packet_id(&first_packet), first_path));
    assert_eq!(first_path, SelectedPath::P2p(p2p_connection));
    p2p_deliveries.push(packet_id(&first_packet));

    let failed_packet = packet(102, SOURCE_IP, PEER_IP);
    let failed_path = manager.select_path_at(PEER_IP, now, 100).selected;
    selections.push((packet_id(&failed_packet), failed_path));
    assert_eq!(failed_path, SelectedPath::P2p(p2p_connection));
    assert!(manager.connection_closed(PEER_IP, start.generation, p2p_connection));
    // A failed P2P send changes only future selection. This packet is not
    // offered to Relay, which is the no-active-duplicate contract.

    let relay_packet = packet(103, SOURCE_IP, PEER_IP);
    let relay_path = manager.select_path_at(PEER_IP, now, 100).selected;
    selections.push((packet_id(&relay_packet), relay_path));
    assert_eq!(relay_path, SelectedPath::Relay);
    assert_eq!(
        routes
            .route_from(source_session, SOURCE_IP, relay_packet, &validator)
            .expect("route only the subsequent packet"),
        RouteOutcome::Forwarded
    );

    let relay_delivery = peer_receiver.recv().await.expect("one Relay delivery");
    assert_eq!(packet_id(&relay_delivery), 103);
    assert!(peer_receiver.try_recv().is_err());
    assert_eq!(p2p_deliveries, [101]);
    assert_eq!(
        selections
            .iter()
            .map(|(id, _)| *id)
            .collect::<HashSet<_>>()
            .len(),
        selections.len()
    );

    let expiry_plan = connect_plan(
        descriptor(1, plan.peer.session_id.parse().expect("session"), 1_000),
        ConnectionRole::Initiator,
    );
    let expiry_plan_session = expiry_plan
        .connection_id
        .parse()
        .expect("expiry plan session");
    let expiry_start = manager
        .start_connect_plan(&expiry_plan, now, 100)
        .expect("replacement plan after disconnect");
    let expiry_ready = manager
        .mark_ready_for_plan(
            PEER_IP,
            expiry_plan_session,
            &fingerprint,
            ConnectionDirection::Outbound,
            now,
            100,
        )
        .expect("replacement ready path");
    let ReadyOutcome::Activated {
        connection_id: expiring_connection,
    } = expiry_ready
    else {
        panic!("replacement path activates");
    };
    let expired = manager.select_path_at(PEER_IP, now, 1_000);
    assert_eq!(expired.selected, SelectedPath::Relay);
    assert!(matches!(
        expired.action,
        Some(ConnectionAction::Close {
            connection_id,
            reason: ConnectionCloseReason::CertificateExpired,
            ..
        }) if connection_id == expiring_connection
    ));
    assert_eq!(
        manager.select_path_at(PEER_IP, now, 1_000).action,
        None,
        "certificate expiry emits only one close action"
    );
    assert!(!manager.connection_closed(PEER_IP, expiry_start.generation, expiring_connection,));

    let idle_plan = connect_plan(descriptor(2, session(), 2_000), ConnectionRole::Responder);
    let idle_plan_session = idle_plan.connection_id.parse().expect("idle plan session");
    let idle_start = manager
        .start_connect_plan(&idle_plan, now, 100)
        .expect("new incarnation plan");
    let idle_fingerprint = manager
        .snapshot(PEER_IP)
        .expect("new incarnation")
        .certificate_fingerprint;
    let idle_ready = manager
        .mark_ready_for_plan(
            PEER_IP,
            idle_plan_session,
            &idle_fingerprint,
            ConnectionDirection::Inbound,
            now,
            100,
        )
        .expect("new incarnation ready");
    let ReadyOutcome::Activated {
        connection_id: idle_connection,
    } = idle_ready
    else {
        panic!("new incarnation activates");
    };
    let idle_actions = manager.maintenance(now + Duration::from_secs(30), 101);
    assert!(matches!(
        idle_actions.as_slice(),
        [ConnectionAction::Close {
            connection_id,
            reason: ConnectionCloseReason::Idle,
            ..
        }] if *connection_id == idle_connection
    ));
    assert_eq!(
        manager
            .select_path_at(PEER_IP, now + Duration::from_secs(30), 101)
            .selected,
        SelectedPath::Relay
    );
    assert!(!manager.connection_closed(PEER_IP, idle_start.generation, idle_connection,));
}
