# Unreleased — ICU Pro email joke (2026-10-31)

The desktop header now has an Email button opening a deliberately gaudy
ICU Lite/Pro upsell. Upgrade fails locally; loading any licence always fails.
This is a Zawinski's Law joke, with no email service, payment flow, network
activation, or restrictions on existing ICU features. The web client is unchanged.

## Carnival window, 2026-11-02

The same local failure stays. The window is a carnival: a strobing hero,
blinking lines, two opposing marquees, drifting dots, and painted BUY NOW,
WOW and NEW stickers. The upgrade button reads BUY NOW. The explanatory
footer is gone. `EMAIL_JOKE_ENABLED` still removes the button and the window.

Checked 2026-11-02 in this checkout: `cargo test -p icu-client --lib ui::email_joke` passed (2), and `cargo clippy -p icu-client --lib --tests -- -D warnings` passed. The native window was not opened.

## Removal

Set `EMAIL_JOKE_ENABLED` to `false` in
`crates/icu-client/src/ui/email_joke.rs` and rebuild to hide the button and window.
The module owns all joke copy, temporary state, and file loading. Full removal
also removes its small main-window hooks, module declaration, and GUI-only `rfd`
dependency. No configuration migration or saved licence cleanup is needed.

## Licence handling

The native file picker runs on a worker thread. Cancellation preserves the current
message. Regular files up to 64 KiB (including empty and binary files) receive the
same joke rejection; larger or unreadable files receive explicit failures. File
contents are discarded, never displayed, logged, uploaded, or persisted.

## Acceptance

Exercise Email twice (one window), close/reopen, upgrade failure, picker cancel,
repeated file loads, empty/binary/text/oversized/missing files, both themes and a
narrow viewport. With the constant disabled, no joke UI should appear. Native
picker verification is platform-specific; automated layout checks do not prove it.

## Verification on biest

- `cargo check -p icu-client`, Rustfmt, workspace Clippy (`-D warnings`), and
  `cargo deny check licenses` passed.
- Final `cargo test --locked --workspace --no-fail-fast`: 1,059 passed,
  17 ignored, zero failures across 46 suites, including both new joke tests
  and existing main-window layout checks.
- Earlier runs failed real-clock audio tests (network jitter, then loopback);
  both passed in the final complete run. Host load was high; causality was not
  established and audio code was not modified.
- Native picker/OS-window interactions, a build with the removal constant disabled,
  Windows execution, and release packaging were not exercised. No deployment.
