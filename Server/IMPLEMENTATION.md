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
  src/main.rs              # CLI-Dispatch (clap)
  src/config.rs            # TOML laden/validieren
  src/ca.rs                # CA erzeugen/laden, Leaf signieren, CSR-Handling (rcgen 0.14)
  src/store.rs             # users.toml: Fingerprint → {name, issued, expires, revoked}, invites.toml
  src/endpoint.rs          # quinn ServerConfig, Bind v4+v6, Accept-Loop, ALPN-Dispatch
  src/enroll.rs            # ALPN icu-enroll/1
  src/session.rs           # Zustandsautomat pro Client, Control-Stream
  src/room.rs              # Teilnehmerliste, Broadcast, Session-ID-Vergabe
  src/relay.rs             # Audio-Datagram-Relay, Video-Stream-Relay
  src/limits.rs            # Token-Bucket je Client/IP
  src/cli/{invite,users,revoke,status}.rs
```

`icu-proto` ist die einzige Stelle, an der Wire-Format definiert wird; Server und Client kompilieren dasselbe Crate.

## 4. Konfiguration `/etc/icu/server.toml`

```toml
[server]
listen = ["0.0.0.0:4711", "[::]:4711"]
server_name = "icu.example.org"      # SNI im Server-Cert und im Invite-Link
public_host = "icu.example.org:4711" # was im Invite-Link steht
max_users = 16

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

State unter `/var/lib/icu/`: `ca.key` (0600), `ca.crt`, `server.key` (0600), `server.crt`, `users.toml`, `invites.toml`. Alle Schreibzugriffe atomar (Tempfile + rename).

## 5. CLI

| Kommando | Wirkung |
| --- | --- |
| `icu-server run [--config PATH]` | Server starten; beim ersten Start CA und Server-Leaf erzeugen. |
| `icu-server invite --name alice [--ttl 24h]` | Token (128 Bit, base64url) erzeugen, in `invites.toml` mit Ablauf und Name speichern, Invite-Link ausgeben (`icu://host:port/enroll?t=…&ca=…&n=…`). |
| `icu-server users` | Tabelle: Name, Fingerprint (gekürzt), ausgestellt, läuft ab, revoked, online (über lokalen Unix-Socket `/run/icu/admin.sock` beim laufenden Server abgefragt). |
| `icu-server revoke <name\|fingerprint>` | `revoked = true` setzen; laufender Server schließt die Verbindung mit `KICKED` (Unix-Socket-Signal, sonst beim nächsten Connect wirksam). |
| `icu-server rename <name> <new>` | Anzeigename ändern. |
| `icu-server status` | Online-Liste, Uptime, Relay-Zähler. |
| `icu-server rotate-server-cert` | Neues Server-Leaf von derselben CA; Clients brauchen nichts. |

Admin-Socket: `/run/icu/admin.sock` (`RuntimeDirectory=icu`), Zeilenprotokoll JSON, nur für lokale CLI. Kein Netzwerk-Admin.

## 6. Ablauf

### 6.1 Start

1. Config laden, validieren (Listen-Adressen, `server_name` nicht leer).
2. CA laden oder erzeugen (`rcgen::CertificateParams` mit `IsCa::Ca(Constrained(0))`, `KeyCertSign|CrlSign`, P-256; Spike 2b).
3. Server-Leaf laden oder erzeugen (SAN = `server_name` und, falls IP, IP-SAN; EKU ServerAuth); Kette = Leaf + CA.
4. `users.toml` und `invites.toml` laden; abgelaufene Invites verwerfen.
5. rustls `ServerConfig`: `WebPkiClientVerifier::builder(roots=[CA]).allow_unauthenticated().build()`, `with_single_cert([leaf, ca], key)`, ALPN `[icu/1, icu-enroll/1]`, `max_early_data_size = 0`.
6. quinn `ServerConfig`: Transport wie `PROTOCOL.md §1`; `migration(true)`.
7. Ein `Endpoint` pro Listen-Adresse; Accept-Loop je Endpoint auf einem tokio-Multi-Thread-Runtime.
8. Admin-Socket öffnen. SIGTERM → allen `close(SHUTDOWN)`, 1 s warten, Ende.

### 6.2 Accept

