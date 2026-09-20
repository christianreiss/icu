# Spikes (Step 3)

Wegwerf-Code der empirischen Prüfungen vom 2026-09-20; Ergebnisse in `../verification.md`. Jeder Ordner ist ein eigenständiges Cargo-Projekt (`cargo run` darin). Kein Produktcode, keine Wartung.

| Ordner | Spike | Binaries |
| --- | --- | --- |
| `spike-xwin/` | 1 Cross-Compile-Gate (Windows-gnu, voller Dependency-Satz); `hello.rs` = minimaler Wine-Test | `cargo build --release --target x86_64-pc-windows-gnu`; vorher `staticlibs/libstdc++.a` aus `/usr/x86_64-w64-mingw32/sys-root/mingw/lib/` kopieren und den Pfad in `.cargo/config.toml` anpassen |
| `spike-quic/` | 2 QUIC mTLS (`src/main.rs`), 2b Enrollment (`src/bin/enroll.rs`) | `cargo run`, `cargo run --bin enroll` |
| `spike-audio/` | 3 Audio-Loopback | `cargo run`, optional `PIPEWIRE_LATENCY=480/48000` |
| `spike-wayland/` | 4a Hotkey (`src/bin/hotkey.rs`, interaktiv: KDE-Dialog bestätigen), 4b Overlay (`src/bin/overlay.rs`) | `cargo run --bin hotkey`, `cargo run --bin overlay` |
| `spike-camera/` | 5 Kamera + H.264 | `cargo run` |
