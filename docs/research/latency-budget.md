# Latenzbudget: Mouth-to-Ear, Jitter-Buffer, Clock-Drift

## Frage

Wie setzt sich die Mouth-to-Ear-Latenz aus Capture-Puffer, Opus-Frame, Encoder-Lookahead, RNNoise, Encode, Netz, Jitter-Buffer, Decode, Mix und Playback-Puffer zusammen, welche Zielwerte sind für LAN und WAN (best/typisch) realistisch bei 10 ms vs. 20 ms Frames, welches Jitter-Buffer-Design ist für v1 richtig, und wie werden Clock-Drift zwischen Capture und Playback sowie zwischen Peers erkannt und kompensiert?

## Kandidaten

| Thema | Kandidaten |
| --- | --- |
| Frame-Größe | 10 ms, 20 ms |
| Jitter-Buffer | (a) fester Puffer (k Frames); (b) adaptiv nach Jitter-Statistik (RFC-3550-Schätzer oder Speex/Mumble-Kostenfunktion); (c) NetEQ-Stil: Histogramm der relativen Ankunftsverzögerung + Time-Stretching (Accelerate/Preemptive Expand) |
| Drift-Kompensation | (a) Frames einfügen/verwerfen in Sprechpausen; (b) PLC/Skip nach RFC 6716 §4.4.1; (c) asynchroner Resampler (`rubato::Slip`/`Async`); (d) Time-Stretching |

## Primärquellen

Abrufdatum aller Quellen: 2026-09-20.

| # | Quelle | URL |
| --- | --- | --- |
| L1 | RFC 3550 §6.4.1 Interarrival Jitter, §5.1 Header | https://www.rfc-editor.org/rfc/rfc3550 |
| L2 | RFC 6716 §2 (Delay), §4.4 PLC, §4.4.1 Clock Drift, §4.2.5 LBRR (Volltext) | https://www.rfc-editor.org/rfc/rfc6716 |
| L3 | libopus `src/opus_encoder.c` (Lookahead) | https://raw.githubusercontent.com/xiph/opus/main/src/opus_encoder.c |
| L4 | libopus 1.5 API Decoder (`decode_fec`, PLC) | https://opus-codec.org/docs/opus_api-1.5/group__opus__decoder.html |
| L5 | WebRTC NetEQ g3doc | https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/g3doc/index.md |
| L6 | WebRTC NetEQ `delay_manager.cc` / `.h` | https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/delay_manager.cc , https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/delay_manager.h |
| L7 | WebRTC NetEQ `underrun_optimizer.cc` | https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/underrun_optimizer.cc |
| L8 | WebRTC `api/neteq/neteq.h` (Config-Defaults) | https://webrtc.googlesource.com/src/+/refs/heads/main/api/neteq/neteq.h |
| L9 | Speex DSP `libspeexdsp/jitter.c` (von Mumble verwendeter Jitter-Buffer) | https://raw.githubusercontent.com/xiph/speexdsp/master/libspeexdsp/jitter.c |
| L10 | Mumble `src/mumble/AudioOutputSpeech.cpp`, `.h`, `Settings.h` | https://raw.githubusercontent.com/mumble-voip/mumble/master/src/mumble/AudioOutputSpeech.cpp , https://raw.githubusercontent.com/mumble-voip/mumble/master/src/mumble/AudioOutputSpeech.h , https://raw.githubusercontent.com/mumble-voip/mumble/master/src/mumble/Settings.h |
| L11 | ITU-T G.114 Landing-/Summary-Seite | https://www.itu.int/ITU-T/recommendations/rec.aspx?rec=6254&lang=en |
| L12 | Microsoft Learn: Low Latency Audio | https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/low-latency-audio |
| L13 | Microsoft Learn: IAudioClient3::GetSharedModeEnginePeriod | https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient3-getsharedmodeengineperiod |
| L14 | PipeWire pipewire.conf(5), pipewire-props(7) | https://docs.pipewire.org/page_man_pipewire_conf_5.html , https://docs.pipewire.org/page_man_pipewire-props_7.html |
| L15 | docs.rs nnnoiseless DenoiseState | https://docs.rs/nnnoiseless/latest/nnnoiseless/struct.DenoiseState.html |
| L16 | rubato README / docs.rs | https://raw.githubusercontent.com/HEnquist/rubato/master/README.md , https://docs.rs/rubato/latest/rubato/ |
| L17 | RFC 9221 (Datagram, unzuverlässig) | https://www.rfc-editor.org/rfc/rfc9221 |
| L18 | Eigene Entscheidungen | `docs/research/codec.md`, `docs/research/audio-io.md` |
| L19 | RFC 7587 §3.1.3/DTX (Unterscheidung Pause vs. Verlust über Sequenznummern) | https://www.rfc-editor.org/rfc/rfc7587 |

