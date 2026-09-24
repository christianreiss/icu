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
crates/icu-proto/          # Wire-Typen (ControlMsg, Passport, Header-Encoder/-Decoder, Fehlercodes)
crates/icu-server/
  src/lib.rs               # `pub async fn run(Config) -> Handle` — der Server als Bibliothek,
                           #   damit die Client-Integrationstests ihn in-process starten können
  src/main.rs              # dünner CLI-Wrapper um lib.rs (clap)
  src/config.rs            # TOML laden/validieren
  src/ca.rs                # CA erzeugen/laden, Leaf signieren, CSR-Handling (rcgen 0.14)
  src/store.rs             # users.toml (user_id → Name + n Certs), flock auf state.lock
  src/endpoint.rs          # quinn ServerConfig, Bind v4+v6, Accept-Loop, ALPN-Dispatch
  src/registration.rs      # Registrierungs-Bundles ausstellen (Sitzungsseite in session.rs)
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
server_name = "icu.example.org"      # SNI im Server-Cert, steht auch im CN der CA
public_host = "icu.example.org:4711" # steht als URI-SAN icu://… in jedem Client-Cert
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

State unter `/var/lib/icu/`: `ca.key` (0600), `ca.crt`, `server.key` (0600), `server.crt`, `users.toml`, `state.lock`. `invites.toml` und `invite.key` von vor 2026-09-24 werden ignoriert und können weg. Alle Schreibzugriffe atomar (Tempfile + rename) und unter `flock(state.lock)`. Jede Änderung geht auf eine Kopie des Datenbestands, die Kopie wird geschrieben und erst danach übernommen — ein fehlgeschlagener Schreibvorgang lässt den laufenden Server also unverändert, statt einen Zustand zu bedienen, der nach dem nächsten Neustart weg ist.

