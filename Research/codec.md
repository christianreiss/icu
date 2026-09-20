# Codec: Opus-Parametrierung, Rust-Crates, Relay vs. Mix

## Frage

Welche Opus-Einstellungen (Frame-Größe, Bitrate, Application, FEC, PLC, DTX, Complexity, Kanäle) liefern für Sprache bei niedrigster Latenz "glasklare" Qualität, welches Rust-Crate bindet libopus sinnvoll ein, und ab welcher Teilnehmerzahl N kippt ein reiner Relay-Server (N−1 Streams je Client, kein Server-Decode) gegenüber einem Server-Mix?

## Kandidaten

| Thema | Kandidaten |
| --- | --- |
| Frame-Größe | 10 ms, 20 ms (2.5/5 ms scheiden aus: CELT-only, kein SILK/Hybrid, kein FEC) |
| Bitrate | 32, 40, 48, 64 kbit/s bei FB 48 kHz mono |
| Application | OPUS_APPLICATION_VOIP, OPUS_APPLICATION_AUDIO, OPUS_APPLICATION_RESTRICTED_LOWDELAY |
| Loss-Robustheit | In-band FEC (LBRR), Decoder-PLC, Deep PLC (1.5+), DRED (1.5+, nicht standardisiert) |
| DTX | an/aus |
| Kanäle | mono, stereo |
| Rust-Crates | `opus` (opus-rs), `opusic-sys` + `opusic-c`, `audiopus` + `audiopus_sys`, `opus-sys` |
| Topologie | Relay (Server leitet Opus-Pakete weiter, Client mischt) vs. Mix (Server decodiert, mischt, encodiert je Client) |

## Primärquellen

Abrufdatum aller Quellen: 2026-09-20.

| # | Quelle | URL |
| --- | --- | --- |
| Q1 | RFC 6716 "Definition of the Opus Audio Codec" (Volltext) | https://www.rfc-editor.org/rfc/rfc6716 |
| Q2 | RFC 7587 "RTP Payload Format for the Opus Speech and Audio Codec" | https://www.rfc-editor.org/rfc/rfc7587 |
| Q3 | libopus 1.5 API: Encoder CTLs | https://opus-codec.org/docs/opus_api-1.5/group__opus__encoderctls.html |
| Q4 | libopus 1.5 API: Encoder | https://opus-codec.org/docs/opus_api-1.5/group__opus__encoder.html |
| Q5 | libopus 1.5 API: Decoder | https://opus-codec.org/docs/opus_api-1.5/group__opus__decoder.html |
| Q6 | libopus 1.5 API: Decoder CTLs | https://opus-codec.org/docs/opus_api-1.5/group__opus__decoderctls.html |
| Q7 | libopus 1.5 API: Generic CTLs | https://opus-codec.org/docs/opus_api-1.5/group__opus__genericctls.html |
| Q8 | libopus Quellcode `src/opus_encoder.c` (main) | https://raw.githubusercontent.com/xiph/opus/main/src/opus_encoder.c |
| Q9 | libopus `include/opus_defines.h` (main) | https://raw.githubusercontent.com/xiph/opus/main/include/opus_defines.h |
| Q10 | libopus README (main) | https://raw.githubusercontent.com/xiph/opus/main/README |
| Q11 | opus-codec.org Startseite (Feature-Liste, Releases) | https://opus-codec.org/ |
| Q12 | opus-codec.org Downloads (aktuelle Version) | https://opus-codec.org/downloads/ |
| Q13 | Opus 1.5 Release-Demo (Deep PLC, DRED, OSCE) | https://opus-codec.org/demo/opus-1.5/ |
| Q14 | libopus 1.5 Release-Notiz | https://opus-codec.org/release/stable/2024/03/04/libopus-1_5.html |
| Q15 | libopus 1.6 Release-Notiz | https://opus-codec.org/release/stable/2025/12/15/libopus-1_6.html |
| Q16 | RFC 9221 "An Unreliable Datagram Extension to QUIC" | https://www.rfc-editor.org/rfc/rfc9221 |
| Q17 | RFC 9000 "QUIC: A UDP-Based Multiplexed and Secure Transport" | https://www.rfc-editor.org/rfc/rfc9000 |
| Q18 | RFC 9001 "Using TLS to Secure QUIC" | https://www.rfc-editor.org/rfc/rfc9001 |
| Q19 | RFC 768 (UDP) | https://www.rfc-editor.org/rfc/rfc768 |
| Q20 | RFC 791 (IPv4) | https://www.rfc-editor.org/rfc/rfc791 |
| Q21 | docs.rs quinn `Connection` (Datagram-API) | https://docs.rs/quinn/latest/quinn/struct.Connection.html |
| Q22 | crates.io API: quinn | https://crates.io/api/v1/crates/quinn |
| Q23 | crates.io API: opus | https://crates.io/api/v1/crates/opus |
| Q24 | crates.io API: opusic-sys | https://crates.io/api/v1/crates/opusic-sys |
| Q25 | crates.io API: opusic-c | https://crates.io/api/v1/crates/opusic-c |
| Q26 | crates.io API: audiopus | https://crates.io/api/v1/crates/audiopus |
| Q27 | crates.io API: audiopus_sys | https://crates.io/api/v1/crates/audiopus_sys |
| Q28 | crates.io API: opus-sys | https://crates.io/api/v1/crates/opus-sys |
| Q29 | docs.rs opus 0.4.0 | https://docs.rs/opus/latest/opus/ |
| Q30 | opus-rs `Cargo.toml` (master) | https://raw.githubusercontent.com/SpaceManiac/opus-rs/master/Cargo.toml |
| Q31 | opusic-sys README (GitHub) | https://github.com/DoumanAsh/opusic-sys |
| Q32 | opusic-sys `build.rs` (master) | https://raw.githubusercontent.com/DoumanAsh/opusic-sys/master/build.rs |
| Q33 | docs.rs opusic-c | https://docs.rs/opusic-c/latest/opusic_c/ |
| Q34 | audiopus_sys README (GitHub) | https://github.com/lakelezz/audiopus_sys |
| Q35 | audiopus_sys `build.rs` (master) | https://raw.githubusercontent.com/lakelezz/audiopus_sys/master/build.rs |
| Q36 | Mumble `src/mumble/Settings.h` (Vergleichswert Bitrate) — Sekundärquelle (Fremdprojekt) | https://raw.githubusercontent.com/mumble-voip/mumble/master/src/mumble/Settings.h |

