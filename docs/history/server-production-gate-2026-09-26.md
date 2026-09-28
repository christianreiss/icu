# Server production gate — 26–27 September 2026

> Pfadhinweis 2026-09-28: Im Hauptcheckout aufbewahrte Gate-Belege liegen jetzt
> unter `.local/gate/evidence/`. Die unten genannten `target/gate/`-Pfade
> dokumentieren die damaligen Läufe und Worktrees.


> Historischer Bericht: Ergebnisse gelten für den genannten Stand. Befehle in
> Codeblöcken bleiben als damaliger Nachweis erhalten. Für heutige Pfade und
> Befehle siehe [Verzeichnisübersicht](../README.md) und
> [Bauen](../development/building.md).


> **Covers commit** (stated 2026-09-27, external review P-1): the main gate
> below — test suite, static musl binary, rpm/deb `packaging/smoke.sh` on
> Debian 12 and AlmaLinux 10, `FROM scratch` container, systemd service test
> on biest.eulie.de with Checkmk and a manual backup/restore — ran on
> **`c449264`** (`ab79478` is formatting only; branch base `180fa55`, merged
> as `37462c1`). The full-room media soak ran on `38e34bc`; the second-round
> follow-up (test suite, Clippy, media soak with junk traffic) on `fa26a64`
> (on top of `8568d82`) — tests only, no smoke, service or Checkmk run.
>
> **Re-run on `f42a792`** (2026-09-27, after the third round of the external
> review; covers the com key with `ComSocket`, passport version 4, the video
> slot rework and the state backup): `packaging/build.sh` end to end —
> `cargo deny`/`cargo about` first, static musl server, clients fc42, fc43 and
> debian13, nfpm, `packaging/smoke.sh` on fc42, fc43, debian13 and
> server-only on debian12 and alma10 — passed; `BUILDINFO.linux` names
> `f42a792`. Workspace tests 749 passed, 0 failed, 8 ignored; Clippy
> `-D warnings`, fmt and `cargo deny` clean. Service test on biest.eulie.de
> with the shipped unit, isolated as `icu-server-gate-test` on
> 127.0.0.1/[::1]:14711: `systemd-analyze verify` clean, `systemd-analyze
> security` 1.4 OK; two headless clients registered with version-4 bundles
> (both over IPv4) and relayed audio, 998 of 998 datagrams, no drops, no
> com-tag drops; the shipped Checkmk plugin reported ICU_Server, ICU_Video,
> both certificates and ICU_Backup OK; with the service stopped, `revoke`
> through the offline CLI as root under umask 027 left `users.toml` at 0644
> and owned like the state directory, and the service started again (review
> S-2); the revoked client ended fatally after the server's proof, the other
> reconnected; automatic backups were written; after SIGKILL systemd
> restarted the service (`NRestarts=1`). Unit, configuration and state were
> removed afterwards. Not repeated on `f42a792`: the manual backup/restore
> drill, the media soak and the load tests. “No state schema or wire change”
> further down holds for `c449264` only.

## Decision

**Server software: passes the tested gates below after hardening. Actual public
production rollout: HOLD until the target host and an external network path are
verified.** No public production address was supplied or changed. This review
starts at `180fa553ed0dcb4c19431517fd7981ef18008bb2` in the separate worktree
`/home/chris/Documents/ICU-server-gate`, branch `fix/server-production-gate`.

Encryption means **QUIC/TLS 1.3 on both client/server legs**. The server is the
trusted relay and can read media; this is **not participant-to-participant E2EE**.
The highest shipping video tier is **320×240, 15 fps, 150 kbit/s H.264**. Audio is
48 kHz mono Opus, 48 kbit/s VBR, complexity 10, with FEC and Deep PLC. The server
preserves codec payloads; it does not transcode or lower quality. Neither HD
video nor protection against a compromised server is provided by this release.

## Fixes

