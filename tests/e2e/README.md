# Real TUN end-to-end tests

The workflow in `.github/workflows/tun-e2e.yml` is a manual release gate for isolated, disposable self-hosted runners. Hosted GitHub runners do not provide the privileges needed to validate native TUN setup and route rollback.

Runner labels:

| Platform | Required labels |
| --- | --- |
| Linux x86_64 | `self-hosted`, `Linux`, `X64`, `fusen-net-tun` |
| macOS x86_64 | `self-hosted`, `macOS`, `X64`, `fusen-net-tun` |
| Windows x86_64 | `self-hosted`, `Windows`, `X64`, `fusen-net-tun` |

Each runner must be disposable, run the test process as root/Administrator, and have outbound loopback/UDP networking. Linux requires `/dev/net/tun`, `iproute2`, `iputils ping`, and network-namespace support; fault scenarios additionally require `tc` with netem and u32 classifiers. Windows requires a provisioned Wintun driver. Do not attach an organization-wide persistent runner to this workflow because repository code executes with network-administration rights.

The executable contract is `crates/fusen-net/tests/real_tun.rs`, exposed as the ignored Cargo integration target `real_tun`. All platforms run `native_tun_protocol_and_route_lifecycle`: it creates the native adapter, installs the exact overlay route, sends real kernel ping/UDP/TCP traffic through the device to a packet-level responder, and verifies route plus interface removal within five seconds. A failure to create the device, run a system command, exchange a packet, or clean up a created resource fails the test.

One host network stack cannot run two production Edge instances honestly: both would install the same overlay CIDR, and both assigned addresses would be local to that stack. Linux therefore runs Edge A and Edge B in separate network namespaces and starts the real Relay/runtime over veth underlay links. The `standard` scenario verifies 100 lossless pings, UDP/TCP echo, all three matching QUIC backends, Relay restart recovery within 30 seconds, and graceful route rollback. macOS and Windows run the platform lifecycle test on one disposable runner; complete cross-host Relay/Edge qualification on those systems still requires a two-VM or two-runner controller and remains a stable-release blocker.

Linux has explicit `fault` and `soak` entries for all three backends. `fault` proves that netem caused nonzero loss and out-of-order delivery, then applies a size-selective silent-drop classifier, proves small packets still pass, proves a large packet is black-holed, removes the classifier, and proves recovery. `soak` defaults to 1800 seconds and continuously opens UDP/TCP exchanges before a final ping gate. It snapshots RSS, open descriptors, and thread counts for the Relay and both Edges after warm-up and fails on sustained growth beyond the documented test tolerances. `FUSEN_REAL_TUN_SOAK_SECONDS` may shorten development runs, but release evidence must use the default.

Run a platform and scenario from the Actions page, or locally on a disposable privileged host:

```bash
sudo tests/e2e/run-real-tun.sh linux standard
sudo tests/e2e/run-real-tun.sh linux fault
sudo tests/e2e/run-real-tun.sh linux soak
sudo tests/e2e/run-real-tun.sh macos standard
```

The scripts always use `Cargo.lock`, build all backend features, and select ignored tests by exact name. Development runs can narrow `standard`, `fault`, or `soak` with `FUSEN_REAL_TUN_BACKENDS`, `FUSEN_REAL_TUN_FAULT_BACKENDS`, or `FUSEN_REAL_TUN_SOAK_BACKENDS` respectively; release evidence must leave all three unset so each scenario retains `quinn s2n gm-quic`.
