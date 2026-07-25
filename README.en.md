# Stellaris

[中文](README.md)

Stellaris is a distributed IPv4 overlay built on QUIC Datagrams and TUN. The
current `0.3.0-alpha.1` runtime combines a single coordinator, a trusted Relay,
and on-demand LAN P2P. Agents establish the Relay path first, request a
connection plan when traffic targets a peer, and switch that destination to a
Quinn P2P path only after mutual authentication and the Ready handshake.

> **Status: early preview.** The v2 runtime is wired into the CLI, but the full
> integration, Linux real-TUN, fault-injection, and resource-soak gates are not
> complete. This alpha is not ready for critical production traffic.

The current scope is one tenant and trust domain, one coordinator instance,
IPv4-only static addresses, at most 256 nodes, and an MTU of 1100 by default.
Enrollment, control, Relay, and P2P use separate ALPNs. The Server exposes
three distinct UDP listeners; every Agent also binds one UDP socket for LAN
P2P.

Initial enrollment uses deployment TLS, a one-time `stl2_` token, and CSR
proof-of-possession. The Server-created node CA then issues short-lived mTLS
certificates for control, Relay, and peer connections. The deployment CA and
node CA are separate trust roots.

The v2 runtime uses Quinn only. The s2n-quic and gm-quic dependencies and
transport abstractions remain compileable, but v2 configuration cannot select
them. NAT traversal, server-reflexive candidates, STUN, HA, ACLs, multi-tenancy,
IPv6, dynamic addresses, DNS, and subnet/default routing are deferred.

**Trusted Relay boundary:** peer traffic on a P2P path is protected by QUIC
mTLS between the Agents. Fallback traffic is decrypted by the Relay, which can
observe complete overlay packets and traffic metadata. Relay-path end-to-end
encryption is not provided.

Linux is the first runtime release gate. macOS and Windows are currently
compile-only and their native TUN/P2P behavior is unverified. See the
[compatibility matrix](docs/compatibility.md).

## Build

Rust 1.97.0 or later is required.

```bash
cargo build --workspace --all-features --locked
cargo test --workspace --all-features --locked
cargo build --release -p stellaris-cli --no-default-features --features backend-quinn --locked
./target/release/stellaris --version
```

The Server does not create a TUN device. A Linux Agent needs `/dev/net/tun` and
root or `CAP_NET_ADMIN`.

## Run

Start from [`configs/`](configs/). Supply a deployment service certificate
whose SAN matches `coordinator.server_name`, and make the Agent trust its CA.

```bash
mkdir -p ./configs/secrets
stellaris token generate --node-id edge-a --output ./configs/secrets/edge-a.token
stellaris config check --config ./configs/server.example.toml
stellaris server init --config ./configs/server.example.toml
stellaris server run --config ./configs/server.example.toml
stellaris config check --config ./configs/agent.example.toml
sudo stellaris agent run --config ./configs/agent.example.toml
```

Put the printed enrollment-token digest, not the token, in the static node
registry. `server init` never replaces or rotates existing state. It may resume
only when a complete, strictly valid node CA certificate/key pair exists and
the coordinator state is still absent; every other partial, invalid, or
already-committed state combination is rejected. Normal startup will not
regenerate missing or invalid state. The only runtime override is `--config`,
also available as `STELLARIS_CONFIG`.

After enrollment succeeds and the identity directory is durably backed up,
`identity.enrollment_token_file` and its secret mount may be removed. A new
token and registry digest are required if an expired identity must enroll again.

Open the Server enrollment, control, and Relay UDP ports and the Agent P2P UDP
ports required for direct LAN reachability. Stellaris does not alter the
default route or DNS.

The authoritative Chinese references are the [architecture](docs/architecture.md),
[v2 protocol](docs/protocol.md), [configuration](docs/configuration.md),
[security model](docs/security-model.md), and
[remaining verification plan](docs/distributed-network-plan.md).

## License and contributions

Stellaris is available under `Apache-2.0 OR MIT`; choose either license. See
[LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT), and
[CONTRIBUTING.md](CONTRIBUTING.md). Contributions are submitted under the same
dual license by default. No CLA or DCO is required.
