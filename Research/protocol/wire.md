# Wire-Protokoll (Entwurf 1)

> **Abgelöst durch `/PROTOCOL.md`** (2026-09-20). Diese Datei bleibt als Entwurfs- und Begründungsprotokoll; bei Abweichungen (Fehlercode-Fatalität, Backoff 15 s, `AUTH_REQUIRED`, `Hello.name`, `Welcome.min_client`, `LossReport`/`LossHint`/`KeyframeRequest`/`SetName`/`UserRenamed`) gilt `PROTOCOL.md`.

## Frage

Wie sieht das ICU-Protokoll über QUIC (Entscheidung in `../transport.md`) auf Byte-Ebene aus: Control-Kanal (Framing, Serialisierung), Audio-Datagram-Header, Video-Frames, Message-Liste, Verbindungs-State-Machine, Versionierung, Fehlercodes — und was muss der Server beim Relay tun, insbesondere: setzt er die Speaker-ID oder vertraut er dem Client?

Alles unter "Entscheidung" ist **Entwurf**, kein Befund; Befunde sind nur die belegten Fakten.

## Kandidaten

- Control-Serialisierung: **postcard** 1.1.3 vs. **bincode** 3.0.0 vs. **handgeschriebenes Binärformat** (vs. protobuf wie Mumble, nur als Vergleich).
- Control-Framing: u16-Längenpräfix vs. QUIC-Varint-Längenpräfix vs. eine Message pro Stream.
- Audio: ein Datagram pro Opus-Paket mit festem Header; Speaker-ID vom Server gesetzt vs. vom Client.
- Video: ein Datagram pro Fragment mit eigener Fragmentierung vs. ein unidirektionaler QUIC-Stream pro Frame.

## Primärquellen

Alle abgerufen am 2026-09-20.

- RFC 9000 QUIC (§16 Varint, §17.3 Short Header, §19.19 CONNECTION_CLOSE, §2 Streams) — https://www.rfc-editor.org/rfc/rfc9000.html
- RFC 9001 (§8.1 ALPN, §5.1 AEAD) — https://www.rfc-editor.org/rfc/rfc9001.html
- RFC 9221 QUIC Datagrams (§5.1 Multiplexing, §5.4 Fragmentierung) — https://www.rfc-editor.org/rfc/rfc9221.html
- RFC 6716 Opus (§3.1 TOC, Frame-Größen, 1275-Byte-Limit) — https://www.rfc-editor.org/rfc/rfc6716.html
- RFC 7587 RTP Payload Format for Opus (§4.1 48-kHz-Clock) — https://www.rfc-editor.org/rfc/rfc7587.html
- crates.io API postcard — https://crates.io/api/v1/crates/postcard
- crates.io API bincode — https://crates.io/api/v1/crates/bincode
- postcard Wire-Format-Spezifikation — https://postcard.jamesmunns.com/wire-format
- docs.rs postcard — https://docs.rs/postcard/latest/postcard/index.html
- docs.rs bincode (Crate-Seite, Status) — https://docs.rs/bincode/latest/bincode/index.html
- bincode 3.0.0 README auf crates.io — https://static.crates.io/readmes/bincode/bincode-3.0.0.html
- bincode README im GitHub-Repo — https://raw.githubusercontent.com/bincode-org/bincode/trunk/README.md
- docs.rs quinn::Connection — https://docs.rs/quinn/latest/quinn/struct.Connection.html
- docs.rs quinn::SendStream — https://docs.rs/quinn/latest/quinn/struct.SendStream.html
- docs.rs quinn::RecvStream — https://docs.rs/quinn/latest/quinn/struct.RecvStream.html
- docs.rs quinn_proto::SendDatagramError — https://docs.rs/quinn-proto/latest/quinn_proto/enum.SendDatagramError.html
- quinn-proto `datagrams.rs` (Größenherleitung) — https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn-proto/src/connection/datagrams.rs
- Mumble `MumbleUDP.proto` (Vergleich Audio-Header) — https://raw.githubusercontent.com/mumble-voip/mumble/master/src/MumbleUDP.proto
- Mumble `voice_data.md` (Vergleich) — https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/voice_data.md
- Mumble `protocol_stack_tcp.md` (Vergleich Control-Framing) — https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/protocol_stack_tcp.md
- Discord Voice Connections (Vergleich RTP-Header) — https://docs.discord.com/developers/topics/voice-connections

