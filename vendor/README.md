# Quinn datagram expiry patch

`quinn-proto/` is crates.io quinn-proto 0.11.18, copied without generated
cache metadata. MIT and Apache-2.0 license files are retained. Upstream:
https://github.com/quinn-rs/quinn. Original crates.io archive SHA-256:
`a9746dbde176634f4f2f1faf2404e30a31b2bc1e9cafb5329c95d8177a18c9fc`.
The included crate lockfile makes its standalone development tests reproducible;
ICU production builds resolve dependencies through the workspace lockfile.

The local patch adds `TransportConfig::datagram_send_max_age`, per-datagram
monotonic deadlines, expiry before `poll_transmit` packet construction and at
enqueue, and `ConnectionStats::datagrams_expired`. Default `None` preserves
upstream expiry behavior; metadata accounting includes the new deadline field. Expiry wakes blocked datagram writers. No wire,
TLS, congestion-control or stream behavior changes. `Datagrams::send_at` lets
unit simulations supply their own monotonic clock. The two deadline tests in
`src/connection/datagrams.rs` cover 200/500/1000/2000 ms stalls, slow drains,
metadata accounting and failed packet packing. The full QUIC regression in
`src/tests/mod.rs` checks actual delivery for 1/8/32/64 speakers and 8/400-byte
payloads after every stall, without a fresh enqueue triggering cleanup.

Validation on 2026-10-30: all 306 upstream/local library tests passed, including
23 datagram tests; standalone `cargo clippy --lib -- -D warnings` passed.

ICU uses 100 ms per outgoing application datagram queue. This bounds local
QUIC queue residence, not propagation time or bytes already handed to the OS.
Rebase this small patch when upgrading Quinn; do not silently replace it with
a byte-limit-only queue. Run the vendored crate tests as well as ICU transport
integration tests after rebasing.
