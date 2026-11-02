# Release 0.1.23 — Carnival email joke

The desktop Email window is now a carnival: a strobing hero, two opposing
marquees, drifting dots, and painted BUY NOW, WOW and NEW stickers. Upgrade
and licence loading still fail locally. There is no purchase, activation,
network email service, or restriction on real ICU features. The explanatory
footer is gone. See [the feature and removal notes](email-joke-2026-10-31.md).

Before this version bump, `cargo test -p icu-client --lib ui::email_joke`
passed (2) and `cargo clippy -p icu-client --lib --tests -- -D warnings`
passed.

## Build and delivery

Package source: `12a8cd43f71ce3f75d8ae80db6a8073bf95e7f51`. This record does
not change the published package bytes.

- `./build.sh --plain` succeeded on biest. The workspace test run inside
  packaging passed 1,059 tests across 46 suites. Five distribution smoke
  tests passed, including the server-only Debian 12 and AlmaLinux 10 checks.
  Supply-chain checks passed. Windows system-DLL import checks and the
  installer PE sanity check passed. Full logs:
  `target/build-logs/20261102T130744.606577Z-2126987`.
- An earlier `./build.sh --plain` was stopped with SIGTERM during the
  workspace tests because the host was overloaded. That run published
  nothing. The successful build started again from the same commit.
- **twingo.alpha-labs.net:** published all seven files under
  `https://misc.alpha-labs.net/icu/downloads/0.1.23/`, kept the existing page,
  and switched `downloads/latest.json` only after the files were in place and
  `sha256sum -c` passed. All seven public HTTPS files match the signed sizes
  and SHA-256 values. The manifest signature (`icu-update`) and
  `SHA256SUMS.sig` (`icu-release`) verified with `packaging/allowed_signers`.
  Older downloads remain. `icu.uggs.io` did not answer on port 443 and was
  not changed.
- The production server on **lab.uggs.io** was not changed or restarted.
  The new client RPM was not installed on biest. Native Windows execution
  was not run.

## Rollback

On twingo, `/root/icu-rollback-0.1.23-web/index.html` and `latest.json`
preserve 0.1.22. Restore them to the site's `index.html` and
`downloads/latest.json` with ownership `web20:client1` and mode 0644.
The 0.1.23 files can stay. No configuration restoration is required.
