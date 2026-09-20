# ICU Client — Implementation Details

Stand 2026-09-20. Normative Grundlage: `../PROTOCOL.md`. Featureliste: `../Research/feature-freeze.md`. Entscheidungen und Belege in `../Research/`.

## 1. Zweck und Grenzen

Ein Fenster, ein Tray-Icon, ein optionales Overlay. Der Client verbindet sich mit genau einem Server, sendet das eigene Mikrofon per PTT oder VAD, mischt die Stimmen der anderen lokal, zeigt wer spricht, und blendet auf Wunsch Kamerakacheln über ein Borderless-Window-Spiel. Keine Channels, kein Chat, keine Zertifikatsverwaltung im UI.

## 2. Artefakte

| Plattform | Binary | Build | Laufzeit |
| --- | --- | --- | --- |
| Windows 10/11 x86_64 | `icu.exe`, portable, unsigniert (ZIP + SHA256SUMS) | Cross von Fedora, Target `x86_64-pc-windows-gnu`, Rezept `Research/cross-compile.md`, statisches libstdc++ | nur System-DLLs; MediaFoundation (`mf.dll`) wird lazy geladen, damit Windows-N ohne Media Feature Pack startet (Kamera dann deaktiviert) |
| Linux x86_64 | `icu`, glibc-dynamisch, Tarball; AppImage optional | nativ auf alter Baseline | Wayland/GL per dlopen, ALSA-lib nur als cpal-Fallback-Host, sonst libpipewire per dlopen |

Kein Auto-Update: `Welcome.min_client` > eigene Version → Banner „Update nötig“ mit Link.

## 3. Modul-Layout (`crates/icu-client/src/`)

```
main.rs            # Start, Argument icu://-Link, Single-Instance (zweiter Start reicht Link an laufende Instanz)
app.rs             # Zustand (AppState), Kommando-Bus zwischen Modulen (tokio mpsc/watch)
config.rs          # settings.toml + identity/ (key, leaf, ca), Pfade je OS
net/
  enroll.rs        # icu-enroll/1, CA-Pinning-Verifier, Key-Erzeugung
  conn.rs          # Verbindung, Hello/Welcome, Reconnect-Backoff, Control-Reader/-Writer
  audio_tx.rs      # Datagram-Sender (seq, ts, END)
  audio_rx.rs      # Datagram-Empfänger → Jitter-Buffer je Speaker, LossReport
  video.rs         # Uni-Streams senden/empfangen, KeyframeRequest
audio/
  backend.rs       # trait AudioBackend { enumerate, open_capture, open_playback, events }
  pipewire.rs      # Linux: pipewire-rs, node.latency 240/48000, node.group
  wasapi.rs        # Windows: IAudioClient3 shared event-driven, Fallback IAudioClient 10 ms
  cpal_fallback.rs # Notfall-Host (ALSA/andere), hinter Feature-Flag
  capture.rs       # Blöcke à 480 Samples → dsp → encoder
  playback.rs      # Mixer: Summe der Speaker-Puffer, Limiter, Ausgabe
dsp/
  denoise.rs       # nnnoiseless, schaltbar
  vad.rs           # RNNoise-VAD-Wahrscheinlichkeit oder Energie-Gate, Hangover
  gate.rs          # PTT/VAD-Zustandsautomat, Release-Delay, END-Flag
  opus.rs          # Encoder (VOIP, 48k mono, 48 kbit/s, FEC) / Decoder je Speaker, PLC, FEC-Decode
  jitter.rs        # adaptiver Jitter-Buffer (Research/latency-budget.md)
  meter.rs         # Peak/RMS für UI
hotkey/
  mod.rs           # trait PttSource { events() -> Stream<PttEvent> }, Bindung (Taste/Maustaste/Kombination)
  portal.rs        # Linux: ashpd GlobalShortcuts (Activated/Deactivated)
  evdev.rs         # Linux opt-in: /dev/input ohne grab, Tastatur + BTN_SIDE/BTN_EXTRA
  rawinput.rs      # Windows: RegisterRawInputDevices + RIDEV_INPUTSINK, Message-Only-Window-Thread
camera/
  mod.rs           # trait CameraSource { frames() -> Stream<Frame> }, Preview
  nokhwa.rs        # V4L2 / MSMF (lazy geladen), 320x240@15 MJPEG/YUYV
  h264.rs          # openh264 Encoder (CameraVideoRealTime, 150 kbit/s, IDR 2 s + on request) / Decoder
  mjpeg.rs         # Feature `mjpeg`: jpeg-encoder / zune-jpeg
overlay/
  mod.rs           # Overlay-Thread, Kachel-Komposition (premultiplied BGRA), Befehle an/aus/Position
  wayland.rs       # smithay-client-toolkit Layer-Shell overlay, leere Input-Region, calloop-Loop
  windows.rs       # WS_EX_LAYERED|TRANSPARENT|TOPMOST|NOACTIVATE|TOOLWINDOW, UpdateLayeredWindow
ui/
  main_window.rs   # eframe/egui: Teilnehmerliste, Buttons, Pegel
  settings.rs      # Settings-Dialog (sofort wirksam)
  tray.rs          # tray-icon (Windows Shell_NotifyIcon, Linux ksni)
  theme.rs         # Farben, Font, Icons; folgt System hell/dunkel
  cues.rs          # Töne (Join/Leave, Sende-Cue, Mute-Cue) als eingebettete PCM-Samples
```

