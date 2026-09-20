# Audio-I/O: Capture/Playback je OS, Rust-Crates, DSP-Kette

## Frage

Welcher Audio-Pfad liefert auf Windows (WASAPI shared vs. exclusive, IAudioClient3) und Linux (PipeWire nativ vs. ALSA-/PulseAudio-Kompatibilität) die niedrigste stabile Latenz für ein Voice-Tool neben einem laufenden Spiel, welches Rust-Crate deckt das ab (cpal, pipewire-rs, windows-rs), wie werden Geräteauswahl, Hot-Plug und Resampling gelöst, und welche DSP-Kette (VAD, Noise Suppression, AEC, AGC) ist für "glasklar" nötig?

## Kandidaten

| Thema | Kandidaten |
| --- | --- |
| Windows-Backend | WASAPI shared (IAudioClient, 10 ms Engine-Periode), WASAPI shared low-period (IAudioClient3), WASAPI exclusive, ASIO |
| Linux-Backend | PipeWire nativ (libpipewire), ALSA-Plugin `pcm_pipewire` (pipewire-alsa), PulseAudio-API (pipewire-pulse) |
| Rust-Crates I/O | `cpal` 0.18 (Hosts: WASAPI, PipeWire, PulseAudio, ALSA, JACK, ASIO), `pipewire` 0.10 (pipewire-rs), `windows` (windows-rs) direkt |
| Resampling | PipeWire-/WASAPI-Engine-Resampler, `rubato` 5.0 (Async, Fft, Slip) |
| VAD | Energie/Schwellwert, RNNoise-VAD-Ausgang, WebRTC-VAD (`webrtc-vad` 0.4), `earshot` 1.2, Silero (`voice_activity_detector` 0.2, onnxruntime) |
| Noise Suppression | RNNoise via `nnnoiseless` 0.5 (pure Rust), WebRTC-NS via `webrtc-audio-processing` 2.1, OS-APO (Windows Communications-Modus) |
| AEC | keine (Headset), `webrtc-audio-processing` AEC3 in-process, Windows-APO, PipeWire `module-echo-cancel` |
| AEC-Referenz | eigener Playback-Mix, Systemmix (WASAPI Loopback bzw. PipeWire Monitor/`stream.capture.sink`) |
| AGC | keine, `webrtc-audio-processing` AGC, einfacher Limiter |

## Primärquellen

Abrufdatum aller Quellen: 2026-09-20.

