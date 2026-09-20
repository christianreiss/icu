# Transport

## Frage

Welcher Transport trägt Control- und Audio-/Video-Daten zwischen ICU-Client und ICU-Server, so dass ein UDP-Port genügt, mutual TLS eingebaut ist, Audio unreliable und ohne Head-of-Line-Blocking läuft und die Rust-Implementierung mit einer gepflegten Lib auskommt? Konkret zu klären: unterstützt quinn Datagrams ohne Retransmit, wie groß darf ein Datagram sein, wie erzwingt man Client-Certs in rustls, wie liest man das Client-Cert nach dem Handshake aus der quinn-Connection.

## Kandidaten

1. **QUIC** (RFC 9000/9001) mit **unreliable Datagrams** (RFC 9221) für Audio und **Streams** für Control. Rust: `quinn` 0.11.12 + `rustls` 0.23.45; Alternative `s2n-quic` 1.88.0.
2. **DTLS/UDP für Voice + TLS/TCP für Control** (Mumble-Modell: TCP+TLS Control, UDP mit eigener Symmetric-Crypto für Voice, siehe `prior-art.md`).
3. **Reines UDP mit eigener Crypto** (eigener Handshake, eigenes AEAD-Framing, eigener Keepalive/NAT-Code).

## Primärquellen

Alle abgerufen am 2026-09-20.

- RFC 9000 QUIC: A UDP-Based Multiplexed and Secure Transport — https://www.rfc-editor.org/rfc/rfc9000.html
- RFC 9001 Using TLS to Secure QUIC — https://www.rfc-editor.org/rfc/rfc9001.html
- RFC 9221 An Unreliable Datagram Extension to QUIC — https://www.rfc-editor.org/rfc/rfc9221.html
- RFC 9308 Applicability of the QUIC Transport Protocol — https://www.rfc-editor.org/rfc/rfc9308.html
- RFC 8446 TLS 1.3 — https://www.rfc-editor.org/rfc/rfc8446.html
- RFC 9147 DTLS 1.3 — https://www.rfc-editor.org/rfc/rfc9147.html
- RFC 4787 NAT Behavioral Requirements for Unicast UDP — https://www.rfc-editor.org/rfc/rfc4787.html
- RFC 8085 UDP Usage Guidelines — https://www.rfc-editor.org/rfc/rfc8085.html
- crates.io API quinn — https://crates.io/api/v1/crates/quinn
- crates.io API rustls — https://crates.io/api/v1/crates/rustls
- crates.io API s2n-quic — https://crates.io/api/v1/crates/s2n-quic
- docs.rs quinn (Index) — https://docs.rs/quinn/latest/quinn/index.html
- docs.rs quinn::Connection — https://docs.rs/quinn/latest/quinn/struct.Connection.html
- docs.rs quinn::Connecting — https://docs.rs/quinn/latest/quinn/struct.Connecting.html
- docs.rs quinn::Incoming — https://docs.rs/quinn/latest/quinn/struct.Incoming.html
- docs.rs quinn::Endpoint — https://docs.rs/quinn/latest/quinn/struct.Endpoint.html
- docs.rs quinn::ServerConfig — https://docs.rs/quinn/latest/quinn/struct.ServerConfig.html
- docs.rs quinn::ClientConfig — https://docs.rs/quinn/latest/quinn/struct.ClientConfig.html
- docs.rs quinn::SendStream — https://docs.rs/quinn/latest/quinn/struct.SendStream.html
- docs.rs quinn::RecvStream — https://docs.rs/quinn/latest/quinn/struct.RecvStream.html
- docs.rs quinn::crypto::rustls::QuicServerConfig — https://docs.rs/quinn/latest/quinn/crypto/rustls/struct.QuicServerConfig.html
- docs.rs quinn::crypto::rustls::HandshakeData — https://docs.rs/quinn/latest/quinn/crypto/rustls/struct.HandshakeData.html
- docs.rs quinn_proto::TransportConfig — https://docs.rs/quinn-proto/latest/quinn_proto/struct.TransportConfig.html
- docs.rs quinn_proto::SendDatagramError — https://docs.rs/quinn-proto/latest/quinn_proto/enum.SendDatagramError.html
- quinn Repo, `quinn/Cargo.toml` (Feature-Flags) — https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn/Cargo.toml
- quinn Repo, `quinn-proto/src/config/transport.rs` (Defaults) — https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn-proto/src/config/transport.rs
- quinn Repo, `quinn-proto/src/config/mod.rs` (ServerConfig-Defaults) — https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn-proto/src/config/mod.rs
- quinn Repo, `quinn-proto/src/connection/datagrams.rs` (Datagram-Größe, Drop-Logik) — https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn-proto/src/connection/datagrams.rs
- docs.rs rustls (Index, Features) — https://docs.rs/rustls/latest/rustls/index.html
- docs.rs rustls::ConfigBuilder — https://docs.rs/rustls/latest/rustls/struct.ConfigBuilder.html
- docs.rs rustls::server::ServerConfig — https://docs.rs/rustls/latest/rustls/server/struct.ServerConfig.html
- docs.rs rustls::server::ServerConnection — https://docs.rs/rustls/latest/rustls/server/struct.ServerConnection.html
- docs.rs rustls::server::WebPkiClientVerifier — https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html
- docs.rs rustls::server::danger::ClientCertVerifier — https://docs.rs/rustls/latest/rustls/server/danger/trait.ClientCertVerifier.html
- docs.rs rustls::server::danger::ClientCertVerified — https://docs.rs/rustls/latest/rustls/server/danger/struct.ClientCertVerified.html
- docs.rs rustls::client::ClientConfig — https://docs.rs/rustls/latest/rustls/client/struct.ClientConfig.html
- s2n-quic README — https://raw.githubusercontent.com/aws/s2n-quic/main/README.md
- s2n-quic `quic/s2n-quic/Cargo.toml` — https://raw.githubusercontent.com/aws/s2n-quic/main/quic/s2n-quic/Cargo.toml
- s2n-quic `quic/s2n-quic-tls/src/server.rs` — https://raw.githubusercontent.com/aws/s2n-quic/main/quic/s2n-quic-tls/src/server.rs
- docs.rs s2n_quic::provider — https://docs.rs/s2n-quic/latest/s2n_quic/provider/index.html

