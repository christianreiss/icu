# ICU Wire-Protokoll v1

Stand 2026-09-20. Abgeleitet aus `Research/transport.md`, `Research/protocol/wire.md`, `Research/auth-enrollment.md`, `Research/codec.md`, `Research/camera-video.md` und den Spike-Ergebnissen in `Research/verification.md`. Diese Datei ist die normative Fassung; die Rust-Typen im Crate `icu-proto` müssen ihr entsprechen.

## 1. Transport

| Punkt | Festlegung |
| --- | --- |
| Transport | QUIC (RFC 9000/9001) mit Unreliable Datagrams (RFC 9221). Implementierung quinn 0.11 + rustls 0.23, Crypto-Provider ring. |
| Port | Ein UDP-Port, Default 4711. Server bindet IPv4 und IPv6 getrennt (quinn-Dual-Stack-Hinweis). |
| TLS | TLS 1.3, Server-Cert und Client-Cert von der ICU-CA des Servers. Client vertraut nur dieser CA. |
| ALPN | `icu/1` (Hauptprotokoll), `icu-enroll/1` (Enrollment). ALPN ist das Major-Versions-Gate. |
| Client-Auth | Server: `WebPkiClientVerifier` mit `allow_unauthenticated()`; Pflicht wird pro ALPN in der Anwendung erzwungen (Spike 2b: ALPN per `handshake_data()` vor Handshake-Ende lesbar, `peer_identity()` ohne Cert = `None`). |
| 0-RTT | Aus (`max_early_data_size = 0`, kein `into_0rtt()`). |
| Idle / Keepalive | `max_idle_timeout` 60 s beidseitig; Client `keep_alive_interval` 15 s. |
| Migration | quinn-Default (erlaubt). |
| Datagram-Puffer | Server und Client `datagram_receive_buffer_size` = 64 KiB, `datagram_send_buffer_size` = 64 KiB (klein, damit `send_datagram` alte Frames verwirft statt Latenz aufzubauen). Wert in Implementierung messen. |
| Streams | Client eröffnet genau einen bidirektionalen Control-Stream. Video: ein unidirektionaler Stream pro Frame, in beide Richtungen. `max_concurrent_uni_streams` = 8 pro Richtung. |
| Datagram-Größe | Laufzeitwert `max_datagram_size()` (Spike 2: 1162 B nach Handshake, 1414 B nach MTU-Discovery). Audio-Datagrams sind ≤ 410 B. |

**Verbunden heißt `Welcome` empfangen.** Auf Client-Seite liefert `connect().await` auch dann `Ok`, wenn der Server das Client-Cert gleich darauf ablehnt (Spike 2). Der Client darf vor `Welcome` weder Audio senden noch „verbunden“ anzeigen.

## 2. Control-Kanal

Framing auf dem Control-Stream in beide Richtungen:

| Offset | Größe | Feld |
| --- | --- | --- |
| 0 | 2 | `len` u16 LE, Länge des Bodys in Bytes, 1 ≤ len ≤ 16384 |
| 2 | len | `body` = postcard-Serialisierung von `ControlMsg` |

`len = 0` oder `len > 16384` → `PROTOCOL_VIOLATION`. postcard 1.x mit serde; Enum-Diskriminante = varint. Neue Varianten nur am Ende, Felder nur am Ende, jeweils hinter einem Capability-Bit.

### 2.1 Typen

```
UserFlags: u8      bit0 muted, bit1 deafened, bit2 speaking, bit3 camera_on, bit4–7 reserviert (senden 0, lesen ignorieren)
                   bit2 ist serverseitig autoritativ: der Server leitet es aus dem Audio-Datagram-Fluss ab (erstes Datagram nach ≥ 500 ms Stille → 1; END-Flag oder 500 ms ohne Datagram → 0) und broadcastet UserState; ein von Clients in SetState gesendetes bit2 wird ignoriert.
Session:   u16     serverseitig vergeben, ab 1, pro Server-Laufzeit monoton, nie wiederverwendet; 0 = „unbekannt/ich“
Name:      String  UTF-8, 1–32 Bytes, keine Steuerzeichen
UserInfo { session: Session, name: Name, flags: UserFlags }
AudioParams { sample_rate: u32 = 48000, frame_ms: u8 = 20, max_bitrate: u32 = 64000 }
```

### 2.2 Messages `ControlMsg`

