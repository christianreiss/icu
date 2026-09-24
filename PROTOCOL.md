# ICU Wire-Protokoll v1

Stand 2026-09-20. Abgeleitet aus `Research/transport.md`, `Research/protocol/wire.md`, `Research/auth-enrollment.md`, `Research/codec.md`, `Research/camera-video.md` und den Spike-Ergebnissen in `Research/verification.md`. Diese Datei ist die normative Fassung; die Rust-Typen im Crate `icu-proto` müssen ihr entsprechen.

## 1. Transport

| Punkt | Festlegung |
| --- | --- |
| Transport | QUIC (RFC 9000/9001) mit Unreliable Datagrams (RFC 9221). Implementierung quinn 0.11 + rustls 0.23, Crypto-Provider ring. |
| Port | Ein UDP-Port, Default 4711. Server bindet IPv4 und IPv6 getrennt (quinn-Dual-Stack-Hinweis). |
| TLS | TLS 1.3, Server-Cert und Client-Cert von der ICU-CA des Servers. Client vertraut nur dieser CA. |
| ALPN | `icu/1`. ALPN ist das Major-Versions-Gate. Das frühere `icu-enroll/1` gibt es nicht mehr (revidiert 2026-09-24, §2.3). |
| Client-Auth | Server: `WebPkiClientVerifier` mit `allow_unauthenticated()`; die Pflicht erzwingt die Anwendung und schließt ohne Cert mit `AUTH_REQUIRED` statt eines nackten TLS-Alerts (Spike 2b: `peer_identity()` ohne Cert = `None`). |
| Widerruf (CRL) | Der Server signiert mit seiner CA eine CRL über alle widerrufenen, noch nicht abgelaufenen Zertifikate: Client-Leaves, verbrauchte Registrierungszertifikate und durch `rotate-server-cert` ersetzte Server-Leaves. Der Client-Verifier des Servers prüft das Client-Leaf dagegen (`only_check_end_entity_revocation`), ein widerrufenes scheitert also schon im Handshake mit dem TLS-Alert `certificate_revoked` (QUIC-Code `0x100 + 44`); der Client zeigt das fatal an. Die CRL wird bei jedem Start und nach jedem Widerruf neu gebaut und ohne Neustart in die TLS-Konfiguration aller Endpoints übernommen (`CrlUpdate`, §2.2). Die CRL-Nummer steigt streng monoton (`users.toml`) und liegt nie unter der aktuellen Unix-Zeit in Sekunden, damit sie auch nach dem Zurückspielen eines Backups über der der Clients bleibt; `nextUpdate` = +30 Tage wird nirgends erzwungen. Der Fingerprint-Abgleich gegen `users.toml` bleibt die eigentliche Zulassung; Zertifikatseinträge von vor der CRL haben keine Seriennummer und sind nur darüber abgedeckt. Der Client prüft das Server-Leaf gegen die neueste gespeicherte CRL (`CrlUpdate`). Ein abgelaufenes oder widerrufenes Server-Leaf ist für den Client **nicht fatal**: beheben kann das nur der Server (automatische Rotation vor Ablauf, `rotate-server-cert`), also versucht er es mit Backoff weiter (§5), und die Prüfung lehnt das alte Leaf weiterhin ab. (2026-09-24) |
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
UserFlags: u8      bit0 muted, bit1 deafened, bit2 speaking, bit3 camera_on, bit4 admin, bit5 viewing, bit6–7 reserviert
                   bit5 (`viewing`, 2026-09-24) setzt der Client per Hello.initial/SetState, solange er Video anzeigt (Overlay an oder Hauptfenster sichtbar). Der Server relayt Video nur an Teilnehmer mit bit5 (§4); ein Sender pausiert seine Kamera (kein Encode, kein Upload), solange kein anderer Teilnehmer bit5 trägt, und beginnt beim Fortsetzen mit einem IDR.
                   bit2 ist serverseitig autoritativ: der Server leitet es aus dem Audio-Datagram-Fluss ab (erstes Datagram nach ≥ 500 ms Stille → 1; END-Flag oder 500 ms ohne Datagram → 0) und broadcastet UserState; ein von Clients in SetState gesendetes bit2 wird ignoriert.
                   bit4 ist ebenfalls serverseitig autoritativ und spiegelt Store::is_admin: reitet kostenlos auf Welcome/UserJoined/UserState statt einer eigenen Variante; ein von Clients in SetState gesendetes bit4 wird ignoriert. AdminSetRole broadcastet zusätzlich zum bisherigen Unicast UserAdminChanged ein UserState für jede betroffene Sitzung, damit das Abzeichen bei jedem Betrachter live kippt.
Session:   u16     serverseitig vergeben, ab 1, monoton; nach 65 535 läuft der Zähler auf 1 um und überspringt belegte IDs, bei leerem Raum beginnt er wieder bei 1. Eine ID kehrt also frühestens nach 65 534 anderen Vergaben zurück; 0 = „unbekannt/ich“.
                   Gilt nur innerhalb einer Verbindung: nach einem Server-Neustart beginnt die Vergabe wieder bei 1.