## Befunde

### QUIC-Grundlagen (RFC 9000/9001)

- Ein UDP-Socket trägt Streams und Datagrams gleichzeitig; Streams sind "a lightweight, ordered byte-stream abstraction", bidirektional oder unidirektional; "QUIC does not provide any means of ensuring ordering between bytes on different streams" (RFC 9000 §2, §2.3) — Head-of-Line-Blocking ist auf einen Stream begrenzt.
- TLS 1.3 ist Pflicht: "Clients MUST NOT offer TLS versions older than 1.3 … An endpoint MUST terminate the connection if a version of TLS older than 1.3 is negotiated" (RFC 9001 §4.2).
- Client-Auth: "A server MAY request that the client authenticate during the handshake. A server MAY refuse a connection if the client is unable to authenticate when requested." Und: "A server MUST NOT use post-handshake client authentication" (RFC 9001 §4.4). Folge: eine unauthentifizierte Verbindung kann nicht nachträglich zu einer mTLS-Verbindung aufgewertet werden (relevant für `auth-enrollment.md`).
- ALPN ist Pflicht: "Endpoints MUST use ALPN"; ohne Match "the connection MUST be closed with a no_application_protocol error code" (RFC 9001 §8.1). Damit ist ALPN das harte Versions-Gate für das ICU-Protokoll.
- AEAD-Overhead: die meisten Cipher Suites erzeugen ein 16-Byte-Tag (RFC 9001 §5.1). Short-Header-Pakete: 1 Byte First Byte, 0–20 Byte Destination Connection ID, 1–4 Byte Packet Number (RFC 9000 §17.3).
- Datagram-Größe: "QUIC MUST NOT be used if the network path cannot support a maximum datagram size of at least 1200 bytes" (RFC 9000 §14.1); `max_udp_payload_size` Default 65527; PMTUD (§14.2) und DPLPMTUD (§14.3); IP-Fragmentierung ist zu vermeiden (DF-Bit).
- Connection Migration: "Only clients are able to migrate in this version of QUIC" (RFC 9000 §1). Connection IDs entkoppeln die Verbindung von der Adresse (§5.1); Pfadvalidierung bei Migration (§9.3); neue CIDs pro Pfad aus Privacy-Gründen (§9.5). RFC 9308 §3.2 relativiert: Migration nach NAT-Rebinding funktioniert nur, wenn ein Endpunkt an seiner Adresse erreichbar bleibt und der Peer als erster nach dem Timeout sendet — genau die ICU-Situation (Server mit fester Adresse, Client sendet).
- Idle-Timeout: effektiv "the lesser of its advertised idle timeout" und dem des Peers (RFC 9000 §10.1); PING-Frames als Keepalive (§5.3, §19.2); "endpoints SHOULD NOT send keepalives more often than necessary" (§10.1.2).
- 0-RTT: "0-RTT provides no protection against replay attacks" (RFC 9000 §5, RFC 9001 §9.2); "A client therefore MUST NOT use 0-RTT for application data unless specifically requested by the application" (RFC 9001 §4.6.3/§5.6). RFC 8446 §2.3: 0-RTT-Daten sind zudem nicht forward-secret.
- Application Error Codes in CONNECTION_CLOSE (Typ 0x1d) sind Variable-Length-Integer, 62 Bit (RFC 9000 §19.19, §16).

