# Verifikation (Step 3)

Zwei Arten: **A. Claim-Check** (jede Entscheidung in `Research/*.md` gegen Primärquellen, Eintrag in der jeweiligen "Verified against"-Zeile) und **B. empirische Spikes** (Wegwerf-Code im Scratchpad, Ergebnisse hier).

## Host-Inventar (2026-09-20, read-only erhoben)

| Punkt | Befund |
| --- | --- |
| OS / Kernel | Fedora 43, Linux 7.2.5 |
| Session | `XDG_SESSION_TYPE=wayland`, `XDG_CURRENT_DESKTOP=KDE`, KWin 6.7.5 |
| Portal | xdg-desktop-portal 1.20.4, xdg-desktop-portal-kde 6.7.5; `org.freedesktop.portal.GlobalShortcuts` Version 1 vorhanden, Signale `Activated` **und** `Deactivated` (Press/Release für PTT) |
| Audio | PipeWire 1.4.11 (PulseAudio-Kompat), clock.rate 48000, default quantum 1024, min-quantum 32 |
| Kamera | `/dev/video0`, `/dev/video1` vorhanden; User in Gruppen `video` und `input` (evdev-Zugriff möglich) |
| Rust | rustc/cargo 1.98.0 aus Fedora-Paketen, **kein rustup**; installiertes Target nur `x86_64-unknown-linux-gnu`. Nach `dnf install rust-std-static-x86_64-pc-windows-gnu` (Spike 1) automatisch auf **1.98.1** aktualisiert, Std und Compiler passen. |
| Cross-Pakete verfügbar (dnf, nicht installiert) | `rust-std-static-x86_64-pc-windows-gnu` 1.98.1, `mingw64-gcc` 15.2.1, `mingw64-winpthreads-static` 13.0.0, `mingw64-opus` 1.5.2, `wine-core` 11.0 |
| Cross-Pakete nicht gefunden | `rust-std-static-x86_64-unknown-linux-musl` (kein Fedora-Paket; musl-Server-Build braucht rustup oder Container) |
| Vorhanden | cmake 3.31.11, nasm 2.16.03, clang 21.1.8 |
| Fehlt | mingw64-gcc, wine, lld, pipewire-devel, alsa-lib-devel, libopus-devel |

## A. Claim-Check (2026-09-20)

Alle 14 Research-Dokumente führen unter „Primärquellen“ ausschließlich RFCs, Vendor-Doku (Microsoft Learn, opus-codec.org, docs.pipewire.org, freedesktop/KDE/GNOME, wayland.app, rust-lang, aws, Cisco, Via LA), Crate-Doku (docs.rs) oder offizielle Repos; Sekundärquellen sind als solche markiert (TeamSpeak-SDK-Spiegel, Wikipedia, choosealicense). Jedes Dokument listet unter „Verified against“ die tatsächlich per WebFetch (bzw. `curl` für große Quellen) gelesenen URLs und trägt seit heute die Step-3-Ergebnisse. Nicht belegbare Aussagen stehen als „unbelegt“ im Text.

Korrekturen aus dem Abgleich (im jeweiligen Dokument eingearbeitet):

| Dokument | Ursprüngliche Aussage | Befund | Folge |
| --- | --- | --- | --- |
| `hotkey-wayland.md` | GlobalShortcuts-Portal lokal Version 2 | `busctl` liefert `u 1` | Text korrigiert; `ConfigureShortcuts` lokal nicht verfügbar, für PTT irrelevant |
| `camera-video.md` | MJPEG als v1-Codec, weil openh264-Cross-Build unbelegt | Spike 1: openh264 baut und läuft im Windows-Cross-Build | Entscheidung revidiert: H.264 (openh264 `source`) v1, MJPEG als Feature-Fallback |
| `cross-compile.md` | rustup nötig, weil Fedora-Std 1.98.1 ≠ rustc 1.98.0 | dnf zieht das passende `rust`-Update mit | rustup nur für musl-Server; Rezept auf verifizierten Link-Trick (`libstdc++.a`-Verzeichnis) umgestellt |
| `cross-compile.md`, `transport.md` | Crypto-Provider aws-lc-rs primär bzw. offen | ring cross-compiliert ohne Zusatzaufwand | ring für Server und Client; aws-lc-rs als ungetestete Alternative |
| `feature-freeze.md` | Invite-Link-Format, PTT-Backend-Reihenfolge, Indicator-Quelle wichen von `auth-enrollment.md` / `hotkey-wayland.md` / `wire.md` ab | — | An die jeweils spezifischen Dokumente angeglichen |
| `codec.md` → `wire.md` | Verlust-Feedback-Nachricht fehlt im Protokoll | — | `LossReport` und `KeyframeRequest` für `PROTOCOL.md` vorgemerkt |

