# Audio feedback — implementation and verification, 2026-09-28

> Historischer Bericht: Ergebnisse gelten für den genannten Stand. Befehle in
> Codeblöcken bleiben als damaliger Nachweis erhalten. Für heutige Pfade und
> Befehle siehe [Verzeichnisübersicht](../README.md) und
> [Bauen](../development/building.md).


Implemented in the working tree on local main (base at final verification:
`ec35dd7`). The concurrent overlay, voice-optimizer default and user-view changes
were preserved. The feature is committed directly on local main at the user's
request; no separate branch merge was needed. It has not been pushed or deployed.

## Delivered

- Capability-gated receiver reports and median consensus with expiry, eligibility
  checks and bounded per-pair storage/rate. Legacy peers retain their audio path.
- Measurements before playback gain, with local corroboration and bounded target
  adjustments in the existing microphone leveler. No competing digital gain stage.
- Temporary system microphone control through native PipeWire/WASAPI APIs,
  exclusive per-device ownership, external-change detection, conditional restoration
  and software fallback. Device operations stay off real-time callbacks.
- Independent default-enabled controls under **Klang → Pegel automatisch → Details**,
  also available during setup, with gain/status diagnostics. Existing window sizes
  retained; light/dark screenshots reviewed and accessibility interactions tested.
- Protocol, client/server implementation and physical acceptance documentation.

## Verification

| Check | Result |
|---|---|
| `cargo test --locked --workspace` | **823 passed, 0 failed, 9 ignored**, exit 0 |
| Actual QUIC, A/B/C reports, agreement/deafen, legacy capability isolation | Passed |
| Opus cascade, two independent receivers with different playback volumes | Passed; correct baseline unchanged |
| Sustained simulated post-processing attenuation | Receive correction **2.62 → 1.89 dB**, bounded peer target offset **+1.5 dB**, peak below full scale |
| Fake device: gain transfer, restoration, manual override, noise, clipping, unsupported steps, competing ICU processes, write failures | Passed |
| Live PipeWire temporary virtual source | Passed: source discovery, −1 dB change, readback, exact restoration; temporary sources removed |
| UI layout and new controls | All 19 tests passed as part of workspace run; light/dark rendered views inspected |
| Windows GNU default-feature all-target compilation | Passed |
| Linux strict Clippy with `--no-default-features --features icu-client/aec,icu-client/gui,icu-client/camera,icu-client/tray` | Passed, workspace/all targets, `-D warnings` |
| Windows strict Clippy with `--no-default-features --features aec,gui,camera,tray` | Passed, client/all targets, `-D warnings` |
| Task-owned Rust formatting and `git diff --check` | Passed |

The full default-feature strict Clippy command remains blocked by the unchanged
`overlay::blit_rotated_glyph` (9 arguments, `clippy::too_many_arguments`). Full
`cargo fmt --all -- --check` reports existing formatting in `overlay/mod.rs` and
`ui/user.rs`; neither file is changed by this feature. The scoped Clippy runs
exclude only the unrelated overlay feature; they cover the new audio, protocol,
server and UI code. An optional Windows `--all-features` probe also exposed an
existing `eframe::App::on_exit` signature incompatibility with `glow`; the shipped
default feature set compiles.

Two verification failures were repaired: the invitation UI fixture now re-signs
its modified test payload with its own test CA (production signature verification
is unchanged); control-stream EOF waits at most 100 ms for the QUIC close reason
so a server restart is correctly reported as SHUTDOWN. The full suite above was
rerun after these repairs.

## Remaining physical acceptance

No claim of physical microphone or acoustic acceptance is made. Real Linux and
Windows microphone gain, speaker/AEC stability, device-specific quantization and
listener judgement still need the checklist in `docs/development/testing.md`. Native Windows
execution was not performed. PipeWire reports unknown hardware provenance honestly;
Windows distinguishes hardware-backed and software endpoint controls. A software
system-volume adjustment cannot repair clipped capture or improve its intrinsic
signal-to-noise ratio. Crash/unplug restoration is not guaranteed.

Rollback is to disable either new switch; existing digital leveling remains
available. Restore system volume only while ICU still owns its last value.