Nutzermodell: ein Nutzer hat eine stabile `user_id` und über die Zeit **mehrere** Zertifikate (Renewal stellt ein neues aus, das alte bleibt bis zum Ablauf gültig). `UserRecord.name` ist seit 2026-09-22 nur noch ein **Spiegel** des Subject-CN des zuletzt ausgestellten Zertifikats (`PROTOCOL.md` §2.1); geschrieben wird er ausschließlich von `Store::add_user` (Registrierung ausstellen), `Store::redeem_registration` (Registrierung einlösen) und `Store::add_cert` (Renewal). `CertRecord.cn` hält den CN des jeweiligen Zertifikats, `CertRecord.renewed` den Zeitpunkt, zu dem dieses Zertifikat zuletzt für ein **Nachzügler**-Renewal benutzt wurde (§6.4). Beide sind `#[serde(default)]`, **`schema` bleibt 1, keine Migration**: Bestandsdatensätze laden mit `cn = ""`, was „unbekannt" heißt und nie „leerer Name" — die Zertifikats-DER werden nicht aufbewahrt, der CN lässt sich also nicht nachtragen. Die Angleichung passiert von selbst, pro Gerät, beim nächsten Connect (§6.4). `revoke` setzt `revoked` auf allen Certs des Nutzers — dauerhaft, kein Un-Revoke. `is_admin` erlaubt `CreateInviteRequest` und die Nutzerverwaltung (`PROTOCOL.md` §2.2/§2.3); gesetzt wird es einmalig für die erste Registrierung, die gegen einen leeren `users.toml`-Bestand ausgestellt wird (§6.3), danach über `promote`/`demote` (CLI, Admin-Socket) oder `AdminSetRole` (in-band, `PROTOCOL.md` §2.2 Variante 19) — `is_admin` fehlt in älteren `users.toml`-Dateien und deserialisiert dann als `false` (`#[serde(default)]`), keine Migration nötig. `disabled` ist eine zweite, unabhängige und **reversible** Login-Sperre (`disable`/`enable`, `AdminSetDisabled`): anders als `revoke` macht `enable` bestehende, nicht widerrufene Zertifikate sofort wieder nutzbar, keine erneute Anmeldung nötig; auch hier `#[serde(default)]`, keine Migration nötig. Beide Aktionen sind gegen das Entfernen des letzten funktionsfähigen Admins geschützt (§6.4).

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
cn      = "alice"                    # Subject-CN dieses Zertifikats; "" = vor der Umstellung ausgestellt
issued  = "2026-09-20T12:00:00Z"
expires = "2027-09-20T12:00:00Z"
revoked = false
# renewed = "..."                    # nur gesetzt, wenn damit ein Nachzügler-Renewal lief
```

`CertRecord.registration = true` markiert ein Registrierungszertifikat (`PROTOCOL.md` §2.3): der Nutzer ist eingeladen, hat sich aber noch nicht registriert. Widerrufen wird es erst, wenn der Client das neue Zertifikat quittiert oder zum ersten Mal damit einloggt (§6.3); ein nie quittiertes verschwindet nach Ablauf samt Nutzer — auch nach einer unquittierten Einlösung, deren Zertifikat womöglich nie ankam; wer es doch gespeichert, aber bis zum Ablauf des Bundles nie eingeloggt hat, braucht eine neue Einladung — (beim Start und bei jeder neuen Ausstellung), damit der Platzhaltername und der Admin-Platz eines leeren Bestands wieder frei werden. Ein Invite-Verzeichnis gibt es nicht mehr.

Zeitstempel durchgehend RFC 3339 in UTC; Fingerprints lowercase Hex ohne Trenner; `schema` erlaubt spätere Migrationen.

## 5. CLI

| Kommando | Wirkung |
| --- | --- |
| `icu-server run [--config PATH]` | Server starten; beim ersten Start CA und Server-Leaf erzeugen. |
| `icu-server invite [--name neu-bob] [--ttl 24h] [--out datei]` | Registrierungs-Bundle ausstellen (`PROTOCOL.md` §2.3): Schlüssel + Registrierungszertifikat + CA als Passport, auf stdout oder mit Modus 0600 in `--out`. `--name` ist nur der Platzhaltername, bis der Eingeladene seinen eigenen wählt; ohne ihn wird `neu-xxxx` erzeugt. `--ttl` höchstens 7 Tage. Das erste Bundle auf einem leeren Server ist das des Admins; es geht auch ohne laufenden Server. |
| `icu-server users` | Tabelle: `user_id`, Name, Fingerprint (gekürzt), ausgestellt, läuft ab, revoked, admin, disabled, pending (eingeladen, Registrierung nicht quittiert — `UserRecord::is_pending`), online. |
| `icu-server revoke <name\|user_id\|fingerprint>` | `revoked = true` auf **allen** Certs des Nutzers; eine laufende Verbindung wird sofort mit `KICKED` geschlossen. Danach neue CRL, ohne Neustart in Kraft (`endpoint::reload_tls`, §6.3): der nächste Handshake scheitert mit `certificate_revoked`. |
| `icu-server promote <name\|user_id\|fingerprint>` | `is_admin = true`. Pusht `UserAdminChanged { is_admin: true }` an jede eigene Sitzung des Nutzers, wenn online. |
| `icu-server demote <name\|user_id\|fingerprint>` | `is_admin = false`. Abgelehnt mit `LAST_ADMIN`, wenn der Nutzer der letzte funktionsfähige Admin ist. Sonst wie `promote`, mit `is_admin: false`. |
| `icu-server disable <name\|user_id\|fingerprint>` | `disabled = true`; eine laufende Verbindung wird sofort mit `AUTH_DISABLED` geschlossen. Abgelehnt mit `LAST_ADMIN`, wenn der Nutzer der letzte funktionsfähige Admin ist. |
| `icu-server enable <name\|user_id\|fingerprint>` | `disabled = false`; bestehende, nicht widerrufene Zertifikate sind sofort wieder gültig. |
| `icu-server status` | Online-Liste, Uptime, Relay-Zähler. |
| `icu-server rotate-server-cert` | Neues Server-Leaf von derselben CA, sofort in Kraft (kein Neustart mehr); das alte kommt auf die CRL (`users.toml` `[[revoked_server]]`), damit Clients es ab ihrer nächsten Verbindung ablehnen — für den Fall, dass sein Schlüssel abhandengekommen ist. Clients brauchen nichts. |

Einladen geht auch über die authentifizierte `icu/1`-Verbindung (`CreateInviteRequest`, `PROTOCOL.md` §2.2) — ein eingeloggter Admin braucht dafür weder CLI noch Host-Zugriff, nur `Welcome.is_admin`. Alle drei Wege laufen über `registration::issue`.

**Wer schreibt den State.** Ein laufender Server ist der alleinige Schreiber von `users.toml`. Jedes CLI-Kommando außer `run` geht deshalb über den Admin-Socket; der Server führt die Änderung aus, persistiert sie und wendet sie sofort auf offene Verbindungen an. Nur wenn kein Server läuft (Socket nicht vorhanden oder `ECONNREFUSED`), schreibt die CLI selbst und hält dabei `flock(/var/lib/icu/state.lock)`. Damit wirkt ein frisch ausgestelltes Bundle sofort statt erst nach dem nächsten Neustart, und CLI und Server können einander keine Änderung überschreiben.

Admin-Socket: `/run/icu/admin.sock` (`RuntimeDirectory=icu`, `RuntimeDirectoryMode=0750`, Gruppe aus `admin_socket_group`), JSON-Zeilenprotokoll, ein Request pro Zeile, eine Response pro Zeile, dann Close. Kein Netzwerk-Admin, keine Authentifizierung außer den Dateirechten des Sockets.

| Kommando | Request | Response |
| --- | --- | --- |
| invite | `{"cmd":"invite","name":"bob","ttl_s":86400}` | `{"ok":true,"bundle":"-----BEGIN ICU PASSPORT-----…","expires":"2026-09-21T12:00:00Z","user_id":7,"name":"bob","is_admin":false}` |
| users | `{"cmd":"users"}` | `{"ok":true,"users":[{"user_id":1,"name":"alice","fingerprint":"9f86d0…","expires":"…","revoked":false,"online":true,"is_admin":true}]}` |
| revoke | `{"cmd":"revoke","user":"alice"}` | `{"ok":true,"user_id":1,"closed":1}` bzw. `{"ok":false,"error":"LAST_ADMIN"}` |
| promote | `{"cmd":"promote","user":"bob"}` | `{"ok":true}` |
| demote | `{"cmd":"demote","user":"bob"}` | `{"ok":true}` bzw. `{"ok":false,"error":"LAST_ADMIN"}` |
| disable | `{"cmd":"disable","user":"bob"}` | `{"ok":true,"user_id":2,"closed":1}` bzw. `{"ok":false,"error":"LAST_ADMIN"}` |
| enable | `{"cmd":"enable","user":"bob"}` | `{"ok":true}` |
| status | `{"cmd":"status"}` | `{"ok":true,"uptime_s":3600,"online":3,"relay":{"datagrams_in":…,"datagrams_out":…,"dropped":{…}}}` |

Fehlerform einheitlich `{"ok":false,"error":"<kurzform>","detail":"<text>"}`.

**Kein `rename` mehr** (entfallen 2026-09-22): der Name steht im Zertifikat, und der Server kann keinen Schlüssel für einen Nutzer erzeugen — umbenennen kann sich also nur der Nutzer selbst, über ein Renewal (`PROTOCOL.md` §2.2 Nr. 9). Muss ein Admin trotzdem eingreifen (anstößiger Name, Tippfehler, von Hand verkorkster Bestand): Server stoppen, `UserRecord.name` in `users.toml` editieren, Server starten. Der nächste Connect dieses Nutzers ist dann ein CN-Mismatch, und sein Client zieht selbsttätig nach — Notausgang und Nachzügler-Mechanismus sind dasselbe. Als härtere Mittel bleiben `revoke` plus neues Bundle.

## 6. Ablauf

### 6.1 Start

1. Config laden, validieren: Listen-Adressen, `server_name` nicht leer, `1 ≤ max_users ≤ 64`, `min_client` als SemVer parsebar, und `max_users × 100 B + Reserve < 16 KiB` (sonst passt ein volles `Welcome` nicht in ein Control-Frame, `PROTOCOL.md` §7).
2. CA laden oder erzeugen (`rcgen::CertificateParams` mit `IsCa::Ca(Constrained(0))`, `KeyCertSign|CrlSign`, P-256; Spike 2b). Eine neue CA heißt `ICU CA <server_name>` — daraus nimmt der Client das SNI (`PROTOCOL.md` §2.3). Eine bestehende CA bleibt, wie sie ist.
3. Server-Leaf laden oder erzeugen (SAN = `server_name` und, falls IP, IP-SAN; EKU ServerAuth) über `CertificateParams::signed_by(&leaf_key_pair, &issuer)` — in rcgen 0.14 nimmt `signed_by` **Public Key und Issuer**, anders als die CSR-Variante in §6.3; Kette = Leaf + CA.
4. `flock(state.lock)` nehmen und für die gesamte Laufzeit halten; `users.toml` laden (`schema` prüfen); nie quittierte, abgelaufene Registrierungen verwerfen. Ab hier ist der Server alleiniger Schreiber des States (§5).
5. rustls `ServerConfig` in **dieser** Reihenfolge — `builder_with_provider(rustls::crypto::ring::default_provider().into())` liefert `WantsVersions`, der Versionsschritt muss also vor dem Verifier kommen, sonst übersetzt es nicht:
   `ServerConfig::builder_with_provider(ring_provider)` → `.with_protocol_versions(&[&rustls::version::TLS13])?` → `.with_client_cert_verifier(WebPkiClientVerifier::builder(Arc::new(roots)).allow_unauthenticated().build()?)` → `.with_single_cert(vec![leaf, ca], key)?`; danach `alpn_protocols = [b"icu/1"]` und `max_early_data_size = 0`.
6. quinn `ServerConfig` aus `QuicServerConfig::try_from(rustls_config)`; Transport wie `PROTOCOL.md §1`. Signaturhinweise: `max_idle_timeout(Some(IdleTimeout::try_from(Duration::from_secs(60))?))`, `datagram_receive_buffer_size(Some(64 * 1024))` (Option) gegen `datagram_send_buffer_size(64 * 1024)` (kein Option), `max_concurrent_uni_streams(VarInt::from_u32(2 * max_users + 4))`. `migration(true)` liegt auf `quinn::ServerConfig`, nicht auf `TransportConfig`.
7. Ein `Endpoint` pro Listen-Adresse; Accept-Loop je Endpoint auf einem tokio-Multi-Thread-Runtime.
8. Admin-Socket öffnen (`/run/icu/admin.sock`, Gruppe aus `admin_socket_group`, Modus 0660). SIGTERM → allen `close(SHUTDOWN)`, 1 s warten, Socket und `state.lock` freigeben, Ende.

### 6.2 Accept

**Eine Verbindung pro Zertifikat** (2026-09-24, `PROTOCOL.md` §7): nach `authenticate` trägt `session::LiveCert` die Verbindung unter dem Leaf-Fingerprint in `Server.live_certs` ein, für normale und Registrierungssitzungen. Stand dort schon eine andere, wird sie mit `SESSION_REPLACED` geschlossen (Raum-Sitzung vorher per `mark_closing` markiert, damit `UserLeft` den Code trägt); der Eintrag wird beim Ende nur entfernt, wenn er noch dieser Verbindung gehört (`stable_id`).

```
incoming = endpoint.accept()
  if offene Verbindungen ≥ max_connections (256) → incoming.refuse()   // Handshake oder etabliert, alle Endpoints
  if under_load (≥ 32 offene Verbindungen) → incoming.retry()      // Adressvalidierung, kostet kein Budget
  if limits.ip_bucket(/64 bzw. IPv4).exhausted || global_bucket.exhausted → incoming.refuse()
  if IP-Tabelle voll (4096) → validiert: nur globales Budget, sonst retry()/refuse()