## Befunde

### Einzelbeiträge (Fakten)

| Stufe | Wert | Quelle |
| --- | --- | --- |
| Capture-Puffer Windows shared default | 10 ms ("by default all applications … use 10-ms buffers") | L12 |
| Capture-Puffer Windows IAudioClient3 | Treiber-Minimum, Inbox-HDAudio 128 Samples = 2.66 ms; Beispiel 44.1 kHz min 48 Frames = 1.09 ms; Engine-Latenz Capture ~0 ms, Render 1.3 ms (Win10+) | L12, L13 |
| Capture-/Playback-Puffer Linux PipeWire | Quantum frei wählbar via `node.latency` ("this latency or less"), Defaults quantum 1024 (21.3 ms), min 32 (0.67 ms) bei 48 kHz | L14 |
| Opus-Frame | 10 oder 20 ms (Frame muss voll sein, bevor encodiert wird) | L2 §2 |
| Encoder-Lookahead | `Fs/400 + Fs/250` = 312 Samples = 6.5 ms bei 48 kHz (VOIP/AUDIO) | L3 |
| RNNoise | Blockgröße 480 Samples = 10 ms; zusätzliche algorithmische Verzögerung **unbelegt** | L15 |
| Encode-/Decode-Rechenzeit | keine Primärquelle mit Zahlen → **unbelegt**; Annahme < 1 ms je Frame auf Desktop-CPU |
| Netz | LAN ~1 ms, WAN 20–40 ms einfach (Vorgabe der Aufgabenstellung, keine Quelle) | Annahme |
| QUIC-Datagramme | keine Retransmission → keine Retransmit-Latenz, Verlust muss PLC/FEC abfangen | L17 |
| FEC-Kopplung | LBRR für Frame n liegt in Paket n+1 (L2 §4.2.5); `decode_fec=1` braucht dieses Paket (L4) → FEC nutzbar nur bei Jitter-Buffer-Tiefe ≥ 1 Frame | L2, L4 |
| PLC-Granularität | `frame_size` bei PLC/FEC "must be a multiple of 2.5 ms" | L4 |
| Pause vs. Verlust | "A receiver can distinguish between DTX and packet loss by looking for gaps in the sequence number." — gilt analog für PTT-Pausen | L19 |
| Mix | Addition von N−1 f32-Puffern, vernachlässigbar (< 0.1 ms, Annahme) | — |
| G.114 | "it is recommended that a one-way delay of 400 ms should not be exceeded for general network planning … highly interactive tasks … can be affected by much lower delays"; der oft zitierte 150-ms-Wert steht nicht auf der gelesenen Seite → **unbelegt** | L11 |

### Jitter-Buffer-Referenzen

| System | Mechanismus | Quelle |
| --- | --- | --- |
| RFC 3550 | Interarrival Jitter `D(i,j) = (Rj − Ri) − (Sj − Si)`, `J(i) = J(i−1) + (|D(i−1,i)| − J(i−1))/16`; "The gain parameter 1/16" glättet | L1 |
| Speex/Mumble | `jitter_buffer_init(step)` mit `delay_step = concealment_size = step`, `buffer_margin = 0`, `late_cutoff = 50`, `auto_adjust = 1`; Kostenfunktion `cost = delay + late_factor·[Frames, die bei diesem Delay zu spät wären]`, ausgewertet über die `TOP_DELAY = 40` spätesten Timings in `MAX_BUFFERS = 3` Subfenstern; `window_size = 100·TOP_DELAY/max_late_rate` | L9 |
| Mumble | `jitter_buffer_init(iFrameSize)` mit `iFrameSize = iSampleRate/100` (10 ms); `margin = iJitterBufferSize · iFrameSize`, Default `iJitterBufferSize = 1` (10 ms); `iFramesPerPacket = 2` (20 ms je Paket); `jitter_buffer_update_delay` wird bevorzugt "when quiet" aufgerufen (Anpassung in Sprechpausen); Rückgabe ≠ `JITTER_BUFFER_OK` → PLC | L10 |
| WebRTC NetEQ | "adaptive jitter buffer … continuously optimized based on the network conditions"; Ausgaben: Normal, Acceleration, Preemptive expand, Expand (PLC), Merge, CNG; `GetAudio` liefert 10 ms; Delay Manager: `kStartDelayMs = 80`, `quantile = 0.95`, `forget_factor = 0.983`, `reorder_forget_factor = 0.9993`; Underrun Optimizer: Histogramm mit `kDelayBuckets = 100`, `kBucketSizeMs = 20`, `optimal_delay_ms = (1 + bucket_index)·20` am Quantil; `target = max(underrun, reorder)`; Config-Defaults `max_packets_in_buffer = 200`, `min_delay_ms = 0`, `max_delay_ms = 0` | L5, L6, L7, L8 |