Plattformcode liegt ausschließlich in `audio/pipewire.rs|wasapi.rs`, `hotkey/portal.rs|evdev.rs|rawinput.rs`, `overlay/wayland.rs|windows.rs`, `config.rs` (Pfade, DPAPI) und `ui/tray.rs` (Feature-Auswahl). Alles andere ist plattformneutral und gegen Traits geschrieben.

## 4. Threads und Datenfluss

| Thread/Task | Aufgabe | Takt |
| --- | --- | --- |
| Audio-Capture (Backend-Callback, RT-Priorität) | Samples in Ringpuffer | 5–10 ms |
| DSP/Encode (eigener Thread, hohe Priorität) | 480-Sample-Blöcke: denoise → VAD/PTT-Gate → Opus (20 ms = 2 Blöcke) → Datagram | 10 ms |
| Netz (tokio, 2 Worker) | QUIC, Control, Datagram-Empfang → Jitter-Buffer | ereignisgetrieben |
| Audio-Playback (Backend-Callback, RT) | pro Callback: aus jedem Jitter-Buffer nächsten Frame holen (Decode/PLC/FEC), summieren, Limiter | 5–10 ms |
| Kamera + H.264 (eigener Thread) | Capture → Encode → Uni-Stream | 15 fps |
| Video-Decode (eigener Thread) | Uni-Streams → Decoder je Speaker → Overlay-Kanal | ereignisgetrieben |
| Overlay (eigener Thread mit eigener Wayland-Verbindung bzw. Win32-Message-Loop) | Komposition, Fenster | 15 fps bzw. bei Änderung |
| Hotkey (Portal: tokio-Stream; evdev: Blocking-Thread; Windows: Message-Only-Window-Thread) | PttEvent → Gate | ereignisgetrieben |
| UI (eframe-Hauptthread) | Rendering, Repaint bei State-Änderung, Pegel 30 Hz | ≤ 60 Hz |

Kommunikation: `tokio::sync::watch<AppState>` für UI/Tray/Overlay (nur lesen), `mpsc` für Kommandos (Mute, Deafen, Connect, SetDevice, OverlayToggle), lock-freie Ringpuffer (`ringbuf`) zwischen Audio-Callbacks und DSP/Playback. Kein Mutex im Audio-Callback.

## 5. Verbindung und Identität

