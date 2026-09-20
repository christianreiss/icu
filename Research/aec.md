# Acoustic Echo Cancellation: Verfahren, Referenzsignal je OS, Pipeline-Position

## Frage

`audio-io.md` und `feature-freeze.md` haben AEC für v1 mit der „Headset-Annahme" ausgeschlossen. Diese Annahme ist gekippt: ICU muss für Lautsprecher-Nutzer eine **sehr gute** Echokompensation liefern, ohne die Latenzziele aus `latency-budget.md` (LAN ≤ 45 ms bei 10-ms-Frames, ≤ 60 ms bei 20 ms) und den Qualitätsanspruch „glasklar, unverfälscht" zu brechen.

Konkret: Welche AEC-Implementierung, mit welchem Referenzsignal je Betriebssystem, an welcher Stelle der Pipeline, mit welchem Delay-Handling, welchem Latenz- und CPU-Aufschlag, und was passiert, wenn die Bibliothek nicht baut? Zentral und produktentscheidend: **Welche Referenz sieht ein In-Process-AEC überhaupt?** Ein Voice-Tool neben einem Spiel muss den Spielton aus dem Mikrofon rechnen, nicht nur die eigene Wiedergabe.

## Kandidaten

| # | Kandidat | Kurzbeschreibung |
| --- | --- | --- |
| K1 | **WebRTC AEC3 in-process** über `webrtc-audio-processing` 2.1.0 (Feature `bundled`) | AudioProcessingModule (APM) im Client-Prozess; `process_render_frame` für die Far-End-Referenz, `process_capture_frame` für das Mikrofon; feste 10-ms-Frames |
| K2 | **Speex-AEC** (`speexdsp`, `speex_echo_*`, MDF) | Klassischer Multidelay-Block-Frequency-Domain-Filter; von Mumble bis heute als Option angeboten; Rust-Bindings: `speexdsp-sys` 0.1.2 (2018), `speexdsp` 0.1.2 (2022) |
| K3 | **PipeWire `module-echo-cancel`** (nur Linux) | Systemseitiges Modul, erzeugt virtuelle Source/Sink; Backend `aec/libspa-aec-webrtc` oder `aec/libspa-aec-null`; Referenz kommt aus dem Graph |
| K4 | **Windows OS-AEC** über `AudioCategory_Communications` + Signal-Processing-Mode „Communications" (APOs), optional gesteuert per `IAcousticEchoCancellationControl` | Das OS rechnet; die App liefert nur die Kategorie bzw. den Referenz-Endpoint |
| K5 | **Referenzquellen** (orthogonal zu K1/K2): eigener Playback-Mix vs. Systemmix per **WASAPI Loopback** (Windows) bzw. **Monitor-Source / `stream.capture.sink`** (Linux) | Entscheidet, ob der Spielton mitgerechnet wird |
| K6 | **Keine AEC** (Status quo, Headset-Annahme) | Fallback, wenn K1 nicht baut |

Ergänzende Teilfragen: Residual-Echo-Suppression, Comfort Noise, Reihenfolge AEC → NS (RNNoise), AEC im PTT-Modus, Delay-Schätzung und Clock-Drift, Zielwerte ERLE, Double-Talk-Verhalten.

## Primärquellen

Abrufdatum aller Quellen: 2026-09-20.

| # | Quelle | URL bzw. Pfad |
| --- | --- | --- |
| E1 | crates.io API: `webrtc-audio-processing` (Versionen, Lizenz) | https://crates.io/api/v1/crates/webrtc-audio-processing |
| E2 | `webrtc-audio-processing` 2.1.0 `README.md` (Feature-Flags, Build-Werkzeuge, dynamisches vs. gebündeltes Linken) | https://raw.githubusercontent.com/tonarino/webrtc-audio-processing/main/README.md , lokal im Crate-Tarball |
| E3 | `webrtc-audio-processing` 2.1.0 `src/lib.rs` (`Processor`, `process_render_frame`, `process_capture_frame`, `analyze_render_frame`, `num_samples_per_frame`, `set_config`, `get_stats`, Thread-Sicherheit) | Crate-Tarball `~/.cargo/registry/src/index.crates.io-*/webrtc-audio-processing-2.1.0/src/lib.rs` |
| E4 | `webrtc-audio-processing-config` 2.1.0 `src/lib.rs` (`Config`, `EchoCanceller::{Full,Mobile}`, `NoiseSuppression`, `GainController`, `HighPassFilter`) | Crate-Tarball `…/webrtc-audio-processing-config-2.1.0/src/lib.rs` |
| E5 | `webrtc-audio-processing-sys` 2.1.0 `build.rs` (meson/ninja-Aufruf, `determine_objcopy_path`, Symbol-Prefixing) | Crate-Tarball `…/webrtc-audio-processing-sys-2.1.0/build.rs` |
| E6 | `webrtc-audio-processing` 2.1.0 `COPYING` (Lizenztext) | Crate-Tarball `…/webrtc-audio-processing-2.1.0/COPYING` |
| E7 | `webrtc-audio-processing` 2.1.0 `examples/simple.rs` (Wertebereich der f32-Samples) | Crate-Tarball `…/webrtc-audio-processing-2.1.0/examples/simple.rs` |
| E8 | PulseAudio/WebRTC-Upstream (im Crate gebündelt), `webrtc/api/audio/echo_canceller3_config.h` — AEC3-Defaults `use_external_delay_estimator`, `filter.refined.length_blocks` | Crate-Tarball `…/webrtc-audio-processing-sys-2.1.0/webrtc-audio-processing/webrtc/api/audio/echo_canceller3_config.h` ; Upstream https://gitlab.freedesktop.org/pulseaudio/webrtc-audio-processing |
| E9 | Upstream `webrtc/modules/audio_processing/aec3/block_processor.cc` (`config_.delay.use_external_delay_estimator`) und `aec3/aec3_common.h` (`kFftLengthBy2 = 64`, `kBlockSize`) | Crate-Tarball, s. o. |
| E10 | Upstream `webrtc/modules/audio_processing/audio_processing_impl.cc` (`set_stream_delay_ms`, `SetAudioBufferDelay`) | Crate-Tarball, s. o. |
| E11 | crates.io API: `speexdsp-sys`, `speexdsp` (Pflegezustand) | https://crates.io/api/v1/crates/speexdsp-sys , https://crates.io/api/v1/crates/speexdsp |
| E12 | Xiph speexdsp `include/speex/speex_echo.h` (API, Frame-Größe, Filterlänge, `speex_echo_capture`-Delay-Hinweis) | https://raw.githubusercontent.com/xiph/speexdsp/master/include/speex/speex_echo.h |
| E13 | PipeWire: `libpipewire-module-echo-cancel` (Blockdiagramm, Optionen, `monitor.mode`, Beispielkonfiguration) | https://docs.pipewire.org/page_module_echo_cancel.html |
| E14 | PipeWire: `pipewire-props(7)` (`node.latency`, `target.object`) | https://docs.pipewire.org/page_man_pipewire-props_7.html |
| E15 | PipeWire: `pipewire/keys.h` (`PW_KEY_STREAM_CAPTURE_SINK = "stream.capture.sink"`) | lokal `/usr/include/pipewire-0.3/pipewire/keys.h` (Paket `pipewire-devel` 1.4.11); Upstream https://gitlab.freedesktop.org/pipewire/pipewire |
| E16 | Microsoft Learn: `AUDIO_STREAM_CATEGORY` (Communications, GameChat) | https://learn.microsoft.com/en-us/windows/win32/api/audiosessiontypes/ne-audiosessiontypes-audio_stream_category |
| E17 | Microsoft Learn: Audio Signal Processing Modes (Communications-Modus, RAW) | https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/audio-signal-processing-modes |
| E18 | Microsoft Learn: `IAcousticEchoCancellationControl` | https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nn-audioclient-iacousticechocancellationcontrol |
| E19 | Microsoft Learn: `IAcousticEchoCancellationControl::SetEchoCancellationRenderEndpoint` | https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iacousticechocancellationcontrol-setechocancellationrenderendpoint |
| E20 | Microsoft Learn: Loopback Recording (`AUDCLNT_STREAMFLAGS_LOOPBACK`) | https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording |
| E21 | Microsoft Learn: Default Ducking Experience (Stream Attenuation) | https://learn.microsoft.com/en-us/windows/win32/coreaudio/stream-attenuation |
| E22 | Microsoft Learn: Disabling the Ducking Experience (`IAudioSessionControl2::SetDuckingPreference`) | https://learn.microsoft.com/en-us/windows/win32/coreaudio/disabling-the-ducking-experience |
| E23 | ITU-T G.168 „Digital network echo cancellers" (Landing-Seite und Summary) | https://www.itu.int/rec/T-REC-G.168-201504-I/en |
| E24 | Eigene Entscheidungen und Messwerte | `Research/audio-io.md`, `Research/latency-budget.md`, `Research/codec.md`, `Research/verification.md`, Spike 6 (`Research/spikes/spike-aec/`) |
| E25 | Host-Inventar (eigene Erhebung, read-only, 2026-09-20) | dieses Dokument, Abschnitt „Host-Befunde" |