## B. Spikes

Reihenfolge: 1 Cross-Compile-Gate, 2 QUIC-Handshake mit Client-Cert, 3 Audio-Loopback-Latenz, 4 Wayland-Hotkey und Overlay, 5 Kamera-Capture und Encode. Quellcode der Spikes liegt unter `Research/spikes/` (ohne `target/`).

### Spike 1: Cross-Compile-Gate Fedora → Windows — **PASS** (2026-09-20)

Toolchain: Fedora-Pakete `mingw64-gcc` 15.2.1, `mingw64-gcc-c++`, `mingw64-winpthreads-static`, `rust-std-static-x86_64-pc-windows-gnu` 1.98.1 (kein rustup nötig), cmake 3.31, nasm 2.16. Target `x86_64-pc-windows-gnu`.

Dependency-Satz im Spike (alle gleichzeitig gelinkt, Binary startet nativ mit allen Subsystemen initialisiert): quinn 0.11 + rustls 0.23 (ring), rcgen 0.13, tokio 1.53, opus 0.3 (audiopus_sys baut libopus per **CMake aus Quelle**, kein mingw64-opus nötig), nnnoiseless 0.5, cpal 0.16 (WASAPI via windows-rs), eframe 0.33 (glow), nokhwa 0.10 (MSMF via windows-rs, zieht mozjpeg-sys per cc), openh264 0.9.8 `source` (cc + nasm).

| Schritt | Ergebnis |
| --- | --- |
| Native Build (Linux) | ok, Binary meldet opus/cpal/nokhwa (2 Kameras)/openh264/quinn initialisiert |
| `cargo build --release --target x86_64-pc-windows-gnu` | **ok beim ersten Versuch**, 13,4 MB `.exe` |
| DLL-Imports (objdump) nach Fix | nur Windows-System-DLLs: kernel32, ntdll, ws2_32, bcrypt, crypt32, advapi32, ole32/combase (WASAPI/MSMF), **mf.dll/mfplat.dll/mfreadwrite.dll** (MediaFoundation, von nokhwa), msvcrt |
| Falle | Erstes Binary importierte `libstdc++-6.dll` (openh264-sys2 emittiert `cargo:rustc-link-lib=stdc++` dynamisch; `-static-libstdc++` wirkt bei rustc nicht). Fix: Verzeichnis mit **nur** `libstdc++.a` (aus `/usr/x86_64-w64-mingw32/sys-root/mingw/lib/`) per `-L native=` vor die System-Suchpfade legen. |
| Hinweis | MediaFoundation-DLLs fehlen auf Windows-N/KN-Editionen ohne Media Feature Pack und unter Wine; Kamera-Modul muss lazy laden oder das Binary startet dort nicht. Für v1: Kamera-Backend zur Laufzeit optional laden (delay-load) oder in Kauf nehmen; wird in `Client/IMPLEMENTATION.md` festgelegt. |

Rezept (`.cargo/config.toml`):

```toml
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"
ar = "x86_64-w64-mingw32-ar"
rustflags = ["-L", "native=<repo>/build/win-static", "-C", "link-arg=-static-libgcc", "-C", "link-arg=-static"]
```

`build/win-static/` enthält nur `libstdc++.a`.

Wine-Smoke-Test (wine-core 11.0, frischer Prefix): Binary startet und durchläuft alle Stufen (ring, rcgen, libopus, RNNoise, openh264, tokio, WASAPI-Enumeration liefert `PulseAudio Output`, MSMF-Kameraabfrage liefert sauber `Err` statt Absturz). Einzige Abweichung: `quinn::Endpoint::client` scheitert unter Wine mit WSAENOPROTOOPT 10045 (Wine implementiert eine Socket-Option nicht, die quinn-udp setzt). Das ist ein Wine-Defizit, kein Windows-Befund; quinn auf echtem Windows bleibt **unverifiziert** bis zur Windows-Hardware. Erster Lauf brauchte > 240 s (Prefix-Initialisierung plus MF-Init), danach < 60 s.