### Unreliable Datagrams (RFC 9221)

- DATAGRAM-Frames werden nicht retransmittiert: "DATAGRAM frames employ the QUIC connection's congestion controller", sind ack-eliciting, aber ohne Retransmit; Verlust kann dem Sender gemeldet werden (§5, §5.2).
- Aktivierung über Transport-Parameter `max_datagram_frame_size`; Default 0 = kein Support; Empfehlung 65535 (§3). "An endpoint MUST NOT send DATAGRAM frames until it has received the max_datagram_frame_size transport parameter with a non-zero value."
- "DATAGRAM frames cannot be fragmented" (§5.4) — Größe ist durch Path MTU begrenzt.
- Keine Flow-Control: "DATAGRAM frames do not provide any explicit flow control signaling" (§5.3); Empfänger dürfen droppen. Congestion Control gilt trotzdem (§5.4) — Audio-Datagrams und Video-Streams teilen sich denselben Congestion Controller.
- Demultiplexing ist Sache der Anwendung: "Defining the identifiers used to multiplex different kinds of datagrams … is the responsibility of the application protocol" (§5.1) — Folge: ICU braucht ein Typ-Byte am Datagram-Anfang (siehe `protocol/wire.md`).
- Frame-Typen 0x30/0x31 (mit/ohne Length-Feld) (§4). Datagrams "MUST be protected either by 0-RTT or 1-RTT keys" (§6).

### quinn 0.11.12 (crates.io: 0.11.12, aktualisiert 2026-09-14)

