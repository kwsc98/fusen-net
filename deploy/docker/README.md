# Container deployment

The Server and Agent images are separate targets built from the same
`stellaris` binary. All v2 runtime endpoints use Quinn over UDP. This example
targets a native Linux Docker Engine with `/dev/net/tun`; Docker Desktop on
macOS/Windows and rootless Docker are unverified and must fail closed if any
owner, mode, device, or capability preflight differs.

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

For an isolated development stack, generate short-lived test material from the
repository root and install the runtime files under the names used by Compose:

```sh
bash scripts/generate-dev-tls.sh deploy/docker/secrets/dev-tls server
install -m 0644 deploy/docker/secrets/dev-tls/deployment-server.pem \
  deploy/docker/secrets/server.pem
install -m 0644 deploy/docker/secrets/dev-tls/deployment-ca.pem \
  deploy/docker/secrets/ca.pem
sudo install -o 10001 -g 10001 -m 0400 \
  deploy/docker/secrets/dev-tls/deployment-server-key.pem \
  deploy/docker/secrets/server-key.pem
```

Do not mount `deployment-ca-key.pem` into either container, and do not use the
helper-generated CA in production.

Generate the token and replace the placeholder digest in
`configs/nodes.example.toml`:

```sh
cargo run --locked -p stellaris-cli -- token generate \
  --node-id edge-a \
  --output deploy/docker/secrets/edge-a.token
```

The Compose file declares the required target `uid`, `gid`, and `mode`. Some
file-backed Compose implementations ignore those attributes and preserve the
source file metadata instead. The Server image runs as UID/GID `10001`, while
the Agent image runs as root for its constrained TUN capability. Prepare the
source files as a fallback before startup:

```sh
sudo chown 10001:10001 deploy/docker/secrets/server-key.pem
sudo chmod 0400 deploy/docker/secrets/server-key.pem
sudo chown root:root deploy/docker/secrets/edge-a.token
sudo chmod 0400 deploy/docker/secrets/edge-a.token
chmod 0644 deploy/docker/secrets/server.pem deploy/docker/secrets/ca.pem
```

Stellaris rejects a service key or token whose owner differs from the process
effective UID, or which grants any group/other permissions. Verify what the
selected Compose implementation mounted before initialization:

```sh
docker compose -f deploy/docker/compose.example.yml run --rm --no-deps \
  --entrypoint /bin/sh server -c \
  'test "$(stat -c %u /run/secrets/server_key)" = 10001 &&
   test "$(stat -c %a /run/secrets/server_key)" = 400 &&
   test "$(stat -c %u /var/lib/stellaris)" = 10001 &&
   test "$(stat -c %a /var/lib/stellaris)" = 700'
docker compose -f deploy/docker/compose.example.yml run --rm --no-deps \
  --entrypoint /bin/sh agent -c \
  'test "$(stat -c %u /run/secrets/agent_token)" = 0 &&
   test "$(stat -c %a /run/secrets/agent_token)" = 400 &&
   test "$(stat -c %u /var/lib/stellaris)" = 0'
```

Stop if either command fails; changing the application to accept `0444`
secrets is not a workaround.

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
docker compose -f deploy/docker/compose.example.yml up -d --build server
```

Start the Server and the Linux Agent profile:

```sh
docker compose -f deploy/docker/compose.example.yml --profile agent up -d --build
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

Never start two containers with the same Agent identity volume. v2 has no
cross-process identity lock; duplicate use causes control/session replacement
and is not a high-availability mechanism.

An offline restore to a replacement host may copy the identity backup only
after the source Agent has stopped. Never run the source identity and its
restored copy concurrently.

Before backing up named volumes, stop the entire Compose project and follow the
consistency, version, hash, owner/mode, expired-leaf, and stale-snapshot rules in
[`docs/deployment.md`](../../docs/deployment.md#备份与恢复). The Compose example
does not yet provide a verified online snapshot or disaster-recovery helper;
an untested `docker cp` or a copy taken while the Server is running is not a
recoverable backup and cannot close a release gate.

## Network

Open UDP 7000 for enrollment, 7001 for control, and 7002 for trusted Relay
traffic. These are separate protocol endpoints, not backend selectors. TCP
mappings do not carry Stellaris traffic.

The example Agent binds UDP 7100 for host-candidate P2P. Within this single
Compose bridge, peers can use their container addresses. The example does not
publish or advertise a usable cross-host candidate: port publishing alone
would not rewrite the candidate, and current P2P has no NAT observation or
traversal. For multiple Agents, assign a unique persistent identity, registry
entry, underlay address, and reachable P2P bind; otherwise packets remain on
the trusted Relay.

Current candidate enumeration has not yet proved that TUN/overlay addresses
are excluded. Inspect the announced address and underlay route during tests;
a P2P metric alone does not prove that the container path bypassed Relay.

Fallback traffic is decrypted by the Server and is visible to it. The current
release does not provide Relay-path end-to-end encryption. Native macOS and
Windows Agents are not supported through these Linux images and remain
compile-only/unverified outside containers as well.
