# ICU Client — Implementation Details

Stand 2026-09-20. Normative Grundlage: `../PROTOCOL.md`. Featureliste: `../Research/feature-freeze.md`. Entscheidungen und Belege in `../Research/`.

## 1. Zweck und Grenzen

Ein Fenster, ein Tray-Icon, ein optionales Overlay. Der Client verbindet sich mit genau einem Server, sendet das eigene Mikrofon per PTT oder VAD, mischt die Stimmen der anderen lokal, zeigt wer spricht, und blendet auf Wunsch Kamerakacheln über ein Borderless-Window-Spiel. Keine Channels, kein Chat, keine Zertifikatsverwaltung im UI.

## 2. Artefakte

| Plattform | Binary | Build | Laufzeit |
| --- | --- | --- | --- |
| Windows 10/11 x86_64 | `icu.exe`, portable, unsigniert (ZIP + SHA256SUMS) | Cross von Fedora, Target `x86_64-pc-windows-gnu`, Rezept `Research/cross-compile.md`, statisches libstdc++; **ohne Feature `aec`**, solange `Research/aec.md` A1 offen ist | nur System-DLLs, darunter `mf.dll`, `mfplat.dll`, `mfreadwrite.dll` (MediaFoundation, von nokhwa). Diese werden über `windows` 0.62 per `raw-dylib` **statisch importiert** und beim Prozessstart aufgelöst; ein Delay-Load-Pfad existiert weder in nokhwa noch im mingw-gnu-Build. Folge: **Windows-N/KN ohne Media Feature Pack wird in v1 nicht unterstützt** (das Binary startet dort nicht). |
| Linux x86_64 | `icu`, glibc-dynamisch, Tarball; AppImage optional | nativ auf alter Baseline; Build-Host braucht PipeWire-Header und clang (pipewire-sys nutzt pkg-config + bindgen) sowie meson, ninja-build und abseil-Header für `webrtc-audio-processing` (§12) | Wayland/GL per dlopen. Hart gelinkte C-Bibliotheken: **`libpipewire-0.3`** (pipewire-rs linkt sie als `DT_NEEDED`, ein dlopen-Pfad ist in pipewire-rs nicht vorgesehen) und ALSA-lib nur, wenn das cpal-Fallback-Feature gebaut wird. |

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
  pipewire.rs      # Linux: pipewire-rs 0.10 (StreamBox/PropertiesBox), node.latency 240/48000, node.group;
                   #   DRITTER Stream: AEC-Referenz, Capture mit stream.capture.sink = true auf dem
                   #   aktiven Sink (Monitor-Ports = Systemmix inkl. Spielton), selbe node.group
  wasapi.rs        # Windows: IAudioClient3 shared event-driven, Fallback IAudioClient 10 ms;
                   #   AEC-Referenz offen (Research/aec.md A1)
  cpal_fallback.rs # Notfall-Host (ALSA/andere), hinter Feature-Flag; ohne AEC-Referenz → AEC aus
  capture.rs       # Blöcke à 480 Samples → dsp → encoder
  playback.rs      # Mixer: Summe der Speaker-Puffer, Limiter, Ausgabe
dsp/
  aec.rs           # webrtc-audio-processing AEC3, 480-Sample-Frames, Referenz- und Capture-Pfad,
                   #   get_stats()-Selbstdiagnose; hinter Feature `aec`, zur Laufzeit schaltbar
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
  nokhwa.rs        # V4L2 / MSMF (statischer MF-Import, kein Lazy-Load), 320x240@15 MJPEG/YUYV
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
| AEC-Referenz-Capture (Backend-Callback, RT; Linux: dritter PipeWire-Stream auf dem Sink-Monitor) | Systemmix-Samples in eigenen Ringpuffer | 5–10 ms |
| DSP/Encode (eigener Thread, hohe Priorität) | 480-Sample-Blöcke: **AEC** (erst `process_render_frame` mit dem Referenzblock, dann `process_capture_frame` mit dem Mikrofonblock) → denoise → VAD/PTT-Gate → Opus (20 ms = 2 Blöcke) → Datagram | 10 ms |
| Netz (tokio, 2 Worker) | QUIC, Control, Datagram-Empfang → Jitter-Buffer | ereignisgetrieben |
| Audio-Playback (Backend-Callback, RT) | pro Callback: aus jedem Jitter-Buffer nächsten Frame holen (Decode/PLC/FEC), summieren, Limiter | 5–10 ms |
| Kamera + H.264 (eigener Thread) | Capture → Encode → Uni-Stream | 15 fps |
| Video-Decode (eigener Thread) | Uni-Streams → Decoder je Speaker → Overlay-Kanal | ereignisgetrieben |
| Overlay (eigener Thread mit eigener Wayland-Verbindung bzw. Win32-Message-Loop) | Komposition, Fenster | 15 fps bzw. bei Änderung |
| Hotkey (Portal: tokio-Stream; evdev: Blocking-Thread; Windows: Message-Only-Window-Thread) | PttEvent → Gate | ereignisgetrieben |
| UI (eframe-Hauptthread) | Rendering, Repaint bei State-Änderung, Pegel 30 Hz | ≤ 60 Hz |

Kommunikation: `tokio::sync::watch<AppState>` für UI/Tray/Overlay (nur lesen), `mpsc` für Kommandos (Mute, Deafen, Connect, SetDevice, OverlayToggle), lock-freie Ringpuffer (`ringbuf`) zwischen Audio-Callbacks und DSP/Playback. Kein Mutex im Audio-Callback.

**AEC im DSP-Thread.** Das APM läuft vollständig im DSP/Encode-Thread, nie in einem Backend-Callback: es kostet gemessen 47 µs je 10-ms-Frame (0,47 % eines Kerns, Spike 6), was im Callback zwar passte, aber nichts im Callback verloren hat. Der Thread zieht je 10-ms-Tick **genau einen** Block aus jedem der beiden Capture-Ringpuffer. Der Referenz-Ringpuffer ist der kritische Teil: läuft er leer (Referenz-Stream noch nicht gestartet, Sink gewechselt, XRun), wird ein Block Stille eingespeist und ein Zähler erhöht; überschreitet der Zähler in 10 s die Schwelle 5, gilt die Referenz als abgerissen und der `Processor` wird per `reinitialize()` zurückgesetzt, statt mit verschobener Zeitachse weiterzulaufen. Begründung: AEC3 braucht keine Delay-Angabe, aber eine lückenlose und reihenfolgetreue Zeitachse — ein Delay-Sprung kostet rund 1 s Rekonvergenz (Spike 6 §5).

## 5. Verbindung und Identität