- Feature-Flags laut `quinn/Cargo.toml`: Default = `tracing-log`, `platform-verifier`, `runtime-tokio`, `rustls-ring`, `bloom`. Weitere: `rustls-aws-lc-rs`, `rustls-aws-lc-rs-fips`, `aws-lc-rs`, `ring`, `runtime-smol`, `lock_tracking`, `qlog`, `rustls-log`. docs.rs-Index nennt zusätzlich async-std-Runtime-Support; im aktuellen Cargo.toml ist nur `runtime-tokio`/`runtime-smol` aufgeführt (Drift zwischen Index-Text und Manifest, für ICU irrelevant: Tokio). Abhängigkeiten: tokio ^1.28.1, rustls ^0.23.5, quinn-proto ^0.11.18.
- **Datagrams ohne Retransmit: ja.** `Connection::send_datagram(&self, data: Bytes) -> Result<(), SendDatagramError>`: "Transmit `data` as an unreliable, unordered application datagram. Application datagrams may be lost or delivered out of order, and data must both fit inside a single QUIC packet and be smaller than the peer's maximum." `send_datagram_wait` wartet bei Congestion auf Pufferplatz und "effectively prioritizing old datagrams over new ones". `read_datagram` empfängt. (docs.rs Connection)
- Drop-Semantik: `datagram_send_buffer_space()` — "When greater than zero, calling `send_datagram()` with a datagram of at most this size is guaranteed not to cause older datagrams to be dropped." Quelle `datagrams.rs`: bei `drop = true` werden "previously queued datagrams which are still unsent … discarded … in order of oldest to newest"; bei vollem Empfangspuffer "dropping stale datagram". Für Voice ist das gewünschte Verhalten (alt wegwerfen, neu senden) also `send_datagram`, nicht `send_datagram_wait`.
- `SendDatagramError`-Varianten: `UnsupportedByPeer`, `Disabled`, `TooLarge` ("the path MTU minus overhead or the limit advertised by the peer has been exceeded"), `Blocked(Bytes)`.
- **Datagram-Größe:** `Connection::max_datagram_size() -> Option<usize>`: "Compute the maximum size of datagrams that may be passed to `send_datagram()`. Returns None if datagrams are unsupported by the peer or disabled locally." Herleitung in `datagrams.rs`: min(`path.current_mtu()` − `predict_1rtt_overhead` − `Datagram::SIZE_BOUND`, `peer_params.max_datagram_frame_size`); "This may change over the lifetime of a connection according to variation in the path MTU estimate." Defaults (`transport.rs`): `initial_mtu` = `min_mtu` = 1200, MTU-Discovery aktiv (`Some(MtuDiscoveryConfig::default())`). Abgeleitetes konservatives Budget vor MTU-Discovery: 1200 − (1 + CID + 1..4) − 16 (AEAD-Tag) − Datagram-Frame-Header ≈ 1150–1170 Byte; exakter Wert nur per `max_datagram_size()` zur Laufzeit (Step 3). Opus-Sprachframes (20 ms, 24–64 kbit/s ⇒ 60–160 Byte) passen mit großem Abstand; Video-Frames nicht.
- Datagram-Puffer-Defaults: `datagram_receive_buffer_size` = `Some(1_250_000)`, `datagram_send_buffer_size` = 1 MiB; `None` schaltet Empfang aus (`transport.rs`, TransportConfig-Doku).
- Idle/Keepalive: `max_idle_timeout` Default `Some(30_000 ms)` ("30 second default recommended by RFC 9308 § 3.2"); "The true idle timeout is the minimum of this and the peer's own max idle timeout." `keep_alive_interval` Default `None`; "Must be set lower than the idle_timeout of both peers to be effective." (`transport.rs`)
- Migration: `ServerConfig::migration(bool)` — "Whether to allow clients to migrate to new addresses. Improves behavior for clients that move between different internet connections or suffer NAT rebinding." Default `true` (`config/mod.rs`). `Connection::remote_address()`: "If ServerConfig::migration is true, clients may change addresses at will."
- 0-RTT: `Connecting::into_0rtt()` — "this enables transmission of 0-RTT data, which is vulnerable to replay attacks, and should therefore never invoke non-idempotent operations"; auf Server-Seite werden 0.5-RTT-Daten vor Abschluss der Client-Auth gesendet. `QuicServerConfig`: "If `max_early_data_size` is not set to `u32::MAX`, the server will not be able to accept incoming 0-RTT data. QUIC prohibits `max_early_data_size` values other than 0 or `u32::MAX`." Client: `enable_early_data` Default `false` (rustls ClientConfig).
- Adressvalidierung/DoS: `Incoming::retry()` — "Respond with a retry packet, requiring the client to retry with address validation"; `remote_address_validated()`; `retry_token_lifetime` Default 15 s; `max_incoming` Default 65536; `incoming_buffer_size` 10 MiB (ServerConfig-Doku, `config/mod.rs`).
- Streams: `open_uni`/`open_bi`/`accept_uni`/`accept_bi`; "The Connection that calls `open_bi()` must write to its SendStream before the other Connection is able to `accept_bi()`." `SendStream::set_priority(i32)`: "Locally buffered data from streams with higher priority will be transmitted before data from streams with lower priority." `RecvStream::read_to_end(size_limit)` schlägt mit `TooLong` fehl. Defaults: 100 bidi / 100 uni Streams, `stream_receive_window` 1_250_000, `send_window` 10_000_000 (`transport.rs`).
- **Client-Cert auslesen:** `Connection::peer_identity() -> Option<Box<dyn Any>>` — "Cryptographic identity of the peer. For the default rustls session, the return value can be downcast to a `Vec<rustls::pki_types::CertificateDer>`." Reihenfolge laut rustls `peer_certificates`: "the first certificate relates to the peer, the second certifies the first …". `handshake_data()` → `HandshakeData { protocol: Option<Vec<u8>> /* ALPN */, server_name: Option<String> }`.
- Endpoint/Plattform: `Endpoint::server(config, addr)`; Hinweis: "platform defaults for dual-stack sockets vary", IPv6-Wildcard-Sockets auf Windows sprechen per Default kein IPv4; portable Anwendungen sollen explizit die Adressfamilie binden (docs.rs Endpoint). Für den Client: `ClientConfig::with_root_certificates(Arc<RootCertStore>)`; `Endpoint::connect(addr, server_name)` — `server_name` muss zum Server-Cert passen.
- `ServerConfig.crypto`: "TLS configuration used for incoming connections. Must be set to use TLS 1.3 only."