## Befunde

### Opus-Grundlagen

| Fakt | Wert | Quelle |
| --- | --- | --- |
| Frame-Größen | 2.5, 5, 10, 20, 40, 60 ms; Pakete bis 120 ms | Q1 §2 |
| Bandbreiten | NB 4 kHz/8 kHz, MB 6 kHz/12 kHz, WB 8 kHz/16 kHz, SWB 12 kHz/24 kHz, FB 20 kHz/48 kHz | Q1 Tabelle 1 |
| Modi | SILK-only: NB/MB/WB, 10–60 ms; Hybrid: SWB/FB, 10 und 20 ms; CELT-only: NB–FB, 2.5–20 ms | Q1 §2, §3.1 Tabelle (config 12–15 = Hybrid SWB/FB, 10/20 ms) |
| Algorithmische Verzögerung | "from 5 ms to 65.2 ms" | Q1 §2 |
| SILK Look-ahead | "an additional 5 ms look-ahead for noise shaping estimation. A small additional delay (up to 1.5 ms) may be required for sampling rate conversion." | Q1 §2 |
| CELT Look-ahead | "additional 2.5 ms look-ahead due to the overlapping MDCT windows" | Q1 §2 |
| Hybrid-Ausgleich | "the CELT encoder input is delayed by an additional 2.7 ms" | Q1 §2 |
| Frames < 10 ms | "Passing in a duration of less than 10 ms (480 samples at 48 kHz) will prevent the encoder from using the LPC or hybrid modes." | Q4 |
| TOC-Byte | Jedes Paket beginnt mit einem TOC-Byte, das "config" (Modus, Bandbreite, Frame-Größe), Stereo-Flag und Frame-Zahl trägt | Q1 §3.1 |
| Stateful | "Opus packets are not coded independently of each other. Packets must be passed into the decoder serially and in the correct order" | Q5 |

### Encoder-Lookahead (Zahlen aus dem Quellcode)

