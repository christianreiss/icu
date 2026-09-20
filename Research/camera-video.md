# Kamera-Capture und Video-Codec

## Frage

Wie greift der ICU-Client auf Linux (V4L2) und Windows (Media Foundation) eine Webcam ab, und mit welchem Codec werden kleine Overlay-Streams (Basis: 320×240 oder 320×180 bei 15 fps) übertragen, sodass Build, Cross-Compile nach Windows, Lizenz/Patente und Bandbreite für eine feste kleine Gruppe zusammenpassen?

## Kandidaten

Capture

- nokhwa (Backends MSMF, V4L2, AVFoundation)
- v4l (direkte V4L2-Bindings) + windows-rs `Win32::Media::MediaFoundation` (Source Reader)

Codec

- H.264 über openh264 (Cisco, Source-Build oder Cisco-Binary)
- VP8 über libvpx (env-libvpx-sys, vpx-sys)
- AV1 über rav1e (Encoder) — Decoder gesondert
- MJPEG über jpeg-encoder (pure Rust, Encode) + zune-jpeg (pure Rust, Decode) oder turbojpeg (libjpeg-turbo)

## Primärquellen

Alle abgerufen am 2026-09-20. Version/Datum/Lizenz aller Crates ausschließlich aus der crates.io-API per `curl https://crates.io/api/v1/crates/<name>`; docs.rs nur für API-Form.

Capture

- N1 nokhwa Crate-Root: https://docs.rs/nokhwa/latest/nokhwa/
- N2 nokhwa README (Branch 0.10): https://raw.githubusercontent.com/l1npengtul/nokhwa/0.10/README.md
- N3 nokhwa Repo-Startseite: https://github.com/l1npengtul/nokhwa
- V1 v4l Crate-Root: https://docs.rs/v4l/latest/v4l/
- M1 windows-rs `Win32::Media::MediaFoundation`: https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/Media/MediaFoundation/index.html
- M2 Microsoft Learn, Audio/Video Capture in Media Foundation: https://learn.microsoft.com/en-us/windows/win32/medfound/audio-video-capture-in-media-foundation

Codec

- H1 openh264 Crate-Root: https://docs.rs/openh264/latest/openh264/
- H2 Cisco openh264 README: https://raw.githubusercontent.com/cisco/openh264/master/README.md
- H3 Cisco BINARY_LICENSE.txt: https://www.openh264.org/BINARY_LICENSE.txt
- H4 Cisco openh264 FAQ: https://www.openh264.org/faq.html
- H5 Via LA (ehem. MPEG LA), AVC/H.264 Patent-Pool: https://www.via-la.com/licensing-2/avc-h-264/
- P1 env-libvpx-sys README: https://raw.githubusercontent.com/astraw/env-libvpx-sys/master/README.md
- P2 libvpx LICENSE: https://raw.githubusercontent.com/webmproject/libvpx/main/LICENSE
- P3 libvpx PATENTS: https://raw.githubusercontent.com/webmproject/libvpx/main/PATENTS
- A1 rav1e Crate-Root: https://docs.rs/rav1e/latest/rav1e/
- A2 rav1e README: https://raw.githubusercontent.com/xiph/rav1e/master/README.md
- A3 AOM Patent License 1.0: https://aomedia.org/license/patent-license/
- J1 jpeg-encoder Crate-Root: https://docs.rs/jpeg-encoder/latest/jpeg_encoder/
- J2 zune-jpeg Crate-Root: https://docs.rs/zune-jpeg/latest/zune_jpeg/
- J3 turbojpeg Crate-Root: https://docs.rs/turbojpeg/latest/turbojpeg/

Bandbreiten-Anker

- B1 YouTube, empfohlene Upload-Bitraten: https://support.google.com/youtube/answer/1722171

Nicht abrufbar (nichts daraus zitiert): `docs.rs/env-libvpx-sys` (404, Doku-Build fehlt; Repo-README P1 stattdessen).

## Befunde

### Capture: nokhwa