1. **Erststart / Invite-Link**: `icu://host:port/enroll?t=…&ca=…&n=…` aus Argument, URI-Handler oder Eingabefeld. Keypair P-256 (`rcgen::KeyPair::generate`), CSR mit CN = n, Enrollment-Verbindung mit ALPN `icu-enroll/1` und Pinning-Verifier (SHA-256 des letzten Kettenglieds = `ca`). Antwort speichern: `identity/key.pk8` (0600; Windows zusätzlich DPAPI User-Scope), `identity/leaf.der`, `identity/ca.der`, `identity/server.toml` (host, port, server_name).
2. **Normalstart**: Identität laden → Verbindung ALPN `icu/1` mit Client-Cert → `Hello{minor:1, caps:0, client:"icu 0.1.0 linux", initial:flags, name}` → `Welcome` → Zustand `Active`. Vorher kein Audio, keine Anzeige „verbunden“ (Spike 2: `connect()` ist kein Erfolgskriterium).
3. **Reconnect**: Backoff 1, 2, 4, 8, 15 s, unbegrenzt; UI-Zustand „verbinde…“; Mute/Deafen/Lautstärken bleiben; nach `Welcome` Audio ohne Neustart der Backends.
4. **Renewal**: ab 30 Tage vor Ablauf einmal pro Start `RenewRequest`; neues Leaf ersetzt das alte atomar.
5. URI-Handler: Windows `HKCU\Software\Classes\icu` (ohne Admin), Linux `.desktop` mit `MimeType=x-scheme-handler/icu` + `xdg-mime default`. Beides beim ersten Start, abschaltbar.

## 6. Audio-Pipeline

| Stufe | Festlegung | Quelle |
| --- | --- | --- |
| Backend Linux | pipewire-rs 0.10, Capture- und Playback-Stream in einer `node.group`, `node.latency = 240/48000`, `media.role = Communication`, Format F32 mono 48 kHz; Geräteliste aus Registry (`Audio/Source`, `Audio/Sink`), Hot-Plug über Registry-Events, `target.object` neu setzen | `audio-io.md`, Spike 3 (cpal ignoriert Fixed-Buffer) |
| Backend Windows | windows-rs `IAudioClient3::InitializeSharedAudioStream`, `PeriodInFrames` = kleinstes Vielfaches der Fundamental-Periode ≥ max(Min, 240); Capture `AudioCategory_Other` + `RAW` wenn unterstützt, Render `AudioCategory_GameChat`; Fallback `IAudioClient::Initialize` shared 10 ms mit `AUTOCONVERTPCM`; Default-Wechsel über `IMMNotificationClient` | `audio-io.md` |
| Resampling | nur wenn Mix-Format ≠ 48 kHz (`rubato::Fft` fest) | |
| Denoise | nnnoiseless 0.5, 480 Samples, Default an; erster Ausgabeblock verwerfen | Spike 3: 1,15 ms/Frame |
| VAD | RNNoise-Wahrscheinlichkeit (Schwelle Default 0,5, im Pegelmeter sichtbar) oder Energie-Gate in dBFS; Hangover 250 ms | |
| PTT-Gate | Press → sofort senden; Release → Release-Delay 150 ms (konfigurierbar) → letztes Paket mit `END`; Doppeldruck-Halten optional | `feature-freeze.md` |
| Encoder | opus 0.4 (opusic-sys, libopus 1.6.1): 48 kHz mono, VOIP, `SIGNAL_VOICE`, 48 kbit/s constrained VBR, FEC an, `PACKET_LOSS_PERC` aus `LossHint`, DTX aus, Complexity 10, Frame 20 ms (Option 10 ms) | `codec.md` |
| Sender | `seq` +1 je Paket, `ts` +960 (bzw. 480), `speaker = 0`; `send_datagram` (nicht `_wait`) | `PROTOCOL.md §3` |
| Empfang | pro Speaker ein Jitter-Buffer + Decoder; `LossReport` alle 2 s bei Änderung ≥ 10 ‰ | |
| Jitter-Buffer | Histogramm der relativen Ankunftsverzögerung (5-ms-Buckets, Forget 0,983), Ziel = 95 %-Quantil + 1 Frame, Klemme [1 Frame bei FEC aktiv, 200 ms], Start 2 Frames; Re-Anchor je Talk-Spurt (`ts`-Sprung bei lückenloser `seq`); Anpassung in Sprechpausen; Verlust → FEC-Decode aus n+1 wenn vorhanden, sonst PLC; > 100 ms Verlust → stumm; Drift-Korrektur um 1 Frame in Pausen | `latency-budget.md`, Spike 3 (ohne Playout-Ziel läuft Rückstand auf) |
| Mixer | Summe f32 × Lautstärke je User (0–200 %, 0 = lokal stumm) → Soft-Limiter → Playback; `deafened` → Playback stumm und Mute | |
| Cues | Join/Leave (Default an), Sende-Cue (Default aus), Mute-Cue (sichtbar; Ton optional) | |
| Messpunkte | Zeitstempel an Capture-Callback, Encode fertig, Send, Receive, Decode, Playback-Callback; Histogramm über Debug-Fenster exportierbar | `latency-budget.md` |

