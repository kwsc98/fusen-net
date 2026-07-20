# Fusen Net qunreliable fork

This directory contains a minimally patched copy of `qunreliable` 0.4.0 from
crates.io.

- Upstream: https://github.com/genmeta/gm-quic
- Crates.io version: `0.4.0`
- Upstream commit: `4cbd9d93871076fbf9f86ed1bdf64bcd6749c4cd`
- Crates.io checksum: `1d696ab6023d0f13ca2ad6377c779b87881dbdbacad63a8d50c2a98f96e3a004`
- Upstream license: Apache-2.0

The crates.io archive did not include a `NOTICE` file. This fork retains the
upstream package metadata and includes the full Apache-2.0 license text.

## Local patch

Upstream stores outgoing and incoming Datagram payloads in unbounded
`VecDeque` instances. The local fork fixes both queue capacities at 256:

- the 257th outgoing Datagram returns `io::ErrorKind::WouldBlock` and remains
  owned by the caller;
- the 257th incoming Datagram is dropped without logging or retaining its
  payload;
- `DatagramReader::dropped_queue_full()` exposes a saturating per-connection
  receive-drop counter without exposing packet contents;
- draining one entry allows the next Datagram to be accepted.

Production receive and send paths no longer use `unwrap` or `expect`. Poisoned
state locks are recovered with a warning, QUIC varint conversion is checked,
and packet serialization failures are returned to the caller. An impossible
locally queued payload whose length cannot be represented as a QUIC varint is
dropped explicitly so it cannot permanently block the queue head.

Focused boundary tests in `src/writer.rs` and `src/reader.rs` cover the exact
capacity, overflow behavior, recovery after one entry is drained, and lock
poison recovery. Fusen Net maps `WouldBlock` to `DatagramQueueFull` and an
oversized `InvalidInput` error to `DatagramTooLarge`.

This fork must be removed when an upstream release provides equivalent
bounded queues and passes the unchanged backend transport contract and soak
tests.