### Clock-Drift-Referenzen

| Fakt | Quelle |
| --- | --- |
| "Clock drift refers to the gradual desynchronization of two endpoints whose sample clocks run at different frequencies"; Sender langsamer → wirkt wie Verlust, "A receiver for live streams SHOULD conceal the effects of drift, and it MAY do so by invoking the PLC"; Sender schneller → "The receiver MAY respond by skipping any packet (i.e., not submitting the packet for decoding). This is likely to produce a less severe artifact than if the frame were dropped after decoding." | L2 §4.4.1 |
| `rubato::Slip`: fügt einzelne Frames ein oder verwirft sie mit Crossfade, für Raten "very close to 1.0"; `Async` mit `set_resample_ratio_relative` für laufende Anpassung | L16 |
| PipeWire: Capture und Playback im selben Graph; `node.group` bewirkt Scheduling im selben Quantum (cpal-Kommentar: "preventing phase drift between simultaneous input/output streams"); Ratenanpassung von Geräten via DLL (`api.alsa.dll-bandwidth-max`) — Mechanismus im Detail unbelegt | L14, `audio-io.md` A31 |
| Windows shared: Engine bedient beide Endpunkte mit eigener Periodik; App-seitig kein Drift zwischen Capture- und Render-Events dokumentiert (**unbelegt**) | L12 |

### Budget-Tabelle (ms, einfache Richtung Mund → Ohr)

Spalten: LAN best (PipeWire 5-ms-Quantum bzw. IAudioClient3 240 Frames, NS aus, Jitter-Buffer minimal), LAN typisch (10-ms-Puffer, NS an, 1 Frame Jitter-Buffer), WAN best, WAN typisch. Werte ohne Quelle sind Annahmen und so markiert.

| Stufe | 20 ms LAN best | 20 ms LAN typ. | 20 ms WAN best | 20 ms WAN typ. | 10 ms LAN best | 10 ms LAN typ. | 10 ms WAN best | 10 ms WAN typ. | Beleg |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Mikrofon/ADC/Treiber | 1 | 2 | 1 | 2 | 1 | 2 | 1 | 2 | Annahme (unbelegt) |
| Capture-Puffer | 5 | 10 | 5 | 10 | 5 | 10 | 5 | 10 | L12, L13, L14 |
| AEC (WebRTC AEC3) | 9 | 9 | 9 | 9 | 9 | 9 | 9 | 9 | **Revidiert 2026-09-20 (AEC)**: gemessen 8,96 ms (Impulsantwort, Spike 6); nur bei aktivem AEC, sonst 0 |
| RNNoise-Block (10 ms) | 0 | 10 | 0 | 10 | 0 | 10 | 0 | 10 | L15; 10 = konservative Annahme für Fensterverzögerung (unbelegt) |
| Opus-Frame füllen | 20 | 20 | 20 | 20 | 10 | 10 | 10 | 10 | L2 |
| Encoder-Lookahead | 6.5 | 6.5 | 6.5 | 6.5 | 6.5 | 6.5 | 6.5 | 6.5 | L3 |
| Encode-Zeit | 0.5 | 1 | 0.5 | 1 | 0.5 | 1 | 0.5 | 1 | Annahme (unbelegt) |
| Netz Client→Server→Client | 1 | 2 | 20 | 40 | 1 | 2 | 20 | 40 | Vorgabe |
| Relay-Verarbeitung | 0.2 | 0.5 | 0.2 | 0.5 | 0.2 | 0.5 | 0.2 | 0.5 | Annahme (unbelegt) |
| Jitter-Buffer (Ziel) | 5 | 20 | 20 | 40 | 5 | 10 | 10 | 30 | Design, siehe unten |
| Decode-Zeit | 0.3 | 1 | 0.3 | 1 | 0.3 | 1 | 0.3 | 1 | Annahme (unbelegt) |
| Mix | 0.1 | 0.1 | 0.1 | 0.1 | 0.1 | 0.1 | 0.1 | 0.1 | Annahme |
| Playback-Puffer | 5 | 10 | 5 | 10 | 5 | 10 | 5 | 10 | L12, L14 |
| DAC/Treiber | 1 | 2 | 1 | 2 | 1 | 2 | 1 | 2 | Annahme (unbelegt) |
| **Summe ohne AEC** | **45.6** | **85.1** | **79.6** | **143.1** | **35.6** | **65.1** | **59.6** | **123.1** | abgeleitet |
| **Summe mit AEC** | **54.6** | **94.1** | **88.6** | **152.1** | **44.6** | **74.1** | **68.6** | **132.1** | abgeleitet (+9 ms) |

