# ICU Server — Implementation Details

Stand 2026-09-20. Normative Grundlage: `../PROTOCOL.md`. Entscheidungen und Belege in `../Research/`.

## 1. Zweck und Grenzen

Ein Prozess, ein Raum, ein UDP-Port. Der Server terminiert QUIC/mTLS, verwaltet die Teilnehmerliste, relayt Audio-Datagrams und Video-Streams ohne Decode, stellt Client-Zertifikate aus und widerruft sie. Er hat keine Channels, keine Rollen, kein Web-UI, keine Datenbank außer einer TOML-Datei. Admin-Bedienung ist die CLI desselben Binarys.

## 2. Artefakt und Umgebung

| Punkt | Festlegung |
| --- | --- |
| Binary | `icu-server`, Rust, Target `x86_64-unknown-linux-musl`, vollstatisch (`Research/cross-compile.md`). Build über rustup-Target oder Container; das Fedora-`rust`-Paket hat keine musl-Std. |
| Laufzeitabhängigkeiten | keine (kein libc, kein CA-Bundle; der Server ist seine eigene CA). |
| Crypto | rustls 0.23 mit ring (Spike 1). |
| Installation | systemd-Unit (unten), `StateDirectory=icu` → `/var/lib/icu/`. Optional Docker `FROM scratch`. |
| Port | UDP 4711 Default, konfigurierbar. Bei Port < 1024 `AmbientCapabilities=CAP_NET_BIND_SERVICE`. |

## 3. Workspace-Layout

```
Cargo.toml                 # workspace: icu-proto, icu-server, icu-client
rust-toolchain.toml        # stable, targets windows-gnu (Client) und musl (Server)
.cargo/config.toml         # Rezept aus Research/cross-compile.md
build/win-static/libstdc++.a
crates/icu-proto/          # Wire-Typen (ControlMsg, EnrollMsg, Header-Encoder/-Decoder, Fehlercodes)
crates/icu-server/
  src/lib.rs               # `pub async fn run(Config) -> Handle` — der Server als Bibliothek,
                           #   damit die Client-Integrationstests ihn in-process starten können
  src/main.rs              # dünner CLI-Wrapper um lib.rs (clap)
  src/config.rs            # TOML laden/validieren
  src/ca.rs                # CA erzeugen/laden, Leaf signieren, CSR-Handling (rcgen 0.14)
  src/store.rs             # users.toml (user_id → Name + n Certs), invites.toml, flock auf state.lock
  src/endpoint.rs          # quinn ServerConfig, Bind v4+v6, Accept-Loop, ALPN-Dispatch
  src/enroll.rs            # ALPN icu-enroll/1
  src/session.rs           # Zustandsautomat pro Client, Control-Stream
  src/room.rs              # Teilnehmerliste, Broadcast, Session-ID-Vergabe
  src/relay.rs             # Audio-Datagram-Relay, Video-Stream-Relay
  src/limits.rs            # Token-Bucket je Client/IP
  src/admin.rs             # Unix-Socket /run/icu/admin.sock, JSON-Zeilenprotokoll
  src/cli/{invite,users,status}.rs   # sprechen über admin.rs mit dem laufenden Server
                           # die übrigen Kommandos sind cli::simple/with_closed in main.rs
```

`icu-proto` ist die einzige Stelle, an der Wire-Format definiert wird; Server und Client kompilieren dasselbe Crate.

## 4. Konfiguration `/etc/icu/server.toml`

```toml
[server]
listen = ["0.0.0.0:4711", "[::]:4711"]
server_name = "icu.example.org"      # SNI im Server-Cert und im Invite-Link
public_host = "icu.example.org:4711" # was im Invite-Link steht
max_users = 16                       # 1..=64; Server validiert die Obergrenze beim Start
min_client = "0.1.0"                 # SemVer, geht als Welcome.min_client an die Clients
admin_socket_group = "icu"           # Gruppe, der /run/icu/admin.sock gehört

[limits]                             # Defaults aus PROTOCOL.md §7, überschreibbar
audio_datagrams_per_s = 110
control_msgs_per_s = 50
video_kbit_s = 400
enroll_per_ip_per_min = 5
enroll_global_per_min = 20

[tls]
ca_validity_days = 3650
client_validity_days = 365
server_validity_days = 365

[log]
level = "info"                       # tracing-Filter
format = "text"                      # oder "json"
```

State unter `/var/lib/icu/`: `ca.key` (0600), `ca.crt`, `invite.key` (0600), `server.key` (0600), `server.crt`, `users.toml`, `invites.toml`, `state.lock`. Alle Schreibzugriffe atomar (Tempfile + rename) und unter `flock(state.lock)`.

