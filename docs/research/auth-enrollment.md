# Auth und Enrollment

## Frage

Wie bekommt ein neuer User ein Client-Zertifikat der ICU-CA, ohne dass er Dateien hin- und herkopiert oder OpenSSL bedient? Dazu: wie wird ein Zertifikat widerrufen, wie lange gilt es, wo liegt der private Key auf dem Client, wie erzeugt der Server seine CA (kann `rcgen` CA + Leaf + CSR-Signing?), wie sieht der Invite-Link aus, wie wird Token-Bruteforce verhindert.

## Kandidaten

- **(a) Invite-Token + CSR:** Admin erzeugt Invite-Token; Client erzeugt lokal ein Keypair, verbindet sich unauthentifiziert per QUIC zum Server, schickt Token + CSR, Server signiert mit eigener CA, speichert den Cert-Fingerprint in der Allowlist, gibt Leaf-Cert + CA-Cert zurück.
- **(b) PKCS#12-Export:** Admin erzeugt Key + Cert auf dem Server, exportiert ein passwortgeschütztes `.p12`, User importiert es im Client.
- **(c) TOFU mit Admin-Freigabe:** Client erzeugt selbstsigniertes Cert, verbindet sich, Server merkt sich den Fingerprint als "pending", Admin gibt frei.

## Primärquellen

Alle abgerufen am 2026-09-20.

- RFC 9001 Using TLS to Secure QUIC (§4.4 Peer Authentication) — https://www.rfc-editor.org/rfc/rfc9001.html
- RFC 8446 TLS 1.3 (§4.3.2 CertificateRequest, §4.6.2 Post-Handshake Auth) — https://www.rfc-editor.org/rfc/rfc8446.html
- RFC 5280 X.509 PKI Certificate and CRL Profile — https://www.rfc-editor.org/rfc/rfc5280.html
- RFC 7250 Raw Public Keys in TLS — https://www.rfc-editor.org/rfc/rfc7250.html
- RFC 7292 PKCS #12 — https://www.rfc-editor.org/rfc/rfc7292.html
- RFC 7595 Guidelines and Registration Procedures for URI Schemes — https://www.rfc-editor.org/rfc/rfc7595.html
- crates.io API rcgen — https://crates.io/api/v1/crates/rcgen
- crates.io API keyring — https://crates.io/api/v1/crates/keyring
- docs.rs rcgen (Index) — https://docs.rs/rcgen/latest/rcgen/index.html
- docs.rs rcgen::CertificateParams — https://docs.rs/rcgen/latest/rcgen/struct.CertificateParams.html
- docs.rs rcgen::CertificateSigningRequestParams — https://docs.rs/rcgen/latest/rcgen/struct.CertificateSigningRequestParams.html
- docs.rs rcgen::Issuer — https://docs.rs/rcgen/latest/rcgen/struct.Issuer.html
- docs.rs rcgen::KeyPair — https://docs.rs/rcgen/latest/rcgen/struct.KeyPair.html
- docs.rs rcgen::ExtendedKeyUsagePurpose — https://docs.rs/rcgen/latest/rcgen/enum.ExtendedKeyUsagePurpose.html
- docs.rs keyring (Index) — https://docs.rs/keyring/latest/keyring/index.html
- docs.rs rustls::server::WebPkiClientVerifier — https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html
- docs.rs rustls::server::danger::ClientCertVerifier — https://docs.rs/rustls/latest/rustls/server/danger/trait.ClientCertVerifier.html
- docs.rs rustls::client::ClientConfig — https://docs.rs/rustls/latest/rustls/client/struct.ClientConfig.html
- docs.rs rustls::ConfigBuilder — https://docs.rs/rustls/latest/rustls/struct.ConfigBuilder.html
- docs.rs quinn::Connection (peer_identity) — https://docs.rs/quinn/latest/quinn/struct.Connection.html
- docs.rs quinn::Incoming (retry, remote_address_validated) — https://docs.rs/quinn/latest/quinn/struct.Incoming.html
- docs.rs quinn::crypto::rustls::HandshakeData (ALPN) — https://docs.rs/quinn/latest/quinn/crypto/rustls/struct.HandshakeData.html
- Microsoft Learn, CryptProtectData (DPAPI) — https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata
- Microsoft Learn (Archiv), Registering an Application to a URI Scheme — https://learn.microsoft.com/en-us/previous-versions/windows/internet-explorer/ie-developer/platform-apis/aa767914(v=vs.85)
- freedesktop.org Desktop Entry Spec, Exec-Variablen — http://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
- Mumble Server-Konfiguration `mumble-server.ini` (Vergleich `certrequired`) — https://raw.githubusercontent.com/mumble-voip/mumble/master/auxiliary_files/mumble-server.ini
- Mumble User-Doku Certificates (Vergleich) — https://www.mumble.info/documentation/user/certificates/

