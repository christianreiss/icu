# Prior Art

## Frage

Was tun Mumble, TeamSpeak 3/5, Ventrilo und Discord bei Transport, Codec, Auth, Overlay und Latenz — belegt aus offizieller Doku oder Source — und was macht ICU bewusst anders?

## Kandidaten

- **Mumble** (Open Source, BSD-Lizenz; Protokoll-Doku und Source öffentlich)
- **TeamSpeak 3 / 5** (proprietär; nur Marketing-Seiten, Support-Artikel und SDK-Doku)
- **Ventrilo** (proprietär; FAQ, About, Changelog)
- **Discord** (proprietär; Developer-Doku des Voice-Gateways, Support-Artikel, archiviertes Game SDK)

## Primärquellen

Alle abgerufen am 2026-09-20.

Mumble:
- Developer-Doku-Einstieg — https://www.mumble.info/documentation/developer/
- Network-Protocol README — https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/README.md
- Overview — https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/overview.md
- Protocol Stack TCP — https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/protocol_stack_tcp.md
- Establishing Connection — https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/establishing_connection.md
- Voice Data — https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/voice_data.md
- `MumbleUDP.proto` — https://raw.githubusercontent.com/mumble-voip/mumble/master/src/MumbleUDP.proto
- `mumble-server.ini` — https://raw.githubusercontent.com/mumble-voip/mumble/master/auxiliary_files/mumble-server.ini
- User-Doku Certificates — https://www.mumble.info/documentation/user/certificates/
- Overlay-Source Windows `overlay/lib.cpp` — https://raw.githubusercontent.com/mumble-voip/mumble/master/overlay/lib.cpp
- Overlay-Verzeichnis (GitHub API) — https://api.github.com/repos/mumble-voip/mumble/contents/overlay
- Overlay-Source Linux `overlay_gl/overlay.c` — https://raw.githubusercontent.com/mumble-voip/mumble/master/overlay_gl/overlay.c
- `overlay_gl`-Verzeichnis (GitHub API) — https://api.github.com/repos/mumble-voip/mumble/contents/overlay_gl

TeamSpeak:
- Features Overview — https://www.teamspeak.com/en/features/overview/
- Sekundärquelle (Spiegel der Client-SDK-Doku, nicht auf teamspeak.com): Sound codecs — https://teamspeakdocs.github.io/ClientSDK/client_html/ar01s14.html
- Nur per Suche gesehen, nicht abrufbar (403), daher Sekundär: "Which ports does the TeamSpeak 3 server use?" — https://support.teamspeak.com/hc/en-us/articles/360002712257-Which-ports-does-the-TeamSpeak-3-server-use ; Forum-Threads zu Opus/3.7.0 (Community-Forum, Sekundär).

Ventrilo:
- FAQ — https://www.ventrilo.com/faq.php
- About — https://www.ventrilo.com/about.php
- Homepage — https://www.ventrilo.com/
- Changelog — https://www.ventrilo.com/changelog.php
- Sekundärquelle: Wikipedia Ventrilo — https://en.wikipedia.org/wiki/Ventrilo

Discord:
- Voice Connections (Developer-Doku) — https://docs.discord.com/developers/topics/voice-connections
- Game SDK Overlay (archiviert, Developer-Doku) — https://docs.discord.com/developers/game-sdk/overlay
- Support-Artikel "Game Overlay 101" (über Zendesk-API abgerufen) — https://support.discord.com/api/v2/help_center/en-us/articles/217659737.json

Allgemein (für "was ICU anders macht"):
- RFC 6716 Opus — https://www.rfc-editor.org/rfc/rfc6716.html
- Microsoft Learn, Window Features (Layered/Topmost Windows) — https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features
- wlr-layer-shell-unstable-v1 — https://wayland.app/protocols/wlr-layer-shell-unstable-v1

## Befunde

### Mumble