connecting = incoming.accept()                // ab hier ≤ 10 s bis Handshake-Ende, sonst verworfen
// handshake_data() nimmt &mut self und liefert Box<dyn Any>; Downcast auf quinn::crypto::rustls::HandshakeData
alpn = connecting.handshake_data().await?
         .downcast::<quinn::crypto::rustls::HandshakeData>()?.protocol   // Spike 2b: vor Handshake-Ende lesbar
conn = connecting.await
match alpn:
  icu/1        → session::handle(conn)
```

### 6.3 Registrierung (`registration.rs`, `session.rs`)

Ablauf und Regeln normativ in `PROTOCOL.md` §2.3; hier nur die Umsetzung.

**Ausstellen** (`registration::issue`, gemeinsam für CLI, Admin-Socket und `CreateInviteRequest`), unter der Store-Sperre:
1. Abgelaufene, nie quittierte Registrierungen verwerfen (`store.purge_expired_registrations`) — sonst hielte ein verfallenes erstes Bundle den Bestand nicht-leer und kostete das nächste die Admin-Rechte bis zum Neustart.
2. Platzhaltername: der mitgegebene (muss `validate_name` erfüllen und frei sein, sonst `NAME_TAKEN`), sonst `neu-` + 4 Hex-Ziffern, bis einer frei ist.
3. `ca.issue_registration`: frischer P-256-Schlüssel, Subject `CN=<Platzhalter>, OU=ICU registration`, URI-SAN `icu://<public_host>`, EKU ClientAuth, `not_after = now + ttl` (Default 24 h, höchstens 7 Tage).
4. `store.add_registration`: Nutzer mit `CertRecord { registration: true }`; `is_admin = store.is_empty()`. Der Schlüssel wird nicht gespeichert, er verlässt den Server nur im Bundle (`PassportV2`).

