# ICU-Server: Bauen und Installieren

Stand 2026-09-20. Normative Grundlage: `../PROTOCOL.md`, Bauplan
`../Server/IMPLEMENTATION.md`.

Der Server ist ein einziges Binary ohne Laufzeitabhängigkeiten: kein libc,
kein CA-Bundle (er ist seine eigene CA), kein Codec, keine Datenbank außer zwei
TOML-Dateien.

## 0. Pakete (rpm/deb)

Fertige Pakete liegen unter https://github.com/christianreiss/icu/releases:
`icu-server` (ein statisches Binary, dasselbe für Fedora und Debian) und der
Client `icu` je Distro (Fedora 42, Fedora 43, Debian 13).

```bash
sudo dnf install ./icu-server-<ver>-1.x86_64.rpm        # Fedora
sudo apt install ./icu-server_<ver>-1_amd64.deb          # Debian
sudoedit /etc/icu/server.toml                            # server_name, public_host
sudo systemctl enable --now icu-server
sudo firewall-cmd --permanent --add-service=icu && sudo firewall-cmd --reload
```

Das Paket legt die Gruppe `icu` an (`/usr/lib/sysusers.d/icu-server.conf`),
installiert die Unit nach `/usr/lib/systemd/system/`, eine Beispielkonfiguration
nach `/etc/icu/server.toml` (bleibt bei Updates erhalten), den firewalld-Dienst
`icu` und den Checkmk-Check nach `/usr/share/icu-server/checkmk/`. Der Dienst
wird **nicht** automatisch gestartet, weil `server_name` gesetzt sein muss.
Deinstallieren lässt den State (`/var/lib/private/icu/`, darin `ca.key`) liegen.

Bauen aller Pakete (nur podman nötig): `packaging/build.sh`, Ergebnis unter
`dist/<ver>/`; Veröffentlichen: `packaging/release.sh`. Details in den
Kommentaren der beiden Skripte.

## 1. Bauen

### Host-Build (Entwicklung, Tests)

```bash
cargo build --profile release-server -p icu-server
cargo test --workspace
```

Fedora 43 mit dem Paket `rust` (1.98.1) reicht; rustup wird **nicht** gebraucht
und darf für dieses Projekt auch nicht installiert werden. Das Binary liegt
dann unter `target/release-server/icu-server` und ist dynamisch gegen die
System-libc gelinkt — gut für Tests, nicht für die Auslieferung.

Der Server wird mit dem Profil `release-server` gebaut (`Cargo.toml`): das
`release`-Profil mit `panic = "unwind"` statt `abort`. Ein Panic in einer
Verbindung beendet dann nur diese — der Server protokolliert ihn, nimmt die
Sitzung aus dem Raum und alle anderen Gespräche laufen weiter —, statt den
Prozess und damit jedes Gespräch zu beenden. Der Client bleibt bei `abort`.

### Statisches musl-Binary (Auslieferung)

Fedora hat kein `rust-std-static-x86_64-unknown-linux-musl`, deshalb läuft der
musl-Build in einem Container. Verifiziert am 2026-09-20 mit podman 5.8.4:

```bash
podman run --rm \
  -e CARGO_TARGET_DIR=/src/target/musl \
  -v "$PWD":/src:Z -w /src \
  docker.io/rust:1-alpine \
  sh -c 'apk add --no-cache musl-dev && \
         cargo build --profile release-server -p icu-server --target x86_64-unknown-linux-musl'
```

Ergebnis: `target/musl/x86_64-unknown-linux-musl/release-server/icu-server`,
rund 4,6 MB, `static-pie linked`, `ldd` meldet `statically linked`.

Hinweise:

* `musl-dev` ist nötig, weil `ring` C-Code übersetzt. rustls/ring bauen danach
  ohne weitere Pakete durch.
* `CARGO_TARGET_DIR` zeigt bewusst in `target/`, das in `.gitignore` steht;
  sonst legt der Container-Build ein zweites, uneingecheckt bleibendes
  Verzeichnis an.
* Unter rootless podman ist Container-root der aufrufende Host-User, die
  erzeugten Dateien gehören also nicht root.
* Ohne Netz im Container schlägt `apk add` fehl. Dann entweder ein Image mit
  vorinstalliertem `musl-dev` verwenden oder auf einem Host mit rustup
  `rustup target add x86_64-unknown-linux-musl` benutzen.

