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
| Streams | Client eröffnet genau einen bidirektionalen Control-Stream. Video: ein unidirektionaler Stream pro Frame, in beide Richtungen. `max_concurrent_uni_streams` = `2 × max_users + 4` pro Richtung, vom Server aus seiner Config gesetzt und vom Client übernommen (bei `max_users` = 16 also 36). Das Protokoll deckelt die Zahl gleichzeitig aktiver Kameras nicht. |
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
UserFlags: u8      bit0 muted, bit1 deafened, bit2 speaking, bit3 camera_on, bit4 admin, bit5–7 reserviert
                   bit2 ist serverseitig autoritativ: der Server leitet es aus dem Audio-Datagram-Fluss ab (erstes Datagram nach ≥ 500 ms Stille → 1; END-Flag oder 500 ms ohne Datagram → 0) und broadcastet UserState; ein von Clients in SetState gesendetes bit2 wird ignoriert.
                   bit4 ist ebenfalls serverseitig autoritativ und spiegelt Store::is_admin: reitet kostenlos auf Welcome/UserJoined/UserState statt einer eigenen Variante; ein von Clients in SetState gesendetes bit4 wird ignoriert. AdminSetRole broadcastet zusätzlich zum bisherigen Unicast UserAdminChanged ein UserState für jede betroffene Sitzung, damit das Abzeichen bei jedem Betrachter live kippt.
Session:   u16     serverseitig vergeben, ab 1, pro Server-Laufzeit monoton, nie wiederverwendet; 0 = „unbekannt/ich“.
                   Gilt nur innerhalb einer Verbindung: nach einem Server-Neustart beginnt die Vergabe wieder bei 1.
UserId:    u32     beim Enrollment vergeben, ab 1, monoton, nie wiederverwendet; stabil über Cert-Renewals, Reconnects
                   und Server-Neustarts hinweg, in `users.toml` persistiert. Schlüssel für alles, was ein Client
                   dauerhaft je Peer speichert (Lautstärken, Notizen). 0 = „unbekannt“.