| # | Quelle | URL |
| --- | --- | --- |
| A1 | Microsoft Learn: Exclusive-Mode Streams | https://learn.microsoft.com/en-us/windows/win32/coreaudio/exclusive-mode-streams |
| A2 | Microsoft Learn: User-Mode Audio Components | https://learn.microsoft.com/en-us/windows/win32/coreaudio/user-mode-audio-components |
| A3 | Microsoft Learn: Low Latency Audio (Windows drivers) | https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/low-latency-audio |
| A4 | Microsoft Learn: IAudioClient3::GetSharedModeEnginePeriod | https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient3-getsharedmodeengineperiod |
| A5 | Microsoft Learn: IAudioClient3::InitializeSharedAudioStream | https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient3-initializesharedaudiostream |
| A6 | Microsoft Learn: IAudioClient::Initialize | https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize |
| A7 | Microsoft Learn: IAudioClient::GetDevicePeriod | https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getdeviceperiod |
| A8 | Microsoft Learn: AUDCLNT_STREAMFLAGS_XXX | https://learn.microsoft.com/en-us/windows/win32/coreaudio/audclnt-streamflags-xxx-constants |
| A9 | Microsoft Learn: AUDCLNT_STREAMOPTIONS | https://learn.microsoft.com/en-us/windows/win32/api/audioclient/ne-audioclient-audclnt_streamoptions |
| A10 | Microsoft Learn: AUDIO_STREAM_CATEGORY | https://learn.microsoft.com/en-us/windows/win32/api/audiosessiontypes/ne-audiosessiontypes-audio_stream_category |
| A11 | Microsoft Learn: Audio Signal Processing Modes | https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/audio-signal-processing-modes |
| A12 | PipeWire: pipewire-props(7) | https://docs.pipewire.org/page_man_pipewire-props_7.html |
| A13 | PipeWire: pipewire.conf(5) | https://docs.pipewire.org/page_man_pipewire_conf_5.html |
| A14 | PipeWire: pipewire(1) (Umgebungsvariablen) | https://docs.pipewire.org/page_man_pipewire_1.html |
| A15 | PipeWire: pipewire-client.conf(5) (ALSA-Plugin-Optionen, stream.properties) | https://docs.pipewire.org/page_man_pipewire-client_conf_5.html |
| A16 | PipeWire: libpipewire-module-protocol-pulse(7) (pulse.min.req usw.) | https://docs.pipewire.org/page_module_protocol_pulse.html |
| A17 | PipeWire: pipewire-pulse(1) | https://docs.pipewire.org/page_man_pipewire-pulse_1.html |
| A18 | PipeWire: pipewire-pulse.conf(5) | https://docs.pipewire.org/page_man_pipewire-pulse_conf_5.html |
| A19 | PipeWire: Tutorial 4 (Playing a tone) | https://docs.pipewire.org/page_tutorial4.html |
| A20 | PipeWire: Scheduling | https://docs.pipewire.org/page_scheduling.html |
| A21 | PipeWire: Overview | https://docs.pipewire.org/page_overview.html |
| A22 | pipewire-rs Doku (offizielle Pages, da docs.rs-Build 0.10.1 fehlgeschlagen) | https://pipewire.pages.freedesktop.org/pipewire-rs/pipewire/ |
| A23 | docs.rs pipewire (Build-Status) | https://docs.rs/pipewire/latest/pipewire/ |
| A24 | crates.io API: pipewire | https://crates.io/api/v1/crates/pipewire |
| A25 | docs.rs cpal 0.18.2 | https://docs.rs/cpal/latest/cpal/ |
| A26 | docs.rs cpal BufferSize | https://docs.rs/cpal/latest/cpal/enum.BufferSize.html |
| A27 | cpal README (master) | https://raw.githubusercontent.com/RustAudio/cpal/master/README.md |
| A28 | cpal CHANGELOG (master) | https://raw.githubusercontent.com/RustAudio/cpal/master/CHANGELOG.md |
| A29 | cpal `src/host/wasapi/device.rs` (master) | https://raw.githubusercontent.com/RustAudio/cpal/master/src/host/wasapi/device.rs |
| A30 | cpal `src/host/alsa/mod.rs` (master) | https://raw.githubusercontent.com/RustAudio/cpal/master/src/host/alsa/mod.rs |
| A31 | cpal `src/host/pipewire/device.rs` (master) | https://raw.githubusercontent.com/RustAudio/cpal/master/src/host/pipewire/device.rs |
| A32 | cpal `src/host` Verzeichnis | https://github.com/RustAudio/cpal/tree/master/src/host |
| A33 | crates.io API: cpal | https://crates.io/api/v1/crates/cpal |
| A34 | docs.rs rubato 5.0.0 | https://docs.rs/rubato/latest/rubato/ |
| A35 | rubato README (master) | https://raw.githubusercontent.com/HEnquist/rubato/master/README.md |
| A36 | crates.io API: rubato | https://crates.io/api/v1/crates/rubato |
| A37 | docs.rs nnnoiseless DenoiseState | https://docs.rs/nnnoiseless/latest/nnnoiseless/struct.DenoiseState.html |
| A38 | docs.rs nnnoiseless (Crate) | https://docs.rs/nnnoiseless/latest/nnnoiseless/ |
| A39 | nnnoiseless README + `src/lib.rs` (master) | https://raw.githubusercontent.com/jneem/nnnoiseless/master/README.md , https://raw.githubusercontent.com/jneem/nnnoiseless/master/src/lib.rs |
| A40 | crates.io API: nnnoiseless | https://crates.io/api/v1/crates/nnnoiseless |
| A41 | RNNoise Repo, README, `include/rnnoise.h`, `src/denoise.c` | https://github.com/xiph/rnnoise , https://raw.githubusercontent.com/xiph/rnnoise/main/README , https://raw.githubusercontent.com/xiph/rnnoise/main/include/rnnoise.h , https://raw.githubusercontent.com/xiph/rnnoise/main/src/denoise.c |
| A42 | docs.rs webrtc-audio-processing | https://docs.rs/webrtc-audio-processing/latest/webrtc_audio_processing/ |
| A43 | webrtc-audio-processing README, `Cargo.toml`, sys-`README.md`, sys-`build.rs` (main) | https://raw.githubusercontent.com/tonarino/webrtc-audio-processing/main/README.md , https://raw.githubusercontent.com/tonarino/webrtc-audio-processing/main/Cargo.toml , https://raw.githubusercontent.com/tonarino/webrtc-audio-processing/main/webrtc-audio-processing-sys/README.md , https://raw.githubusercontent.com/tonarino/webrtc-audio-processing/main/webrtc-audio-processing-sys/build.rs |
| A44 | crates.io API: webrtc-audio-processing | https://crates.io/api/v1/crates/webrtc-audio-processing |
| A45 | crates.io API: webrtc-vad | https://crates.io/api/v1/crates/webrtc-vad |
| A46 | docs.rs earshot, README, Cargo.toml | https://docs.rs/earshot/latest/earshot/ , https://raw.githubusercontent.com/pykeio/earshot/main/README.md , https://raw.githubusercontent.com/pykeio/earshot/main/Cargo.toml |
| A47 | crates.io API: earshot | https://crates.io/api/v1/crates/earshot |
| A48 | docs.rs voice_activity_detector | https://docs.rs/voice_activity_detector/latest/voice_activity_detector/ |
| A49 | crates.io API: voice_activity_detector | https://crates.io/api/v1/crates/voice_activity_detector |
| A50 | Host-Inventar (eigene Erhebung) | `Research/verification.md` |