UserId:    u32     beim Ausstellen der Registrierung vergeben, ab 1, monoton, nie wiederverwendet; stabil über Cert-Renewals, Reconnects
                   und Server-Neustarts hinweg, in `users.toml` persistiert. Schlüssel für alles, was ein Client
                   dauerhaft je Peer speichert (Lautstärken, Notizen). 0 = „unbekannt“.
Name:      String  UTF-8, 1–32 Bytes, in NFC; keine Steuer-, Format- (Bidi, Zero-Width), Private-Use-, nicht
                   zugewiesenen oder Zeilen-/Absatztrennzeichen; als Leerraum nur U+0020, nicht am Anfang, nicht am Ende,
                   nicht doppelt; kein kombinierendes Zeichen am Anfang; eine einzige Schrift (UTS #39 „single script“,
                   Ziffern und Satzzeichen gehen mit jeder). Clients bringen Eingaben vorher mit `normalize_name` in Form
                   (trimmen, Leerraum zusammenfassen, NFC). Serverweit eindeutig über `name_key` = UTS-#39-Skeleton von
                   NFKC + Kleinschreibung: „Chris“, „chris“ und ein kyrillisches „Сһris“ gelten als derselbe Name
                   (2026-09-24). Schon gespeicherte Namen werden nicht nachgeprüft, nur neue: ein Renewal oder Nachzügler,
                   der den gespeicherten Namen unverändert weiterträgt, braucht nur 1–32 Bytes ohne Steuerzeichen
                   (`validate_stored_name`) und keine erneute Eindeutigkeitsprüfung.
                   Der Name steht im Subject-CN des Client-Zertifikats. Er wird bei der Registrierung durch die CSR gesetzt
                   und danach ausschließlich dadurch geändert, dass ein neues Zertifikat ausgestellt wird (§2.2 Nr. 9).
                   `users.toml` spiegelt den CN des zuletzt ausgestellten Zertifikats; dieser Spiegel füllt alle
                   Roster-Felder. Identität bleibt der Fingerprint, nie der Name.
                   Eindeutig ist ein Name zum Ausstellungszeitpunkt, nicht über die Zeit: alte Zertifikate bleiben bis
                   zum Ablauf gültig, also kann nach einer Umbenennung ein altes Zertifikat denselben CN tragen wie ein
                   neuer Nutzer. Der CN ist deshalb kein Schlüssel.