| Fakt | Wert | Quelle |
| --- | --- | --- |
| `OPUS_GET_LOOKAHEAD` | `*value = st->Fs/400; if (application != RESTRICTED_LOWDELAY && != RESTRICTED_CELT) *value += st->delay_compensation;` | Q8 |
| `delay_compensation` | `st->Fs/250` mit Kommentar "Delay compensation of 4 ms (2.5 ms for SILK's extra look-ahead + 1.5 ms for SILK resamplers and stereo prediction)" | Q8 |
| Lookahead VOIP/AUDIO bei 48 kHz | 120 + 192 = **312 Samples = 6.5 ms** (abgeleitet aus Q8) | Q8 |
| Lookahead RESTRICTED_LOWDELAY bei 48 kHz | 120 Samples = 2.5 ms (abgeleitet) | Q8 |
| Gesamtverzögerung Encoder | 20 ms Frame + 6.5 ms = 26.5 ms; 10 ms Frame + 6.5 ms = 16.5 ms (abgeleitet) | Q8, Q1 |
| Default-Bitrate bei OPUS_AUTO | `60*Fs/frame_size + Fs*channels` → 48 kHz, 20 ms, mono: 60·48000/960 + 48000 = 51 000 bit/s (abgeleitet) | Q8 |
| Modus-Schwelle Sprache mono | `mode_thresholds` voice mono 64000: unterhalb 64 kbit/s SILK/Hybrid, oberhalb CELT (Tabelle im Quellcode) | Q8 |
| FEC-Schwellen | `fec_thresholds` FB: 22000 (LBRR erst oberhalb ~22 kbit/s bei FB, mit Hysterese 1000) | Q8 |
| Stereo-Schwelle Sprache | `stereo_voice_threshold = 19000` | Q8 |

### Bitrate

| Fakt | Wert | Quelle |
| --- | --- | --- |
| Empfehlung 20 ms Frames, Sprache | NB 8–12, WB 16–20, **FB 28–40 kbit/s**; FB mono Musik 48–64 | Q2 §3.1.1 |
| Bereich | "Bitrates from 6 kb/s to 510 kb/s" | Q11 |
| `OPUS_SET_BITRATE` | 500–512000 bit/s, OPUS_AUTO, OPUS_BITRATE_MAX | Q3 |
| VBR | `OPUS_SET_VBR` 1 = VBR (default); `OPUS_SET_VBR_CONSTRAINT` 1 = constrained (default) | Q3 |
| Vergleichswert Mumble | `iQuality = 40000` (40 kbit/s Default) | Q36 (Sekundärquelle) |
| "Glasklar" oberhalb 40 kbit/s | Keine Primärquelle mit MOS-Werten gelesen — **unbelegt**; RFC 7587 nennt 28–40 als Empfehlung, nicht als Obergrenze | — |

### Application, FEC, PLC, DTX, Complexity, Kanäle