Zielwerte: Mouth-to-Ear LAN ≤ 60 ms (20-ms-Frames) / ≤ 45 ms (10 ms); Speaking-Indicator ≤ 1 Frame + 1 UI-Frame nach erstem Paket; PTT-Druck → erstes Datagram ≤ 1 Frame.

## 7. Hotkeys

| OS | Backend | Details |
| --- | --- | --- |
| Linux | Portal GlobalShortcuts (ashpd 0.13, Feature `global_shortcuts`) primär: Session anlegen, `bind_shortcuts` mit `preferred_trigger`, `Activated`/`Deactivated` → Press/Release. `Cancelled` oder Fehler → Backend nicht verfügbar (Spike 4a: passiert ohne bestätigten Dialog). | Anzeige des aktiven Backends in den Settings; Dialog-Hinweis beim ersten Binden |
| Linux opt-in | evdev 0.13 ohne `grab()`: alle lesbaren `/dev/input/event*`, Tastatur-Keycodes und `BTN_SIDE`/`BTN_EXTRA`; Hotplug per inotify auf `/dev/input`. Aktivierung nur nach Nutzerwahl, mit Erklärung zu Gruppe `input` (Keylogger-Aspekt). Spike 4a: auf Chris' Host ohne Konfiguration lesbar. | Einzige Option für Maustasten unter Wayland |
| Windows | Raw Input: verstecktes Message-Only-Window in eigenem Thread, `RegisterRawInputDevices` Keyboard (0x01/0x06) + Mouse (0x01/0x02) mit `RIDEV_INPUTSINK`; `RI_KEY_MAKE/BREAK`, `RI_MOUSE_BUTTON_4/5_DOWN/UP`. Kein Schlucken der Taste. Elevated Spiele im Fokus: Eingaben unsichtbar (UIPI), UI-Hinweis. | `hotkey-wayland.md` |

Bindung: Aufnahme-Modus in den Settings („Taste drücken“), Speicherung als Backend-neutrale Beschreibung (Keycode/Keysym/Maustaste + Modifier). Zweite Bindung für Mute-Toggle, dritte für Deafen-Toggle, vierte für PTT/VAD-Umschalten (alle optional).

## 8. Kamera und Video