UserInfo { user_id: UserId, session: Session, name: Name, flags: UserFlags }
AudioParams { sample_rate: u32 = 48000, frame_ms: u8 = 20, max_bitrate: u32 = 64000 }
```

**Reservierte Felder und Bits** — eine Regel für alle Stellen (`UserFlags` bit6–7, Audio-`flags` bit1–7, Video-`flags` bit1–7, Video-Byte 11): der Sender schreibt 0, der Empfänger ignoriert den Inhalt und verwirft deswegen nie eine Nachricht. „= 0“ in den Tabellen ist eine Sendevorschrift, keine Empfangsprüfung; sonst wäre kein additives Minor möglich.

### 2.2 Messages `ControlMsg`

| Nr | Variante | Richtung | Felder | Semantik |
| --- | --- | --- | --- | --- |
| 0 | `Hello` | C→S | `minor: u16`, `caps: u32`, `client: String` (≤ 64 B, Format `<name>/<semver> <os>`, z. B. `icu/0.1.0 linux`), `initial: UserFlags`, `name: Name` | Erste Message nach Handshake, binnen 5 s. `name` ist obsolet: der Server ignoriert es, weil der Name im Zertifikat steht (§2.1). Das Feld bleibt aus Wire-Kompatibilität erhalten. Namensänderung läuft ausschließlich über `RenewRequest` (Nr. 9). |
| 1 | `Welcome` | S→C | `minor: u16`, `caps: u32`, `you: UserInfo`, `audio: AudioParams`, `users: Vec<UserInfo>` (ohne sich selbst), `min_client: String`, `is_admin: bool`, `registration: bool` | Vollständiger State. `you` ist der eigene Eintrag inklusive `user_id` und dem serverseitig gespeicherten Namen — der Client übernimmt diesen Namen, auch wenn er von seinem `Hello.name` abweicht. `min_client` = niedrigste erlaubte Client-Version als SemVer-String; der Client vergleicht sie in SemVer-Ordnung gegen die eigene Version. `is_admin` erlaubt `CreateInviteRequest` (§2.3). `registration = true` heißt: der Client hat sich mit einem Registrierungszertifikat angemeldet (§2.3); dann ist `users` leer, `you.session` = 0, die Sitzung ist nicht im Raum, und außer `Ping` nimmt der Server nur `RenewRequest` an. Weicht `you.name` vom CN des eigenen Zertifikats ab, fordert der Client ein neues Zertifikat mit dem aktuellen Namen an; der Server lässt das außerhalb des Tageslimits zu (Nr. 9). |
| 2 | `UserJoined` | S→C | `user: UserInfo` | |
| 3 | `UserLeft` | S→C | `session: Session`, `reason: u8` (Fehlercode < 256) | |
| 4 | `UserState` | S→C | `session: Session`, `flags: UserFlags` | Broadcast nach jeder Änderung, inkl. an den Verursacher. |
| 5 | `SetState` | C→S | `flags: UserFlags` | Server begrenzt auf 20/s pro Client, darüber `RATE_LIMITED`. |
| 6 | `Ping` | C→S | `nonce: u64` | Anwendungs-RTT; Intervall 5 s. |
| 7 | `Pong` | S→C | `nonce: u64`, `server_time_ms: u64` | `server_time_ms` = Unix-Zeit in Millisekunden (UTC). Server antwortet unverzüglich; der Client misst die RTT über `nonce`. |
| 8 | `Error` | S→C | `code: u32`, `detail: String` | Nicht-fatal. Fatale Fehler gehen über CONNECTION_CLOSE. |
| 9 | `RenewRequest` | C→S | `csr_der: Vec<u8>` (≤ 2 KiB) | Cert-Renewal über die authentifizierte Verbindung **und der einzige Weg, den Namen zu ändern**. Der Subject-CN der CSR ist der gewünschte Name; er muss genau einmal vorkommen, `Name` erfüllen und serverweit frei sein. Drei Fälle, unterschieden durch exakten Stringvergleich gegen den gespeicherten Namen: **normales Renewal** (CN = gespeichert, vorgelegtes Cert aktuell; höchstens eines pro Tag, ab 30 Tage vor Ablauf), **Nachzügler** (CN = gespeichert, vorgelegtes Cert trägt noch einen alten CN, weil auf einem anderen Gerät umbenannt wurde; vom Tageslimit ausgenommen, aber je Zertifikat auf einen pro Tag begrenzt) und **Umbenennung** (CN ≠ gespeichert; kostet das Tagesbudget). Kollision → `Error { code: NAME_TAKEN }`, nichts geändert, nicht fatal. Ratenlimit oder unbrauchbare CSR → `Error { code: RENEW_REJECTED }`. Bei einer Umbenennung broadcastet der Server `UserRenamed` für jede Sitzung dieses Nutzers. |
| 10 | `RenewResponse` | S→C | `leaf_der: Vec<u8>` | Das neue Leaf trägt den angeforderten Namen als CN. Alter Fingerprint bleibt bis Ablauf gültig. |
| 11 | `LossReport` | C→S | `speaker: Session`, `loss_permille: u16` | Vom Empfänger gemessener Verlust des Audio-Streams eines Sprechers über die letzten 2 s. Intervall 2 s, nur wenn sich der Wert um ≥ 10 ‰ ändert. |
| 12 | `LossHint` | S→C | `loss_permille: u16` | Server leitet das Maximum aller `LossReport` für diesen Sprecher weiter; der Client setzt `OPUS_SET_PACKET_LOSS_PERC` (auf ganze Prozent gerundet, gedeckelt auf 20 — bewusste Festlegung, nicht aus dem Research belegt: oberhalb 20 % kostet LBRR mehr Bitrate als es rettet). Verfallen alle Reports eines Sprechers (6 s ohne neuen Report), sendet der Server `LossHint { loss_permille: 0 }`; der Client setzt dann auch die Jitter-Buffer-Untergrenze zurück. |
| 13 | `KeyframeRequest` | C→S | `speaker: Session` | Empfänger braucht ein H.264-IDR (Join, Verlust). Server leitet als `KeyframeRequest { speaker: 0 }` an den Sprecher weiter; der erzeugt binnen 100 ms einen IDR. Rate ≤ 1/s pro Sprecher. |
| 14 | `SetName` | — | `name: Name` | **Obsolet.** Namensänderung läuft über `RenewRequest` (Nr. 9), weil der Name im Zertifikat steht. Die Variantennummer bleibt reserviert, damit die Nummerierung normativ bleibt; kein Client sendet sie, und ein Server, der sie empfängt, schließt mit `PROTOCOL_VIOLATION`. |
| 15 | `UserRenamed` | S→C | `session: Session`, `name: Name` | |
| 16 | `VideoCongestionHint` | S→C | `drop_permille: u16` | Server leitet das Maximum der pro Empfänger gemessenen Slot-Verdrängungsrate (`Server/IMPLEMENTATION.md` §6.5) für diesen Sprecher weiter, gemessen über die letzten 2 s (`icu_proto::limits::VIDEO_CONGESTION_WINDOW_S`), nur wenn sich der Wert um ≥ 20 ‰ ändert (`VIDEO_CONGESTION_HINT_STEP_PERMILLE`). Mirror von `LossHint`s Maximum-Regel, aber für Video statt Audio. Der Client speist den Wert in die Qualitäts-Ladder (`Client/IMPLEMENTATION.md` §8): Video trägt die gesamte Anpassung, Audio-Bitrate bleibt unangetastet. |
| 17 | `CreateInviteRequest` | C→S | `name: String` (Platzhaltername, leer = generiert), `ttl_s: u64` (`0` = Server-Default `limits::INVITE_TTL_S`, höchstens `limits::MAX_INVITE_TTL_S` = 7 Tage) | Stellt ein Registrierungszertifikat aus (§2.3). Nur Admins (`Welcome.is_admin`); sonst `Error { code: NOT_ADMIN }`. Ein vergebener oder ungültiger Platzhalter → `Error { code: NAME_TAKEN }`. Beides nicht-fatal, Verbindung bleibt. |
| 18 | `CreateInviteResponse` | S→C | `bundle: String` (Passport-Armor, §2.3.1), `expires: String` (RFC 3339 UTC) | Antwort auf `CreateInviteRequest`: das Registrierungs-Bundle. |
| 19 | `AdminSetRole` | C→S | `user_id: UserId`, `is_admin: bool` | Nur Admins, sonst `AdminActionError { code: NOT_ADMIN }`; bei letztem funktionsfähigen Admin `AdminActionError { code: LAST_ADMIN }`. Nicht-fatal in beiden Fällen. |
| 20 | `AdminSetRoleOk` | S→C | `user_id: UserId`, `is_admin: bool` | Bestätigung an den ausführenden Admin. |
| 21 | `UserAdminChanged` | S→C | `is_admin: bool` | **Unicast** an alle eigenen Sitzungen des betroffenen Nutzers (kein Broadcast, kein `session`-Feld nötig): eigener `is_admin`-Stand aktualisiert sich live, ohne Reconnect. |
| 22 | `AdminSetDisabled` | C→S | `user_id: UserId`, `disabled: bool` | Reversible Login-Sperre, unabhängig von Zertifikatswiderruf. Gleiche Ablehnung wie `AdminSetRole`. Wird ein Online-Nutzer deaktiviert, kickt der Server ihn sofort mit `AUTH_DISABLED` — das Schließen der Verbindung ist die einzige Benachrichtigung, kein separater Push. |
| 23 | `AdminSetDisabledOk` | S→C | `user_id: UserId`, `disabled: bool` | Bestätigung an den ausführenden Admin. |
| 24 | `AdminRevokeUser` | C→S | `user_id: UserId` | Widerruft alle Zertifikate von `user_id` und kickt jede laufende Verbindung mit `KICKED`. Gleiche Ablehnung wie `AdminSetRole`. |
| 25 | `AdminRevokeUserOk` | S→C | `user_id: UserId`, `closed: u32` | Bestätigung an den ausführenden Admin, mit Anzahl geschlossener Verbindungen. |
| 26 | `UserListRequest` | C→S | – | Nur Admins, sonst `AdminActionError { code: NOT_ADMIN }`. |
| 27 | `UserListResponse` | S→C | `users: Vec<UserSummary>` | Jeder registrierte Nutzer, online oder nicht. `UserSummary { user_id, name, created, is_admin, disabled, revoked, online, pending }` — `pending`: eingeladen, Registrierung noch nicht quittiert (§2.3). Größe durch `MAX_CONTROL_FRAME` begrenzt (§7): ~230 Nutzer passen in einen Frame, keine Paginierung gebaut. |
| 28 | `AdminActionError` | S→C | `user_id: UserId`, `code: u32`, `detail: String` | Ablehnung von Variante 19/22/24/26. `user_id = 0` bei einem `UserListRequest`-Fehler, da kein einzelnes Ziel benannt ist. Eigene Variante statt der generischen `Error`, damit `NOT_ADMIN` nicht zwischen Einladung und Nutzerverwaltung mehrdeutig wird. |
| 29 | `UserRtt` | S→C | `rtts: Vec<(Session, u16)>` | Broadcast alle ~2 s, solange der Raum ≥ 2 Teilnehmer hat: ein Eintrag je Online-Teilnehmer mit dessen serverseitig gemessener QUIC-Pfad-RTT in Millisekunden (`Connection::stats().path.rtt`, gesättigt auf `u16::MAX`). Das ist die Sicht des Servers, kein vom Client selbst gemeldeter `Ping`/`Pong`-Wert — jeder Betrachter sieht für einen gegebenen Peer dieselbe Zahl. Bei einem Raum mit nur einem Teilnehmer entfällt der Broadcast. |
| 30 | `CrlUpdate` | S→C | `crl_der: Vec<u8>` | Die aktuelle CRL des Servers (§1), DER, von der CA signiert. Direkt nach `Welcome` und an alle, sobald sie sich ändert (Widerruf, verbrauchte Registrierung, `rotate-server-cert`). Der Client speichert sie nur, wenn Aussteller und Signatur zur eigenen CA passen und ihre CRL-Nummer größer ist als die der gespeicherten, und prüft das Server-Leaf ab der nächsten Verbindung dagegen. Die Datei liegt neben der Identität (`crl.der`), nicht darin, damit die Nummer ein Renewal überlebt. Größe durch `MAX_CONTROL_FRAME` begrenzt: ~350 Einträge (2026-09-24). |

Varianten 19–28 sind die ersten C→S-Nachrichten, bei denen der sendende Client eine **andere** `user_id` benennt als seine eigene.

Capability-Bits (`caps`), v1 alle 0; reserviert für spätere Erweiterungen. v1-Baseline ist `minor = 1`; ein Minor 0 existiert nicht, und alle 30 Variantennummern sind in v1 belegt — Nr. 14 nur noch reserviert, siehe Tabelle — (Varianten 19–28 wie zuvor `VideoCongestionHint`, und ebenso Variante 29 `UserRtt`: neue Pflicht-Varianten ohne Minor-Bump, da Client und Server in diesem Projekt im Lockstep ausgeliefert werden). Regel für spätere Minors: eine Seite sendet nie eine Variante oberhalb des ausgehandelten Minors (= `min(Hello.minor, Server-Minor)`, siehe §8).

### 2.3 Registrierung (revidiert 2026-09-24, nur Zertifikate)

Es gibt kein Einladungs-Token, keinen Link, keinen Blob und kein eigenes Enrollment-Protokoll mehr. Eine Einladung **ist** ein Zertifikat:

1. Ein Admin (CLI `icu-server invite`, Admin-Socket oder `CreateInviteRequest`) erhält ein **Registrierungs-Bundle**: einen frisch erzeugten P-256-Schlüssel, ein kurzlebiges **Registrierungszertifikat** darauf und die CA — als Passport armiert (§2.3.1). Der Server legt dabei sofort einen Nutzer mit Platzhalternamen an (der mitgegebene Name, sonst `neu-` + 4 Hex-Ziffern; er muss frei sein wie jeder Name) und merkt sich das Zertifikat mit `registration = true`. Den Schlüssel behält er nicht.
2. Der Eingeladene verbindet sich damit ganz normal über `icu/1`. Er vertraut der CA aus dem Bundle; Adresse und SNI stehen in den Zertifikaten (unten). Nichts wird gepinnt, nichts ist TOFU.
3. Der Server antwortet auf `Hello` mit `Welcome { registration: true, users: [], … }`. Die Sitzung tritt dem Raum nicht bei.
4. Der Client erzeugt einen **eigenen** Schlüssel und sendet `RenewRequest` mit einer CSR, deren CN der gewählte Name ist.
5. Der Server prüft: Schlüssel ≠ Schlüssel des Registrierungszertifikats (sonst `RENEW_REJECTED`), Name frei (sonst `NAME_TAKEN`, das Bundle bleibt gültig, der Nutzer kann einen anderen Namen versuchen), CSR-Regeln wie bei jedem Renewal (sonst `RENEW_REJECTED`). Dann stellt er das echte Zertifikat aus, übernimmt den Namen und antwortet `RenewResponse`. Das Registrierungszertifikat bleibt noch gültig.
6. Der Client **speichert** Schlüssel und Zertifikat und schließt erst danach mit `REGISTRATION_STORED` (§6) — das ist die Quittung. `NO_ERROR` ist keine: QUIC-Implementierungen schließen eine bloß fallengelassene Verbindung implizit mit Code 0 (revidiert 2026-09-24). Mit ihr widerruft der Server das Registrierungszertifikat; ebenso beim ersten Login mit einem echten Zertifikat dieses Nutzers. Ab dann ist das Bundle wertlos: es steht auf der CRL und scheitert im Handshake (`certificate_revoked`, §1).

**Ohne Quittung bleibt das Bundle gültig** (revidiert 2026-09-24): Geht `RenewResponse` verloren, bricht die Verbindung ab oder scheitert das Speichern, schließt der Client mit einem anderen Code (oder gar nicht) und kann dasselbe Bundle erneut einlösen. Eine erneute Einlösung widerruft die Zertifikate früherer, unquittierter Einlösungen — die sollte niemand halten, sonst hätte ihr erster Login die Registrierung verbraucht. Das Bundle bleibt also zwischen Einlösung und Quittung (Millisekunden, schlimmstenfalls bis zum nächsten Client-Start) ein Inhaber-Nachweis: wer eine Kopie in diesem Fenster einlöst, übernimmt den Zugang. Es bleibt derselbe Nutzer (`user_id`). Vorher widerrief schon die Einlösung das Bundle; ein verlorenes Antwortpaket hinterließ dann einen Nutzer mit einem Zertifikat, das niemand besaß, und einen dauerhaft belegten Namen.

Eine Registrierungssitzung darf außer `Ping` und `RenewRequest` nichts senden (sonst `PROTOCOL_VIOLATION`), höchstens 10 `RenewRequest` stellen und höchstens 120 s offen bleiben. Registrierungssitzungen sind auf 5/min pro IP und 20/min global begrenzt (`ENROLL_RATE_LIMITED`); ein abgewiesener Versuch verdrängt eine laufende Sitzung desselben Bundles nicht. Ein Registrierungszertifikat ist höchstens 7 Tage gültig (Default 24 h); ein nie quittiertes wird nach Ablauf samt Nutzer entfernt (beim Start, bei jeder neuen Einladung und bei jeder Einlösung), sein Name ist dann wieder frei. Das gilt auch nach einer unquittierten Einlösung: wer das Zertifikat doch gespeichert, sich aber bis zum Ablauf des Bundles nie damit angemeldet hat, braucht eine neue Einladung.

**Wer wird Admin:** wer als Erster seine Registrierung quittiert, solange es noch keinen quittierten Admin gibt (revidiert 2026-09-24). Das erste Bundle eines leeren Bestands und jede Einlösung ohne quittierten Admin sind nur vorläufig als Admin markiert; mit der ersten Quittung verlieren alle übrigen noch eingeladenen Nutzer diese Markierung. So hat der Server immer genau einen Admin, auch wenn das erste Bundle nie eingelöst wird. Vorher entschied die Ausstellung: löste jemand ein späteres Bundle zuerst ein und verfiel das erste, blieb der Server ohne Admin. Den ersten Admin erzeugt man weiterhin mit `icu-server invite` auf dem Host, auch ohne laufenden Server.

**Registrierungszertifikat:** Subject `CN=<Platzhalter>, OU=ICU registration`, EKU ClientAuth, URI-SAN wie unten. Die OU ist nur ein Hinweis für die Client-Oberfläche (Namensfeld zeigen); der Server entscheidet allein anhand seines Eintrags.

**Wo der Server ist, steht in den Zertifikaten.** Jedes Client-Zertifikat, Registrierung wie Renewal, trägt als einzigen SAN die URI `icu://<public_host>` (`host:port`, IPv6 in Klammern). Die CA trägt den CN `ICU CA <server_name>`; daraus nimmt der Client das SNI. Bei einer CA von vor dieser Änderung (CN nur `ICU CA`) ist das SNI der Host aus der URI. Zieht der Server um, schreibt er die neue Adresse ins nächste Renewal; der Client übernimmt sie von dort.