### Spike 2: QUIC mit mutual TLS — **PASS** (2026-09-20)

Stack: quinn 0.11.12 / quinn-proto 0.11.18, rustls 0.23.45 (ring 0.17.14), rcgen 0.13.2, tokio 1.53. Loopback 127.0.0.1.

| Messung | Ergebnis |
| --- | --- |
| Server mit `WebPkiClientVerifier` (Client-Cert Pflicht), Client mit von der CA signiertem Cert | Handshake ok, Server liest per `peer_identity()` die Cert-Kette, CN `chris` und SHA-256-Fingerprint auslesbar |
| Client **ohne** Cert | Server: `error 116: peer sent no certificates`. **Falle:** auf Client-Seite liefert `connect().await` trotzdem `Ok` (TLS 1.3, Client-Finished geht vor der Server-Ablehnung raus); die Verbindung wird 0,2 ms später mit dem TLS-Alert geschlossen. Konsequenz für das Protokoll: Client gilt erst als verbunden, wenn die erste Server-Control-Message (`ServerHello`) da ist, nicht bei `connect()`-Erfolg. |
| Handshake + Connect (Loopback) | 2,5 ms |
| 200 Datagram-Roundtrips à 104 B | min 160 µs, p50 176 µs, p99 471 µs |
| `max_datagram_size()` | 1162 B direkt nach Handshake, 1414 B nach MTU-Discovery; 1415 B → `datagram too large`. Für ~100 B Opus-Frames irrelevant, für Video-Frames: Fragmentierung nötig oder uni-Streams. |
| rcgen | kann CA (self-signed, `IsCa`), Leaf mit ClientAuth/ServerAuth-EKU und `signed_by(ca)`; CSR-Signing wird in Spike 2b geprüft, sobald `auth-enrollment.md` das Verfahren festlegt |

Nicht geprüft: Verhalten über echtes Netz/NAT, Idle-Timeout-Keepalive, Connection Migration.

### Spike 2b: Enrollment-Fragen (ALPN-Peek, unauthentifizierte Identität, CSR-Signing, postcard) — **PASS** (2026-09-20)

Stack: quinn 0.11.12, rustls 0.23.45, rcgen 0.14 (`x509-parser`), postcard 1.1 (`use-std`), serde.

| Frage aus `auth-enrollment.md` / `protocol/wire.md` | Befund |
| --- | --- |
| Eine `ServerConfig` mit `allow_unauthenticated()` für beide ALPNs: `peer_identity()` ohne Client-Cert | `None` (nicht `Some(leer)`). Mit Cert: `Some(Vec<CertificateDer>)` mit 1 Eintrag (nur Leaf; der Client schickt keine CA). |
| ALPN vor Handshake-Abschluss lesbar | Ja: `Incoming::accept()` → `Connecting::handshake_data().await` liefert `HandshakeData.protocol` (`icu-enroll/1` bzw. `icu/1`) **vor** `Connecting.await`. Damit kann der Server pro ALPN unterschiedlich verfahren, mit einer einzigen `ServerConfig`. |
| rcgen 0.14 CSR-Flow | Client `CertificateParams::serialize_request` (CSR 218 B DER). Server `CertificateSigningRequestParams::from_der`, überschreibt `params.distinguished_name`, `extended_key_usages`, `not_before/after`, dann `signed_by(&Issuer)`. Subject-Override wirkt: CSR-CN `admin-impersonator` → Leaf-CN `alice`, Issuer `CN=ICU CA`. |
| DER-Größen (P-256) | CA 366 B, Server-Leaf 360 B, Client-Leaf 318 B. Handshake-Kette Leaf+CA < 1 KiB, passt in ein Initial-Paket-Budget. |
| postcard: unbekannte Enum-Variante | `Err("Serde Deserialization Error")`, kein Panic. Bekannte Variante `Hello{minor:3}` = 2 Bytes. Capability-Gate wie in `wire.md` ist also durchsetzbar. |

### Spike 3: Audio-Loopback Capture → RNNoise → Opus → Decode → Playback — **PASS** (2026-09-20)

