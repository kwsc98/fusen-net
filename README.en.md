# Fusen Net

[中文](README.md)

Fusen Net is a layer-3 virtual network built on QUIC Datagrams and TUN. The
0.1 architecture uses a central Relay: an Edge reads complete IPv4 packets
from a local TUN device, sends them over QUIC, and the Relay forwards each
packet to the authenticated Edge owning its destination overlay address.
The Chinese README and `docs/` are authoritative for 0.1 behavior; this file
is a compact entry point.

> **Status: early preview.** The `0.1.0-alpha` line is undergoing protocol,
> security, and portability work. It has not received an independent security
> audit and should not be exposed directly to untrusted production networks.
> It is incompatible with the old TCP port-forwarding CLI and protocol.

```text
Edge A             Relay with UDP listeners              Edge B
TUN <-> IPv4 <-> QUIC Datagram <-> overlay route <-> QUIC Datagram <-> TUN
```

An Edge and the Relay listener it connects to must use the same backend.
The Relay may listen with `quinn`, `s2n`, and `gm-quic` at the same time and
route packets between authenticated sessions using different backends.

The 0.1 scope is a central Relay, one trusted tenant, IPv4 only, static node
registration, server-authenticated TLS, per-node 256-bit tokens, and an MTU of
1100 by default. ACLs, IPv6, DNS, default-route takeover, NAT traversal, peer
discovery, and a full mesh are out of scope.

## Build

Rust 1.97.0 or later is required.

```bash
cargo build --workspace --all-features --locked
cargo test --workspace --all-features --locked
cargo build --release -p fusen-net-cli --all-features --locked
./target/release/fusen-net --version
```

The Relay does not create a TUN device and normally needs no elevated
privileges. An Agent needs permission to create a TUN interface and an overlay
route. On Linux that means `/dev/net/tun` and root or `CAP_NET_ADMIN`; macOS and
Windows require their corresponding administrative permissions.

## Run

Start from the files under [`configs/`](configs/). Supply a server certificate
whose SAN matches the Agent's `server_name`, and never commit its private key.

```bash
fusen-net token generate --node-id edge-a --output ./secrets/edge-a.token
fusen-net config check --config ./configs/server.example.toml
fusen-net server --config ./configs/server.example.toml
sudo fusen-net agent --config ./configs/agent.example.toml
```

Relay listeners are UDP. Fusen Net adds only the overlay CIDR route and does
not alter the default route or DNS. See the authoritative Chinese
[configuration reference](docs/configuration.md),
[architecture](docs/architecture.md), [protocol](docs/protocol.md), and
[security model](docs/security-model.md). Current backend and platform status
is tracked in [compatibility](docs/compatibility.md).

## License and contributions

Fusen Net is available under `Apache-2.0 OR MIT`; choose either license. See
[LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT), and
[CONTRIBUTING.md](CONTRIBUTING.md). Contributions are submitted under the same
dual license by default. No CLA or DCO is required.