## Befunde

### Transport-Fakten, die das Format bestimmen

- Demultiplexing von Datagrams ist Sache der Anwendung; RFC 9221 §5.1 empfiehlt einen Identifier am Datagram-Anfang. Datagrams können nicht fragmentiert werden (§5.4); quinn-Datagrams müssen "fit inside a single QUIC packet" (docs.rs Connection). Maximalgröße zur Laufzeit: `Connection::max_datagram_size()`; bei 1200-Byte-Initial-MTU konservativ ≈ 1150 Byte nutzbar (Herleitung in `../transport.md`).
- Streams sind geordnete Byte-Streams ohne Message-Grenzen (RFC 9000 §2) → Framing im Control-Kanal nötig. `read_to_end(size_limit)` begrenzt einen Stream-Body (`TooLong`). `SendStream::set_priority(i32)`: höher priorisierte Stream-Daten werden zuerst gesendet. "The Connection that calls `open_bi()` must write to its SendStream before the other Connection is able to `accept_bi()`."
- QUIC-Varint: 1/2/4/8 Byte, bis 2^62−1 (RFC 9000 §16). Application Error Codes in CONNECTION_CLOSE sind Varints (§19.19).
- ALPN ist Pflicht und schließt bei Mismatch mit `no_application_protocol` (RFC 9001 §8.1).
- `send_datagram` verwirft bei vollem Puffer ältere Datagrams; `SendDatagramError::TooLarge`, `Blocked`, `UnsupportedByPeer`, `Disabled` (quinn-proto).

### Opus

- Frame-Dauern 2,5/5/10/20/40/60 ms, bis 120 ms pro Paket; 6–510 kbit/s; TOC-Byte = config (5 Bit) + stereo (1 Bit) + code (2 Bit); "The length of any individual frame MUST NOT exceed 1275 bytes" (RFC 6716 §3.1, §3.2). Das TOC-Byte ist Teil des Opus-Pakets, also braucht der ICU-Header keine Codec-Parameter. In-band FEC und PLC sind Decoder-Sache (RFC 6716).
- RTP-Clock: "The RTP timestamp is incremented with a 48000 Hz clock rate for all modes of Opus and all sampling rates" (RFC 7587 §4.1); 20 ms = 960 Samples; bei DTX müssen aufeinanderfolgende Timestamps Vielfache von 120 bleiben (§3.1.3).

### Serialisierung

- **postcard 1.1.3** (crates.io, aktualisiert 2025-07-24 laut API; docs.rs-Seite nennt 2026-09-01 — Datumsdrift zwischen crates.io-API-Feld und docs.rs-Anzeige, Version identisch): Wire-Format "stable as of v1.0.0", Brüche nur mit 2.0; nicht selbstbeschreibend ("mutual understanding of the encoded data"); Integers als varint/LEB128, Signed mit Zigzag; Enums = varint(u32)-Diskriminante + Daten; Option = 1 Byte + Daten; Strings/Sequenzen mit varint-Längenpräfix; Structs ohne Präfix in Feldreihenfolge (Wire-Format-Spec). Features `use-std`, `alloc`, `heapless`; API `to_slice`, `to_allocvec`, `from_bytes`, `take_from_bytes` (docs.rs).
- **bincode 3.0.0** (crates.io, 2025-12-16): README auf crates.io: "Due to a doxxing and harassment incident, development on bincode has ceased. No further releases will be published on crates.io."; empfohlene Alternativen wincode, postcard, rkyv. docs.rs: Build von 3.0.0 fehlgeschlagen, letzter erfolgreicher Build 2.0.1. GitHub-README sagt dagegen "This repo has migrated to sourcehut" — **Drift zwischen crates.io-README und Repo-README**; für crates.io-Konsumenten gilt die crates.io-Aussage.
- Vergleich Mumble: TCP-Control = 2 Byte Typ + 4 Byte Länge (big-endian) + protobuf; 26 Typen (`protocol_stack_tcp.md`). Mumble-Audio (1.5, `MumbleUDP.proto`): `sender_session` "is not required when sending audio to the server, but will always be set when receiving audio from the server"; `frame_number`, `opus_data`, `is_terminator`; `voice_data.md`: Session-ID "the server derives this from outgoing packet transport layer information". Discord: 12-Byte-RTP-Header (0x80, 0x78, u16 Seq BE, u32 Timestamp BE, u32 SSRC BE) + verschlüsseltes Opus; Opus 48 kHz stereo.