## Befunde

### Windows WASAPI

| Fakt | Wert | Quelle |
| --- | --- | --- |
| Exclusive-Modus | "Exclusive-mode streams seize the audio endpoint—all system sounds, notifications, and audio from other applications are silenced." Nutzer können Exclusive je Gerät deaktivieren. | A1 |
| Exclusive-Empfehlung von Microsoft | "Evaluate shared-mode low-period streams before committing to exclusive mode unless you have a hard requirement for bit-exact output or custom sample rates." | A1 |
| Shared-Modus, Format | Client "must open the stream in the mix format that is currently in use by the audio engine (or a format that is similar to the mix format)"; Engine mischt nur PCM | A2 |
| Shared-Modus, Sample-Rate-Konvertierung | `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`: "A channel matrixer and a sample rate converter are inserted as necessary"; `SRC_DEFAULT_QUALITY` = besserer SRC "if the audio is ultimately intended to be heard by humans" | A8 |
| Engine-Latenz | Vor Windows 10: Engine ~12 ms (float) / ~6 ms (int), Puffer "always set to ~10 ms"; Windows 10+: Engine-Latenz 1.3 ms (Render), ~0 ms (Capture); Puffergröße "defined by the audio driver" | A3 |
| Default-Periode | "by default all applications in Windows 10 and later will use 10-ms buffers to render and capture audio" | A3 |
| IAudioClient3 | `GetSharedModeEnginePeriod` liefert Default-, Fundamental-, Min-, Max-Periode in Frames; Beispiel 44.1 kHz: default 448 (10.16 ms), fundamental 4, min 48 (1.09 ms), max 448 | A4 |
| InitializeSharedAudioStream | Periode muss Vielfaches der Fundamental-Periode und zwischen Min und Max sein; einziges erlaubtes Stream-Flag `AUDCLNT_STREAMFLAGS_EVENTCALLBACK`; "this method does not allow you to specify a buffer size. The buffer size is computed based on the periodicity" | A5 |
| Periode ist global je Endpoint | "if one application requests the usage of small buffers, then the audio engine will start transferring audio using that particular buffer size. In that case, all applications that use the same endpoint and mode will automatically switch"; Fehler `AUDCLNT_E_ENGINE_PERIODICITY_LOCKED` wenn bereits anders gesperrt | A3, A5 |
| Inbox-HDAudio-Treiber | unterstützt "buffer sizes between 128 samples (2.66ms@48kHz) and 480 samples (10ms@48kHz)"; kleinere Puffer brauchen aktualisierte Treiber (OEM-abhängig) | A3 |
| Exclusive, event-driven | `hnsPeriodicity` muss ungleich 0 und gleich `hnsBufferDuration` sein; zwei Puffer ("ping-pong"); "the latency for an event-driven, exclusive-mode stream depends directly on the buffer size"; `AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED` → mit `GetBufferSize` neu ausrichten | A6 |
| Shared, event-driven (IAudioClient) | "the caller must set both hnsPeriodicity and hnsBufferDuration to 0"; Engine-Periode fix, Client kann sie nicht steuern | A6, A7 |
| GetDevicePeriod | Default-Periode = Shared-Scheduling-Periode; Minimum-Periode = Exclusive-Minimum; Einheit 100 ns | A7 |
| Shared vs. Exclusive Latenz | Exclusive: "the data bypasses the audio engine"; "if an application opens an endpoint in exclusive mode, then there's no other application that can use that endpoint" | A3 |
| Raw-Modus | `AUDCLNT_STREAMOPTIONS_RAW`: "bypasses all signal processing except for endpoint specific, always-on processing"; nur wenn `System.Devices.AudioDevice.RawProcessingSupported` | A9, A11 |
| Raw Capture | "Raw capture streams must not include any time varying or adaptive processing, such as echo control, automatic gain control, or noise suppression." | A11 |
| MATCH_FORMAT | `AUDCLNT_STREAMOPTIONS_MATCH_FORMAT`: Engine übernimmt Client-Format "only if the format is supported by the audio driver and associated APOs"; sonst `AUDCLNT_E_ENGINE_FORMAT_LOCKED` | A9, A5 |
| Kategorien Capture | gültig nur `AudioCategory_Communications`, `AudioCategory_Speech`, `AudioCategory_Other` | A10 |
| Kategorie Communications | "Real-time communications, such as VOIP or chat"; mappt auf Modus Communications (OEM-APOs: AEC, NS, AGC möglich); "tagging a stream as Communications will let Windows know to pause background music" | A10, A11 |
| Kategorie GameChat (Render) | "Similar to AudioCategory_Communications except that AudioCategory_GameChat will not attenuate other streams" | A10 |
| Thread-Priorität | MMCSS "Pro Audio" / "Audio"; Microsoft empfiehlt für WASAPI die RT Work Queue API statt eigener Threads | A1, A3 |
| Timing für Windows-Tests | Nicht auf diesem Host verifizierbar (kein Windows) | A50 |