- Capture hinter `CameraSource`; v1 nokhwa 0.10 (`input-native`), Windows-Backend über ein lazy geladenes Modul, damit `mf.dll` erst beim Einschalten gezogen wird (Spike 1: statischer Import macht das Binary auf Windows-N unstartbar). Linux: Capture-Node explizit (Spike 5: Metadaten-Node überspringen). Format 320×240 @ 15 fps, MJPEG oder YUYV vom Gerät.
- Encoder openh264 0.9 `source`, `CameraVideoRealTime`, 150 kbit/s, IDR alle 2 s und auf `KeyframeRequest`; SPS/PPS vor jedem IDR. Frame → Uni-Stream mit Header (`PROTOCOL.md §4`). Bei `max_datagram`/Flow-Control-Stau: Frame verwerfen, nicht puffern.
- Decoder je Speaker; erster Frame ohne IDR → `KeyframeRequest`; ≥ 3 s ohne Frame → Kachel ausblenden.
- Vorschau im Settings-Dialog vor dem Einschalten. Kamera an/aus per Button und Hotkey; `camera_on` im `SetState`.
- Feature `mjpeg` für den Fallback-Codec (jpeg-encoder/zune-jpeg), Default aus.

## 9. Overlay

Eigener Thread, eigene Compositor-Verbindung, kein winit. Inhalt: Kacheln der Teilnehmer mit Kamera (und optional nur sprechende ohne Kamera als Namensschild), Speaking-Rahmen, Name; CPU-Komposition in premultiplied BGRA (≤ 4 × 320×240 @ 15 fps).

| OS | Fenster | Belegt |
| --- | --- | --- |
| Linux Wayland | smithay-client-toolkit 0.21 Layer-Shell: Layer `overlay`, `keyboard_interactivity none`, `set_exclusive_zone(0)`, Anker Ecke + Margin, leere Input-Region, `wl_shm` ARGB8888; Event-Loop calloop mit Timer (Spike 4b: `blocking_dispatch` allein blockiert). GNOME: Layer-Shell fehlt → Overlay „nicht verfügbar“. | Spike 4b PASS auf KWin 6.7.5 |
| Windows | `CreateWindowExW(WS_EX_LAYERED\|WS_EX_TRANSPARENT\|WS_EX_TOPMOST\|WS_EX_NOACTIVATE\|WS_EX_TOOLWINDOW, WS_POPUP)`, `UpdateLayeredWindow(ULW_ALPHA)` aus 32-bit-DIB, Fenstergröße = Kachelgruppe. Nur Borderless/FSO-Spiele; UI-Hinweis. | Doku belegt, empirisch offen (kein Windows) |

Bedienung: an/aus (Button, Hotkey optional), Ecke (4 Optionen) + Offset, Kachelgröße (klein/mittel), Bearbeitungsmodus mit Drag im Hauptfenster (nicht im Overlay selbst, das bleibt klick-durchlässig).

## 10. UI

eframe/egui 0.36, Backend wgpu (Fallback glow per Feature). Ein Hauptfenster, ~320×480 px, dekorationslos mit eigener Titelleiste (Drag), abgerundet, folgt System hell/dunkel, eine eingebettete Schrift (Inter oder ähnlich, Lizenz prüfen), Icons als eingebettete SVG-Rasterungen.

```
┌──────────────────────────────┐
│ ● ICU   icu.example.org   – ×│  Status-Punkt: grün verbunden, gelb verbinde…, rot Fehler
├──────────────────────────────┤
│ ▌chris        🎥            │  Speaking-Balken links (animiert), Kamera-Icon, Mute/Deafen-Icons
│  alice   ──────●── 120 %    │  Lautstärkeregler erscheint bei Hover/Klick
│ ▌bob     🔇                 │
│  dora    🔇🔈               │
├──────────────────────────────┤
│ ▁▃▅▇▅▃▁ ─┼─  Pegel + VAD-Schwelle                       │
│ [ PTT ▾ ] [ Mute ] [ Deafen ] [ 🎥 ] [ Overlay ] [ ⚙ ] │
└──────────────────────────────┘
```