- **Transport:** "a TCP connection which is used to reliably transfer control data between the client and the server" plus "a UDP connection which is used for low latency transfer of voice data"; Voice mit "OCB-AES128"; "While the TCP connection is mandatory the UDP connection can be compensated by tunnelling the UDP packets through the TCP connection" (overview.md). Ein Port für TCP und UDP: "Port to bind TCP and UDP sockets to. port=64738" (`mumble-server.ini`). Der README warnt: Doku spiegelt Mumble 1.2.8 und "may become outdated"; maßgeblich sind `Mumble.proto`/`MumbleUDP.proto`. Die Angabe "TLSv1 AES256-SHA" in overview.md ist damit als historisch zu lesen (Drift zur aktuellen Implementierung, nicht weiter belegt).
- **Control-Framing:** 2 Byte Typ + 4 Byte Länge, big-endian, protobuf-Payload, 26 Typen (0 `Version` … 25 `SuggestConfig`), Typ 1 `UDPTunnel` roh (protocol_stack_tcp.md).
- **Voice-Paket (legacy):** erstes Byte = 3 Bit Typ (CELT Alpha/Beta, Speex, Opus, Ping) + 5 Bit Target (0 normal, 1–30 Whisper, 31 Loopback); varint Sequence Number; Opus-Frame mit 13-Bit-Länge (max 8191) und Terminator-Bit; optional Positional Audio (3 Floats); eingehend zusätzlich varint Session-ID des Sprechers, "the server derives this from outgoing packet transport layer information"; max 1020 Byte Audio in 1024-Byte-UDP-Puffern mit 4 Byte Crypto-Header (voice_data.md).
- **Voice-Paket (protobuf, 1.5):** `Audio { oneof header { target | context }, sender_session, frame_number, opus_data, positional_data, volume_adjustment, is_terminator }`; `sender_session` "is not required when sending audio to the server, but will always be set when receiving audio from the server"; `Ping { timestamp, request_extended_information, server_version_v2, user_count, max_user_count, max_bandwidth_per_user }` (`MumbleUDP.proto`).
- **Codec:** Opus (plus CELT/Speex legacy); Server-Option `opusthreshold` ("Amount of users with Opus support needed to force Opus usage, in percent"), `bandwidth=558000` bit/s Sprach-Limit (`mumble-server.ini`).
- **Auth:** TLS über TCP; "clients may provide strong certificates, this is recommended but not mandatory"; `Authenticate` mit Username/Passwort/Tokens; Server schickt `CryptSetup` mit Key/Nonce für OCB-AES128; Ping-Timeout 30 s (establishing_connection.md). "Mumble uses certificates to identify users. A certificate can be self-generated or issued by a third party organization." (User-Doku). Server-Option `certrequired=false`: "If this options is enabled, only clients which have a certificate are allowed to connect." (`mumble-server.ini`). Kein serverseitiges CA-Modell: der Server kennt Zertifikate registrierter User, keine Kette.
- **Overlay Windows:** `overlay/` enthält `d3d9.cpp`, `d3d10.cpp`, `d3d11.cpp`, `dxgi.cpp`, `opengl.cpp`, `HardHook*.cpp` (u. a. MinHook), `overlay_blacklist.h`, `overlay_whitelist.h`, `excludecheck.cpp`. `lib.cpp`: DLL hookt `LoadLibraryA`/`LoadLibraryW`, ruft `checkD3D9Hook`/`checkDXGIHook`/`checkOpenGLHook`; Shared Memory `MumbleOverlayPrivate-x86`/`-x64` zum Mumble-Client; Ausschluss über `nooverlay`-Datei und Blacklist/Whitelist.
- **Overlay Linux:** `overlay_gl/overlay.c` hookt `glXSwapBuffers`, `glXGetProcAddressARB`, `glXGetProcAddress` via `dlsym(RTLD_DEFAULT, …)` (LD_PRELOAD-Interposition), Unix-Domain-Socket zum Client, Textur per `mmap(… PROT_READ, MAP_SHARED …)`; `drawContext()` sichert und restauriert GL-State. Nur GLX/OpenGL, kein Vulkan, kein Wayland-nativer Pfad (aus Dateiliste; kein Vulkan-File vorhanden).
- **Latenz:** keine Zahlen in der Doku; UDP-Pfad mit TCP-Fallback ist das Latenzmittel. Unbelegt darüber hinaus.

### TeamSpeak 3 / 5