### Linux PipeWire

| Fakt | Wert | Quelle |
| --- | --- | --- |
| Quantum-Defaults | `default.clock.rate = 48000`, `default.clock.quantum = 1024`, `min-quantum = 32`, `max-quantum = 8192`, `quantum-limit = 8192`, `quantum-floor = 4` | A13 |
| Host-Werte | clock.rate 48000, default quantum 1024, min-quantum 32; PipeWire 1.4.11 | A50 |
| Latenz-Anforderung je Node | `node.latency` "Sets a suggested latency on the node as a fraction. This is just a suggestion, the graph will try to configure this latency or less for the graph." Beispiel 1024/48000 | A12 |
| Rate | `node.rate` Vorschlag; `node.force-rate`/`node.force-quantum` erzwingen; `node.lock-quantum` friert ein | A12 |
| Env-Variablen | `PIPEWIRE_LATENCY` "Sets a specific latency for a stream or filter. This is only a suggestion but the configured latency will not be larger."; `PIPEWIRE_QUANTUM` erzwingt Rate und Buffer-Size | A14 |
| Graph-Format | "the graph processing format is always float 32" | A12 |
| Resampler | `resample.quality` 0–14, default 4; höhere Qualität "at the expense of (much) more CPU consumption, latency and more ringing"; `resample.disable` erlaubt nur kompatible Raten | A12 |
| Node-Gruppierung | `node.group`/`node.link-group`; cpal setzt `NODE_GROUP` "so PipeWire schedules them in the same quantum, preventing phase drift between simultaneous input/output streams" | A31 |
| Stream-API | `pw_stream_new_simple` mit `PW_KEY_MEDIA_TYPE/CATEGORY/ROLE`; `pw_stream_connect` mit `AUTOCONNECT | MAP_BUFFERS | RT_PROCESS`; Prozess-Callback `on_process` | A19 |
| PulseAudio-Kompat | `pulse.min.req = 256/48000 # 5.3ms`, `pulse.default.req = 960/48000 # 20 milliseconds`, `pulse.min.frag = 256/48000`, `pulse.default.frag = 96000/48000 # 2 seconds`, `pulse.default.tlength = 96000/48000`, `pulse.min.quantum = 256/48000 # 5.3ms` | A16 |
| PulseAudio-Kompat Client-Seite | `PULSE_LATENCY_MSEC` "controls buffering logic in libpulse" | A17 |
| ALSA-Kompat | Plugin-Optionen `alsa.rate` (0 = alle), `alsa.format`, `alsa.period-bytes` (0 = beliebig); Mapping Period → `node.latency` in der gelesenen Doku nicht beschrieben (**unbelegt**) | A15 |
| Quantum-Auswahlalgorithmus | In A12/A13/A20/A21 nicht als Algorithmus beschrieben; belegt ist nur "this latency or less" (A12) → Graph läuft mit dem kleinsten angeforderten Quantum ≥ min-quantum (Folgerung, **unbelegt** als Zitat) | A12 |
| Drift-Kompensation zwischen Geräten | Nur `api.alsa.dll-bandwidth-max` (Delay-Locked-Loop-Filter für Ratenanpassung) in A12 erwähnt; Mechanismus im Detail **unbelegt** | A12 |

### Rust-Crates I/O