- nokhwa 0.10.11, Apache-2.0, veröffentlicht 2026-05-15 (crates.io-API). "A Simple-to-use, cross-platform Rust Webcam Capture Library"; `Camera`-Struct über allen Backends; Module `backends`, `buffer`, `pixel_format`, `threaded` (Callback-Modus), `utils` (`FrameFormat`, `ApiBackend`); `RequestedFormat` / `CameraFormat` für Auflösung und fps; mindestens ein `input-*`-Feature nötig. [N1]
- Backend-Tabelle (README 0.10): Video4Linux (Linux), MSMF (Windows), AVFoundation (Mac) je mit Input/Query/Query-Device; OpenCV nur Input; WASM. Features `input-native`, `input-opencv`, `input-jscam`. [N2]
- Repo: Default-Branch `senpai`, 57 offene Issues, 812 Stars. [N3] Der 0.10-Branch bekommt weiter Releases (0.10.11 im Mai 2026); dass `senpai` ein Rewrite ist und 0.10 nur noch Patches bekommt, ist **Inferenz** aus Branch-Namen und Release-Muster, nicht dokumentiert.

### Capture: v4l + Media Foundation direkt

- v4l 0.14.0, MIT, veröffentlicht 2023-05-12 (crates.io-API). "safe bindings for the Video4Linux (v4l) stack"; Capture per MMAP/UserPtr/DMA-Streams; Format-, Framesize-, Frameinterval-Negotiation; wahlweise libv4l2-Bindings oder direkte ioctl über `v4l2-sys`. [V1] Seit 2023 kein Release — V4L2 ist ein stabiles Kernel-ABI, das ist tolerierbar, aber Bugfixes kommen nicht.
- windows 0.62.2, MIT OR Apache-2.0, 2025-10-06 (crates.io-API). Feature `Win32_Media_MediaFoundation` exponiert `IMFSourceReader`, `IMFAttributes`, `IMFMediaType`, `IMFMediaBuffer` u. a. [M1]
- Microsoft: Video-Capture-Geräte "are supported through the UVC class driver"; Enumeration über `MFCreateAttributes` + `MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID` + `MFEnumDeviceSources`; Daten über den Source Reader ("use the Source Reader to get data from the device"); `Shutdown` zwingend. [M2] Format-Konvertierung (MJPEG/YUY2 → NV12/RGB) durch MF-Transforms ist auf dieser Seite nicht beschrieben → **unbelegt**.

### Codec: H.264 / openh264

- openh264 0.9.8 (Crate und -sys2), BSD-2-Clause, 2026-08-08 (crates.io-API). Feature `source` (Default): "Uses the bundled OpenH264 source; works out of the box", kompiliert Ciscos C++ per `cc`. Feature `libloading`: lädt Ciscos vorkompiliertes Binary zur Laufzeit. Encoder (`Encoder::encode`, YUV/RGB → H.264, Bitrate/fps konfigurierbar) und Decoder (`Decoder`, NAL → YUV) im selben Crate. "Cross-compilation requires setting the CXX environment variable"; `nasm` optional ("up to 3x speed boost"). [H1]
- Cisco-Encoder: "Constrained Baseline Profile up to Level 5.2", "Arbitrary resolution", "Rate control with adaptive quantization, or constant quantization", "Temporal scalability up to 4 layers". Build: NASM ≥ 2.10.06 für Assembler-Pfade. [H2]
- Patente: "The binary form of this Software is distributed by Cisco under the AVC/H.264 Patent Portfolio License from MPEG LA." Bedingungen u. a.: "The Cisco-provided binary is separately downloaded to an end user's device, and not integrated into or combined with third party software", Nutzer muss es abschalten können, Attribution "OpenH264 Video Codec provided by Cisco Systems, Inc." [H3] FAQ: "Cisco is only covering the royalties that would apply to the binary module ... products or projects that utilize it must download it at the time the product or project is installed." Source-Builds sind nicht abgedeckt. [H4]
- Via LA: AVC/H.264-Pool; für PC-Software-Codecs "For the first 1 to 100,000 units" pro Jahr "$0.00", danach $0.20/Unit (100.001–5 Mio.). [H5] Für eine feste private Gruppe faktisch 0 $, aber es bleibt ein Patent-Hinweis in der Doku nötig; kommerzielle Verteilung > 100k/Jahr wäre lizenzpflichtig.

### Codec: VP8 / libvpx

- env-libvpx-sys 5.1.3, MPL-2.0, 2023-09-10 (crates.io-API). Findet libvpx per pkg-config oder `VPX_LIB_DIR`/`VPX_INCLUDE_DIR`/`VPX_VERSION`, `VPX_STATIC=1` für statisches Linken; **baut libvpx nicht aus Quellen**, braucht eine vorhandene Bibliothek. [P1] Für den Windows-Cross-Build heißt das: libvpx für `x86_64-pc-windows-msvc` separat beschaffen (vcpkg/msys2-Artefakt) und per Env-Variablen anbinden.
- vpx-sys 0.1.1, MIT, 2021-03-01 (crates.io-API) — seit 2021 kein Release; nicht weiter betrachtet.
- Lizenz: BSD-3-Clause ("Copyright (c) 2010, The WebM Project authors") [P2]; Patentgrant "perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable" mit Defensive-Termination. [P3]

