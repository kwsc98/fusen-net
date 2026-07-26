# Linux real-TUN end-to-end gates

`.github/workflows/tun-e2e.yml` is a manual gate for an isolated, disposable
self-hosted Linux x86_64 runner. Hosted runners do not provide the privileges
needed for native TUN, routes, network namespaces, or fault injection.

Required labels are `self-hosted`, `Linux`, `X64`, `stellaris-tun`. Run the
test process as root. The host needs `/dev/net/tun`, Rust/Cargo, `ripgrep`,
`iproute2`, `ping`, and the native compiler/linker dependencies needed by the
Quinn feature; the complete gate also needs `tc`. Do not attach a persistent
organization-wide runner because repository code executes with
network-administration rights.
The workflow does not call `sudo`: the disposable runner service itself must
run as UID 0, and an explicit preflight fails before repository tests otherwise.

## Current executable coverage

The privileged harness is `crates/stellaris/tests/real_tun.rs`, exposed as the
ignored Cargo target `real_tun`.

The implemented `native_tun_protocol_and_route_lifecycle` smoke creates the
real Linux adapter, installs the overlay route, passes kernel ICMP/UDP/TCP to a
packet-level responder, and verifies route/interface cleanup. It does **not**
exercise enrollment, QUIC, Relay, P2P, Server restart, or two Agent runtimes.

Run it with:

```bash
sudo tests/e2e/run-real-tun.sh linux native
```

The script uses `Cargo.lock`, builds the real-TUN test with only the Quinn
runtime feature, selects the ignored test by exact name, and fails if the
expected test does not exist. All-feature compile coverage remains a separate
CI/release check.

## Required complete gate

```bash
sudo tests/e2e/run-real-tun.sh linux all
```

In addition to the native lifecycle smoke, `all` requires these exact tests:

| Test | Required behavior |
| --- | --- |
| `linux_v2_overlay_e2e` | two namespace-isolated Agents complete enrollment/control/Relay/P2P and exchange ping/TCP/UDP |
| `linux_v2_server_restart` | Server restart forces reauthentication without losing durable identity/state |
| `linux_v2_agent_restart` | Agent restart reuses identity, restores TUN/route, and does not reuse its token |
| `linux_v2_fault_injection` | loss, reordering, MTU black hole, P2P failure and Relay recovery |
| `linux_v2_soak` | at least 30 minutes of connection churn with bounded RSS/tasks/threads/fds/queues |

These complete tests are not all implemented in the current tree. The runner
therefore fails closed instead of treating a missing scenario as skipped. This
README and the workflow are a specification of the release gate, not evidence
that it passed.

The full scenarios must also record packet IDs across Relay/P2P transitions to
prove that Stellaris does not actively duplicate a packet, create a loop, or
resend a failed P2P packet through Relay. Session replacement, certificate
renewal/expiry, source spoofing, Ready gating, queue pressure, and route
rollback are required assertions.

macOS and Windows are compile-only/unverified for this release line. The shell
runner rejects them; future native support needs separate real multi-host or
multi-VM gates rather than extrapolating from Linux results.