| Crate | Version | Lizenz | Befund | Quelle |
| --- | --- | --- | --- | --- |
| `cpal` | 0.18.2 (2026-08-16) | Apache-2.0 | Hosts: Linux ALSA (default), JACK, PipeWire, PulseAudio (Features); Windows WASAPI (default), ASIO; "Default host priority: PipeWire > PulseAudio > ALSA" seit 0.18.0 (wenn Features aktiv) | A25, A27, A28, A33 |
| cpal WASAPI | — | — | Quellcode enthält **nur** `AUDCLNT_SHAREMODE_SHARED`, kein `EXCLUSIVE`, kein `IAudioClient3`/`InitializeSharedAudioStream`; Flags `EVENTCALLBACK` + Output `AUTOCONVERTPCM | SRC_DEFAULT_QUALITY`; `BufferSize::Fixed` wird zu `hnsBufferDuration` (100-ns) umgerechnet, `Default` → 0. Mit `hnsPeriodicity = 0` im Shared-Modus ist die Engine-Periode nicht steuerbar (A6/A7): Default 10 ms, außer ein anderer Prozess senkt sie per IAudioClient3 für den Endpoint (A3) → cpal kann die kleine Periode nicht selbst anfordern | A29, A6, A3 |
| cpal WASAPI 0.18 | — | — | "Default output and input streams now automatically reroute when the system default device changes"; "Timestamps now include hardware pipeline latency"; 0.18.2: "Reported buffer sizes are no longer off by one frame" | A28 |
| cpal PipeWire | — | — | `BufferSize::Fixed(n)` → `NODE_LATENCY = "n/rate"`, `NODE_RATE = "1/rate"`, `NODE_GROUP = "cpal-<pid>"`, `TARGET_OBJECT` = object.serial; `SupportedBufferSize::Range{min_quantum, max_quantum}`; 0.18.2 fixt "real-time promotion blocking the audio callback thread" | A31, A28 |
| cpal ALSA | — | — | Kommentar: "When BufferSize::Fixed(x) is specified, cpal attempts to configure the period size to approximately x frames"; bei Default "does NOT set explicit period size or period count constraints" | A30 |
| cpal Hot-Plug | — | — | WASAPI: automatisches Rerouting bei Default-Wechsel; CoreAudio `ErrorKind::DeviceChanged`; JACK `DeviceNotAvailable`; PipeWire-Host: nichts Explizites im Changelog (**unbelegt**) | A28 |
| `pipewire` (pipewire-rs) | 0.10.1 (2026-08-19) | MIT | Bindings zu libpipewire; Typen `MainLoop`, `Context`, `Core`, `Registry`, `stream::Stream`, `properties!()`; Build braucht `pkg-config` und `clang` (bindgen); Mindest-libpipewire-Version in gelesener Doku nicht genannt (**unbelegt**); docs.rs-Build 0.10.1 fehlgeschlagen, offizielle Doku auf Pages | A22, A23, A24 |
| Host-Voraussetzung | `pipewire-devel`, `alsa-lib-devel` fehlen auf dem Host | A50 |
| `windows` (windows-rs) | — | — | cpal nutzt `windows` ≥ 0.61 für WASAPI (A28); direkte Nutzung von `IAudioClient3` damit möglich (API existiert, A4/A5); eigene Verifikation der Rust-Bindings **unbelegt** | A28 |

### Resampling

| Fakt | Wert | Quelle |
| --- | --- | --- |
| Wer resampelt | PipeWire: Graph immer float32, Node-Resampler (A12); WASAPI shared: `AUTOCONVERTPCM` (A8) → App kann 48 kHz mono f32 verlangen und die Engine konvertiert | A12, A8 |
| rubato 5.0.0 (2026-08-10, MIT/Apache-2.0) | `Async` (sinc oder polynomial, `set_resample_ratio_relative`, Grenze `max_resample_ratio_relative`), `Fft` (synchron, feste Ratio), `Slip` ("passes samples through and occasionally inserts or drops a single frame to keep two near-identical clocks in step, hiding each correction behind a short crossfade", für "a few ppm") ; `output_delay()` liefert Latenz | A34, A35, A36 |

### DSP-Kette