## Entscheidung

### Übersicht

| Kanal | QUIC-Primitiv | Richtung | Format |
| --- | --- | --- | --- |
| Control | 1 bidirektionaler Stream, vom Client eröffnet, lebt bis Close | beide | u16-LE-Längenpräfix + postcard-Body |
| Audio | DATAGRAM | Client→Server, Server→Clients | fester 10-Byte-Header + Opus-Paket |
| Video | 1 unidirektionaler Stream pro Frame | Client→Server, Server→Clients | fester 12-Byte-Header + Frame-Bytes, Stream `finish()` |
| Enrollment | ALPN `icu-enroll/1`, 1 bidirektionaler Stream | beide | wie Control, eigene Message-Menge |

Byte-Order in handgeschriebenen Headern: **little-endian** (Rust-Zielplattformen x86_64/aarch64 sind LE; kein Konvertierungsaufwand; einheitlich mit postcard-Varints, die byteweise sind). Abweichung von RTP/Mumble (BE) ist bewusst; es gibt keine Interop.

### Control-Kanal

Framing: `len: u16 LE` (Bytes des Bodys, 0 verboten, max 16384 — größere Frames → `PROTOCOL_VIOLATION`), dann `body = postcard(ControlMsg)`. u16 statt Varint, weil ein festes 2-Byte-Feld ohne Parser-Zustand gelesen werden kann (`read_exact(2)` → `read_exact(len)`); 64 KiB reichen für alle Messages (ServerState mit 256 Usern à ≤ 100 Byte ≈ 26 KiB; Cert-DER für Renewal ≤ 2 KiB).

Serialisierung: **postcard** mit `serde`. Ein Rust-Enum `ControlMsg` in einem gemeinsamen Crate (`icu-proto`) ist die Wire-Definition; postcard kodiert es als varint-Diskriminante + Felder. Reihenfolge der Varianten und Felder ist damit Protokoll — Änderungen nur additiv am Ende und nur mit Capability-Gate (siehe Versionierung).

Message-Liste (Entwurf; `u16 session` = serverseitige Session-ID, `Name` = UTF-8 ≤ 32 Byte):