## Befunde

### Transport-Randbedingungen

- "A server MUST NOT use post-handshake client authentication" (RFC 9001 §4.4). Eine ohne Client-Cert aufgebaute QUIC-Verbindung kann also nicht nachträglich authentifiziert werden; die Enrollment-Verbindung ist zwingend eine eigene Verbindung, die nach dem Signieren geschlossen wird.
- Client-Auth ist im QUIC-Handshake optional ("A server MAY request …", "MAY refuse a connection if the client is unable to authenticate", RFC 9001 §4.4). rustls: `WebPkiClientVerifier::builder(roots)…allow_unauthenticated()` macht Client-Auth optional; ohne diesen Aufruf Pflicht (`client_auth_mandatory()` Default = `offer_client_auth()` = true).
- ALPN ist pro Verbindung sichtbar: `HandshakeData.protocol` — "The negotiated application protocol, if ALPN is in use. Guaranteed to be set if a nonempty list of protocols was specified" (quinn). Damit können `icu-enroll/1` und `icu/1` auf einem Port unterschieden werden.
- Client-Cert nach Handshake: `Connection::peer_identity()` → `Vec<rustls::pki_types::CertificateDer>`, Leaf zuerst (siehe `transport.md`).
- Adressvalidierung: `Incoming::retry()` erzwingt Retry-Token; `remote_address_validated()` zeigt, ob der Absender beweisbar an seiner Adresse erreichbar ist (quinn Incoming). Retry-Token-Lebensdauer Default 15 s.
- Raw Public Keys (RFC 7250) wären eine Alternative zu X.509, "only provide[] authentication when an out-of-band mechanism is also used to bind the public key to the entity" (§1) — genau das leistet die Fingerprint-Allowlist. rustls kennt `requires_raw_public_keys()` im Verifier-Trait. Für ICU nicht nötig, da rcgen X.509 ohne Mehraufwand liefert.

### rcgen 0.14.10 (crates.io: 0.14.10, aktualisiert 2026-08-28)

- **CA:** `CertificateParams` mit `is_ca = IsCa::Ca(BasicConstraints::Constrained(0))` (kein Sub-CA-Pfad), `key_usages` (`KeyUsagePurpose`), `not_before`/`not_after` (`OffsetDateTime`, Helfer `date_time_ymd`), `serial_number: SerialNumber`, `distinguished_name`; `CertificateParams::self_signed(&KeyPair)` erzeugt das CA-Cert. (docs.rs Index, CertificateParams)
- **Leaf direkt:** `CertificateParams::signed_by(&KeyPair, &Issuer)`; `Issuer::new(params, signing_key)` / `Issuer::from_params(&params, signing_key)` / `Issuer::from_ca_cert_der(&CertificateDer, signing_key)` (Feature `x509-parser`) / `Issuer::from_ca_cert_pem(&str, signing_key)` (Features `pem` + `x509-parser`). `from_ca_cert_der` "assumes the certificate is a CA without validating the `BasicConstraints` extension". (docs.rs Issuer)
- **CSR:** Client: `CertificateParams::serialize_request(&KeyPair)` → DER/PEM. Server: `CertificateSigningRequestParams::from_der(&CertificateSigningRequestDer) -> Result<Self, Error>` (unterstützt SAN, Key Usage, Extended Key Usage, Basic Constraints; andere Extensions → `UnsupportedExtension`; ungültige Signatur → `InvalidCertificationRequestSignature`), dann `signed_by(&self, issuer: &Issuer<'_, impl SigningKey>) -> Result<Certificate, Error>` — "The issuer field is set to the issuer's subject, and the authority key identifier extension is populated. No validation of the issuer certificate itself is performed." (docs.rs CertificateSigningRequestParams)
- **Keys:** `KeyPair::generate()` = ECDSA P-256/SHA-256; `generate_for(alg)` mit `PKCS_ECDSA_P256_SHA256`, `PKCS_ECDSA_P384_SHA384`, `PKCS_ED25519`, RSA nur mit aws-lc-rs-Backend; `serialize_der()`/`serialize_pem()` liefern PKCS#8; Laden via `from_pem`, `from_pkcs8_der_and_sign_algo`. Backend-Feature `aws_lc_rs` oder `ring` (docs.rs KeyPair).
- EKU: `ExtendedKeyUsagePurpose::ClientAuth` / `ServerAuth` vorhanden (docs.rs ExtendedKeyUsagePurpose).
- **Antwort auf die Frage: ja, rcgen kann CA, Leaf und CSR-Signing.**