### Codec: AV1 / rav1e

- rav1e 0.8.1, BSD-2-Clause, 2025-06-16 (crates.io-API). "The fastest and safest AV1 encoder"; `Config`/`EncoderConfig` mit Speed 0–10, `low_latency`, Keyframe-Intervall, Bitrate; "most suitable for cases where libaom (the reference encoder) is too slow". [A1] "11 speed settings (0-10, exhaustive to near real-time)"; x86_64-Assembler braucht NASM ≥ 2.14.02, per Feature `asm` abschaltbar → reiner Rust-Build möglich. [A2]
- **rav1e ist nur ein Encoder.** [A1][A2] Ein AV1-Decoder wäre dav1d über C-Bindings (nicht recherchiert); ein produktionsreifer pure-Rust-AV1-Decoder ist mir nicht bekannt (**unbelegt**). Damit verliert AV1 den Cross-Compile-Vorteil wieder.
- Patente: AOM Patent License 1.0: "non-exclusive, no-charge, royalty-free, irrevocable patent license to its Necessary Claims" für Implementierungen, mit Defensive-Termination. [A3]

### Codec: MJPEG

- jpeg-encoder 0.7.1, "(MIT OR Apache-2.0) AND IJG", 2026-07-27 (crates.io-API). Pure Rust; Eingabe RGB/RGBA/Luma/YCbCr/CMYK; Quality 0–100; Chroma-Subsampling (4:2:0 möglich); progressive optional. [J1] Die IJG-Lizenzkomponente verlangt einen Hinweis in der Doku ("based in part on the work of the Independent JPEG Group"); Detail der Pflicht aus der Lizenz selbst hier **nicht** geprüft.
- zune-jpeg 0.5.15 stabil (0.5.16-rc2 vorhanden), MIT OR Apache-2.0 OR Zlib, 2026-09-08 (crates.io-API). Pure-Rust-Decoder für Baseline und Progressive, SSE/AVX-beschleunigt; **decode-only**. [J2]
- turbojpeg 1.5.1, Unlicense OR MIT, 2026-07-25 (crates.io-API). Bindet libjpeg-turbo über turbojpeg-sys; `Compressor`/`Decompressor`, Quality, `Subsamp`; Features `pkg-config`, `bindgen`. Ob libjpeg-turbo aus Quellen gebaut wird, ist auf der Seite nicht beschrieben → **unbelegt**; für Cross-Compile in jedem Fall ein C-Toolchain-Schritt.
- MJPEG ist zustandslos: jeder Frame ist unabhängig dekodierbar. Kein IDR/Keyframe-Request bei Paketverlust, kein Referenzframe-Drift, trivialer Wiedereinstieg für spät beitretende Clients. Das ist der protokollseitige Hauptvorteil.
- Viele UVC-Webcams liefern MJPEG nativ; ob eine konkrete Kamera 320×240 oder 320×180 als MJPEG anbietet: kameraabhängig, **unbelegt** → Step 3. Wenn ja, kann der Client die Kamera-Frames ohne Re-Encode weiterreichen (nur Qualität/Bitrate nicht steuerbar).

### Bandbreite und Latenz (Schätzungen)

Anker: YouTube empfiehlt für 360p (640×360 = 230.400 px) bei 24–30 fps 1 Mbit/s, für 480p 2,5 Mbit/s [B1]. Daraus bei 30 fps ≈ 0,145 bit pro Pixel und Frame für universellen Content in Upload-Qualität. Skalierung auf 320×240 (76.800 px) × 15 fps: 76.800 × 15 × 0,145 ≈ **167 kbit/s**; auf 320×180 (57.600 px): ≈ **125 kbit/s**. Webcam-Content (statischer Hintergrund, wenig Bewegung) liegt typisch darunter; das YouTube-Profil ist konservativ. Alle Zahlen in der Tabelle sind **Schätzungen**, abgeleitet aus diesem Anker bzw. aus üblichen Kompressionsraten; nichts davon ist gemessen.