Prüfen:

```bash
file   target/musl/x86_64-unknown-linux-musl/release-server/icu-server   # static-pie
ldd    target/musl/x86_64-unknown-linux-musl/release-server/icu-server   # statically linked
       target/musl/x86_64-unknown-linux-musl/release-server/icu-server --version
```

## 2. Installieren von Hand (systemd)

```bash
sudo install -m0755 target/musl/x86_64-unknown-linux-musl/release-server/icu-server \
  /usr/bin/icu-server
sudo install -m0644 deploy/icu-server.service /etc/systemd/system/
sudo groupadd --system icu            # Gruppe für den Admin-Socket
sudo install -d -m0755 /etc/icu
sudo install -m0640 -g icu /dev/stdin /etc/icu/server.toml <<'TOML'
[server]
listen = ["0.0.0.0:4711", "[::]:4711"]
server_name = "icu.example.org"      # durch den eigenen Namen ersetzen
public_host = "icu.example.org:4711"
max_users = 16
min_client = "0.1.0"
admin_socket_group = "icu"

[log]
level = "info"
format = "text"
TOML
sudo systemctl daemon-reload
sudo systemctl enable --now icu-server
```

`server_name` und `public_host` landen in der CA und in jedem Zertifikat;
später ändern heißt neue CA und alle registrieren sich neu. Mit den Namen aus
dem Beispiel (`example.org`, `.example`, `.invalid`, `.test`, RFC 2606) startet
der Server deshalb nicht, und `icu-server invite` stellt nichts aus.
`localhost` und IP-Adressen gehen.

`StateDirectory=icu` legt `/var/lib/icu/` an; beim ersten Start entstehen dort
`ca.key` (0600), `ca.crt`, `server.key` (0600), `server.crt`, `users.toml`
und `state.lock`. **`ca.key` ist das einzige Geheimnis, das zählt** — geht es
verloren, müssen sich alle Nutzer neu registrieren. Ein Backup von
`/var/lib/icu/` gehört zur Inbetriebnahme. Eine neue CA legt der Server nur an,
wenn `ca.crt` und `ca.key` beide fehlen und `users.toml` noch nie benutzt wurde
(keine Nutzer, keine CRL); fehlt nur eine, ist eine nicht lesbar (etwa nach
einem Restore mit falschem Besitzer), passen sie nicht zusammen oder fehlen
beide neben einer benutzten `users.toml`, startet er nicht und sagt, welche
Datei es ist. `server.crt` und
`server.key` sind dagegen entbehrlich: passen sie nicht, stellt der Server beim
Start selbst ein neues Leaf aus. `server.crt.retired` gibt es nur, solange ein
ersetztes Server-Leaf noch nicht auf der CRL steht (etwa weil `users.toml` bei
der Rotation nicht geschrieben werden konnte); der nächste Start trägt es nach
und löscht die Datei. Nicht von Hand löschen, sonst bleibt das alte Leaf
gültig.

**Backup und Restore.** `ca.crt`, `ca.key` und `users.toml` gehören zusammen
und kommen aus demselben Backup zurück, lesbar für den Dienst. Meldet der
Start `no CA in …: ca.crt and ca.key are both missing, but users.toml has been
in use`, fehlt die CA neben einem benutzten Nutzerbestand — ein unvollständiger
Restore, eine Migration ohne die CA: beide Dateien aus dem Backup zurückholen.
Eine neue CA sperrte jeden registrierten Nutzer aus, deshalb legt der Server
dann keine an. Wer wirklich neu anfangen will (neue CA, alle registrieren sich
neu), schiebt das ganze State-Verzeichnis beiseite.

**Nach dem Zurückspielen eines Backups** `icu-server rotate-server-cert`
ausführen. Lag zwischen Backup und Restore eine Rotation, präsentiert der Server
sonst ein Leaf, das die Clients schon auf ihrer CRL haben; sie versuchen es dann
im Backoff weiter, bis das neue Leaf da ist. Die CRL-Nummer folgt der Uhr und
fällt durch einen Restore nicht unter die der Clients.

Die Unit bindet IPv4 und IPv6 getrennt auf denselben Port; der Server setzt
dafür `IPV6_V6ONLY` auf dem v6-Socket. Für einen Port unter 1024 siehe den
Kommentar in `icu-server.service` (`AmbientCapabilities=CAP_NET_BIND_SERVICE`).