**Einlösen** (`session::serve_registration`): `authenticate` liest `CertRecord.registration` und den SubjectPublicKeyInfo des vorgelegten Leafs. Ist es eine Registrierung, läuft die Sitzung nicht über den Raum:
1. IP-Ratenlimit (`allow_enroll`, 5/min pro IP, 20/min global) → sonst `close(ENROLL_RATE_LIMITED)`.
2. `Hello` wie in §6.4 Schritt 2, dann direkt auf den Stream `Welcome { registration: true, users: [], you.session: 0 }`.
3. Schleife, höchstens 120 s: `Ping` → `Pong`; `RenewRequest` → `redeem`; alles andere → `PROTOCOL_VIOLATION`. `redeem`: CN aus der CSR (`csr_common_name`), Schlüssel ≠ Registrierungsschlüssel, Name frei (außer für sich selbst), `sign_client_csr`, dann `store.redeem_registration` — neues Cert anhängen, Namen übernehmen, Zertifikate früherer, unquittierter Einlösungen widerrufen, **ein** Schreibvorgang. Das Registrierungszertifikat bleibt dabei gültig (revidiert 2026-09-24). Nicht-fatale Fehler gehen als `Error` hinaus, die Sitzung bleibt für einen neuen Versuch offen (höchstens 10, dann `RATE_LIMITED_FATAL`).
4. Nach `RenewResponse` wartet der Server bis zu 5 s darauf, dass der Client schließt, statt selbst zu schließen — ein eigenes Close könnte die Antwort überholen. Schließt der Client mit `NO_ERROR`, hat er das Zertifikat gespeichert: `store.consume_registration` widerruft das Registrierungszertifikat. Jeder andere Ausgang (anderer Code, Timeout, Abbruch) lässt das Bundle gültig, damit eine verlorene Antwort oder ein gescheitertes Speichern es nicht verbrennt. `authenticate` verbraucht die Registrierung zusätzlich beim ersten Login mit einem echten Zertifikat des Nutzers (Absturz nach dem Speichern, vor der Quittung).

**Client-Zertifikate** (`sign_client_csr`, auch für Renewals): Subject nur der geprüfte CN aus der CSR (als UTF8String neu gebaut), einziger SAN die URI `icu://<public_host>`, EKU ClientAuth, keine Key Usage, `not_before = now − 1 h`, `not_after = now + client_validity_days`, Serial = 16 Zufallsbytes; nur P-256- und P-384-Schlüssel. Der CN wird mit x509-parser aus dem rohen DER gelesen (nicht über rcgens `DistinguishedName`, das zwei CNs still zu einem verschmelzen würde): genau einer, als UTF-8 lesbar, `validate_name` erfüllt.

**CRL** (2026-09-24, `PROTOCOL.md` §1): `CertRecord.serial` (Hex der Serial, beim Ausstellen gesetzt; alte Einträge ohne) und `UsersFile.revoked_server`/`crl_number` liefern `Store::crl_serials`; `Ca::crl` signiert daraus eine CRL (rcgen, CRL-Nummer aus `Store::next_crl_number`, streng monoton, `nextUpdate` +30 d, nicht erzwungen). `endpoint::server_config` hängt sie an den `WebPkiClientVerifier` (`with_crls`, `only_check_end_entity_revocation`). `endpoint::reload_tls` baut CRL und TLS-Konfiguration neu, setzt sie per `set_server_config` auf alle Endpoints (`Server.endpoints`) und broadcastet `CrlUpdate`; aufgerufen beim Start (Offline-Widerrufe greifen ab dem ersten Handshake), nach `revoke` (Socket und in-band), nach jeder verbrauchten oder wiederholten Registrierung und nach `rotate-server-cert`. Nie unter dem Store-Lock aufrufen; Lock-Reihenfolge überall Store → CA. Neue Sitzungen bekommen die aktuelle CRL direkt nach `Welcome`.

### 6.4 Session (`session.rs`)