| Finding | Change and proof |
| --- | --- |
| A viewer withholding stream credit could keep a video forwarding task blocked after the sender disconnected, then receive a stale frame. | Slot closure interrupts `open_uni` and writes. The new departure regression failed on the base code and passes after the fix. |
| A connected receiver could block video forwarding indefinitely through stream or byte flow control. | Opening and writing a frame share a bounded deadline, using the existing `video_frame_timeout_ms` (5 s by default). Expired writes are reset; the receiver can recover. Separate stream-credit and blocked-write tests pass. |
| Camera permission was checked only before reading a frame. | Recheck active sender/camera state after reading. A partial frame completed after camera-off is discarded; this regression failed before the fix. |
| The early return for zero viewers bypassed slot cleanup. | Close and clear previous receiver slots even when the last viewer stops watching. |
| Atomic writes reused predictable `.name.tmp` paths and followed symlinks; creation mode did not protect against an existing file's permissions. | Use exclusively created random temporary files with the requested permissions, then fsync and atomic rename. A fixture proved that the old writer overwrote an unrelated symlink target; the fixed writer preserves it and writes private material as 0600. `tempfile` was already locked and used by the workspace; it is now also a normal server dependency. |
| Service-created files depended on the inherited umask. | Add `UMask=0077`; private state and keys were verified as 0600 under systemd. |
| The load test checked relay enqueue counts without requiring actual receiver delivery. | Assert sent = server input and received = server output. Await aborted reader tasks before reclaiming their buffers. The packet-loss test now also requires useful receiver delivery. |

KISS: retain the existing slot design and timeout setting, read video directly
into one frame buffer instead of copying a second payload buffer, and reuse
`tempfile` rather than implementing a new atomic-file abstraction. No protocol
version, codec profile or client behavior changes. Existing rustfmt drift is
normalized so the canonical formatting check passes; client implementation
changes in this branch are formatting only. Update the relay documentation to
match buffering and QUIC enqueue semantics (`finish` is not delivery proof).

## Verification

All tests use fresh fixture identities and state. Synthetic audio and video go
through real Opus/H.264 codecs and authenticated QUIC connections.

| Gate | Result |
| --- | --- |
| `cargo test --locked --workspace` | **596 passed**, zero failures; five explicitly ignored tests in the default run. All three ignored server tests (load, netem and ten-minute idle) passed when run separately on the final binary. The remaining client-only exclusions are the decoder benchmark and desktop notification test. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed. |
| `cargo fmt --all -- --check`; `git diff --check` | Passed. |
| `cargo deny --locked check` | Advisories, bans, licenses and sources passed. Existing duplicate-version/license-metadata warnings remain; advisory exceptions are the existing documented client/build dependencies. No new ignored advisory. |
| `cargo about generate --locked --fail packaging/about.hbs` | License inventory generated successfully for the packages. |
| Bidirectional real client media, `tests/media_gate.rs` | Both clients each received **250 audio packets and 76 video frames in 5 s**, decoded non-silent audio and 320×240 pictures, and remained at video tier 0. No relay rate, malformed or slow-receiver drops. |
| Server video regressions | 19 tests passed, including max 512 KiB frame delivery, reconnect-persistent budget, stalled input, blocked output and camera-off. |
| 16 clients × 50 audio datagrams/s, 60 s | **48,016 input packets; 720,240 forwarded and received; zero loss or send errors.** Loopback latency p50 1.067 ms, p99 1.464 ms, maximum 2.238 ms. Debug harness CPU 37.1% of one core includes all 16 clients and the server; this is not a server-only CPU measurement. |
| Real `tc netem`, isolated network namespace | 30 ms ± 10 ms delay, 2% loss: 500 sent, **490 server input = 490 output**, 483 received. No extra relay loss. The host's network/qdiscs were untouched. |
| Ten-minute idle keepalive | Passed on the final server test binary: connection remained authenticated and open through 600 s of silence, then sent audio. |
| Final static musl binary | Built with pinned `rust:1.98.1-alpine`, `--locked --profile release-server`; `file` confirms static PIE. Panic unwinding retained for session containment. |
| rpm/deb installation | Canonical `packaging/smoke.sh`, server-only on **Debian 12 and AlmaLinux 10**, passed: binary, group/config permissions, unit validation, local configuration survives reinstall, state survives uninstall. These review artifacts are not published. |
| `FROM scratch` container | Built the repository Dockerfile; status, invitation creation and certificate rotation passed as uid 65532 with a fresh named state volume. Test container and volume removed. |

### Full-room concurrent media follow-up — 27 September

`crates/icu-server/tests/media_load.rs` adds two explicit 30-second soaks with
16 mutually authenticated clients, each sending 50 maximum-size (400-byte)
audio payloads/s and 15 3000-byte video payloads/s (360 kbit/s). This tests
relay capacity and payload integrity; the payloads are opaque test patterns,
so it complements the separate real-codec test rather than measuring decoded
quality. Application send rates stay fixed; client quality adaptation is not
modeled here. Only enrollment/handshake setup burst limits are increased;
production media limits remain unchanged.

- All receivers healthy: **360,000/360,000 audio packets and
  108,000/108,000 video frames** arrived with intact payloads; audio p99 **4.916 ms**.