- **Transport:** Marketing-Seite nennt kein Protokoll; nur "Military-grade AES-based encryption is standard" und "Ultra-Low" latency (Marketing-Claim, keine Zahl) (features/overview). Ports 9987 UDP (Voice), 10011 TCP (ServerQuery), 30033 TCP (File Transfer): nur per Suchtreffer auf support.teamspeak.com gesehen, Seite lieferte 403 → **Sekundär/unbelegt**. Das Client-Protokoll selbst ist nicht veröffentlicht → unbelegt.
- **Codec:** Client-SDK-Doku (Spiegel, Sekundär): Speex NB/WB/UWB, CELT Mono 48 kHz, Opus Voice Mono 48 kHz, Opus Music Stereo 48 kHz; "Opus Voice is recommended for voice transmission"; Bitrate-Tabelle je Qualitätsstufe (z. B. Stufe 5: Opus Voice 24 576 bit/s). Entfernung der Legacy-Codecs ab Server 3.7.0: nur Forum (Sekundär).
- **Auth:** Marketing nennt AES; Identitäts-/Public-Key-Modell nicht aus Primärquellen belegt → unbelegt.
- **Overlay:** nicht aus Primärquellen belegt → unbelegt.
- **Latenz:** "Low latency for real-time communication" (Marketing, ohne Zahl).

### Ventrilo

- **Transport:** FAQ: Verbindungstest per `telnet <host> 3784` — "If telnet can't connect then Ventrilo won't either as telnet is a function of the tcp/ip stack"; Default-Port 3784, TCP-Erreichbarkeit vorausgesetzt. Ob Voice über UDP läuft: unbelegt.
- **Codec:** "Server and/or channel specified codecs to control voice quality and bandwidth usage" (About). Changelog 4.0.0: "The Speex codec has been replaced with the Opus codec. Formats = 8KHz mono, 12KHz mono/stereo, 16KHz mono/stereo, 24KHz mono/stereo, 48KHz mono/stereo"; 4.1.1 "Updated Opus codec." GSM 6.10 nur per Wikipedia (Sekundär).
- **Auth/Crypto:** "Ventrilo uses a strict client-server based connection that is always encrypted" (About); Verfahren unbelegt. Kein Zertifikatsmodell dokumentiert.
- **Overlay:** nicht dokumentiert → unbelegt. **Latenz:** unbelegt. Plattformen: Windows, macOS; kein Linux-Client (Homepage/Wikipedia).

### Discord

- **Transport:** WebSocket-Voice-Gateway (Control, Version 8 empfohlen; < 4 seit 2024-11-18 abgeschaltet) + UDP für Audio; "clients be able to receive UDP packets, even through a firewall or NAT" (IP Discovery über UDP).
- **Voice-Paket:** RTP-artiger 12-Byte-Header: `0x80` (Version+Flags), `0x78` (Payload Type), u16 BE Sequence, u32 BE Timestamp, u32 BE SSRC, dann verschlüsseltes Audio. Transport-Encryption-Modi: `aead_aes256_gcm_rtpsize` (bevorzugt), `aead_xchacha20_poly1305_rtpsize` (Pflicht), `xsalsa20_poly1305`-Familie deprecated.
- **Codec:** "Opus codec, two channels (stereo) and a sample rate of 48kHz"; Silence-Frames `0xF8 0xFF 0xFE` vor Sendepausen.
- **E2EE:** ab 2026-03-01 ausschließlich DAVE-Protokoll, Frame-Level-Encryption mit "AES128-GCM" getrennt von der Transport-Verschlüsselung.
- **Auth:** Token/Session über das Gateway (kein Client-Zertifikat) — im abgerufenen Abschnitt nur implizit; Details unbelegt.
- **Overlay:** "The overlay is compatible with Windows 10 & 11 only; it does not function on Mac OS or Linux."; Empfehlung "windowed or borderless windowed"; Logs unter `…\modules\discord_hook\hook` (Support-Artikel). Game SDK (archiviert): "The overlay is only supported on Windows for DirectX or OpenGL games. Linux, Mac, and games using Vulkan are not supported."
- **Latenz:** keine Zahlen in der Doku → unbelegt.

### Querschnitt

