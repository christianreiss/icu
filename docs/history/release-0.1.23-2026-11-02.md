# Release 0.1.23 — Carnival email joke

The desktop Email window is now a carnival: a strobing hero, two opposing
marquees, drifting dots, and painted BUY NOW, WOW and NEW stickers. Upgrade
and licence loading still fail locally. There is no purchase, activation,
network email service, or restriction on real ICU features. The explanatory
footer is gone. See [the feature and removal notes](email-joke-2026-10-31.md).

Before this version bump, `cargo test -p icu-client --lib ui::email_joke`
passed (2) and `cargo clippy -p icu-client --lib --tests -- -D warnings`
passed. Release build and delivery evidence will be recorded below once the
committed source has been packaged and published.