| Baustein | Befund | Quelle |
| --- | --- | --- |
| RNNoise | "noise suppression library based on a recurrent neural network"; arbeitet auf "RAW 16-bit (machine endian) mono PCM files sampled at 48 kHz"; `rnnoise_process_frame(st, out, in)` gibt `float` zurück, Puffer ≥ `rnnoise_get_frame_size()`; Lizenz BSD (Header: BSD-2-Clause-Text, Repo: BSD-3-Clause-Anzeige) | A41 |
| `nnnoiseless` 0.5.2 (2025-12-18) | "a (safe) rust port of the RNNoise C library"; BSD-3-Clause; `FRAME_SIZE = 120 << 2` = **480 Samples = 10 ms @ 48 kHz**, `WINDOW_SIZE = 2*FRAME_SIZE` (960); `process_frame(&mut self, output: &mut [f32], input: &[f32]) -> f32`, Ein-/Ausgabe "assumed to be in 16-bit, 48kHz signed PCM format" mit Wertebereich [-32768, 32767]; "you might prefer to discard the very first output; it will contain some fade-in artifacts" | A37, A39, A40 |
| RNNoise-Rückgabewert | `rnnoise_process_frame` endet mit `return vad_prob;` (`vad_prob` aus `compute_rnn(&st->model, &st->rnn, g, &vad_prob, features, …)`), also die VAD-Wahrscheinlichkeit des Netzes; Header dokumentiert das nicht | A41 (`src/denoise.c`) |
| RNNoise-Latenz | Verarbeitung blockweise 480 Samples; ob die 960er-Analysefenster eine zusätzliche Ausgabeverzögerung von 10 ms erzeugen: in Quellen nicht beziffert → **unbelegt**, Step 3 |
| `webrtc-audio-processing` 2.1.0 (2026-05-13) | Wrapper um "PulseAudio's repackaging of WebRTC's AudioProcessing module" (`webrtc-audio-processing-2`, ≥ 2.1); AEC, NS, AGC, VAD; `license-file = "COPYING"` (crates.io: "Non-standard"); Build `bundled`: meson + ninja + C++-Compiler + bindgen (clang), abseil ≥ 20240722 via pkg-config oder als Meson-Subproject; Symbol-Prefixing per `objcopy`; docs.rs-Build fehlgeschlagen; Windows-Support nicht dokumentiert (**unbelegt**) | A42, A43, A44 |
| `webrtc-vad` | 0.4.0, letzte Version 2019-10-01, MIT | A45 |
| `earshot` 1.2.2 (2026-08-19) | "streaming voice activity detection, written in pure Rust"; 16 ms Frames à 256 Samples bei 16 kHz; "RTF of 0.0003"; ~8 KiB je Detector; MIT/Apache-2.0; `no_std` möglich | A46, A47 |
| `voice_activity_detector` 0.2.1 | Silero VAD v5 über `ort` 2.0.0-rc.10; "downloads prebuilt ONNX Runtime from Microsoft" (oder `load-dynamic`); 8/16 kHz, 256/512-Sample-Chunks; Lizenz crates.io "Non-standard" | A48, A49 |
| Windows-OS-DSP | Modus Communications kann OEM-Effekte AEC/NS/AGC/Beamforming enthalten; Anwendung sieht nicht, welcher Modus läuft, nur RAW/non-RAW; Deep Noise Suppression ab Windows 11 24H2 als eigener Effekt | A11 |

## Entscheidung

**Revidiert 2026-09-20 (AEC).** Die Zeile „AEC | keine in v1" unten ist überholt. Der Auftraggeber hat die Headset-Annahme gekippt: AEC muss für Lautsprecher-Nutzer sehr gut funktionieren. Die vollständige Recherche samt Kandidatenvergleich, Primärquellen und Messungen steht in `aec.md`; Kurzfassung: In-Process-AEC3 über `webrtc-audio-processing` ~2.1 (`bundled`) als **erste** Stufe der Capture-Kette, vor RNNoise, mit 10-ms-/480-Sample-Frames, ohne `set_stream_delay_ms` und ohne AGC. Entscheidend ist das Referenzsignal: ein In-Process-AEC sieht nur den eigenen Render-Stream, nicht den Spielton — mit unvollständiger Referenz bleiben 7,7 dB statt 38,8 dB Echodämpfung übrig. Unter Linux liefert deshalb ein zweiter Capture-Stream mit `stream.capture.sink = true` den Systemmix (gleicher Graph-Clock, kein Drift, kein Root). Die Entscheidungen zu Backend, Frame-Größe, VAD und Noise Suppression bleiben davon unberührt; RNNoise bleibt der Denoiser und läuft **nach** dem AEC. Kosten: +8,96 ms im Sendepfad, 0,47 % eines Kerns.

