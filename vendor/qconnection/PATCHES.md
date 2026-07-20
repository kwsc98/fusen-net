# Fusen Net qconnection fork

This directory contains a minimally patched copy of `qconnection` 0.4.0 from
crates.io.

- Upstream: https://github.com/genmeta/gm-quic
- Crates.io version: `0.4.0`
- Upstream commit: `4cbd9d93871076fbf9f86ed1bdf64bcd6749c4cd`
- Crates.io checksum: `4f561741b24076d3160bb7e6f1a4de88f5aed3d14c9fc13a94dcf37ace2cc77a`
- Upstream license: Apache-2.0

The crates.io archive did not include a `NOTICE` file. This fork retains the
upstream package metadata and includes the full Apache-2.0 license text.

## Local patch

Upstream negotiates RFC 9221 and exposes `DatagramReader` and
`DatagramWriter`, but `Components::packages` leaves Datagram assembly as a
TODO. `DatagramWriter::send_bytes` therefore queues data that the 1-RTT packet
builder never consumes.

The local change in `src/path/burst.rs` wraps `DatagramFlow` in a package-local
newtype and appends it to the 1-RTT packet sources. `Repeat` preserves FIFO
order and drains all Datagram frames that fit in the current packet. Datagram
is intentionally not added to 0-RTT because overlay packets are not replay
safe.

The receive dispatcher in `src/space/data.rs` also delivers Datagram frames
directly to the bounded `DatagramFlow`. This removes an upstream unbounded
intermediate channel that otherwise bypasses the 256-entry receive limit in
the local `qunreliable` fork.

All remaining packet-space frame dispatch channels are bounded to 256 entries
per frame class. Reliable ACK, CRYPTO, stream, and connection-control frames
use non-blocking enqueue: a full queue or stopped consumer raises a QUIC
`INTERNAL_ERROR` and closes the connection instead of dropping a reliable
frame or retaining unbounded input. The secondary connection-event queue is
also bounded to 256 entries.

Receive errors from path validation, Datagram handling, and downstream frame
consumers now reach the connection event broker. A parser/dispatcher mismatch
in Initial, Handshake, or Data space raises `PROTOCOL_VIOLATION` rather than
panicking or silently ignoring the frame. Stateless Reset transitions the
connection to draining without using the upstream `todo!` path.

Focused unit tests verify FIFO Datagram encoding, full dispatch queue failure,
and Stateless Reset state transition. Fusen Net's backend transport contract
also tests bidirectional, empty, and 1100-byte Datagram delivery over gm-quic.

This fork must be removed when an upstream release includes an equivalent
fix and passes `backend_transport_contract` unchanged.