| Codec | Bandbreite je Stream, 320×240 @ 15 fps | 320×180 @ 15 fps | Encode-Latenz pro Frame | Cross-Compile Linux → Windows | Lizenz / Patente |
| --- | --- | --- | --- | --- | --- |
| MJPEG (jpeg-encoder + zune-jpeg) | ~1,2–2,5 Mbit/s (Schätzung: 1–2 bit/px bei q≈70–80, 4:2:0 → 10–20 KB/Frame) | ~0,9–1,9 Mbit/s (Schätzung) | < 1 ms (Schätzung, reine DCT/Huffman bei 77k px) | Pure Rust, keine C-Toolchain [J1][J2] | MIT/Apache + IJG-Hinweis [J1]; JPEG patentfrei (Allgemeinwissen, unbelegt) |
| H.264 CBP (openh264, Source-Build) | ~100–250 kbit/s (Schätzung, Anker B1 → 167 kbit/s) | ~80–200 kbit/s (Schätzung) | ~1–5 ms (Schätzung) | C++ per `cc`, `CXX` für Target setzen [H1]; mit cargo-xwin/clang-cl machbar (unbelegt für dieses Crate) | Crate BSD-2 [H1]; Source-Build nicht durch Cisco gedeckt [H4]; Via LA: 0 $ bis 100k Units/Jahr [H5], Hinweis in Doku nötig |
| VP8 (libvpx via env-libvpx-sys) | ~100–300 kbit/s (Schätzung, ähnlich H.264 Baseline) | ~80–250 kbit/s (Schätzung) | ~1–5 ms Realtime-Preset (Schätzung) | Vorgebaute libvpx für Windows-Target nötig [P1] → aufwendigster Pfad | BSD-3 + Patentgrant [P2][P3] |
| AV1 (rav1e speed 10, Decoder separat) | ~80–250 kbit/s (Schätzung; bei speed 10 schrumpft der AV1-Vorteil, unbelegt) | ~60–200 kbit/s (Schätzung) | unbekannt bei dieser Größe, "near real-time" [A2] → Step 3 | Encoder pure Rust ohne `asm` [A2]; Decoder (dav1d, C) nicht pure Rust | BSD-2 + AOM-Patentlizenz [A3] |

Topologie-Rechnung (Annahme: feste Gruppe, N = 5, Server-Relay, jeder sendet 1 Stream und empfängt N−1 = 4):

- MJPEG @ 1,7 Mbit/s: Client up 1,7 / down 6,8 Mbit/s; Server in 8,5 / out 34 Mbit/s.
- H.264 @ 0,17 Mbit/s: Client up 0,17 / down 0,7 Mbit/s; Server in 0,85 / out 3,4 Mbit/s.
- Bei N = 8: MJPEG Server out ≈ 95 Mbit/s, H.264 ≈ 9,5 Mbit/s. Ab etwa N = 8 wird MJPEG für den Server-Uplink unangenehm.

## Entscheidung

Capture

- **v1: nokhwa 0.10.11** mit `input-native` (V4L2 auf Linux, MSMF auf Windows), 320×240 oder 320×180 @ 15 fps per `RequestedFormat` anfordern, MJPEG oder YUYV vom Gerät bevorzugen. Begründung: ein Crate für beide Plattformen, aktive Releases 2026 [N1][N3].
- **Pflicht: Capture hinter einem eigenen Trait** (`trait CameraSource { fn frames() -> Stream<Frame> }`), damit nokhwa ohne Protokoll- oder UI-Änderung durch v4l 0.14.0 [V1] + windows-rs Media Foundation Source Reader [M1][M2] ersetzt werden kann, falls der 0.10-Branch stehen bleibt oder die MSMF-Formate nicht passen.

Codec