| Punkt | Entscheidung | Begründung |
| --- | --- | --- |
| Windows-Modus | **WASAPI shared, event-driven, IAudioClient3 `InitializeSharedAudioStream`** mit `PeriodInFrames` = kleinstes Vielfaches der Fundamental-Periode ≥ max(MinPeriod, 240 Frames = 5 ms); Fallback `IAudioClient::Initialize` shared (10 ms) | Exclusive nimmt dem Spiel den Endpoint (A1, A3) — Ausschluss unabhängig von der Latenz; IAudioClient3 liefert "latency comparable to exclusive mode while preserving system sound mixing" (A1); 5 ms statt Treiber-Minimum, um nicht alle Apps am Endpoint auf 2.66 ms zu zwingen (A3) |
| Windows-Format/Optionen | IAudioClient3-Pfad: Mix-Format der Engine unverändert übernehmen (einziges erlaubtes Flag ist `EVENTCALLBACK`, A5); ist das Mix-Format ≠ 48 kHz, in der App resampeln (`rubato::Fft`, feste Ratio) oder `MATCH_FORMAT` versuchen (kann `ENGINE_FORMAT_LOCKED` liefern, A5/A9); `AUTOCONVERTPCM | SRC_DEFAULT_QUALITY` nur im 10-ms-Fallback über `IAudioClient::Initialize` (A8); Capture-Kategorie `AudioCategory_Other` + `AUDCLNT_STREAMOPTIONS_RAW` wenn `RawProcessingSupported`, sonst `Other` ohne RAW; Render-Kategorie `AudioCategory_GameChat` | RAW verhindert OEM-AEC/NS/AGC, die unsere Kette doppeln würden (A11); `Communications` würde andere Streams dämpfen ("pause background music", A11), `GameChat` nicht (A10) |
| Linux-Backend | **PipeWire nativ** über `pipewire` 0.10.1; Stream-Properties `node.latency = 240/48000`, `node.rate = 1/48000`, `node.group` für Capture+Playback, `media.category` Capture/Playback, `media.role` Communication | Native API ist die einzige mit dokumentierter Latenz-Anforderung je Node (A12); PulseAudio-Kompat hat Mindestquantum 256/48000 = 5.3 ms und Default-Req 20 ms (A16); ALSA-Plugin-Mapping unbelegt (A15) |
| cpal | **nicht als Primärpfad**; erlaubt als Step-3-Baseline und Fallback-Host | WASAPI-Host ohne IAudioClient3/Exclusive → 10 ms Periode fix (A29, A6); PipeWire-Host setzt `node.latency` korrekt (A31), aber ein einheitlicher Backend-Abstraktionslayer bringt nichts, wenn Windows ohnehin eigen implementiert wird |
| Sample-Rate | 48 kHz mono f32 im Client, keine eigene SRC im Normalfall | PipeWire-Graph float32 mit eigenem Resampler (A12), WASAPI `AUTOCONVERTPCM` (A8) |
| Resampling-Crate | `rubato` 5.0 nur für **Peer-Drift** (`Slip`) und für den Fall, dass ein Backend kein 48 kHz liefert (`Async`) | A34, A35; Details in `latency-budget.md` |
| Geräteauswahl | Enumeration je Backend (PipeWire Registry `Audio/Source`/`Audio/Sink`; Windows MMDevice), Auswahl per persistentem Namen; "Default"-Option folgt dem System-Default | A12 (`media.class`), A2 |
| Hot-Plug | Windows: Default-Endpoint-Wechsel folgen (Rerouting wie cpal 0.18, A28) — Implementierung über `IMMNotificationClient` (**unbelegt**, Step 3); PipeWire: Registry-Events, `target.object` neu setzen | A28, A12 |
| VAD | **RNNoise-VAD-Ausgang** (wenn NS aktiv) mit Schwellwert + Hangover (200 ms) + Hysterese; ohne NS: **Energie-Gate** (RMS über 10 ms, Schwellwert in dBFS, gleiches Hangover) | Kein zusätzliches Modell/Resampling; `earshot` bräuchte 16 kHz/16-ms-Frames (A46); Silero bräuchte onnxruntime-Download (A48) |
| Noise Suppression | **`nnnoiseless`, per Nutzer-Schalter, Default an** | Pure Rust, BSD-3, 10-ms-Frames passen zu 480-Sample-Blöcken (A37); OS-NS wird via RAW abgeschaltet |
| AEC | **`webrtc-audio-processing` ~2.1 (AEC3) in-process, Default an**, Referenz = Systemmix (Linux: zweiter Capture-Stream mit `stream.capture.sink = true`; Windows: WASAPI-Loopback, Pfad noch offen) | **Revidiert 2026-09-20 (AEC)**, Herleitung und Messungen in `aec.md` |
| AGC | **keine**; nur Soft-Clip/Limiter nach dem Mix | AGC verändert Dynamik ("glasklar" = unverfälscht); Nutzer stellt Mikrofon-Gain |
| Minimale Kette | Capture 48 kHz mono f32 → Blöcke à 480 → [nnnoiseless] → VAD/PTT-Gate → Opus-Encoder (VOIP hat eigenen HPF, Q4 in `codec.md`); Empfang: N−1 Decoder → Summe f32 → Limiter → Playback | — |

## Verworfen, mit Grund

| Verworfen | Grund |
| --- | --- |
| WASAPI exclusive | Entzieht anderen Apps den Endpoint (A1, A3); Nutzer kann es je Gerät sperren; Microsoft rät zu IAudioClient3 (A1) |
| ASIO | Exclusive-Semantik, Fremd-Treiber, SDK-Lizenz (A27) |
| PulseAudio-Kompat (pipewire-pulse) | Mindestquantum 256/48000, Default-Req 960/48000 (A16); indirekter Pfad |
| ALSA-Kompat (pcm_pipewire) | Period→Latenz-Mapping in Doku nicht belegt (A15); zusätzliche Schicht ohne Nutzen |
| cpal als Primärpfad | WASAPI nur Shared-10-ms (A29, A6) |
| `webrtc-vad` | Letzte Version 2019 (A45) |
| `voice_activity_detector` (Silero) | onnxruntime-Binary-Download/Dynamic-Load (A48), Lizenz "Non-standard" (A49) |
| `earshot` | 16 kHz/16-ms-Raster passt nicht zum 48-kHz/10-ms-Raster; RNNoise liefert VAD mit |
| ~~`webrtc-audio-processing`~~ | **Revidiert 2026-09-20 (AEC)**: nicht mehr verworfen, sondern Entscheidung — siehe `aec.md`. Der schwere Build ist real (meson/ninja/clang/abseil plus ein objcopy-Workaround), der Windows-Cross-Build scheitert tatsächlich; beides ist dort dokumentiert. |
| Windows `AudioCategory_Communications` | Dämpft andere Streams (A11), zieht OEM-APOs (A11) |
| OS-seitige NS/AGC | Nicht kontrollierbar, App sieht Modus nicht (A11) |