| Fakt | Wert | Quelle |
| --- | --- | --- |
| VOIP | "gives best quality at a given bitrate for voice signals. It enhances the input signal by high-pass filtering and emphasizing formants and harmonics." | Q4 |
| AUDIO | "best quality at a given bitrate for most non-voice signals like music … and applications requiring less than 15 ms of coding delay" | Q4 |
| RESTRICTED_LOWDELAY | "disables the speech-optimized mode in exchange for slightly reduced delay" | Q4 |
| `OPUS_SET_INBAND_FEC` | 0 aus (default); 1 an, "If the packet loss rate is sufficiently high, Opus will automatically switch to SILK even at high rates"; 2 an ohne erzwungenen SILK-Wechsel bei Musik | Q3 |
| `OPUS_SET_PACKET_LOSS_PERC` | 0–100, default 0 | Q3 |
| LBRR-Inhalt | "The LBRR frames, if present, contain an encoded representation of the signal immediately prior to the current Opus frame" → Redundanz für Frame n liegt in Paket n+1 | Q1 §4.2.5 |
| LBRR = SILK-Layer | LBRR ist Teil der SILK-Frame-Struktur (Q1 §4.2); CELT-only-Modus hat kein LBRR | Q1 §4.2 |
| Decoder-FEC | `decode_fec` "Flag (0 or 1) to request that any in-band forward error correction data be decoded. If no such data is available, the frame is decoded as if it were lost." frame_size muss exakt der fehlenden Dauer entsprechen, Vielfaches von 2.5 ms | Q5 |
| PLC | "Lost packets can be replaced with loss concealment by calling the decoder with a null pointer and zero length"; PLC ist "optional decoder-side feature that SHOULD be included"; CELT: Pitch-Wiederholung, SILK: LPC-Extrapolation | Q5, Q1 §4.4 |
| Deep PLC (1.5+) | "set the decoder complexity to 5 or more"; ~1 MB Binary; "about 1% of a laptop CPU core"; Build-Flag `--enable-deep-plc` | Q13, Q14 |
| DRED (1.5+) | bis 1 s Redundanz je Paket, "12-32 kb/s overhead", ~2 MB, `--enable-dred`; "not yet standardized"; `OPUS_SET_DRED_DURATION` in 10-ms-Einheiten | Q13, Q3 |
| OSCE LACE/NoLACE | Decoder-Complexity 6 bzw. ≥7, 100 bzw. 400 MFLOPS, nützlich bei 6–9 kbit/s | Q13 |
| DTX | `OPUS_SET_DTX` default 0; "only applicable to the LPC layer"; bei DTX "only one frame is encoded every 400 milliseconds" | Q3, Q1 |
| DTX-Qualität | DTX "will have a slightly lower speech or audio quality than continuous transmission"; "continuous transmission is RECOMMENDED unless constraints on available network bandwidth are severe" | Q2 |
| Complexity | Bereich 0–10, "10 representing the highest complexity" (Q3/Q9, kein Default-Wert in der API-Doku); Library-Init `st->silk_mode.complexity = 9` (Q8); opus_demo "default: 10" (Q10) | Q3, Q9, Q8, Q10 |
| Encoder-Sample-Rates | 8000, 12000, 16000, 24000, 48000 Hz; 1 oder 2 Kanäle | Q4 |
| Zulässige `frame_size` bei 48 kHz | 120, 240, 480, 960, 1920, 2880 Samples | Q4 |
| Signal-Hint | `OPUS_SET_SIGNAL` OPUS_SIGNAL_VOICE "Bias thresholds towards choosing LPC or Hybrid modes" | Q3 |
| Clock-Drift | RFC empfiehlt: Sender langsamer → PLC einsetzen; Sender schneller → Pakete überspringen statt decodierte Frames verwerfen | Q1 §4.4.1 |
| Aktuelle libopus | 1.6.1 (2026-01-14); 1.6 (2025-12-15): WB→FB-Bandbreitenerweiterung, 96 kHz, DRED-Verbesserung, 24-bit-API | Q11, Q12, Q15 |

### Rust-Crates

| Crate | Version (Datum) | Lizenz | libopus-Anbindung | Build | Befund | Quelle |
| --- | --- | --- | --- | --- | --- | --- |
| `opus` (opus-rs) | 0.4.0 (2026-08-23) | MIT/Apache-2.0 | `opusic-sys = "0.7.3"` | via opusic-sys (cmake) | "Safe Rust bindings for libopus"; Encoder/Decoder/Repacketizer; aktiv gepflegt | Q23, Q29, Q30 |
| `opusic-sys` | 0.7.5 (2026-08-06) | BSD-3-Clause | bündelt libopus, "Target version 1.6.1"; Feature `bundled` (default), sonst `OPUS_LIB_DIR`/`OPUS_LIB_STATIC` | `cmake` Crate, Ninja falls vorhanden; CMake-Defines u. a. `OPUS_DRED`, `OPUS_OSCE` (feature-gated), `OPUS_HARDENING`, `OPUS_X86_PRESUME_*`; kein pkg-config-Fallback | Aktuellste Bindung; Android-Toolchain-Support | Q24, Q31, Q32 |
| `opusic-c` | 1.6.1 (2026-04-27) | BSD-3-Clause | `opusic-sys ^0.7.1` | wie opusic-sys; Features `bundled` (default), `dred`, `osce`, `no-simd`, `fixed-point` | High-level API mit `decode_fec`, DRED-Decoder, Multistream | Q25, Q33 |
| `audiopus` / `audiopus_sys` | 0.3.0-rc.0 / 0.2.2 (2021-04-22) | ISC | bündelt libopus **1.3** als Submodul; pkg-config zuerst (Unix), sonst `cmake::build` | cmake | Seit 2021 kein Release; libopus 1.3 (kein Deep PLC/DRED) | Q26, Q27, Q34, Q35 |
| `opus-sys` | 0.2.1 (2016-07-04) | MIT | — | — | Tot seit 2016 | Q28 |