| Nr | Message | Richtung | Felder | Zweck |
| --- | --- | --- | --- | --- |
| 0 | `Hello` | C→S | `minor: u16`, `caps: u32`, `client: String` (Build-Info ≤ 64 B), `initial: UserFlags` | Erste Message nach Handshake; Server wartet max. 5 s darauf |
| 1 | `Welcome` | S→C | `minor: u16`, `caps: u32`, `you: u16`, `audio: AudioParams { sample_rate: u32 = 48000, frame_ms: u8 = 20, max_bitrate: u32 }`, `users: Vec<UserInfo>` | Vollständiger Server-State beim Join |
| 2 | `UserJoined` | S→C | `UserInfo { session: u16, name: String, flags: UserFlags }` | |
| 3 | `UserLeft` | S→C | `session: u16`, `reason: u8` | |
| 4 | `UserState` | S→C | `session: u16`, `flags: UserFlags` | Broadcast nach jeder Flag-Änderung |
| 5 | `SetState` | C→S | `flags: UserFlags` | Mute/Deafen/Speaking/Camera in einem Bitfeld |
| 6 | `Ping` | C→S | `nonce: u64` | Anwendungs-RTT; Keepalive macht QUIC (`keep_alive_interval`) |
| 7 | `Pong` | S→C | `nonce: u64`, `server_time_ms: u64` | |
| 8 | `Error` | S→C | `code: u32`, `detail: String` | Nicht-fatale Fehler; fatale via CONNECTION_CLOSE |
| 9 | `RenewRequest` | C→S | `csr_der: Vec<u8>` | Cert-Renewal (siehe `../auth-enrollment.md`) |
| 10 | `RenewResponse` | S→C | `leaf_der: Vec<u8>` | |

Zuordnung zur Aufgabenliste: Hello/Version = `Hello`/`Welcome` (+ ALPN); ServerState = `Welcome`; UserJoined/Left = `UserJoined`/`UserLeft`; SpeakingState, Mute/Deafen, CameraOn/Off = `SetState` (C→S) bzw. `UserState` (S→C) über `UserFlags`; Ping = `Ping`/`Pong`; Error = `Error` bzw. CONNECTION_CLOSE-Code.

`UserFlags: u8` Bitfeld: bit0 `muted`, bit1 `deafened`, bit2 `speaking`, bit3 `camera_on`, bit4–7 reserviert (0 senden, beim Lesen ignorieren). `speaking` sendet der Client bei PTT-Druck/-Release bzw. VAD-Übergang (Server begrenzt auf 20 Änderungen/s pro Client); zusätzlich markiert das Audio-Flag `END` das Ende eines Talk-Spurts für die Jitter-Buffer der Empfänger. `deafened` impliziert serverseitig: kein Audio/Video an diesen Client. `camera_on`: Empfänger wissen, ob Video-Streams zu erwarten sind.

Enrollment-Messages (nur auf ALPN `icu-enroll/1`, gleiche Framing-Regel, eigenes Enum `EnrollMsg`): `EnrollRequest { token: String, csr_der: Vec<u8> }` (C→S), `EnrollResponse { leaf_der: Vec<u8>, ca_der: Vec<u8> }` (S→C), `Error`. Stream-Limit 8 KiB.

### Audio-Datagram

Ein Datagram = genau ein Opus-Paket (ein 20-ms-Frame; TOC im Payload). Header 10 Byte:

| Offset | Größe | Feld | Inhalt |
| --- | --- | --- | --- |
| 0 | 1 | `kind` | `0x01` = Audio v1. Andere Werte: Datagram verwerfen. (RFC 9221 §5.1: Demux-ID am Anfang) |
| 1 | 1 | `flags` | bit0 `END` (letztes Paket des Talk-Spurts, vgl. Mumble `is_terminator`), bit1–7 reserviert = 0 |
| 2 | 2 | `speaker` | u16 LE Session-ID des Sprechers. **Client sendet 0, Server überschreibt.** |
| 4 | 2 | `seq` | u16 LE, pro Sender monoton, +1 je Paket, Wrap. Für Loss/Reorder-Erkennung (bei 20 ms: Wrap alle ~21 min) |
| 6 | 4 | `ts` | u32 LE, 48-kHz-Sample-Clock des Senders (RFC 7587 §4.1), +960 je 20-ms-Frame, Wrap nach ~24,9 h. Bei DTX/PTT-Pausen springt `ts`, `seq` nicht → Empfänger unterscheidet Pause von Verlust |
| 10 | n | `opus` | Opus-Paket (RFC 6716), n ≥ 1 |

Größenregel: n ≤ `max_datagram_size() − 10`; Client kappt die Opus-Bitrate so, dass 20-ms-Pakete ≤ 400 Byte bleiben (≈ 160 kbit/s), weit unter dem ≈ 1150-Byte-Budget und unter dem Opus-Frame-Maximum von 1275 Byte. Server verwirft Datagrams < 11 Byte oder > 1200 Byte.