Nutzermodell: ein Nutzer hat eine stabile `user_id` und über die Zeit **mehrere** Zertifikate (Renewal stellt ein neues aus, das alte bleibt bis zum Ablauf gültig). `revoke` setzt `revoked` auf allen Certs des Nutzers — dauerhaft, kein Un-Revoke. `is_admin` erlaubt `CreateInviteRequest` und die Nutzerverwaltung (`PROTOCOL.md` §2.2/§2.3.1); gesetzt wird es einmalig bei der ersten Anmeldung, die gegen einen leeren `users.toml`-Bestand erfolgreich abschließt (§6.3), danach über `promote`/`demote` (CLI, Admin-Socket) oder `AdminSetRole` (in-band, `PROTOCOL.md` §2.2 Variante 19) — `is_admin` fehlt in älteren `users.toml`-Dateien und deserialisiert dann als `false` (`#[serde(default)]`), keine Migration nötig. `disabled` ist eine zweite, unabhängige und **reversible** Login-Sperre (`disable`/`enable`, `AdminSetDisabled`): anders als `revoke` macht `enable` bestehende, nicht widerrufene Zertifikate sofort wieder nutzbar, keine erneute Anmeldung nötig; auch hier `#[serde(default)]`, keine Migration nötig. Beide Aktionen sind gegen das Entfernen des letzten funktionsfähigen Admins geschützt (§6.4).

`users.toml`:

```toml
schema = 1
next_user_id = 3

[user.1]
name = "alice"
created = "2026-09-20T12:00:00Z"
is_admin = true
disabled = false

[[user.1.cert]]
fingerprint = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"  # SHA-256(Leaf-DER), lowercase hex
issued  = "2026-09-20T12:00:00Z"
expires = "2027-09-20T12:00:00Z"
revoked = false
```

`invites.toml`:

```toml
schema = 1

[invite."k3nP8sQ2vL7xR1wY4tZ6bA"]   # Token, base64url ohne Padding, 22 Zeichen
name    = "bob"
created = "2026-09-20T12:00:00Z"
expires = "2026-09-21T12:00:00Z"
used_by = 0                          # 0 = unbenutzt, sonst die vergebene user_id
```

Zeitstempel durchgehend RFC 3339 in UTC; Fingerprints lowercase Hex ohne Trenner; `schema` erlaubt spätere Migrationen.

## 5. CLI

| Kommando | Wirkung |
| --- | --- |
| `icu-server run [--config PATH]` | Server starten; beim ersten Start CA und Server-Leaf erzeugen. |
| `icu-server invite --name alice [--ttl 24h]` | Token (128 Bit, base64url) erzeugen, in `invites.toml` mit Ablauf und Name speichern, Invite-Link ausgeben (`icu://host:port/enroll?t=…&ca=…&n=…`). |
| `icu-server users` | Tabelle: `user_id`, Name, Fingerprint (gekürzt), ausgestellt, läuft ab, revoked, online. |
| `icu-server revoke <name\|user_id\|fingerprint>` | `revoked = true` auf **allen** Certs des Nutzers; eine laufende Verbindung wird sofort mit `KICKED` geschlossen. |
| `icu-server rename <name\|user_id> <new>` | Anzeigename ändern. Kollidiert `<new>` case-insensitive mit einem bestehenden Namen, bricht die CLI mit Fehler ab und ändert nichts. Broadcast `UserRenamed`, wenn der Nutzer online ist. |
| `icu-server promote <name\|user_id\|fingerprint>` | `is_admin = true`. Pusht `UserAdminChanged { is_admin: true }` an jede eigene Sitzung des Nutzers, wenn online. |
| `icu-server demote <name\|user_id\|fingerprint>` | `is_admin = false`. Abgelehnt mit `LAST_ADMIN`, wenn der Nutzer der letzte funktionsfähige Admin ist. Sonst wie `promote`, mit `is_admin: false`. |
| `icu-server disable <name\|user_id\|fingerprint>` | `disabled = true`; eine laufende Verbindung wird sofort mit `AUTH_DISABLED` geschlossen. Abgelehnt mit `LAST_ADMIN`, wenn der Nutzer der letzte funktionsfähige Admin ist. |
| `icu-server enable <name\|user_id\|fingerprint>` | `disabled = false`; bestehende, nicht widerrufene Zertifikate sind sofort wieder gültig. |
| `icu-server status` | Online-Liste, Uptime, Relay-Zähler. |
| `icu-server rotate-server-cert` | Neues Server-Leaf von derselben CA; Clients brauchen nichts. |

Invite-Erstellung ist seit `CreateInviteRequest` (`PROTOCOL.md` §2.2/§2.3.1) auch über die authentifizierte `icu/1`-Verbindung möglich — ein eingeloggter Admin braucht dafür weder CLI noch Host-Zugriff, nur `Welcome.is_admin`. CLI und Admin-Socket bleiben unverändert für headless/skriptgesteuerte Deployments; beide Pfade laufen über dieselben `store.create_invite`/`peek_invite`/`consume_invite`, kein eigener Invite-Lebenszyklus.