Host-Befund (aus `Research/verification.md`): cmake 3.31.11 vorhanden, `libopus-devel` fehlt, `mingw64-opus` 1.5.2 als Paket verfügbar → für Linux-Build ist Bündelung via cmake ohnehin nötig oder `libopus-devel` nachzuinstallieren.

### Header-Overhead je Paket (Eingangsgrößen)

| Schicht | Bytes | Quelle / Annahme |
| --- | --- | --- |
| IPv4 | 20 (IHL min 5) | Q20 |
| UDP | 8 | Q19 |
| QUIC 1-RTT Short Header | 1 (Flags) + DCID 0–20 + PN 1–4 → Annahme **1 + 8 + 2 = 11** | Q17 §17.3; DCID-Länge 8 und PN-Länge 2 sind Annahmen |
| QUIC AEAD-Tag | 16 ("16-byte authentication tag") | Q18 §5.3 |
| DATAGRAM-Frame | Typ 0x30 ohne Length-Feld (letztes Frame im Paket): **1** | Q16 |
| Eigener Header | Annahme **8** (Sender-ID 2, Sequenz 2, Timestamp 4); Opus-TOC ist Teil der Nutzlast | Annahme |
| **Summe** | **64 B** (IPv6: 84 B) | abgeleitet |

QUIC-Datagramme: "not retransmitted upon loss detection", "do not contribute to any per-flow or connection-wide data limit", unterliegen aber dem Congestion Controller; müssen in ein QUIC-Paket passen (Q16). quinn 0.11.12 (2026-09-14): `send_datagram` "unreliable, unordered", `max_datagram_size()` liefert `None` wenn Peer keine Datagramme unterstützt, schwankt mit Path-MTU; `send_datagram_wait` priorisiert alte vor neuen Datagrammen (Q21, Q22). Für Voice ist `send_datagram` (nicht `_wait`) richtig: ein altes Sprachpaket zu verwerfen ist besser als es verspätet zu senden.

### Bandbreite je Stream (48 kHz mono, Opus-Nutzlast = Bitrate × Frame-Dauer, plus 64 B Overhead)

| Frame | Opus-Bitrate | Nutzlast/Paket | Pakete/s | Bytes/s | **kbit/s auf dem Draht** | Overhead-Anteil |
| --- | --- | --- | --- | --- | --- | --- |
| 20 ms | 32 kbit/s | 80 B | 50 | 7 200 | **57.6** | 44 % |
| 20 ms | 48 kbit/s | 120 B | 50 | 9 200 | **73.6** | 35 % |
| 20 ms | 64 kbit/s | 160 B | 50 | 11 200 | **89.6** | 29 % |
| 10 ms | 32 kbit/s | 40 B | 100 | 10 400 | **83.2** | 62 % |
| 10 ms | 48 kbit/s | 60 B | 100 | 12 400 | **99.2** | 52 % |
| 10 ms | 64 kbit/s | 80 B | 100 | 14 400 | **115.2** | 44 % |

Alle Werte abgeleitet (Rechnung: (Nutzlast + 64) × Pakete/s × 8). VBR schwankt real um den Zielwert; CBR-Annahme als Obergrenze.

### Relay vs. Mix bei S = 73.6 kbit/s je Stream (20 ms, 48 kbit/s)

Worst case: alle N sprechen gleichzeitig (VAD ohne PTT-Disziplin). Typisch: T = 2 gleichzeitige Sprecher (PTT). Werte in kbit/s, abgeleitet.

