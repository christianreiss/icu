# Voice transport and Linux ducking validation

Date: 2026-10-30. Baseline: ICU commit `62b0069719b0602b171304afb390108509c75073`.
Tests below use production jitter, Opus, send-chain classification and native
PipeWire controls. Synthetic clocks/audio do not establish microphone or
loudspeaker hardware acceptance.

## Transport freshness

The previous 8 KiB/64 KiB bounds limited bytes, not age: small Opus silence
packets could remain queued through an arbitrary congestion pause. The local
Quinn patch expires individual application datagrams after 100 ms at actual
QUIC packet construction, independently on client uplink and server relay.
Sequence numbers, END flags and payloads of retained packets are unchanged.
Expired END is treated as loss; the receiver limits tail concealment to 100 ms.
OS/socket buffers and network propagation remain outside this local queue TTL.

`cargo test --manifest-path vendor/quinn-proto/Cargo.toml --lib`: **306 passed**.
Coverage includes 200/500/1000/2000 ms stalls, tiny/large payloads, 1/8/32/64
interleaved speakers, no fresh enqueue, slow drain and failed packet packing.
`ConnectionStats::datagrams_expired` distinguishes expiry from packet-loss counts.
See [vendor provenance](../../vendor/README.md).

## Jitter, duration transitions and clock drift

The old target was explicitly monotonic inside one talk spurt. The new
continuous-monologue test observes 140 → 20 ms (10 ms packets) and 160 → 30 ms
(20 ms packets) after a single 120 ms stall. Old peaks retain their conservative
20–40 s memory; after expiry, a 2 s hysteresis permits at most 10 ms/s descent.
The existing 60-seed Wi-Fi profile retains 0 underruns/minute and 0.20
stretches/minute after warm-up. Histogram forgetting, patience and tail budgets
now use time rather than packet counts.

Packet durations are stored individually. An adjacent timestamp delimits a lost
slot at 10 ↔ 20 ms transitions, even if successors arrive out of order. If
multiple missing packets straddle a transition, their individual durations are
not identifiable on this wire format; the previous duration is retained until
an adjacent timestamp provides evidence. FEC and PLC receive the missing slot's
explicit duration, never the newest arriving packet's duration.

Ten-minute simulations at ±50/100/200 ppm and 10/20 ms packets keep measured
arrival-to-playout delays within 20–60.084 ms. No real loss, backlog drops or
spurt resets occur; the worst case needs 10 corrections in 10 minutes. A new
fractional resampler was not introduced without evidence of a quality benefit.

The real large-quantum integration test exposed an additional scheduling bug:
when a 1024-sample callback consumed a batch, the mixer sometimes decoded ahead
of its nominal cadence and manufactured PLC despite no missing packet. The
proactive stretch margin now includes already-decoded pending/playback audio.
Arrival, prefill and actual packet availability retain the real clock. The
1024-sample, small-quantum and Wi-Fi integration runs all passed with zero
continuity glitches, stretches or underruns after this fix. A deterministic
regression also confirms that genuinely missing packets still use concealment.

## Codec and mixing

Bundled libopus is 1.6.1. Its DRED build option enables Deep PLC, but ICU does
not configure DRED transmission. The runtime version is available through
`opus::version`. A FEC attempt is distinct from a recovered frame, recovered
sample count and PLC-only fallback; a larger lost slot may contain a PLC prefix
plus real FEC. Neither FEC nor PLC is allowed to train microphone feedback.

Production codec tests: **12 passed**, one optional benchmark ignored. FEC
correlation against original PCM is 0.97–0.98 versus 0.54 for PLC in the existing
voice fixture. Explicit-duration/provenance tests include an impossible 10 ms
FEC request from a 20 ms successor, which must be counted as PLC fallback.

An additional deterministic matrix runs 400 frames per case through actual
Opus and jitter at 10/20 ms, 0/1/3/5/10% application loss and 25–31 ms arrival
delay. All ten cases retained the exact media duration, with zero artificial
stretch, skip, underrun or backlog drop. Correlation against the complete Opus
round trip was 0.9962–1.0000. At 10% loss, 10 ms recovered 17/40 missing frames
with FEC and used PLC for 23; 20 ms recovered 18/40 and used PLC for 22. Low-loss
10 ms cases correctly reported PLC-only fallback rather than invented FEC.
This matrix is deterministic codec/playout coverage; a live 64-client mixed
voice/video stress test remains separate from the 64-speaker QUIC queue test.