### rustls 0.23.45 (crates.io: 0.23.45, aktualisiert 2026-09-14)

- Features (docs.rs Index): Default `aws_lc_rs`, `std`, `logging`, `prefer-post-quantum` (X25519MLKEM768 bevorzugt), `tls12`. Optional `ring`, `fips`, `custom-provider`, `brotli`, `zlib`. MSRV 1.71. Beide Provider (aws-lc-rs, ring) sind offiziell; quinn wählt per Feature `rustls-ring` (Default) oder `rustls-aws-lc-rs`.
- **Client-Cert-Pflicht konfigurieren:** `ServerConfig::builder().with_client_cert_verifier(client_cert_verifier: Arc<dyn ClientCertVerifier>) -> ConfigBuilder<ServerConfig, WantsServerCert>` ("Choose how to verify client certificates."), danach `.with_single_cert(cert_chain: Vec<CertificateDer<'static>>, key_der: PrivateKeyDer<'static>) -> Result<ServerConfig, Error>`. Verifier: `WebPkiClientVerifier::builder(roots: Arc<RootCertStore>)` — "Client certificate authentication will be offered by the server, and client certificates will be verified using the trust anchors found in the provided `roots`." Builder-Optionen: `allow_unauthenticated()` (Client-Auth optional), `with_crls(..)`, `only_check_end_entity_revocation()`, `allow_unknown_revocation_status()`, `build() -> Arc<dyn ClientCertVerifier>`. Ohne `allow_unauthenticated()` ist Client-Auth Pflicht: Trait-Default `client_auth_mandatory()` = Wert von `offer_client_auth()` = `true`.
- Eigener Verifier: Trait `ClientCertVerifier` (Pflicht: `root_hint_subjects`, `verify_client_cert`, `verify_tls12_signature`, `verify_tls13_signature`, `supported_verify_schemes`; optional `offer_client_auth`, `client_auth_mandatory`, `requires_raw_public_keys`). `ClientCertVerified::assertion()` ist der Zero-Sized-Marker. Damit ließe sich eine Fingerprint-Prüfung direkt im TLS-Handshake einhängen.
- `ServerConfig.max_early_data_size`: "Specify 0 to disable early data."; `alpn_protocols`: "If empty we don't do ALPN at all." (für QUIC muss ALPN gesetzt sein, RFC 9001 §8.1).
- `ServerConnection::peer_certificates(&self) -> Option<&[CertificateDer<'static>]>` (bei quinn über `peer_identity()` erreichbar, s. o.).

### s2n-quic 1.88.0 (crates.io: 1.88.0, aktualisiert 2026-08-21)

- README: Linux, macOS, Windows; "s2n-quic requires Linux kernel version 5.0 or later" (GSO); TLS-Provider s2n-tls (Unix, Windows-GNU) oder rustls (Windows-MSVC); MSRV 1.92.0.
- Unreliable Datagrams nur hinter `unstable-provider-datagram` (Cargo.toml `[features]`); der docs.rs-Provider-Index listet kein `datagram`-Modul (Feature nicht in der Docs-Build aktiv).
- mTLS vorhanden: `Builder::with_client_authentication()` — "Configures this server instance to require client authentication (mutual TLS)."; `with_trusted_certificate(..)` (`s2n-quic-tls/src/server.rs`).