| N | Relay: Client Up | Relay: Client Down worst (N−1)·S | Relay: Client Down typisch T·S | Relay: Server In worst N·S | Relay: Server Out worst N(N−1)·S | Relay: Server Out typisch T(N−1)·S | Mix: Client Up/Down | Mix: Server In / Out worst N·S | Mix: Server-CPU je 20 ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 4 | 73.6 | 220.8 | 147.2 | 294.4 | 883.2 | 441.6 | 73.6 / 73.6 | 294.4 / 294.4 | 4 Decodes + 4 Encodes |
| 8 | 73.6 | 515.2 | 147.2 | 588.8 | 4 121.6 | 1 030.4 | 73.6 / 73.6 | 588.8 / 588.8 | 8 + 8 |
| 12 | 73.6 | 809.6 | 147.2 | 883.2 | 9 715.2 | 1 619.2 | 73.6 / 73.6 | 883.2 / 883.2 | 12 + 12 |
| 20 | 73.6 | 1 398.4 | 147.2 | 1 472.0 | 27 968.0 | 2 796.8 | 73.6 / 73.6 | 1 472.0 / 1 472.0 | 20 + 20 |

Bei 10 ms Frames (S = 99.2) alle Relay-Zahlen × 1.35; Server Out worst bei N = 20: 37.7 Mbit/s.

Client-Decode-Last im Relay: N−1 Opus-Decodes je 20 ms; CPU-Kosten je Decode nicht aus Primärquelle belegt (**unbelegt**, Step 3).

Kippkriterium (explizit, damit die Entscheidung falsifizierbar ist):
1. Client-Downlink worst case (N−1)·S ≤ 2 Mbit/s (konservative Annahme für schwache Anschlüsse) → N ≤ 28.
2. Server-Egress worst case N(N−1)·S ≤ 30 Mbit/s (Annahme: 100-Mbit/s-Uplink, 30 % Reserve) → N ≤ 20.
3. Mix wird erst dann attraktiver, wenn Kriterium 2 reißt **oder** Clients die N−1 Decodes nicht schaffen (mobile Clients sind Nicht-Ziel).

Damit kippt Relay rechnerisch bei N > 20 (Server-Egress), bei PTT-Betrieb (T ≤ 3) real erst weit darüber. Der Mix hätte zusätzlich: Server braucht libopus, Transcodierung (Qualitätsverlust, +1 Encoder-Lookahead 6.5 ms + 1 Frame Pufferung auf dem Server), Server-CPU N Decodes + N Encodes je Frame, und mutual TLS schützt dann nur noch Transport statt Ende-zu-Ende-Nutzlast.

## Entscheidung

