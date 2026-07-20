# Container deployment

The relay and agent images are separate runtime targets built from the same `fusen-net` binary. QUIC listeners use UDP. The agent profile is Linux-only because it needs a TUN device.

## Prepare files

Create `deploy/docker/secrets/` locally and provide:

```text
server.pem       leaf-first server certificate chain with localhost in the leaf SAN
server-key.pem   matching private key
ca.pem           PEM bundle containing one or more CA certificates trusted by the agent
edge-a.token     token generated for edge-a
```

These files are intentionally ignored and must not be committed. Replace the placeholder digest in `configs/nodes.example.toml` with the digest printed by:

```sh
cargo run --locked -p fusen-net-cli -- token generate \
  --node-id edge-a \
  --output deploy/docker/secrets/edge-a.token
```

The Compose file uses long-form secret mounts so the relay's UID `10001` can read its
certificate and private key while the private key and token remain owner-readable only.
Use a Compose implementation that honors `uid`, `gid`, and `mode`; otherwise fix the
source file permissions and ownership before starting the containers. The application
rejects group/other-readable secret files on Unix.

## Run

Start only the unprivileged relay:

```sh
docker compose -f deploy/docker/compose.example.yml up --build relay
```

Start the relay and Linux agent:

```sh
docker compose -f deploy/docker/compose.example.yml --profile agent up --build
```

The relay drops all Linux capabilities. The agent is not privileged: it receives only `NET_ADMIN` and `/dev/net/tun`, then drops all other capabilities. Do not replace these settings with `privileged: true`.

Open UDP ports 7000, 7001, and 7002 for Quinn, s2n-quic, and gm-quic respectively. TCP port mappings do not carry Fusen Net traffic. Native macOS and Windows agents are not supported through these Linux images.