1. `peer_identity()` → `None` → `close(AUTH_REQUIRED)`. Sonst Leaf-DER → SHA-256 → `users.toml`: unbekannt → `AUTH_UNKNOWN_CERT`, revoked → `AUTH_REVOKED`, `disabled` → `AUTH_DISABLED`. (rustls hat Kette und Ablauf schon geprüft.) Zusätzlich wird der Subject-CN des Leafs gelesen und in `Identity` mitgeführt; weicht er vom gespeicherten Namen ab, wird das **nur protokolliert, nie abgelehnt** — der Server bewahrt keine Zertifikats-DER auf und könnte den CN für Bestandszertifikate gar nicht kennen. Der Client gleicht sich selbst an (Nachzügler-Renewal).
2. `accept_bi()` und erstes Frame binnen 5 s — **eine** gemeinsame Frist ab Beginn, nicht je 5 s —, sonst `HELLO_TIMEOUT`. Muss `Hello` sein. `minor` aushandeln (`min(client, server)`, nicht bedienbar → `VERSION_UNSUPPORTED`). `Hello.name` wird ignoriert (`PROTOCOL.md` §2.2).
3. Ist der Raum voll (`room.len() >= max_users`), `close(SERVER_FULL)`. Unmittelbar vor `room.join()` werden Revoke und `disabled` unter dem Store-Lock erneut geprüft, und das Lock bleibt bis nach dem Join gehalten: ein Revoke während des `Hello`-Wartens trifft sonst niemanden, weil der Kick nur Raum-Mitglieder sieht. Sonst `room.join()` → Session-ID (monoton; nach 65 535 läuft der Zähler auf 1 um und überspringt belegte IDs, bei leerem Raum beginnt er wieder bei 1), `Welcome { you: UserInfo{user_id, session, name, flags}, users, audio, min_client, minor, caps, is_admin }` senden — `name` ist der gespeicherte Name, also der CN des jüngsten Zertifikats, nicht der des vorgelegten —, `UserJoined` an alle anderen. Beides wird unter dem Raum-Lock eingereiht, der das Mitglied einfügt, damit kein Broadcast vor `Welcome` ankommt.
4. Zwei Tasks pro Client: (a) Control-Reader (Frames → `ControlMsg` → Handler), (b) Datagram-Reader (`read_datagram` → `relay::audio`). Video: `accept_uni` in einem dritten Task → `relay::video`.
5. Ausgehend: ein begrenzter `mpsc`-Kanal (256 Frames) pro Client für Control-Frames (Broadcasts), ein Writer-Task schreibt auf den Control-Stream. Ist der Kanal voll, liest der Client seinen Control-Stream nicht mehr; er wird mit `TIMEOUT` geschlossen, statt dass sein Rückstau im Server wächst. Datagrams werden direkt aus dem Relay per `send_datagram` geschickt (nicht blockierend).
6. Ende: Verbindung geschlossen oder Fehler → `room.leave()` → `UserLeft { session, reason }` Broadcast; `reason` = `NO_ERROR` bei sauberem Close, `TIMEOUT` bei Idle-Timeout oder Transportabbruch, sonst der Code, mit dem der Server geschlossen hat. Die Session-ID wird erst nach einem Zählerumlauf wieder vergeben.

#### Control-Handler (vollständig, `PROTOCOL.md` §2.2)

| Message | Server tut |
| --- | --- |
| `SetState` | mute/deafen/camera_on/viewing übernehmen, bit2 (`speaking`) verwerfen (§6.5 leitet es ab), `UserState` an alle inkl. Verursacher; ≤ 20/s, darüber `Error(RATE_LIMITED)`. |
| `Ping` | unverzüglich `Pong { nonce, server_time_ms }`, `server_time_ms` = Unix-Millisekunden UTC. Zählt gegen das Control-Limit. |
| `SetName` | Entfällt. Nicht mehr C→S: Empfang schließt mit `PROTOCOL_VIOLATION`. Umbenennen läuft über `RenewRequest`. |
| `LossReport` | Maximum je Sprecher fortschreiben, siehe §6.5. |
| `KeyframeRequest` | an den Sprecher weiterleiten, siehe §6.5. |
| `RenewRequest` | CSR wie in §6.3 („Client-Zertifikate“) prüfen; der CN **kommt aus der CSR** und ist der gewünschte Name. Drei Fälle, unterschieden durch exakten Stringvergleich gegen den gespeicherten Namen (exakt, nicht `name_key`: sonst liefe eine reine Groß-/Kleinschreibungs-Umbenennung auf dem Zweitgerät als normales Renewal ins Tageslimit): **normales Renewal** (CN = gespeichert, vorgelegtes Cert aktuell) erst ab 30 Tage vor Ablauf des vorgelegten Zertifikats, zählt gegen `last_renew`, ≤ 1/Tag; **Nachzügler** (CN = gespeichert, vorgelegter CN alt) ist davon ausgenommen und stattdessen über `CertRecord.renewed` des vorgelegten Zertifikats auf einen pro Tag begrenzt; **Umbenennung** (CN ≠ gespeichert) prüft zuerst `name_taken(cn, Some(self))` → `Error(NAME_TAKEN)` und zählt sonst gegen `last_renew`. Die Eindeutigkeitsprüfung liegt **vor** dem Ratenlimit, damit ein belegter Name `NAME_TAKEN` meldet und nicht eine verwirrende Ratenmeldung. Danach `store.add_cert`: Cert anhängen, CN als Namen übernehmen, ggf. den vorgelegten Fingerprint stempeln — **ein** Schreibvorgang, damit kein Crash zwischen zwei Writes einen Nutzer hinterlässt, dessen Name nicht zu seinem jüngsten Zertifikat passt. Gestempelt wird nur beim Nachzügler: dasselbe Zertifikat kann auf einem zweiten Gerät liegen (Passport), das seinen eigenen Nachzügler noch vor sich hat. Antwort `RenewResponse { leaf_der }`; bei geändertem Namen zusätzlich `room.rename` + `UserRenamed` für **jede** Sitzung dieses Nutzers. Das alte Cert bleibt bis zu seinem `expires` gültig. Abgelaufene, nicht widerrufene `CertRecord`s räumt derselbe Schreibvorgang weg; widerrufene bleiben stehen. |
| `CreateInviteRequest` | Nur wenn `store.is_admin(user_id)`, sonst `Error(NOT_ADMIN)` und nichts weiter. Sonst `registration::issue` (§6.3) und `CreateInviteResponse { bundle, expires }`; ein belegter oder ungültiger Platzhalter → `Error(NAME_TAKEN)`. |
| `AdminSetRole` | Nur wenn `store.is_admin(user_id)` (live geprüft), sonst `AdminActionError(NOT_ADMIN)`. Sonst `store.set_admin(target, is_admin)`; bei `LAST_ADMIN` `AdminActionError(LAST_ADMIN)`. Bei Erfolg `AdminSetRoleOk` an den Absender und `UserAdminChanged { is_admin }` unicast an jede eigene Sitzung des Ziels (live, ohne Reconnect). |
| `AdminSetDisabled` | Wie `AdminSetRole`, aber `store.set_disabled(target, disabled)`. Wird dabei deaktiviert, kickt der Server jede laufende Verbindung des Ziels sofort mit `AUTH_DISABLED` (`session::kick_user`) — kein separater Push, das Schließen ist die Benachrichtigung. `AdminSetDisabledOk` an den Absender. |
| `AdminRevokeUser` | Wie `AdminSetRole`, aber `store.revoke(target)` und `session::kick_user(target, KICKED)`. `AdminRevokeUserOk { user_id, closed }` an den Absender. |
| `UserListRequest` | Nur wenn `store.is_admin(user_id)`, sonst `AdminActionError(NOT_ADMIN)`. Sonst jeden registrierten Nutzer (online oder nicht) als `UserSummary` sammeln (`online` über `room.sessions_of_user`, `pending` über `UserRecord::is_pending`) und als `UserListResponse { users }` zurücksenden. |
| unbekannte Variante / Decode-Fehler | `close(PROTOCOL_VIOLATION)`. |