### X.509-Regeln (RFC 5280)

- Validity: `notBefore`/`notAfter`; für Zertifikate ohne definiertes Ablaufdatum "the notAfter SHOULD be assigned the GeneralizedTime value of 99991231235959Z" (§4.1.2.5).
- Serial: positiv, eindeutig pro CA, "MUST NOT use serialNumber values longer than 20 octets" (§4.1.2.2).
- Basic Constraints `cA`/`pathLenConstraint` (§4.2.1.9); Key Usage Pflicht in CA-Certs (§4.2.1.3); SAN muss critical sein, wenn Subject leer ist (§4.2.1.6).
- CRL (§5) und OCSP sind das PKI-Standardmittel für Revocation; rustls bietet `with_crls(..)`, `only_check_end_entity_revocation()`, `allow_unknown_revocation_status()` (WebPkiClientVerifier).

### Key-Speicherung auf dem Client

- **keyring 4.2.0** (crates.io: 4.2.0, aktualisiert 2026-08-29): Windows Credential Manager, macOS Keychain, Linux Secret Service und keyutils (docs.rs Index). Die Doku nennt eine Größenbegrenzung des Windows-Credential-Blobs und eingeschränkte Secret-Service-Verfügbarkeit auf headless Linux; konkrete Zahlen: unbelegt (nicht im abgerufenen Text).
- **DPAPI:** `CryptProtectData` — "Typically, only a user with the same logon credential as the user who encrypted the data can decrypt the data. In addition, the encryption and decryption usually must be done on the same computer." Flag `CRYPTPROTECT_LOCAL_MACHINE` bindet an den Rechner statt den User. Fügt MAC hinzu. Prompt-Flow (`pPromptStruct`) "deprecated and will be removed in February 2027" (Microsoft Learn).
- Datei mit Rechten 0600 unter `$XDG_CONFIG_HOME`/`%APPDATA%`: kein Beleg nötig, Standard-Dateisystemsemantik.

### PKCS#12 (RFC 7292)

- Zweck: "a transfer syntax for personal identity information, including private keys, certificates". Privacy- und Integrity-Mode mit Passwort. Die RFC selbst: "this method for password privacy mode is not recommended and is deprecated for new usage"; Legacy-Algorithmen `pbeWithSHAAnd3-KeyTripleDES-CBC`, RC2/RC4 nur für Kompatibilität.

### Invite-Link / URI-Schema

- RFC 7595: Scheme-Namen in Kleinbuchstaben, US-ASCII (RFC 3986); private Schemes "SHOULD use a prefix based on their domain name, expressed in reverse order"; "unregistered schemes can cause problems if use is not limited to a private environment"; Provisional-Registrierung ist First-Come-First-Served.
- Windows: Registrierung unter `HKEY_CLASSES_ROOT\<scheme>` mit leerem String-Wert `URL Protocol` und `shell\open\command = "…\app.exe" "%1"`. Warnungen aus der Doku: der Handler erhält die URI ggf. percent-decodiert oder nicht ("Internet Explorer percent-decodes the URI, but the Windows Run… command does not"), Leerzeichen/Quotes/Backslashes können die Kommandozeile aufspalten; Empfehlung: "Avoid spaces, quotes, or backslashes in your URI", bei Bedarf Base64. Handler müssen alle Parameter als bösartig behandeln. (Microsoft Learn, archivierte, aber offizielle Seite.)
- Linux: Desktop-Entry `Exec=… %u` — "%u: A single URL." (Desktop Entry Spec). Die Zuordnung `MimeType=x-scheme-handler/<scheme>` stammt aus der mime-apps-Spezifikation; deren Seite war nicht abrufbar (403/404) → **unbelegt**, in Step 3 prüfen.

### Vergleich Mumble

- Mumble: "Mumble uses certificates to identify users. A certificate can be self-generated or issued by a third party organization."; Zertifikat wird beim ersten Start erzeugt (User-Doku). Server: `certrequired=false` Default — "If this options is enabled, only clients which have a certificate are allowed to connect." (`mumble-server.ini`). Das ist TOFU-artig: der Server prüft keine CA, sondern kennt Zertifikat-Hashes registrierter User.

