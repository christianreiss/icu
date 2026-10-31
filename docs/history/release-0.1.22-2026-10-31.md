# Release 0.1.22 — Zawinski Compliance Edition

The desktop client gains a removable Email button opening a deliberately gaudy
ICU Lite/Pro upgrade joke. Every licence fails locally; no purchase, activation,
network email service, or restriction on real ICU functionality is introduced.
See [the feature and removal notes](email-joke-2026-10-31.md).

The feature's final workspace run passed 1,059 tests with 17 hardware tests
ignored; Clippy, formatting and dependency licence checks passed. Earlier
real-clock audio tests failed intermittently and passed in the full rerun.
Native Windows execution and native file-picker interaction remain separate
acceptance checks. Build and delivery evidence follows.

## Build and delivery

Package source: `fc4b19db19431dfc1299652b5abc2af1ee061832`. This subsequent
record does not change the published package bytes.

- `./build.sh --plain` succeeded: all seven packages, 1,059 tests passed,
  17 ignored, five distribution smoke tests, supply-chain/licence checks,
  Windows system-DLL import checks and installer PE validation.
- **biest.eulie.de:** installed `icu-client-0.1.22-1.fc43.x86_64` through DNF;
  `rpm -V icu-client` clean and `/usr/bin/icu --version` reports 0.1.22.
  All nine configuration/identity files remained byte-identical. The client
  was closed before installation and remains closed; no GUI runtime adoption
  or native licence-picker acceptance is claimed. DNF installed the local
  unsigned RPM using its existing command-line-package policy; package hashes
  were verified by the release builder.
- **twingo.alpha-labs.net:** published all seven files under
  `https://misc.alpha-labs.net/icu/downloads/0.1.22/`, retained the page layout,
  updated the release text/links/sizes, and atomically switched the signed
  manifest last. Older downloads remain intact. All seven public HTTPS files
  were downloaded and verified for size and SHA-256; the manifest's Ed25519
  signature and the page version were independently verified.
- An early remote checksum check encountered an incomplete upload and stopped
  before publication. Verification passed after the upload completed. A later
  IPv6 SSH session stalled before remote execution; after checking that no
  publication process was running, it was terminated and retried over IPv4
  with normal host-key checking. No TLS or SSH verification was disabled.
- The production server on **lab.uggs.io** was not changed or restarted.
  Native Windows execution, Windows Clippy and the native licence-picker flow
  were not run for this release; Windows build/PE checks are not runtime proof.

## Evidence and rollback

Full build logs: `target/build-logs/20261031T195159.626095Z-3427520/`.
Installation, configuration hashes and public-download verification:
`target/release-0.1.22/`.

On twingo, `/root/icu-rollback-0.1.22-web/index.html` and `latest.json` preserve
0.1.21. Restore them to the site's `index.html` and `downloads/latest.json`
with ownership `web20:client1` and mode 0644 to roll back the website.
The local prior RPM remains in `dist/0.1.21/` for an explicit DNF downgrade;
no configuration restoration is required. No old downloads were deleted.