**Letzter-Admin-Schutz** (`store::guard_last_admin`, gilt für `set_admin(_, false)`, `set_disabled(_, true)` und `revoke`, über alle drei Pfade — in-band, Admin-Socket, CLI-Offline-Fallback — identisch, da eine einzige Store-Methode): ein Nutzer zählt als *funktionsfähiger Admin*, wenn `is_admin && !disabled && mindestens ein Cert nicht revoked`. Die Aktion wird nur abgelehnt, wenn ihr Ziel aktuell funktionsfähiger Admin ist **und** danach kein funktionsfähiger Admin mehr übrig wäre — ein bereits deaktivierter oder vollständig revoked Admin zählt nicht mehr mit und blockiert daher nichts.

### 6.5 Relay (`relay.rs`)

Audio: siehe `PROTOCOL.md §3`. Implementierung: `Bytes` aus `read_datagram`, `BytesMut::from` nur für die 2 Speaker-Bytes, dann `freeze()`; ein `Bytes`-Clone pro Empfänger (Refcount). Das ist der einzige heiße Pfad des Servers (16 Clients × 50 Pakete/s), deshalb prüft er zuerst das Datagram-Ratenlimit (nur der Bucket des Tasks, kein geteilter Zustand) und holt erst dann das Room-Lock, genau zweimal — einmal für Absenderflags **und** Empfängerliste zusammen (`room.audio_fanout`), einmal für die Talk-Spurt-Buchführung — und schreibt die Empfängerliste in einen Puffer, den der `datagram_task` besitzt und über alle Datagrams wiederverwendet; pro Datagram wird also nichts alloziert. Zähler pro Empfänger für `TooLarge | UnsupportedByPeer | Disabled | ConnectionLost` — das sind die Varianten von `quinn::SendDatagramError`; ein `Blocked` gibt es nur auf `quinn_proto::SendDatagramError` und nie auf der hier benutzten High-Level-API. Zusätzlich setzt der Server `speaking` implizit: erstes Datagram nach ≥ 500 ms Stille → `UserState{speaking=1}` Broadcast, `END`-Flag oder 500 ms ohne Datagram → `speaking=0`. Damit brauchen Clients für den Indicator keine eigene Meldung; ein von Clients in `SetState` gesetztes bit2 wird ignoriert (`PROTOCOL.md` §2.1: bit2 ist serverseitig autoritativ).

Video: pro Sender ein Task-Slot. Frame-Header lesen, prüfen, `speaker` setzen. Pro Empfänger `open_uni()`, Header schreiben, `read_chunk(max_len, true)` → `write_chunk` durchreichen, `finish()` (in quinn 0.11 **synchron**, `Result<(), ClosedStream>`; für „zugestellt“ `stopped()` awaiten). Die Drop-Regel gilt **pro Sender-Empfänger-Paar**: hat ein Empfänger noch einen Frame desselben Senders offen, wird der ältere per `reset(RATE_LIMITED)` verworfen — ein langsamer Empfänger bremst die übrigen damit nicht aus. Bitrate-Limit gleitend über 2 s. Header zuerst: Kamera-Flag, aktiver Sender und Frame-Rate werden am 12-Byte-Header geprüft, ein verworfener Frame wird mit `stop(RATE_LIMITED)` abgelehnt statt gelesen; erst dann wird die Nutzlast (≤ 512 KiB) gelesen und gegen das Bitraten-Budget gebucht, das dabei ins Minus gehen darf — sonst passte ein Frame über 100 KB (2 s × 400 kbit/s) nie. Lebensdauer der Slots: ein Slot hängt an der `stable_id` der Empfängerverbindung und darf einen Sender nicht überleben. Verlässt ein Empfänger den Raum oder gehört seine Session-ID nach einem Umlauf einer anderen Verbindung, fällt sein Slot beim nächsten Frame weg; verlässt der **Sender** den Raum, schließt `Drop for VideoSender` jeden seiner Slots. Beides verwirft den noch wartenden Frame und weckt den Forwarder-Task, der sonst bis zum Verbindungsende des Empfängers auf einem Slot parken würde, den niemand mehr füllt.