Warum kein u64-Timestamp, keine Sender-Uhrzeit: Jitter-Buffer arbeiten relativ (Differenzen modulo 2^32); absolute Zeit gibt es über `Pong.server_time_ms`, falls je gebraucht. Warum u16 `speaker` statt u8: Session-IDs werden pro Server-Laufzeit monoton vergeben und nicht wiederverwendet, damit verspätete Datagrams eines gegangenen Users nicht dem Nachfolger zugeordnet werden; u8 würde nach 255 Joins wrappen.

### Video-Frames

Ein unidirektionaler Stream pro Frame (Sender: `open_uni` → Header + Bytes → `finish()`). Header 12 Byte:

| Offset | Größe | Feld | Inhalt |
| --- | --- | --- | --- |
| 0 | 1 | `kind` | `0x02` = Video v1 |
| 1 | 1 | `flags` | bit0 `KEYFRAME`, bit1–7 reserviert |
| 2 | 2 | `speaker` | u16 LE, Client sendet 0, Server setzt |
| 4 | 2 | `seq` | u16 LE Frame-Zähler |
| 6 | 4 | `ts` | u32 LE, dieselbe 48-kHz-Sender-Clock wie Audio (ermöglicht A/V-Zuordnung ohne zweite Uhr; keine Interop, daher kein RTP-Video-Takt nötig) |
| 10 | 1 | `codec` | Codec-ID (Wert wird in der Video-Research festgelegt; `0` = undefiniert) |
| 11 | 1 | reserviert | 0 |
| 12 | n | `frame` | Codierter Frame, Ende = Stream-FIN |

Begründung Stream statt Datagram: Frames überschreiten das Datagram-Budget um ein Vielfaches; QUIC-Streams fragmentieren, retransmittieren und ordnen selbst, Head-of-Line-Blocking bleibt auf den einen Frame beschränkt (RFC 9000 §2.3). Ein alter, noch nicht zugestellter Frame kann per `SendStream::reset()` verworfen werden. Empfänger: `read_to_end(limit)` mit `limit` = 512 KiB. Priorität: Video-Streams `set_priority(-1)`, Control-Stream `set_priority(1)`, damit Stream-Daten den Control-Kanal nicht verzögern; ob Datagrams vor Streams gepackt werden, ist unbelegt (Step 3).

### Verbindungs-State-Machine (Server-Sicht, pro Client)

```
Incoming ──accept──▶ Handshaking ──TLS ok──▶ Authenticating
                                   │                 │ Fingerprint in Allowlist, nicht revoked
                       TLS-Fehler  ▼                 ▼
                       (kein App-Code)          AwaitHello ──Hello (≤ 5 s)──▶ Active
                                                     │ Timeout/ungültig            │
                                                     ▼                             │ Close/Idle/Kick/Shutdown
                                                  Closed ◀─────────────────────────┘
```

- `Authenticating`: `peer_identity()` → Leaf-DER → SHA-256 → Lookup. Fehlschlag: `close(AUTH_UNKNOWN_CERT | AUTH_REVOKED, "")`.
- `AwaitHello`: Server `accept_bi()`; erster Frame muss `Hello` sein; Versionsprüfung `minor`; dann `Welcome`, Broadcast `UserJoined`, Übergang `Active`.
- `Active`: Audio-Datagrams und Video-Streams werden nur in `Active` relayt; Control-Messages verarbeitet; Ping/Pong.
- `Closed`: Broadcast `UserLeft`, Session-ID bleibt gesperrt.
- Client-Sicht spiegelbildlich: `Connecting → Handshaking → SendHello → AwaitWelcome → Active → Closed`; bei `Closed` mit `TIMEOUT`/Netzfehler automatischer Reconnect mit Backoff (1 s, 2 s, 4 s, max 30 s), 1-RTT (kein 0-RTT).