Name:      String  UTF-8, 1–32 Bytes, keine Steuerzeichen; serverweit eindeutig, Vergleich case-insensitive
UserInfo { user_id: UserId, session: Session, name: Name, flags: UserFlags }
AudioParams { sample_rate: u32 = 48000, frame_ms: u8 = 20, max_bitrate: u32 = 64000 }
```

**Reservierte Felder und Bits** — eine Regel für alle Stellen (`UserFlags` bit5–7, Audio-`flags` bit1–7, Video-`flags` bit1–7, Video-Byte 11): der Sender schreibt 0, der Empfänger ignoriert den Inhalt und verwirft deswegen nie eine Nachricht. „= 0“ in den Tabellen ist eine Sendevorschrift, keine Empfangsprüfung; sonst wäre kein additives Minor möglich.

### 2.2 Messages `ControlMsg`

| Nr | Variante | Richtung | Felder | Semantik |
| --- | --- | --- | --- | --- |
| 0 | `Hello` | C→S | `minor: u16`, `caps: u32`, `client: String` (≤ 64 B, Format `<name>/<semver> <os>`, z. B. `icu/0.1.0 linux`), `initial: UserFlags`, `name: Name` | Erste Message nach Handshake, binnen 5 s. `name` wird **nur** übernommen, wenn der Server zu dieser `user_id` noch keinen Namen gespeichert hat; danach ignoriert er das Feld. Namensänderung läuft ausschließlich über `SetName` (Client) und die CLI `rename` (Admin). |
| 1 | `Welcome` | S→C | `minor: u16`, `caps: u32`, `you: UserInfo`, `audio: AudioParams`, `users: Vec<UserInfo>` (ohne sich selbst), `min_client: String`, `is_admin: bool` | Vollständiger State. `you` ist der eigene Eintrag inklusive `user_id` und dem serverseitig gespeicherten Namen — der Client übernimmt diesen Namen, auch wenn er von seinem `Hello.name` abweicht. `min_client` = niedrigste erlaubte Client-Version als SemVer-String; der Client vergleicht sie in SemVer-Ordnung gegen die eigene Version. `is_admin` erlaubt `CreateInviteRequest` (§2.3.1). |
| 2 | `UserJoined` | S→C | `user: UserInfo` | |
| 3 | `UserLeft` | S→C | `session: Session`, `reason: u8` (Fehlercode < 256) | |
| 4 | `UserState` | S→C | `session: Session`, `flags: UserFlags` | Broadcast nach jeder Änderung, inkl. an den Verursacher. |
| 5 | `SetState` | C→S | `flags: UserFlags` | Server begrenzt auf 20/s pro Client, darüber `RATE_LIMITED`. |
| 6 | `Ping` | C→S | `nonce: u64` | Anwendungs-RTT; Intervall 5 s. |
| 7 | `Pong` | S→C | `nonce: u64`, `server_time_ms: u64` | `server_time_ms` = Unix-Zeit in Millisekunden (UTC). Server antwortet unverzüglich; der Client misst die RTT über `nonce`. |
| 8 | `Error` | S→C | `code: u32`, `detail: String` | Nicht-fatal. Fatale Fehler gehen über CONNECTION_CLOSE. |
| 9 | `RenewRequest` | C→S | `csr_der: Vec<u8>` (≤ 2 KiB) | Cert-Renewal über die authentifizierte Verbindung, ab 30 Tage vor Ablauf. |
| 10 | `RenewResponse` | S→C | `leaf_der: Vec<u8>` | Alter Fingerprint bleibt bis Ablauf gültig. |
| 11 | `LossReport` | C→S | `speaker: Session`, `loss_permille: u16` | Vom Empfänger gemessener Verlust des Audio-Streams eines Sprechers über die letzten 2 s. Intervall 2 s, nur wenn sich der Wert um ≥ 10 ‰ ändert. |
| 12 | `LossHint` | S→C | `loss_permille: u16` | Server leitet das Maximum aller `LossReport` für diesen Sprecher weiter; der Client setzt `OPUS_SET_PACKET_LOSS_PERC` (auf ganze Prozent gerundet, gedeckelt auf 20 — bewusste Festlegung, nicht aus dem Research belegt: oberhalb 20 % kostet LBRR mehr Bitrate als es rettet). Verfallen alle Reports eines Sprechers (6 s ohne neuen Report), sendet der Server `LossHint { loss_permille: 0 }`; der Client setzt dann auch die Jitter-Buffer-Untergrenze zurück. |
| 13 | `KeyframeRequest` | C→S | `speaker: Session` | Empfänger braucht ein H.264-IDR (Join, Verlust). Server leitet als `KeyframeRequest { speaker: 0 }` an den Sprecher weiter; der erzeugt binnen 100 ms einen IDR. Rate ≤ 1/s pro Sprecher. |
| 14 | `SetName` | C→S | `name: Name` | Namensänderung. Server prüft Länge, Steuerzeichen und serverweite Eindeutigkeit (case-insensitive); bei Kollision `Error { code: NAME_TAKEN }` und keine Änderung, sonst persistieren und `UserRenamed` an alle broadcasten. |
| 15 | `UserRenamed` | S→C | `session: Session`, `name: Name` | |
| 16 | `VideoCongestionHint` | S→C | `drop_permille: u16` | Server leitet das Maximum der pro Empfänger gemessenen Slot-Verdrängungsrate (`Server/IMPLEMENTATION.md` §6.5) für diesen Sprecher weiter, gemessen über die letzten 2 s (`icu_proto::limits::VIDEO_CONGESTION_WINDOW_S`), nur wenn sich der Wert um ≥ 20 ‰ ändert (`VIDEO_CONGESTION_HINT_STEP_PERMILLE`). Mirror von `LossHint`s Maximum-Regel, aber für Video statt Audio. Der Client speist den Wert in die Qualitäts-Ladder (`Client/IMPLEMENTATION.md` §8): Video trägt die gesamte Anpassung, Audio-Bitrate bleibt unangetastet. |
| 17 | `CreateInviteRequest` | C→S | `name: Name`, `ttl_s: u64` (`0` = Server-Default `limits::INVITE_TTL_S`) | Nur Admins (`Welcome.is_admin`); sonst `Error { code: NOT_ADMIN }`, nicht-fatal, Verbindung bleibt. Siehe §2.3.1. |
| 18 | `CreateInviteResponse` | S→C | `blob: String` (`ICU1-…`), `expires: String` (RFC 3339 UTC) | Antwort auf `CreateInviteRequest`. |
| 19 | `AdminSetRole` | C→S | `user_id: UserId`, `is_admin: bool` | Nur Admins, sonst `AdminActionError { code: NOT_ADMIN }`; bei letztem funktionsfähigen Admin `AdminActionError { code: LAST_ADMIN }`. Nicht-fatal in beiden Fällen. |
| 20 | `AdminSetRoleOk` | S→C | `user_id: UserId`, `is_admin: bool` | Bestätigung an den ausführenden Admin. |
| 21 | `UserAdminChanged` | S→C | `is_admin: bool` | **Unicast** an alle eigenen Sitzungen des betroffenen Nutzers (kein Broadcast, kein `session`-Feld nötig): eigener `is_admin`-Stand aktualisiert sich live, ohne Reconnect. |
| 22 | `AdminSetDisabled` | C→S | `user_id: UserId`, `disabled: bool` | Reversible Login-Sperre, unabhängig von Zertifikatswiderruf. Gleiche Ablehnung wie `AdminSetRole`. Wird ein Online-Nutzer deaktiviert, kickt der Server ihn sofort mit `AUTH_DISABLED` — das Schließen der Verbindung ist die einzige Benachrichtigung, kein separater Push. |
| 23 | `AdminSetDisabledOk` | S→C | `user_id: UserId`, `disabled: bool` | Bestätigung an den ausführenden Admin. |
| 24 | `AdminRevokeUser` | C→S | `user_id: UserId` | Widerruft alle Zertifikate von `user_id` und kickt jede laufende Verbindung mit `KICKED`. Gleiche Ablehnung wie `AdminSetRole`. |
| 25 | `AdminRevokeUserOk` | S→C | `user_id: UserId`, `closed: u32` | Bestätigung an den ausführenden Admin, mit Anzahl geschlossener Verbindungen. |
| 26 | `UserListRequest` | C→S | – | Nur Admins, sonst `AdminActionError { code: NOT_ADMIN }`. |
| 27 | `UserListResponse` | S→C | `users: Vec<UserSummary>` | Jeder registrierte Nutzer, online oder nicht. `UserSummary { user_id, name, created, is_admin, disabled, revoked, online }`. Größe durch `MAX_CONTROL_FRAME` begrenzt (§7): ~230 Nutzer passen in einen Frame, keine Paginierung gebaut. |
| 28 | `AdminActionError` | S→C | `user_id: UserId`, `code: u32`, `detail: String` | Ablehnung von Variante 19/22/24/26. `user_id = 0` bei einem `UserListRequest`-Fehler, da kein einzelnes Ziel benannt ist. Eigene Variante statt der generischen `Error`, damit `NOT_ADMIN` nicht zwischen Einladung und Nutzerverwaltung mehrdeutig wird. |
| 29 | `UserRtt` | S→C | `rtts: Vec<(Session, u16)>` | Broadcast alle ~2 s, solange der Raum ≥ 2 Teilnehmer hat: ein Eintrag je Online-Teilnehmer mit dessen serverseitig gemessener QUIC-Pfad-RTT in Millisekunden (`Connection::stats().path.rtt`, gesättigt auf `u16::MAX`). Das ist die Sicht des Servers, kein vom Client selbst gemeldeter `Ping`/`Pong`-Wert — jeder Betrachter sieht für einen gegebenen Peer dieselbe Zahl. Bei einem Raum mit nur einem Teilnehmer entfällt der Broadcast. |

Varianten 19–28 sind die ersten C→S-Nachrichten, bei denen der sendende Client eine **andere** `user_id` benennt als seine eigene.

Capability-Bits (`caps`), v1 alle 0; reserviert für spätere Erweiterungen. v1-Baseline ist `minor = 1`; ein Minor 0 existiert nicht, und alle 30 Varianten sind in v1 Pflicht (Varianten 19–28 wie zuvor `VideoCongestionHint`, und ebenso Variante 29 `UserRtt`: neue Pflicht-Varianten ohne Minor-Bump, da Client und Server in diesem Projekt im Lockstep ausgeliefert werden). Regel für spätere Minors: eine Seite sendet nie eine Variante oberhalb des ausgehandelten Minors (= `min(Hello.minor, Server-Minor)`, siehe §8).

### 2.3 Enrollment `EnrollMsg` (nur ALPN `icu-enroll/1`)

Gleiche Framing-Regel, Stream-Limit 8 KiB, ein Request/Response, danach Close.

| Nr | Variante | Richtung | Felder |
| --- | --- | --- | --- |
| 0 | `EnrollRequest` | C→S | `token: String`, `csr_der: Vec<u8>` (≤ 2 KiB), `name: Name` (gewünschter Anzeigename; der Server nimmt ihn als initialen Anzeigenamen — kollidiert er case-insensitive mit einem bestehenden Namen, hängt der Server `-2`, `-3`, … an, bis er eindeutig ist. Das Cert-CN kommt immer aus dem Invite, nie aus der CSR, außer beim Bootstrap-Fall unten) |
| 1 | `EnrollResponse` | S→C | `leaf_der: Vec<u8>`, `ca_der: Vec<u8>`, `server_name: String` (SNI für die Hauptverbindung) |
| 2 | `Error` | S→C | `code: u32`, `detail: String` |

`EnrollRequest.token` hat drei Formen, unterschieden allein durch die Länge:

| Form | Länge | Bedeutung |
| --- | --- | --- |
| leer | 0 | **Bootstrap**: nur solange `users.toml` noch keinen Eintrag hat. CN kommt aus `EnrollRequest.name` (validiert), kein Invite wird konsumiert. |
| Plain-Token | 22 Zeichen (base64url, 128 Bit) | Klassischer Invite, wie bisher über `store.peek_invite`/`consume_invite` geprüft. |
| Blob-Passthrough | länger | Base64url-Body eines `ICU1-…`-Blobs (§2.3.1) ohne das Präfix, vom Client unverändert durchgereicht. Der Server entschlüsselt, gewinnt daraus das Plain-Token und validiert es identisch zum Plain-Token-Pfad. |

**Wer wird Admin:** die *erste* Anmeldung, die gegen einen leeren `users.toml`-Bestand erfolgreich abschließt — gleich über welche der drei Formen —, erhält `is_admin = true` in `UserRecord`. Danach lehnt ein leeres Token mit `ENROLL_BOOTSTRAP_CLOSED` ab; alle weiteren Anmeldungen brauchen ein gültiges Token oder einen Blob.

Fehlerpfad: der Server sendet **immer zuerst** `Error { code, detail }`, dann `finish()`, dann `close(code)` mit demselben Code — sonst sieht der Nutzer im Enrollment-Dialog nur eine Codenummer ohne Text.

Server-Pinning beim Enrollment: der Client verifiziert die Serverkette nicht gegen einen Root-Store, sondern per eigenem `ServerCertVerifier`. Bei einem `icu://`-Link ist die Regel: `ca` ist base64url **ohne Padding** über SHA-256 des DER des **letzten** Kettenglieds; der Verifier akzeptiert genau Kettenlänge 2 (Leaf + CA), vergleicht nur diesen Hash, ignoriert SNI und Gültigkeitszeitraum (das Leaf ist zu diesem Zeitpunkt noch nicht vertrauenswürdig), muss aber `verify_tls12_signature`/`verify_tls13_signature` an den CryptoProvider delegieren. Bei einem `ICU1-…`-Blob (§2.3.1) vergleicht der Verifier stattdessen das DER byteweise gegen `InviteBlob.ca_der`. Beim Bootstrap-Fall (leeres Token) gibt es noch keinen Prior — der Verifier akzeptiert dort jede Kette der Länge 2 unconditional (Trust-on-first-use, unvermeidbar, da noch kein Geheimnis existiert). Der Server sendet daher immer die Kette Leaf + CA. Server-Seite: CSR-Subject, SAN, EKU und Validity werden vom Server gesetzt, nicht aus der CSR übernommen (Spike 2b).

