# Release 0.1.25 — Accessibility and state fixes from the UX/UI review

The Fleet slop review of the whole UX/UI (job over the egui client and the
marketing site, four full runs) produced these changes. Commit `431aef9`.

## Changes

- **Client:** status pills and badges expose their text to assistive
  technology. The push-to-talk mode button has its own full-width row, so the
  complete binding is readable at the 400 px main window; the action icons sit
  on a second row (the bottom panel is about 50 px taller).
- **Site:** no distribution is recommended when detection fails; the
  selector keeps keyboard focus; reveal content fails open without `site.js`;
  a failed copy reports the failure and keeps the full selectable checksum
  until dismissed; a pause control stops continuous decorative motion; the
  download selector is a labelled group of pressed-state buttons; hero and
  showcase use current captures of the UI test harness, and the camera tab
  labels its older image.

## Review state

The last review run still disagreed on four P2 items. Three were fixed
afterwards. The fourth, a clipped refused-passport capture, is not a layout
bug: `ui_layout` shows the heading and all three verdict lines inside the
560×600 window with about 10 px of margin.

Not covered by the review or by tests: screen-reader behaviour, Windows and
Safari rendering of the site, update-prompt and Pro-offer captures, an
AccessKit export, and live (non-fixture) window screenshots.

## Build and delivery

Package source: `911f56e79f7c2e84e7e06d80c1ecfce45b6e240f`. This record does
not change the published package bytes.

- The first `./build.sh --plain` attempt stopped at the supply-chain stage:
  `cargo deny` rejected `async-executor 1.14.0` as yanked. The lockfile now
  uses 1.14.1; no other dependency moved. The second run succeeded on biest in
  27:58: all stages passed, including the workspace tests and the distribution
  smoke tests, and seven artifacts with verified checksums were produced.
- **twingo.alpha-labs.net:** published the whole version directory (all 13
  files, 7 packages) under `https://misc.alpha-labs.net/icu/downloads/0.1.25/`,
  verified `sha256sum -c` on the server, and only then replaced
  `downloads/latest.json` atomically (`web20:client1`, 0644). The public HTTPS
  files are byte-identical to the local build, all listed checksums match, and
  `SHA256SUMS.sig` verifies against `packaging/allowed_signers`
  (`icu-release`). The manifest signature was verified by
  `web/tools/publish-release.py` when signing. Older downloads remain.
- The existing page on twingo was not changed. It is not the `web/` site of
  this repository (its `index.html` differs and `assets/css/site.css` does not
  exist there), so the site fixes of this release are committed but not
  deployed anywhere. `icu.uggs.io` was not touched.
- The production server on lab.uggs.io was not changed or restarted. Native
  Windows execution and the update flow on a real client were not run.

## Rollback

On twingo, `/root/icu-rollback-0.1.25-web/index.html` and `latest.json`
preserve the state with 0.1.24. Restore `latest.json` to
`downloads/latest.json` with ownership `web20:client1` and mode 0644. The
0.1.25 files can stay. No configuration restoration is required.
