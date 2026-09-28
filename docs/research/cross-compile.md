# Cross-Compile: Fedora → Windows-Client, Fedora → statischer Linux-Server

## Frage

Wie werden von der Fedora-43-Workstation (rustc/cargo 1.98.0 aus dem Fedora-Paket, kein rustup, kein Windows-Rechner, kein Windows-CI-Runner) reproduzierbar (a) ein Windows-x86_64-Client-Binary mit der vollständigen nativen Dependency-Kette (QUIC/TLS-Crypto, libopus, Audio-I/O, GUI, Kamera, Video-Codec) und (b) ein statisches Linux-x86_64-Server-Binary gebaut? Welches Setup wird in Step 3 empirisch getestet?

## Kandidaten

Windows-Client:

- A. `x86_64-pc-windows-gnu` mit Fedora-mingw64-Toolchain (GCC/binutils, msvcrt)
- B. `x86_64-pc-windows-msvc` via `cargo-xwin` (clang-cl + lld-link, Windows SDK/CRT per `xwin` heruntergeladen)
- C. `cargo-zigbuild` (zig als C-Compiler/Linker)
- D. nativer Windows-CI-Runner (GitHub/GitLab); Annahme laut Aufgabenstellung: git.alpha-labs.net hat keinen Windows-Runner
- E. `x86_64-pc-windows-gnullvm` (llvm-mingw, UCRT) als Nebenkandidat

Linux-Server:

- S1. `x86_64-unknown-linux-musl`, vollstatisch
- S2. `x86_64-unknown-linux-gnu` mit `-C target-feature=+crt-static` (glibc statisch)

## Primärquellen

Alle abgerufen am 2026-09-20.

Rust-Projekt (rust-lang):

- Platform Support (stable): https://doc.rust-lang.org/rustc/platform-support.html
- Platform Support (nightly): https://doc.rust-lang.org/nightly/rustc/platform-support.html
- Platform Support, Markdown-Quelle im Repo: https://raw.githubusercontent.com/rust-lang/rust/master/src/doc/rustc/src/platform-support.md
- windows-gnu Target-Seite: https://doc.rust-lang.org/rustc/platform-support/windows-gnu.html und https://raw.githubusercontent.com/rust-lang/rust/master/src/doc/rustc/src/platform-support/windows-gnu.md
- windows-gnullvm Target-Seite: https://doc.rust-lang.org/rustc/platform-support/windows-gnullvm.html
- Reference, Linkage (crt-static): https://doc.rust-lang.org/reference/linkage.html
- rustc Codegen-Optionen (`-C link-self-contained`, `-C target-feature`): https://doc.rust-lang.org/rustc/codegen-options/index.html
- Cargo-Konfiguration (`[target.<triple>]`, `[env]`, `[build]`): https://doc.rust-lang.org/cargo/reference/config.html

Cross-Tools:

- cargo-xwin README: https://github.com/rust-cross/cargo-xwin und https://raw.githubusercontent.com/rust-cross/cargo-xwin/main/README.md ; crates.io: https://crates.io/api/v1/crates/cargo-xwin
- xwin README: https://github.com/Jake-Shadle/xwin
- cargo-zigbuild README: https://github.com/rust-cross/cargo-zigbuild ; crates.io: https://crates.io/api/v1/crates/cargo-zigbuild
- cc-Crate-Doku: https://docs.rs/cc/latest/cc/
- pkg-config-Crate-Doku: https://docs.rs/pkg-config/latest/pkg_config/
- cmake-Crate: https://docs.rs/cmake/latest/cmake/ , https://docs.rs/cmake/latest/cmake/struct.Config.html , Quelltext https://docs.rs/crate/cmake/latest/source/src/lib.rs , README https://github.com/rust-lang/cmake-rs/blob/master/README.md

Crypto/Transport:

- aws-lc-rs User Guide: https://aws.github.io/aws-lc-rs/requirements/index.html , https://aws.github.io/aws-lc-rs/requirements/windows.html , https://aws.github.io/aws-lc-rs/requirements/linux.html , https://aws.github.io/aws-lc-rs/platform_support.html , https://aws.github.io/aws-lc-rs/faq.html
- aws-lc-sys README: https://github.com/aws/aws-lc-rs/blob/main/aws-lc-sys/README.md
- ring BUILDING.md: https://github.com/briansmith/ring/blob/main/BUILDING.md ; README: https://github.com/briansmith/ring
- quinn README: https://github.com/quinn-rs/quinn ; Cargo.toml (main): https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn/Cargo.toml
- rustls README: https://github.com/rustls/rustls

Audio/Video/GUI:

- opus-rs: https://github.com/SpaceManiac/opus-rs ; opusic-sys: https://github.com/DoumanAsh/opusic-sys ; audiopus_sys: https://github.com/lakelezz/audiopus_sys
- cpal README: https://github.com/RustAudio/cpal ; Cargo.toml (master): https://raw.githubusercontent.com/RustAudio/cpal/master/Cargo.toml
- nokhwa README: https://github.com/l1npengtul/nokhwa ; docs.rs: https://docs.rs/nokhwa/latest/nokhwa/ ; Bindings-Manifeste: https://raw.githubusercontent.com/l1npengtul/nokhwa/senpai/nokhwa-bindings-windows/Cargo.toml , https://raw.githubusercontent.com/l1npengtul/nokhwa/senpai/nokhwa-bindings-linux/Cargo.toml
- openh264-rs README: https://github.com/ralfbiedert/openh264-rs ; build.rs: https://raw.githubusercontent.com/ralfbiedert/openh264-rs/master/openh264-sys2/build.rs
- libvpx README: https://github.com/webmproject/libvpx
- winit Wayland-Backend: https://docs.rs/winit/latest/winit/platform/wayland/index.html ; wayland-sys: https://docs.rs/wayland-sys/latest/wayland_sys/
- tray-icon README: https://github.com/tauri-apps/tray-icon