1. **Erststart / Invite**: `net::enroll::InviteLink` akzeptiert drei Formen (aus Argument, URI-Handler oder demselben Eingabefeld im Enroll-Fenster, alle drei laufen durch dieselbe `parse()`/`enroll()`-Funktion):
   - `icu://host:port/enroll?t=…&ca=…&n=…` — unverändert wie bisher. CSR mit CN = `n` aus dem Link.
   - `ICU1-…` (`icu_proto::invite_blob::InviteBlob`, `PROTOCOL.md` §2.3.1) — von einem Admin über `CreateInviteRequest` gemintet (§10), enthält Host/Port/volles CA-Zertifikat/verschlüsseltes Token bereits vollständig, kein separater `ca=`-Abruf nötig. Anzeigename kommt aus einem zusätzlichen Namensfeld im Enroll-Fenster (der Blob trägt keinen eigenen).
   - bloßes `host[:port]`, kein Token — **Bootstrap**: nur nutzbar, solange der Server noch keinen einzigen Nutzer hat; die erste erfolgreiche Anmeldung wird automatisch Admin (`PROTOCOL.md` §2.3). Auch hier kommt der Anzeigename aus dem Namensfeld.

   Keypair P-256 (`rcgen::KeyPair::generate`), CSR mit CN = Name aus der jeweiligen Quelle, Enrollment-Verbindung mit ALPN `icu-enroll/1` und einem von drei `PinVerifier`-Modi: `Hash` (SHA-256 des letzten Kettenglieds = `ca=`, wie bisher, `PROTOCOL.md` §2.3), `FullDer` (byteweiser Vergleich gegen `InviteBlob.ca_der`), `TrustFirst` (akzeptiert jede Kettenlänge 2 ungeprüft — reines Trust-on-first-use, nur für den Bootstrap-Fall, wo es noch keinen Prior geben kann). In allen drei Fällen genau Kettenlänge 2, Signaturprüfung an den CryptoProvider delegiert. Antwort speichern: `identity/key.pk8` (0600; Windows zusätzlich DPAPI User-Scope), `identity/leaf.der`, `identity/ca.der`, `identity/server.toml` (host, port, server_name) — das ist die **einzige** Ablage der Serverkoordinaten, `settings.toml` hat keinen `[server]`-Abschnitt.
2. **Normalstart**: Identität laden → Verbindung ALPN `icu/1` mit Client-Cert → `Hello{minor:1, caps:0, client:"icu/0.1.0 linux", initial:flags, name}` → `Welcome` → Zustand `Active`. Vorher kein Audio, keine Anzeige „verbunden“ (Spike 2: `connect()` ist kein Erfolgskriterium). Aus `Welcome.you: UserInfo` übernimmt der Client die eigene `user_id` und den **serverseitig gespeicherten** Namen — auch wenn der von `Hello.name` abweicht, weil der Server `Hello.name` nur beim allerersten Verbinden annimmt. `Welcome.min_client` wird in SemVer-Ordnung gegen `CARGO_PKG_VERSION` verglichen. `Welcome.is_admin` landet in `AppState.is_admin` und schaltet die Einladungs-Erzeugung im Settings-Dialog frei (§10).
3. **Reconnect**: Backoff 1, 2, 4, 8, 15 s, unbegrenzt; UI-Zustand „verbinde…“; eigener Zustand (Mute/Deafen/Lautstärken) bleibt. Jedes `Welcome` ersetzt dagegen den **vollständigen Peer-Zustand**: Jitter-Buffer, Decoder, Video-Kacheln und alle Session-Zuordnungen werden verworfen, weil Session-IDs nur innerhalb einer Verbindung gültig sind. Persistente Daten je Peer hängen an `user_id`. Nach `Welcome` Audio ohne Neustart der Backends.
4. **Renewal**: ab 30 Tage vor Ablauf einmal pro Start `RenewRequest`; neues Leaf ersetzt das alte atomar.
5. URI-Handler: Windows `HKCU\Software\Classes\icu` (ohne Admin), Linux `.desktop` mit `MimeType=x-scheme-handler/icu` + `xdg-mime default`. Beides beim ersten Start, abschaltbar. **Laut `auth-enrollment.md` offen**: die Doku beschreibt nur HKCR, und die mime-apps-Spezifikation war nicht abrufbar — beide Wege sind in M2 (Linux) bzw. M3 (Windows) zu belegen.
6. **Laufende Verbindung**: `Ping` alle 5 s (RTT im Debug-Fenster, `Pong.server_time_ms` = Unix-Millisekunden). Jede lokale Änderung von mute/deafen/camera_on sendet genau ein `SetState`, begrenzt auf 20/s; bit2 (`speaking`) sendet der Client immer als 0, weil der Server es autoritativ ableitet. Namensänderung im Settings-Dialog sendet `SetName`; `Error(NAME_TAKEN)` zeigt der Client als Banner am Eingabefeld und behält den alten Namen. `UserRenamed` aktualisiert die Liste.

## 6. Audio-Pipeline