**CSR-Regeln** (revidiert 2026-09-22, Name im Zertifikat): das Subject der CSR muss **genau einen** CN tragen, der als UTF-8 lesbar ist und `Name` erfüllt; dieser CN wird der Anzeigename. Er muss serverweit frei sein (case-insensitive). Kein stilles `-2`/`-3`-Anhängen. SAN, EKU, Basic Constraints, Key Usage, Validity und Serial setzt der Server; aus der CSR stammen der Public Key **und der geprüfte CN**. Der Server baut das Subject trotzdem von Grund auf neu: das kanonisiert die Kodierung auf UTF8String und wirft jedes andere DN-Attribut weg. Nur P-256- und P-384-Schlüssel.

### 2.3.1 Passport `-----BEGIN ICU PASSPORT-----`

Die ganze Identität eines Clients in einem ASCII-armierten Block, damit ein Nutzer sie auf ein zweites Gerät mitnehmen kann. Dasselbe Format transportiert das Registrierungs-Bundle (§2.3) — das ist der einzige Fall, in dem der Server einen Passport erzeugt; er liest nie einen. Ein wieder eingespielter Passport verbindet sofort mit dem ursprünglichen Server: CA und Adresse reisen mit.

```
PassportV2 {                 // Version 2, aktuell geschrieben
    key_der: Vec<u8>,    // privater Schlüssel, PKCS#8 DER
    leaf_der: Vec<u8>,   // Client-Zertifikat, DER — trägt die Serveradresse als URI-SAN
    ca_der: Vec<u8>,     // CA des Servers, DER — trägt das SNI im CN
}

PassportV1 {                 // Version 1, nur noch gelesen
    host: String, port: u16, server_name: String, name: String,
    key_der: Vec<u8>, leaf_der: Vec<u8>, ca_der: Vec<u8>,
}
```