Video-Empfänger sind nur die anderen Teilnehmer mit `viewing` (`PROTOCOL.md` §2.1, `room.video_receivers`); wer nicht zuschaut, bekommt keine Frames, und sein Slot fällt beim nächsten Frame weg. Ist die Empfängermenge leer (niemand sonst `Active`, oder niemand schaut zu), wird der eingehende Stream trotzdem geleert — `stop(0x00)` auf dem `RecvStream` oder bis FIN lesen und verwerfen —, sonst läuft die Stream-Flow-Control des Senders voll. Zähler `video_dropped_no_receiver`.

**`VideoCongestionHint`** (`PROTOCOL.md` §2.2 Variante 16, 2026-09-21): pro Sender (im selben `VideoSender`, keine neue Struktur) eine rollierende Offer/Verdrängungs-Zählung je Empfänger, alle 2 s zu einer Verdrängungsrate `Verdrängungen · 1000 / Angebote` je Empfänger verrechnet; das **Maximum über alle Empfänger** dieses Senders wird bei ≥ 20 ‰ Änderung als `VideoCongestionHint { drop_permille }` an den Sender geschickt — dieselbe Maximum-Regel wie bei `LossHint`, nur für Video statt Audio. Vereinfachung: das Fenster schließt nur bei einem eingehenden Frame; hört ein Sender ganz auf zu senden (Kamera aus), bleibt der letzte Hinweis stehen statt auf 0 zu verfallen — harmlos, weil ohne Video nichts mehr anzupassen ist. Der Client speist den Wert in seine Qualitäts-Ladder (`Client/IMPLEMENTATION.md` §8); der Server selbst transkodiert nichts, die Anpassung passiert ausschließlich beim sendenden Client.

`LossReport` → Maximum pro Sprecher über alle Empfänger (jeder Report verfällt nach 6 s) → `LossHint` an den Sprecher, wenn sich der Wert ändert. **Der Verfall ist eine Änderung:** fallen alle Reports eines Sprechers weg, sendet der Server `LossHint { loss_permille: 0 }`, sonst bliebe der FEC-Overhead beim Sender dauerhaft stehen. `KeyframeRequest` → an den Sprecher mit `speaker = 0`, ≤ 1/s.

### 6.6 Limits (`limits.rs`)

Token-Buckets pro Client (Control 50/s, Audio 110/s, Video 20/s und 400 kbit/s) und pro IP (Registrierungssitzungen 5/min, Handshakes 10/s; IPv6 zählt pro /64, weil ein Host meist ein ganzes /64 hält, IPv4-mapped als IPv4), dazu global (Handshakes 100/s, Registrierungen 20/min, höchstens 256 offene Verbindungen, Handshake ≤ 10 s). Konfigurierbar als `limits.handshakes_global_per_s`, `limits.max_connections` (mindestens `2 × max_users`) und `timing.handshake_timeout_ms`. Abgewiesene Verbindungen ohne Zertifikat (`AUTH_REQUIRED`) loggt der Server nur auf `debug`, damit Scanner das Journal nicht füllen. Überschreitung: Datagram/Frame verwerfen und zählen; bei Control `Error(RATE_LIMITED)` (nicht fatal, Verbindung bleibt), beim dritten Verstoß binnen 60 s `close(RATE_LIMITED_FATAL)` — ein eigener, fataler Code, damit der Client nicht in einen Reconnect-Loop läuft (`PROTOCOL.md` §5/§6).

## 7. Logging und Metriken

`tracing` mit `tracing-subscriber` (text/json), Felder: `session`, `name`, `remote` (nur bei Connect/Disconnect, nie bei jedem Datagram). Zähler (atomics, über `status` abrufbar): Verbindungen, Relay-Datagrams in/out, verworfene Datagrams pro Grund, Video-Frames, Registrierungsversuche (JSON-Schlüssel weiterhin `enroll`). Kein Prometheus in v1; `status --json` reicht für einen Checkmk-Local-Check.

## 8. systemd-Unit