```
incoming = endpoint.accept()
  if limits.ip_bucket(remote).exhausted → incoming.refuse()
  if under_load (≥ 32 handshakes in flight) → incoming.retry()      // Adressvalidierung
connecting = incoming.accept()
alpn = connecting.handshake_data().await.protocol                   // Spike 2b
conn = connecting.await
match alpn:
  icu-enroll/1 → enroll::handle(conn)
  icu/1        → session::handle(conn)
```

### 6.3 Enrollment (`enroll.rs`)

1. `accept_bi()` mit 5-s-Timeout; Frame lesen (≤ 8 KiB).
2. `EnrollRequest` decodieren; Token nachschlagen (konstantzeitiger Vergleich, `subtle`), abgelaufen/verbraucht → `close(ENROLL_BAD_TOKEN)`.
3. `CertificateSigningRequestParams::from_der(csr)`; Fehler → `ENROLL_BAD_CSR`. Server setzt Subject CN = Name, EKU ClientAuth, `not_before = now − 1 h`, `not_after = now + client_validity_days`, Serial = 16 Zufallsbytes; `signed_by(&issuer)`.
4. Fingerprint = SHA-256(Leaf-DER); Eintrag in `users.toml` schreiben; Token als verbraucht markieren (beides vor der Antwort persistieren).
5. `EnrollResponse { leaf_der, ca_der, server_name }` senden, `finish()`, `close(NO_ERROR)`.

Ein präsentiertes Client-Cert auf diesem ALPN wird ignoriert.

### 6.4 Session (`session.rs`)

1. `peer_identity()` → `None` → `close(AUTH_REQUIRED)`. Sonst Leaf-DER → SHA-256 → `users.toml`: unbekannt → `AUTH_UNKNOWN_CERT`, revoked → `AUTH_REVOKED`. (rustls hat Kette und Ablauf schon geprüft.)
2. `accept_bi()` und erstes Frame binnen 5 s, sonst `HELLO_TIMEOUT`. Muss `Hello` sein. `minor` aushandeln; `name` übernehmen und persistieren, wenn geändert.
3. `room.join()` → Session-ID (monoton, nie wiederverwendet), `Welcome` senden, `UserJoined` an alle anderen.
4. Zwei Tasks pro Client: (a) Control-Reader (Frames → `ControlMsg` → Handler), (b) Datagram-Reader (`read_datagram` → `relay::audio`). Video: `accept_uni` in einem dritten Task → `relay::video`.
5. Ausgehend: ein `mpsc`-Kanal pro Client für Control-Frames (Broadcasts), ein Writer-Task schreibt auf den Control-Stream. Datagrams werden direkt aus dem Relay per `send_datagram` geschickt (nicht blockierend).
6. Ende: Verbindung geschlossen oder Fehler → `room.leave()` → `UserLeft` Broadcast; Session-ID bleibt gesperrt.

### 6.5 Relay (`relay.rs`)

Audio: siehe `PROTOCOL.md §3`. Implementierung: `Bytes` aus `read_datagram`, `BytesMut::from` nur für die 2 Speaker-Bytes, dann `freeze()`; ein `Bytes`-Clone pro Empfänger (Refcount). Zähler pro Empfänger für `TooLarge|Blocked|UnsupportedByPeer`. Zusätzlich setzt der Server `speaking` implizit: erstes Datagram nach ≥ 500 ms Stille → `UserState{speaking=1}` Broadcast, `END`-Flag oder 500 ms ohne Datagram → `speaking=0`. Damit brauchen Clients für den Indicator keine eigene Meldung, das Flag in `SetState` bleibt erlaubt.

Video: pro Sender ein Task-Slot. Frame-Header lesen, prüfen, `speaker` setzen. Pro Empfänger `open_uni()`, Header schreiben, `read_chunk` → `write_chunk` durchreichen, `finish()`. Wenn für einen Empfänger noch ein Frame desselben Senders offen ist, wird der ältere per `reset(RATE_LIMITED)` verworfen. Bitrate-Limit gleitend über 2 s.

`LossReport` → Maximum pro Sprecher über alle Empfänger (verfällt nach 6 s) → `LossHint` an den Sprecher, wenn sich der Wert ändert. `KeyframeRequest` → an den Sprecher mit `speaker = 0`, ≤ 1/s.

### 6.6 Limits (`limits.rs`)