Firewall:

```bash
sudo firewall-cmd --permanent --add-port=4711/udp && sudo firewall-cmd --reload
```

### Im offenen Internet

Freigeben muss man nur den einen UDP-Port; der Server braucht keinen
Reverse-Proxy und kein TLS davor, er terminiert QUIC/TLS 1.3 selbst. Ohne
gültiges Zertifikat der eigenen CA kommt niemand über den Handshake hinaus —
die Angriffsfläche für Unbekannte ist genau dieser Handshake. Dafür gilt
(`PROTOCOL.md` §7, Werte in `[limits]`/`[timing]` überschreibbar):

* Handshakes 10/s pro Quell-IP, IPv6 pro /64, und 100/s global; ab 32
  laufenden Handshakes muss jede neue Quelle ihre Adresse per Retry beweisen.
* Eine Quelle, die das getan hat, hält höchstens 4 Handshakes zugleich (je
  IPv6-/48 höchstens 16); insgesamt laufen höchstens 256
  (`limits.max_handshakes`), jeder darf höchstens 5 s dauern. Etablierte
  Verbindungen zählen getrennt davon (`limits.max_connections`, 256), damit
  hängende Handshakes niemandem den Reconnect verbauen.
* Verbindungen ohne Zertifikat loggt der Server nur auf `debug`.

Der Admin-Socket ist ein Unix-Socket und nie im Netz. Die Unit begrenzt den
Speicher mit `MemoryMax=1G` als Rückfallebene und ist so weit gehärtet, dass
`systemd-analyze security icu-server` 1,4 meldet (vorher 3,5). Gegen volumetrische UDP-Floods
hilft nur Upstream-Filterung (Provider, Firewall) — das kann kein Prozess
selbst.

## 3. Bedienen

Alle Kommandos außer `run` sprechen über `/run/icu/admin.sock` mit dem
laufenden Server; eine Änderung wirkt damit sofort und nicht erst nach einem
Neustart. Sie brauchen root oder Mitgliedschaft in der Gruppe `icu`: die Unit
läuft mit `Group=icu`, damit gehören `/run/icu` (0750) und der Socket (0660)
dieser Gruppe. `server.admin_socket_group` bleibt deshalb `icu`.

```bash
icu-server invite --ttl 24h --out bob.txt  # Registrierungs-Bundle, einmalig, 24 h gültig
icu-server users                           # Tabelle mit user_id, Cert, online
icu-server revoke alice                    # alle Certs sperren, sofort kicken
icu-server status                          # Uptime, online, Relay-Zähler
icu-server status --json                   # dasselbe für Monitoring
icu-server rotate-server-cert              # neues Server-Leaf derselben CA
icu-server renew-ca                        # CA-Zertifikat neu, selber Schlüssel
```

**Erster Admin:** Admin wird, wer als Erster eine Registrierung abschließt
(quittiert), solange es noch keinen bestätigten Admin gibt — nicht das zuerst
ausgestellte Bundle (`PROTOCOL.md` §2.3). Direkt nach der Installation also
`icu-server invite --out admin.txt` (geht auch ohne laufenden Server), die Datei
auf den eigenen Rechner bringen und dort `icu admin.txt --name <name>` ausführen
oder den Inhalt ins Erststart-Fenster einfügen — bevor weitere Einladungen
hinausgehen. Solange es keinen bestätigten Admin gibt, weist `invite` darauf
hin. Das Bundle enthält einen privaten Schlüssel und gilt, bis es
eingelöst oder abgelaufen ist; es gehört nicht in einen Chat, den andere lesen.
Weitere Bundles stellt ein Admin auch im Client aus (⚙ → Allgemein →
Einladung). Die Serveradresse steht in den Zertifikaten (`public_host`,
`server_name`), der Eingeladene braucht nichts weiter.

Läuft **kein** Server, erledigt die CLI dieselbe Arbeit selbst und hält dabei
`flock(/var/lib/icu/state.lock)`. Läuft einer und der Socket fehlt, bricht die
CLI mit `cannot lock … (is a server running?)` ab — das ist Absicht, damit CLI
und Server einander nie eine Änderung überschreiben.