**Revidiert 2026-09-27 (Readiness-Review, aus dem Code gerechnet; `docs/reference/client.md` §6):** Für den gebauten Client stimmen zwei Zeilen nicht. (1) Jitter-Buffer: das Ziel ist nie kleiner als 1 Frame (FEC-Untergrenze, seit 2026-09-26 immer) plus ein 5-ms-Bucket (das Quantil nennt die Obergrenze seines Buckets), also ≥ 25 ms mit 20-ms-Frames und ≥ 15 ms mit 10 ms statt 5. (2) Es fehlt der Wiedergabe-Vorlauf des Mixers: 2 Blöcke, im Mittel ≈ 21,5 ms bis zum Wiedergabe-Callback (5-ms-Quantum, 2-ms-Mixertakt), zusätzlich zum Playback-Puffer des Geräts. Damit „LAN best“ ≈ 87 ms ohne / ≈ 96 ms mit AEC (20-ms-Frames) und ≈ 67 / 76 ms (10-ms-Frames); die Empfangsseite allein (Jitter-Ziel + Vorlauf) ≈ 47 ms. Jitter-Untergrenze und Vorlauf werden nicht gesenkt, der stotterfreie Stand vom 2026-09-26 hängt an ihnen; B4 bleibt offen.

Ableitung 10 ms vs. 20 ms: der Frame-Wechsel spart genau 10 ms Frame-Füllzeit plus feinere Jitter-Buffer-Schritte (5–10 ms weniger Ziel-Tiefe bei gleicher Jitter-Toleranz in Frames), bei +35 % Draht-Bandbreite (siehe `codec.md`). Der Lookahead bleibt 6.5 ms (L3).

Zielwerte:

| Szenario | Ziel v1 ohne AEC | Ziel v1 mit AEC | Herleitung |
| --- | --- | --- | --- |
| LAN, 20 ms | ≤ 60 ms best, ≤ 90 ms typisch | ≤ 69 ms best, ≤ 99 ms typisch | Tabelle; **gebaut ≈ 87 / ≈ 96 ms best** (2026-09-27, s. o.) |
| LAN, 10 ms | ≤ 45 ms best, ≤ 70 ms typisch | ≤ 54 ms best, ≤ 79 ms typisch | Tabelle; **gebaut ≈ 67 / ≈ 76 ms best** (2026-09-27, s. o.) |
| WAN (20–40 ms Netz), 20 ms | ≤ 85 ms best, ≤ 150 ms typisch | ≤ 94 ms best, ≤ 160 ms typisch | Tabelle; unter G.114-Planungsgrenze 400 ms (L11) |
| WAN, 10 ms | ≤ 65 ms best, ≤ 130 ms typisch | ≤ 74 ms best, ≤ 140 ms typisch | Tabelle |

### Jitter-Buffer-Design, Kandidaten