## Entscheidung

**Kandidat (a): Invite-Token + CSR über eine unauthentifizierte QUIC-Enrollment-Verbindung auf demselben Port, Server signiert mit rcgen-CA, Fingerprint-Allowlist im Server.**

Ablauf:
1. Admin: `icu-server invite --name alice` → Token (≥ 128 Bit zufällig, base64url ohne Padding, einmalig, Ablauf 24 h) → Link `icu://<host>:<port>/enroll?t=<token>&ca=<sha256-des-CA-Certs, base64url>`.
2. Client öffnet Link (URI-Handler) oder User fügt ihn ins Feld ein. Client erzeugt `KeyPair::generate()` (P-256), CSR mit CN = Name aus dem Invite (Server überschreibt ohnehin), verbindet mit ALPN `icu-enroll/1`. Server-Prüfung ohne TOFU: die Enrollment-`ClientConfig` setzt per `dangerous().set_certificate_verifier(..)` einen eigenen `ServerCertVerifier`, der das letzte Cert der präsentierten Kette (die CA) SHA-256-hasht und mit dem `ca`-Wert aus dem Link vergleicht; dafür muss der Server in `with_single_cert(cert_chain, ..)` die Kette Leaf + CA senden, nicht nur das Leaf. Die Hauptverbindung nutzt danach `with_root_certificates(ca_der)` mit dem in `EnrollResponse` gelieferten CA-Cert.
3. Server (ALPN `icu-enroll/1`, `peer_identity()` leer): liest Token + CSR (Größenlimit), prüft Token, `CertificateSigningRequestParams::from_der` → setzt selbst Subject, SAN, EKU `ClientAuth`, Validity, Serial → `signed_by(&Issuer)`; speichert SHA-256 des Leaf-DER + Name in der Allowlist; antwortet mit Leaf + CA-Cert; markiert Token als verbraucht; schließt die Verbindung.
4. Client speichert Key + Certs, verbindet neu mit ALPN `icu/1` und Client-Cert.
5. Server-Hauptverbindung: eine `ServerConfig` für beide ALPNs, daher `WebPkiClientVerifier::builder(icu_ca_roots).allow_unauthenticated().build()` — auf TLS-Ebene ist Client-Auth optional, ein präsentiertes Cert wird aber gegen die ICU-CA verifiziert. Auf Anwendungsebene ist es für ALPN `icu/1` Pflicht: `peer_identity()` leer → `close(AUTH_UNKNOWN_CERT)`; sonst Check "Fingerprint in Allowlist und nicht revoked", sonst `close(AUTH_UNKNOWN_CERT | AUTH_REVOKED)`. Für `icu-enroll/1` wird ein präsentiertes Cert ignoriert.

