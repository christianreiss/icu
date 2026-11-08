# ICU 0.1.30 — Stable controls during speech

Fixes the main-window buttons jumping between one and two rows when speech
starts or stops. Both idle and sending labels now contribute to the responsive
layout decision. See [the regression and fix](speech-control-layout-2026-11-08.md).

Before the version bump, all 1,070 workspace tests and strict workspace Clippy
passed. The regression checks repeated speech transitions across seven widths,
PTT/VAD and both themes.

Rollback: install the 0.1.29 client package, keeping identity and settings.
This restores the layout bug. Previous packages remain available.

## Build and deployment

Package source: `498492f91fa859bd8ef9773485cf97a66a87bb34`.

- `./build.sh --plain` passed all five stages on biest, including 1,070
  workspace tests (zero failures, 18 ignored), supply-chain/license checks,
  Linux and Windows release builds, seven packages and ten checksums.
  Linux installation smokes passed on Fedora 42/43, Debian 12/13 and
  AlmaLinux 10. The existing webrtc COMDAT linker warning appeared on Windows;
  build succeeded. Logs: `target/build-logs/20261108T085716.679198Z-1711238/`.
- Installed the verified Fedora 43 RPM on biest. `rpm -V icu-client` is clean
  and `icu --version` reports 0.1.30. No ICU process was running before install.
- Published all seven packages and signed metadata on twingo.alpha-labs.net,
  under `https://misc.alpha-labs.net/icu/downloads/0.1.30/`. All remote checksums
  and the `icu-release` signature passed before atomically switching
  `downloads/latest.json` (web20:client1, 0644). The website index stayed
  byte-identical. Older packages remain available.
- Native Windows execution, physical audio/video tests and a new interactive
  desktop session were not run. The production voice server on lab.uggs.io
  was not changed or restarted.

Rollback channel metadata is on twingo at
`/root/icu-rollback-0.1.30/latest-before.json`. Install it as web20:client1,
mode 0644, to a temporary sibling of
`/var/www/clients/client1/web20/web/icu/downloads/latest.json`, then rename it
atomically over `latest.json`. For local rollback, use `rpm -Uvh --oldpackage`
with the 0.1.29 client RPM. Identity and settings are retained.

Fresh public HTTPS verification passed: exact local manifest bytes, valid
`icu-update` signature, and size/SHA-256 of every one of the seven packages.
Manifest evidence: `target/release-0.1.30/served/`.