| Kandidat | Latenz | Robustheit | Aufwand | Bewertung |
| --- | --- | --- | --- | --- |
| (a) fest, k Frames | k·Frame konstant | keine Anpassung an WAN-Jitter; auf LAN unnötig groß oder auf WAN zu klein | minimal | nur als Debug-Modus |
| (b) adaptiv, Statistik der relativen Ankunftsverzögerung (Quantil), Anpassung in Sprechpausen | min. 1 Frame (für FEC) bis Quantil | Verluste durch Late-Packets = 1 − Quantil; Anpassung ohne Artefakte nur in Pausen (Mumble-Ansatz, L10) | gering | **v1** |
| (c) NetEQ-Stil mit Time-Stretching (Accelerate/Preemptive Expand, 10-ms-Ausgabe) | wie (b), aber Anpassung auch während Sprache | beste Qualität bei WAN-Schwankungen | hoch (WSOLA-artige Zeitdehnung, Merge) | v2 |

### Warum (b) für v1 reicht

- PTT/VAD erzeugt Sprech-Bursts mit Pausen; die Anpassung der Zieltiefe kann in jeder Pause artefaktfrei erfolgen (Mumble macht genau das: `jitter_buffer_update_delay` "when quiet", L10).
- FEC braucht ohnehin ≥ 1 Frame Tiefe (L2 §4.2.5, L4); die Untergrenze ist damit gesetzt.
- Auf LAN ist die gemessene Ankunftsstreuung klein; Quantil-Ziel fällt auf die Untergrenze.
- Time-Stretching ist erst nötig, wenn lange, pausenlose Sprache über stark schwankendes WAN läuft — Nicht-Ziel für v1.

## Entscheidung

**Revidiert 2026-09-20 (AEC).** Das Budget enthält seit heute eine Stufe „AEC (WebRTC AEC3)" mit **9 ms** im Sendepfad (gemessen 8,96 ms per Impulsantwort, Spike 6). Der Empfangspfad bleibt unverändert: `process_render_frame` verändert den Frame nicht. Die 9 ms fallen nur bei aktivem AEC an — für Headset-Nutzer, die es abschalten, gelten weiter die Werte ohne AEC. Die Zielwerte wurden entsprechend verdoppelt (mit/ohne), nicht angehoben: eine Zielverfehlung ist das nicht, sondern ein bewusst bezahlter Preis für Lautsprecherbetrieb. Alle Spalten bleiben weit unter der G.114-Planungsgrenze von 400 ms (L11). Herleitung und Messungen in `aec.md`.

Nebenbefund für die Drift-Zeilen unten: AEC3 toleriert Clock-Drift zwischen Mikrofon und Referenzsignal nur bis zu einer **Rate** von rund 100 ppm — der aufsummierte Schlupf ist dabei egal (100 ppm über 600 s kosten 0,4 dB, 1 000 ppm über 60 s bei identischem Schlupf brechen auf 8 dB ein). Für Linux ist das folgenlos, weil Mikrofon- und Monitor-Stream am selben PipeWire-Graph-Clock hängen. Für den Windows-Loopback-Pfad ist es eine Anforderung an die Hardware und in M3 zu messen (`aec.md` A6).