Invite-Link: `icu://<host>:<port>/enroll?t=<token>&ca=<sha256-base64url>&n=<name-urlencoded>`. Token einmalig, 24 h gültig. Weiterhin per CLI (`icu-server invite`) oder Admin-Socket erzeugbar, unverändert.

### 2.3.1 Invite-Blob `ICU1-…`

Selbstständige Alternative zum Link: ein Admin fordert ihn über `CreateInviteRequest` an (§2.2), ohne CLI/Host-Zugriff. Enthält alles, was eine unregistrierte Instanz braucht — kein separater `ca=`-Hash-Abruf nötig.

Aufbau vor Armor: postcard-serialisiertes `icu_proto::invite_blob::InviteBlob`:

```
InviteBlob {
    host_port: String,   // "host:port" bzw. "[ipv6]:port", wie public_host()
    server_name: String, // SNI, nur zur Anzeige
    ca_der: Vec<u8>,      // volles CA-Zertifikat, nicht nur ein Hash
    expires: String,      // RFC 3339 UTC, informativ — maßgeblich bleibt der Server
    nonce: [u8; 12],
    sealed: Vec<u8>,       // ChaCha20-Poly1305(token) mit angehängtem 16-B-Tag
}
```

Armor: `"ICU1-" + base64url(postcard(InviteBlob))`, ohne Padding, ≤ 4096 Byte gesamt.