Token-Buckets pro Client (Control 50/s, Audio 110/s, Video 20/s und 400 kbit/s) und pro IP (Enrollment 5/min, Handshakes 10/s). Überschreitung: Datagram/Frame verwerfen und zählen; bei Control `Error(RATE_LIMITED)`, dreimal in 60 s → `close(RATE_LIMITED)`.

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
| quinn | 0.11.12 | MIT/Apache-2.0 | QUIC |
| rustls | 0.23.45 (`ring`, `std`, ohne Default-Provider) | Apache-2.0/ISC/MIT | TLS |
| ring | 0.17.14 | Apache-2.0 AND ISC | Crypto |
| rcgen | 0.14.10 (`x509-parser`) | MIT/Apache-2.0 | CA, CSR |
| x509-parser | 0.16 | MIT/Apache-2.0 | Fingerprint/CN lesen |
| tokio | 1.53.1 | MIT | Runtime |
| postcard | 1.1.3 (`use-std`) | MIT/Apache-2.0 | Control-Serialisierung |
| serde | 1 | MIT/Apache-2.0 | |
| bytes | 1 | MIT | Datagram-Puffer |
| clap | 4 | MIT/Apache-2.0 | CLI |
| toml | 0.8 | MIT/Apache-2.0 | Config/State |
| tracing, tracing-subscriber | 0.1 / 0.3 | MIT | Logging |
| subtle | 2 | BSD-3 | Token-Vergleich |
| base64 | 0.22 | MIT/Apache-2.0 | Token/CA-Hash |
| sha2 | 0.10 | MIT/Apache-2.0 | Fingerprint (oder ring::digest) |

Kein libopus, kein Codec, keine GUI auf dem Server. `cargo-deny` mit Allowlist MIT, Apache-2.0, ISC, BSD-2/3, Zlib, Unicode.

## 10. Tests

| Ebene | Inhalt |
| --- | --- |
| Unit (`icu-proto`) | Roundtrip aller `ControlMsg`/`EnrollMsg`; Header-Encode/Decode; unbekannte Variante → `Err`; Framing-Grenzen (0, 16384, 16385). |
| Unit (`ca.rs`, `store.rs`) | CA-Erzeugung, CSR-Override (Spike 2b), Fingerprint-Stabilität, atomares Schreiben, Revoke-Persistenz. |
| Integration (in-process, `127.0.0.1:0`) | Enrollment → Connect → Hello/Welcome; Connect ohne Cert → `AUTH_REQUIRED`; unbekanntes Cert → `AUTH_UNKNOWN_CERT`; Revoke während Verbindung → `KICKED`; Relay: 3 Clients, Audio von A kommt bei B und C mit `speaker = A` an, nicht bei A; `deafened` C bekommt nichts; `muted` A wird nicht relayt; Rate-Limits; Video-Relay mit Frame-Verwerfen; `LossReport` → `LossHint`; `KeyframeRequest`-Weiterleitung; Hello-Timeout. |
| Last | 16 Clients × 50 Datagrams/s für 60 s: keine Verluste auf Loopback, CPU < 10 % eines Kerns, keine Latenzzunahme (Datagram-Roundtrip p99 < 1 ms). |
| Netz | `tc netem` 30 ms ± 10 ms, 2 % Verlust: Relay verliert nichts zusätzlich; Keepalive hält die Verbindung über 10 min Stille. |

## 11. Milestones

1. **M1 Skeleton**: Workspace, `icu-proto` mit allen Typen + Tests, Server startet, erzeugt CA, akzeptiert QUIC, `Hello/Welcome`.
2. **M2 Enrollment**: `invite`, `icu-enroll/1`, `users.toml`, `revoke`, Integrationstests Auth.
3. **M3 Audio-Relay**: Datagram-Relay mit `speaker`-Override, Mute/Deafen-Semantik, Speaking-Ableitung, Rate-Limits, Lasttest.
4. **M4 Video-Relay** und `LossReport/LossHint`, `KeyframeRequest`.
5. **M5 Betrieb**: systemd-Unit, Admin-Socket, `status`, musl-Build, `scratch`-Image, Checkmk-Local-Check.

Jeder Milestone endet mit grünen Tests und einem Eintrag in `../Research/verification.md`, wenn er einen offenen Messpunkt schließt.