| Punkt | Entscheidung |
| --- | --- |
| Frame-Größe fürs Budget | 20 ms Default (siehe `codec.md`); 10 ms als Option; Step 3 misst beide gegen die Zielwerte |
| Puffer je OS | Capture/Playback je 5 ms: PipeWire `node.latency = 240/48000`; Windows IAudioClient3 `PeriodInFrames` ≥ 240 (bzw. Treiber-Minimum, wenn größer); Fallback 10 ms |
| Jitter-Buffer v1 | Kandidat (b): Paketpuffer je Sender-Stream, sortiert nach Sequenznummer; je Paket relative Ankunftsverzögerung `d = (R_i − R_0) − (S_i − S_0)` (aus RFC-3550-Definition, L1) mit S aus dem eigenen 4-Byte-Timestamp im Header (Sender-Sample-Clock, 48 kHz); Histogramm in 5-ms-Buckets über gleitendes Fenster mit Forget-Factor 0.983 (NetEQ-Wert, L6; **revidiert 2026-09-25 auf 0,997**, und der Anker `R_0` folgt der frühesten Ankunft des Bursts — Begründung und Messung in `docs/reference/client.md` §16); Ziel = max(Jitter-Term, Stall-Term) (**revidiert 2026-09-26**, vorher nur der Jitter-Term; Formel und Messung in `docs/reference/client.md` §6 und §16): Jitter-Term = 95 %-Quantil (L6) + 1 Frame; Stall-Term = höchste Verzögerung der letzten 20–40 s, die mehr als 1 Frame über dem Median lag, + 2 Frames (ein wiederkehrender Hänger wie beim WLAN-Stromsparen trifft nur 2–5 % der Pakete, das 95-%-Quantil sieht ihn also nicht); das Ziel geklemmt auf [Untergrenze, 200 ms] mit Untergrenze = 1 Frame solange `PACKET_LOSS_PERC > 0` (FEC braucht Paket n+1), sonst 0.25 Frame (5 ms; die LAN-best-Spalte gilt für diesen verlustfreien Fall) — **revidiert 2026-09-26/27:** jeder Sender codiert FEC ab 3 %, die Untergrenze ist also immer 1 Frame, mit dem 5-ms-Bucket des Quantils ≥ 1 Frame + 5 ms; den verlustfreien 5-ms-Fall gibt es nicht; Startwert 2 Frames; Anker `R_0/S_0` je Sprech-Burst neu setzen (erstes Paket nach Sequenz-kontinuierlichem Timestamp-Sprung), damit Peer-Drift die Statistik nicht linear aufbläht; Pause vs. Verlust: Timestamp-Sprung bei lückenloser Sequenz = PTT-Pause (kein PLC, Re-Anchor), Sequenzlücke = Verlust (FEC/PLC) (L19); Anpassung nur in Sprechpausen (kein Paket ≥ 2 Frames) oder durch Einfügen (PLC-Frame) / Verwerfen (Skip vor Decode, L2 §4.4.1) einzelner Frames |
| Verlust | fehlt Paket n und n+1 ist da → `opus_decode(n+1-Daten, decode_fec=1, frame_size=Frame)`; sonst `opus_decode(NULL, 0, …)` (PLC); Verluste in Folge > 100 ms → Stream stumm (Opus-Decoder-Zustand bleibt, kein Reset) |
| Clock-Drift Capture↔Playback (gleicher Host) | PipeWire: beide Streams in einer `node.group` am selben Graph-Clock, Geräte-Drift kompensiert der Graph (DLL) — keine eigene Maßnahme; Windows shared: Engine-getaktet, keine eigene Maßnahme; Beleg in Step 3 durch Zählen |
| Clock-Drift Peer↔Peer | Erkennung: Trend der Jitter-Buffer-Füllung je Sender (lineare Regression über 30 s der mittleren relativen Ankunftsverzögerung); Kompensation v1: Korrektur um genau 1 Frame in der nächsten Sprechpause (PTT-Bursts sind kurz: 100 ppm × 30 s = 3 ms < 1 Frame); ohne Pause (Dauersprecher) Einfügen eines PLC-Frames bzw. Skip eines Pakets (RFC 6716 §4.4.1); `rubato::Slip` als spätere Verfeinerung für einzelne Samples statt ganzer Frames |
| Messpunkte im Code | Zeitstempel an: Capture-Callback, Encode fertig, Send, Receive, Decode, Playback-Callback; Export als Histogramm, damit Step 3 die Tabelle mit Messwerten ersetzen kann |

## Verworfen, mit Grund

| Verworfen | Grund |
| --- | --- |
| Fester Jitter-Buffer | Kein Kompromiss zwischen LAN (≈ 0 Jitter) und WAN möglich |
| NetEQ-Stil Time-Stretching in v1 | Hoher Implementierungsaufwand; Nutzen erst bei pausenloser WAN-Sprache; NetEQ-Startwert 80 ms (L6) ist für LAN viel zu hoch |
| RFC-3550-EWMA (`J`) allein als Zielgröße | Mittelwert des |D| ist kein Quantil; Late-Rate nicht steuerbar; als Metrik fürs Monitoring aber verwendet |
| Speex/Mumble-Kostenfunktion 1:1 | Braucht Tuning von `late_factor`/`max_late_rate` (L9); Quantil-Histogramm ist direkter interpretierbar |
| Drift-Kompensation per asynchronem Resampler (`rubato::Async`) in v1 | CPU und Latenz (`output_delay`, L16) für einen Effekt von wenigen ppm, der bei PTT innerhalb eines Bursts unter 1 Frame bleibt |
| Retransmission über QUIC-Streams für Audio | Ein Retransmit kostet ≥ 1 RTT (WAN 40–80 ms) > Jitter-Buffer; Datagramme + FEC/PLC sind billiger (L17) |