`host_port`/`server_name`/`ca_der`/`expires` sind zwangsläufig Klartext — der Client muss sie lesen, bevor überhaupt eine gesicherte Verbindung existiert (identisches TOFU-Modell wie der `ca=`-Hash-Link, nur mit vollen CA-Bytes statt Hash). Nur das Token selbst braucht Vertraulichkeit: es ist mit einem serverseitigen, unabhängig vom CA-Key gehaltenen `invite.key` (32 Zufallsbytes, `ChaCha20-Poly1305`, analog `ca.key` mit Mode 0600 persistiert) versiegelt. Die AAD der Versiegelung ist `postcard(host_port, server_name, ca_der, expires)` — jede Manipulation eines dieser Klartextfelder macht das Öffnen serverseitig ungültig (`ENROLL_BAD_TOKEN`), auch wenn der Client die Manipulation vorher nicht erkennen kann.

Der Client reicht beim Enrollment nicht den `ICU1-…`-String, sondern den nackten Base64url-Body (ohne Präfix) unverändert als `EnrollRequest.token` durch; der Server dekodiert, berechnet die AAD aus den Klartextfeldern neu und öffnet die Versiegelung mit `invite.key`. Gelingt das, läuft die Validierung identisch zum Plain-Token-Pfad über `store.peek_invite`/`consume_invite` weiter — kein separater Invite-Lebenszyklus.

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