| | Mumble | TeamSpeak 3 | Ventrilo | Discord |
| --- | --- | --- | --- | --- |
| Control | TCP+TLS, protobuf | unbelegt (proprietär) | TCP (Port 3784) | WebSocket |
| Voice | UDP, OCB-AES128, TCP-Tunnel-Fallback | UDP 9987 (Sekundär) | unbelegt | UDP, RTP-Header, AEAD, ab 2026 DAVE E2EE |
| Codec | Opus (Legacy CELT/Speex) | Opus Voice/Music (Sekundär) | Opus seit 4.0.0 | Opus 48 kHz stereo |
| Auth | Client-Cert optional, User-registriert per Cert-Hash, Passwort möglich | AES (Marketing), Rest unbelegt | "always encrypted", Rest unbelegt | Token/Gateway |
| Overlay | DLL-Injection + D3D9/10/11/DXGI/GL-Hooks (Win), LD_PRELOAD-GLX-Hook (Linux) | unbelegt | unbelegt | Hook-Modul, Windows 10/11, DirectX/OpenGL, kein Vulkan/Linux |
| Latenzangaben | keine | "Ultra-Low" (Marketing) | keine | keine |

## Entscheidung

Was ICU bewusst anders macht (Ableitung; Begründungen in `transport.md`, `auth-enrollment.md`, `protocol/wire.md`):

1. **Ein Transport statt zwei:** QUIC auf einem UDP-Port ersetzt Mumbles TCP+UDP-Paar bzw. Discords WebSocket+UDP. Kein TCP-Tunnel-Fallback: wer kein UDP hat, kann ICU nicht nutzen — bewusst akzeptiert, weil ein Fallback über TCP die Latenzgarantie bricht und Code für einen Nicht-Zielfall wäre.
2. **Keine eigene Voice-Crypto:** Mumbles OCB-AES128 mit `CryptSetup` und Discords Encryption-Modi entfallen; QUIC-1-RTT-Keys schützen Datagrams (RFC 9221 §6). Keine E2EE-Schicht wie DAVE: der Server ist selbst betrieben und vertrauenswürdig; E2EE würde Relay ohne Decode nicht ändern, aber Key-Management erfordern.
3. **mTLS mit eigener CA statt optionaler Certs oder Tokens:** Mumble akzeptiert per Default Clients ohne Cert und identifiziert per Cert-Hash; ICU erzwingt ein von der Server-CA ausgestelltes Client-Cert und hält die Allowlist per Fingerprint — kein Passwort, kein Token nach dem Enrollment.
4. **Opus mono, 20 ms, festes Profil:** kein Codec-Wechsel pro Channel (TeamSpeak/Ventrilo), kein Stereo-Default (Discord), keine Legacy-Codecs (Mumble). Sample-Clock 48 kHz wie Discord/RTP (RFC 7587).
5. **Server-gesetzte Speaker-ID, sonst kein Header-Umbau:** wie Mumbles `sender_session`; kein Positional Audio, kein Whisper-Target, kein Volume-Adjustment im Header.
6. **Kein Injection-Overlay:** Mumble (DLL-Injection, LoadLibrary-/Present-Hooks, LD_PRELOAD) und Discord (Hook-Modul, Windows-only, kein Vulkan) zeigen die Kosten: Anti-Cheat-Konflikte, pro-API-Code, Plattform-Lücken. ICU nutzt ein eigenes Fenster: Windows `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST` ("if the layered window has the WS_EX_TRANSPARENT extended window style … the mouse events will be passed to other windows underneath"; Topmost-Fenster "overlaps all other non-topmost windows"), Linux/Wayland `wlr-layer-shell` Layer `overlay` mit `keyboard_interactivity = none` (wlroots-Compositoren; GNOME Mutter laut Übersicht unvollständig → Einschränkung dokumentieren). Exklusives Fullscreen ist Nicht-Ziel (README).
7. **Ein Raum, keine Channels/Targets:** Mumbles 5-Bit-Target, Whisper, Channel-Listener und TeamSpeaks Channel-Codecs entfallen komplett aus dem Wire-Format.
8. **Protokoll-Doku als Quelle der Wahrheit:** Mumbles README räumt ein, dass die Protokoll-Doku (1.2.8) veraltet ist und die `.proto`-Dateien maßgeblich sind. ICU legt die Wire-Definition in ein gemeinsames Rust-Crate (`icu-proto`), das Client und Server kompilieren — Doku und Implementierung können nicht auseinanderlaufen.

## Verworfen, mit Grund