### DTLS 1.3 / eigenes UDP

- RFC 9147: DTLS nimmt TLS 1.3 auf Datagramme, muss aber Handshake-Zuverlässigkeit selbst nachbauen (Fragment-Offset/-Length, Retransmit-Timer "initial timer value of 1000 ms", Cookie-Exchange gegen Amplification, Replay-Window). Connection ID (RFC 9146) für NAT-Rebinding. "The DTLS record framing expands the datagram size, thus lowering the effective PMTU." Kein Stream-Multiplexing, keine Congestion Control — Control bräuchte einen zweiten Kanal (TCP+TLS), also zwei Ports/zwei Handshakes/zwei Cert-Prüfungen.
- RFC 8085: UDP-Anwendungen "must employ mechanisms to prevent congestion collapse" (§3.1); Keepalives "SHOULD NOT transmit them more frequently than once every 15 seconds" (§3.5); keine IP-Fragmentierung (§3.2). Eigene Crypto hieße: Handshake, Key-Schedule, AEAD-Nonce-Management, Replay-Schutz, Anti-Amplification und Congestion Control selbst spezifizieren und implementieren.

### NAT

- RFC 4787 REQ-5: "A NAT UDP mapping timer MUST NOT expire in less than two minutes"; "A default value of five minutes or more … is RECOMMENDED" (§4.3). REQ-1: Endpoint-Independent Mapping (§4.1). Gegenposition RFC 9308 §3.2: praktisch kann UDP-State "after just thirty seconds of inactivity" verfallen; Keepalive-Minimum 15 s (RFC 8085). Für PTT-Nutzer mit langen Stille-Phasen ist ein aktiver Keepalive nötig.

## Entscheidung

**QUIC über quinn 0.11.12 + rustls 0.23.45, ein UDP-Port, Audio als RFC-9221-Datagrams, Control auf einem bidirektionalen Stream, Video auf unidirektionalen Streams.**

Begründung:
- Alle vier Prüfpunkte sind belegt: quinn sendet Datagrams ohne Retransmit (`send_datagram`, RFC 9221), die Größe ergibt sich aus Path-MTU minus Overhead (Runtime-Wert `max_datagram_size()`, konservativ ≈ 1150 Byte bei 1200-Byte-MTU), Client-Cert-Pflicht ist `with_client_cert_verifier(WebPkiClientVerifier::builder(roots).build())`, das Client-Cert liest man über `Connection::peer_identity()` → `Vec<CertificateDer>` (Leaf zuerst).
- TLS 1.3 und mTLS sind Teil des Transports (RFC 9001 §4.2/§4.4), Congestion Control, Pfadvalidierung, Anti-Amplification (`Incoming::retry`) und Migration bei NAT-Rebinding kommen mit.
- Ein Port, ein Handshake, eine Identität für Control und Audio — der Mumble-Ansatz braucht zwei Kanäle und eine eigene Voice-Crypto.

Konfigurationsfestlegungen (aus den Befunden abgeleitet):
- Provider: **`rustls-ring`** (Step 3: cross-compiliert nach Windows-gnu ohne Zusatzaufwand, Spike 1). aws-lc-rs bräuchte cmake+nasm im mingw-Build und bringt für ICU nichts (kein RSA, kein FIPS nötig).
- `max_early_data_size = 0`, kein `into_0rtt()`: Voice-Reconnect mit 1-RTT ist ausreichend, Replay-Risiko entfällt.
- ALPN `icu/1` als hartes Versions-Gate (RFC 9001 §8.1).
- `keep_alive_interval` clientseitig 15 s (RFC 8085-Minimum), `max_idle_timeout` beidseitig 60 s (> Keepalive, < RFC-4787-Minimum von 2 min ist unerheblich, da Keepalive das Mapping hält).
- `migration = true` (quinn-Default) belassen.
- `datagram_receive_buffer_size` klein halten (Ziel: wenige Frames, damit "dropping stale datagram" statt Latenzaufbau) — Wert in Step 3 messen.
- Server bindet explizit `0.0.0.0` und `[::]` getrennt oder nur eine Familie (quinn-Dual-Stack-Hinweis).