Armor: `-----BEGIN ICU PASSPORT-----`, Base64 (Standard-Alphabet mit Padding) auf 64 Spalten umbrochen, `-----END ICU PASSPORT-----`. Body = `[Version-Byte] ++ postcard(PassportV<n>)`, ≤ 8192 Byte dekodiert. Das Versionsbyte steht **außerhalb** des postcard-Bodys, weil postcard nicht selbstbeschreibend ist: eine andere Version würde sonst als plausibler Müll durchgehen statt abgelehnt zu werden. Beim Dekodieren wird jeder Whitespace ignoriert, damit ein neu umbrochener oder eingerückter Block aus einem Chat-Client weiter lesbar ist.

Der Client exportiert Version 2, wenn sein Zertifikat die Adresse trägt, sonst Version 1 mit ausgeschriebener Adresse (ein Zertifikat von vor 2026-09-24, bis zum nächsten Renewal). Beim Import von Version 2 ohne Adresse im Zertifikat lehnt er ab.

**Der Body ist Klartext.** Ein Passport ist bis zum Ablauf des Zertifikats oder bis zum Widerruf ein vollwertiger Identitätsnachweis: wer den Text liest, kann sich als dieser Nutzer anmelden. Der Client warnt entsprechend, schreibt die Datei mit Mode 0600 und zeigt den Text nie am Bildschirm. Für ein Registrierungs-Bundle gilt dasselbe, bis es quittiert oder abgelaufen ist.