Regeln: Server verwirft Datagrams < 11 B oder > 410 B; Empfänger unterscheidet Pause (`ts`-Sprung bei lückenloser `seq`) von Verlust (`seq`-Lücke). **Wrap:** `seq` und `ts` laufen über (bei 20 ms nach ~22 min bzw. ~24,8 h). Alle Vergleiche sind vorzeichenbehaftete Differenzen modulo 2^16 bzw. 2^32 (`(a.wrapping_sub(b)) as i16` bzw. `as i32`); eine Differenz jenseits der halben Wertebereichsbreite ist ein Rücksprung, kein Vorwärtssprung. Jitter-Buffer, Verlusterkennung und Re-Anchor rechnen ausschließlich so. Opus-Profil: 48 kHz mono, VOIP, Frame 20 ms Default (10 ms Client-Option ohne Protokolländerung, TOC trägt die Dauer), 48 kbit/s constrained VBR, FEC an, DTX aus, Complexity 10.

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
| 10 | 1 | `codec` | `1` = H.264 Constrained Baseline (Annex-B-NALs, SPS/PPS vor jedem IDR), `2` = MJPEG (Baseline JFIF). v1-Clients müssen 1 decodieren; **`2` darf in v1 nicht gesendet werden** (reiner Empfangs-/Debug-Pfad hinter dem Client-Feature `mjpeg`, ohne Aushandlung). |
| 11 | 1 | reserviert | 0 |