Versionen (crates.io-API, jeweils `https://crates.io/api/v1/crates/<name>`): quinn, rustls, aws-lc-rs, ring, opus, opusic-sys, audiopus_sys, cpal, egui, eframe, iced, wgpu, winit, nokhwa, openh264, openh264-sys2, env-libvpx-sys, nnnoiseless, rcgen, tokio, postcard, tray-icon, ashpd, evdev, windows, rubato.

Lokale Prüfung (read-only, `dnf info` / `dnf provides` / `dnf repoquery -l`, Fedora 43, 2026-09-20): siehe Befunde „Lokale Toolchain".

## Befunde

### Lokale Toolchain (dnf, 2026-09-20)

- Installiert: `rustc 1.98.0 (Fedora 1.98.0-1.fc43)`, `cargo 1.98.0`, `rust-std-static-1.98.0-1.fc43` nur für `x86_64-unknown-linux-gnu` (`/usr/lib/rustlib/` enthält nur dieses Target). Kein `rustup` im PATH.
- Verfügbar im Repo: `rustup 1.29.0-7.fc43` (liefert `/usr/bin/rustup-init`); `rust-std-static-x86_64-pc-windows-gnu 1.98.1-1.fc43` (Version ≠ installierter rustc 1.98.0, std muss exakt zum Compiler passen); **kein** `rust-std-static` für `x86_64-unknown-linux-musl` und kein `…-gnullvm`.
- mingw64: `mingw64-gcc 15.2.1-2.fc43`, `mingw64-gcc-c++ 15.2.1`, `mingw64-binutils 2.45.1-2.fc43`, `mingw64-headers 13.0.0`, `mingw64-crt 13.0.0`, `mingw64-winpthreads 13.0.0`, `mingw64-winpthreads-static 13.0.0`, `mingw64-filesystem 152` (liefert `/usr/share/mingw/toolchain-mingw64.cmake`, `/usr/bin/mingw64-cmake`, `/usr/bin/x86_64-w64-mingw32-pkg-config`), `mingw64-binutils` liefert `/usr/bin/x86_64-w64-mingw32-windres`. `mingw64-opus 1.5.2` vorhanden; `mingw64-openh264` und `mingw64-libvpx` existieren nicht.
- Inhalt des Fedora-Toolchain-Files (gelesen): setzt `CMAKE_SYSTEM_NAME Windows`, `CMAKE_C_COMPILER x86_64-w64-mingw32-gcc`, `CMAKE_CXX_COMPILER x86_64-w64-mingw32-g++`, `CMAKE_FIND_ROOT_PATH /usr/x86_64-w64-mingw32/sys-root/mingw`, `CMAKE_RC_COMPILER /usr/bin/x86_64-w64-mingw32-windres` (jeweils nur, wenn `CC`/`CXX`/`RC` nicht in der Umgebung gesetzt sind).
- LLVM: `clang 21.1.8` (liefert `/usr/bin/clang-cl`), `lld 21.1.8` (`/usr/bin/lld-link`), `llvm 21.1.8` (`/usr/bin/llvm-lib`, `/usr/bin/llvm-ar`, `/usr/bin/llvm-rc`), `ninja-build 1.13.1`.
- Sonstiges: `nasm 2.16.03`, `cmake 3.31.11`, `musl-gcc 1.2.5-5.fc43` (`/usr/bin/musl-gcc`), `musl-libc-static 1.2.5`, `zig 0.16.0-1.fc43`, `wine 11.0`, `perl-interpreter 5.42.3`.

### Rust-Targets (rust-lang Platform Support)