- One authenticated viewer advertises zero video-stream credit: all
  **360,000 audio packets** and all **101,250 video frames** addressed to healthy
  receivers arrived intact; audio p99 **4.767 ms**. The refusal was counted as
  slow-receiver drops without disconnecting the remaining clients.
- Both tests passed. Strict server Clippy and formatting checks passed.
- These are two additional opt-in tests; they do not change the 596-test
  default-suite result recorded above. Logs: `target/gate/evidence/icu-server-gate-media-load.log`.

```sh
cargo test --locked -p icu-server --test media_load -- --ignored --nocapture --test-threads=1
# For longer runs (10–600 seconds): prefix ICU_MEDIA_SECONDS=300
```

### External review follow-up, second round — 27 September

Code at `fa26a64` (branch `fix/review-2026-09-27-round2`, on top of `8568d82`):

- **Lost `users.toml` beside the keys** (`9de7922`): `Store::open` creates an
  empty user database only in a state directory with no CA, server leaf or
  com key, and writes it at once; beside any of them a missing `users.toml`
  is a startup error naming the files found, and nothing is written.
  `a_lost_user_database_is_not_replaced_beside_the_keys` (start, enroll,
  stop, delete only `users.toml`: `run` and the offline CLI both fail, no
  file appears) and `a_new_state_gets_its_user_database_first` (offline CLI
  first, then the first start) in `tests/auth.rs`.