Sekundär, nicht als Beleg verwendet: Mumble-Nutzerdoku (Echo-Cancellation-Modi „Mixed/Multichannel", bereits in `feature-freeze.md` zitiert).

## Befunde

### Host-Befunde (read-only erhoben, 2026-09-20)

| Prüfung | Ergebnis |
| --- | --- |
| `pactl list modules short \| grep -i echo` | **keine Treffer** — kein Echo-Cancel-Modul geladen |
| `pw-cli ls Node \| grep -i echo` | **keine Treffer** — keine Echo-Cancel-Nodes im Graph |
| `/usr/lib64/pipewire-0.3/libpipewire-module-echo-cancel.so` | **vorhanden** (94 512 B, aus `pipewire-libs-1.4.11-1.fc43`) |
| SPA-Backends `/usr/lib64/spa-0.2/aec/` | **beide vorhanden**: `libspa-aec-webrtc.so` (20 256 B) und `libspa-aec-null.so` (11 712 B), ebenfalls aus `pipewire-libs` |
| `ldd libspa-aec-webrtc.so` | linkt gegen `libwebrtc-audio-processing-1.so.3` → das PipeWire-Backend nutzt die **alte APM-Generation 1.3**, nicht die vom Crate gebündelte 2.1 |
| PipeWire-Version | 1.4.11 (`pipewire --version`, `pipewire-1.4.11-1.fc43`) |
| Fedora-Pakete `webrtc-audio-processing` | 1.3 (installiert, als Abhängigkeit von `pipewire-libs`) und `webrtc-audio-processing0.3` 0.3.1 verfügbar. **Kein Paket `webrtc-audio-processing-2`** — das Crate 2.x kann auf Fedora 43 nicht dynamisch gegen ein Systempaket linken, `bundled` ist Pflicht. |
| `speexdsp` | 1.2.1-9.fc43 installiert; `speexdsp-devel` war nicht installiert (für Spike 6b nachinstalliert) |

Konsequenz für K3: Das Modul ist auf diesem Host **einsatzbereit, aber nicht aktiv**. Das WebRTC-Backend dort ist APM 1.3; die Qualität der AEC3-Implementierung in 1.3 gegenüber 2.1 ist **unbelegt** (nicht vermessen, nicht dokumentiert verglichen).

### K1 — `webrtc-audio-processing` 2.1.0 (AEC3 in-process)

| Fakt | Wert | Quelle |
| --- | --- | --- |
| Version / Datum | 2.1.0, 2026-05-13; Vorgänger 2.0.0 seit 2026-02-10 | E1 |
| Lizenz | crates.io meldet „non-standard"; `license-file = "COPYING"`; der Text ist wörtlich die **BSD-3-Clause** von Google („Copyright (c) 2011, Google Inc.", drei Klauseln inkl. Namensklausel) | E6, E1 |
| Versionierung | „Major version of this Rust wrapper tracks the major version of the PulseAudio WebRTC AudioProcessing upstream"; „version `2.3` can introduce an API-breaking change over `2.2`" → **Tilde-Requirement `~2.1` pinnen** | E2 |
| Build-Werkzeuge (`bundled`) | „`clang` or `gcc`", „`pkg-config`", „`meson`", „`ninja-build`" | E2 |
| Bundled-Quellen im Crate enthalten? | **Ja.** Der veröffentlichte `.crate` enthält `webrtc-audio-processing/` samt `webrtc/`-Baum und `meson.build` (`version : '2.1'`), 5,3 MB entpackt. Der README-Satz über das Git-Submodul gilt nur fürs Repo, nicht fürs Crate. | E5, eigene Prüfung |
| abseil | Wird per `pkg-config` gesucht; ist es nicht da, zieht meson es als Subproject über `subprojects/abseil-cpp.wrap` (Download von github.com bzw. wrapdb.mesonbuild.com) → **ohne Netz und ohne System-abseil baut es vermutlich nicht** (Folgerung aus dem Inhalt der `.wrap`-Datei, nicht getestet — der Host hatte nach der Installation von `abseil-cpp-devel` beides) | E5, eigene Prüfung |
| Symbol-Prefixing | `bundled` präfixt alle Symbole mit `v2_` per `objcopy --redefine-sym` (3 827 Symbole), „so that multiple major versions … can be linked together" | E2, E5 |
| **Falle: objcopy-Pfad** | `build.rs::determine_objcopy_path()` baut **hart** `<rustc-sysroot>/lib/rustlib/<HOST>/bin/rust-objcopy` und bietet **keine Env-Variable** zum Überschreiben. Fedoras `rust`-Paket liefert diese Komponente nicht (kein `llvm-tools`, kein rustup) → Build bricht ab. | E5, Spike 6 |
| API | `Processor::new(sample_rate_hz)`; `process_render_frame(&mut frame)` (Far-End), `process_capture_frame(&mut frame)` (Mikrofon), `analyze_render_frame(&frame)` (nicht-modifizierend); `frame` ist **non-interleaved**: Iterator über Kanäle von `[f32]` | E3 |
| Frame-Größe | „Panics if the number of samples doesn't match 10ms @ `sample_rate_hz`"; `num_samples_per_frame() = sample_rate_hz / 100` → **bei 48 kHz exakt 480 Samples**, dieselbe Blockgröße wie RNNoise | E3 |
| Wertebereich | Beispiel nutzt f32 in **±1.0** (`cos()*0.4`) — nicht ±32768 wie bei `nnnoiseless` | E7 |
| Thread-Sicherheit | `Processor` ist `Send + Sync`, Methoden nehmen `&self`: „one can wrap this type in an `Arc` to share ownership between capture and render threads" | E3 |
| `set_stream_delay_ms` | Existiert nur indirekt über `EchoCanceller::Full { stream_delay_ms: Option<u16> }` bzw. `Mobile { stream_delay_ms: u16 }`. Full: „If None, we let the AEC processor try determining it." Mobile: „Mandatory for the Mobile echo canceller variant." Der Wert wird vor **jedem** `process_capture_frame` gesetzt. | E3, E4 |
| **Warum `stream_delay_ms` bei AEC3 wirkungslos ist** | Upstream: `bool has_delay_estimator = !config_.delay.use_external_delay_estimator;` und der extern gesetzte Delay wird nur über `render_buffer_->AlignFromExternalDelay()` verwendet. Default in `echo_canceller3_config.h`: **`use_external_delay_estimator = false`**. AEC3 schätzt das Delay also selbst und ignoriert den übergebenen Wert, solange man die (experimentelle) AEC3-Config nicht ändert. | E8, E9, E10 |
| AEC3-Filterlänge | `filter.refined.length_blocks = 13`, `kBlockSize = kFftLengthBy2 = 64` Samples bei 16 kHz Bandrate → **13 × 64 / 16000 ≈ 52 ms** linear modellierter Echoschwanz; längerer Nachhall bleibt dem Residual-Suppressor | E8, E9 |
| Erzwungener Hochpass | `EchoCanceller::enforce_high_pass_filtering` ist „hard-coded to true on Full, false on Mobile" — bei AEC3 läuft also immer ein Hochpass mit, unabhängig vom separaten `high_pass_filter`-Submodul | E4 |
| Statistik | `get_stats()` → `echo_return_loss` (ERL), `echo_return_loss_enhancement` (ERLE), `residual_echo_likelihood`, `residual_echo_likelihood_recent_max` (beide nur mit `bundled`), `delay_ms` (Momentanwert) | E3 |
| Weitere Hinweise ans APM | `set_output_will_be_muted(bool)`, `set_stream_key_pressed(bool)` | E3 |
| docs.rs | Build für dieses Crate ist fehlgeschlagen (in `audio-io.md` A42 vermerkt); Primärquelle ist daher der Crate-Quelltext selbst | `audio-io.md`, E3 |

### K2 — Speex-AEC (speexdsp, MDF)