### Versionierung

- **Major:** ALPN-String `icu/1`. Inkompatible Änderungen → `icu/2`; Server kann mehrere anbieten, quinn liefert das Ergebnis in `HandshakeData.protocol`. Mismatch endet im TLS-Handshake (RFC 9001 §8.1).
- **Minor + Capabilities:** `Hello.minor`/`Welcome.minor` (u16) und `caps: u32` Bitfeld. Regel: eine Seite sendet nur Messages/Flags, die die Gegenseite per `caps` angekündigt hat; neue Enum-Varianten werden nur hinten angefügt; unbekannte Varianten sind ein postcard-Decode-Fehler → `PROTOCOL_VIOLATION`. Deshalb ist das Capability-Gate Pflicht, nicht optional.
- Header-`kind`-Bytes tragen die Version im Wert (`0x01` = Audio v1); ein Audio-v2-Header bekäme `0x03`.

### Fehlercodes

Anwendungs-Fehlercodes, identisch in `CONNECTION_CLOSE` (Varint, RFC 9000 §19.19), in `Error.code` und in `UserLeft.reason` (dort auf u8 gekürzt, Werte < 256):

| Code | Name | Fatal | Bedeutung |
| --- | --- | --- | --- |
| 0x00 | `NO_ERROR` | – | normales Close |
| 0x01 | `PROTOCOL_VIOLATION` | ja | Framing/Decode-Fehler, falsche erste Message, Limits überschritten |
| 0x02 | `VERSION_UNSUPPORTED` | ja | `Hello.minor` nicht bedienbar |
| 0x03 | `AUTH_UNKNOWN_CERT` | ja | Cert gültig gegen CA, Fingerprint unbekannt |
| 0x04 | `AUTH_REVOKED` | ja | Fingerprint revoked |
| 0x05 | `SERVER_FULL` | ja | Max. User erreicht |
| 0x06 | `KICKED` | ja | Admin-Aktion |
| 0x07 | `HELLO_TIMEOUT` | ja | kein `Hello` binnen 5 s |
| 0x08 | `SHUTDOWN` | ja | Server fährt herunter |
| 0x09 | `RATE_LIMITED` | nein | Message/Datagram-Rate überschritten (nach 3× fatal) |
| 0x10 | `ENROLL_BAD_TOKEN` | ja | Token unbekannt/verbraucht/abgelaufen |
| 0x11 | `ENROLL_BAD_CSR` | ja | CSR nicht parsebar/Signatur ungültig |
| 0x12 | `ENROLL_RATE_LIMITED` | ja | Enrollment-Limit |
| 0x13 | `RENEW_REJECTED` | nein | Renewal-CSR abgelehnt |

### Was der Server beim Relay tut

Audio (pro empfangenem Datagram):
1. Länge ≥ 11 und `kind == 0x01`, sonst verwerfen (Zähler, kein Close; nach 100 fehlerhaften Datagrams/min `PROTOCOL_VIOLATION`).
2. Absender in `Active`, nicht `muted` (Server-State ist maßgeblich, nicht das Client-Verhalten), Rate ≤ 110 Datagrams/s (erlaubt 10-ms-Frames mit Reserve).
3. `speaker` (Bytes 2–3) mit der Session-ID der Verbindung überschreiben — in place in einem `BytesMut`, dann `freeze()`.
4. Für jeden anderen `Active`-Client, der nicht `deafened` ist: `send_datagram(bytes.clone())`; `TooLarge`/`Blocked`/`UnsupportedByPeer` → dieses Datagram für diesen Empfänger verwerfen, Zähler erhöhen, weiter. Kein Decode, kein Mixing, kein Puffern.