Stack: cpal 0.16 (Host `Alsa`, also PipeWire über das ALSA-Plugin), opus 0.3 (System-libopus 1.5.2), nnnoiseless 0.5. 48 kHz, 10-ms-Frames (480 Samples), Opus VOIP, 64 kbps Ziel, in-band FEC an, 5 % Loss-Annahme. Stumm ausgegeben, nur Timing gemessen. 4 s Laufzeit, Mikrofon-Signal des Raums.

| Messung | default | `PIPEWIRE_LATENCY=480/48000` | `PIPEWIRE_LATENCY=128/48000` |
| --- | --- | --- | --- |
| Capture-Callback-Intervall p50 | 2,67 ms (128 Samples; `BufferSize::Fixed(480)` wurde **ignoriert**) | 5,33 ms (256) | 2,66 ms (128) |
| Pipeline capture-cb → playback-cb p50 / p99 | 13,8 / 16,4 ms | 15,3 / 16,0 ms | 14,2 / 17,0 ms |
| RNNoise pro 10-ms-Frame | 1,12 ms | 1,16 ms | 1,15 ms |
| Opus encode pro Frame (avg / max) | 91 / 193 µs | 110 / 174 µs | 85 / 241 µs |
| Opus decode pro Frame | 13 µs | 14 µs | 12 µs |
| Effektive Bitrate (Raumgeräusch, kein DTX) | 47,5 kbps | 20,1 kbps | 47,7 kbps |

Befunde:
- Die Software-Pipeline kostet bei 10-ms-Frames rund **14 ms** (Frame-Sammeln 10 ms + Callback-Granularität + Verarbeitung). Nicht enthalten: ALSA-/PipeWire-Puffer vor dem Capture-Callback und nach dem Playback-Callback sowie Hardware-Latenz. Die sind mit cpal nicht sichtbar; pipewire-rs würde sie per `node.latency` steuerbar machen.
- cpal-ALSA-Backend respektiert `BufferSize::Fixed` unter PipeWire nicht; die Periodengröße kommt vom PipeWire-Quantum (`PIPEWIRE_LATENCY`). Für den Client heißt das: entweder pipewire-rs nativ oder Env-Variable beim Start setzen.
- Ohne Playout-Steuerung sammelte sich beim Start ein Rückstand von ~50 ms an, der nie abgebaut wurde. Ein Jitter-Buffer mit aktivem Ziel-Delay ist Pflicht, nicht optional.
- RNNoise kostet 11 % einer Core-Zeit pro Stream (nur auf dem eigenen Mikrofon nötig, also einmal). Opus encode/decode ist vernachlässigbar; 20 Decoder parallel wären < 1 ms pro 10 ms.
- Geräte auf dem Host: Jabra Engage 75 (Headset), C-Media USB, Logitech-Webcam-Mikrofon, HDMI. cpal zeigte nur `default`, keine Einzelgeräte; Geräteauswahl braucht ALSA-Enumeration oder pipewire-rs.

Nicht geprüft: WASAPI (Windows), akustische Ende-zu-Ende-Latenz (Lautsprecher → Mikrofon), Verhalten bei Geräte-Hotplug.

### Spike 4a: Globaler PTT-Hotkey unter KDE Wayland — **TEILWEISE** (2026-09-20)

Stack: evdev 0.13.2, ashpd 0.11.1 (tokio).

| Weg | Ergebnis |
| --- | --- |
| evdev direkt | **PASS.** Als uid 1000 (Gruppe `input`) sind alle 21 `/dev/input/event*` ohne root lesbar; Tastatur (`FSC KBPC PX`) und Maus (`Logitech USB-PS/2 Optical Mouse`, mit `BTN_SIDE`) erkannt. Liefert Press und Release, unabhängig vom Fokus und vom Compositor. |
| XDG GlobalShortcuts-Portal | **OFFEN.** `CreateSession` ok. `BindShortcuts` mit `preferred_trigger CTRL+SHIFT+F12` kam sofort mit `Portal request didn't succeed: Cancelled` zurück, ohne dass ein Dialog bestätigt wurde (unbeaufsichtigte Session). Muss von Chris interaktiv wiederholt werden: in `Research/spikes/spike-wayland/` `cargo run --bin hotkey`, KDE-Dialog bestätigen, dann Taste drücken und auf `Activated`/`Deactivated` prüfen. |