**Wer schreibt den State.** Ein laufender Server ist der alleinige Schreiber von `users.toml` und `invites.toml`. Jedes CLI-Kommando außer `run` geht deshalb über den Admin-Socket; der Server führt die Änderung aus, persistiert sie und wendet sie sofort auf offene Verbindungen an. Nur wenn kein Server läuft (Socket nicht vorhanden oder `ECONNREFUSED`), schreibt die CLI selbst und hält dabei `flock(/var/lib/icu/state.lock)`. Damit wirkt ein frisch erzeugtes Invite-Token sofort statt erst nach dem nächsten Neustart, und CLI und Server können einander keine Änderung überschreiben.

Admin-Socket: `/run/icu/admin.sock` (`RuntimeDirectory=icu`, `RuntimeDirectoryMode=0750`, Gruppe aus `admin_socket_group`), JSON-Zeilenprotokoll, ein Request pro Zeile, eine Response pro Zeile, dann Close. Kein Netzwerk-Admin, keine Authentifizierung außer den Dateirechten des Sockets.

| Kommando | Request | Response |
| --- | --- | --- |
| invite | `{"cmd":"invite","name":"bob","ttl_s":86400}` | `{"ok":true,"token":"k3nP…","link":"icu://…","expires":"2026-09-21T12:00:00Z"}` |
| users | `{"cmd":"users"}` | `{"ok":true,"users":[{"user_id":1,"name":"alice","fingerprint":"9f86d0…","expires":"…","revoked":false,"online":true,"is_admin":true}]}` |
| revoke | `{"cmd":"revoke","user":"alice"}` | `{"ok":true,"user_id":1,"closed":1}` bzw. `{"ok":false,"error":"LAST_ADMIN"}` |
| rename | `{"cmd":"rename","user":"alice","new":"alice2"}` | `{"ok":true}` bzw. `{"ok":false,"error":"NAME_TAKEN"}` |
| promote | `{"cmd":"promote","user":"bob"}` | `{"ok":true}` |
| demote | `{"cmd":"demote","user":"bob"}` | `{"ok":true}` bzw. `{"ok":false,"error":"LAST_ADMIN"}` |
| disable | `{"cmd":"disable","user":"bob"}` | `{"ok":true,"user_id":2,"closed":1}` bzw. `{"ok":false,"error":"LAST_ADMIN"}` |
| enable | `{"cmd":"enable","user":"bob"}` | `{"ok":true}` |
| status | `{"cmd":"status"}` | `{"ok":true,"uptime_s":3600,"online":3,"relay":{"datagrams_in":…,"datagrams_out":…,"dropped":{…}}}` |

Fehlerform einheitlich `{"ok":false,"error":"<kurzform>","detail":"<text>"}`.

## 6. Ablauf

### 6.1 Start

1. Config laden, validieren: Listen-Adressen, `server_name` nicht leer, `1 ≤ max_users ≤ 64`, `min_client` als SemVer parsebar, und `max_users × 100 B + Reserve < 16 KiB` (sonst passt ein volles `Welcome` nicht in ein Control-Frame, `PROTOCOL.md` §7).
2. CA laden oder erzeugen (`rcgen::CertificateParams` mit `IsCa::Ca(Constrained(0))`, `KeyCertSign|CrlSign`, P-256; Spike 2b). Danach `invite.key` laden oder erzeugen (`invite_key.rs`, 32 Zufallsbytes, hex in einer Datei mit Modus 0600 neben `ca.key`, eigener Zweck also eigener Schlüssel statt CA-Key-Wiederverwendung) — versiegelt/öffnet ausschließlich das Bearer-Token in einem `InviteBlob` (`PROTOCOL.md` §2.3.1), rotiert in v1 nie.
3. Server-Leaf laden oder erzeugen (SAN = `server_name` und, falls IP, IP-SAN; EKU ServerAuth) über `CertificateParams::signed_by(&leaf_key_pair, &issuer)` — in rcgen 0.14 nimmt `signed_by` **Public Key und Issuer**, anders als die CSR-Variante in §6.3; Kette = Leaf + CA.
4. `flock(state.lock)` nehmen und für die gesamte Laufzeit halten; `users.toml` und `invites.toml` laden (`schema` prüfen); abgelaufene Invites verwerfen. Ab hier ist der Server alleiniger Schreiber des States (§5).
5. rustls `ServerConfig` in **dieser** Reihenfolge — `builder_with_provider(rustls::crypto::ring::default_provider().into())` liefert `WantsVersions`, der Versionsschritt muss also vor dem Verifier kommen, sonst übersetzt es nicht:
   `ServerConfig::builder_with_provider(ring_provider)` → `.with_protocol_versions(&[&rustls::version::TLS13])?` → `.with_client_cert_verifier(WebPkiClientVerifier::builder(Arc::new(roots)).allow_unauthenticated().build()?)` → `.with_single_cert(vec![leaf, ca], key)?`; danach `alpn_protocols = [b"icu/1", b"icu-enroll/1"]` und `max_early_data_size = 0`.