**Speaker-ID: der Server setzt sie.** Begründung: Die Identität des Absenders ist dem Server über die mTLS-Verbindung zweifelsfrei bekannt; ein clientgesetztes Feld müsste der Server ohnehin gegen diese Zuordnung prüfen (gleicher Aufwand: 2-Byte-Vergleich statt 2-Byte-Schreiben), sonst könnte ein Client Sprache unter fremder ID einspeisen. Mumble macht es genauso (`sender_session` wird vom Server gesetzt, "derives this from outgoing packet transport layer information"). Das Überschreiben ist ein Byte-Write ohne Parsing des Opus-Payloads; die Latenzkosten sind null. `seq` und `ts` bleiben clientgesetzt: sie betreffen nur den Jitter-Buffer der Empfänger für genau diesen Sprecher, ein lügender Client schadet nur sich selbst.

Video (pro `accept_uni` vom Sender): 12-Byte-Header per `read_exact`, `kind == 0x02` prüfen, Absender `camera_on` und `Active`; `speaker` setzen; für jeden Empfänger, der nicht `deafened` ist: `open_uni`, Header schreiben, dann Chunks per `read_chunk` → `write_chunk` durchreichen (kein Frame-Buffering), `finish()`. Wenn ein Empfänger-Stream blockiert (Flow-Control) und der nächste Frame desselben Senders bereits eintrifft: alten Stream `reset(0x09)` — Frame-Drop pro Empfänger statt Latenzaufbau. Server-Limits: max 1 offener Video-Stream pro Sender in Bearbeitung + 1 wartend; Frame ≤ 512 KiB.

Control: Server verarbeitet nur Messages des eigenen Streams; Broadcasts (`UserJoined`/`UserLeft`/`UserState`) schreibt er auf den Control-Stream jedes Clients. Der Server öffnet nie selbst Streams außer für Video-Relay.

## Verworfen, mit Grund

- **bincode:** laut crates.io-README eingestellt, keine weiteren Releases; docs.rs-Build von 3.0.0 fehlgeschlagen. Kein Wire-Format-Versprechen, das an postcards Spezifikation heranreicht.
- **Handgeschriebenes Format für Control:** möglich, aber jede Message braucht Encoder/Decoder-Paare und Tests; postcard liefert das aus `#[derive(Serialize, Deserialize)]` mit spezifiziertem Wire-Format. Handgeschrieben bleibt nur, wo der Server in-place schreiben muss (Audio-/Video-Header).
- **protobuf (Mumble-Modell):** Codegen, Descriptor-Abhängigkeit, selbstbeschreibende Feld-Tags, die niemand braucht, wenn beide Seiten dasselbe Rust-Crate teilen.
- **Varint-Längenpräfix im Control-Kanal:** spart 1 Byte bei kleinen Messages, kostet einen zustandsbehafteten Reader; u16 ist trivial und ausreichend.
- **Eine Message pro Stream (Control):** Stream-Open/Close pro Ping/State-Update ist Overhead ohne Nutzen; Ordnung der Control-Messages ist gewollt.
- **Video als Datagram-Fragmente:** eigene Fragmentierung, eigener Reassembly-Puffer, eigene Drop-Policy für unvollständige Frames — alles, was QUIC-Streams bereits tun. Erst dann sinnvoll, wenn Step 3 zeigt, dass Stream-Retransmits Video sichtbar verzögern.
- **Client-gesetzte Speaker-ID:** siehe oben; Prüfen kostet dasselbe wie Setzen, Nicht-Prüfen erlaubt Impersonation.
- **Big-Endian-Header:** keine Interop mit RTP; LE spart auf allen Zielplattformen Konvertierung.
- **Speaking-State nur aus Audio abgeleitet (kein `speaking`-Flag):** deafened Clients bekämen keinen Talk-Indikator; das Flag ist eine Message pro Talk-Spurt, vernachlässigbar.

## Offene Punkte für Step 3