## Verworfen, mit Grund

- **DTLS/UDP + TLS/TCP (Mumble-Modell):** zwei Sockets, zwei Handshakes, zwei Cert-Prüfungen, Fallback-Logik UDP→TCP-Tunnel; DTLS bringt kein Multiplexing und keine Congestion Control (RFC 9147/8085). Der einzige Vorteil (Reife der TCP-Control-Strecke) wiegt die doppelte Komplexität nicht auf.
- **Reines UDP mit eigener Crypto:** Handshake, Replay-Schutz, Anti-Amplification, Keepalive, Congestion Control müssten selbst spezifiziert und auditiert werden (RFC 8085 §3.1 verlangt Congestion Control explizit). Kein Sicherheits- oder Latenzgewinn gegenüber QUIC-Datagrams, die ebenfalls ohne Retransmit laufen.
- **s2n-quic:** Datagrams nur hinter `unstable-provider-datagram`; Linux-Kernel ≥ 5.0 Pflicht; TLS-Provider hängt auf Windows vom Toolchain ab; MSRV 1.92. quinn deckt denselben Bedarf mit stabiler Datagram-API ab.
- **0-RTT:** Replay ohne Nutzen für ICU (RFC 9001 §9.2, quinn-Warnung).
- **Eigener `ClientCertVerifier` für Fingerprint-Prüfung im Handshake:** möglich, aber unnötig — `WebPkiClientVerifier` prüft die Kette gegen die ICU-CA, die Fingerprint-Allowlist prüft die Anwendung nach `peer_identity()` (siehe `auth-enrollment.md`). Bleibt als Option, falls Step 3 zeigt, dass abgelehnte Clients besser schon im Handshake scheitern sollen.

## Offene Punkte für Step 3

- `max_datagram_size()` real messen: direkt nach Handshake (1200-MTU-Basis) und nach MTU-Discovery, über Linux→Linux und Windows→Linux; prüfen, ob `TooLarge` beim Relay an Peers mit kleinerer MTU auftritt.
- Verhalten von `send_datagram` unter Congestion: werden wirklich die ältesten Frames verworfen, und wie groß muss `datagram_send_buffer_size`/`datagram_receive_buffer_size` sein, damit keine Latenz aufläuft?
- Priorisierung: bevorzugt quinn beim Packen ausgehender Pakete DATAGRAM-Frames gegenüber Stream-Daten (Video)? Unbelegt in der Doku; messen (Audio-Jitter bei laufendem Video-Stream).
- Shared Congestion Controller: Video-Streams drosseln Audio-Datagrams? Messen mit künstlichem Paketverlust.
- Idle/Keepalive: bestätigen, dass 15 s Keepalive NAT-Mappings über typische Consumer-Router hält; Reconnect-Zeit nach Rebinding messen (Migration).
- `peer_identity()` bei Verbindungen mit `allow_unauthenticated()` und ohne Client-Cert: `None` oder leerer Vec? (relevant für Enrollment-Verbindung).
- Handshake-Größe/-Dauer mit `prefer-post-quantum` (rustls-Default) vs. ohne; Initial-Paket-Anzahl bei Client-Cert + CA-Chain.
- Windows: Dual-Stack-Binding, quinn `rustls-aws-lc-rs` Build mit MSVC-Toolchain (aws-lc-rs benötigt C-Build).
- Retry/Adressvalidierung: Latenzkosten von `Incoming::retry()` (ein zusätzlicher RTT) messen; Entscheidung, ob nur bei Last aktivieren.

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `verification.md`):
  - Spike 2 **bestätigt**: quinn 0.11.12 + rustls 0.23.45 (ring), Client-Cert-Pflicht via `WebPkiClientVerifier`, `peer_identity()` → `Vec<CertificateDer>` (Leaf), `max_datagram_size()` 1162 B nach Handshake, 1414 B nach MTU-Discovery (Loopback), 1415 B → `TooLarge`; Datagram-RTT p50 176 µs.
  - Spike 2 **Falle**: `connect().await` liefert auf Client-Seite `Ok`, bevor der Server das fehlende Client-Cert ablehnt; die Verbindung wird 0,2 ms später mit TLS-Alert 116 geschlossen. Protokollfolge: Client gilt erst nach `Welcome` als verbunden.
  - Spike 2b **bestätigt**: ALPN vor Handshake-Ende lesbar (`Connecting::handshake_data()`); `peer_identity()` ohne Cert = `None`.
  - Spike 1 **entscheidet** den Provider: `rustls-ring` (cross-compiliert nach `x86_64-pc-windows-gnu` ohne Zusatzaufwand); aws-lc-rs nicht getestet und nicht nötig.
  - Offen: Verhalten über echtes Netz/NAT, Keepalive, Datagram-vs-Stream-Priorisierung, quinn auf echtem Windows (unter Wine scheitert `Endpoint::client` an WSAENOPROTOOPT, Wine-Defizit).

