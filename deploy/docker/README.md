# Container deployment

The Server and Agent images are separate targets built from the same
`stellaris` binary. All v2 runtime endpoints use Quinn over UDP. The Agent
image is Linux-only because it needs a TUN device.

> `0.3.0-alpha.1` is an early preview. The complete Linux real-TUN, failure,
> and soak gates have not passed. Use this Compose stack only on an isolated
> test network.

## Prepare files

Create `deploy/docker/secrets/` locally and provide:

```text
server.pem       leaf-first deployment service certificate chain; SAN includes "server"
server-key.pem   matching deployment service private key
ca.pem           deployment CA bundle trusted by the Agent
edge-a.token     one-time enrollment token for edge-a
```

The deployment CA is not the node CA. `server init` creates the node CA inside
the Server state volume; Agents receive only its certificate during enrollment.
Never commit either private key or the token.

Generate the token and replace the placeholder digest in
`configs/nodes.example.toml`:

```sh
cargo run --locked -p stellaris-cli -- token generate \
  --node-id edge-a \
  --output deploy/docker/secrets/edge-a.token
```

The Compose file uses long-form secret mounts. Use an implementation that
honors `uid`, `gid`, and `mode`, or set source ownership and permissions before
startup. Stellaris rejects group/other-readable secret files on Unix.

## Initialize and run

Create the node CA and coordinator state once:

```sh
docker compose -f deploy/docker/compose.example.yml run --rm server \
  server init --config /etc/stellaris/server.toml
```

The command never replaces or rotates existing state. If a previous attempt
stopped after a complete, strictly valid node CA certificate/key pair was
persisted but before coordinator state was created, rerunning the command keeps
that CA and completes initialization. Every other partial, invalid, or
already-committed state combination is rejected. Do not delete and reinitialize
the volume to recover from an ordinary startup error; restore a consistent
node-CA/coordinator-state backup instead.

Start the unprivileged coordinator and trusted Relay:

```sh
docker compose -f deploy/docker/compose.example.yml up --build server
```

Start the Server and the Linux Agent profile:

```sh
docker compose -f deploy/docker/compose.example.yml --profile agent up --build
```

The Server drops all Linux capabilities. The Agent receives only `NET_ADMIN`
and `/dev/net/tun`; do not replace these settings with `privileged: true`.

The Server state volume and each Agent identity volume must be persistent.
Agent identity contains the private key, certificate, node CA, and any pending
idempotent enrollment request. Losing it after token consumption prevents the
Agent from re-enrolling with that token. Never share one identity volume
between nodes.

After the first enrollment is durable, deployments may remove
`identity.enrollment_token_file` from `agent.toml` and the `agent_token` secret
mount. Re-enrollment after certificate expiry requires a newly generated token
and a restarted Server with the corresponding registry digest.

## Network

Open UDP 7000 for enrollment, 7001 for control, and 7002 for trusted Relay
traffic. These are separate protocol endpoints, not backend selectors. TCP
mappings do not carry Stellaris traffic.

The example Agent binds UDP 7100 for host-candidate P2P. For multiple Agents,
assign a unique persistent identity, node registry entry, underlay address, and
reachable P2P bind. Current P2P does not traverse NAT; when direct reachability
fails, subsequent packets remain on the trusted Relay.

Fallback traffic is decrypted by the Server and is visible to it. The current
release does not provide Relay-path end-to-end encryption. Native macOS and
Windows Agents are not supported through these Linux images and remain
compile-only/unverified outside containers as well.