Detailentscheidungen:
- **Revocation:** Fingerprint-Allowlist im Server (SQLite oder TOML), Prüfung nach `peer_identity()` bei jedem Connect. Ein `revoke <name>` wirkt sofort (laufende Verbindung wird geschlossen). CRL/OCSP entfallen: es gibt genau einen Verifier (den eigenen Server), also braucht niemand ein verteilbares Revocation-Artefakt. rustls-CRL-Support bleibt ungenutzt.
- **Laufzeiten:** CA 10 Jahre; Client-Leaf 1 Jahr; Server-Leaf 1 Jahr. Renewal über die bereits authentifizierte Hauptverbindung (Client sendet neue CSR ab 30 Tage vor Ablauf, Server signiert, alter Fingerprint bleibt bis Ablauf gültig). Kein 9999-Ablauf (RFC 5280 erlaubt es), weil ein ablaufendes Cert ein natürliches Aufräumen verlorener Geräte erzwingt und webpki-Ablaufprüfung ohnehin greift.
- **Key-Speicherung:** Baseline Datei mit 0600 (Linux: `$XDG_CONFIG_HOME/icu/`, Windows: `%APPDATA%\icu\`), Key als PKCS#8 (`serialize_der`). Windows zusätzlich mit DPAPI (`CryptProtectData`, User-Scope) verschlüsselt. OS-Keystore (keyring) nicht in v1: Secret-Service-Abhängigkeit auf Linux ist für ein statisches Binary unerwünscht, Windows-Blob-Limit unbelegt.
- **CA-Erzeugung:** Server beim ersten Start: `KeyPair::generate()` + `CertificateParams { is_ca: Ca(Constrained(0)), key_usages: [KeyCertSign, CrlSign], … }.self_signed()`; CA-Key liegt nur auf dem Server (0600). Server-Leaf mit SAN (DNS und/oder IP) aus der Config, signiert von derselben CA; Clients vertrauen nur dieser CA (`ClientConfig::with_root_certificates`), `connect(addr, server_name)` mit dem Host aus dem Invite-Link.
- **Rate-Limits:** Bruteforce ist bei ≥ 128 Bit Token rechnerisch aussichtslos; das Limit dient nur der DoS-Abwehr für CSR-Signing (ECDSA-Signatur + DB-Write): pro Quell-IP max. 5 Enrollment-Versuche/Minute, global max. 20/Minute, ungültiger Token → sofort `close()`. Bei Last `Incoming::retry()` aktivieren, damit nur adressvalidierte Clients den Handshake bekommen.
- **URI-Schema:** `icu://` als privates Schema (RFC 7595: nur für privaten Gebrauch, keine Registrierung); Token und CA-Hash base64url (keine Leerzeichen/Quotes/Backslashes wegen der Windows-Kommandozeilen-Warnung). Alternativ-Weg ohne Handler: Link im Client-Dialog einfügen.

## Verworfen, mit Grund

- **(b) PKCS#12:** Der private Key entsteht auf dem Server und wandert über einen Seitenkanal (Chat, Mail) samt Passwort; RFC 7292 markiert die eigenen PBE-Modi als deprecated; Import-Dialoge sind die UX, die vermieden werden soll. Bleibt einziger Weg für Umgebungen ohne Netz zum Server — nicht ICU-Ziel.
- **(c) TOFU + Admin-Freigabe:** Kein Invite nötig, aber der Server müsste unbekannte, selbstsignierte Certs akzeptieren (`allow_unauthenticated()` oder eigener Verifier ohne CA-Kette), also kann jeder eine "pending"-Anfrage erzeugen (DoS auf die Freigabeliste, Namenskollisionen). Der Admin-Schritt liegt zeitlich nach dem Verbindungsversuch — schlechtere UX als ein Link, der sofort funktioniert. Mumble-Modell, bewusst nicht übernommen.
- **CRL/OCSP:** Es gibt keinen dritten Verifier. Eine Fingerprint-Liste im Server ist vollständig und sofort wirksam.
- **Raw Public Keys (RFC 7250):** spart X.509-Parsing, aber rcgen liefert X.509 ohne Mehraufwand und rustls/webpki prüfen Ablauf/EKU gratis.
- **Fingerprint-Prüfung im TLS-Handshake (eigener `ClientCertVerifier`):** möglich (`ClientCertVerified::assertion()`), aber die Anwendungs-Prüfung nach `peer_identity()` ist gleich sicher, einfacher zu testen und liefert dem Client einen Application-Fehlercode statt eines TLS-Alerts. Kann in Step 3 revidiert werden.
- **OS-Keystore in v1:** siehe oben; Windows-Blob-Limit und headless-Linux-Verhalten unbelegt.

## Offene Punkte für Step 3

- `peer_identity()` bei `allow_unauthenticated()`-Verbindung ohne Client-Cert: `None` vs. `Some(leer)`; und: sendet rustls bei `allow_unauthenticated()` trotzdem einen CertificateRequest, so dass ein Client mit Cert auf `icu-enroll/1` sein Cert liefert? (Server muss das ignorieren.)
- Ein `ServerConfig` mit `allow_unauthenticated()` für beide ALPNs vs. zwei `ServerConfig`s per `Incoming::accept_with(..)` nach ALPN-Peek: prüfen, ob `Incoming` den ALPN vor `accept` liefert (in der Doku nicht ersichtlich → Entwurf geht von einer Config aus; Pflicht-Auth auf `icu/1` wird in der Anwendung erzwungen).
- `rustls::client::danger::ServerCertVerifier`-Trait (Pflichtmethoden, Kettenreihenfolge in `intermediates`, ob das CA-Cert als Teil der vom Server gesendeten Kette ankommt) — Trait-Doku nicht abgerufen, nur `dangerous()`/`set_certificate_verifier` belegt; nachlesen und den Hash-Pinning-Verifier testen.
- rcgen `from_der` mit CSRs, die keine Extensions tragen; ob `signed_by` die vom Server gesetzten Params übernimmt oder die CSR-Params (Subject-Override testen).
- Cert-Chain-Größe im Handshake (P-256 Leaf + P-256 CA) und ob Initial-Pakete gepaddet ausreichen.
- Windows: `icu://`-Handler-Registrierung ohne Admin-Rechte (HKCU statt HKCR) — Doku beschreibt nur HKCR; Linux: `x-scheme-handler/icu` in `.desktop` + `xdg-mime default` verifizieren.
- DPAPI-Aufruf aus Rust (`windows`-Crate) und Roaming-Profile-Verhalten.
- Uhrzeit-Skew: Client mit falscher Uhr scheitert an `notBefore`; `not_before` 1 h in die Vergangenheit setzen und Fehlermeldung im Client testen.
- Renewal-Pfad über authentifizierte Verbindung end-to-end.

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `../history/verification.md`):
  - Spike 2b **bestätigt**: eine `ServerConfig` mit `allow_unauthenticated()` für beide ALPNs; ALPN per `handshake_data()` vor Handshake-Ende lesbar; `peer_identity()` ohne Client-Cert = `None`, mit Cert = `Some(vec![leaf])`.
  - Spike 2b **bestätigt** rcgen 0.14: `serialize_request` (CSR 218 B) → `CertificateSigningRequestParams::from_der` → Server überschreibt Subject/EKU/Validity → `signed_by(&Issuer)`; CSR-CN `admin-impersonator` wird zu `alice`. DER-Größen: CA 366 B, Server-Leaf 360 B, Client-Leaf 318 B.
  - Offen: `ServerCertVerifier`-Pinning auf CA-Hash (nicht gebaut), URI-Handler-Registrierung je OS, DPAPI, Renewal end-to-end.