Regeln: keine modalen Fehlerdialoge (Banner im Fenster), alle Settings sofort wirksam, Fenster schließen = Tray, Doppelklick Tray = Fenster. Settings-Bereiche: Audio (Geräte, Pegel, Verstärkung, Modus PTT/VAD, Schwelle, Hotkeys, Release-Delay, Cues, Rauschunterdrückung, Loopback-Test), Kamera (Gerät, Vorschau), Overlay (an/aus, Ecke, Offset, Größe), Allgemein (Anzeigename, Autostart, URI-Handler, Backend-Anzeige), Über (Version, Lizenzen inkl. OpenH264/Opus-Hinweise).

Tray: tray-icon 0.25 (Linux `default-features = false, features = ["ksni"]`), Icon-Varianten verbunden/getrennt/mute/deafen, Menü Mute/Deafen/Fenster/Beenden.

## 11. Konfiguration

`settings.toml` unter `$XDG_CONFIG_HOME/icu/` bzw. `%APPDATA%\icu\`:

```toml
[server]      host, port, server_name              # aus dem Enrollment
[audio]       input_device, output_device, mode = "ptt"|"vad", vad_threshold, gain_db, denoise = true,
              release_ms = 150, frame_ms = 20, cue_join = true, cue_tx = false, cue_mute = true