**Import nur nach Bestätigung** (2026-09-24). Ein Passport bringt CA und Adresse selbst mit; wer einen gefälschten einspielt, landet auf einem fremden Server mit fremder CA. Deshalb zeigt der Client vor jedem Import Server-Name (SNI), Adresse und den eigenen Namen aus dem Zertifikat (`ImportSummary`) und übernimmt erst nach Bestätigung: im Einstellungsfenster „Prüfen“ → „Übernehmen“, im Erststart-Fenster „Diesem Server beitreten“, auf der Kommandozeile eine Rückfrage `[j/N]` oder `--yes`. Vertraut die vorhandene Identität einer anderen CA, steht zusätzlich die Warnung „anderer Server als bisher“ dabei.

Beide Geräte halten danach dasselbe Zertifikat. Der Server lässt davon **eine** Verbindung gleichzeitig zu (§7): verbindet sich das zweite Gerät, schließt der Server das erste mit `SESSION_REPLACED`. Benennt sich eines der Geräte um, zieht das andere beim nächsten Start über den Nachzügler-Pfad aus §2.2 Nr. 9 nach.

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

Profil: 320×240 @ 15 fps, Ziel 150 kbit/s, IDR alle 2 s und auf `KeyframeRequest`. Server-Relay: Header lesen, `camera_on` und `Active` prüfen, `speaker` setzen, Chunks durchreichen, `finish()`. Empfänger sind nur Teilnehmer mit `viewing` (§2.1); ohne solche wird der Frame geleert und verworfen. Pro **Sender-Empfänger-Paar** max. 1 Frame in Bearbeitung + 1 wartend; der ältere wartende Frame wird per `reset(0x09)` verworfen. Die Drop-Entscheidung fällt je Empfänger, damit ein langsamer Empfänger die übrigen nicht ausbremst. Video-Streams `set_priority(-1)`, Control-Stream `set_priority(1)`.