Profil: 320×240 @ 15 fps, Ziel 150 kbit/s, IDR alle 2 s und auf `KeyframeRequest`. Server-Relay: Header lesen, `camera_on` und `Active` prüfen, `speaker` setzen, Chunks durchreichen, `finish()`. Pro **Sender-Empfänger-Paar** max. 1 Frame in Bearbeitung + 1 wartend; der ältere wartende Frame wird per `reset(0x09)` verworfen. Die Drop-Entscheidung fällt je Empfänger, damit ein langsamer Empfänger die übrigen nicht ausbremst. Video-Streams `set_priority(-1)`, Control-Stream `set_priority(1)`.

## 5. Verbindungsablauf

Server-Sicht pro Client:

```
Incoming ─accept─▶ Handshaking ─ALPN icu/1, TLS ok─▶ Authenticating ─Fingerprint ok─▶ AwaitHello ─Hello ≤5 s─▶ Active ─▶ Closed
                                │ ALPN icu-enroll/1                │ unbekannt/revoked            │ Timeout
                                ▼                                  ▼                              ▼
                             Enrolling ─▶ Closed                close(AUTH_*)                close(HELLO_TIMEOUT)
```

Client-Sicht: `Connecting → Handshaking → SendHello → AwaitWelcome → Active → Closed`. Reconnect bei jedem nicht-fatalen Close und bei Netzfehlern: Backoff 1, 2, 4, 8, 15 s, dann 15 s, unbegrenzt; lokaler Zustand (Mute, Deafen, Lautstärken) bleibt. Fatal (kein Reconnect, UI-Hinweis) sind genau die in §6 mit „ja“ markierten Codes: PROTOCOL_VIOLATION, VERSION_UNSUPPORTED, AUTH_*, KICKED, RATE_LIMITED_FATAL, ENROLL_*. Alle anderen → Reconnect mit Backoff.

**Zustand beim Reconnect:** jedes `Welcome` ersetzt den vollständigen Peer-Zustand. Session-IDs gelten nur innerhalb einer Verbindung; der Client verwirft bei jedem Reconnect Jitter-Buffer, Decoder, Video-Kacheln und alle Session-Zuordnungen. Dauerhaft je Peer gespeicherte Daten (Lautstärken) hängen an `user_id`, nie an `session`.

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
| 0x09 | `RATE_LIMITED` | nein | Rate überschritten; als `Error` gesendet, Verbindung bleibt |
| 0x0A | `AUTH_REQUIRED` | ja | ALPN `icu/1` ohne Client-Cert |
| 0x0B | `RATE_LIMITED_FATAL` | ja | dritter `RATE_LIMITED`-Verstoß binnen 60 s; Server schließt, Client reconnectet nicht |
| 0x0C | `TIMEOUT` | nein | Idle-Timeout oder Transportabbruch; auch `UserLeft.reason`, wenn eine Verbindung ohne sauberes Close endet |
| 0x0D | `NAME_TAKEN` | nein | `SetName` auf einen serverweit schon vergebenen Namen (case-insensitive); nur als `Error`, nie als Close |
| 0x10 | `ENROLL_BAD_TOKEN` | ja | Token unbekannt/verbraucht/abgelaufen |
| 0x11 | `ENROLL_BAD_CSR` | ja | CSR nicht parsebar/Signatur ungültig |
| 0x12 | `ENROLL_RATE_LIMITED` | ja | 5/min pro IP oder 20/min global |
| 0x13 | `RENEW_REJECTED` | nein | Renewal-CSR abgelehnt |
| 0x14 | `ENROLL_BOOTSTRAP_CLOSED` | ja | Leeres Token (Bootstrap), aber `users.toml` hat schon mindestens einen Eintrag |
| 0x15 | `NOT_ADMIN` | nein | `CreateInviteRequest` von einem Nicht-Admin; nur als `Error`, nie als Close |
| 0x16 | `LAST_ADMIN` | nein | Admin-Aktion würde den letzten funktionsfähigen Admin entfernen (Demote/Disable/Revoke); nur als `AdminActionError`, nie als Close |
| 0x17 | `AUTH_DISABLED` | ja | Nutzer ist deaktiviert (`UserRecord.disabled`); geprüft beim Handshake, auch der Close-Code beim sofortigen Kick eines Online-Nutzers durch `AdminSetDisabled` |
| 0x18 | `NO_SUCH_USER` | nein | Admin-Aktion nennt eine unbekannte `user_id`; nur als `AdminActionError`, nie als Close |