[hotkeys]     ptt = {...}, mute = {...}, deafen = {...}, toggle_mode = {...}, backend = "auto"|"portal"|"evdev"
[camera]      device, enabled = false
[overlay]     enabled = false, corner = "top-right", offset = [40, 40], size = "small"
[volumes]     "<fingerprint>" = 1.2                  # pro Peer, persistent
[ui]          name, start_minimized = false, register_uri_handler = true
```

## 12. Build und Packaging

- Workspace-Root `.cargo/config.toml` und `build/win-static/libstdc++.a` nach `Research/cross-compile.md` (verifiziertes Rezept).
- `cargo build --release -p icu-client` (Linux), `cargo build --release --target x86_64-pc-windows-gnu -p icu-client` (Windows). Release-Profil: `lto = "fat"`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.
- Prüfung im CI: `x86_64-w64-mingw32-objdump -p icu.exe | grep "DLL Name"` darf nur System-DLLs zeigen; `ldd icu` (Linux) listet keine GTK/Qt.
- Release: `icu-<ver>-windows-x86_64.zip` (exe + THIRD-PARTY-LICENSES + README), `icu-<ver>-linux-x86_64.tar.gz` (Binary + .desktop + Icon + Lizenzen). `cargo-about` erzeugt THIRD-PARTY-LICENSES; `cargo-deny` Allowlist wie beim Server plus BSD-2 (openh264), IJG (nur mit Feature `mjpeg`).

## 13. Abhängigkeiten (gepinnt)

| Crate | Version | Lizenz | Zweck |
| --- | --- | --- | --- |
| icu-proto | workspace | – | Wire |
| quinn / rustls (ring) / ring / rcgen / postcard / serde / bytes / tokio | wie Server | | |
| opus | 0.4.0 (opusic-sys 0.7.5, libopus 1.6.1 bundled) | MIT/Apache-2.0, BSD-3 | Codec |
| nnnoiseless | 0.5.2 | BSD-3 | Denoise/VAD |
| pipewire | 0.10.1 (Linux) | MIT | Audio Linux |
| windows | 0.62 (Features `Win32_Media_Audio`, `Win32_UI_Input`, `Win32_UI_WindowsAndMessaging`, `Win32_Graphics_Gdi`, `Win32_Security_Cryptography`) | MIT/Apache-2.0 | WASAPI, Raw Input, Layered Window, DPAPI |
| cpal | 0.18.2 | Apache-2.0 | Fallback-Host (Feature) |
| rubato | 5.0.0 | MIT/Apache-2.0 | Resampling nur bei Bedarf |
| ringbuf | 0.4 | MIT/Apache-2.0 | Audio-Ringpuffer |
| eframe / egui | 0.36.2 | MIT/Apache-2.0 | UI |
| tray-icon | 0.25.1 (Linux `ksni`) | MIT/Apache-2.0 | Tray |
| ashpd | 0.13.13 (`global_shortcuts`, tokio) | MIT | Portal |
| evdev | 0.13.2 (`tokio`) | Apache-2.0/MIT | evdev |
| smithay-client-toolkit / wayland-client / calloop | 0.21 / 0.31 / 0.14 | MIT | Overlay Wayland |
| nokhwa | 0.10.11 (`input-native`, ohne `decoding`, wenn YUYV direkt verarbeitet wird) | Apache-2.0 | Kamera |
| openh264 | 0.9.8 (`source`) | BSD-2 | H.264 |
| jpeg-encoder / zune-jpeg | 0.7.1 / 0.5.x (Feature `mjpeg`) | MIT/Apache-2.0 + IJG / MIT/Apache-2.0/Zlib | Fallback-Codec |
| directories | 5 | MIT/Apache-2.0 | Pfade |
| toml, clap, tracing | wie Server | | |

## 14. Tests

| Ebene | Inhalt |
| --- | --- |
| Unit | Jitter-Buffer (Verlust vs. Pause, Re-Anchor, Zielanpassung, FEC-Pfad, Drift-Korrektur) mit synthetischen Ankunftsfolgen; Gate-Automat (PTT, Release-Delay, END); VAD-Hangover; Mixer/Limiter; Header-Encode; Settings-Roundtrip; Hotkey-Bindungs-Parser. |
| Integration (Loopback, ohne Hardware) | Fake-`AudioBackend` mit synthetischem Signal: Capture → Encode → in-process Server (aus `icu-server` als Lib) → zweiter Client → Decode; Latenz aus Messpunkten, Erwartung ≤ 14 ms Software-Anteil bei 10 ms (Spike 3); Reconnect nach Server-Neustart; Enrollment end-to-end; `LossHint` setzt `PACKET_LOSS_PERC`; `KeyframeRequest` erzeugt IDR. |
| Hardware (manuell, Checkliste in `Client/TESTPLAN.md` bei M1 anlegen) | Linux KDE Wayland: Geräteliste, Hot-Plug, Portal-Bind-Dialog, evdev-Maustaste, Overlay über Vollbild-Spiel, Tray. Windows: WASAPI-Periode, Raw-Input bei Borderless-Spiel, Layered Window, SmartScreen-Dialog, Windows-N-Start ohne MF. Zwei Rechner: Mouth-to-Ear mit Klick-Messung, WAN-Emulation `tc netem`. |

## 15. Milestones

1. **M1 Voice-Kern (Linux)**: Enrollment + Verbindung, PipeWire-Backend, Opus-Pipeline, Jitter-Buffer, Mixer, minimales egui-Fenster mit Teilnehmerliste und Mute/Deafen, evdev-PTT. Ziel: zwei Linux-Clients sprechen über den Server, Latenzmessung im LAN.
2. **M2 Polish**: Portal-Hotkey, Settings-Dialog vollständig, Pegel/VAD, Cues, Tray, Reconnect, URI-Handler, Theme.
3. **M3 Windows**: WASAPI-Backend, Raw Input, DPAPI, URI-Handler, Cross-Build und Test auf echter Hardware.
4. **M4 Kamera + Overlay**: Capture, H.264, Video-Relay, Wayland-Overlay, Windows-Overlay, Bearbeitungsmodus.
5. **M5 Release**: Packaging beider Plattformen, THIRD-PARTY-LICENSES, Testplan abgearbeitet, `Research/verification.md` um Windows-Messungen ergänzt.

Reihenfolge ist bewusst: der Voice-Kern auf Chris' eigenem Desktop zuerst, weil dort alles messbar ist; Windows danach, weil es echte Hardware braucht; Kamera/Overlay zuletzt, weil optional.