| Parameter | Wert | Begründung |
| --- | --- | --- |
| Topologie | **Relay** (Server leitet Opus-Datagramme ohne Decode weiter; Client decodiert N−1 Streams und mischt) | Zielgruppe "feste Gruppe" ≤ 20; Egress-Kriterium hält; keine Transcodierung; Server ohne libopus; PTT/VAD senkt reale Last auf T·S |
| Sample-Rate / Bandbreite | 48 kHz, Fullband, **mono** | Q1 Tabelle 1; Stereo bringt bei Sprache nichts, verdoppelt Decodes |
| Application | **OPUS_APPLICATION_VOIP**, `OPUS_SET_SIGNAL(OPUS_SIGNAL_VOICE)` | Q4: Sprachoptimierung; Lookahead identisch zu AUDIO (Q8) |
| Frame-Größe | **20 ms Default**, 10 ms als Client-Option ohne Protokolländerung; Empfänger liest die Frame-Dauer je Paket (`OPUS_GET_LAST_PACKET_DURATION`, Q6) für Jitter-Buffer-Arithmetik und `frame_size` bei PLC/FEC | Opus-Pakete sind über das TOC-Byte selbstbeschreibend (Q1 §3.1); 10 ms kostet +35 % Draht-Bandbreite und doppelte Paketrate, spart 10 ms; Entscheidung 10 vs. 20 wird in Step 3 gegen das Latenzbudget gemessen |
| Bitrate | **48 kbit/s VBR (constrained, libopus-Default)**, konfigurierbar 32–64 | Über RFC-7587-Empfehlung 28–40 (Q2) als Qualitätsreserve; liegt unter der 64-kbit/s-Modusschwelle (Q8), also SILK/Hybrid mit LBRR-Fähigkeit; Draht-Kosten 73.6 kbit/s je Stream tragbar |
| In-band FEC | `OPUS_SET_INBAND_FEC(1)`, `OPUS_SET_PACKET_LOSS_PERC` = max über die Verlustraten aller Empfänger (Relay: ein Encoder bedient N−1 Empfänger mit unterschiedlichem Downlink), Untergrenze = vom Server gemessener Uplink-Verlust; Start 0; **Protokollfolge: Verlust-Feedback-Nachricht (Empfänger → Server → Sender, Quantisierung 1 %) ist nötig — für `Research/protocol/` vormerken** | Bei 0 % Verlust erzeugt der Encoder keine LBRR-Bits (kein Overhead); Decoder nutzt `decode_fec=1`, wenn Paket n fehlt und n+1 schon im Jitter-Buffer liegt (Q5, Q1 §4.2.5) → Kopplung an Jitter-Buffer-Tiefe ≥ 1 Frame, siehe `latency-budget.md` |
| PLC | Klassische libopus-PLC (`opus_decode(NULL, 0, …)`) | Q5; Deep PLC in v1 aus (Binary +1 MB, Build-Flag, Complexity ≥ 5 im Decoder), Kandidat für v2 |
| DRED | aus | Q13: nicht standardisiert, 12–32 kbit/s Overhead |
| DTX | **aus** | PTT/VAD senden ohnehin nichts bei Stille; DTX nur LPC-Layer, leichte Qualitätseinbuße (Q2, Q3) |
| Complexity | **10** (Encoder, explizit gesetzt) | Bereich 0–10 (Q3); Library-Default intern 9 (Q8), opus_demo-Default 10 (Q10); CPU-Kosten auf Desktop unkritisch (unbelegt, Step 3 misst) |
| Crate | **`opus` 0.4.0** (auf `opusic-sys` 0.7.5, bundled libopus 1.6.1, cmake) | Aktiv, MIT/Apache-2.0, bündelt aktuelles libopus; `opusic-c` als Alternative, falls DRED/Deep-PLC-API gebraucht wird |
| Transport | QUIC DATAGRAM (RFC 9221) via quinn `send_datagram` | Unzuverlässig, ungeordnet, kein Flow-Control-Limit (Q16, Q21) |

## Verworfen, mit Grund

| Verworfen | Grund |
| --- | --- |
| Server-Mix | Transcodierung senkt Qualität ("glasklar"), +≥ 26.5 ms Latenz auf dem Server, Server braucht libopus, bricht Ende-zu-Ende-Nutzlastintegrität; erst ab N > 20 rechnerisch nötig |
| 2.5 / 5 ms Frames | Nur CELT-only, kein SILK/Hybrid, kein LBRR (Q4, Q1) |
| 40 / 60 ms Frames | Latenz; LBRR-Nutzung verlangt ohnehin +1 Frame Puffer |
| OPUS_APPLICATION_AUDIO | Keine Sprachoptimierung (Q4), gleicher Lookahead (Q8) |
| OPUS_APPLICATION_RESTRICTED_LOWDELAY | Spart 4 ms Lookahead, aber "disables the speech-optimized mode" (Q4) → CELT-only, kein LBRR |
| Stereo | `stereo_voice_threshold` 19 kbit/s irrelevant; verdoppelt Decode-Last im Relay; Sprache ist mono |
| DTX | siehe oben |
| DRED | nicht standardisiert (Q13), hoher Overhead |
| `audiopus` | libopus 1.3, letzte Version 2021 (Q26, Q34) |
| `opus-sys` | 2016 (Q28) |
| Bitrate 64 kbit/s+ | Oberhalb 64 kbit/s wechselt der Encoder für Sprache mono auf CELT (Q8); RFC 7587 sieht 28–40 als ausreichend; 89.6 kbit/s je Stream verschlechtert das Egress-Kriterium |

## Offene Punkte für Step 3

