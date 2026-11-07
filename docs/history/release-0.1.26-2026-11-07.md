# Release 0.1.26 — Single-row main window controls again

0.1.25 always put the send-mode button on its own row, which made the bottom
panel about 50 px taller and moved the icons to the left even in the everyday
Automatisch state. The controls are a single row again (status left, icons
right) and only stack when the title or hint does not fit beside the icons,
for example a long push-to-talk binding like `CTRL+SHIFT+F12` or the hint
"Sendet, wenn du sprichst". Nothing is truncated either way.

Rendered with the `ui_layout` harness: Automatisch alone in the room keeps one
row; push-to-talk, and Automatisch with other people present, stack.

## Build and delivery

Package source: `bfcdb85e66f9253e2caad12d8906872c1c4bd956`. This record does
not change the published package bytes.

- `./build.sh --plain` succeeded on biest in 20:51: all stages passed,
  including the workspace tests and the distribution smoke tests, and seven
  artifacts with verified checksums were produced. Before packaging,
  `cargo clippy --all-targets -p icu-client --features gui` was clean.
- **twingo.alpha-labs.net:** published the whole version directory (13 files,
  7 packages) under `https://misc.alpha-labs.net/icu/downloads/0.1.26/`,
  verified `sha256sum -c` on the server, and only then replaced
  `downloads/latest.json` atomically (`web20:client1`, 0644). The public HTTPS
  files are byte-identical to the local build, the listed checksums match,
  `latest.json` is identical to the signed local copy, and `SHA256SUMS.sig`
  verifies against `packaging/allowed_signers` (`icu-release`). Older downloads
  remain. The existing page on twingo was not changed (as for 0.1.25, it is
  not this repository's `web/` site, whose fixes remain undeployed).
- Installed locally on biest from the verified fc43 RPM; a running client keeps
  the previous version until it is restarted.
- The production server on lab.uggs.io was not changed or restarted. Native
  Windows execution and the update flow on a real client were not run.

## Rollback

On twingo, `/root/icu-rollback-0.1.26-web/index.html` and `latest.json`
preserve the state with 0.1.25. Restore `latest.json` to
`downloads/latest.json` with ownership `web20:client1` and mode 0644. The
0.1.26 files can stay. No configuration restoration is required.