| Nr | Variante | Richtung | Felder | Semantik |
| --- | --- | --- | --- | --- |
| 0 | `Hello` | C→S | `minor: u16`, `caps: u32`, `client: String` (≤ 64 B), `initial: UserFlags`, `name: Name` | Erste Message nach Handshake, binnen 5 s. `name` ist der Anzeigename (Server nimmt ihn, wenn er sich vom gespeicherten unterscheidet, und persistiert ihn zum Fingerprint). |
| 1 | `Welcome` | S→C | `minor: u16`, `caps: u32`, `you: Session`, `audio: AudioParams`, `users: Vec<UserInfo>` (ohne sich selbst), `min_client: String` | Vollständiger State. `min_client` = niedrigste erlaubte Client-Version (SemVer-String) für den Update-Hinweis. |
| 2 | `UserJoined` | S→C | `user: UserInfo` | |
| 3 | `UserLeft` | S→C | `session: Session`, `reason: u8` (Fehlercode < 256) | |
| 4 | `UserState` | S→C | `session: Session`, `flags: UserFlags` | Broadcast nach jeder Änderung, inkl. an den Verursacher. |
| 5 | `SetState` | C→S | `flags: UserFlags` | Server begrenzt auf 20/s pro Client, darüber `RATE_LIMITED`. |
| 6 | `Ping` | C→S | `nonce: u64` | Anwendungs-RTT; Intervall 5 s. |
| 7 | `Pong` | S→C | `nonce: u64`, `server_time_ms: u64` | |
| 8 | `Error` | S→C | `code: u32`, `detail: String` | Nicht-fatal. Fatale Fehler gehen über CONNECTION_CLOSE. |
| 9 | `RenewRequest` | C→S | `csr_der: Vec<u8>` (≤ 2 KiB) | Cert-Renewal über die authentifizierte Verbindung, ab 30 Tage vor Ablauf. |
| 10 | `RenewResponse` | S→C | `leaf_der: Vec<u8>` | Alter Fingerprint bleibt bis Ablauf gültig. |
| 11 | `LossReport` | C→S | `speaker: Session`, `loss_permille: u16` | Vom Empfänger gemessener Verlust des Audio-Streams eines Sprechers über die letzten 2 s. Intervall 2 s, nur wenn sich der Wert um ≥ 10 ‰ ändert. |
| 12 | `LossHint` | S→C | `loss_permille: u16` | Server leitet das Maximum aller `LossReport` für diesen Sprecher weiter; der Client setzt `OPUS_SET_PACKET_LOSS_PERC` (auf ganze Prozent gerundet, ≤ 20). |
| 13 | `KeyframeRequest` | C→S | `speaker: Session` | Empfänger braucht ein H.264-IDR (Join, Verlust). Server leitet als `KeyframeRequest { speaker: 0 }` an den Sprecher weiter; der erzeugt binnen 100 ms einen IDR. Rate ≤ 1/s pro Sprecher. |
| 14 | `SetName` | C→S | `name: Name` | Namensänderung; Server broadcastet `UserRenamed`. |
| 15 | `UserRenamed` | S→C | `session: Session`, `name: Name` | |

Capability-Bits (`caps`), v1 alle 0; reserviert für spätere Erweiterungen. Eine Seite sendet nur Varianten ≥ 11, wenn die Gegenseite dasselbe Minor ≥ 1 gemeldet hat; in v1 ist `minor = 1` und alle 16 Varianten sind Pflicht.

### 2.3 Enrollment `EnrollMsg` (nur ALPN `icu-enroll/1`)

Gleiche Framing-Regel, Stream-Limit 8 KiB, ein Request/Response, danach Close.

| Nr | Variante | Richtung | Felder |
| --- | --- | --- | --- |
| 0 | `EnrollRequest` | C→S | `token: String` (base64url, 22 Zeichen = 128 Bit), `csr_der: Vec<u8>` (≤ 2 KiB), `name: Name` (gewünschter Anzeigename; der Server nimmt ihn als initialen Anzeigenamen, das Cert-CN kommt immer aus dem Invite) |
| 1 | `EnrollResponse` | S→C | `leaf_der: Vec<u8>`, `ca_der: Vec<u8>`, `server_name: String` (SNI für die Hauptverbindung) |
| 2 | `Error` | S→C | `code: u32`, `detail: String` |