6. quinn `ServerConfig` aus `QuicServerConfig::try_from(rustls_config)`; Transport wie `PROTOCOL.md §1`. Signaturhinweise: `max_idle_timeout(Some(IdleTimeout::try_from(Duration::from_secs(60))?))`, `datagram_receive_buffer_size(Some(64 * 1024))` (Option) gegen `datagram_send_buffer_size(64 * 1024)` (kein Option), `max_concurrent_uni_streams(VarInt::from_u32(2 * max_users + 4))`. `migration(true)` liegt auf `quinn::ServerConfig`, nicht auf `TransportConfig`.
7. Ein `Endpoint` pro Listen-Adresse; Accept-Loop je Endpoint auf einem tokio-Multi-Thread-Runtime.
8. Admin-Socket öffnen (`/run/icu/admin.sock`, Gruppe aus `admin_socket_group`, Modus 0660). SIGTERM → allen `close(SHUTDOWN)`, 1 s warten, Socket und `state.lock` freigeben, Ende.

### 6.2 Accept

```
incoming = endpoint.accept()
  if limits.ip_bucket(remote).exhausted → incoming.refuse()
  if under_load (≥ 32 handshakes in flight) → incoming.retry()      // Adressvalidierung
connecting = incoming.accept()
// handshake_data() nimmt &mut self und liefert Box<dyn Any>; Downcast auf quinn::crypto::rustls::HandshakeData
alpn = connecting.handshake_data().await?
         .downcast::<quinn::crypto::rustls::HandshakeData>()?.protocol   // Spike 2b: vor Handshake-Ende lesbar
conn = connecting.await
match alpn:
  icu-enroll/1 → enroll::handle(conn)
  icu/1        → session::handle(conn)
```

### 6.3 Enrollment (`enroll.rs`)

1. `accept_bi()` mit 5-s-Timeout; Frame lesen (≤ 8 KiB).
2. `EnrollRequest` decodieren. `token` wird allein über seine Länge in drei Formen unterschieden (`PROTOCOL.md` §2.3), alles unter derselben State-Sperre, die auch den bisherigen Compare-and-Set hält:
   - **leer** (Bootstrap): nur zulässig, solange `store.is_empty()`; sonst `ENROLL_BOOTSTRAP_CLOSED`. `EnrollRequest.name` wird validiert und wird direkt zum Cert-CN — der einzige Fall, in dem das CN nicht aus einem Invite kommt, weil bei der allerersten Anmeldung noch keiner existieren kann.
   - **22 Zeichen** (Plain-Token): wie bisher `peek_invite` (konstantzeitiger Vergleich, `subtle`).
   - **länger** (Blob-Passthrough): Base64url dekodieren, `InviteBlob` per postcard parsen, AAD aus den Klartextfeldern neu berechnen, mit `invite_key.open()` öffnen (`ChaCha20-Poly1305`) → das gewonnene Plain-Token läuft anschließend durch denselben `peek_invite`-Pfad. Fehlschlagendes Öffnen (Manipulation an `sealed`/`nonce` oder an einem AAD-Feld) → `ENROLL_BAD_TOKEN`.

   In allen drei Fällen wird der Invite (falls vorhanden) erst **unter derselben Sperre** per Compare-and-Set verbraucht, nachdem `is_first = store.is_empty()` gelesen wurde — damit gewinnt bei zwei parallelen Enrollments (gleiches Token oder beide leer) genau eines, und `is_first` ist über alle drei Pfade hinweg race-sicher.
3. `CertificateSigningRequestParams::from_der(&csr_der)` (Feature `x509-parser`); Parse- oder Signaturfehler → `ENROLL_BAD_CSR`. Ebenso `ENROLL_BAD_CSR`, wenn der CSR-Key nicht P-256 oder P-384 ist. Server setzt Subject CN wie in Schritt 2 bestimmt, EKU ClientAuth, `not_before = now − 1 h`, `not_after = now + client_validity_days`, Serial = 16 Zufallsbytes; `signed_by(&issuer)` — die CSR-Variante nimmt **nur** den Issuer, der Public Key kommt aus der CSR.
4. `user_id` = `next_user_id++`. Anzeigename = `EnrollRequest.name`, sofern gültig und es kein Bootstrap ist, sonst das CN aus Schritt 2; kollidiert er case-insensitive mit einem bestehenden Namen, hängt der Server `-2`, `-3`, … an, bis er eindeutig ist (`PROTOCOL.md` §2.3). Fingerprint = SHA-256(Leaf-DER), lowercase Hex; Nutzer- und Cert-Eintrag in `users.toml` schreiben (`is_admin = is_first` aus Schritt 2 — **jede** erste erfolgreiche Anmeldung gegen einen leeren Bestand wird Admin, nicht nur der Bootstrap-Pfad), Token als verbraucht markieren — beides atomar und **vor** der Antwort persistieren. Schlägt das Schreiben fehl, wird das Cert nicht ausgeliefert: `Error(ENROLL_BAD_TOKEN, "state write failed")`, Token (falls vorhanden) bleibt unverbraucht.
5. `EnrollResponse { leaf_der, ca_der, server_name }` senden, `finish()`, `close(NO_ERROR)`.