Konsequenz: evdev ist der verlässliche Linux-Pfad für PTT und ist auf diesem Host ohne Konfiguration nutzbar. Das Portal bleibt die saubere Variante für Hosts, deren User nicht in `input` ist; die Entscheidung in `hotkey-wayland.md` muss beide vorsehen.

### Spike 4b: Overlay per wlr-layer-shell auf KWin 6.7.5 — **PASS** (2026-09-20)

Stack: smithay-client-toolkit 0.19.2, wayland-client 0.31, wl_shm-Buffer ARGB8888 (premultiplied).

- `zwlr_layer_shell_v1` ist auf KWin vorhanden; Layer `Overlay`, Anchor oben rechts, 320×240, Margin 40, `KeyboardInteractivity::None`, leere Input-Region, Exclusive-Zone −1.
- Screenshot (spectacle) zeigt das halbtransparente Overlay mit transparentem Loch **über allen Fenstern**, ohne Fokus zu nehmen. Rendering per Software-Buffer reicht für Kamera-Kacheln dieser Größe.
- Nicht interaktiv geprüft: dass Klicks durch die leere Input-Region hindurchgehen (Protokoll garantiert es, KWin implementiert es; Chris kann es mit dem laufenden Spike prüfen). Nicht geprüft: Verhalten über einem Vollbild-Spiel (Fullscreen-Fenster liegen unter Layer `overlay`; nachmessen).
- Falle: `blocking_dispatch` ohne eigenen Frame-Callback-Loop blockiert ewig; der Client braucht einen Event-Loop mit Timer (calloop), nicht nur Dispatch.

### Spike 5: Kamera-Capture + H.264 — **PASS mit Einschränkung** (2026-09-20)

Stack: nokhwa 0.10.11 (`input-native`, V4L2), openh264 0.9.8 mit Feature `source` (baut libopenh264 aus Quelle, nasm vorhanden).

| Messung | Ergebnis |
| --- | --- |
| Geräte | Logitech UVC 046d:0825 (C270) als `/dev/video0` (Capture) und `/dev/video1` (Metadaten-Node, öffnet keinen Stream; Index 0 explizit wählen) |
| Format | 320×240 MJPEG 15 fps ausgehandelt |
| Reale Framerate | **3,7 fps** über nokhwa, 6 fps über `v4l2-ctl --stream-mmap`, unabhängig von Auflösung und Pixelformat. Ursache: Auto-Belichtung bei wenig Raumlicht (Exposure 30–40 ms, C270-typisch). Hardware-/Lichtproblem, nicht Software. Bandbreite bei 15 fps damit **unbelegt**. |
| MJPEG-Decode (nokhwa → RGB) | 0,24 ms/Frame |
| RGB→YUV + H.264-Encode (CameraVideoRealTime, 150 kbps Ziel) | 2,5 ms/Frame |
| H.264-Bitrate bei 3,7 fps, statisches Bild | 27–29 kbps |

Nicht geprüft: MSMF (Windows), 15 fps unter Tageslicht, Decoder-Seite, Latenz Kamera→Anzeige.

### Zusammenfassung

| Spike | Ergebnis |
| --- | --- |
| 1 Cross-Compile-Gate | PASS (Windows-gnu, voller Dependency-Satz, statisch, Wine-Start) |
| 2 QUIC mTLS | PASS (+ Falle: `connect()` vor Server-Ablehnung erfolgreich) |
| 2b Enrollment-Details | PASS (ALPN-Peek, `peer_identity` None, CSR-Override, postcard-Err) |
| 3 Audio-Loopback | PASS (Pipeline 14 ms p50 bei 10-ms-Frames; cpal-Grenzen bestätigt) |
| 4a PTT-Hotkey | evdev PASS, Portal OFFEN (interaktiv nötig) |
| 4b Overlay Layer-Shell | PASS (KWin 6.7.5, Screenshot) |
| 5 Kamera + H.264 | PASS mit Einschränkung (Kamera lichtbedingt 4–6 fps) |

## Nicht von diesem Host verifizierbar

Windows-Audio (WASAPI), Windows-Overlay, Windows-Hotkeys. Wine ist nur Smoke-Test für Start und Netz. Braucht echte Windows-Hardware in einem späteren Schritt.
