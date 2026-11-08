# Stable control rows during speech — 2026-11-08

The main window chose a single row or a stacked layout from the current send
label on every frame. PTT's active hint (`Push-to-Talk · <key>`) is wider than
its idle hint (`Taste <key>`), so starting speech could move every icon to a
second row. VAD could jump in the opposite direction.

The layout now measures both idle and sending labels before choosing the row
layout whenever speech is possible. It uses one row when both fit; otherwise
both states use the stacked layout. No text is removed or shortened. Muted,
Privacy, offline and empty-room states keep their existing layout rules.

`speech_keeps_control_positions_stable` reproduces the original failure at
460 px in PTT mode, then verifies exact mode/microphone/Privacy rectangles
across repeated idle/sending transitions, seven widths (400–640 px), PTT/VAD,
and both themes. A wide window must still use one row.

Verification: the regression passed with screenshots rendered serially; strict
workspace Clippy, Rustfmt and diff checks passed. A whole-suite parallel
screenshot run exited 101 after SIGSEGV in the renderer; its cause was not
established. Serial regression rendering succeeded. The quick slop review had
zero findings but scanned zero Rust files, so it provides no native UI coverage.
Screenshots are under `target/speech-layout-shots/`.

Initially verified as a source-only fix; subsequently shipped in
[0.1.30](release-0.1.30-2026-11-08.md). No saved configuration or identity data
changes. A source rollback is a revert; a package rollback uses 0.1.29.

Final `cargo test --locked --workspace`: 1,070 passed, zero failed, 18 ignored
across 46 suites, including all 43 native UI layout tests without screenshots.