| Fakt | Wert | Quelle |
| --- | --- | --- |
| Rust-Bindings | `speexdsp-sys` 0.1.2 — letzte Version **2018-08-03**; `speexdsp` 0.1.2 — letzte Version **2022-04-28**; beide MIT, beide aus `rust-av/speexdsp-rs`. Beide gelten nach dem Maßstab aus `audio-io.md` (dort wurde `webrtc-vad` mit „letzte Version 2019" verworfen) als **ungepflegt**. | E11 |
| API | `speex_echo_state_init(frame_size, filter_length)`, `speex_echo_cancellation(st, rec, play, out)`, `speex_echo_ctl(st, SPEEX_ECHO_SET_SAMPLING_RATE, &rate)`, `speex_echo_state_destroy(st)`; alles **`spx_int16_t`**, also int16, kein f32 | E12 |
| Frame-Größe | „Number of samples to process at one time (should correspond to 10-20 ms)" — frei wählbar, nicht auf 10 ms festgenagelt wie das APM | E12 |
| Filterlänge | „Number of samples of echo to cancel (should generally correspond to 100-500 ms)" — der Filter muss Delay **plus** Nachhall abdecken | E12 |
| Delay-Handling | `speex_echo_cancellation` fügt „no delay … to playback in this form" hinzu; die Alternative `speex_echo_capture`/`speex_echo_playback` nutzt einen internen Puffer, der „is delayed by two frames to account for the delay introduced by most soundcards (**but it could be off!**)" — es gibt **keine** Delay-Schätzung im Speex-AEC | E12 |
| Residual-Echo / Comfort Noise | Der Echo-Canceller allein ist ein rein linearer Filter ohne nichtlinearen Prozessor. Residual-Echo-Suppression liegt bei Speex im **Preprocessor** (`SPEEX_PREPROCESS_SET_ECHO_STATE`), nicht im Echo-Canceller. Im Spike wurde der Preprocessor nicht verwendet. | E12 |
| Qualität vs. WebRTC laut Doku/Papers | In den gelesenen Primärquellen steht **kein** Vergleich → **unbelegt**. Ersetzt durch die eigene Messung (Spike 6b, siehe „Verified against"). | — |
| Verfügbarkeit | `speexdsp` 1.2.1 ist auf Fedora 43 gepackt und auf dem Host installiert; für Windows gäbe es keinen Fedora-Cross-Pfad, wohl aber einen CMake-/autotools-Build aus Quelle (**ungetestet, unbelegt**) | E25 |

### K3 — PipeWire `module-echo-cancel`

Das Modul ist ausdrücklich für genau dieses Problem gebaut und dokumentiert seine Topologie:

> „A capture stream that captures audio from a microphone. A Sink that takes the signal containing the data that should be canceled out from the capture stream. This is where the application (video conference application) send the audio to … A playback stream that just passes the signal from the Sink to the speaker. … A Source that exposes the echo-canceled data captured from the capture stream." (E13)

| Punkt | Befund | Quelle |
| --- | --- | --- |
| Nodes | Virtuelle `echo-cancel-capture`-Source und `echo-cancel-playback`-Sink plus die zugehörigen Streams | E13 |
| Backend | `library.name = <str>`: „the echo cancellation library Currently supported: aec/libspa-aec-webrtc. **Leave unset to use the default method (aec/libspa-aec-webrtc).**" Auf dem Host liegt zusätzlich `libspa-aec-null.so` (Durchreiche, für Tests). | E13, E25 |
| **Referenzsignal im Default-Modus** | Die Referenz ist das, was die Anwendung **in die virtuelle Sink schickt** — also nur der eigene Voice-Playback. Der Spielton wird in diesem Modus **nicht** herausgerechnet, weil er an der virtuellen Sink vorbei direkt auf das echte Gerät geht. | E13 |
| **`monitor.mode`** | „Instead of making a sink, make a stream that captures from the monitor ports of the **default sink**." Erst damit ist die Referenz der **komplette Systemmix** inklusive Spielton. Die Annahme „Spielton wird herausgerechnet" gilt also **nur mit `monitor.mode = true`**. | E13 |
| Adressierung durch den Client | Über die Node-Namen aus der Konfiguration: `source.props = { node.name = "Echo Cancellation Source" }` und `sink.props = { node.name = "Echo Cancellation Sink" }`; der Client wählt sie per `target.object` („this can be a `node.name` or an `object.serial`"). | E13, E14 |
| Latenzkosten | Steuerbar über `node.latency` (im Beispiel `1024/48000` auskommentiert). Die zusätzliche algorithmische Verzögerung des Backends ist in der Modul-Doku **nicht beziffert** → **unbelegt**; sie entspricht der des darunterliegenden APM (auf diesem Host Generation 1.3). Hinzu kommen zwei zusätzliche Graph-Knoten in der Kette. | E13 |
| Konfiguration ohne Root | Die Beispielkonfiguration liegt laut Doku in `~/.config/pipewire/pipewire.conf.d/my-echo-cancel.conf` — also im Home des Nutzers, **kein Root nötig**. Sie wirkt allerdings erst nach einem Neustart des PipeWire-Dienstes. | E13 |
| `pw-cli load-module` zur Laufzeit als User | **Unbelegt und ungetestet.** Auf diesem Host wurde bewusst nichts in den laufenden Graph geladen (Prüfung war read-only). In der gelesenen Modul-Doku ist ausschließlich der Konfigurationsdatei-Weg beschrieben. | E13, E25 |
| Reichweite | Nur Linux/PipeWire. Für Windows gibt es kein Äquivalent. | E13 |

### K4 — Windows: OS-AEC, Kategorien und Ducking

| Punkt | Befund | Quelle |
| --- | --- | --- |
| Kategorie Communications | „Real-time communications, such as VOIP or chat"; mappt auf den Signal-Processing-Mode „Communications", in dem OEM-APOs AEC, NS, AGC und Beamforming liefern **können** | E16, E17 |
| Die App sieht den Modus nicht | Eine Anwendung kann nur RAW/non-RAW wählen, nicht welcher Modus oder welche Effekte laufen; im RAW-Modus gilt: „Raw capture streams must not include any time varying or adaptive processing, such as **echo control**, automatic gain control, or noise suppression." | E17 |
| **Ducking als Nebenwirkung** | „tagging a stream as Communications will let Windows know to pause background music" (E16). Die Systemreaktion ist die *Default Ducking Experience*: „On the **Communications** tab, the user can choose an attenuation level (**default value is 80%**), mute all non-communication streams, or disable the default stream attenuation experience." Der Fade dauert „a period of 1 second". | E16, E21 |
| **Wer das Ducking abschalten kann** | „Programmatically, a direct WASAPI client can opt out by using the session manager for the audio session that provides volume control for the **non-communication streams**." Der Weg ist `IAudioSessionControl2::SetDuckingPreference(TRUE)` auf der Session **der gedämpften Anwendung**. → **Das Spiel müsste sich abmelden, nicht ICU.** ICU kann das Ducking, das es selbst auslöst, nicht per API unterdrücken; nur der Nutzer kann es im Mmsys.cpl global abstellen. Außerdem: „even if a client opts out of ducking, it still receives ducking notifications" und „the opt-out change does not take effect until the stream is stopped and started again". | E22 |
| Kategorie GameChat | „Similar to `AudioCategory_Communications` except that `AudioCategory_GameChat` will **not attenuate** other streams" — aber nur als **Render**-Kategorie gültig; für Capture sind nur `Communications`, `Speech`, `Other` zulässig | E16 |
| **`IAcousticEchoCancellationControl`** (Windows Build 22621 = Win 11 22H2) | „Provides a mechanism for determining if an audio capture endpoint supports acoustic echo cancellation (AEC) and, if so, allows the client to **set the audio render endpoint that should be used as the reference stream**." Zu holen per `IAudioClient::GetService`; „If the call to GetService fails with **E_NOINTERFACE**, then the AEC effect on the endpoint (if supported), does not allow control over the loopback reference endpoint." | E18 |
| `SetEchoCancellationRenderEndpoint(endpointId)` | „Setting an invalid render device ID will fail with `E_INVALIDARG`. Setting the value to **NULL** will result in Windows using its own algorithm to pick the loopback reference device." | E19 |
| Bewertung K4 | Die OS-AEC ist **optional und OEM-abhängig** (E17), nicht inspizierbar, nur ab Win 11 22H2 steuerbar (E18), und der Preis für ihre Aktivierung ist eine Kategorie, die systemweit alles andere um bis zu 80 % dämpft, ohne dass ICU das abstellen kann (E21, E22). Für ein Tool, das *neben einem Spiel* läuft, ist das ein Ausschlusskriterium. | E16–E22 |
| WASAPI Loopback als Referenz | „WASAPI provides loopback mode **primarily to support acoustic echo cancellation (AEC)**." Vorgehen: `GetDefaultAudioEndpoint` mit `eRender` statt `eCapture`, `IAudioClient::Initialize` mit `AUDCLNT_STREAMFLAGS_LOOPBACK`. Einschränkungen: „A client can enable loopback mode only for a **shared-mode** stream … Exclusive-mode streams cannot operate in loopback mode." Event-getrieben: „In versions of Windows prior to **Windows 10 1703**, pull-mode capture client does not receive any events when a stream is initialized with event-driven buffering and is loopback-enabled … In Windows 10 versions 1703 and higher, event-driven loopback clients are supported." Inhalt: „WASAPI loopback by default contains the **mix of all audio being played**". | E20 |

### K5 — Die zentrale Frage: welche Referenz sieht ein In-Process-AEC?

Ein In-Process-AEC kennt per Konstruktion nur die Samples, die **die eigene Anwendung** in ihren Render-Stream schreibt: den Mix der anderen Gesprächsteilnehmer. Der Spielton läuft in einem fremden Prozess über einen eigenen Stream in die Audio-Engine und taucht dort nie auf.

Für einen Lautsprecher-Nutzer heißt das konkret: das Mikrofon nimmt **beides** auf — die Stimmen der anderen *und* den Spielton. Ein AEC mit unvollständiger Referenz kann nur den Teil entfernen, den es kennt; der Rest bleibt in voller Lautstärke im gesendeten Signal. Die Mitspieler hören dann das Spiel des Sprechers, jedes Mal wenn dieser die Taste drückt. Spike 6 §8 misst genau das: mit unvollständiger Referenz bleiben **7,7 dB** ERLE übrig, mit Systemmix-Referenz **38,8 dB** (Details unter „Verified against"). Das ist der Unterschied zwischen „funktioniert nicht" und „funktioniert".

Daraus folgt zwingend: **Loopback-Capture des Systemmix ist für ICU nicht optional, sondern die Voraussetzung dafür, dass AEC in einem Gaming-Kontext überhaupt etwas taugt.**

| OS | Referenzquelle | Beleg |
| --- | --- | --- |
| Linux | PipeWire-Capture-Stream mit `stream.capture.sink = true` („Try to capture the sink output instead of source output") auf dem aktiven Sink → dessen Monitor-Ports, also der Systemmix | E15 |
| Linux, Alternative | `module-echo-cancel` mit `monitor.mode = true` (nutzt „the monitor ports of the default sink") | E13 |
| Windows | WASAPI-Loopback auf dem Render-Endpoint, `AUDCLNT_STREAMFLAGS_LOOPBACK`, shared mode, event-getrieben ab Win 10 1703 | E20 |
| Windows, Alternative | OS-AEC mit `IAcousticEchoCancellationControl::SetEchoCancellationRenderEndpoint` — das OS macht das Loopback selbst | E18, E19 |

Nebenwirkung, die mitbedacht werden muss: Eine Systemmix-Referenz bedeutet, dass **auch der eigene Spielton** aus dem Mikrofonsignal verschwindet. Das ist genau das gewünschte Verhalten (die anderen sollen das Spiel des Sprechers nicht hören), aber es ist ein Verhaltensunterschied zu „AEC aus", der im UI benannt gehört.

### Clock-Drift

Die Systemmix-Referenz erkauft man sich mit einem zweiten Zeitproblem: Capture-Gerät und Render-Gerät können unterschiedliche Quarze haben.

| Punkt | Befund | Quelle |
| --- | --- | --- |
| Linux | Capture-Stream und Loopback-Stream hängen im selben PipeWire-Graph am selben Graph-Clock; `node.group` sorgt schon heute dafür, dass Capture und Playback im selben Quantum laufen (Entscheidung aus `audio-io.md`). Gerätedrift kompensiert der Graph per DLL. → **Drift zwischen Mikrofon und Referenz praktisch 0.** | E14, `audio-io.md` |
| Windows | Der Loopback-Stream kommt aus der Audio-Engine des Render-Endpoints, das Mikrofon von einem eigenen Endpoint mit eigenem Takt. Ein Drift von einigen zehn bis wenigen hundert ppm ist bei getrennten USB-/Onboard-Geräten normal (**Zahl unbelegt, Annahme**). | E20 |
| Toleranz gemessen | Ein 10-s-Lauf sagt hierzu **nichts Belastbares**: 100 ppm ergeben in 10 s nur 48 Samples Schlupf, weniger als ein AEC3-Block (64 Samples bei 16 kHz Bandrate = 192 bei 48 kHz) — es findet gar keine Neuausrichtung statt. Deshalb zusätzlich über 60–600 s gemessen (Spike 6 §6b). Ergebnis: **entscheidend ist die Driftrate, nicht der aufsummierte Schlupf.** 100 ppm über 600 s (2 880 Samples = 60 ms Schlupf) kosten 0,4 dB; 1 000 ppm über 60 s bei **identischem** Schlupf von 2 880 Samples brechen auf 8,0 dB ein. AEC3 richtet also laufend nach und folgt langsamem Drift beliebig lange, scheitert aber an schnellem. Die Grenze liegt zwischen 100 und 500 ppm. → **AEC3 verträgt realistischen Gerätedrift (Größenordnung einige zehn ppm) dauerhaft ohne Zusatzmaßnahme.** Speex bricht dagegen schon bei 100 ppm in 10 s um 10 dB ein (Spike 6b). | Spike 6 |

### Residual Echo, Comfort Noise, Double-Talk

| Punkt | Befund | Quelle |
| --- | --- | --- |
| Residual-Echo-Suppression | Bei AEC3 Teil des Moduls (nichtlinearer Suppressor hinter dem linearen Filter); `get_stats()` liefert `residual_echo_likelihood` als Messgröße. Bei Speex liegt sie außerhalb des Echo-Cancellers, im Preprocessor. | E3, E12 |
| Comfort Noise | Im APM gibt es keinen separat schaltbaren Comfort-Noise-Generator in der Crate-API; der Suppressor blendet stattdessen auf ein geschätztes Hintergrundniveau. Ein eigener CNG ist für ICU nicht nötig, weil RNNoise ohnehin nach dem AEC läuft und das Ausgangsrauschen prägt. **Kein Beleg für ein CNG-Flag in der gelesenen API → unbelegt.** | E3, E4 |
| Reihenfolge AEC → NS | Im APM ist die Reihenfolge fest: der Echo-Canceller läuft vor der Noise Suppression (belegt durch die Config-Struktur `analyze_linear_aec_output`, die der NS erlaubt, „the output of the **linear AEC** instead of the capture frame" zu analysieren — das setzt voraus, dass die NS *nach* dem AEC liegt). Diese Reihenfolge ist auch physikalisch die richtige: ein Denoiser vor dem AEC verändert das Mikrofonsignal nichtlinear und zerstört damit die lineare Beziehung zur Referenz, auf der der adaptive Filter beruht. | E4 |
| Double-Talk | AEC3 hat einen internen Double-Talk-Detektor, der die Filteradaption bei gleichzeitiger Nah-Sprache einfriert. Die Crate-API exponiert dafür keinen Schalter. Gemessen in Spike 6: in den Near-End-Pausen innerhalb des Double-Talk-Fensters bleiben 28–32 dB ERLE erhalten; die Nah-Sprache wird dabei hörbar beeinflusst (Details und Messvorbehalte unter „Verified against"). | E3, Spike 6 |
| ERLE-Zielwerte aus Normen | ITU-T G.168 ist die einschlägige Empfehlung für Echo-Canceller, schließt Akustik aber ausdrücklich aus: „does not cover acoustic echo cancellation" (Summary). Ein normativer ERLE-Zielwert für AEC ist damit aus den erreichbaren Quellen **nicht belegbar**; die Zielwerte unten stammen aus der eigenen Messung. | E23 |

### AEC und PTT

**Ja, das AEC muss auch im PTT-Modus durchgehend laufen — und zwar auf beiden Seiten der Verarbeitung.** Begründung:

1. Der adaptive Filter lernt den Echopfad **genau dann**, wenn Far-End-Signal da ist und der Nutzer *nicht* spricht — also in den PTT-Pausen. Wer den AEC nur bei gedrückter Taste rechnet, hat bei jedem Tastendruck einen ungelernten Filter und beginnt die Konvergenz von vorn. Spike 6 §1 zeigt die Konvergenz: Sekunde 0 bereits 37,5 dB, eingeschwungen nach ~2 s bei 39 dB. Nach einer Störung (Delay-Sprung, §5) fällt die ERLE auf 10,1 dB und ist erst nach ~1 s wieder bei 38,6 dB. Genau diese Sekunde wäre bei tastengesteuertem AEC jeder Sprech-Burst.
2. `process_render_frame` **muss** lückenlos aufgerufen werden, sonst reißt die Zeitachse der Referenz ab und die interne Delay-Schätzung verliert die Ausrichtung.
3. Der CPU-Preis dafür ist die dauerhafte Last von 0,47 % eines Kerns (gemessen) — nicht verhandlungsbedürftig.

Das PTT-Gate gehört also **hinter** den AEC: Capture → AEC (läuft immer) → RNNoise (läuft immer) → PTT/VAD-Gate (schaltet nur, was gesendet wird).

### Latenz- und CPU-Delta

| Größe | Wert | Quelle |
| --- | --- | --- |
| Algorithmische Verzögerung AEC3 im Sendepfad | **8,96 ms** (430 Samples, per Impuls gemessen; Spitzenamplitude 0,446 von 0,500) | Spike 6 §0 |
| Zusätzliche Verzögerung im Empfangs-/Wiedergabepfad | **0 ms** — `process_render_frame` verändert den Frame nicht (das Crate-Beispiel prüft genau das per `assert_eq!`) | E7, E3 |
| CPU (AEC allein, 48 kHz mono, 10-ms-Frames) | capture mean 43 µs / p99 61 µs, render mean 4 µs / p99 6 µs → **47 µs je 10 ms = 0,47 % eines Kerns** | Spike 6 §1 |
| CPU (AEC + HPF + WebRTC-NS) | 53 µs je 10 ms = 0,53 % eines Kerns | Spike 6 §7 |
| Vergleich Rest der Kette | RNNoise 1,15 ms je 10-ms-Frame = 11 % eines Kerns (Spike 3) — der AEC kostet also **rund ein Zwanzigstel** dessen, was der Denoiser schon kostet | `verification.md`, Spike 3 |

Wirkung auf das Budget aus `latency-budget.md` (Sendepfad, einfache Richtung):

| Szenario | bisher | mit AEC | neues Ziel |
| --- | --- | --- | --- |
| 10 ms LAN best | 35,6 ms | **44,6 ms** | ≤ 50 ms |
| 10 ms LAN typisch | 65,1 ms | **74,1 ms** | ≤ 80 ms |
| 20 ms LAN best | 45,6 ms | **54,6 ms** | ≤ 60 ms |
| 20 ms LAN typisch | 85,1 ms | **94,1 ms** | ≤ 100 ms |
| 10 ms WAN typisch | 123,1 ms | **132,1 ms** | ≤ 140 ms |
| 20 ms WAN typisch | 143,1 ms | **152,1 ms** | ≤ 160 ms |

Die 9 ms fallen **nur an, wenn AEC aktiv ist**. Für Headset-Nutzer mit abgeschaltetem AEC bleiben die alten Zielwerte gültig. Damit ist die Zielverfehlung kein Bruch, sondern ein bewusst bezahlter Preis für Lautsprecherbetrieb — die G.114-Planungsgrenze von 400 ms (`latency-budget.md` L11) bleibt in allen Spalten weit unterschritten.

### Windows-Cross-Build: harter Befund

Der Versuch, `webrtc-audio-processing` 2.1.0 mit `bundled` nach `x86_64-pc-windows-gnu` zu cross-compilieren, ist **fehlgeschlagen — und zwar strukturell, nicht durch eine fehlende Abhängigkeit.**

`build.rs` ruft `meson setup` **ohne Cross-File** auf (`meson.arg("setup").arg("--prefix")…`, kein `--cross-file`, E5). Die Folge: meson baut die WebRTC-Bibliothek für den **Host**. Die im Windows-Target-Verzeichnis erzeugte `libwebrtc-audio-processing-2.a` (54 MB) enthält nachweislich **ELF-x86-64-Objekte** (`file` auf ein extrahiertes `.o`: „ELF 64-bit LSB relocatable, x86-64"), keine PE/COFF-Objekte. Selbst wenn der unmittelbare Abbruch behoben würde — `x86_64-w64-mingw32-g++` findet beim Kompilieren von `src/wrapper.cpp` die abseil-Header nicht, weil der Include-Pfad auf das nie entpackte meson-Subproject `subprojects/abseil-cpp-20240722.0` zeigt —, würde der anschließende Link an den ELF-Objekten scheitern.

Das Crate unterstützt Cross-Compiling nach Windows in dieser Form also **nicht**. Für ICU heißt das: der Windows-Build von ICU braucht entweder einen Patch am `build.rs` (meson-Cross-File erzeugen, abseil für mingw bereitstellen) oder einen nativen Windows-Build-Host. Das ist eine **Abweichung vom Cross-Only-Rezept aus `cross-compile.md`** und muss vor der Implementierung entschieden werden.

## Entscheidung

| Punkt | Entscheidung | Begründung |
| --- | --- | --- |
| **Verfahren** | **In-Process WebRTC AEC3** über `webrtc-audio-processing = "~2.1"` mit Feature `bundled`, Konfiguration `EchoCanceller::Full { stream_delay_ms: None }`; alle anderen APM-Submodule aus (`high_pass_filter: None`, `noise_suppression: None`, `gain_controller: None`) | Einziger Kandidat, der auf beiden Plattformen dieselbe, von uns kontrollierte Kette liefert; gemessen 34–39 dB ERLE gegenüber 25–31 dB bei Speex, bei halbem CPU-Verbrauch und ohne Delay-Tuning (Spike 6/6b). AGC bleibt aus (Entscheidung aus `audio-io.md` unverändert: „glasklar" = unverfälscht). |
| **Pipeline-Position** | `Capture (48 kHz mono f32, 480er-Blöcke)` → **`APM::process_capture_frame`** → `nnnoiseless` → `VAD/PTT-Gate` → `Opus`. Parallel und lückenlos: `Referenzquelle (480er-Blöcke)` → **`APM::process_render_frame`**, im selben 10-ms-Takt und **vor** dem zugehörigen Capture-Frame | AEC muss vor jeder nichtlinearen Verarbeitung stehen, sonst verliert der adaptive Filter die lineare Beziehung zur Referenz (E4). Render-vor-Capture, weil das Echo im Capture-Frame *k* aus Render-Frames ≤ *k* stammt. |
| **Frame-Größe** | **10 ms / 480 Samples**, hart vorgegeben vom APM (`num_samples_per_frame() = rate/100`, sonst Panic, E3) | Deckt sich exakt mit der RNNoise-Blockgröße (480, `audio-io.md` A37). Bei 20-ms-Opus-Frames sind das **2 APM-Frames je Opus-Frame** — keine zusätzliche Pufferung nötig. |
| **Wertebereich** | f32 in **±1.0** ans APM, danach für `nnnoiseless` auf ±32768 skalieren | E7 vs. `audio-io.md` A37 — die beiden Bibliotheken erwarten unterschiedliche Skalen; das ist die wahrscheinlichste Fehlerquelle beim Zusammenbau und gehört in einen Test. |
| **Referenzsignal Linux** | **Zweiter PipeWire-Capture-Stream mit `stream.capture.sink = true`** auf dem aktiven Output-Sink (Monitor-Ports = Systemmix), in derselben `node.group` wie Capture und Playback, `node.latency = 240/48000`, Format F32 mono 48 kHz; Sink-Wechsel wird per Registry-Event nachgezogen | Liefert den **vollständigen** Systemmix inklusive Spielton (E15) und hängt am selben Graph-Clock wie das Mikrofon → kein Drift (E14). Braucht keine Systemkonfiguration, keinen Dienstneustart, kein Root. |
| **Referenzsignal Windows** | **WASAPI-Loopback-Capture** auf dem Render-Endpoint: `GetDefaultAudioEndpoint(eRender, …)`, `IAudioClient::Initialize` shared mit `AUDCLNT_STREAMFLAGS_LOOPBACK`, event-getrieben (ab Win 10 1703 ohne Dummy-Render-Stream). Dass der Loopback-Stream bei völliger Stille keine Daten liefert, ist für ICU **kein Problem**: der eigene Render-Stream ist ab dem Verbindungsaufbau dauerhaft offen und schreibt in Sprechpausen Nullen, der Endpoint spielt also immer — die Referenz bleibt lückenlos. | „WASAPI provides loopback mode primarily to support acoustic echo cancellation"; enthält „the mix of all audio being played" (E20). Drift gegenüber dem Mikrofon-Endpoint ist unkritisch, solange die Rate unter ~100 ppm bleibt: 100 ppm über 600 s kosten 0,4 dB (Spike 6 §6b). |
| **Capture-Kategorie Windows** | **Unverändert `AudioCategory_Other` + `AUDCLNT_STREAMOPTIONS_RAW`** (Entscheidung aus `audio-io.md` bleibt), Render weiter `AudioCategory_GameChat` | `Communications` würde alle anderen Streams um standardmäßig 80 % dämpfen (E21) — genau das Spiel, neben dem ICU laufen soll. ICU kann dieses Ducking **nicht** abstellen: `SetDuckingPreference` ist der Opt-out der *gedämpften* Anwendung, nicht der auslösenden (E22). RAW hält zusätzlich die OEM-APOs draußen, die unsere Kette doppeln würden (E17). |
| **Delay-Handling** | **`stream_delay_ms: None`**, also AEC3s eigene Delay-Schätzung. `set_stream_delay_ms` wird **nicht** verwendet. | AEC3 ignoriert den Wert bei Default-Konfiguration (`use_external_delay_estimator = false`, E8/E9) — empirisch bestätigt: 0/50/80/110/200 ms liefern identische ERLE (Spike 6 §3). Die Gegenprobe mit AECM zeigt, dass der Parameter grundsätzlich ankommt (dort kostet ein um 120 ms falscher Wert 17 dB) — die Wirkungslosigkeit ist also eine Eigenschaft von AEC3, kein Bindungsfehler. Die Delay-Schätzung deckt 40–400 ms Echopfad gleich gut ab (Spike 6 §4). |
| **Was statt Delay-Angabe wirklich zählt** | **Lückenlose, reihenfolgetreue Fütterung** beider Streams: je 10-ms-Tick genau ein `process_render_frame` und genau ein `process_capture_frame`, Render zuerst. Bei einem Aussetzer (XRun, Gerätewechsel) wird der `Processor` per `reinitialize()` zurückgesetzt statt mit verschobener Zeitachse weiterzulaufen. | Die Zeitachse der Referenz ist das Einzige, was AEC3 braucht; ein Delay-Sprung kostet ~1 s Rekonvergenz (Spike 6 §5). |
| **Reihenfolge AEC → NS** | **AEC (webrtc) → NS (nnnoiseless)**, die WebRTC-eigene NS bleibt aus | Reihenfolge ist im APM ohnehin fest (E4) und physikalisch zwingend. `nnnoiseless` bleibt der Denoiser, weil die Entscheidung in `audio-io.md` (pure Rust, BSD-3, 480er-Blöcke) davon unberührt ist. |
| **Residual-Echo-Reserve** | Die WebRTC-NS bleibt als **schaltbare Reserve** in der Konfiguration erhalten: wird per Telemetrie/`residual_echo_likelihood` oder per Nutzerbeschwerde hörbares Restecho festgestellt, kann `noise_suppression` im APM zugeschaltet werden (dann `nnnoiseless` aus) | Gemessen: AEC+HPF+NS hebt die ERLE von 34,3 auf 41,0 dB, kostet 6 µs mehr je Frame (Spike 6 §7). |
| **AEC im PTT-Modus** | **AEC läuft immer**, unabhängig von PTT, Mute und VAD. Das Gate liegt dahinter. Bei `Mute`/`Deafen` wird zusätzlich `set_output_will_be_muted(true)` gesetzt. | Siehe „AEC und PTT" oben: der Filter lernt in den Sprechpausen; ein tastengesteuerter AEC ist bei jedem Burst unkonvergiert. |
| **Zielwerte** | Eingeschwungene **ERLE ≥ 30 dB** im Single-Talk bei linearem Lautsprecher, **≥ 25 dB** bei deutlich verzerrendem Lautsprecher; **≥ 25 dB** in den Near-End-Pausen während Double-Talk; Konvergenz nach Delay-Sprung **≤ 1,5 s**; **≥ 30 dB nach 10 min Dauerbetrieb bei 100 ppm Gerätedrift** | Abgeleitet aus den gemessenen Werten mit Sicherheitsabstand (Spike 6). Der Drift-Zielwert ist bewusst als Rate über eine lange Dauer formuliert, weil §6b zeigt, dass die Rate entscheidet und ein kurzer Test die Frage gar nicht stellt. Kein normativer Wert belegbar: G.168 schließt Akustik aus (E23). |
| **Fallback, wenn die Bibliothek nicht baut** | Feature-Flag `aec` im Client. Ist es aus: AEC-Stufe entfällt ersatzlos, die UI zeigt „Echo-Unterdrückung auf dieser Installation nicht verfügbar", die alten Latenzziele gelten. **Zusätzlich auf Linux**: Hinweis auf `module-echo-cancel` mit `monitor.mode = true` als Systemlösung, samt fertiger Konfigurationsdatei für `~/.config/pipewire/pipewire.conf.d/` und Auswahl der Node `"Echo Cancellation Source"` per `target.object`. **Speex wird nicht als Fallback eingebaut.** | Ein zweiter, schlechterer AEC-Pfad wäre dauerhaft zu pflegender Code für ein Verfahren, das in der Messung durchweg unterlegen ist (Spike 6b). Die PipeWire-Variante kostet uns dagegen nur Dokumentation. |
| **Build-Voraussetzungen, die ins Build-Rezept müssen** | `meson`, `ninja-build`, `clang`, `pkg-config`, **abseil-Header** (Fedora: `abseil-cpp-devel`) und ein unter `<rustc-sysroot>/lib/rustlib/<HOST>/bin/rust-objcopy` auffindbares objcopy. Auf Fedora ohne rustup ist Letzteres ein **Symlink auf `/usr/bin/llvm-objcopy`** (Paket `llvm`). | E2, E5; empirisch erzwungen (Spike 6). `build.rs` bietet keine Env-Variable für den objcopy-Pfad — ohne diesen Schritt bricht jeder `bundled`-Build auf Fedora ab. |
| **Windows-Build** | **Offen und blockierend**: das Crate cross-compiliert nicht nach `x86_64-pc-windows-gnu` (meson ohne Cross-File, ELF-Objekte im Windows-Target). Zu entscheiden vor der Implementierung: (a) `build.rs`-Patch mit meson-Cross-File und mingw-abseil, (b) nativer Windows-Build-Host, (c) Windows-Variante über die OS-AEC mit `IAcousticEchoCancellationControl`. | Eigene Messung, E5; Abweichung vom Rezept in `cross-compile.md`. |
| **UI-Schalter** | Ein einfacher Schalter „Echo-Unterdrückung": **An (Default) / Aus**. **Keine** automatische Geräteerkennung. Dazu ein Hinweistext, dass bei aktiver Echo-Unterdrückung auch der eigene Spielton aus dem Mikrofonsignal entfernt wird. Das Abschalten schlägt die Selbstdiagnose (nächste Zeile) vor, wenn sie über längere Zeit keinen Echopfad sieht. | Eine zuverlässige API-Erkennung „Headset vs. Lautsprecher" existiert auf keiner der beiden Plattformen (**unbelegt** — in keiner gelesenen Quelle beschrieben). Weil der Preis von AEC gering ist (9 ms, 0,5 % Kern) und der Schaden ohne AEC groß (7,7 dB statt 38,8 dB, Spike 6 §8), ist „an" die richtige Default-Seite. Die Entscheidung, wann sich das Abschalten lohnt, trifft die Messung zur Laufzeit — nicht eine Heuristik über Gerätenamen. |
| **Laufende Selbstdiagnose** | `get_stats()` alle 2 s auswerten. Zwei Fälle: (1) bleibt `echo_return_loss_enhancement` über 10 s unter 6 dB, **obwohl** `echo_return_loss` einen Echopfad anzeigt → Hinweis „Echo erkannt, wird aber nicht entfernt — Referenzgerät prüfen". (2) zeigt `echo_return_loss` über 30 s Sprechzeit **keinen** Echopfad (kein messbarer Rückkopplungsweg, typisch für Headset) → einmaliger, wegklickbarer Hinweis „Kein Echo feststellbar — Echo-Unterdrückung kann ausgeschaltet werden, das spart 9 ms Latenz". | E3. Fall (1) deckt den Fehler ab, den Spike 6 §8 misst (falsches Referenzgerät → ERLE 7,7 dB). Fall (2) ersetzt die verworfene Headset-Heuristik durch eine Messung, die genau das misst, worauf es ankommt: ob es überhaupt einen Echopfad gibt. |

### Konsequenzen für andere Dokumente (nicht hier eingearbeitet)

Diese Datei ändert nur sich selbst und `verification.md`. Nachzuziehen sind:

- `audio-io.md`: Zeile „AEC | **keine** in v1" ist überholt; Kandidatenliste und Entscheidungstabelle müssen K1/K5 aufnehmen.
- `feature-freeze.md`: „Echo Cancellation | nein" wandert von „Ausdrücklich abgelehnt" nach „Enthalten"; der offene Punkt 9 („Bedarf an Echo Cancellation abfragen") ist durch die Vorgabe des Auftraggebers erledigt.
- `latency-budget.md`: neue Budgetzeile „AEC" mit 9 ms und die angepassten Zielwerte.
- `Client/IMPLEMENTATION.md` §6: neue Pipeline-Stufe, zweiter Referenz-Stream je Backend, Modul `dsp/aec.rs`. **Wird gerade von einem anderen Agenten bearbeitet — bewusst nicht angefasst.**
- `cross-compile.md` und `packaging-licensing.md`: der Windows-Cross-Build-Befund und die BSD-3-Clause der gebündelten WebRTC-Quellen (plus deren Weitergabepflicht im Binary).

## Verworfen, mit Grund

| Verworfen | Grund |
| --- | --- |
| **K2 Speex-AEC** als Primär- oder Fallback-Lösung | Gemessen durchweg unterlegen: 24,5–32,3 dB ERLE gegen 34–39 dB (Spike 6b vs. 6), doppelter CPU-Verbrauch (94 µs vs. 47 µs je 10 ms), ERLE bricht bei 120 ms Echopfad um 6 dB ein (AEC3: 0 dB), bei 100 ppm Clock-Drift um 10 dB (AEC3: 0 dB). Dazu: kein gepflegtes Rust-Binding (letzte Versionen 2018/2022, E11), keine Delay-Schätzung (E12), Residual-Suppression nur über den separaten Preprocessor. Einziger Vorteil — 0 ms algorithmische Verzögerung — wiegt das nicht auf. |
| **K3 `module-echo-cancel` als Produktlösung** | Verlagert die Verantwortung in eine Systemkonfiguration, die ICU nicht kontrolliert: Datei anlegen, Dienst neu starten, Node-Namen treffen. Nur Linux (E13) — wir bräuchten für Windows ohnehin K1, also zwei Implementierungen für ein Problem. Das Backend auf diesem Host ist zudem APM **1.3**, nicht 2.1 (E25), also eine andere und nicht von uns vermessene Codebasis. Bleibt als dokumentierter Fallback erhalten. |
| **K3 im Default-Modus (ohne `monitor.mode`)** | Referenz ist nur, was die App in die virtuelle Sink schickt — der Spielton fehlt (E13). Damit löst es genau das Problem nicht, dessentwegen AEC überhaupt eingeführt wird. |
| **K4 OS-AEC über `AudioCategory_Communications`** | Löst als Nebenwirkung die Default Ducking Experience aus (standardmäßig 80 % Dämpfung aller anderen Streams, 1 s Fade, E21) — für ein Tool neben einem Spiel disqualifizierend. ICU kann dieses Ducking nicht per API abstellen: `SetDuckingPreference` ist der Opt-out der gedämpften Anwendung (E22). Dazu: OEM-abhängig, für die App nicht inspizierbar, Qualität unbekannt (E17). |
| **`IAcousticEchoCancellationControl` als Primärpfad** | Erst ab Windows Build 22621 (Win 11 22H2, E18); `GetService` kann mit `E_NOINTERFACE` scheitern, selbst wenn der Endpoint AEC beherrscht (E18) → wir bräuchten K1 als Fallback und hätten wieder zwei Pfade. Bleibt als Option (c) für den blockierten Windows-Build notiert. |
| **In-Process-AEC mit ausschließlich eigener Playback-Referenz** | Gemessen 7,7 dB ERLE statt 38,8 dB, sobald außer der Voice noch Spielton aus demselben Lautsprecher kommt (Spike 6 §8). Für den Anwendungsfall „Voice neben Spiel" nutzlos. |
| **`set_stream_delay_ms` mit gemessenem oder geschätztem Delay füttern** | Bei AEC3 mit Default-Konfiguration ohne jede Wirkung, belegt im Upstream-Quelltext (`use_external_delay_estimator = false`, E8/E9) und empirisch (sechs Werte von 0 bis 200 ms, identische ERLE auf 0,3 dB, Spike 6 §3). Code, der so tut, als hätte er Wirkung, ist schlechter als kein Code. |
| **`EchoCanceller::Mobile` (AECM)** | Erreicht im Spike zwar hohe ERLE-Werte, aber **nur bei korrekt angegebenem Delay**: 120 ms daneben kosten 17 dB (Spike 6 §3). Damit hängt die Funktion an einer Größe, die auf Desktop-Systemen mit wechselnden Geräten und Puffern nicht verlässlich bekannt ist. AECM ist für Mobilgeräte mit bekannter, fester Hardwarekette gedacht. |
| **WebRTC-NS statt `nnnoiseless`** | Würde die in `audio-io.md` belegte Entscheidung (pure Rust, BSD-3, VAD-Ausgang inklusive) umwerfen, um 6,7 dB Restecho zu gewinnen, das der Residual-Suppressor des AEC ohnehin adressiert. Bleibt als schaltbare Reserve, nicht als Default. |
| **WebRTC-AGC** | Unverändert abgelehnt (`audio-io.md`): AGC verändert die Dynamik; „glasklar" heißt unverfälscht. Im Spike zusätzlich gemessen: bringt keine ERLE, kostet 1 µs. |
| **Feste AEC ohne UI-Schalter** | Headset-Nutzer zahlen 9 ms Latenz für nichts. Der Schalter kostet eine Zeile UI. |
| **Automatische Headset-Erkennung über Geräteklassen/Form-Faktor-APIs** | Keine der gelesenen Quellen beschreibt eine verlässliche Klassifikation „Headset vs. Lautsprecher" (**unbelegt**). |
| **Heuristik „AEC aus, wenn Input und Output dasselbe Gerät sind"** (zwischenzeitlich erwogen) | Trifft genau den häufigsten Echo-Fall falsch: Laptop-Mikrofon plus Laptop-Lautsprecher sind dieselbe Soundkarte, dasselbe Gerät — die Heuristik würde dort AEC abschalten. Sie unterscheidet USB-Headset (richtig: aus) nicht von eingebauter Hardware (falsch: aus). Ersetzt durch die Laufzeit-Diagnose über `echo_return_loss`, die misst statt zu raten. |

## Offene Punkte für Step 3

| # | Prüfung | Messvorschrift |
| --- | --- | --- |
| A1 | **Windows-Build entscheiden** (blockierend) | Variante (a) `build.rs`-Patch: meson-Cross-File für `x86_64-w64-mingw32` erzeugen, abseil für mingw bauen oder als meson-Subproject entpacken, danach `cargo build --target x86_64-pc-windows-gnu`; Erfolgskriterium: `file` auf ein Objekt der erzeugten `.a` meldet PE/COFF, und das `.exe` linkt. Variante (b): nativer Windows-Host. Variante (c): OS-AEC-Pfad. |
| A2 | **Akustische Ende-zu-Ende-Messung mit echtem Lautsprecher und Mikrofon** | Jabra Engage 75 als Mikrofon, externe Lautsprecher als Ausgabe; Far-End = 60 s Sprachaufnahme; ERLE gegen die Messung ohne AEC; Zielwert ≥ 30 dB. Ersetzt die synthetische Raumsimulation aus Spike 6. |
| A3 | **Double-Talk mit echter Sprache** | Spike 6 verwendet moduliertes, gefärbtes Rauschen als Near-End. Das ist für den adaptiven Filter realistisch, für die Bewertung des *Spracherhalts* aber ungeeignet: WebRTC-NS behandelt rauschähnliche Signale definitionsgemäß als Rauschen. Zu wiederholen mit zwei echten Sprachaufnahmen; Metrik PESQ oder Hörtest, nicht SI-SDR. |
| A4 | **PipeWire `stream.capture.sink` praktisch** | Zweiten Capture-Stream mit `stream.capture.sink = true` auf dem Default-Sink öffnen, prüfen: liefert er den Systemmix inklusive eines fremden Abspielprozesses? Wird er beim Sink-Wechsel automatisch umgehängt oder braucht es ein neues `target.object`? Latenz gegenüber dem Mikrofon-Stream in Samples messen. |
| A5 | **PipeWire-Drift zwischen Mikrofon und Monitor-Stream** | 10 min beide Streams zählen; erwartet 0 ppm, weil derselbe Graph-Clock; Abweichung protokollieren. |
| A6 | **Windows-Loopback praktisch** (echte Hardware) | Loopback-Stream event-getrieben öffnen, Drift gegen das Mikrofon über 10 min in ppm messen, Verhalten bei Default-Endpoint-Wechsel. Der gemessene ppm-Wert ist gegen die Grenze aus Spike 6 §6b zu halten (unkritisch bis ~100 ppm, Ausfall ab ~500 ppm); liegt er darüber, braucht die Referenz einen Slip-Resampler (`rubato::Slip`, `latency-budget.md` L16). |
| A7 | **`module-echo-cancel` mit `monitor.mode = true`** | Konfigurationsdatei in `~/.config/pipewire/pipewire.conf.d/` anlegen, PipeWire neu starten, prüfen: erscheinen die Nodes, ist die Referenz wirklich der Systemmix, wie hoch ist die Zusatzlatenz (`pw-top`)? Zusätzlich prüfen, ob `pw-cli load-module` als unprivilegierter User funktioniert (aktuell unbelegt). |
| A8 | **Skalen-Bug-Test** | Unit-Test, der einen bekannten Sinus durch `APM → nnnoiseless` schickt und prüft, dass die Amplitude am Ende stimmt. Die beiden Bibliotheken erwarten ±1.0 bzw. ±32768. |
| A9 | **XRun-/Gerätewechsel-Verhalten** | Render-Stream 200 ms aussetzen lassen, ERLE-Verlauf protokollieren; vergleichen: einfach weiterlaufen vs. `reinitialize()`. Entscheidet die Recovery-Strategie. |
| A10 | **CPU auf schwacher Hardware** | 0,47 % gelten für den Entwicklungshost. Auf der langsamsten Zielmaschine der Gruppe nachmessen; Budget zusammen mit RNNoise (11 %) und Opus. |
| A11 | **Lizenz-Weitergabe** | Die gebündelten WebRTC-Quellen stehen unter BSD-3-Clause (Google). Prüfen, dass der Lizenztext den Binary-Distributionen beiliegt („Redistributions in binary form must reproduce the above copyright notice"), und in `packaging-licensing.md` eintragen. |
| A12 | **Vergleich APM 1.3 (PipeWire-Backend) vs. 2.1 (unser Crate)** | Nur nötig, falls der PipeWire-Fallback jemals Produktweg werden soll: gleiche Signale durch beide, ERLE vergleichen. Aktuell unbelegt. |

## Verified against

- [x] Step 3 am 2026-09-20 — empirisch belegt durch **Spike 6** (`Research/spikes/spike-aec/`, Details auch in `verification.md`). Host: Fedora 43, Linux 7.2.5, rustc 1.98.1, Release-Build mit Debug-Info.

### Build-Gate

| Prüfung | Ergebnis |
| --- | --- |
| `webrtc-audio-processing` 2.1.0 mit Feature `bundled`, Target `x86_64-unknown-linux-gnu` | **PASS**, nach Installation von `meson` + `abseil-cpp-devel` und Anlegen des `rust-objcopy`-Symlinks |
| Erster Versuch ohne Symlink | **FAIL**: `cargo:warning=rust-objcopy not found at "/usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-objcopy"` → `Error: Failed to execute … Caused by: No such file or directory (os error 2)`. Kein Env-Override im `build.rs` vorhanden. |
| Workaround | `sudo mkdir -p /usr/lib/rustlib/x86_64-unknown-linux-gnu/bin && sudo ln -sfn /usr/bin/llvm-objcopy /usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-objcopy` (GNU-/LLVM-objcopy beherrschen `--redefine-sym` und `@file`) |
| Bundled-Quellen im Crate | vorhanden (5,3 MB, `meson.build` meldet `version : '2.1'`); meson+ninja bauen `libwebrtc-audio-processing-2.a`, 3 827 Symbole werden mit `v2_` präfixt |
| Target `x86_64-pc-windows-gnu` | **FAIL**. Unmittelbarer Abbruch: `x86_64-w64-mingw32-g++` beim Kompilieren von `src/wrapper.cpp` → `fatal error: absl/base/config.h: No such file or directory` (Include-Pfad zeigt auf das nie entpackte meson-Subproject `subprojects/abseil-cpp-20240722.0`, vorhanden ist nur `abseil-cpp.wrap`). **Strukturelle Ursache**: `build.rs` ruft `meson setup` ohne `--cross-file`; die im Windows-Target-Verzeichnis erzeugte `libwebrtc-audio-processing-2.a` enthält ELF-x86-64-Objekte (`file` auf `aec3_adaptive_fir_filter_avx2.cc.o`: „ELF 64-bit LSB relocatable, x86-64"), also Linux-Code. |

### Messaufbau Spike 6

48 kHz mono, 10-ms-Frames (480 Samples), 10 s Material je Lauf. Far-End und Near-End sind unabhängig erzeugte, sprachähnliche Signale (gefärbtes Rauschen mit zwei Formant-Resonatoren, silbenartige Hüllkurve bei 3,3 bzw. 2,7 Hz, Satzpausen) — absichtlich kein weißes Rauschen, weil das einen adaptiven Filter unrealistisch schnell konvergieren lässt. Echopfad: Direktschall nach *D* ms mit ERL ≈ 6 dB plus 80 ms abklingende Nachhall-IR (60 Taps); Mikrofon-Grundrauschen −50 dBFS. Far-RMS −20 dBFS, Echo-RMS −8,3 dBFS, Near-RMS im Double-Talk −9,4 dBFS. Zeitplan: 0–4 s nur Far-End, 4–7 s Double-Talk, 7–10 s nur Far-End. Reihenfolge je Tick: Render-Frame, dann Capture-Frame. Gegenprobe per Kreuzkorrelation: bei *D* = 80 ms liegt das Echo tatsächlich 4 130 Samples = 86,0 ms hinter dem Far-End (80 ms Direktschall + IR-Schwerpunkt).

**Messvorbehalt**: Das Mikrofon-Grundrauschen von −50 dBFS deckelt die messbare ERLE auf etwa 42 dB. Werte nahe 39 dB heißen daher „Restecho liegt am Rauschteppich", nicht „genau 39 dB Dämpfung". Werte unter 35 dB sind echte algorithmische Grenzen.

### Ergebnisse WebRTC AEC3

| Messung | Ergebnis |
| --- | --- |
| **Algorithmische Verzögerung** (Impuls durch den Capture-Pfad, Render still) | **430 Samples = 8,96 ms**, Spitzenamplitude 0,446 von 0,500 |
| **ERLE Single-Talk, eingeschwungen** (linearer Lautsprecher, *D* = 80 ms) | **39,3 dB** (2–4 s) / **36,8 dB** (8–10 s) |
| **Konvergenz** (ERLE je Sekunde) | 0 s 37,5 · 1 s 35,4 · 2 s 38,2 · 3 s 40,6 · 4 s 4,1 (Double-Talk beginnt) · 5 s 10,8 · 6 s 21,7 · 7 s 30,4 · 8 s 41,5 · 9 s 31,5 |
| **CPU je 10-ms-Frame** | capture mean **43 µs** (p50 43, p99 61) · render mean **4 µs** (p50 4, p99 6) · Summe **47 µs = 0,47 % eines Kerns** |
| **Double-Talk, Near-End-Pausen** | ERLE **31,7 dB** (unbearbeitet 0,0 dB) |
| **Double-Talk, Near-End aktiv** | SI-SDR 0,6 dB gegen die Baseline (dasselbe APM ohne Far-End), Korrelation 0,536; unbearbeitetes Mikrofon 7,2 dB / 0,709. → messbare Beeinträchtigung der Nah-Sprache während Double-Talk. **Vorbehalt**: das synthetische Near-End ist rauschähnlich; belastbare Aussagen brauchen echte Sprache (offener Punkt A3). |

**Lautsprecher-Nichtlinearität** (Soft-Clipping vor dem Raum; `drive` = Übersteuerungsgrad):

| drive | ERLE 2–4 s | ERLE 8–10 s | DT SI-SDR | DT-Pausen-ERLE |
| --- | --- | --- | --- | --- |
| 0 (linear) | 39,3 dB | 36,8 dB | 0,6 dB | 31,7 dB |
| 2 | 38,6 dB | 36,0 dB | 0,6 dB | 31,5 dB |
| 5 | 35,3 dB | 34,3 dB | 0,7 dB | 31,5 dB |
| 10 (stark) | 18,1 dB | 31,1 dB | −0,5 dB | 28,9 dB |

**Delay-Robustheit, `set_stream_delay_ms` bei AEC3 Full** (wahres Delay 80 ms):

| stream_delay_ms | ERLE 2–4 s | ERLE 8–10 s | DT SI-SDR |
| --- | --- | --- | --- |
| None (auto) | 39,3 dB | 36,8 dB | 0,4 dB |
| 50 (−30 ms) | 39,5 dB | 36,7 dB | 0,4 dB |
| 80 (korrekt) | 39,2 dB | 36,8 dB | 0,4 dB |
| 110 (+30 ms) | 39,6 dB | 36,9 dB | 0,5 dB |
| 0 | 39,3 dB | 36,9 dB | 0,4 dB |
| 200 | 39,3 dB | 36,9 dB | 0,4 dB |

→ **Kein Effekt.** Gegenprobe mit `EchoCanceller::Mobile` (AECM), wo der Parameter laut Crate-Doku Pflicht ist, zeigt, dass der Wert sehr wohl ankommt:

| stream_delay_ms (AECM) | ERLE 2–4 s | ERLE 8–10 s |
| --- | --- | --- |
| 0 | 42,6 dB | 43,9 dB |
| 50 | 44,9 dB | 44,4 dB |
| 80 | 32,0 dB | 39,4 dB |
| 110 | 24,3 dB | 35,9 dB |
| 200 | 23,6 dB | 26,4 dB |

**Wahres Echopfad-Delay variieren** (AEC3, auto):

| wahres Delay | 40 ms | 80 ms | 120 ms | 200 ms | 400 ms |
| --- | --- | --- | --- | --- | --- |
| ERLE 2–4 s | 39,5 dB | 39,3 dB | 39,4 dB | 38,8 dB | 37,2 dB |
| ERLE 8–10 s | 36,5 dB | 36,8 dB | 37,0 dB | 36,5 dB | 37,5 dB |

**Delay-Sprung** 80 → 140 ms bei *t* = 5 s (ERLE je Sekunde): … 6 s 23,6 · **7 s 10,1** · 8 s 38,6 · 9 s 30,0 → Rekonvergenz in **rund 1 s**.

**Clock-Drift** zwischen Capture- und Playback-Uhr, 10-s-Lauf:

| Drift | 0 ppm | 10 ppm | 100 ppm | 1 000 ppm | 5 000 ppm |
| --- | --- | --- | --- | --- | --- |
| ERLE 8–10 s, linear | 36,8 dB | 37,0 dB | 36,9 dB | 35,9 dB | 10,9 dB |
| ERLE 8–10 s, drive 5 | 34,3 dB | 34,3 dB | 34,0 dB | 33,2 dB | 11,3 dB |

**Diese Tabelle allein ist irreführend** und wurde deshalb nachgemessen: 100 ppm ergeben in 10 s nur 48 Samples Schlupf — weniger als ein AEC3-Block (64 Samples bei 16 kHz Bandrate, 192 bei 48 kHz). In diesem Lauf findet also überhaupt keine Neuausrichtung statt; „kein Verlust" ist trivial wahr und sagt nichts über eine echte Session. Langer Lauf ohne Double-Talk, drive 5, ERLE in den ersten 8 s gegen die letzten 10 s:

| Drift | Dauer | Schlupf am Ende | ERLE 2–10 s | ERLE letzte 10 s |
| --- | --- | --- | --- | --- |
| 0 ppm | 60 s | 0 Samples / 0,0 ms | 34,2 dB | 34,7 dB |
| 100 ppm | 60 s | 288 Samples / 6,0 ms | 34,1 dB | 34,3 dB |
| 500 ppm | 60 s | 1 440 Samples / 30,0 ms | 33,7 dB | **17,4 dB** |
| 1 000 ppm | 60 s | 2 880 Samples / 60,0 ms | 17,7 dB | **8,0 dB** |
| 100 ppm | 300 s | 1 440 Samples / 30,0 ms | 34,1 dB | 34,2 dB |
| 100 ppm | **600 s** | **2 880 Samples / 60,0 ms** | 34,1 dB | **33,8 dB** |
| 20 ppm | 600 s | 576 Samples / 12,0 ms | 34,3 dB | 34,6 dB |

→ Die beiden hervorgehobenen Zeilen haben **denselben** aufsummierten Schlupf (2 880 Samples), unterscheiden sich aber um 26 dB. **Entscheidend ist die Driftrate, nicht der Schlupf.** AEC3 richtet laufend nach und folgt langsamem Drift zeitlich unbegrenzt; bei schnellem Drift überholt der Schlupf die Nachführung. Die Grenze liegt zwischen 100 und 500 ppm — realistischer Gerätedrift getrennter Soundkarten liegt darunter (**Größenordnung unbelegt, Annahme**), aber der Punkt gehört gemessen (offener Punkt A6).

**Kombinationen im APM** (Szene mit drive 5):

| Konfiguration | ERLE 8–10 s | DT SI-SDR vs. Baseline | DT-Korr | DT-Pausen-ERLE | CPU je 10 ms |
| --- | --- | --- | --- | --- | --- |
| AEC allein | 34,3 dB | 0,7 dB | 0,557 | 31,5 dB | 46 µs (0,46 % Kern) |
| AEC + HPF + NS | 41,0 dB | 6,2 dB | 0,897 | 28,1 dB | 53 µs (0,53 % Kern) |
| AEC + HPF + NS + AGC2 | 41,0 dB | 6,2 dB | 0,897 | 28,1 dB | 54 µs (0,54 % Kern) |

**Die entscheidende Messung — welche Referenz?** Mikrofon hört Voice-Echo *und* Spielton (beide über denselben Lautsprecher, drive 5):

| Referenzsignal | ERLE 2–4 s | ERLE 8–10 s |
| --- | --- | --- |
| nur eigener Voice-Playback | **8,7 dB** | **7,7 dB** |
| Systemmix (Voice + Spielton) | **40,9 dB** | **38,8 dB** |

### Ergebnisse Speex-AEC (Spike 6b, libspeexdsp 1.2.1, identische Signale)

Algorithmische Verzögerung: **0 Samples = 0,00 ms** (Spitze 16 253 von 16 384).

| filter_length | ERLE 2–4 s | ERLE 8–10 s | DT SI-SDR | DT-Pausen-ERLE | CPU/Frame |
| --- | --- | --- | --- | --- | --- |
| 100 ms | 26,6 dB | 29,2 dB | −7,9 dB | 26,9 dB | 56 µs |
| 200 ms | 27,1 dB | 32,1 dB | −8,0 dB | 28,8 dB | 75 µs |
| 300 ms | 24,5 dB | 30,8 dB | −8,1 dB | 27,8 dB | 93 µs |
| 500 ms | 23,4 dB | 25,2 dB | −8,6 dB | 23,5 dB | 128 µs |

| Szenario (Frame 10 ms, filter 300 ms) | ERLE 2–4 s | ERLE 8–10 s |
| --- | --- | --- |
| linear, kein Drift | 24,5 dB | 30,8 dB |
| drive 5 | 25,0 dB | 27,8 dB |
| drive 10 | 23,7 dB | 25,1 dB |
| Drift 100 ppm | 22,8 dB | **20,1 dB** |
| Drift 1 000 ppm | 19,5 dB | 20,4 dB |
| Drift 5 000 ppm | 19,7 dB | 20,2 dB |

| wahres Delay | 40 ms | 80 ms | 120 ms | 200 ms | 400 ms |
| --- | --- | --- | --- | --- | --- |
| ERLE 8–10 s | 32,3 dB | 30,8 dB | 24,2 dB | 25,4 dB | 20,6 dB |

Frame 20 ms statt 10 ms: 28,2 dB statt 30,8 dB, 129 µs je 20-ms-Frame (0,65 % Kern).

### Direktvergleich

| Kriterium | WebRTC AEC3 (2.1.0) | Speex MDF (1.2.1) |
| --- | --- | --- |
| ERLE eingeschwungen, linear, 80 ms | **36,8 dB** | 30,8 dB |
| ERLE bei starker Lautsprecherverzerrung | **31,1 dB** | 25,1 dB |
| ERLE bei 120 ms Echopfad | **37,0 dB** | 24,2 dB |
| ERLE bei 100 ppm Clock-Drift (10 s) | **36,9 dB** | 20,1 dB |
| ERLE bei 100 ppm Clock-Drift nach 600 s | **33,8 dB** | nicht gemessen |
| CPU je 10-ms-Frame | **47 µs** | 93 µs |
| Algorithmische Verzögerung | 8,96 ms | **0,00 ms** |
| Delay-Angabe nötig | nein (eigene Schätzung) | nein, aber Filter muss Delay abdecken |
| Rust-Binding gepflegt | ja (2026-05) | **nein** (2018/2022) |
| Baut auf Fedora 43 | ja (mit objcopy-Workaround) | ja (Systembibliothek) |
| Cross nach Windows-gnu | **nein** (struktureller Crate-Mangel) | ungetestet |

### Gelesene Quellen

Per WebFetch gelesen (2026-09-20): E13, E14, E18, E19, E20, E21, E22, E23 (Suche + Summary).

Per `curl` im Volltext gelesen und mit `grep`/`sed` zitiert: E12, E13 (HTML-Volltext).

Per crates.io-API abgefragt: E1, E11.

Lokal im entpackten Crate bzw. auf dem Host gelesen und zitiert: E3, E4, E5, E6, E7, E8, E9, E10, E15.

Nicht erneut abgerufen: **E16 und E17** — die wörtlichen Zitate sind aus `audio-io.md` (dort A10 bzw. A11) übernommen, die beide am 2026-09-20 per WebFetch gelesen wurden.

Nicht gelesen / nicht erreichbar: ITU-T G.168 Volltext-PDF (hinter Login; nur Landing-Seite und Summary verwendet, E23); ITU-T G.167 „Acoustic echo controllers" (nicht abgerufen — als möglicher normativer Bezug für ERLE-Zielwerte offen); docs.rs für `webrtc-audio-processing` (Build dort fehlgeschlagen, siehe `audio-io.md` A42).