## Offene Punkte für Step 3

| # | Prüfung | Messvorschrift |
| --- | --- | --- |
| I1 | PipeWire Roundtrip-Latenz nativ | Loopback-Kabel oder Lautsprecher→Mikrofon; Impuls senden, Capture-Timestamp vs. Playback-Timestamp; `node.latency` 128/240/480/1024 je Messreihe; `pw-top` zeigt Quantum; Ziel ≤ 2 × Quantum + Hardware |
| I2 | pipewire-rs 0.10.1 baut gegen PipeWire 1.4.11 | `pipewire-devel` installieren, `cargo build`; Mindestversion damit belegt |
| I3 | cpal-PipeWire-Host als Vergleich | Gleiche Messung wie I1 mit `BufferSize::Fixed(240)`; Differenz zur nativen Lösung dokumentieren |
| I4 | RNNoise-Latenz | Impuls (1 Sample) durch `nnnoiseless` schicken, Position des Ausgabemaximums in Samples messen; erwartet 0 oder 480 |
| I5 | RNNoise-VAD-Ausgang praktisch brauchbar | Stille vs. Sprache vs. Tastaturklappern: Rückgabewert protokollieren, Verteilung und Schwellwert (Start 0.5) prüfen |
| I6 | Windows IAudioClient3 (später, echte Hardware) | `GetSharedModeEnginePeriod` und Mix-Format (`GetMixFormat`) auf Zielhardware protokollieren; Roundtrip mit 240 vs. Default-Periode; `RawProcessingSupported` prüfen; Verhalten von `MATCH_FORMAT` bei 48 kHz mono |
| I7 | Hot-Plug | Headset während Stream abziehen/anstecken; erwartet: Stream endet mit Fehler, Neuaufbau ≤ 1 s auf neuen Default |
| I8 | CPU der Kette | nnnoiseless + Opus-Encode je 10/20 ms; Ziel < 5 % eines Kerns |
| I9 | Windows `windows`-Crate Bindings für `IAudioClient3` | Cross-Build mit mingw prüfen (`windows` Crate Feature `Win32_Media_Audio`) |

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `verification.md`):
  - Spike 3 **bestätigt** die cpal-Einschränkung empirisch: cpal 0.16 (ALSA-Host über PipeWire-Plugin) ignoriert `BufferSize::Fixed(480)`; Callback-Periode kommt vom PipeWire-Quantum (128 Samples default, per `PIPEWIRE_LATENCY` steuerbar). Geräte nur als `default` sichtbar. Native PipeWire-Anbindung ist damit für Geräteauswahl und Latenzsteuerung Pflicht (I3 beantwortet).
  - Spike 3 **bestätigt** I8: nnnoiseless 0.5.2 kostet 1,12–1,16 ms je 10-ms-Frame (11 % eines Kerns), Opus-Encode 0,1 ms → Kette < 15 % eines Kerns.
  - Spike 3 **beobachtet**: Software-Pipeline Capture-Callback → Playback-Callback 14 ms p50 bei 10-ms-Frames, ohne Backend-Puffer; ohne Playout-Steuerung baut sich beim Start ein Rückstand von ~50 ms auf, der nie abgebaut wird (Jitter-Buffer mit Ziel-Delay ist Pflicht).
  - `pipewire-devel` und `alsa-lib-devel` sind seit 2026-09-20 installiert (I2 vorbereitet, Build von pipewire-rs noch nicht ausgeführt).
  - Offen: I1 (Roundtrip mit Kabel), I4 (RNNoise-Impulsantwort), I5, I6/I9 (Windows), I7 (Hotplug).

Per WebFetch gelesen (2026-09-20): A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A32, A33, A34, A35, A36, A37, A38, A39 (README), A40, A41 (Repo-Seite, README, rnnoise.h), A42, A43, A44, A45, A46, A47, A48, A49.

Per `curl` im Volltext gelesen und mit `grep`/`sed` zitiert: A29 (device.rs, ergänzend), A30, A31, A39 (`src/lib.rs`), A41 (`rnnoise.h` ergänzend, `src/denoise.c`).

Nicht erreichbar: https://gitlab.freedesktop.org/pipewire/pipewire/-/wikis/Config-ALSA und …/Config-PipeWire (Anubis-Blockade), https://docs.pipewire.org/page_man_pipewire-alsa_conf_5.html (404).