Mit WebFetch gelesen (2026-09-20):
- https://www.rfc-editor.org/rfc/rfc9001.html
- https://www.rfc-editor.org/rfc/rfc8446.html
- https://www.rfc-editor.org/rfc/rfc5280.html
- https://www.rfc-editor.org/rfc/rfc7250.html
- https://www.rfc-editor.org/rfc/rfc7292.html
- https://www.rfc-editor.org/rfc/rfc7595.html
- https://crates.io/api/v1/crates/rcgen
- https://crates.io/api/v1/crates/keyring
- https://docs.rs/rcgen/latest/rcgen/index.html
- https://docs.rs/rcgen/latest/rcgen/struct.CertificateParams.html
- https://docs.rs/rcgen/latest/rcgen/struct.CertificateSigningRequestParams.html
- https://docs.rs/rcgen/latest/rcgen/struct.Issuer.html
- https://docs.rs/rcgen/latest/rcgen/struct.KeyPair.html
- https://docs.rs/rcgen/latest/rcgen/enum.ExtendedKeyUsagePurpose.html
- https://docs.rs/keyring/latest/keyring/index.html
- https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html
- https://docs.rs/rustls/latest/rustls/server/danger/trait.ClientCertVerifier.html
- https://docs.rs/rustls/latest/rustls/server/danger/struct.ClientCertVerified.html
- https://docs.rs/rustls/latest/rustls/client/struct.ClientConfig.html
- https://docs.rs/rustls/latest/rustls/struct.ConfigBuilder.html
- https://docs.rs/quinn/latest/quinn/struct.Connection.html
- https://docs.rs/quinn/latest/quinn/struct.Incoming.html
- https://docs.rs/quinn/latest/quinn/crypto/rustls/struct.HandshakeData.html
- https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata
- https://learn.microsoft.com/en-us/previous-versions/windows/internet-explorer/ie-developer/platform-apis/aa767914(v=vs.85)
- http://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
- https://raw.githubusercontent.com/mumble-voip/mumble/master/auxiliary_files/mumble-server.ini
- https://www.mumble.info/documentation/user/certificates/

> **Revidiert 2026-09-22 (Name im Zertifikat).** Der Subject-Override ist entfallen: der Server prüft den CN der CSR (genau einer, `validate_name`, serverweit eindeutig) und übernimmt ihn, statt ihn zu ersetzen. SAN, EKU, Basic Constraints, Key Usage, Validity und Serial setzt er weiterhin selbst. Die Sätze in Zeile 98 und 136 („Server überschreibt ohnehin", „CSR-CN `admin-impersonator` wird zu `alice`") beschreiben den Stand von Spike 2b und bleiben als Spike-Protokoll stehen. Maßgeblich ist `../reference/protocol.md` §2.3.