```ini
[Unit]
Description=ICU voice server
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/bin/icu-server run --config /etc/icu/server.toml
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
| sha2 | 0.10 | MIT/Apache-2.0 | Fingerprint (oder ring::digest) |
| rand | 0.9 | MIT/Apache-2.0 | Platzhalternamen, Cert-Serial (16 B) |
| time | 0.3 | MIT/Apache-2.0 | `OffsetDateTime` für rcgen-Validity, RFC-3339 im State |
| rustix | 1 | Apache-2.0/MIT | `flock` auf `state.lock`, Unix-Socket-Rechte |

Kein libopus, kein Codec, keine GUI auf dem Server.

Lizenz-Allowlist: **eine** `deny.toml` im Workspace-Root, gültig für Server und Client — MIT, Apache-2.0, ISC, BSD-2-Clause, BSD-3-Clause, Zlib, Unicode-3.0; `IJG` ausschließlich hinter dem Client-Feature `mjpeg`. Alles andere bricht den Build.

Lizenz von ICU selbst: **Apache-2.0** (entschieden 2026-09-24 mit der Veröffentlichung auf GitHub, Empfehlung aus `Research/packaging-licensing.md`); `license` steht in `[workspace.package]`, Text in `LICENSE`.

## 10. Tests

| Ebene | Inhalt |
| --- | --- |
| Unit (`icu-proto`) | Roundtrip aller `ControlMsg`, Passport v1/v2; Header-Encode/Decode; unbekannte Variante → `Err`; Framing-Grenzen (0, 16384, 16385). |
| Unit (`ca.rs`, `store.rs`) | CA-Erzeugung; CSR-CN-Prüfung (CN wird übernommen, aber genau einer, `validate_name`-konform, SAN/EKU/`is_ca` weiterhin servergesetzt; kein CN, zwei CNs, ungültiger CN → `ENROLL_BAD_CSR`); `leaf_common_name` degradiert bei Müll zu `""`; Fingerprint-Stabilität; atomares Schreiben; Revoke-Persistenz über **alle** Certs eines Nutzers; `user_id`-Vergabe und -Stabilität über Renewal; `add_user`/`add_cert` weisen Kollisionen ab und schreiben dabei nichts; `add_cert` übernimmt den CN und meldet die Änderung; Altdatensatz ohne `cn` lädt; `schema`-Prüfung beim Laden; `flock`-Verhalten bei parallelem CLI-Zugriff. |
| Integration (in-process, `127.0.0.1:0`) | Registrierung → Connect → Hello/Welcome; Bundle nach Quittung (sauberes Close) oder erstem Login verbraucht, danach `AUTH_REVOKED`; unquittierte Einlösung wiederholbar, das erste Zertifikat wird dabei widerrufen; Registrierung mit dem Bundle-Schlüssel → `RENEW_REJECTED`; abgelaufenes Bundle verbindet nicht; Registrierungssitzung darf nur `Ping`/`RenewRequest`; Zertifikate tragen `icu://`-URI und CA-CN `ICU CA <server_name>`; erstes Bundle = Admin; Connect ohne Cert → `AUTH_REQUIRED`; unbekanntes Cert → `AUTH_UNKNOWN_CERT`; Revoke während Verbindung → `KICKED`; Relay: 3 Clients, Audio von A kommt bei B und C mit `speaker = A` an, nicht bei A; `deafened` C bekommt nichts; `muted` A wird nicht relayt; Rate-Limits; Video-Relay mit Frame-Verwerfen; `LossReport` → `LossHint`; `KeyframeRequest`-Weiterleitung; Hello-Timeout; `max_users` erreicht → `SERVER_FULL`; `Ping` → `Pong` mit plausibler `server_time_ms`; belegter Name bei der Registrierung → `NAME_TAKEN`, **und das Bundle bleibt benutzbar**; Renewal mit neuem CN benennt um und broadcastet `UserRenamed`, das alte Cert verbindet weiter und sieht den neuen Namen; Renewal auf einen belegten Namen → `NAME_TAKEN`, nichts geändert, Verbindung überlebt; veralteter CN verbindet und darf am selben Tag nachziehen, ein zweites Mal mit demselben Zertifikat nicht; `SetName` → `PROTOCOL_VIOLATION`; `Hello.name` wird ignoriert; Bundle ohne Namen bekommt einen Platzhalter; `RenewRequest` → `RenewResponse`, altes und neues Cert verbinden beide; normales Renewal mehr als 30 Tage vor Ablauf → `RENEW_REJECTED`; dritter `RATE_LIMITED`-Verstoß → `close(RATE_LIMITED_FATAL)`; Video-Frame ohne Empfänger wird geleert statt zu stauen; Verfall aller `LossReport` → `LossHint{0}`; Bundle über den Admin-Socket wirkt ohne Serverneustart. |
| Last | 16 Clients × 50 Datagrams/s für 60 s: keine Verluste auf Loopback, CPU < 10 % eines Kerns, keine Latenzzunahme (Datagram-Roundtrip p99 < 1 ms). |
| Netz | `tc netem` 30 ms ± 10 ms, 2 % Verlust: Relay verliert nichts zusätzlich; Keepalive hält die Verbindung über 10 min Stille. |

## 11. Milestones

1. **M1 Skeleton**: Workspace, `icu-proto` mit allen Typen + Tests, Server startet, erzeugt CA, akzeptiert QUIC, `Hello/Welcome`.
2. **M2 Enrollment und Admin**: `invite`, `icu-enroll/1`, `users.toml` mit `user_id`-Nutzermodell, `revoke`, Renewal (`RenewRequest`/`RenewResponse`), **Admin-Socket** und die CLI darüber, Integrationstests Auth. Der Admin-Socket gehört hierher und nicht nach M5: M2 verlangt `revoke`, und der Integrationstest „Revoke während Verbindung → `KICKED`" (§10) ist ohne ihn nicht ausführbar.
3. **M3 Audio-Relay**: Datagram-Relay mit `speaker`-Override, Mute/Deafen-Semantik, Speaking-Ableitung, Rate-Limits, Lasttest.
4. **M4 Video-Relay** und `LossReport/LossHint`, `KeyframeRequest`.
5. **M5 Betrieb**: systemd-Unit, `status`-Zähler, musl-Build, `scratch`-Image, Checkmk-Local-Check.

Jeder Milestone endet mit grünen Tests und einem Eintrag in `../Research/verification.md`, wenn er einen offenen Messpunkt schließt.