- **v1: MJPEG**, Encode mit jpeg-encoder 0.7.1 (Quality ~70–80, 4:2:0), Decode mit zune-jpeg 0.5.x. Begründung: (1) beide Richtungen pure Rust, Cross-Compile nach Windows ohne C/C++-Toolchain [J1][J2]; (2) zustandslos, daher kein Keyframe-/Recovery-Mechanismus im Wire-Protokoll; (3) Encode-Latenz vernachlässigbar bei 77k Pixeln; (4) keine Patentfrage; (5) bei N ≤ ~6 und Server-Relay liegt die Server-Ausgangsbandbreite (Schätzung ≤ 50 Mbit/s) im Rahmen eines dedizierten Servers. Wenn die Kamera 320×240-MJPEG nativ liefert, kann Re-Encode entfallen.
- **Dokumentierter Upgrade-Pfad: openh264 0.9.8 (Source-Build)**, ein Crate für Encoder und Decoder [H1], ~10× weniger Bandbreite (Schätzung). Kosten: C++-Build im Cross-Compile [H1], Via-LA-Hinweis [H5], Keyframe-Request und Decoder-Resync im Protokoll. Wird gezogen, wenn Step 3 die MJPEG-Bandbreite als Problem misst oder N wächst.
- Konsequenz für `Research/protocol/`: Das Video-Frame-Format muss ein Codec-Feld tragen (MJPEG jetzt, H.264 später) und pro Stream eine Bitraten-/Größenbegrenzung erlauben; die Server-Relay-Bandbreite ist mit N×(N−1)×Streamrate zu budgetieren.

## Entscheidung (revidiert nach Step 3, 2026-09-20)

Die ursprüngliche Wahl von MJPEG beruhte auf zwei Risiken: (1) openh264 im Cross-Build unbelegt, (2) Bandbreite bei kleinem N tragbar. Spike 1 hat (1) ausgeräumt (Source-Build cross-kompiliert, statisch gelinkt, Binary startet unter Wine). Damit bleibt der Bandbreitenunterschied von rund 10× (Schätzung oben: 1,2–2,5 Mbit/s vs. 100–250 kbit/s je Stream) das ausschlaggebende Kriterium. Der Relay multipliziert ihn mit N(N−1); bei N = 5 sind das 34 Mbit/s Server-Egress und 6,8 Mbit/s Download je Client für MJPEG, während der Spieler gleichzeitig spielt.

- **v1: H.264 Constrained Baseline über openh264 0.9.8, Feature `source`** (Encoder und Decoder aus einem Crate), `UsageType::CameraVideoRealTime`, Zielbitrate 150 kbit/s bei 320×240 @ 15 fps, Keyframe-Intervall 2 s plus Keyframe auf Anfrage (`KeyframeRequest` im Control-Kanal, siehe `protocol/wire.md`). Attribution und Via-LA-Hinweis in der Doku (0 $ unter 100 000 Units/Jahr).
- **Fallback, per Feature-Flag im Build**: MJPEG mit jpeg-encoder + zune-jpeg, unverändert wie oben beschrieben, falls ein Zielsystem den C++-Build nicht kann. Das `codec`-Feld im Video-Header trägt beide.
- Capture-Entscheidung (nokhwa hinter eigenem Trait) bleibt.

## Verworfen, mit Grund

- **openh264 mit Cisco-Binary (`libloading`)**: Binary muss zur Installationszeit vom Nutzer separat von Cisco geladen werden, darf nicht gebündelt werden, Abschaltbarkeit und Attribution nötig [H3][H4] — passt nicht zu "ein statisches Binary".
- **VP8 / env-libvpx-sys**: baut libvpx nicht selbst [P1]; für den Windows-Cross-Build muss eine passende libvpx beschafft werden — der aufwendigste Build-Pfad ohne Qualitätsvorteil gegenüber H.264 CBP. vpx-sys ist seit 2021 tot.
- **AV1 / rav1e**: Encoder-only [A1]; Decoder nur über C-Bindings; Encode-Latenz bei speed 10 unbekannt [A2]. Bandbreitenvorteil bei dieser Bildgröße nicht belegt.
- **turbojpeg**: C-Bibliothek ohne Not, wo jpeg-encoder + zune-jpeg pure Rust sind [J3]. Bleibt Option, falls Step 3 zeigt, dass jpeg-encoder auf schwachen CPUs zu langsam ist (unwahrscheinlich bei 77k px).
- **v4l + Media Foundation direkt für v1**: doppelte Implementierung mit COM-Handling auf Windows; nur als Trait-Ersatz vorgesehen.

## Offene Punkte für Step 3

Auf Chris' Fedora-43-Box (PipeWire 1.4.11 installiert; ob die Kamera über V4L2 direkt oder über PipeWire/`org.freedesktop.portal.Camera` angesprochen wird, ist bei einem Host-Binary V4L2 direkt; ein späteres Flatpak bräuchte das Camera-Portal — nicht recherchiert):