## 5. Verbindungsablauf

Server-Sicht pro Client:

```
Incoming ─accept─▶ Handshaking ─ALPN icu/1, TLS ok─▶ Authenticating ─Fingerprint ok─▶ AwaitHello ─Hello ≤5 s─▶ Active ─▶ Closed
                                                                   │ unbekannt/revoked            │ Timeout
                                                                   │                              │ Registrierungszertifikat
                                                                   ▼                              ▼
                                                                close(AUTH_*)                  Registering ─RenewResponse, Close REGISTRATION_STORED─▶ Closed
```

Client-Sicht: `Connecting → Handshaking → SendHello → AwaitWelcome → Active → Closed`. Reconnect bei jedem nicht-fatalen Close und bei Netzfehlern: Backoff 1, 2, 4, 8, 15 s, dann 15 s, unbegrenzt; lokaler Zustand (Mute, Deafen, Lautstärken) bleibt. Fatal (kein Reconnect, UI-Hinweis) sind genau die in §6 mit „ja“ markierten Codes: PROTOCOL_VIOLATION, VERSION_UNSUPPORTED, AUTH_*, KICKED, RATE_LIMITED_FATAL, ENROLL_*. Alle anderen → Reconnect mit Backoff. Ein Server-Zertifikat, das die Prüfung nicht besteht, ist fatal, außer es ist abgelaufen oder widerrufen (§1).

**Zustand beim Reconnect:** jedes `Welcome` ersetzt den vollständigen Peer-Zustand. Session-IDs gelten nur innerhalb einer Verbindung; der Client verwirft bei jedem Reconnect Jitter-Buffer, Decoder, Video-Kacheln und alle Session-Zuordnungen. Dauerhaft je Peer gespeicherte Daten (Lautstärken) hängen an `user_id`, nie an `session`.

## 6. Fehlercodes

Identisch in CONNECTION_CLOSE (Varint), `Error.code` und `UserLeft.reason` (u8).