| Stufe | Festlegung | Quelle |
| --- | --- | --- |
| Backend Linux | pipewire-rs 0.10.1 gegen die **0.10-API**: `StreamBox::new(&core, name, PropertiesBox)` (kein `Stream::new` mehr, das gab es zuletzt in 0.8; `Properties` heißt jetzt `PropertiesBox`), `add_local_listener_with_user_data().process(..).register()`, `stream.connect(Direction::Input, Option<u32>, StreamFlags::AUTOCONNECT \| MAP_BUFFERS \| RT_PROCESS, &mut [&Pod])`. Capture- und Playback-Stream in einer `node.group`, `node.latency = 240/48000`, `node.rate = 1/48000`, `media.role = Communication`, Format F32 mono 48 kHz; Geräteliste über `Core::get_registry()` + `registry.add_listener_local().global(..)` (`Audio/Source`, `Audio/Sink`), Hot-Plug über dieselben Registry-Events, `target.object` neu setzen. **Dritter Stream für die AEC-Referenz**: Capture-Stream mit `stream.capture.sink = true` (`PW_KEY_STREAM_CAPTURE_SINK`, „Try to capture the sink output instead of source output") und `target.object` = aktiver Sink → dessen Monitor-Ports, also der **komplette Systemmix inklusive Spielton**; selbe `node.group`, gleiches Format, gleiche `node.latency`. Damit hängen Mikrofon und Referenz am selben Graph-Clock, Clock-Drift zwischen beiden ist konstruktionsbedingt 0. Sink-Wechsel setzt `target.object` neu und löst ein `reinitialize()` des APM aus | `audio-io.md`, `aec.md`, Spike 3 (cpal ignoriert Fixed-Buffer); **pipewire-rs selbst ist ungebaut — Nachweis in M0**, `stream.capture.sink` praktisch ungeprüft (`aec.md` A4) |
| Backend Windows | windows-rs `IAudioClient3::InitializeSharedAudioStream`, `PeriodInFrames` = kleinstes Vielfaches der Fundamental-Periode ≥ max(Min, 240); Capture `AudioCategory_Other` + `RAW` wenn unterstützt, Render `AudioCategory_GameChat`; Fallback `IAudioClient::Initialize` shared 10 ms mit `AUTOCONVERTPCM`; Default-Wechsel über `IMMNotificationClient` (laut `audio-io.md` **unbelegt**, in M3 auf echter Hardware zu belegen). **AEC-Pfad offen**, siehe `Research/aec.md` A1; Kandidaten: `build.rs`-Patch mit `meson --cross-file`, nativer Windows-Build, `IAcousticEchoCancellationControl` (ab Win 11 22H2). Bis zur Entscheidung ist das Feature `aec` auf Windows aus | `audio-io.md`, `aec.md` |
| Resampling | nur wenn Mix-Format ≠ 48 kHz (`rubato::Fft` fest) | |
| **AEC** | `webrtc-audio-processing` ~2.1 (`bundled`), `Processor::new(48_000)`, `Config { echo_canceller: Some(EchoCanceller::Full { stream_delay_ms: None }), high_pass_filter: None, noise_suppression: None, gain_controller: None, .. }`. Frames **480 Samples = 10 ms** (vom APM erzwungen, sonst Panic), non-interleaved, f32 in **±1.0** — danach für `nnnoiseless` auf ±32768 skalieren. Je Tick erst `process_render_frame` (Referenz), dann `process_capture_frame` (Mikrofon); Reihenfolge ist zwingend, weil das Echo in Capture-Frame *k* aus Render-Frames ≤ *k* stammt. **Kein `set_stream_delay_ms`**: AEC3 schätzt das Delay selbst und ignoriert den Wert (`use_external_delay_estimator = false` im Upstream-Default; sechs Werte von 0–200 ms ergaben identische ERLE). **Kein AGC** (Dynamik bleibt unangetastet, `audio-io.md`). Bei Mute/Deafen zusätzlich `set_output_will_be_muted(true)` | `aec.md`, Spike 6: 36,8 dB ERLE, 8,96 ms, 47 µs/Frame |
| Denoise | nnnoiseless 0.5, 480 Samples, Default an; ob der erste Ausgabeblock verworfen werden muss, ist offen (`audio-io.md` I4, RNNoise-Impulsantwort nicht gemessen) — in M1 messen und hier festschreiben | Spike 3: 1,15 ms/Frame |
| VAD | RNNoise-Wahrscheinlichkeit (Schwelle Default 0,5, im Pegelmeter sichtbar) oder Energie-Gate in dBFS; Hangover 200 ms | |
| PTT-Gate | Press → sofort senden; Release → Release-Delay 150 ms (konfigurierbar) → letztes Paket mit `END`; Doppeldruck-Halten optional. Das Gate liegt **hinter** AEC und Denoise: beide laufen im PTT-Modus durchgehend weiter, auch bei losgelassener Taste. Grund: der adaptive Filter lernt den Echopfad genau in den Sprechpausen — ein tastengesteuertes AEC wäre bei jedem Burst unkonvergiert (nach einer Störung braucht AEC3 rund 1 s, Spike 6 §5), und `process_render_frame` muss ohnehin lückenlos laufen, sonst reißt die Zeitachse der Referenz ab | `feature-freeze.md`, `aec.md` |
| Encoder | opus 0.4.0 (opusic-sys 0.7.5, libopus 1.6.1 bundled): `Encoder::new(48000, Channels::Mono, Application::Voip)`, `set_signal(Signal::Voice)`, `set_bitrate(Bitrate::Bits(48_000))`, `set_vbr(true)` + `set_vbr_constraint(true)`, `set_inband_fec(true)`, `set_packet_loss_perc(i32)` aus `LossHint` (**i32**, nicht u32), `set_dtx(false)`, `set_complexity(10)`, Frame 20 ms (Option 10 ms). Der Cross-Build mit opusic-sys statt audiopus_sys ist laut `codec.md` C1 noch zu belegen (Spike 1 lief mit opus 0.3) | `codec.md` |
| Sender | `seq` +1 je Paket, `ts` +960 (bzw. 480), `speaker = 0`; `send_datagram` (nicht `_wait`) | `PROTOCOL.md §3` |
| Empfang | pro Speaker ein Jitter-Buffer + Decoder; Frame-Dauer je Paket über `Decoder::get_nb_samples(&packet)` (ein Äquivalent zu `OPUS_GET_LAST_PACKET_DURATION` gibt es in der Safe-API nicht) für Jitter-Arithmetik und `frame_size` bei PLC/FEC; `seq`/`ts`-Vergleiche modulo nach `PROTOCOL.md` §3; `LossReport` alle 2 s bei Änderung ≥ 10 ‰; `LossHint{0}` setzt `PACKET_LOSS_PERC` auf 0 zurück | |
| Jitter-Buffer | Histogramm der relativen Ankunftsverzögerung (5-ms-Buckets, Forget 0,983), Ziel = 95 %-Quantil + 1 Frame, Klemme [Untergrenze, 200 ms] mit Untergrenze = 1 Frame solange `PACKET_LOSS_PERC > 0` (FEC braucht Paket n+1), sonst 0,25 Frame (5 ms), Start 2 Frames; Re-Anchor je Talk-Spurt (`ts`-Sprung bei lückenloser `seq`); Anpassung in Sprechpausen; Verlust → FEC-Decode aus n+1 wenn vorhanden, sonst PLC; > 100 ms Verlust → stumm; Drift-Korrektur um 1 Frame in Pausen | `latency-budget.md`, Spike 3 (ohne Playout-Ziel läuft Rückstand auf) |
| Mixer | Summe f32 × Lautstärke je User (0–200 %, 0 = lokal stumm) → Soft-Limiter → Playback; `deafened` → Playback stumm und Mute | |
| Cues | Join/Leave (Default an), Sende-Cue (Default aus), Mute-Cue (sichtbar; Ton optional) | |
| Messpunkte | Zeitstempel an Capture-Callback, Encode fertig, Send, Receive, Decode, Playback-Callback; Histogramm über Debug-Fenster exportierbar | `latency-budget.md` |

**Speaking-Indicator, zwei Pfade.** Für Sprecher, deren Audio tatsächlich ankommt, leitet der Client den Indicator lokal aus der Datagram-Ankunft ab — das ist der schnelle Pfad und der einzige, der das Zielwert-Budget hält. Für alle anderen Fälle (eigener `deafened`-Zustand, Peer auf Lautstärke 0, kein Audio-Empfang) ist `UserState.speaking` aus dem Control-Kanal die Quelle; der Server leitet es autoritativ aus dem Datagram-Fluss ab (`PROTOCOL.md` §2.1). Der Client sendet `speaking` nie selbst.

**AEC-Schalter und Latenzbudget.** Im Settings-Dialog gibt es genau einen Schalter „Echo-Unterdrückung", **Default an**. Keine automatische Headset-Erkennung: eine verlässliche Klassifikation „Headset vs. Lautsprecher" existiert auf keiner Plattform, und die naheliegende Heuristik „aus, wenn Input und Output dasselbe Gerät sind" trifft ausgerechnet den häufigsten Echo-Fall falsch (Laptop-Mikrofon plus Laptop-Lautsprecher sind dieselbe Soundkarte). Stattdessen misst der Client: zeigt `echo_return_loss` über 30 s Sprechzeit keinen Echopfad, erscheint ein einmaliger, wegklickbarer Hinweis „Kein Echo feststellbar — Echo-Unterdrückung kann ausgeschaltet werden, das spart 9 ms Latenz". Zeigt umgekehrt `echo_return_loss` einen Echopfad, während `echo_return_loss_enhancement` über 10 s unter 6 dB bleibt, erscheint „Echo erkannt, wird aber nicht entfernt — Referenzgerät prüfen" (der Fall, in dem die Referenz nur den eigenen Playback statt des Systemmix sieht: 7,7 dB statt 38,8 dB, Spike 6 §8). Ein Tooltip nennt die Nebenwirkung: bei aktiver Echo-Unterdrückung verschwindet auch der eigene Spielton aus dem gesendeten Signal — genau so gewollt.

Zielwerte: Mouth-to-Ear LAN ≤ 60 ms (20-ms-Frames) / ≤ 45 ms (10 ms) **ohne AEC**, mit aktivem AEC **≤ 69 bzw. ≤ 54 ms** — das AEC kostet gemessen 8,96 ms im Sendepfad und 0 ms im Empfangspfad (`process_render_frame` verändert den Frame nicht). Das ist die „best“-Spalte aus `latency-budget.md`, typisch sind 90 bzw. 70 ms (mit AEC 99 bzw. 79 ms). Speaking-Indicator ≤ 1 Frame + 1 UI-Frame nach erstem Paket (lokaler Pfad); PTT-Druck → erstes Datagram ≤ 1 Frame. Ziel-ERLE: **≥ 30 dB** eingeschwungen im Single-Talk, **≥ 25 dB** in den Near-End-Pausen bei Double-Talk, **≥ 30 dB nach 10 min Dauerbetrieb bei 100 ppm Gerätedrift**.

## 7. Hotkeys

| OS | Backend | Details |
| --- | --- | --- |
| Linux | Portal GlobalShortcuts (ashpd 0.13.13, Feature `global_shortcuts`) primär. API 0.13: `GlobalShortcuts::new()`, `create_session(CreateSessionOptions::default())`, `bind_shortcuts(&session, &[NewShortcut::new(id, desc).preferred_trigger(..)], None, BindShortcutsOptions::default())` (vier Argumente), Ergebnis über das synchrone `Request::response()`; Signale über `receive_activated()` / `receive_deactivated()` → Press/Release. `Cancelled` oder Fehler → Backend nicht verfügbar (Spike 4a, allerdings mit ashpd 0.11.1 und ohne bestätigten Dialog — der interaktive Nachweis steht aus und gehört in M2). | Anzeige des aktiven Backends in den Settings; Dialog-Hinweis beim ersten Binden |
| Linux opt-in | evdev 0.13.2 ohne `grab()`: `Device::open` + `into_event_stream()` (Feature `tokio`), alle lesbaren `/dev/input/event*`, Tasten über `KeyCode` (in 0.13 heißt der Typ `KeyCode`, nicht `Key`) inkl. `KeyCode::BTN_SIDE`/`KeyCode::BTN_EXTRA`; Hotplug per inotify auf `/dev/input`. Aktivierung nur nach Nutzerwahl, mit Erklärung zu Gruppe `input` (Keylogger-Aspekt). Spike 4a: auf Chris' Host ohne Konfiguration lesbar. | Einzige Option für Maustasten unter Wayland |
| Windows | Raw Input: verstecktes Message-Only-Window in eigenem Thread, `RegisterRawInputDevices` Keyboard (0x01/0x06) + Mouse (0x01/0x02) mit `RIDEV_INPUTSINK`; `RI_KEY_MAKE/BREAK`, `RI_MOUSE_BUTTON_4/5_DOWN/UP`. Kein Schlucken der Taste. Elevated Spiele im Fokus: Eingaben unsichtbar (UIPI), UI-Hinweis. | `hotkey-wayland.md` |

Bindung: Aufnahme-Modus in den Settings („Taste drücken“), Speicherung als Backend-neutrale Beschreibung (Keycode/Keysym/Maustaste + Modifier). Zweite Bindung für Mute-Toggle, dritte für Deafen-Toggle, vierte für PTT/VAD-Umschalten (alle optional).

## 8. Kamera und Video

- Capture hinter `CameraSource`; v1 nokhwa 0.10.11 mit `input-native` und **eingeschaltetem** Default-Feature `decoding` (Spike 5 dekodiert MJPEG über `Buffer::decode_image::<RgbFormat>()`; ohne `decoding` gibt es dafür keinen Dekoder). `Camera::new(CameraIndex::Index(0), RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(..)))`. Ein Lazy-Load des Windows-Backends ist **nicht möglich** (`windows` 0.62 importiert MF per `raw-dylib`); Windows-N ist damit in v1 nicht unterstützt, siehe §2. Linux: Capture-Node explizit (Spike 5: Metadaten-Node überspringen). Format 320×240 @ 15 fps, MJPEG oder YUYV vom Gerät.
- Encoder openh264 0.9.8, Feature `source` (ist Default): `EncoderConfig::new().bitrate(BitRate::from_bps(tier.bitrate())).max_frame_rate(FrameRate::from_hz(15.0)).usage_type(UsageType::CameraVideoRealTime).rate_control_mode(RateControlMode::Bitrate)`, dann `Encoder::with_api_config(OpenH264API::from_source(), cfg)` (ein `Encoder::with_config` gibt es nicht). Encode über `encode(&impl YUVSource)` mit `YUVBuffer::from_rgb_source(..)`; IDR erzwingen per `force_intra_frame()`, sonst alle 2 s; SPS/PPS vor jedem IDR. 150 kbit/s bei 15 fps (`VideoTier::T0`) ist der v1-Zielwert, kein Messwert — Spike 5 kam lichtbedingt nur auf 3,7–6 fps, die Bitrate bei 15 fps ist laut `camera-video.md` unbelegt und in M4 nachzumessen. Frame → Uni-Stream mit Header (`PROTOCOL.md §4`). Bei Flow-Control-Stau (50 ms `open_uni`-Timeout): Frame verwerfen, nicht puffern.
- **Netzwerk-adaptive Qualität** (neu, 2026-09-21): `crate::netcond::NetworkTracker` sampelt alle 2 s `quinn::Connection::stats()` (`path.{rtt,min_rtt,cwnd,congestion_events,lost_packets,sent_packets}`, bislang in diesem Repo ungenutzt) und `Connection::datagram_send_buffer_space()` und faltet sie mit `LossHint`/`VideoCongestionHint` (`PROTOCOL.md` §2.2, neue Variante 16) zu einer `NetworkCondition`. `crate::quality::QualityController` fährt daraus eine Qualitäts-Ladder für `VideoTier` (`crate::video::VideoTier`): T0 150 kbit/s @ 15 fps → T1 100 kbit/s @ 7,5 fps → T2 60 kbit/s @ 5 fps → T3 40 kbit/s @ 3 fps → T4 pausiert. Abstieg 1–2 bestätigende Ticks, Aufstieg 6+ (mit Backoff bei Flapping) — bewusst asymmetrisch, damit ein zu früher Aufstieg nicht dieselbe Stauung reproduziert, die Audio-Datagramme (drop-old-favor-new, `Research/transport.md`) gefährden würde. **Audio-Bitrate bleibt von dieser Ladder unberührt** (fest 48 kbit/s, `dsp::opus`); die Wire-Kosten-Rechnung in `Research/codec.md` zeigt, dass ein einzelner Video-Tier-Schritt mehr Bandbreite spart als das gesamte erreichbare Opus-Bitratenfenster. `camera::run_sender` liest den Tier einmal pro Iteration (`CameraLink::video_tier()`, ein `AtomicU8` auf `Shared`) und baut den `Encoder` bei Tier-Wechsel neu (openh264s sicheres Rust-Binding hat keinen Laufzeit-Bitraten-Setter); ein frisch gebauter Encoder liefert ohnehin ein IDR als erstes Bild. `T4` (pausiert) encodiert und sendet gar nichts, ohne Audio zu berühren. Kein Nutzer-Schalter (Analogie zum Jitter-Puffer: automatisch, unsichtbar), aber ein kleines Statuslabel neben der RTT-Anzeige ("Video reduziert"/"Video pausiert (Netz)"), weil eine Degradation für die Gegenseite sichtbar ist.
- Decoder je Speaker über `Decoder::new()` und `decode(&[u8]) -> Result<Option<DecodedYUV>>` (das `Option` bedeutet „noch kein vollständiges Bild“, nicht Fehler); erster Frame ohne IDR → `KeyframeRequest`; ≥ 3 s ohne Frame → Kachel ausblenden.
- Vorschau im Settings-Dialog vor dem Einschalten. Kamera an/aus per Button und Hotkey; `camera_on` im `SetState`.
- Feature `mjpeg` für den Fallback-Codec (jpeg-encoder/zune-jpeg), Default aus. In v1 ist das ein reiner **Empfangs- und Debug-Pfad**: `codec = 2` darf nicht gesendet werden, weil es dafür keine Aushandlung gibt und Default-Clients es nicht decodieren (`PROTOCOL.md` §4).

## 9. Overlay

Eigener Thread, eigene Compositor-Verbindung, kein winit. Inhalt: Kacheln der Teilnehmer mit Kamera (und optional nur sprechende ohne Kamera als Namensschild), Speaking-Rahmen, Name; CPU-Komposition in premultiplied BGRA (≤ 4 × 320×240 @ 15 fps).

| OS | Fenster | Belegt |
| --- | --- | --- |
| Linux Wayland | smithay-client-toolkit 0.21.1 Layer-Shell: `LayerShell::bind(&globals, &qh)`, `create_layer_surface(&qh, surface, Layer::Overlay, Some("icu-overlay"), None)`, `set_keyboard_interactivity(KeyboardInteractivity::None)`, **`set_exclusive_zone(-1)`** (so im Spike belegt; damit ignoriert die Surface fremde Exclusive-Zonen und bleibt an der gewählten Ecke), Anker als Bitflags (`Anchor::TOP \| Anchor::RIGHT`) + Margin, leere Input-Region über `compositor::Region::new(..)` und `set_input_region(Some(region.wl_region()))`, Buffer aus `shm::slot::SlotPool` mit `wl_shm::Format::Argb8888`. `commit()` und `set_input_region()` kommen aus dem Trait `shell::WaylandSurface`, der importiert sein muss. Event-Loop calloop mit Timer (Spike 4b: `blocking_dispatch` allein blockiert) — dafür muss das SCTK-Feature `calloop` aktiviert sein, es ist optional. GNOME: Layer-Shell fehlt → Overlay „nicht verfügbar“. | Spike 4b PASS auf KWin 6.7.5, allerdings mit SCTK 0.19.2; der Sprung auf 0.21 ist ungetestet (in M4 zu belegen) |
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

Regeln: keine modalen Fehlerdialoge (Banner im Fenster), alle Settings sofort wirksam, Fenster schließen = Tray, Doppelklick Tray = Fenster. Settings-Bereiche: Audio (Geräte, Pegel, Verstärkung, Modus PTT/VAD, Schwelle, Hotkeys, Release-Delay, Cues, Rauschunterdrückung, **Echo-Unterdrückung**, Loopback-Test), Kamera (Gerät, Vorschau), Overlay (an/aus, Ecke, Offset, Größe), Allgemein (Anzeigename, Autostart, URI-Handler, Backend-Anzeige, **Einladung erzeugen** — nur sichtbar wenn `AppState.is_admin`: Namensfeld + Button sendet `Command::CreateInvite`, Antwort erscheint als kopierbares `ICU1-…` im selben Bereich, Fehler `NOT_ADMIN` als Banner), Über (Version, Lizenzen inkl. OpenH264/Opus-Hinweise).

Tray: tray-icon 0.25 (Linux `default-features = false, features = ["ksni"]`), Icon-Varianten verbunden/getrennt/mute/deafen, Menü Mute/Deafen/Fenster/Beenden.

## 11. Konfiguration

`settings.toml` unter `$XDG_CONFIG_HOME/icu/` bzw. `%APPDATA%\icu\`:

```toml
# Serverkoordinaten stehen ausschließlich in identity/server.toml (siehe §5), nicht hier.
[audio]       input_device, output_device, mode = "ptt"|"vad", vad_threshold, gain_db, denoise = true,
              aec = true, release_ms = 150, frame_ms = 20, cue_join = true, cue_tx = false, cue_mute = true
[hotkeys]     ptt = {...}, mute = {...}, deafen = {...}, toggle_mode = {...}, backend = "auto"|"portal"|"evdev"
[camera]      device, enabled = false
[overlay]     enabled = false, corner = "top-right", offset = [40, 40], size = "small"
[volumes]     "<user_id>" = 1.2                     # pro Peer, persistent; Schlüssel ist die stabile
                                                    # user_id aus UserInfo — Session-IDs gelten nur je
                                                    # Verbindung, Fingerprints sieht der Client nie
[ui]          name, start_minimized = false, register_uri_handler = true
```

## 12. Build und Packaging

- Workspace-Root `.cargo/config.toml` und `build/win-static/libstdc++.a` nach `Research/cross-compile.md` (verifiziertes Rezept). Einschränkung: Spike 1 hat den Cross-Build mit eframe 0.33, `windows` 0.52/0.53, cpal 0.16, opus 0.3 und rcgen 0.13 belegt, **nicht** mit den hier gepinnten Versionen — der Nachweis ist mit dem echten Dependency-Satz in M3 zu wiederholen.
- **Build-Voraussetzungen für das Feature `aec`** (`webrtc-audio-processing` mit `bundled`): `meson`, `ninja-build`, `clang`, `pkg-config` und abseil-Header (Fedora: `abseil-cpp-devel`). Die gebündelten WebRTC-Quellen liegen im Crate-Tarball, ein Netzzugriff ist damit nicht nötig, solange abseil aus dem System kommt — fehlt es, lädt meson es als Subproject nach.
- **Workaround, Upstream-Issue prüfen**: `webrtc-audio-processing-sys/build.rs` präfixt die Symbole der statischen Bibliothek per objcopy und bestimmt den Pfad dazu **hart** als `<rustc-sysroot>/lib/rustlib/<HOST>/bin/rust-objcopy`, ohne Env-Variable zum Überschreiben. Fedoras `rust`-Paket liefert diese Komponente nicht (kein rustup, kein `llvm-tools`), der Build bricht mit `No such file or directory (os error 2)` ab. Bis das upstream behoben ist, braucht jeder Build-Host einmalig:

  ```sh
  sudo mkdir -p /usr/lib/rustlib/x86_64-unknown-linux-gnu/bin
  sudo ln -sfn /usr/bin/llvm-objcopy /usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-objcopy   # Paket: llvm
  ```

  Rückbau: `sudo rm /usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-objcopy`. GNU- und LLVM-objcopy beherrschen beide `--redefine-sym` und `@file`, der Symlink ist also inhaltlich korrekt und kein Hack am Ergebnis, nur an der Pfadsuche. Vor M1 ist zu prüfen, ob es dafür ein Upstream-Issue gibt (sonst eines aufmachen) — eine dauerhafte Änderung an `/usr` gehört nicht in eine Build-Anleitung.
- **Der Windows-Cross-Build kann `aec` derzeit nicht bauen**: `build.rs` ruft `meson setup` ohne `--cross-file`, die im Windows-Target erzeugte `libwebrtc-audio-processing-2.a` enthält ELF-x86-64-Objekte. Der Windows-Build läuft deshalb vorerst **ohne** Feature `aec`; die Entscheidung steht als A1 in `Research/aec.md`.
- `cargo build --release -p icu-client` (Linux), `cargo build --release --target x86_64-pc-windows-gnu -p icu-client` (Windows). Release-Profil: `lto = "fat"`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.
- Prüfung im CI: `x86_64-w64-mingw32-objdump -p icu.exe | grep "DLL Name"` darf nur System-DLLs zeigen; `ldd icu` (Linux) listet keine GTK/Qt.
- Release: `icu-<ver>-windows-x86_64.zip` (exe + THIRD-PARTY-LICENSES + README), `icu-<ver>-linux-x86_64.tar.gz` (Binary + .desktop + Icon + Lizenzen). THIRD-PARTY-LICENSES wird mit `cargo-about` erzeugt (Generator laut `packaging-licensing.md` Punkt 10 eigentlich noch offen, hier bewusst festgelegt). `cargo-deny` nutzt die **eine** Workspace-`deny.toml` aus `Server/IMPLEMENTATION.md` §9 — MIT, Apache-2.0, ISC, BSD-2-Clause, BSD-3-Clause, Zlib, Unicode-3.0 — plus `IJG` ausschließlich, wenn das Feature `mjpeg` gebaut wird. Die Lizenz von ICU selbst ist **TBD** (Empfehlung Apache-2.0); bis zur Entscheidung trägt der Workspace kein `license`-Feld.

## 13. Abhängigkeiten (gepinnt)

| Crate | Version | Lizenz | Zweck |
| --- | --- | --- | --- |
| icu-proto | workspace | – | Wire |
| quinn / rustls (ring) / ring / rcgen / x509-parser / postcard / serde / bytes / tokio | wie Server (quinn mit `default-features = false`, Features `runtime-tokio`, `rustls-ring`, `log`; rustls ohne Default-Provider; x509-parser 0.18) | | |
| opus | 0.4.0 (opusic-sys 0.7.5, libopus 1.6.1 bundled) | MIT/Apache-2.0, BSD-3 | Codec |
| nnnoiseless | 0.5.2 | BSD-3 | Denoise/VAD |
| webrtc-audio-processing | ~2.1 (2.1.0, Feature `bundled`; Tilde-Pin, weil Minor-Versionen laut Crate-README API-brechend sein dürfen) | BSD-3 (crates.io meldet „non-standard", `license-file = COPYING`; der Text ist wörtlich die Google-BSD-3-Clause) | AEC3. Build braucht meson, ninja-build, clang, pkg-config, abseil-Header; baut die gebündelten WebRTC-Quellen mit und präfixt die Symbole mit `v2_`. Hinter Feature `aec` (Default an auf Linux, aus auf Windows). Bringt `webrtc-audio-processing-config` ~2.1 für die `Config`-Typen mit. |
| pipewire | 0.10.1 (Linux) | MIT | Audio Linux. Build braucht PipeWire-Header + clang (pkg-config/bindgen), linkt `libpipewire-0.3` hart; **noch nie gebaut**, Nachweis in M0. |
| windows | 0.62.2 (Features `Win32_Media_Audio`, `Win32_UI_Input`, `Win32_UI_WindowsAndMessaging`, `Win32_Graphics_Gdi`, `Win32_Security_Cryptography`) | MIT/Apache-2.0 | WASAPI, Raw Input, Layered Window, DPAPI. `UpdateLayeredWindow` liegt in `Win32::UI::WindowsAndMessaging`, ist aber `#[cfg(feature = "Win32_Graphics_Gdi")]` — beide Features nötig. `RegisterRawInputDevices` liegt in `Win32::UI::Input` (nicht `…::KeyboardAndMouse`). DPAPI-Blob heißt `CRYPT_INTEGER_BLOB`, nicht `DATA_BLOB`. |
| cpal | 0.18.2 | Apache-2.0 | Fallback-Host (Feature) |
| rubato | 5.0.0 | MIT/Apache-2.0 | Resampling nur bei Bedarf |
| ringbuf | 0.5 | MIT/Apache-2.0 | Audio-Ringpuffer (0.4.8 wäre eine Major-Version zurück, 0.4.9 ist yanked) |
| eframe / egui | 0.36.2 | MIT/Apache-2.0 | UI |
| tray-icon | 0.25.1 (Linux `ksni`) | MIT/Apache-2.0 | Tray |
| ashpd | 0.13.13 (`global_shortcuts`, tokio) | MIT | Portal |
| evdev | 0.13.2 (`tokio`) | Apache-2.0/MIT | evdev |
| smithay-client-toolkit / wayland-client / calloop | 0.21.1 (Feature `calloop`) / 0.31 / 0.14 | MIT | Overlay Wayland. `calloop` ist in SCTK ein optionales Feature und muss explizit an, sonst fehlt die Event-Loop-Anbindung. |
| nokhwa | 0.10.11 (`input-native`, Default-Feature `decoding` **an**) | Apache-2.0 | Kamera |
| openh264 | 0.9.8 (`source`) | BSD-2 | H.264 |
| jpeg-encoder / zune-jpeg | 0.7.1 / 0.5.x (Feature `mjpeg`) | MIT/Apache-2.0 + IJG / MIT/Apache-2.0/Zlib | Fallback-Codec |
| directories | 6 | MIT/Apache-2.0 | Pfade (5.0.1 ist von 2023 und eine Major-Version zurück) |
| toml, clap, tracing | wie Server | | |

## 14. Tests

| Ebene | Inhalt |
| --- | --- |
| Unit | Jitter-Buffer (Verlust vs. Pause, Re-Anchor, Zielanpassung, FEC-Pfad, Drift-Korrektur) mit synthetischen Ankunftsfolgen; Gate-Automat (PTT, Release-Delay, END); VAD-Hangover; Mixer/Limiter; Header-Encode; Settings-Roundtrip; Hotkey-Bindungs-Parser. |
| Unit AEC (offline, ohne Hardware, Feature `aec`) | Der Messaufbau aus Spike 6 wird als Test übernommen (`Research/spikes/spike-aec/src/signal.rs` ist die Vorlage): synthetischer Echopfad — sprachähnliches Far-End (gefärbtes Rauschen, Formant-Resonatoren, Silbenhüllkurve), Direktschall nach 80 ms mit ERL ≈ 6 dB, 80 ms Nachhall-IR, Mikrofonrauschen −50 dBFS. Geprüft wird: (a) eingeschwungene **ERLE ≥ 30 dB** im Single-Talk; (b) ERLE ≥ 25 dB in den Near-End-Pausen bei Double-Talk; (c) algorithmische Verzögerung per Impuls = 430 Samples ± 1 Frame (Regressionsschutz gegen einen Versionswechsel, der das Latenzbudget verschiebt); (d) Skalen-Test: bekannter Sinus durch `APM → nnnoiseless`, Amplitude am Ende korrekt (±1.0 vs. ±32768 ist die wahrscheinlichste Fehlerquelle beim Zusammenbau); (e) Referenz = nur eigener Playback statt Systemmix ⇒ ERLE bricht ein (sichert die Verdrahtung des Referenz-Streams ab). Laufzeit ≈ 10 s Audio je Fall, deterministischer PRNG, keine Hardware. |
| Integration (Loopback, ohne Hardware) | Fake-`AudioBackend` mit synthetischem Signal: Capture → Encode → in-process Server (aus `icu-server` als Lib) → zweiter Client → Decode; Latenz aus Messpunkten, Erwartung ≤ 14 ms Software-Anteil bei 10 ms (Spike 3); Reconnect nach Server-Neustart; Enrollment end-to-end; `LossHint` setzt `PACKET_LOSS_PERC`; `KeyframeRequest` erzeugt IDR. |
| Hardware (manuell, Checkliste in `Client/TESTPLAN.md` bei M1 anlegen) | Linux KDE Wayland: Geräteliste, Hot-Plug, Portal-Bind-Dialog, evdev-Maustaste, Overlay über Vollbild-Spiel, Tray. Windows: WASAPI-Periode, Raw-Input bei Borderless-Spiel, Layered Window, SmartScreen-Dialog, Windows-N-Start ohne MF. Zwei Rechner: Mouth-to-Ear mit Klick-Messung, WAN-Emulation `tc netem`. |

## 15. Milestones

0. **M0 pipewire-rs-Spike (vor allem anderen)**: `pipewire` 0.10.1 gegen PipeWire 1.4.11 bauen (Header + clang auf dem Build-Host) und gegen die 0.10-API einen Capture- und einen Playback-Stream in einer `node.group` mit `node.latency = 240/48000` öffnen, Geräteliste aus der Registry ziehen, Roundtrip-Latenz messen (`audio-io.md` I1/I2). Grund: die Spec war ursprünglich gegen die 0.8-API geschrieben, pipewire-rs wurde nie gebaut, und das Backend trägt den gesamten Linux-Voice-Kern. Ergebnis in `Research/verification.md` nachtragen und §6 „Backend Linux“ danach festschreiben.
1. **M1 Voice-Kern (Linux)**: Enrollment + Verbindung, PipeWire-Backend **inklusive AEC-Referenz-Stream (`stream.capture.sink`) und AEC3-Stufe**, Opus-Pipeline, Jitter-Buffer, Mixer, minimales egui-Fenster mit Teilnehmerliste und Mute/Deafen, evdev-PTT. Ziel: zwei Linux-Clients sprechen über den Server, Latenzmessung im LAN. AEC gehört zum Voice-Kern und nicht in einen späteren Polish-Schritt: es sitzt in der Capture-Kette vor allem anderen, verschiebt das Latenzbudget um 9 ms und bestimmt die Struktur des Backends (dritter Stream, zweiter Ringpuffer). Abnahme zusätzlich: akustische Messung mit echtem Lautsprecher, ERLE ≥ 30 dB (`aec.md` A2), sowie A4 (`stream.capture.sink` praktisch) und A5 (Drift Mikrofon↔Monitor).
2. **M2 Polish**: Portal-Hotkey (inkl. des ausstehenden interaktiven Bind-Tests aus Spike 4a) und Linux-URI-Handler belegen, Settings-Dialog vollständig, Pegel/VAD, Cues, Tray, Reconnect, URI-Handler, Theme.
3. **M3 Windows**: WASAPI-Backend, Raw Input, DPAPI, URI-Handler, Cross-Build und Test auf echter Hardware. **Zusätzlich die offene AEC-Entscheidung (`aec.md` A1)**: `build.rs`-Patch mit `meson --cross-file` plus abseil für mingw, nativer Windows-Build-Host, oder OS-AEC über `IAcousticEchoCancellationControl` (ab Win 11 22H2) mit WASAPI-Loopback als Referenz. Bis dahin baut Windows ohne Feature `aec`. Mitzumessen: A6 (Drift zwischen Loopback- und Mikrofon-Endpoint in ppm — ab ~500 ppm versagt AEC3, siehe `aec.md`).
4. **M4 Kamera + Overlay**: Capture, H.264, Video-Relay, Wayland-Overlay, Windows-Overlay, Bearbeitungsmodus.
5. **M5 Release**: Packaging beider Plattformen, THIRD-PARTY-LICENSES, Testplan abgearbeitet, `Research/verification.md` um Windows-Messungen ergänzt.

Reihenfolge ist bewusst: der Voice-Kern auf Chris' eigenem Desktop zuerst, weil dort alles messbar ist; Windows danach, weil es echte Hardware braucht; Kamera/Overlay zuletzt, weil optional.

## 16. Revidiert in Implementierung (2026-09-20, M0/M1/M2/M4-Linux)

Abweichungen des gebauten Clients (`crates/icu-client`) von den Abschnitten oben. Jede Zeile ist eine bewusste Entscheidung beim Bauen; die Begründung steht dabei.

| Abschnitt | Spezifikation | Implementiert | Grund |
| --- | --- | --- | --- |
| §6 Backend Linux | `node.latency = 240/48000` als feste Periode | Backend fordert 240 an, erhält aber das Graph-Quantum (128 Samples = 2,67 ms auf diesem Host, Spike 8). Die Engine sammelt Blöcke beliebiger Größe in Ringpuffern (`ringbuf` 0.5) zu 480er-DSP-Blöcken. Feature `v0_3_49` für `Buffer::requested()`. | `node.latency` ist in PipeWire eine Obergrenze, kein Zwang; Latenz bleibt ≤ 5 ms je Richtung. |
| §6 AEC-Referenz | `target.object` = aktiver Sink, Sink-Wechsel per Registry-Event nachziehen | Referenzstream mit `stream.capture.sink = true` **ohne** `target.object`; PipeWire hängt ihn an den Default-Sink und folgt Default-Wechseln selbst. | Weniger Code, gleicher Effekt; Spike 8 zeigt den Sink-Mix (A4 bestätigt). Sink-Wechsel zur Laufzeit ist ungetestet. |
| §4 Threads | Playback-Callback holt je Callback aus jedem Jitter-Buffer | Eigener Thread `icu-mixer` decodiert/mischt 480er-Blöcke und hält den Playback-Ringpuffer auf 2 Blöcken (≤ 20 ms Vorlauf); der Callback liest nur den Ringpuffer (`try_lock` auf einen unbestrittenen Mutex, nie blockierend). | Kein Decoder/Mutex im RT-Callback (§4-Regel), Decoder-Laufzeit im Callback vermieden; Preis ≤ 10–20 ms Playout-Vorlauf, im Debug-Stats-Feld `jitter delay` sichtbar. |
| §4 Datagram-Sender | eigener Netz-Task | Der DSP-Thread ruft `Connection::send_datagram` direkt (synchron, nicht blockierend) über `Mutex<Option<Connection>>`. | Ein Thread-Hop weniger im Sendepfad. |
| §6 Jitter-Buffer | Ziel = 95-%-Quantil + 1 Frame, Drift-Korrektur um 1 Frame in Pausen | Histogramm/Quantil/Klemme wie spezifiziert, Anpassung nur an Spurt-Grenzen; **Drift-Korrektur nicht implementiert** (Spurts enden nach `END` oder 100 ms Verlust ohnehin mit Re-Anchor). | Im Live-Test irrelevant; nachziehen, wenn lange Dauersprecher Drift zeigen. |
| §6 Denoise | „erster Ausgabeblock verwerfen offen“ | Der erste RNNoise-Ausgabeblock wird durch Stille ersetzt (Filter-Warm-up). | Skalentest zeigt korrekte Amplitude ab Block 2. |
| §5 URI-Handler | beim ersten Start, abschaltbar | `.desktop` + `xdg-mime default` bei jedem GUI-Start, wenn `[ui] register_uri_handler = true` (Default) und nicht `--no-uri-handler`; Datei wird nur geschrieben, wenn sich der Inhalt ändert. | Idempotent; Windows (HKCU) ist M3-Stub. |
| §5 Renewal | `RenewRequest` ab 30 Tage vor Ablauf | **Nicht implementiert.** | Leaf gilt 365 Tage; kommt vor dem ersten Ablauf. |
| §7 Hotkeys | Aufnahme-Modus „Taste drücken“ | Textfelder für evdev-Name (`KEY_F12`, `BTN_SIDE`, `code:<n>`) und Portal-Trigger; Änderungen gelten ab Neustart. Portal zuerst (60-s-Timeout für den KDE-Dialog), bei Fehler/`Cancelled` evdev; evdev-Hotplug per Rescan alle 3 s statt inotify. | Kein interaktiver Test möglich; Aufnahme-Modus ist Polish. |
| §8 Kamera | Vorschau im Settings-Dialog | Vorschau im Hauptfenster unter der Teilnehmerliste, solange die Kamera an ist; Geräteauswahl als Index-Textfeld, `synthetic` = Testbild (auch `--camera synthetic` für Tests). | Weniger UI-Code; nokhwa braucht Feature `camera-sync-impl`, damit `Camera` `Send` ist. |
| §9 Overlay | Kacheln mit Name, Speaking-Rahmen; Bearbeitungsmodus mit Drag | Kacheln nur für Teilnehmer mit `camera_on` (Kamerabild oder dunkle Kachel), Rahmen blau wenn spricht, **keine Namen** (kein Textrenderer im Overlay-Thread), **kein Drag-Bearbeitungsmodus** (Ecke/Offset/Größe in den Einstellungen). SCTK 0.21 nutzt `delegate_dispatch2!` statt der einzelnen `delegate_*`-Makros; calloop-Timer 15 fps. | Zeit; Layer-Surface auf KWin 6.7.5 erzeugt und konfiguriert (Log), Screenshot war wegen gesperrter Sitzung nicht aussagekräftig. |
| §10 UI | dekorationslos mit eigener Titelleiste, eingebettete Schrift, SVG-Icons | Normale Fensterdekoration, egui-Default-Fonts, Unicode-Symbole statt Icons; eframe 0.36 (`App::ui` statt `update`, `egui::Panel::top/bottom`), Renderer wgpu (0.36-Default), Feature `glow` optional. Fenster schließen = Tray, Shift+Schließen beendet. | Zeit; Verhalten (Banner statt Dialoge, Settings sofort wirksam) wie spezifiziert. |
| §11 Konfiguration | wie spezifiziert | Zusätzlich `[audio] hangover_ms`, `output_volume`; `[ui] theme`; Hotkey-Bindung `{ key, trigger }`. | Vom Settings-Dialog gebraucht. |
| §12 Build | `lto = "fat"` usw. | unverändert; `cargo-deny`/`cargo-about` sind auf dem Host nicht installiert, `deny.toml` wurde anhand `cargo metadata` erweitert (OFL-1.1, Ubuntu-font-1.0, Unlicense, BSL-1.0, MPL-2.0; IJG-Ausnahme für mozjpeg; Clarify für webrtc-audio-processing). | Nicht ausgeführt; im Bericht vermerkt. |
| §14 Tests | Loopback-Latenz ≤ 14 ms Software-Anteil | Gemessen wird `capture→send` und `jitter delay` in `--debug-stats`, nicht Ende-zu-Ende im Test; Korrelationstest A→B mit harmonischem Signal (Opus erhält Rauschsignale nicht wellenformtreu). | Latenz-Assertion wäre auf einem Debug-Build/CI nicht stabil. |
| §14 CLI | — | `icu [LINK] [--config-dir] [--headless] [--debug-stats] [--test-tone] [--fake-audio] [--server-addr] [--no-uri-handler] [--no-hotkeys] [--camera <idx|synthetic>]`. | Live-Test und Integrationstests. |
| §15 M1 | akustische ERLE-Messung (A2), A4/A5 praktisch | A4 in Spike 8 bestätigt (Monitor liefert Sink-Mix); A2 und A5 (Drift über 10 min) stehen aus — brauchen Lautsprecher/Mikrofon bzw. Zeit. | Unbeaufsichtigt nicht messbar. |
| §3, §8 Netzwerk-adaptive Qualität (2026-09-21) | in §3 nicht vorgesehen | Zwei neue, plattformunabhängige Module `netcond.rs` (`NetworkTracker`/`NetworkCondition`, quinn-Stats-Sampling) und `quality.rs` (`QualityController`, Bitraten-Ladder) statt eines Unterordners; beide immer kompiliert (nicht hinter Feature `camera`), weil `engine.rs`/`Shared` sie unabhängig vom Kamera-Feature halten. `PROTOCOL.md` §2.2 bekam Variante 16 `VideoCongestionHint` (S→C), Server-seitig in `crates/icu-server/src/relay.rs` (`VideoSender`-Zähler, kein neues Modul, keine `Room`-Erweiterung). `tc netem`-Messung steht aus (`PROTOCOL.md` §9, `Research/transport.md`). | Kleinerer Diff als ein neuer Unterordner; Begründung der Ladder-Werte und der Audio-Priorität in `Research/codec.md`/`Research/transport.md`. |
| §10 UI, 2026-Redesign (2026-09-21, Branch `ui/2026-polish`) | s.o. (2026-09-20-Zeile), inzwischen selbst veraltet | Vollständige Neufassung von `theme.rs` (Farbrollen wurden zu einer per-Theme `Tokens`-Struktur, `theme::tokens(&Visuals)`, statt fixer Konstanten — jede Rolle unterscheidet sich laut Vorgabe zwischen hell/dunkel; neuer WCAG-Kontrasttest), neue Datei `widgets.rs` (toggle, segmented, level_meter, avatar, speaking_bars, kbd, status_pill, banner, group_card, setting_row/slider_row), `main_window.rs` und `settings.rs` komplett auf dieses Vokabular umgestellt (Icon-Rail statt Tab-Leiste, Kopf mit Logo-Badge/Status-Pille/Zahnrad-Taste, Segment-Control für PTT/VAD anstelle der bisherigen versteckten Rechtsklick-Geste). Die 2026-09-20-Zeile oben („Unicode-Symbole statt Icons") war zu diesem Zeitpunkt schon falsch — Inter/Phosphor waren längst im Einsatz — und wird durch diese Zeile ersetzt. | Vorgabe des Nutzers (vollständige Client-UI-Neufassung); Details und offene Punkte in `crates/icu-client/src/ui/theme.rs`, `widgets.rs`. |
| §10 UI, Akzentfarbe (2026-09-21) | `tray.rs` **und** `overlay.rs` spiegeln `ACCENT` hart als `#2E9BFF`, bleibt so | Geprüft: nur `overlay.rs:334` (Speaking-Rahmen der Wayland-Overlay-Kachel) spiegelt den (dunklen) Akzentwert hart. `tray.rs`s Icon-Farben (`#9E9E9E`/`#F44336`/`#FF9800`/`#4CAF50`, Material-Design-Palette für getrennt/deafened/muted/verbunden) sind unabhängig vom Akzent und wurden **nicht** angefasst. Der alte, falsche Kommentar in `theme.rs`, der `tray.rs` mit einschloss, wurde korrigiert. | Vorgabe ging von einer Dopplung in `tray.rs` aus, die im Code nicht existiert; Code hat Vorrang, siehe Auftrag. |
| §10 UI, Einstellungen/Allgemein (2026-09-21) | Gruppe „Identität": Anzeigename, `name_error`, drei Wertzeilen Server/`user_id`/Zertifikat gültig bis | Nur Server und `user_id` als Wertzeilen — kein Feld in `AppState` oder `Identity` liefert ein Ablaufdatum des Leaf-Zertifikats (nur rohes `leaf_der: Vec<u8>`, ungeparst). Zeile ersatzlos weggelassen statt erfunden. Das bestehende, in der Vorgabe nicht erwähnte admin-only „Einladung erzeugen"-Feld (`state.is_admin`/`Command::CreateInvite`, aus dem 2026-09-21-Admin-Bootstrap-Commit) bleibt erhalten, nur in eine eigene `group_card` restyled. | „nichts wird erfunden"; bestehende Funktionalität nicht ersatzlos entfernen. |
| §10 UI, Histogramm-Export (2026-09-21) | keine Vorgabe zum Speicherort | `Engine` bekam ein `paths: Paths`-Feld + `pub fn paths(&self)`, damit `settings.rs`s „Histogramm exportieren"-Taste (About-Tab) nach `{config_dir}/histograms.txt` schreiben kann, statt das Verzeichnis lokal in `settings.rs` neu zu bestimmen (würde bei `--config-dir` vom laufenden Prozess abweichen) oder ein Downloads-Verzeichnis zu verwenden. Einzige Änderung dieses Redesigns außerhalb von `crates/icu-client/src/ui/`. Export enthält denselben formatierten Text wie `DebugStats::report()` (inkl. Histogramm-Quantile), keinen rohen Bucket-Dump — die Bucket-Zählung ist `stats.rs`-intern und wurde nicht exponiert. | Nutzer-Entscheidung (Rückfrage während der Umsetzung); kleinster Eingriff, der den echten Konfigurationspfad trifft. |