## Offene Punkte für Step 3

| # | Prüfung | Messvorschrift |
| --- | --- | --- |
| B1 | Loopback-Latenz je OS/Backend (I1 in `audio-io.md`) | Impuls Playback→Capture, Zeitdifferenz aus Backend-Timestamps; Werte ersetzen Zeilen "Capture-Puffer", "Playback-Puffer", "ADC/DAC" |
| B2 | Encode/Decode-Zeit (C3 in `codec.md`) | Median/p99 in µs; ersetzt Zeilen "Encode-Zeit"/"Decode-Zeit" |
| B3 | RNNoise-Verzögerung (I4) | Impulsantwort in Samples; ersetzt Zeile "RNNoise-Block" |
| B4 | Ende-zu-Ende LAN | Zwei Clients + Server im LAN; Sender spielt Klick in sein Mikrofon-Signal (Loopback-Eingang), Empfänger misst Ausgabezeit; 100 Wiederholungen, Median/p95; Ziel ≤ 60 ms (20 ms Frames) bzw. ≤ 45 ms (10 ms) |
| B5 | Ende-zu-Ende WAN-Emulation | `tc netem delay 30ms 10ms loss 2%` auf Server-Interface; Jitter-Buffer-Zieltiefe und Late-Rate protokollieren; Late-Rate ≤ 5 % erwartet (95 %-Quantil) |
| B6 | FEC-Nutzen | Wie B5 mit `PACKET_LOSS_PERC` 0 vs. gemessen; Anteil `decode_fec`-Erfolge vs. PLC zählen |
| B7 | Drift Capture↔Playback | 10 min Stream; Capture-Samples und Playback-Samples zählen; ppm = (in − out)/out·10⁶; erwartet ≈ 0 unter PipeWire mit `node.group`; auf Windows später auf echter Hardware |
| B8 | Drift Peer↔Peer | Zwei Hosts 10 min Dauersenden (VAD aus); Trend der relativen Ankunftsverzögerung in ms/min; Korrekturereignisse zählen; hörbare Artefakte je Korrektur bewerten |
| B9 | Startwert Jitter-Buffer | Einschwingzeit vom Start (2 Frames) zum Ziel unter B5 messen; ≤ 5 s erwartet |

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `../history/verification.md`):
  - Spike 3 **liefert Messwerte** für B2: Encode 0,09–0,11 ms, Decode 0,013 ms je Frame (Tabellenzeilen „Encode-Zeit“/„Decode-Zeit“ können auf 0,1 / 0,02 ms gesetzt werden; RNNoise 1,15 ms Rechenzeit kommt hinzu, seine Fensterverzögerung bleibt B3/offen).
  - Spike 3 **bestätigt** die Grundannahme: die reine Software-Kette (Frame füllen + Verarbeitung + Übergabe) liegt bei 10-ms-Frames um 14 ms p50; das passt zur Budgetspalte „10 ms LAN typisch“.
  - Offen: B1, B4–B9.

Per WebFetch gelesen (2026-09-20): L1, L4, L5, L6 (`delay_manager.cc`, Zusammenfassung), L7, L11, L12, L13, L14, L15, L16, L17.

Per `curl` im Volltext gelesen und mit `grep`/`sed` zitiert: L2 (https://www.rfc-editor.org/rfc/rfc6716.txt), L3, L6 (`delay_manager.cc`/`.h` via gitiles `?format=TEXT`), L8, L9, L10.

Nicht erreichbar / nicht gelesen: ITU-T G.114 Volltext-PDF (https://www.itu.int/rec/dologin_pub.asp?lang=e&id=T-REC-G.114-200305-I!!PDF-E&type=items → HTTP 500), https://www.itu.int/rec/T-REC-G.114-200305-I/en (nur Landing-Page); https://raw.githubusercontent.com/mumble-voip/mumble/master/src/mumble/JitterBuffer.cpp (404, Datei existiert nicht; Mumble nutzt den Speex-Jitter-Buffer, L9/L10).