- `x86_64-pc-windows-gnu`: **Tier 1 with Host Tools** (Markdown-Quelle im rust-Repo, nightly-Seite, windows-gnu-Seite). Eine Zusammenfassung der stable-HTML-Seite ordnete es „Tier 2" zu; die Quelle im Repo und die Target-Seite widersprechen, ich werte das als Lesefehler des Fetch-Tools. Die windows-gnu-Seite warnt: „These targets do not have any maintainers and are not properly maintained", Demotion möglich. Mindestversionen der C-Toolchain: „GNU Binutils 2.44, GCC 14.2, mingw-w64 12.0.0" — Fedora 43 erfüllt alle drei (2.45.1 / 15.2.1 / 13.0.0). Runtime: msvcrt. „Rust ships a pre-compiled std library for these targets, allowing easy compilation and cross-compilation from other hosts if a proper C toolchain is installed."
- `x86_64-pc-windows-msvc`: Tier 1 with Host Tools.
- `x86_64-pc-windows-gnullvm`: Tier 2 with Host Tools; braucht llvm-mingw oder MSYS2-CLANG-Umgebung; UCRT statt msvcrt; „No implicit self-contained mode (unlike windows-gnu)"; Tests laufen nicht in CI. Fedora hat kein llvm-mingw-Paket (nicht gesucht; unbelegt) und kein rust-std dafür.
- `x86_64-unknown-linux-musl`: Tier 2 without Host Tools (musl 1.2.5). Tier-2-Garantie: „Automated tests are not always run so it's not guaranteed to produce a working build, but tier 2 targets often work to quite a good degree".
- Reference/Linkage: `x86_64-unknown-linux-musl` gehört zu den Targets, die standardmäßig `+crt-static` sind; MSVC-Targets können per `-C target-feature=+crt-static` statisch gegen die CRT gelinkt werden („/MT").
- Codegen: `-C link-self-contained` „controls whether the linker will use libraries and objects shipped with Rust instead of those in the system"; ohne Wert entscheidet eine Heuristik.

### Cargo-Konfiguration (cargo reference)

- `target.<triple>.linker` (Env `CARGO_TARGET_<triple>_LINKER`), `target.<triple>.rustflags` (Env `CARGO_TARGET_<triple>_RUSTFLAGS`), `target.<triple>.ar` ist „deprecated and unused".
- `[env]` setzt Variablen „for build scripts, rustc invocations, cargo run, and cargo build"; überschreibt vorhandene Umgebung nur mit `force = true`; `relative = true` für config-relative Pfade.
- `build.target` (Env `CARGO_BUILD_TARGET`) kann Liste sein.

### cc-Crate (Cross-Erkennung)

Prioritätsreihenfolge der Variablen: `CC_<target>` (z. B. `CC_x86_64-pc-windows-gnu`), `CC_<target_mit_Unterstrichen>`, `TARGET_CC`, `CC`; analog `CXX`, `AR`, `CFLAGS`, `CXXFLAGS`. Für windows-gnu erwartet cc einen Compiler im PATH („recommends MinGW-w64 distribution"); für MSVC-Targets wird Visual-Studio-Erkennung versucht, `clang-cl` per `prefer_clang_cl_over_msvc`. Wie cc von einem Linux-Host automatisch `x86_64-w64-mingw32-gcc` wählt, steht nicht in der Doku (unbelegt; Verhalten laut Erfahrung: Prefix `<triple>-gcc` wird probiert; Step 3 prüfen, Recipe setzt die Variablen explizit).

### pkg-config-Crate (Cross-Guard)

Liest `PKG_CONFIG_PATH`, `PKG_CONFIG_LIBDIR`, `PKG_CONFIG_SYSROOT_DIR`, `PKG_CONFIG` (Executable) und `PKG_CONFIG_ALLOW_CROSS`, jeweils auch target-suffigiert (`PKG_CONFIG_PATH_x86_64-unknown-linux-gnu` bzw. mit Unterstrichen). Guard: „This crate will allow pkg-config to be used in cross-compilation if PKG_CONFIG_SYSROOT_DIR or PKG_CONFIG is set."; `PKG_CONFIG_ALLOW_CROSS=1` umgeht ihn, aber „enabling use of pkg-config in cross-compilation without appropriate sysroot and search paths set is likely to break builds." Für ICU nur relevant, falls `mingw64-opus` statt der gebündelten libopus verwendet wird; cpal/ALSA betrifft nur den Linux-Host-Build.

### cmake-Crate (Cross-Verhalten, aus Quelltext)

- Liest `CMAKE_TOOLCHAIN_FILE`, `CMAKE_GENERATOR`, `CMAKE_PREFIX_PATH`, `CMAKE`, `NUM_JOBS`, `CARGO_MAKEFLAGS`, `TARGET`/`HOST`, `CARGO_CFG_TARGET_OS/ARCH`; target-suffigierte Varianten (`CMAKE_<TARGET>`, `HOST_CMAKE`) existieren laut Fetch, ob auch `CMAKE_TOOLCHAIN_FILE_<target>` gilt: unbelegt (Step 3).
- Beim Cross-Compile nach Windows von Nicht-Windows: setzt `CMAKE_SYSTEM_NAME`/`CMAKE_SYSTEM_PROCESSOR` aus Target-OS/-Arch und sucht einen `windres` durch Ersetzen von `gcc` im C-Compiler-Namen („If we're cross compiling onto windows, then set some variables which will hopefully get things to succeed"). Kein automatischer „MinGW Makefiles"-Generator im Cross-Fall (nur nativ auf Windows). C/C++-Compiler kommt aus `cc::Build` (also aus den `CC_<target>`-Variablen), wird nur gesetzt, wenn kein Toolchain-File und nicht schon definiert.

### Kandidat A: windows-gnu + Fedora mingw64

- Vollständig aus Fedora-Paketen; Rust-std entweder aus `rust-std-static-x86_64-pc-windows-gnu` (nur mit exakt passendem rustc, aktuell 1.98.1 vs 1.98.0 installiert) oder via rustup.
- ABI: GNU-ABI, msvcrt. Kein MSVC-ABI, daher keine MSVC-only-Bibliotheken; für ICU sind keine geplant.
- Fedora-Toolchain-File für cmake vorhanden (siehe oben); `x86_64-w64-mingw32-pkg-config` findet `mingw64-opus`.
- Risiko DLL-Abhängigkeiten: mingw-C++ (openh264) zieht normalerweise `libstdc++-6.dll`, `libgcc_s_seh-1.dll`, `libwinpthread-1.dll`; `mingw64-winpthreads-static` liefert die statische winpthreads. Ob rustc's self-contained windows-gnu-Modus libgcc bereits statisch bündelt, ist auf der windows-gnu-Seite nicht dokumentiert (unbelegt); Gegenmaßnahme im Rezept: `-static-libgcc -static-libstdc++` als Link-Args, in Step 3 per `x86_64-w64-mingw32-objdump -p … | grep "DLL Name"` verifizieren.
- Wine 11.0 verfügbar für einen Start-Smoke-Test (`--version`, Protokoll-Handshake); Audio/GUI/Kamera unter Wine sind kein Nachweis.

### Kandidat B: windows-msvc via cargo-xwin

- cargo-xwin 0.23.1 (MIT). Voraussetzungen: clang, `llvm-tools` (rustup-Komponente oder volle LLVM-Installation), optional wine für `cargo xwin test`; Fedora liefert clang-cl, lld-link, llvm-lib, llvm-rc, ninja-build. Cross-Compiler wählbar: `clang-cl` (cl-Driver-Modus) oder `clang` (GNU-Driver-Modus, aber MSVC-ABI), Env `XWIN_CROSS_COMPILER`.
- Lädt Microsoft CRT und Windows SDK via `xwin` (Apache-2.0 OR MIT). README wörtlich: „By using this software you are consented to accept the license at https://go.microsoft.com/fwlink/?LinkId=2086102". xwin: `--accept-license` bzw. `XWIN_ACCEPT_LICENSE=1`. Der Link führt auf die „Microsoft Visual Studio 2019 Remote Debugger, Stand-alone Profiler, IntelliTrace, Snapshot Debugger, and Build Tools"-Lizenzseite, deren Text nur als .docx angeboten wird; der Wortlaut der Nutzungsbeschränkungen (insbesondere ob Nutzung auf Nicht-Windows-Hosts erlaubt ist) ist **unbelegt** (nicht gelesen).
- CMake: „automatically generates CMake toolchain files for projects using cmake-rs, requiring ninja".
- Unterstützte Archs: x86, x86_64, aarch, aarch64; MSVC-Version wählbar (`XWIN_VERSION`), Cache-Dir (`XWIN_CACHE_DIR`), `cargo xwin cache xwin` für Offline-Builds.
- Vorteile: MSVC-ABI wie native Windows-Builds; ring liefert vorgebaute asm-Objekte explizit für MSVC; aws-lc-rs `prebuilt-nasm` explizit für Windows x86-64. Nachteil: Lizenzgrauzone beim SDK-Download, C++-Cross-Build (openh264) mit clang-cl von Linux nicht durch Vendor-Doku belegt.

### Kandidat C: cargo-zigbuild

cargo-zigbuild 0.23.4 (MIT), Caveats wörtlich: „Currently only Linux and macOS targets are supported, other target platforms can be added if you can make it work, pull requests are welcome." Windows (gnu oder msvc) wird nicht unterstützt. Die Annahme der Aufgabenstellung („nur gnu-Target") ist damit falsch. Für `*-musl` wird zig als Weg zu vollstatischen Binaries erwähnt. Kompatibilität Fedora-zig 0.16.0 mit cargo-zigbuild 0.23.4: unbelegt.

### Kandidat D: nativer Windows-Runner

GitHub-hosted Windows-Runner existieren (nicht per Quelle geprüft, unbelegt in diesem Dokument); git.alpha-labs.net hat laut Aufgabenstellung nur einen Linux-Runner. Ohne Windows-Runner und ohne Windows-Rechner ist D für Step 3 nicht verfügbar; sobald ein Windows-Rechner da ist, dient er als Verifikationsziel (Audio, Hotkeys, Overlay, SmartScreen), nicht als Build-Host.

### Dependencies einzeln

Versionen = crates.io `max_stable_version` am 2026-09-20.

| Crate (Version) | Native Build | Cross windows-gnu | Cross msvc/xwin | musl (Server) | Fallen |
| --- | --- | --- | --- | --- | --- |
| quinn 0.11.12 (main: 0.12.0) | pure Rust | ja | ja | ja | Default-Features auf main: `rustls-ring`; `rustls-aws-lc-rs` ist opt-in (`default-features = false`). README: „tested on Linux, macOS and Windows". |
| rustls 0.23.45 | pure Rust + Provider | ja | ja | ja | README (main) spricht von 0.24 mit Pflicht zur expliziten Provider-Wahl; crates.io stable ist 0.23.45. Empfehlung des Projekts: aws-lc-rs („complete feature set and performance"), ring „easier to build on a variety of platforms". Client-Zertifikate: `WebPkiClientVerifier` + `with_client_cert_verifier()` (docs.rs). |
| aws-lc-rs 1.18.1 / aws-lc-sys | C via `cc`-Builder; Non-FIPS: „CMake: Never required", „Bindgen: Never required" (universelle vorgenerierte Bindings) | Platform-Support: Build ✓ Tests ✓; vorgenerierte Bindings für `x86_64_pc_windows_gnu`; Windows-Seite: für windows-gnu „C/C++ compiler and NASM" | Build ✓ Tests ✓; `prebuilt-nasm`-Feature bzw. `AWS_LC_SYS_PREBUILT_NASM=1` erspart NASM (Non-FIPS, x86-64) | Build ✓ Tests ✓ FIPS ✓; vorgenerierte Bindings für `x86_64_unknown_linux_musl` | NASM muss für den mingw-Cross-Build win64-Objekte erzeugen; ob `prebuilt-nasm` auch beim gnu-Target greift: unbelegt (Step 3). Cross-Probe „runs only when the host can launch the target directly or through `CARGO_TARGET_<TRIPLE>_RUNNER`". Die Aufgaben-Annahme „braucht cmake" gilt nur für FIPS. |
| ring 0.17.14 | C-Compiler; crates.io-Paket enthält „pre-compiled object files for assembly modules on Windows x86/x86-64"; Perl nur aus Git | BUILDING.md nennt nur MSVC-Toolchain für Windows; gnu nicht erwähnt (unbelegt) | ja (dokumentiert) | nicht erwähnt (unbelegt) | Letztes Release 2025-03-11, 18 Monate ohne Release; README: „experimental project", warnt vor Toolchains, die von Chromes Clang abweichen. Cross: `TARGET_CC`/`TARGET_AR` setzen. |
| opus 0.4.0 → opusic-sys 0.7.5 | cmake (bundled libopus 1.6.1), ninja optional; `bundled` abschaltbar → `OPUS_LIB_DIR`, `OPUS_LIB_STATIC=true` | über cmake-Crate + `CMAKE_TOOLCHAIN_FILE` (Fedora-File) oder System-`mingw64-opus` 1.5.2 per pkg-config | über cargo-xwin-generiertes Toolchain-File (ninja) | Server dekodiert nicht (Relay); nicht nötig | audiopus_sys 0.2.2 (2021, Opus 1.3) ist veraltet → verworfen. |
| cpal 0.18.2 (master 0.19.0) | Windows: `windows` 0.62 + `windows-core`, keine C-Deps (ASIO optional, nicht geplant); Linux: `alsa` 0.12 → alsa-lib (pkg-config, „alsa-lib-devel on Fedora"), optional jack/pulseaudio/pipewire | ja („supports MinGW-w64 for cross-compiling Windows applications from Linux/macOS") | ja | n/a | Linux-Client linkt libasound dynamisch → Client nicht musl-static (siehe packaging-licensing.md). |
| egui/eframe 0.36.2 | pure Rust; Backends glow (OpenGL) oder wgpu | ja (unbelegt für Cross, keine C-Deps) | ja | n/a | GL/Vulkan-Loader zur Laufzeit; auf Windows via `windows`/`windows-sys`. |
| iced 0.14.0 | pure Rust (wgpu/tiny-skia) | unbelegt | unbelegt | n/a | Letztes Release 2025-12-07. |
| wgpu 30.0.1 | pure Rust (DX12 via windows-rs, Vulkan via ash) | unbelegt | unbelegt | n/a | C-Freiheit der Backends nicht per Quelle geprüft. |
| winit 0.30.13 | pure Rust; Wayland: „By default, Winit loads system libraries using dlopen" (Feature `wayland-dlopen`) | ja | ja | n/a | Wayland-Libs werden zur Laufzeit geladen → glibc-dynamisches Linux-Client-Binary. |
| nokhwa 0.10.11 | Windows: `nokhwa-bindings-windows` → `windows` **0.43** (Features Win32_Media_MediaFoundation u. a.), keine C-Deps; Linux: `v4l` 0.14 + `v4l2-sys-mit` 0.3; Default-Feature `decoding` = mozjpeg | ja (windows-rs) | ja | n/a | Zwei `windows`-Crate-Versionen (0.43 nokhwa, 0.62 cpal) im Build → längere Compile-Zeit, keine Konflikte. mozjpeg-sys baut C (unbelegt) → `decoding` abschalten, wenn Frames direkt als NV12/YUYV verarbeitet werden. Ob `v4l` libv4l dynamisch braucht: unbelegt. |
| openh264 0.9.8 / openh264-sys2 0.9.8 | `cc` mit `.cpp(true)`, Quellcode gebündelt; NASM wird im PATH automatisch erkannt („up to 3x speed"), Define `WIN64` für x86_64-Windows; Stack-Protector-Flags auf MinGW abgeschaltet | README: „full support for Windows (MSVC/GNU)"; Cross braucht `CXX` (README) bzw. `CXX_<target>` | „Windows (MSVC)" getestet, Cross via clang-cl unbelegt | n/a | build.rs setzt **kein** `-static-libstdc++` → `libstdc++-6.dll`-Abhängigkeit ohne Gegenmaßnahme. NASM-Objektformat beim Cross: build.rs wählt nicht explizit win64/elf64 (unbelegt, ob cc/nasm das per Target korrekt tut) → Step 3. |
| libvpx (env-libvpx-sys 5.1.3) | configure/make, NASM/Yasm für x86; „All Windows builds require that Cygwin or MSYS2 be installed" | kein `mingw64-libvpx` in Fedora, Source-Cross-Build via configure `x86_64-win64-gcc` (nicht getestet) | unbelegt | n/a | Crate von 2023, MPL-2.0. Build-Aufwand deutlich über openh264. |
| nnnoiseless 0.5.2 | pure Rust | ja | ja | n/a | — |
| rcgen 0.14.10, tokio 1.53.1, postcard 1.1.3, rubato 5.0.0 | pure Rust | ja | ja | ja | — |
| tray-icon 0.25.1 | Windows: `windows-sys`; Linux: Default-Backend GTK3 + libxdo + libappindicator/libayatana-appindicator, alternativ KSNI (D-Bus, keine Systemlibs) | ja | ja | n/a | Linux: KSNI-Backend wählen, sonst GTK-Abhängigkeit. |
| ashpd 0.13.13 | pure Rust (zbus), nur Linux | n/a | n/a | n/a | — |
| evdev 0.13.2 | nur Linux; C-Freiheit unbelegt | n/a | n/a | n/a | — |
| windows 0.62.2 | pure Rust, Import-Libs im Crate-Bundle | ja (Import-Libs für gnu im Bundle: unbelegt, Step 3) | ja | n/a | — |

### Server: musl

- rust-std für musl nur via rustup (kein Fedora-Paket).
- C-Deps des Servers: nur der Crypto-Provider (aws-lc-sys oder ring). aws-lc-rs: musl Build ✓ Tests ✓, vorgenerierte Bindings. Für den C-Anteil `CC_x86_64_unknown_linux_musl=musl-gcc` (Fedora `musl-gcc` 1.2.5 + `musl-libc-static`).
- `+crt-static` ist Default für das musl-Target (Reference). Ergebnis: ein Binary ohne Laufzeitabhängigkeiten, direkt in ein `scratch`-Image legbar.
- S2 (glibc `+crt-static`) ist möglich, aber Reference warnt allgemein: „It's recommended to inspect the resulting binary"; glibc-Statik hat NSS/dlopen-Einschränkungen (allgemein bekannt, hier unbelegt) → nicht gewählt.

## Entscheidung

**Revidiert 2026-09-20 (AEC).** Kandidat A (`x86_64-pc-windows-gnu`) bleibt der Primärweg, hat aber seit heute **eine belegte Lücke**: das für die Echokompensation entschiedene Crate `webrtc-audio-processing` ~2.1 (`bundled`) lässt sich damit **nicht** bauen. Ursache ist nicht eine fehlende Abhängigkeit, sondern das Crate selbst: sein `build.rs` ruft `meson setup` ohne `--cross-file`, weshalb meson die WebRTC-Bibliothek für den **Host** baut — die im Windows-Target-Verzeichnis erzeugte `libwebrtc-audio-processing-2.a` enthält nachweislich ELF-x86-64-Objekte (`file` auf ein extrahiertes `.o`). Der unmittelbare Abbruch davor (`x86_64-w64-mingw32-g++` findet `absl/base/config.h` nicht, weil der Include-Pfad auf das nie entpackte meson-Subproject zeigt) ist nur das erste Symptom; selbst behoben würde der Link an den ELF-Objekten scheitern.

Gelöst 2026-09-26 (A1, Variante von (a) ohne Patch am Crate): `packaging/windows/toolchain/meson` steht in `PATH` vor meson, gibt `meson setup` für `x86_64-pc-windows-gnu` das Cross-File `packaging/windows/toolchain/mingw64.ini` mit (abseil mit `-DNTDDI_VERSION=0x0A000000`, statisches libstdc++) und streicht `WEBRTC_ENABLE_SYMBOL_EXPORT` aus den kopierten Quellen; `packaging/windows/build.sh` baut so mit `aec`.

Zweiter Punkt, der hierher gehört: auch der **native** Linux-Build des Crates braucht auf Fedora einen Eingriff, weil `build.rs` den objcopy-Pfad hart als `<rustc-sysroot>/lib/rustlib/<HOST>/bin/rust-objcopy` bildet und Fedoras `rust`-Paket diese Komponente nicht liefert. Workaround (Symlink auf `/usr/bin/llvm-objcopy`) samt Rückbau steht in `docs/reference/client.md` §12.

1. **Toolchain: Fedora-`rust` + `rust-std-static-x86_64-pc-windows-gnu`** für den Windows-Client (Step 3 verifiziert; die Paketabhängigkeit hält rustc und std auf gleicher Version). **rustup nur für das musl-Server-Target** (kein Fedora-Paket), alternativ Server-Build im Container. Pinning per `rust-toolchain.toml` (`channel = "stable"`, `targets = ["x86_64-pc-windows-gnu", "x86_64-unknown-linux-musl"]`). Fallback nur für windows-gnu ohne rustup: `dnf install rust-std-static-x86_64-pc-windows-gnu` zusammen mit dem passenden `rust`-Update.
2. **Windows-Client: Kandidat A (`x86_64-pc-windows-gnu`, Fedora mingw64)** als Primärweg. Begründung: Tier 1, Toolchain vollständig aus Distro-Paketen ohne Lizenzgrauzone, cmake-Toolchain-File vorhanden, openh264 nennt GNU explizit als getestet, aws-lc-rs nennt windows-gnu explizit mit Build/Tests ✓ und vorgenerierten Bindings.
3. **Fallback: Kandidat B (cargo-xwin)**, dokumentiert und in Step 3 ebenfalls einmal durchgebaut, falls A an NASM-Objektformat, libstdc++-Statik oder Runtime-Problemen scheitert. Vor produktiver Nutzung den EULA-Text lesen (offener Punkt).
4. **Crypto-Provider: ring** für Server und Client (Step 3: cross-compiliert nach windows-gnu ohne Zusatzaufwand, siehe Verified against). aws-lc-rs bleibt dokumentierte Alternative, falls ring (letztes Release 2025-03) aufgegeben wird; sein mingw-Cross-Build ist ungetestet.
5. **Server: `x86_64-unknown-linux-musl`**, vollstatisch.
6. **Video-Codec-Build**: openh264 (cc-Build, gebündelte Quellen) ist für Cross-Build die einzige mit Vendor-Doku belegte Option; libvpx wird build-technisch verworfen. Die Patent-/Lizenzfrage steht in packaging-licensing.md.

### Rezept für Step 3

Pakete (Fedora 43):

```
sudo dnf install rustup mingw64-gcc mingw64-gcc-c++ mingw64-binutils mingw64-filesystem \
  mingw64-winpthreads-static nasm cmake ninja-build musl-gcc musl-libc-static \
  alsa-lib-devel clang lld llvm wine
rustup-init -y --default-toolchain stable --profile minimal
# ~/.cargo/bin muss in PATH vor /usr/bin stehen (oder Fedora-Paket `rust` entfernen), sonst
# baut Fedoras rustc 1.98.0 ohne musl-std und mit unpassender windows-gnu-std.
rustup target add x86_64-pc-windows-gnu x86_64-unknown-linux-musl
# Fallback B:
cargo install --locked cargo-xwin
rustup target add x86_64-pc-windows-msvc
```

`rust-toolchain.toml` im Repo-Root:

```
[toolchain]
channel = "stable"
targets = ["x86_64-pc-windows-gnu", "x86_64-unknown-linux-musl"]
```

`.cargo/config.toml`:

```
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"
# target/windows-static/ enthält NUR libstdc++.a (Kopie aus /usr/x86_64-w64-mingw32/sys-root/mingw/lib/);
# -static-libstdc++ wirkt bei rustc nicht, weil openh264-sys2 `-lstdc++` dynamisch anfordert (Step 3).
rustflags = ["-L", "native=target/windows-static", "-C", "link-arg=-static-libgcc", "-C", "link-arg=-static"]

[target.x86_64-unknown-linux-musl]
rustflags = ["-C", "target-feature=+crt-static"]

[env]
CC_x86_64_pc_windows_gnu  = "x86_64-w64-mingw32-gcc"
CXX_x86_64_pc_windows_gnu = "x86_64-w64-mingw32-g++"
AR_x86_64_pc_windows_gnu  = "x86_64-w64-mingw32-ar"
PKG_CONFIG_x86_64_pc_windows_gnu = "x86_64-w64-mingw32-pkg-config"  # nur für System-mingw64-opus statt bundled; setzt den Cross-Guard der pkg-config-Crate außer Kraft (Step 3)
CC_x86_64_unknown_linux_musl = "musl-gcc"
AR_x86_64_unknown_linux_musl = "ar"
```

Beim Cross-Build zusätzlich in der Shell (nicht in `[env]`, weil es den Host-Build stören würde, solange die target-suffigierte Form unbelegt ist):

```
CMAKE_TOOLCHAIN_FILE=/usr/share/mingw/toolchain-mingw64.cmake \
CMAKE_GENERATOR=Ninja \
cargo build --release --target x86_64-pc-windows-gnu -p icu-client
cargo build --release --target x86_64-unknown-linux-musl -p icu-server
```

Verifikation:

```
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/icu-client.exe | grep "DLL Name"
# erwartet: nur System-DLLs (KERNEL32, msvcrt, ws2_32, ole32, mfplat/mf, …); kein libstdc++-6, libgcc_s_seh-1, libwinpthread-1
file target/x86_64-unknown-linux-musl/release/icu-server       # "statically linked"
ldd  target/x86_64-unknown-linux-musl/release/icu-server       # "not a dynamic executable"
wine target/x86_64-pc-windows-gnu/release/icu-client.exe --version
```

Fallback B:

```
XWIN_ACCEPT_LICENSE=1 cargo xwin build --release --target x86_64-pc-windows-msvc -p icu-client
```

## Verworfen, mit Grund

- **cargo-zigbuild**: unterstützt laut eigener README nur Linux- und macOS-Targets; kein Windows.
- **x86_64-pc-windows-gnullvm**: Tier 2, kein Fedora-rust-std, kein llvm-mingw-Paket in Fedora geprüft, Tests nicht in CI; bringt nur UCRT, das ICU nicht braucht.
- **Nativer Windows-Runner**: nicht verfügbar (kein Windows-Runner auf git.alpha-labs.net, kein Windows-Rechner); später als Testziel.
- **audiopus_sys**: letztes Release 2021, Opus 1.3; opusic-sys bündelt 1.6.1.
- **libvpx als Cross-Build**: configure/make + MSYS-Anforderung auf Windows, kein mingw64-libvpx in Fedora, Rust-Binding von 2023 (MPL-2.0).
- **glibc-static Server (S2)**: musl ist dafür gemacht und von aws-lc-rs mit Tests abgedeckt.
- **cargo-xwin als Primärweg**: EULA-Text ungelesen, C++-Cross via clang-cl nicht durch openh264-Doku belegt; bleibt Fallback.

## Offene Punkte für Step 3

1. Vollständiger Cross-Build des Client-Workspaces (quinn+rustls+aws-lc-rs, opus/opusic-sys, cpal, eframe, nokhwa ohne `decoding`, openh264, tray-icon, windows) mit dem Rezept; Build-Log archivieren.
2. DLL-Abhängigkeiten der .exe per objdump prüfen; falls libstdc++/libgcc/winpthread auftauchen: `-C link-self-contained=yes` bzw. `-static` testen, Ergebnis dokumentieren.
3. aws-lc-sys unter mingw: erzeugt NASM win64-Objekte? Greift `prebuilt-nasm` beim gnu-Target? Wenn nein: `AWS_LC_SYS_NO_ASM`-Variante (in aws-lc-sys-README nicht gefunden, unbelegt) oder ring-Fallback.
4. openh264-sys2 unter mingw mit NASM im PATH: baut und läuft (Encode/Decode-Roundtrip-Test unter Wine oder auf Windows-Rechner)?
5. ring-Fallback: baut ring 0.17.14 mit `TARGET_CC=x86_64-w64-mingw32-gcc` für windows-gnu (nicht dokumentiert)?
6. cmake-Crate: gilt `CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_gnu`? Wenn ja, in `[env]` übernehmen.
7. `windows`-Crate: liegen Import-Libs für das gnu-Target bei (Link-Erfolg reicht als Nachweis)?
8. musl-Server: `file`/`ldd`, dann Start in `FROM scratch`-Container; aws-lc-sys mit `CC=musl-gcc`.
9. cargo-xwin-Fallback einmal durchbauen; vorher den EULA-Text (docx) lesen und die Fundstelle zur Host-Beschränkung zitieren.
10. Nicht erreichbar am 2026-09-20 (kein Inhalt): `https://doc.rust-lang.org/rustc/platform-support/unknown-linux-musl.html` (404), Windows-SDK-EULA (`https://visualstudio.microsoft.com/license-terms/mlt031519/`, nur Download-Link).
11. Nach `rustup-init`: `which rustc` muss `~/.cargo/bin/rustc` zeigen; Fedora-`rust` ggf. entfernen.
12. Wine-Smoke-Test ist kein Ersatz für einen echten Windows-Rechner (WASAPI, Media Foundation, Hotkeys): Testplan für den späteren Windows-Zugriff anlegen.

## Verified against

[x] Step 3 am 2026-09-20 (Details in `../history/verification.md`):

- Spike 1 **bestätigt Kandidat A** mit dem vollen Dependency-Satz (quinn+rustls/ring, rcgen, tokio, opus/audiopus_sys via CMake, nnnoiseless, cpal, eframe 0.33 glow, nokhwa, openh264 `source`): Build ok beim ersten Versuch, 13,4 MB `.exe`, unter Wine lauffähig.
- **Korrektur zur Toolchain**: `dnf install rust-std-static-x86_64-pc-windows-gnu` zieht das passende `rust`-Update mit (Paketabhängigkeit); rustc ist seitdem 1.98.1. Für windows-gnu ist **kein rustup nötig**. rustup bleibt nur für das musl-Server-Target erforderlich (oder ein Container-Build), Entscheidung 1 wird entsprechend eingeschränkt.
- Punkt 2 **beantwortet**: `-static-libstdc++` wirkt nicht (openh264-sys2 emittiert `rustc-link-lib=stdc++`, rustc linkt es dynamisch); `-static-libgcc`/`-static` allein reichen ebenfalls nicht. Wirksam: Verzeichnis mit ausschließlich `libstdc++.a` per `-L native=` vor die Systempfade. Danach nur Windows-System-DLLs (inkl. mf/mfplat/mfreadwrite von nokhwa). Rezept unten korrigiert.
- Punkt 4 **bestätigt**: openh264-sys2 baut unter mingw mit nasm aus dem PATH; Encoder initialisiert (Linux nativ gemessen: 2,5 ms je 320×240-Frame).
- Punkt 5 **bestätigt**: ring 0.17.14 baut für windows-gnu ohne weitere Variablen (cc findet `x86_64-w64-mingw32-gcc` über den Target-Prefix). Entscheidung 4 wird daher auf **ring** gedreht; aws-lc-rs (Punkt 3) ist ungetestet und nicht nötig.
- Punkt 6 **nicht nötig**: audiopus_sys baute libopus per cmake-Crate ohne `CMAKE_TOOLCHAIN_FILE` (cmake-Crate setzt `CMAKE_SYSTEM_NAME` selbst). Für opusic-sys in der Implementierung erneut prüfen.
- Punkt 7 **bestätigt**: `windows` 0.52/0.53 Import-Libs für gnu liegen bei (Link erfolgreich).
- Punkt 12 **beobachtet**: Wine startet das Binary, WASAPI-Enumeration und MF-Abfrage laufen; quinn scheitert unter Wine an WSAENOPROTOOPT (Wine-Defizit). Echter Windows-Test bleibt offen.
- Offen: 8 (musl-Server), 9 (cargo-xwin), 11.

Mit WebFetch gelesen (Inhalt erhalten), 2026-09-20:

- https://doc.rust-lang.org/nightly/rustc/platform-support.html
- https://doc.rust-lang.org/rustc/platform-support.html
- https://raw.githubusercontent.com/rust-lang/rust/master/src/doc/rustc/src/platform-support.md
- https://doc.rust-lang.org/rustc/platform-support/windows-gnu.html
- https://raw.githubusercontent.com/rust-lang/rust/master/src/doc/rustc/src/platform-support/windows-gnu.md
- https://doc.rust-lang.org/rustc/platform-support/windows-gnullvm.html
- https://doc.rust-lang.org/reference/linkage.html
- https://doc.rust-lang.org/rustc/codegen-options/index.html
- https://doc.rust-lang.org/cargo/reference/config.html
- https://github.com/rust-cross/cargo-xwin
- https://raw.githubusercontent.com/rust-cross/cargo-xwin/main/README.md
- https://github.com/Jake-Shadle/xwin
- https://github.com/rust-cross/cargo-zigbuild
- https://docs.rs/cc/latest/cc/
- https://docs.rs/pkg-config/latest/pkg_config/
- https://docs.rs/cmake/latest/cmake/
- https://docs.rs/cmake/latest/cmake/struct.Config.html
- https://docs.rs/crate/cmake/latest/source/src/lib.rs
- https://github.com/rust-lang/cmake-rs/blob/master/README.md
- https://aws.github.io/aws-lc-rs/requirements/index.html
- https://aws.github.io/aws-lc-rs/requirements/windows.html
- https://aws.github.io/aws-lc-rs/requirements/linux.html
- https://aws.github.io/aws-lc-rs/platform_support.html
- https://aws.github.io/aws-lc-rs/faq.html
- https://github.com/aws/aws-lc-rs/blob/main/aws-lc-sys/README.md
- https://github.com/briansmith/ring/blob/main/BUILDING.md
- https://github.com/briansmith/ring
- https://github.com/quinn-rs/quinn
- https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn/Cargo.toml
- https://github.com/rustls/rustls
- https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html
- https://github.com/SpaceManiac/opus-rs
- https://github.com/DoumanAsh/opusic-sys
- https://github.com/lakelezz/audiopus_sys
- https://github.com/RustAudio/cpal
- https://raw.githubusercontent.com/RustAudio/cpal/master/Cargo.toml
- https://github.com/l1npengtul/nokhwa
- https://docs.rs/nokhwa/latest/nokhwa/
- https://raw.githubusercontent.com/l1npengtul/nokhwa/senpai/nokhwa-bindings-windows/Cargo.toml
- https://raw.githubusercontent.com/l1npengtul/nokhwa/senpai/nokhwa-bindings-linux/Cargo.toml
- https://github.com/ralfbiedert/openh264-rs
- https://raw.githubusercontent.com/ralfbiedert/openh264-rs/master/openh264-sys2/build.rs
- https://github.com/webmproject/libvpx
- https://docs.rs/winit/latest/winit/platform/wayland/index.html
- https://docs.rs/wayland-sys/latest/wayland_sys/
- https://github.com/tauri-apps/tray-icon
- crates.io-API (`https://crates.io/api/v1/crates/<name>`) für: quinn, rustls, aws-lc-rs, ring, opus, opusic-sys, audiopus_sys, cpal, egui, eframe, iced, wgpu, winit, nokhwa, openh264, openh264-sys2, env-libvpx-sys, nnnoiseless, rcgen, tokio, postcard, tray-icon, ashpd, evdev, windows, rubato, cargo-xwin, cargo-zigbuild