Fehlerpfad einheitlich (`PROTOCOL.md` §2.3): erst `EnrollMsg::Error { code, detail }` senden, dann `finish()`, dann `close(code)` mit demselben Code. Ein präsentiertes Client-Cert auf diesem ALPN wird ignoriert.

### 6.4 Session (`session.rs`)

1. `peer_identity()` → `None` → `close(AUTH_REQUIRED)`. Sonst Leaf-DER → SHA-256 → `users.toml`: unbekannt → `AUTH_UNKNOWN_CERT`, revoked → `AUTH_REVOKED`, `disabled` → `AUTH_DISABLED`. (rustls hat Kette und Ablauf schon geprüft.)
2. `accept_bi()` und erstes Frame binnen 5 s, sonst `HELLO_TIMEOUT`. Muss `Hello` sein. `minor` aushandeln (`min(client, server)`, nicht bedienbar → `VERSION_UNSUPPORTED`). `Hello.name` wird **nur** übernommen, wenn zu dieser `user_id` noch kein Name gespeichert ist; sonst ignoriert (`PROTOCOL.md` §2.2).
3. Ist der Raum voll (`room.len() >= max_users`), `close(SERVER_FULL)`. Sonst `room.join()` → Session-ID (monoton, nie wiederverwendet), `Welcome { you: UserInfo{user_id, session, name, flags}, users, audio, min_client, minor, caps, is_admin }` senden (`is_admin` aus `store.is_admin(user_id)`, unter derselben Sperre wie `set_initial_name` gelesen), `UserJoined` an alle anderen.
4. Zwei Tasks pro Client: (a) Control-Reader (Frames → `ControlMsg` → Handler), (b) Datagram-Reader (`read_datagram` → `relay::audio`). Video: `accept_uni` in einem dritten Task → `relay::video`.
5. Ausgehend: ein `mpsc`-Kanal pro Client für Control-Frames (Broadcasts), ein Writer-Task schreibt auf den Control-Stream. Datagrams werden direkt aus dem Relay per `send_datagram` geschickt (nicht blockierend).
6. Ende: Verbindung geschlossen oder Fehler → `room.leave()` → `UserLeft { session, reason }` Broadcast; `reason` = `NO_ERROR` bei sauberem Close, `TIMEOUT` bei Idle-Timeout oder Transportabbruch, sonst der Code, mit dem der Server geschlossen hat. Session-ID bleibt gesperrt.

#### Control-Handler (vollständig, `PROTOCOL.md` §2.2)

| Message | Server tut |
| --- | --- |
| `SetState` | mute/deafen/camera_on übernehmen, bit2 (`speaking`) verwerfen (§6.5 leitet es ab), `UserState` an alle inkl. Verursacher; ≤ 20/s, darüber `Error(RATE_LIMITED)`. |
| `Ping` | unverzüglich `Pong { nonce, server_time_ms }`, `server_time_ms` = Unix-Millisekunden UTC. Zählt gegen das Control-Limit. |
| `SetName` | Länge 1–32 B, keine Steuerzeichen, serverweit eindeutig (case-insensitive). Kollision → `Error(NAME_TAKEN)`, keine Änderung. Sonst in `users.toml` persistieren und `UserRenamed { session, name }` an alle. |
| `LossReport` | Maximum je Sprecher fortschreiben, siehe §6.5. |
| `KeyframeRequest` | an den Sprecher weiterleiten, siehe §6.5. |
| `RenewRequest` | CSR wie §6.3.3 prüfen und signieren (Subject CN und `user_id` aus dem bestehenden Eintrag, nicht aus der CSR), neuen Cert-Eintrag zu **demselben** Nutzer in `users.toml` anhängen; das alte Cert bleibt bis zu seinem `expires` gültig. `RenewResponse { leaf_der }`. Rate ≤ 1/Tag pro `user_id`, sonst `Error(RENEW_REJECTED)`; ungültige CSR ebenfalls `Error(RENEW_REJECTED)`. |
| `CreateInviteRequest` | Nur wenn `store.is_admin(user_id)`, sonst `Error(NOT_ADMIN)` und nichts weiter. Sonst `store.create_invite` (unverändert) aufrufen, das Ergebnis als `InviteBlob` mit `invite_key.seal()` versiegeln (`PROTOCOL.md` §2.3.1) und als `CreateInviteResponse { blob, expires }` zurücksenden. |
| `AdminSetRole` | Nur wenn `store.is_admin(user_id)` (live geprüft), sonst `AdminActionError(NOT_ADMIN)`. Sonst `store.set_admin(target, is_admin)`; bei `LAST_ADMIN` `AdminActionError(LAST_ADMIN)`. Bei Erfolg `AdminSetRoleOk` an den Absender und `UserAdminChanged { is_admin }` unicast an jede eigene Sitzung des Ziels (live, ohne Reconnect). |
| `AdminSetDisabled` | Wie `AdminSetRole`, aber `store.set_disabled(target, disabled)`. Wird dabei deaktiviert, kickt der Server jede laufende Verbindung des Ziels sofort mit `AUTH_DISABLED` (`session::kick_user`) — kein separater Push, das Schließen ist die Benachrichtigung. `AdminSetDisabledOk` an den Absender. |
| `AdminRevokeUser` | Wie `AdminSetRole`, aber `store.revoke(target)` und `session::kick_user(target, KICKED)`. `AdminRevokeUserOk { user_id, closed }` an den Absender. |
| `UserListRequest` | Nur wenn `store.is_admin(user_id)`, sonst `AdminActionError(NOT_ADMIN)`. Sonst jeden registrierten Nutzer (online oder nicht) als `UserSummary` sammeln (`online` über `room.sessions_of_user`) und als `UserListResponse { users }` zurücksenden. |
| unbekannte Variante / Decode-Fehler | `close(PROTOCOL_VIOLATION)`. |