| # | Prüfung | Messvorschrift |
| --- | --- | --- |
| C1 | `opus` 0.4.0 / `opusic-sys` bundled baut auf Fedora 43 (cmake 3.31) und cross für `x86_64-pc-windows-gnu` (mingw64) | `cargo build --target x86_64-pc-windows-gnu` mit `CMAKE_TOOLCHAIN_FILE`/`CC` für mingw; Erfolg = Link ohne `libopus-devel`; alternativ Feature `bundled` aus + `mingw64-opus` 1.5.2 |
| C2 | Lookahead-Wert | `OPUS_GET_LOOKAHEAD` nach `opus_encoder_create(48000, 1, VOIP)` → erwartet 312 |
| C3 | Encode-/Decode-Zeit bei Complexity 10 | 1000 Frames à 20 ms encodieren/decodieren, Median und p99 in µs messen; Ziel < 1 ms je Frame; ebenso 19 parallele Decodes (N = 20) |
| C4 | Reale Draht-Bandbreite | 60 s Sprache über quinn senden, Bytes auf Socket-Ebene zählen (VBR-Mittel gegen 9 200 B/s prüfen) |
| C5 | LBRR-Verhalten | Mit `PACKET_LOSS_PERC` 0/5/20 die Paketgrößen vergleichen (erwartet: 0 → keine Zunahme); künstliches 5 %-Loss, Decodes mit `decode_fec=1` gegen PLC-only hören/messen (PESQ/POLQA nicht verfügbar → Blindvergleich) |
| C6 | Deep PLC schaltbar über opusic-sys | Prüfen, ob `OPUS_DEEP_PLC` ohne `dred`-Feature aktivierbar ist (build.rs listet `OPUS_DRED`/`OPUS_OSCE`, Deep-PLC-Define **unbelegt**) |
| C7 | 10 ms vs. 20 ms Hörvergleich | Gleiche Sprachdatei mit 48 kbit/s bei 10 und 20 ms; Bytes und subjektiver Vergleich; Ergebnis in `latency-budget.md` eintragen |
| C8 | QUIC-Header-Annahmen | quinn: tatsächliche DCID-Länge und PN-Länge im Wireshark-Trace prüfen; Overhead-Summe korrigieren |
| C9 | `max_datagram_size()` | Wert bei MTU 1500 (erwartet ≥ 1 200); ein 160-B-Frame passt immer |
| C10 | Verlust-Feedback | Format und Intervall der Feedback-Nachricht in `Research/protocol/` festlegen (offen); Step 3 misst, wie schnell `PACKET_LOSS_PERC` nach Beginn eines 5 %-Loss wirkt (Ziel ≤ 2 s) |

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `verification.md`):
  - Spike 3 **bestätigt** (opus 0.3.1 auf System-libopus 1.5.2, nicht opus 0.4/opusic-sys; Zahlen übertragbar): Encode 85–110 µs/10-ms-Frame (max 241 µs), Decode 12–14 µs/Frame. C3 damit erfüllt: 19 parallele Decodes ≈ 0,3 ms je 10 ms.
  - Spike 1 **bestätigt** C1 teilweise: `opus` 0.3 / audiopus_sys baut libopus per CMake aus Quelle im Cross-Build nach Windows-gnu ohne Toolchain-File. opusic-sys 0.7.5 (Ziel-Crate) muss in der Implementierung denselben Nachweis liefern (gleicher cmake-Mechanismus, erwartet identisch).
  - Spike 3 **beobachtet**: VOIP-Mode ohne DTX sendet bei Raumgeräusch 20–48 kbps (VBR arbeitet); C4 bleibt für echte Sprache offen.
  - Offen: C2 Lookahead-Wert, C5 LBRR-Verhalten, C7 Hörvergleich 10/20 ms, C10 Feedback-Nachricht (in `PROTOCOL.md` als `LossReport` vorgesehen).

Per WebFetch gelesen (2026-09-20): Q2, Q3, Q4, Q5, Q6, Q7, Q11, Q12, Q13, Q14, Q15, Q16, Q17, Q18, Q19, Q20, Q21, Q22, Q23, Q24, Q25, Q26, Q27, Q28, Q29, Q30, Q31, Q32, Q33, Q34, Q35 (URLs siehe Tabelle Primärquellen).

Per WebFetch teilweise (Seite zu groß, Zusammenfassung) und zusätzlich per `curl` im Volltext gelesen und mit `grep`/`sed` zitiert: Q1 (https://www.rfc-editor.org/rfc/rfc6716.txt), Q8, Q9, Q10, Q36.

Nicht erreichbar / nicht gelesen: https://opus-codec.org/release/stable/2024/03/04/libopus-1_5_release.html (404; ersetzt durch Q14).