`rotate-server-cert` stellt ein neues Leaf derselben CA aus, setzt das alte auf
die CRL und wirkt ohne Neustart für jede neue Verbindung. Clients brauchen
nichts. **Der Server rotiert selbst**, sobald sein Leaf weniger als 30 Tage
(höchstens ein Drittel von `server_validity_days`) Restlaufzeit hat, geprüft
beim Start und stündlich. Ein abgelaufenes oder widerrufenes Server-Leaf ist
für die Clients kein fataler Fehler: sie versuchen es weiter und verbinden
sich, sobald das neue Leaf da ist.

**Die CA** (Default 3650 Tage) stellt der Server ein Jahr vor Ablauf selbst
neu aus — mit demselben Schlüssel und Subject, also ohne dass ein Client etwas
tun muss (von Hand: `icu-server renew-ca`). Clients vertrauen dem Schlüssel,
nicht den Daten; auch eine abgelaufene CA-Datei beim Client verankert weiter.

**Abgelaufene Client-Zertifikate** erneuern sich selbst: der Client verbindet
sich mit dem alten Zertifikat, bekommt eine renew-only-Sitzung und ein neues
Zertifikat auf denselben Schlüssel (`PROTOCOL.md` §2.3.3) — egal, wie lange es
her ist. Wer einen Zugang wirklich beenden will, nimmt `revoke` oder
`disable`; ein bloß abgelaufenes Zertifikat sperrt niemanden aus.

## 4. Container (`FROM scratch`)

```bash
podman build -f deploy/Dockerfile -t icu-server .
podman volume create icu-state
podman run -d --name icu \
  -p 4711:4711/udp \
  -v icu-state:/var/lib/icu \
  -v ./server.toml:/etc/icu/server.toml:ro \
  icu-server
podman exec icu /icu-server invite --name alice
```

Das Image erwartet das musl-Binary aus Schritt 1 unter
`target/x86_64-unknown-linux-musl/release-server/icu-server`. Baut man mit dem
`CARGO_TARGET_DIR` von oben, entweder den Pfad im Dockerfile anpassen oder

```bash
mkdir -p target/x86_64-unknown-linux-musl/release-server
cp target/musl/x86_64-unknown-linux-musl/release-server/icu-server \
   target/x86_64-unknown-linux-musl/release-server/
```

Der Admin-Socket liegt im Container unter `/run/icu/admin.sock`; die CLI läuft
deshalb per `podman exec` **im selben Container**, nicht auf dem Host. `/run/icu`
und `/var/lib/icu` legt das Image für uid 65532 an; ein neues benanntes Volume
übernimmt diesen Besitzer. Für einen Bind-Mount statt eines Volumes unter
rootless podman `-v ./state:/var/lib/icu:U` verwenden.

## 5. Monitoring

`checkmk/icu_server` ist ein Checkmk-Local-Check. Installation:

```bash
sudo install -m0755 deploy/checkmk/icu_server /usr/lib/check_mk_agent/local/
sudo usermod -aG icu root      # oder den Agent-User in die Gruppe icu
```

Er ruft `icu-server status --json` auf und liefert `online`, `uptime` und die
Relay-Zähler als Metriken. Ist der Server unten, geht der Check auf CRIT.
Dazu kommen `ICU_Server_Certificate` und `ICU_CA_Certificate` (`tls` im
Status-JSON): WARN unter 30, CRIT unter 7 Tagen Restlaufzeit. Beide erneuert
der Server selbst (Leaf 30 Tage, CA ein Jahr vor Ablauf), ein WARN heißt also,
dass das gescheitert ist (Journal; von Hand `rotate-server-cert` bzw.
`renew-ca`).

## 6. Grenzen

* **Kein Prometheus in v1.** `status --json` ist die Schnittstelle.
* **Keine Rechteverwaltung.** Wer den Admin-Socket lesen darf, darf alles.
* **Ein Raum, ein Port, ein Prozess.** Zwei Server auf derselben State-Datei
  sind durch `flock` ausgeschlossen, nicht durch Koordination.
* Der Unix-Socket-Pfad muss kürzer als 108 Byte sein (`SUN_LEN`). Kann der
  Socket nicht angelegt werden, läuft der Server weiter und protokolliert eine
  Warnung; nur die CLI ist dann ohne ihn.