The cascade now uses the same candidate-aware classification, speech-gated
finishing and duration-aware gate update as production. All **7 cascade tests**
passed. Independent linear-fit residual distortion for 8 simultaneous speakers
was −46.5 dB at 100% and −20.4 dB at 200% personal volume; output stays finite and
bounded. Gain-ramp/mute tests remain separate. Nonfinite input from one stream
is discarded so it cannot poison other speakers. Higher combined personal and
master boosts necessarily engage the existing limiter more strongly.

## Native Linux ducking

`experiments/test-ducking-linux.py` creates a dedicated null sink and synthetic
stereo playback on the real user PipeWire daemon; it never changes the default
output device or real application volumes. On host biest.eulie.de / PipeWire
1.4.11, native tests verified −12 dB transient soft volume, untouched persistent
channel volumes 0.63/0.315, preserved balance, restoration and manual override (including a foreign controller returning to an older,
already-acknowledged ICU soft-volume value).

All five modes now have native PCM/control evidence. In the final isolated run,
Light requested 6 dB and measured **6.033 dB**, Medium requested 12 dB and measured
**12.015 dB**, Strong requested 18 dB and measured **17.997 dB**. Off restored the
baseline (**−0.027 dB** measured difference, within the fixture's block-phase
variation); channel volumes, balance and mute remained unchanged. Auto settled
at **13.515–13.520 dB** with the constant foreground/background pair. Light,
Strong and Off exercise the public mode envelope before native control; the
Medium guardian and Auto feedback-loop checks exercise their production paths.

The passive application monitor measures post-stream-volume PCM. Native measured
RMS was −28.5237 dBFS before attenuation and −40.4842 dBFS after −12 dB; removing
only ICU's known gain reconstructed −28.4842 dBFS. This is RMS energy, not peak
or an aggregate sink meter. Independent stream energies are summed without
cross-channel cancellation. ICU streams, wrong sinks and multi-sink fanout are
excluded. At most 32 application monitors run; absent, stale or incomplete
measurement uses the UI's named Medium fallback. Gain changes require a fresh
post-change audio block before energy is paired with the new factor.

The guardian uses native PipeWire APIs without subprocesses on its control tick.
Only transient `softVolumes` are attenuated; ordinary channel/scalar volume and
mute are preserved. A generation-cookie/node-serial journal records actual
observed prior values and restores conditionally after a guardian restart.
Unknown or invalid recovery records are retained and reported. All raw audio
remains inside the monitor callback; only energy crosses to the control loop.

A further native closed-loop Auto run with constant −32 dBFS foreground kept
attenuation at **13.515–13.521 dB** over the measured steady second (0.006 dB
span), using comparable stream RMS throughout. Corrupt and oversized recovery
journals were rejected without overwriting their bytes. Guardian integration
passed heartbeat expiry, stdin EOF, actual sender-process SIGKILL, and guardian
SIGKILL followed by restart/journal restoration. Killing the foreign fixture application with SIGKILL while it was still
attenuated, then restarting it, retained the original persistent channel-volume
baseline. The actual
GUI process was not killed during this isolated test; its guardian IPC failure
mechanism was exercised by a separate sender process. Simultaneously killing
both the main process and guardian cannot restore until a guardian restarts;
transient soft volume is never persisted as the application's normal volume.

Re-run commands:

```sh
cargo test --locked -p icu-client --lib dsp::jitter
cargo test --locked -p icu-client --lib dsp::opus
cargo test --locked -p icu-client --test cascade
cargo test --locked -p icu-client --test transport_loss
cargo test --locked -p icu-client --test playout
python3 experiments/test-ducking-linux.py
# With a freshly built ICU executable, additionally exercise guardian recovery:
python3 experiments/test-ducking-linux.py --worker-binary target/debug/icu
```

Windows native session/volume acceptance and acoustic hardware checks are
separate from these Linux and deterministic results.
