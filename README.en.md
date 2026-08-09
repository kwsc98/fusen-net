# Stellaris

[中文](README.md)

Stellaris is a distributed IPv4 overlay built on QUIC Datagrams and TUN. The
current `0.3.0-alpha.1` runtime combines a single coordinator, a trusted Relay,
and on-demand LAN P2P. Agents establish the Relay path first, request a
connection plan when traffic targets a peer, and switch that destination to a
Quinn P2P path only after mutual authentication and the Ready handshake.

> **Status: early preview.** The v2 runtime is wired into the CLI, but no gate
> record is currently bound to its implementation commit, so verification is
> Unverified. The full integration, Linux real-TUN, fault-injection, and
> resource-soak gates are incomplete. This alpha is not ready for critical
> production traffic.

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
transport abstractions remain compilable, but v2 configuration cannot select
them. NAT traversal, server-reflexive candidates, STUN, HA, ACLs, multi-tenancy,
IPv6, dynamic addresses, DNS, and subnet/default routing are deferred.

**Trusted Relay boundary:** peer traffic on a P2P QUIC path is protected by
mTLS between the Agents. With a genuine underlay host candidate it can bypass
the Server data plane. Fallback traffic is decrypted by the Relay, which can
observe complete overlay packets and traffic metadata. Candidate enumeration
has not yet proved that TUN/overlay addresses are excluded, so a P2P metric is
not by itself evidence that Relay was bypassed. Relay-path end-to-end
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

The Server does not create a TUN device. A Linux Agent needs `/dev/net/tun`, the
`ip` command from `iproute2`, and root or `CAP_NET_ADMIN`.

## Run

Start from [`configs/`](configs/). For an isolated development environment, the
repository helper creates short-lived test deployment TLS material. Production
deployments must use an organizational CA or public WebPKI instead.

```bash
cp ./configs/server.example.toml ./configs/server.toml
cp ./configs/agent.example.toml ./configs/agent.toml
cp ./configs/nodes.example.toml ./configs/nodes.toml
install -d -m 0700 ./configs/state
bash ./scripts/generate-dev-tls.sh ./configs/certs localhost
sudo install -d -o root -g root -m 0700 ./configs/secrets
sudo ./target/release/stellaris token generate \
  --node-id edge-a --output ./configs/secrets/edge-a.token
```

Set `registry.nodes_file = "nodes.toml"` in `configs/server.toml` and put the
printed digest in `configs/nodes.toml`. The service certificate SAN must match
`coordinator.server_name`, and the Agent must trust its deployment CA. Never
commit or use the helper-generated CA private key in production. Then validate
the final configuration and initialize the Server state:

```bash
./target/release/stellaris config check --config ./configs/server.toml
./target/release/stellaris server init --config ./configs/server.toml
```

Start the Server in the first terminal:

```bash
./target/release/stellaris server run --config ./configs/server.toml
```

Then check and start the Agent as the same effective user that owns its token
and identity directory. This compact example uses root because the Agent needs
TUN privileges:

```bash
sudo ./target/release/stellaris config check --config ./configs/agent.toml
sudo ./target/release/stellaris agent run --config ./configs/agent.toml
```

`server init` never replaces or rotates existing state. It may resume only when
a complete, strictly valid node CA certificate/key pair exists and the
coordinator state is still absent; every other partial, invalid, or
already-committed state combination is rejected. Normal startup will not
regenerate missing or invalid state. The only runtime override is `--config`,
also available as `STELLARIS_CONFIG`.

Every command that reads a secret or identity directory must run as the same
effective user that owns it. A dedicated Agent account with `CAP_NET_ADMIN` is
preferable to root, but token generation, configuration checking, and runtime
startup must all use that account. Server checking, initialization, and startup
must likewise use the account that owns its service key and state directory.

After enrollment succeeds and the identity directory is durably backed up,
`identity.enrollment_token_file` and its secret mount may be removed. A new
token and registry digest are required if the identity is lost or its
certificate has expired before it can renew over an authenticated control
session.

Open the Server enrollment, control, and Relay UDP ports and the Agent P2P UDP
ports required for direct LAN reachability. Stellaris does not alter the
default route or DNS.

The authoritative Chinese references are the
[documentation center](docs/README.md),
[documentation-driven change workflow](docs/documentation-workflow.md),
[design overview](docs/design-overview.md),
[modern design survey](docs/research/modern-distributed-network-survey.md),
[architecture](docs/architecture.md),
[v2 protocol](docs/protocol.md), [configuration](docs/configuration.md),
[security model](docs/security-model.md),
[proposed node identity, trust, and addressing plan](docs/node-identity-trust-addressing-plan.md),
and [remaining verification plan](docs/distributed-network-plan.md).

## License and contributions

Stellaris is available under `Apache-2.0 OR MIT`; choose either license. See
[LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT), and
[CONTRIBUTING.md](CONTRIBUTING.md). Contributions are submitted under the same
dual license by default. No CLA or DCO is required.