**Letzter-Admin-Schutz** (`store::guard_last_admin`, gilt für `set_admin(_, false)`, `set_disabled(_, true)` und `revoke`, über alle drei Pfade — in-band, Admin-Socket, CLI-Offline-Fallback — identisch, da eine einzige Store-Methode): ein Nutzer zählt als *funktionsfähiger Admin*, wenn `is_admin && !disabled && mindestens ein Cert nicht revoked`. Die Aktion wird nur abgelehnt, wenn ihr Ziel aktuell funktionsfähiger Admin ist **und** danach kein funktionsfähiger Admin mehr übrig wäre — ein bereits deaktivierter oder vollständig revoked Admin zählt nicht mehr mit und blockiert daher nichts.

### 6.5 Relay (`relay.rs`)

Audio: siehe `PROTOCOL.md §3`. Implementierung: `Bytes` aus `read_datagram`, `BytesMut::from` nur für die 2 Speaker-Bytes, dann `freeze()`; ein `Bytes`-Clone pro Empfänger (Refcount). Das ist der einzige heiße Pfad des Servers (16 Clients × 50 Pakete/s), deshalb holt er das Room-Lock genau zweimal — einmal für Absenderflags **und** Empfängerliste zusammen (`room.audio_fanout`), einmal für die Talk-Spurt-Buchführung — und schreibt die Empfängerliste in einen Puffer, den der `datagram_task` besitzt und über alle Datagrams wiederverwendet; pro Datagram wird also nichts alloziert. Zähler pro Empfänger für `TooLarge | UnsupportedByPeer | Disabled | ConnectionLost` — das sind die Varianten von `quinn::SendDatagramError`; ein `Blocked` gibt es nur auf `quinn_proto::SendDatagramError` und nie auf der hier benutzten High-Level-API. Zusätzlich setzt der Server `speaking` implizit: erstes Datagram nach ≥ 500 ms Stille → `UserState{speaking=1}` Broadcast, `END`-Flag oder 500 ms ohne Datagram → `speaking=0`. Damit brauchen Clients für den Indicator keine eigene Meldung; ein von Clients in `SetState` gesetztes bit2 wird ignoriert (`PROTOCOL.md` §2.1: bit2 ist serverseitig autoritativ).

Video: pro Sender ein Task-Slot. Frame-Header lesen, prüfen, `speaker` setzen. Pro Empfänger `open_uni()`, Header schreiben, `read_chunk(max_len, true)` → `write_chunk` durchreichen, `finish()` (in quinn 0.11 **synchron**, `Result<(), ClosedStream>`; für „zugestellt“ `stopped()` awaiten). Die Drop-Regel gilt **pro Sender-Empfänger-Paar**: hat ein Empfänger noch einen Frame desselben Senders offen, wird der ältere per `reset(RATE_LIMITED)` verworfen — ein langsamer Empfänger bremst die übrigen damit nicht aus. Bitrate-Limit gleitend über 2 s. Lebensdauer der Slots: Session-IDs werden nie wiederverwendet, ein Slot darf einen Sender also nicht überleben. Verlässt ein Empfänger den Raum, fällt sein Slot beim nächsten Frame weg; verlässt der **Sender** den Raum, schließt `Drop for VideoSender` jeden seiner Slots. Beides verwirft den noch wartenden Frame und weckt den Forwarder-Task, der sonst bis zum Verbindungsende des Empfängers auf einem Slot parken würde, den niemand mehr füllt.

Ist die Empfängermenge leer (niemand sonst `Active`, oder alle `deafened`), wird der eingehende Stream trotzdem geleert — `stop(0x00)` auf dem `RecvStream` oder bis FIN lesen und verwerfen —, sonst läuft die Stream-Flow-Control des Senders voll. Zähler `video_dropped_no_receiver`.