- **In-flight deadlines during the next open/write** (`a0a3101`):
  `a_frame_in_flight_is_given_up_while_the_next_waits_for_a_stream` in
  `tests/slow_viewer.rs` (one stream of credit, downlink cut, frame 2 waiting
  for a stream while frame 1 is in flight). It **fails against the forwarder of
  `8568d82`** (frame 1 given up only at frame 2's deadline) and passes now.
- **Junk metering** (`fa26a64`): only datagrams the socket accepted count as
  sent; the soak prints the sent and filtered rates and checks the filter
  drops no more than arrived.

| Check | Result |
|---|---|
| `cargo test --locked --workspace --no-fail-fast` | **663 passed**, 0 failed, 8 ignored (the opt-in soaks, load, netem, idle and client-only exclusions). |
| `cargo clippy --locked --workspace --all-targets -- -D warnings`; `cargo fmt --all -- --check`; `git diff --check` | Passed. |
| Media soak, 16 clients, 30 s, no junk | All receivers healthy: **360,000/360,000 audio, 108,000/108,000 video**, audio p99 8.472 ms. Zero-credit viewer: **360,000 audio, 101,250/101,250 video** to healthy receivers, p99 7.666 ms. |
| Same, `ICU_MEDIA_JUNK_PPS=10000` | All healthy: **299,960 junk datagrams sent (9,998/s), 299,960 dropped by the com filter (9,998/s)**; 360,000/360,000 audio, 108,000/108,000 video, p99 7.875 ms. Zero-credit viewer: **300,010 sent, 300,010 filtered (10,000/s)**; 360,000 audio, 101,250/101,250 video, p99 7.548 ms. No disconnects, no rate or stall drops. |

The p99 values are higher than in the follow-up above (4.9 ms) with and
without junk alike: this is a debug build on a shared workstation, and the
junk runs are not slower than the baseline of the same session. Loopback
only; this says nothing about a WAN path. Log:
`target/gate/evidence/icu-server-media-load-2026-09-27-round2.log`.

```sh
ICU_MEDIA_JUNK_PPS=10000 cargo test --locked -p icu-server --test media_load -- --ignored --nocapture --test-threads=1
```

### Service test on biest.eulie.de

An isolated unit, `icu-server-gate-test.service`, used the shipped security
settings, with distinct dynamic group, state/runtime directories and executable
path. It bound **127.0.0.2:14711 and [::1]:14711**, leaving the existing ICU test
servers on 4711/4712 untouched. `systemd-analyze security` returned **1.4 OK**.

The **final static binary**, including secure atomic writes, was exercised by
two standalone real clients: one connected over IPv4, one over IPv6; both sent
synthetic camera and voice. At the first sample, 368/368 video frames were
forwarded and every drop counter was zero. Audio input/output differ during
the first client's solo startup, as expected. After live certificate rotation
and service restart, both identities reconnected without enrollment: **620
received/forwarded audio packets and 188/188 video frames**, all drop counters
zero. Revoking `StaticBob` disconnected only that client. Client logs showed
video tier 0 and no playback underruns.

The initial release binary also recovered automatically from an intentional
SIGKILL (`NRestarts=1`), retaining identities and revocation. The shipped Checkmk
script reported server, server certificate and CA certificate **OK** against the
fixture service. Peak service memory after the final static-binary media run
was about 7.8 MiB; this is a two-client smoke measurement, not a capacity bound.

Backup/restore passed: stopped the fixture, archived its full state, restored
into a fresh directory, rotated the server leaf, and reconnected the existing
StaticAlice identity. The CA fingerprint was unchanged and StaticBob remained
revoked.
Cleanup: the temporary systemd unit, runtime files, state/backup and client
identities were removed. Port 14711 is closed and no test container remains.
Builds, review packages and sanitized evidence logs remain under this
worktree’s ignored `target/gate/evidence/` and `dist/0.1.0/` directories.

### Artifact checksums

Final static server SHA-256:
`9d21ec096ed1557a0e9b87a966cecc9f1c6ed079f14dc726281f9fb9858b007d`

Review packages in this worktree's ignored `dist/0.1.0/`:

- rpm: `fa068887edf96ca9173d1dcd0141ebc50b5c2aad4a4a4851dc3a7b285146edef`
- deb: `6f190a651b07033790a83a6d92a2137ae78da6f1964ef2221ea89a9b4845c5fc`

## Security review coverage and boundaries

- Transport: TLS 1.3 only, early data disabled, server-CA verification and
  authenticated client identities. Anonymous connections receive `AUTH_REQUIRED`
  immediately after TLS; they never enter the room. Unknown CA, certificate
  revocation/disable, expired renew-only identities and downgrade rejection are
  covered by server/client tests. The pinned rustls 0.23.45 includes the fix for
  [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html).
- Authorization: in-band admin checks execute under the store lock against the
  session certificate; ordinary clients cannot mint identities or alter roles.
  Forced mute/deafen, revocation races, renewals and replacement sessions are
  exercised by the integration suite. The local admin socket is intentionally
  fully privileged; its Unix permissions are the access boundary.
- Resource limits: bounded handshake/connection counts, IPv6 prefix budgets,
  Retry under load, handshake/Hello/input deadlines, bounded flow-control
  windows, control queues and video concurrency/budgets. Adversarial handshake
  tests check that a legitimate client can still connect. Outbound video waits
  are now bounded too. Volumetric UDP denial of service still needs provider or
  upstream filtering.
- State: incomplete/lost CA fails closed; rotation, durable retired leaves,
  revocation persistence, failed writes and restore semantics are covered.
  Exports/passports are bearer identities and must remain private. TLS protects
  network traffic; it does not protect plaintext in a compromised server's RAM.
- Quality: the server keeps audio separate from reliable video streams and
  prioritizes control above video. Payload integrity, real decoding, FEC/PLC,
  jitter-buffer behavior and simultaneous A/V pass the suite. Actual WAN
  latency/loss, speakers/microphones, acoustic AEC and physical camera quality
  cannot be inferred from loopback or synthetic fixtures.

## Remaining production gates

1. Identify the actual public host/name, deployed artifact and required room
   size. Verify UDP reachability, IPv4/IPv6, firewall/NAT and reconnect from an
   independent external network. This workstation test is not that evidence.
2. Verify the real service's effective sandbox, resource limits, monitoring,
   private state ownership and a restored backup of its own CA/users state.
3. Run the intended Windows/Linux clients on physical hardware and the actual
   WAN path, with concurrent audio/video and an idle/reconnect period. Client
   hardware acceptance remains outside this server software gate.
4. Confirm that the shipping trusted-relay and 320×240 overlay profile meets
   the release requirement. If “fully encrypted” means E2EE even against the
   server, or “highest quality” means HD video, this protocol/client release
   does not satisfy that requirement; server limit changes cannot add it.

## Reproduce and roll back

```sh
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo deny --locked check
cargo test --locked -p icu-client --test media_gate -- --nocapture
ICU_LOAD_SECONDS=60 cargo test --locked -p icu-server --test load -- --ignored --nocapture datagram_load
cargo test --locked -p icu-server --test load -- --ignored --nocapture keepalive_holds_over_ten_minutes_of_silence
DISTS= SERVER_DISTS='debian12 alma10' packaging/smoke.sh
```

Run `netem` only inside a private network namespace with loopback up. The
existing test's direct privileged invocation otherwise modifies the host's
loopback qdisc. For the static binary, use the server build command and pinned
image from `packaging/build.sh`; packages were rendered from the existing nfpm
server template, with the generated license inventory.

No state schema or wire change. Roll back the binary/unit together; preserve
the CA and `users.toml`. Do not regenerate a CA to recover an installation.
The implementation commit is `c449264` (signed); `ab79478` contains only
formatting. The branch is local and remains separate from `main`; nothing was pushed or
installed on a production host.