- **Mumble-Modell (TCP-Control + UDP-Voice mit eigener Crypto + TCP-Tunnel):** zwei Kanäle, eigene Crypto, Fallback-Komplexität; siehe `transport.md`.
- **Discord-Modell (WebSocket + UDP + IP-Discovery + separate Encryption-Modi + E2EE-Schicht):** für Browser/Mobile-Reichweite gebaut, die ICU nicht hat (Nicht-Ziele: Web, Mobile).
- **TeamSpeak/Ventrilo als Vorlage:** Protokolle nicht veröffentlicht; nichts Belegbares übernehmbar außer "Opus, UDP-Voice".
- **Injection-Overlay:** Nicht-Ziel laut README; Belege oben.
- **Positional Audio, Whisper-Targets, Channel-Codecs:** Features außerhalb des ICU-Zwecks.

## Offene Punkte für Step 3

- Layered-Topmost-Fenster auf Windows über einem Spiel im Borderless-Fullscreen: Sichtbarkeit, Klick-Durchleitung, DWM-Verhalten bei DirectX-12/Vulkan-Spielen messen (Doku belegt nur die API-Semantik, nicht das Verhalten über Spielen).
- wlr-layer-shell auf Hyprland/Sway/KWin verifizieren; GNOME-Verhalten (Mutter ohne Layer-Shell) klären und Fallback (kein Overlay) dokumentieren. X11-Pfad (override-redirect + `_NET_WM_STATE_ABOVE`) — unbelegt, prüfen.
- TeamSpeak-Ports/-Codecs aus einer erreichbaren Primärquelle nachbelegen (support.teamspeak.com lieferte 403) oder als Sekundär belassen.
- Mumbles aktuelle TLS-Konfiguration (overview.md nennt "TLSv1 AES256-SHA", vermutlich veraltet) nur relevant, falls ICU Vergleichszahlen zum Handshake will — sonst streichen.
- Discord-Latenzverhalten: keine Primärquelle; nur eigene Messungen gegen ICU sinnvoll (Mikrofon → Lautsprecher, gleiche LAN-Bedingungen).

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `verification.md`):
  - Spike 4b **bestätigt** den Layer-Shell-Weg auf KWin 6.7.5 (Overlay sichtbar über allen Fenstern, kein Fokus).
  - Keine weiteren empirischen Prüfungen nötig; Vergleichsfakten sind Quellenarbeit.

Mit WebFetch gelesen (2026-09-20):
- https://www.mumble.info/documentation/developer/
- https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/README.md
- https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/overview.md
- https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/protocol_stack_tcp.md
- https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/establishing_connection.md
- https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/voice_data.md
- https://raw.githubusercontent.com/mumble-voip/mumble/master/src/MumbleUDP.proto
- https://raw.githubusercontent.com/mumble-voip/mumble/master/auxiliary_files/mumble-server.ini
- https://www.mumble.info/documentation/user/certificates/
- https://raw.githubusercontent.com/mumble-voip/mumble/master/overlay/lib.cpp
- https://api.github.com/repos/mumble-voip/mumble/contents/overlay
- https://raw.githubusercontent.com/mumble-voip/mumble/master/overlay_gl/overlay.c
- https://api.github.com/repos/mumble-voip/mumble/contents/overlay_gl
- https://api.github.com/repos/mumble-voip/mumble/contents/docs/dev
- https://api.github.com/repos/mumble-voip/mumble/contents/docs/dev/network-protocol
- https://www.teamspeak.com/en/features/overview/
- https://teamspeakdocs.github.io/ClientSDK/client_html/ar01s14.html (Sekundär)
- https://www.ventrilo.com/faq.php
- https://www.ventrilo.com/about.php
- https://www.ventrilo.com/
- https://www.ventrilo.com/changelog.php
- https://www.ventrilo.com/dlprod.php?id=1
- https://en.wikipedia.org/wiki/Ventrilo (Sekundär)
- https://docs.discord.com/developers/topics/voice-connections
- https://docs.discord.com/developers/game-sdk/overlay
- https://support.discord.com/api/v2/help_center/en-us/articles/217659737.json
- https://www.rfc-editor.org/rfc/rfc6716.html
- https://www.rfc-editor.org/rfc/rfc7587.html
- https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features
- https://wayland.app/protocols/wlr-layer-shell-unstable-v1