Server-Pinning beim Enrollment: der Client verifiziert die Serverkette nicht gegen einen Root-Store, sondern per eigenem `ServerCertVerifier`, der SHA-256 des letzten Kettenglieds mit dem `ca`-Wert aus dem Invite-Link vergleicht. Der Server sendet daher immer die Kette Leaf + CA. Server-Seite: CSR-Subject, SAN, EKU und Validity werden vom Server gesetzt, nicht aus der CSR übernommen (Spike 2b).

Invite-Link: `icu://<host>:<port>/enroll?t=<token>&ca=<sha256-base64url>&n=<name-urlencoded>`. Token einmalig, 24 h gültig.

## 3. Audio-Datagram

Genau ein Opus-Paket pro Datagram. Header 10 Byte, little-endian:

| Offset | Größe | Feld | Inhalt |
| --- | --- | --- | --- |
| 0 | 1 | `kind` | `0x01` = Audio v1. Andere Werte verwerfen. |
| 1 | 1 | `flags` | bit0 `END` (letztes Paket eines Talk-Spurts), bit1–7 = 0 |
| 2 | 2 | `speaker` | Session-ID des Sprechers. **Client sendet 0, Server überschreibt in place.** |
| 4 | 2 | `seq` | pro Sender +1 je Paket, Wrap |
| 6 | 4 | `ts` | 48-kHz-Sample-Clock des Senders, +960 je 20-ms-Frame (+480 bei 10 ms); springt bei PTT-Pausen, `seq` nicht |
| 10 | n | `opus` | Opus-Paket (TOC enthalten), 1 ≤ n ≤ 400 |

Regeln: Server verwirft Datagrams < 11 B oder > 410 B; Empfänger unterscheidet Pause (`ts`-Sprung bei lückenloser `seq`) von Verlust (`seq`-Lücke). Opus-Profil: 48 kHz mono, VOIP, Frame 20 ms Default (10 ms Client-Option ohne Protokolländerung, TOC trägt die Dauer), 48 kbit/s constrained VBR, FEC an, DTX aus, Complexity 10.

Server-Relay pro Datagram: Länge/`kind` prüfen → Absender `Active` und nicht `muted` → Rate ≤ 110/s → `speaker` setzen → `send_datagram` an alle anderen `Active`, nicht `deafened`; Sendefehler pro Empfänger zählen, nie blockieren.

## 4. Video-Frame (unidirektionaler Stream)

Header 12 Byte, dann Frame-Bytes bis Stream-FIN. Empfänger `read_to_end(512 KiB)`.

| Offset | Größe | Feld | Inhalt |
| --- | --- | --- | --- |
| 0 | 1 | `kind` | `0x02` = Video v1 |
| 1 | 1 | `flags` | bit0 `KEYFRAME` (IDR), bit1–7 = 0 |
| 2 | 2 | `speaker` | Client sendet 0, Server setzt |
| 4 | 2 | `seq` | Frame-Zähler pro Sender |
| 6 | 4 | `ts` | dieselbe 48-kHz-Clock wie Audio |
| 10 | 1 | `codec` | `1` = H.264 Constrained Baseline (Annex-B-NALs, SPS/PPS vor jedem IDR), `2` = MJPEG (Baseline JFIF). v1-Clients müssen 1 decodieren, 2 optional. |
| 11 | 1 | reserviert | 0 |

Profil: 320×240 @ 15 fps, Ziel 150 kbit/s, IDR alle 2 s und auf `KeyframeRequest`. Server-Relay: Header lesen, `camera_on` und `Active` prüfen, `speaker` setzen, Chunks durchreichen, `finish()`. Pro Sender max. 1 Frame in Bearbeitung + 1 wartend; älterer wartender Frame wird per `reset(0x09)` verworfen. Video-Streams `set_priority(-1)`, Control-Stream `set_priority(1)`.

## 5. Verbindungsablauf

Server-Sicht pro Client:

```
Incoming ─accept─▶ Handshaking ─ALPN icu/1, TLS ok─▶ Authenticating ─Fingerprint ok─▶ AwaitHello ─Hello ≤5 s─▶ Active ─▶ Closed
                                │ ALPN icu-enroll/1                │ unbekannt/revoked            │ Timeout
                                ▼                                  ▼                              ▼
                             Enrolling ─▶ Closed                close(AUTH_*)                close(HELLO_TIMEOUT)
```