**`VideoCongestionHint`** (`PROTOCOL.md` §2.2 Variante 16, 2026-09-21): pro Sender (im selben `VideoSender`, keine neue Struktur) eine rollierende Offer/Verdrängungs-Zählung je Empfänger, alle 2 s zu einer Verdrängungsrate `Verdrängungen · 1000 / Angebote` je Empfänger verrechnet; das **Maximum über alle Empfänger** dieses Senders wird bei ≥ 20 ‰ Änderung als `VideoCongestionHint { drop_permille }` an den Sender geschickt — dieselbe Maximum-Regel wie bei `LossHint`, nur für Video statt Audio. Vereinfachung: das Fenster schließt nur bei einem eingehenden Frame; hört ein Sender ganz auf zu senden (Kamera aus), bleibt der letzte Hinweis stehen statt auf 0 zu verfallen — harmlos, weil ohne Video nichts mehr anzupassen ist. Der Client speist den Wert in seine Qualitäts-Ladder (`Client/IMPLEMENTATION.md` §8); der Server selbst transkodiert nichts, die Anpassung passiert ausschließlich beim sendenden Client.

`LossReport` → Maximum pro Sprecher über alle Empfänger (jeder Report verfällt nach 6 s) → `LossHint` an den Sprecher, wenn sich der Wert ändert. **Der Verfall ist eine Änderung:** fallen alle Reports eines Sprechers weg, sendet der Server `LossHint { loss_permille: 0 }`, sonst bliebe der FEC-Overhead beim Sender dauerhaft stehen. `KeyframeRequest` → an den Sprecher mit `speaker = 0`, ≤ 1/s.

### 6.6 Limits (`limits.rs`)

Token-Buckets pro Client (Control 50/s, Audio 110/s, Video 20/s und 400 kbit/s) und pro IP (Enrollment 5/min, Handshakes 10/s). Überschreitung: Datagram/Frame verwerfen und zählen; bei Control `Error(RATE_LIMITED)` (nicht fatal, Verbindung bleibt), beim dritten Verstoß binnen 60 s `close(RATE_LIMITED_FATAL)` — ein eigener, fataler Code, damit der Client nicht in einen Reconnect-Loop läuft (`PROTOCOL.md` §5/§6).

## 7. Logging und Metriken

`tracing` mit `tracing-subscriber` (text/json), Felder: `session`, `name`, `remote` (nur bei Connect/Disconnect, nie bei jedem Datagram). Zähler (atomics, über `status` abrufbar): Verbindungen, Relay-Datagrams in/out, verworfene Datagrams pro Grund, Video-Frames, Enrollment-Versuche. Kein Prometheus in v1; `status --json` reicht für einen Checkmk-Local-Check.

## 8. systemd-Unit

```ini
[Unit]
Description=ICU voice server
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/icu-server run --config /etc/icu/server.toml
DynamicUser=yes
StateDirectory=icu
RuntimeDirectory=icu
ConfigurationDirectory=icu
ProtectSystem=strict
ProtectHome=yes
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
SystemCallFilter=@system-service
MemoryDenyWriteExecute=yes
LockPersonality=yes
CapabilityBoundingSet=
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
```

CLI-Aufrufe als root oder als Mitglied der Gruppe, der der Admin-Socket gehört (`RuntimeDirectoryMode=0750`, Socket-Gruppe per Config).

## 9. Abhängigkeiten (gepinnt, Lizenz aus `Research/packaging-licensing.md`)

| Crate | Version | Lizenz | Zweck |
| --- | --- | --- | --- |
| quinn | 0.11.12 (`default-features = false`, features `runtime-tokio`, `rustls-ring`, `log`) | MIT/Apache-2.0 | QUIC. Die Default-Features enthalten `platform-verifier` und ziehen rustls-platform-verifier mit — für den statischen musl-Build abschalten. |
| rustls | 0.23.45 (`ring`, `std`, ohne Default-Provider) | Apache-2.0/ISC/MIT | TLS |
| ring | 0.17.14 | Apache-2.0 AND ISC | Crypto |
| rcgen | 0.14.10 (Feature `x509-parser`) | MIT/Apache-2.0 | CA, CSR. Default-Backend ist `ring` (`default = ["crypto","pem","ring"]`), passt also ohne Zutun zur ring-Linie; `CertificateSigningRequestParams::from_der` und `Issuer::from_ca_cert_der` existieren nur mit `x509-parser`. |
| x509-parser | 0.18 | MIT/Apache-2.0 | Fingerprint/CN lesen. **Nicht 0.16**: rcgen 0.14.10 hängt an `^0.18`, ein 0.16-Pin erzeugt zwei Kopien im Baum mit inkompatiblen Typen an der rcgen-Grenze. |
| tokio | 1.53.1 | MIT | Runtime |
| postcard | 1.1.3 (`use-std`) | MIT/Apache-2.0 | Control-Serialisierung. `default = ["heapless-cas"]`, `use-std` also explizit setzen. Eine unbekannte Enum-Variante liefert `Error::SerdeDeCustom` (nicht `DeserializeBadEnum`, das in 1.1.3 toter Code ist) — darauf matcht der `PROTOCOL_VIOLATION`-Pfad. |
| serde | 1 | MIT/Apache-2.0 | |
| bytes | 1 | MIT | Datagram-Puffer |
| clap | 4 | MIT/Apache-2.0 | CLI |
| toml | 0.8 | MIT/Apache-2.0 | Config/State |
| tracing, tracing-subscriber | 0.1 / 0.3 | MIT | Logging |
| subtle | 2 | BSD-3 | Token-Vergleich |
| base64 | 0.22 | MIT/Apache-2.0 | Token/CA-Hash |
| sha2 | 0.10 | MIT/Apache-2.0 | Fingerprint (oder ring::digest) |
| rand | 0.9 | MIT/Apache-2.0 | Invite-Token (128 Bit), Cert-Serial (16 B) |
| time | 0.3 | MIT/Apache-2.0 | `OffsetDateTime` für rcgen-Validity, RFC-3339 im State |
| rustix | 1 | Apache-2.0/MIT | `flock` auf `state.lock`, Unix-Socket-Rechte |

