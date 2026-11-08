# ICU 0.1.29 — Remove the Email joke

Completely removes the desktop Email button and ICU Lite/Pro carnival window,
including its licence picker, temporary state, animations, and `rfd` dependency.
The header gives the recovered space back to the server name. No settings or
identity migration is needed.

Rollback: install the previous 0.1.28 client package, keeping identity and settings.
This also restores the joke. Previous download versions remain available.

## Verification and delivery

Package source: `61ebdf747714e0e826dcd8a5805bdbbed7ca4756`.

- Rustfmt, strict workspace Clippy (`--all-targets -- -D warnings`) and diff
  checks passed. Workspace tests: 1,069 passed, zero failed, 18 ignored across
  46 suites. The main-window layout test also passed with rendered screenshots
  in both themes; visual inspection confirms the Email button is gone.
- `./build.sh --plain` passed all five stages on biest: Linux and Windows
  builds, seven packages, ten checksums, supply-chain/license checks and
  installation smokes on Fedora 42/43, Debian 12/13 and AlmaLinux 10.
  Windows emitted the existing webrtc COMDAT linker warning; build succeeded.
  Logs: `target/build-logs/20261108T080237.450424Z-1545159/`.
- Installed the verified Fedora 43 RPM on biest. `rpm -V icu-client` is clean;
  `icu --version` reports 0.1.29. No client process was running at installation.
- Published all seven packages and signed metadata on twingo.alpha-labs.net at
  `https://misc.alpha-labs.net/icu/downloads/0.1.29/`. Remote checksums and the
  `icu-release` signature passed before atomically replacing `downloads/latest.json`
  (web20:client1, 0644). Previous versions remain available.
- Fresh public HTTPS fetches verified the signed `icu-update` manifest, exact
  local manifest bytes, and size/SHA-256 of all seven packages. Manifest evidence:
  `target/release-0.1.29/served/`. The website index stayed byte-identical.
  The production voice server on lab.uggs.io was not changed or restarted.
- The quick slop scan reported no findings but scanned zero Rust source files;
  it supplies no native UI assurance. The rendered layout checks above cover
  the changed header. Native Windows execution and physical audio/video tests
  were not run for this removal.

The previous public manifest is saved on twingo at
`/root/icu-rollback-0.1.29/latest-before.json`. To roll the channel back, install
that file as web20:client1, mode 0644, to a temporary file beside
`/var/www/clients/client1/web20/web/icu/downloads/latest.json`, then rename it
atomically over `latest.json`. Local rollback uses `rpm -Uvh --oldpackage`
with the previous 0.1.28 client RPM. Identity and settings are retained.
