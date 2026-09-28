# Server: ursprüngliche Implementierungsentscheidungen

> Historischer Bericht: Ergebnisse gelten für den genannten Stand. Befehle in
> Codeblöcken bleiben als damaliger Nachweis erhalten. Für heutige Pfade und
> Befehle siehe [Verzeichnisübersicht](../README.md) und
> [Bauen](../development/building.md).


Historische Begründungen aus der Implementierungsplanung vom 2026-09-20.
Aktuelle Versionen und Features stehen in Cargo.toml und Cargo.lock; der
[Server-Vertrag](../reference/server.md) beschreibt das aktuelle Verhalten.

## 9. Abhängigkeiten (gepinnt, Lizenz aus `docs/research/packaging-licensing.md`)

| Crate | Version | Lizenz | Zweck |
| --- | --- | --- | --- |
| quinn | 0.11.12 (`default-features = false`, features `runtime-tokio`, `rustls-ring`, `log`) | MIT/Apache-2.0 | QUIC. Die Default-Features enthalten `platform-verifier` und ziehen rustls-platform-verifier mit — für den statischen musl-Build abschalten. |
| rustls | 0.23.45 (`ring`, `std`, ohne Default-Provider) | Apache-2.0/ISC/MIT | TLS |
| ring | 0.17.14 | Apache-2.0 AND ISC | Crypto; HMAC-SHA256 des Com-Tags (`icu-proto`) |
| subtle | 2.6.1 | BSD-3-Clause | Konstantzeit-Vergleich des gekürzten Com-Tags (`icu-proto`); schon über rustls im Baum |
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
| rand | 0.9 | MIT/Apache-2.0 | Platzhalternamen, Cert-Serial (16 B, 127 Zufallsbits) |
| time | 0.3 | MIT/Apache-2.0 | `OffsetDateTime` für rcgen-Validity, RFC-3339 im State |
| rustix | 1 | Apache-2.0/MIT | `flock` auf `state.lock`, Unix-Socket-Rechte |

Kein libopus, kein Codec, keine GUI auf dem Server.

Lizenz-Allowlist: **eine** `deny.toml` im Workspace-Root, gültig für Server und Client — MIT, Apache-2.0, ISC, BSD-2-Clause, BSD-3-Clause, Zlib, Unicode-3.0; `IJG` ausschließlich für `mozjpeg`/`mozjpeg-sys`, über die nokhwa (Client-Feature `camera`) Kamerabilder im MJPEG-Format dekodiert. Alles andere bricht den Build.

Lizenz von ICU selbst: **Apache-2.0** (entschieden 2026-09-24 mit der Veröffentlichung auf GitHub, Empfehlung aus `docs/research/packaging-licensing.md`); `license` steht in `[workspace.package]`, Text in `LICENSE`.