Client-Sicht: `Connecting → Handshaking → SendHello → AwaitWelcome → Active → Closed`. Reconnect bei jedem nicht-fatalen Close und bei Netzfehlern: Backoff 1, 2, 4, 8, 15 s, dann 15 s, unbegrenzt; lokaler Zustand (Mute, Deafen, Lautstärken) bleibt. Fatal (kein Reconnect, UI-Hinweis) sind genau die in §6 mit „ja“ markierten Codes: PROTOCOL_VIOLATION, VERSION_UNSUPPORTED, AUTH_*, KICKED, ENROLL_*. Alle anderen → Reconnect mit Backoff.

## 6. Fehlercodes

Identisch in CONNECTION_CLOSE (Varint), `Error.code` und `UserLeft.reason` (u8).

| Code | Name | Fatal | Bedeutung |
| --- | --- | --- | --- |
| 0x00 | `NO_ERROR` | – | normales Close |
| 0x01 | `PROTOCOL_VIOLATION` | ja | Framing/Decode-Fehler, falsche erste Message, Limit überschritten; kein Reconnect (würde loopen) |
| 0x02 | `VERSION_UNSUPPORTED` | ja | `Hello.minor` nicht bedienbar |
| 0x03 | `AUTH_UNKNOWN_CERT` | ja | Cert gültig gegen CA, Fingerprint unbekannt |
| 0x04 | `AUTH_REVOKED` | ja | Fingerprint revoked |
| 0x05 | `SERVER_FULL` | nein | `max_users` erreicht; Client reconnectet mit Backoff |
| 0x06 | `KICKED` | ja | Admin-Aktion (= Revoke mit sofortigem Close) |
| 0x07 | `HELLO_TIMEOUT` | nein | kein `Hello` binnen 5 s; Client reconnectet mit Backoff |
| 0x08 | `SHUTDOWN` | nein | Server fährt herunter; Client reconnectet |
| 0x09 | `RATE_LIMITED` | nein | dreimal in 60 s → fatal |
| 0x0A | `AUTH_REQUIRED` | ja | ALPN `icu/1` ohne Client-Cert |
| 0x10 | `ENROLL_BAD_TOKEN` | ja | Token unbekannt/verbraucht/abgelaufen |
| 0x11 | `ENROLL_BAD_CSR` | ja | CSR nicht parsebar/Signatur ungültig |
| 0x12 | `ENROLL_RATE_LIMITED` | ja | 5/min pro IP oder 20/min global |
| 0x13 | `RENEW_REJECTED` | nein | Renewal-CSR abgelehnt |

## 7. Limits (Server erzwingt)

| Limit | Wert |
| --- | --- |
| Control-Frame | ≤ 16 KiB |
| Control-Messages | ≤ 50/s pro Client |
| Audio-Datagrams | ≤ 110/s pro Client, ≤ 410 B |
| Video-Frames | ≤ 20/s pro Client, ≤ 512 KiB; nur bei `camera_on` |
| Video-Bitrate | ≤ 400 kbit/s pro Sender (gleitend über 2 s), darüber Frames verwerfen |
| Users | `max_users` (Default 16, Obergrenze 64) |
| Enrollment | Stream ≤ 8 KiB, 5/min pro IP, 20/min global |
| Hello | ≤ 5 s nach Handshake |

## 8. Versionierung

- Major = ALPN-String (`icu/1`). Inkompatible Änderung → `icu/2`, Server bietet ggf. beide.
- Minor = `Hello.minor` / `Welcome.minor`; Server antwortet mit `min(client, server)`; Varianten oberhalb des ausgehandelten Minors werden nicht gesendet.
- `caps` für optionale Features innerhalb eines Minors.
- Header-`kind` trägt die Version (`0x01` Audio v1, `0x02` Video v1); ein Audio-v2-Header bekäme `0x03`.
- Unbekannte Enum-Variante beim Decode ist ein `Err` (Spike 2b) und führt zu `PROTOCOL_VIOLATION`; deshalb nie Varianten senden, die die Gegenseite nicht kennt.

## 9. Offene Messpunkte (in Implementierung nachziehen)

Aus `Research/verification.md` und den Research-Dokumenten: Datagram-vs-Stream-Priorisierung in quinn, In-place-`speaker`-Write ohne Kopie bei 16 Empfängern, Video-Relay-Latenz bei 1 % Verlust, Wirksamkeit von `LossHint` binnen 2 s, Keepalive über Consumer-NAT, echtes Windows.