Mit WebFetch gelesen (2026-09-20):
- https://www.rfc-editor.org/rfc/rfc9000.html
- https://www.rfc-editor.org/rfc/rfc9001.html
- https://www.rfc-editor.org/rfc/rfc9221.html
- https://www.rfc-editor.org/rfc/rfc9308.html
- https://www.rfc-editor.org/rfc/rfc8446.html
- https://www.rfc-editor.org/rfc/rfc9147.html
- https://www.rfc-editor.org/rfc/rfc4787.html
- https://www.rfc-editor.org/rfc/rfc8085.html
- https://crates.io/api/v1/crates/quinn
- https://crates.io/api/v1/crates/rustls
- https://crates.io/api/v1/crates/s2n-quic
- https://docs.rs/quinn/latest/quinn/index.html
- https://docs.rs/quinn/latest/quinn/struct.Connection.html
- https://docs.rs/quinn/latest/quinn/struct.Connecting.html
- https://docs.rs/quinn/latest/quinn/struct.Incoming.html
- https://docs.rs/quinn/latest/quinn/struct.Endpoint.html
- https://docs.rs/quinn/latest/quinn/struct.ServerConfig.html
- https://docs.rs/quinn/latest/quinn/struct.ClientConfig.html
- https://docs.rs/quinn/latest/quinn/struct.SendStream.html
- https://docs.rs/quinn/latest/quinn/struct.RecvStream.html
- https://docs.rs/quinn/latest/quinn/crypto/rustls/struct.QuicServerConfig.html
- https://docs.rs/quinn/latest/quinn/crypto/rustls/struct.HandshakeData.html
- https://docs.rs/quinn-proto/latest/quinn_proto/struct.TransportConfig.html
- https://docs.rs/quinn-proto/latest/quinn_proto/enum.SendDatagramError.html
- https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn/Cargo.toml
- https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn-proto/src/config/transport.rs
- https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn-proto/src/config/mod.rs
- https://raw.githubusercontent.com/quinn-rs/quinn/main/quinn-proto/src/connection/datagrams.rs
- https://docs.rs/rustls/latest/rustls/index.html
- https://docs.rs/rustls/latest/rustls/struct.ConfigBuilder.html
- https://docs.rs/rustls/latest/rustls/server/struct.ServerConfig.html
- https://docs.rs/rustls/latest/rustls/server/struct.ServerConnection.html
- https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html
- https://docs.rs/rustls/latest/rustls/server/danger/trait.ClientCertVerifier.html
- https://docs.rs/rustls/latest/rustls/server/danger/struct.ClientCertVerified.html
- https://docs.rs/rustls/latest/rustls/client/struct.ClientConfig.html
- https://raw.githubusercontent.com/aws/s2n-quic/main/README.md
- https://raw.githubusercontent.com/aws/s2n-quic/main/quic/s2n-quic/Cargo.toml
- https://raw.githubusercontent.com/aws/s2n-quic/main/quic/s2n-quic-tls/src/server.rs
- https://docs.rs/s2n-quic/latest/s2n_quic/provider/index.html