| Code | Name | Fatal | Bedeutung |
| --- | --- | --- | --- |
| 0x00 | `NO_ERROR` | – | normales Close |
| 0x01 | `PROTOCOL_VIOLATION` | ja | Framing/Decode-Fehler, falsche erste Message, Limit überschritten; kein Reconnect (würde loopen) |
| 0x02 | `VERSION_UNSUPPORTED` | ja | `Hello.minor` nicht bedienbar |
| 0x03 | `AUTH_UNKNOWN_CERT` | ja | Cert gültig gegen CA, Fingerprint unbekannt |
| 0x04 | `AUTH_REVOKED` | ja | Fingerprint revoked. Seit der CRL (§1) nur noch für Zertifikate ohne Seriennummer in `users.toml` und für ein Zertifikat, das zwischen Handshake und Anmeldung widerrufen wurde; sonst endet schon der Handshake mit `certificate_revoked` |
| 0x05 | `SERVER_FULL` | nein | `max_users` erreicht; Client reconnectet mit Backoff |
| 0x06 | `KICKED` | ja | Admin-Aktion (= Revoke mit sofortigem Close) |
| 0x07 | `HELLO_TIMEOUT` | nein | kein `Hello` binnen 5 s; Client reconnectet mit Backoff |
| 0x08 | `SHUTDOWN` | nein | Server fährt herunter; Client reconnectet |
| 0x09 | `RATE_LIMITED` | nein | Rate überschritten; als `Error` gesendet, Verbindung bleibt |
| 0x0A | `AUTH_REQUIRED` | ja | ALPN `icu/1` ohne Client-Cert |
| 0x0B | `RATE_LIMITED_FATAL` | ja | dritter `RATE_LIMITED`-Verstoß binnen 60 s; Server schließt, Client reconnectet nicht |
| 0x0C | `TIMEOUT` | nein | Idle-Timeout oder Transportabbruch; auch `UserLeft.reason`, wenn eine Verbindung ohne sauberes Close endet |
| 0x0D | `NAME_TAKEN` | nein | Angeforderter Name (CSR-CN bei Registrierung oder Renewal, Platzhalter bei `CreateInviteRequest`) ist serverweit schon vergeben (case-insensitive); nichts geändert. Als `Error` |
| 0x10 | `ENROLL_BAD_TOKEN` | ja | reserviert; nur das entfernte `icu-enroll/1` sendete ihn |
| 0x11 | `ENROLL_BAD_CSR` | ja | reserviert; CSR-Fehler melden Renewal und Registrierung als `RENEW_REJECTED` |
| 0x12 | `ENROLL_RATE_LIMITED` | ja | Registrierungssitzungen: 5/min pro IP oder 20/min global |
| 0x13 | `RENEW_REJECTED` | nein | Renewal-CSR abgelehnt (Ratenlimit, unbrauchbare CSR, unbekannter Nutzer) |
| 0x14 | `ENROLL_BOOTSTRAP_CLOSED` | ja | reserviert; nur das entfernte `icu-enroll/1` sendete ihn |
| 0x15 | `NOT_ADMIN` | nein | `CreateInviteRequest` von einem Nicht-Admin; nur als `Error`, nie als Close |
| 0x16 | `LAST_ADMIN` | nein | Admin-Aktion würde den letzten funktionsfähigen Admin entfernen (Demote/Disable/Revoke); nur als `AdminActionError`, nie als Close |
| 0x17 | `AUTH_DISABLED` | ja | Nutzer ist deaktiviert (`UserRecord.disabled`); geprüft beim Handshake, auch der Close-Code beim sofortigen Kick eines Online-Nutzers durch `AdminSetDisabled` |
| 0x18 | `NO_SUCH_USER` | nein | Admin-Aktion nennt eine unbekannte `user_id`; nur als `AdminActionError`, nie als Close |
| 0x19 | `SESSION_REPLACED` | ja | Dasselbe Client-Zertifikat hat sich erneut verbunden; die neuere Verbindung gewinnt, die ältere wird damit geschlossen (§7). Fatal, damit zwei Geräte mit demselben Passport sich nicht abwechselnd verdrängen |
| 0x1A | `REGISTRATION_STORED` | – | Nur als Close des Clients am Ende einer Registrierungssitzung: Identität gespeichert, der Server widerruft das Bundle (§2.3). Jeder andere Code, auch `NO_ERROR`, lässt es gültig |

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
| Handshakes | 10/s pro IP (IPv6: pro /64), 100/s global; ≤ 10 s je Handshake; ab 32 offenen Verbindungen Retry (Adressvalidierung) |
| Verbindungen | ≤ 256 offen (Handshake oder etabliert, über alle Listen-Adressen), darüber `refuse` |
| Verbindungen je Zertifikat | 1 (Fingerprint des Client-Leafs, Registrierungssitzungen eingeschlossen); die neueste gewinnt, die ältere endet mit `SESSION_REPLACED` (2026-09-24) |
| CRL | ≤ `MAX_CONTROL_FRAME` als `CrlUpdate`, ~350 Einträge; abgelaufene Zertifikate fallen heraus |
| Registrierung | 5/min pro IP (IPv6: pro /64), 20/min global; je Sitzung ≤ 10 `RenewRequest`, ≤ 120 s; Registrierungszertifikat ≤ 7 Tage |
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