## 7. Limits (Server erzwingt)

| Limit | Wert |
| --- | --- |
| Control-Frame | ≤ 16 KiB |
| Control-Messages | ≤ 50/s pro Client |
| Audio-Datagrams | ≤ 110/s pro Client, ≤ 410 B |
| Video-Frames | ≤ 20/s pro Client, ≤ 512 KiB; nur bei `camera_on` |
| Video-Bitrate | ≤ 400 kbit/s pro Sender (gleitend über 2 s), darüber Frames verwerfen. Bewusste Festlegung als Missbrauchs-Deckel, nicht aus dem Research hergeleitet: Zielbitrate ist 150 kbit/s (§4), der Abstand fängt CBR-Spitzen bei 15 fps ab. |
| Users | `max_users` (Default 16, Obergrenze 64); beim Erreichen `close(SERVER_FULL)` |
| Uni-Streams | `max_concurrent_uni_streams` = `2 × max_users + 4` je Richtung |
| `Welcome`-Größe | Server validiert beim Start, dass `max_users` × 100 B + Reserve unter dem 16-KiB-Frame-Limit bleibt |
| Enrollment | Stream ≤ 8 KiB, 5/min pro IP, 20/min global |
| Hello | ≤ 5 s nach Handshake |

## 8. Versionierung

- Major = ALPN-String (`icu/1`). Inkompatible Änderung → `icu/2`, Server bietet ggf. beide.
- Minor = `Hello.minor` / `Welcome.minor`; Server antwortet mit `min(client, server)`; Varianten oberhalb des ausgehandelten Minors werden nie gesendet. v1-Baseline ist `minor = 1` (ein Minor 0 gibt es nicht); kann der Server ein gemeldetes Minor nicht bedienen, schließt er mit `VERSION_UNSUPPORTED`.
- `caps` für optionale Features innerhalb eines Minors.
- Header-`kind` trägt die Version (`0x01` Audio v1, `0x02` Video v1); ein Audio-v2-Header bekäme `0x03`.
- Unbekannte Enum-Variante beim Decode ist ein `Err` (Spike 2b) und führt zu `PROTOCOL_VIOLATION`; deshalb nie Varianten senden, die die Gegenseite nicht kennt.

## 9. Offene Messpunkte (in Implementierung nachziehen)

Aus `Research/verification.md` und den Research-Dokumenten: In-place-`speaker`-Write ohne Kopie bei 16 Empfängern, Video-Relay-Latenz bei 1 % Verlust, Wirksamkeit von `LossHint` binnen 2 s, Keepalive über Consumer-NAT, echtes Windows.

**Belegt (2026-09-21):** Datagram-vs-Stream-Priorisierung in quinn — `quinn-proto-0.11.18/src/connection/mod.rs::populate_packet` schreibt `self.datagrams` vor `self.streams` in jedes ausgehende Paket; Audio-Datagramme gewinnen die Paket-interne Framing-Priorität gegenüber Video-Streams immer. Ungeklärt bleibt die Congestion-Window-Ebene: ein gesättigtes `cwnd` drosselt trotzdem beide (`Client/IMPLEMENTATION.md` §8, Messpunkt).

**Neu (Netzwerk-adaptive Qualität, `Client/IMPLEMENTATION.md` §8):** `tc netem`-Messung, ob `queuing_delay_ms` (`rtt − min_rtt`) bei diesem App-Bitratenniveau (74 kbit/s Audio + bis 150 kbit/s Video) vor `congestion_events` messbar ausschlägt; ob ein aktiver Video-Stream Audio-Datagramme über das gemeinsame `cwnd` spürbar verzögert; Wirksamkeit von `VideoCongestionHint` binnen 2 s (analog zu `LossHint`).