1. nokhwa-Spike: Kamera enumerieren, `RequestedFormat` 320×240@15 und 320×180@15 anfordern; welche Formate (MJPEG/YUYV) und welche tatsächlichen Auflösungen/fps liefert die Kamera? Latenz Capture → Frame im Programm.
2. Liefert die Kamera 320×240-MJPEG nativ (Pass-through ohne Re-Encode möglich)? Frame-Größen messen.
3. Messen statt schätzen: jpeg-encoder bei q = 60/70/80 auf echten Webcam-Frames: KB/Frame, Mbit/s bei 15 fps, Encode-Zeit; zune-jpeg Decode-Zeit.
4. Vergleichsmessung openh264 (Source-Build) auf denselben Frames: kbit/s bei gleicher subjektiver Qualität, Encode-/Decode-Latenz, Größe des Binaries.
5. Cross-Compile-Probe: Workspace mit jpeg-encoder + zune-jpeg nach `x86_64-pc-windows-msvc` (cargo-xwin) bzw. `-gnu`; dann dasselbe mit openh264 `source` (CXX-Setup).
6. rav1e speed 10 + `low_latency` bei 320×240: Encode-Zeit pro Frame (nur, falls AV1 doch in Frage kommt).
7. Anzahl Kacheln gleichzeitig dekodieren (4–7 Streams @ 15 fps): CPU-Last des Decoders im Overlay-Pfad.

Auf Windows (später):

8. nokhwa MSMF: Format-Negotiation, ob MJPEG/NV12 ankommt, Verhalten bei mehreren Kameras, Latenz.
9. Falls nokhwa scheitert: Media-Foundation-Source-Reader-Spike mit windows-rs (Enumeration, `IMFSourceReader::ReadSample`, Format auf RGB32/NV12 setzen).

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `verification.md`):
  - Spike 5 **bestätigt** nokhwa 0.10.11/V4L2: Enumeration, Format-Negotiation 320×240 MJPEG 15 fps, MJPEG-Decode 0,24 ms/Frame. Falle: `/dev/video1` ist der Metadaten-Node und öffnet keinen Stream; Capture-Node explizit wählen.
  - Spike 5 **misst** openh264 0.9.8 (Source-Build): RGB→YUV + Encode 2,5 ms je 320×240-Frame. Kamera lieferte nur 3,7–6 fps (Auto-Belichtung bei wenig Licht, auch mit `v4l2-ctl`), daher keine belastbare Bitrate bei 15 fps.
  - Spike 1 **widerlegt** die Cross-Compile-Sorge bei openh264: das Crate baut im `x86_64-pc-windows-gnu`-Build aus Quelle (cc + nasm), einzige Falle ist das dynamische `libstdc++`, gelöst per statischem `libstdc++.a` im Link-Pfad (Rezept in `verification.md`).
  - **Entscheidung revidiert**, siehe Abschnitt „Entscheidung (revidiert nach Step 3)“.
  - Offen: Punkt 3 (jpeg-encoder-Messung), 4 (H.264 bei 15 fps und Tageslicht), 7 (Decoder-Last), Windows 8–9.

Mit WebFetch tatsächlich gelesen (2026-09-20):

- https://docs.rs/nokhwa/latest/nokhwa/
- https://raw.githubusercontent.com/l1npengtul/nokhwa/0.10/README.md
- https://github.com/l1npengtul/nokhwa
- https://docs.rs/v4l/latest/v4l/
- https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/Media/MediaFoundation/index.html
- https://learn.microsoft.com/en-us/windows/win32/medfound/audio-video-capture-in-media-foundation
- https://docs.rs/openh264/latest/openh264/
- https://raw.githubusercontent.com/cisco/openh264/master/README.md
- https://www.openh264.org/BINARY_LICENSE.txt
- https://www.openh264.org/faq.html
- https://www.via-la.com/licensing-2/avc-h-264/
- https://raw.githubusercontent.com/astraw/env-libvpx-sys/master/README.md
- https://raw.githubusercontent.com/webmproject/libvpx/main/LICENSE
- https://raw.githubusercontent.com/webmproject/libvpx/main/PATENTS
- https://docs.rs/rav1e/latest/rav1e/
- https://raw.githubusercontent.com/xiph/rav1e/master/README.md
- https://aomedia.org/license/patent-license/
- https://docs.rs/jpeg-encoder/latest/jpeg_encoder/
- https://docs.rs/zune-jpeg/latest/zune_jpeg/
- https://docs.rs/turbojpeg/latest/turbojpeg/
- https://support.google.com/youtube/answer/1722171