Kein libopus, kein Codec, keine GUI auf dem Server.

Lizenz-Allowlist: **eine** `deny.toml` im Workspace-Root, gültig für Server und Client — MIT, Apache-2.0, ISC, BSD-2-Clause, BSD-3-Clause, Zlib, Unicode-3.0; `IJG` ausschließlich hinter dem Client-Feature `mjpeg`. Alles andere bricht den Build.

Lizenz von ICU selbst: **TBD** (Empfehlung Apache-2.0, siehe `Research/packaging-licensing.md`). Bis zur Entscheidung trägt der Workspace kein `license`-Feld.

## 10. Tests

| Ebene | Inhalt |
| --- | --- |
| Unit (`icu-proto`) | Roundtrip aller `ControlMsg`/`EnrollMsg`; Header-Encode/Decode; unbekannte Variante → `Err`; Framing-Grenzen (0, 16384, 16385). |
| Unit (`ca.rs`, `store.rs`) | CA-Erzeugung, CSR-Override (Spike 2b), Fingerprint-Stabilität, atomares Schreiben, Revoke-Persistenz über **alle** Certs eines Nutzers, `user_id`-Vergabe und -Stabilität über Renewal, Namenskollision mit `-2`/`-3`-Suffix, `schema`-Prüfung beim Laden, `flock`-Verhalten bei parallelem CLI-Zugriff. |
| Integration (in-process, `127.0.0.1:0`) | Enrollment → Connect → Hello/Welcome; Connect ohne Cert → `AUTH_REQUIRED`; unbekanntes Cert → `AUTH_UNKNOWN_CERT`; Revoke während Verbindung → `KICKED`; Relay: 3 Clients, Audio von A kommt bei B und C mit `speaker = A` an, nicht bei A; `deafened` C bekommt nichts; `muted` A wird nicht relayt; Rate-Limits; Video-Relay mit Frame-Verwerfen; `LossReport` → `LossHint`; `KeyframeRequest`-Weiterleitung; Hello-Timeout; `max_users` erreicht → `SERVER_FULL`; `Ping` → `Pong` mit plausibler `server_time_ms`; `SetName` auf freien Namen → `UserRenamed`, auf belegten → `Error(NAME_TAKEN)`; `Hello.name` bei bereits gespeichertem Namen wird ignoriert; `RenewRequest` → `RenewResponse`, altes und neues Cert verbinden beide; dritter `RATE_LIMITED`-Verstoß → `close(RATE_LIMITED_FATAL)`; Video-Frame ohne Empfänger wird geleert statt zu stauen; Verfall aller `LossReport` → `LossHint{0}`; Invite über den Admin-Socket wirkt ohne Serverneustart. |
| Last | 16 Clients × 50 Datagrams/s für 60 s: keine Verluste auf Loopback, CPU < 10 % eines Kerns, keine Latenzzunahme (Datagram-Roundtrip p99 < 1 ms). |
| Netz | `tc netem` 30 ms ± 10 ms, 2 % Verlust: Relay verliert nichts zusätzlich; Keepalive hält die Verbindung über 10 min Stille. |

## 11. Milestones

1. **M1 Skeleton**: Workspace, `icu-proto` mit allen Typen + Tests, Server startet, erzeugt CA, akzeptiert QUIC, `Hello/Welcome`.
2. **M2 Enrollment und Admin**: `invite`, `icu-enroll/1`, `users.toml` mit `user_id`-Nutzermodell, `revoke`, `rename`, Renewal (`RenewRequest`/`RenewResponse`), **Admin-Socket** und die CLI darüber, Integrationstests Auth. Der Admin-Socket gehört hierher und nicht nach M5: M2 verlangt `revoke`, und der Integrationstest „Revoke während Verbindung → `KICKED`" (§10) ist ohne ihn nicht ausführbar.
3. **M3 Audio-Relay**: Datagram-Relay mit `speaker`-Override, Mute/Deafen-Semantik, Speaking-Ableitung, Rate-Limits, Lasttest.
4. **M4 Video-Relay** und `LossReport/LossHint`, `KeyframeRequest`.
5. **M5 Betrieb**: systemd-Unit, `status`-Zähler, musl-Build, `scratch`-Image, Checkmk-Local-Check.

Jeder Milestone endet mit grünen Tests und einem Eintrag in `../Research/verification.md`, wenn er einen offenen Messpunkt schließt.