- Header-Layout gegen `max_datagram_size()` verifizieren: ≥ 400-Byte-Opus-Pakete bei 1200-MTU relayfähig?
- In-place-Überschreiben von `speaker` mit `Bytes`/`BytesMut` ohne Kopie pro Empfänger (Clone eines `Bytes` ist Refcount; messen bei 16 Empfängern × 50 Pakete/s).
- Verhalten des Empfängers bei `seq`-Sprung ohne `ts`-Sprung (Verlust) vs. `ts`-Sprung mit `seq+1` (PTT-Pause): Jitter-Buffer-Logik testen; DTX-Ausgabe von libopus mit 48-kHz-Timestamps kontrollieren (Vielfache von 120, RFC 7587 §3.1.3).
- Video-Relay über Streams: Latenz Frame-Ende → Empfänger bei 1 % Verlust; wann `reset()` greift; Effekt von `set_priority` auf Control-Latenz unter Video-Last.
- Ob DATAGRAM-Frames in quinn vor Stream-Frames gepackt werden (Audio-Jitter bei laufendem Video): unbelegt, messen.
- postcard-Decoding unbekannter Enum-Varianten liefert erwartungsgemäß `Err` (nicht Panic) — testen, weil `PROTOCOL_VIOLATION` darauf beruht.
- 5-s-Hello-Timeout und 16-KiB-Frame-Limit gegen reale `Welcome`-Größen bei max. Usern prüfen.
- Enrollment-Stream-Größen (CSR + Token) gegen das 8-KiB-Limit; Cert-DER-Größen von rcgen messen.

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `verification.md`):
  - Spike 2b **bestätigt**: postcard-Decode einer unbekannten Enum-Variante liefert `Err` (kein Panic); `Hello{minor:3}` = 2 Bytes.
  - Spike 2 **bestätigt**: 400-Byte-Opus-Pakete + 10-Byte-Header passen mit großem Abstand in `max_datagram_size()` (≥ 1162 B).
  - Spike 3 **bestätigt**: 10-ms-Frames sind praktikabel (Encode 90 µs/Frame); die Rate-Grenze 110 Datagrams/s bleibt richtig.
  - **Ergänzung aus `codec.md`** (C10): Verlust-Feedback-Message `LossReport { speaker: u16, loss_permille: u16 }` C→S (Server leitet an den Sprecher weiter, der `PACKET_LOSS_PERC` setzt) wird in `PROTOCOL.md` aufgenommen; ebenso `KeyframeRequest { speaker: u16 }` für H.264 (siehe `camera-video.md`, revidiert).
  - Offen: In-place-Überschreiben ohne Kopie messen, Video-Relay-Latenz, Datagram-vs-Stream-Priorisierung.

Mit WebFetch gelesen (2026-09-20):
- https://www.rfc-editor.org/rfc/rfc9000.html
- https://www.rfc-editor.org/rfc/rfc9001.html
- https://www.rfc-editor.org/rfc/rfc9221.html
- https://www.rfc-editor.org/rfc/rfc6716.html
- https://www.rfc-editor.org/rfc/rfc7587.html
- https://crates.io/api/v1/crates/postcard
- https://crates.io/api/v1/crates/bincode
- https://postcard.jamesmunns.com/wire-format
- https://docs.rs/postcard/latest/postcard/index.html
- https://docs.rs/bincode/latest/bincode/index.html
- https://static.crates.io/readmes/bincode/bincode-3.0.0.html
- https://raw.githubusercontent.com/bincode-org/bincode/trunk/README.md
- https://docs.rs/quinn/latest/quinn/struct.Connection.html
- https://docs.rs/quinn/latest/quinn/struct.SendStream.html
- https://docs.rs/quinn/latest/quinn/struct.RecvStream.html
- https://docs.rs/quinn-proto/latest/quinn_proto/enum.SendDatagramError.html
- https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn-proto/src/connection/datagrams.rs
- https://raw.githubusercontent.com/mumble-voip/mumble/master/src/MumbleUDP.proto
- https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/voice_data.md
- https://raw.githubusercontent.com/mumble-voip/mumble/master/docs/dev/network-protocol/protocol_stack_tcp.md
- https://docs.discord.com/developers/topics/voice-connections
