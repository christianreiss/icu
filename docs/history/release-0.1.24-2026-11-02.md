# Release 0.1.24 — Offer window fits

The ICU Pro email joke keeps the same local failure. Sticker letters now sit
on their plates, and the offer window sizes itself to the full carnival so
nothing in that window scrolls. No purchase, activation, network email, or
restriction on real ICU features. See [the feature notes](email-joke-2026-10-31.md).

Before this version bump, `cargo test -p icu-client --lib ui::email_joke`
passed (3).

## Build and delivery

Package source: `d7c88794424d9e3b28fa3ce2835c1335e500eee0`. This record does
not change the published package bytes.

- `./build.sh --plain` succeeded on biest. The workspace test run inside
  packaging passed 1,060 tests across 46 suites. Five distribution smoke
  tests passed, including the server-only Debian 12 and AlmaLinux 10 checks.
  Supply-chain checks passed. Windows system-DLL import checks and the
  installer PE sanity check passed. Full logs:
  `target/build-logs/20261102T140717.684798Z-2316194`.
- **twingo.alpha-labs.net:** published all seven files under
  `https://misc.alpha-labs.net/icu/downloads/0.1.24/`, kept the existing page,
  and switched `downloads/latest.json` only after the files were in place and
  `sha256sum -c` passed. All seven public HTTPS files match the signed sizes
  and SHA-256 values. The manifest signature (`icu-update`) and
  `SHA256SUMS.sig` (`icu-release`) verified with `packaging/allowed_signers`.
  Older downloads remain. `icu.uggs.io` did not answer on port 443 and was
  not changed.
- The production server on **lab.uggs.io** was not changed or restarted.
  Native Windows execution was not run.

## Rollback

On twingo, `/root/icu-rollback-0.1.24-web/index.html` and `latest.json`
preserve 0.1.23. Restore them to the site's `index.html` and
`downloads/latest.json` with ownership `web20:client1` and mode 0644.
The 0.1.24 files can stay. No configuration restoration is required.
