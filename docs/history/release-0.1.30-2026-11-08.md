# ICU 0.1.30 — Stable controls during speech

Fixes the main-window buttons jumping between one and two rows when speech
starts or stops. Both idle and sending labels now contribute to the responsive
layout decision. See [the regression and fix](speech-control-layout-2026-11-08.md).

Before the version bump, all 1,070 workspace tests and strict workspace Clippy
passed. The regression checks repeated speech transitions across seven widths,
PTT/VAD and both themes.

Rollback: install the 0.1.29 client package, keeping identity and settings.
This restores the layout bug. Previous packages remain available.
