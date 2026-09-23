# ICU-Server: Bauen und Installieren

Stand 2026-09-20. Normative Grundlage: `../PROTOCOL.md`, Bauplan
`../Server/IMPLEMENTATION.md`.

Der Server ist ein einziges Binary ohne Laufzeitabhängigkeiten: kein libc,
kein CA-Bundle (er ist seine eigene CA), kein Codec, keine Datenbank außer zwei
TOML-Dateien.

## 1. Bauen

### Host-Build (Entwicklung, Tests)

```bash
cargo build --release -p icu-server
cargo test --workspace
```

Fedora 43 mit dem Paket `rust` (1.98.1) reicht; rustup wird **nicht** gebraucht
und darf für dieses Projekt auch nicht installiert werden. Das Binary liegt
dann unter `target/release/icu-server` und ist dynamisch gegen die System-libc
gelinkt — gut für Tests, nicht für die Auslieferung.

### Statisches musl-Binary (Auslieferung)

Fedora hat kein `rust-std-static-x86_64-unknown-linux-musl`, deshalb läuft der
musl-Build in einem Container. Verifiziert am 2026-09-20 mit podman 5.8.4:

```bash
podman run --rm \
  -e CARGO_TARGET_DIR=/src/target/musl \
  -v "$PWD":/src:Z -w /src \
  docker.io/rust:1-alpine \
  sh -c 'apk add --no-cache musl-dev && \
         cargo build --release -p icu-server --target x86_64-unknown-linux-musl'
```

Ergebnis: `target/musl/x86_64-unknown-linux-musl/release/icu-server`,
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
file   target/musl/x86_64-unknown-linux-musl/release/icu-server   # static-pie
ldd    target/musl/x86_64-unknown-linux-musl/release/icu-server   # statically linked
       target/musl/x86_64-unknown-linux-musl/release/icu-server --version
```

## 2. Installieren (systemd)

```bash
sudo install -m0755 target/musl/x86_64-unknown-linux-musl/release/icu-server \
  /usr/local/bin/icu-server
sudo install -m0644 deploy/icu-server.service /etc/systemd/system/
sudo groupadd --system icu            # Gruppe für den Admin-Socket
sudo install -d -m0755 /etc/icu
sudo install -m0640 -g icu /dev/stdin /etc/icu/server.toml <<'TOML'
[server]
listen = ["0.0.0.0:4711", "[::]:4711"]
server_name = "icu.example.org"
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

`StateDirectory=icu` legt `/var/lib/icu/` an; beim ersten Start entstehen dort
`ca.key` (0600), `ca.crt`, `server.key` (0600), `server.crt`, `users.toml`
und `state.lock`. **`ca.key` ist das einzige Geheimnis, das zählt** — geht es
verloren, müssen sich alle Nutzer neu registrieren. Ein Backup von
`/var/lib/icu/` gehört zur Inbetriebnahme.

Die Unit bindet IPv4 und IPv6 getrennt auf denselben Port; der Server setzt
dafür `IPV6_V6ONLY` auf dem v6-Socket. Für einen Port unter 1024 siehe den
Kommentar in `icu-server.service` (`AmbientCapabilities=CAP_NET_BIND_SERVICE`).

Firewall:

```bash
sudo firewall-cmd --permanent --add-port=4711/udp && sudo firewall-cmd --reload
```

## 3. Bedienen

Alle Kommandos außer `run` sprechen über `/run/icu/admin.sock` mit dem
laufenden Server; eine Änderung wirkt damit sofort und nicht erst nach einem
Neustart. Sie brauchen root oder Mitgliedschaft in der Socket-Gruppe.

```bash
icu-server invite --ttl 24h --out bob.txt  # Registrierungs-Bundle, einmalig, 24 h gültig
icu-server users                           # Tabelle mit user_id, Cert, online
icu-server revoke alice                    # alle Certs sperren, sofort kicken
icu-server status                          # Uptime, online, Relay-Zähler
icu-server status --json                   # dasselbe für Monitoring
icu-server rotate-server-cert              # neues Server-Leaf derselben CA
```

**Erster Admin:** das erste Bundle auf einem leeren Server ist das des Admins —
direkt nach der Installation also `icu-server invite --out admin.txt` (geht auch
ohne laufenden Server), die Datei auf den eigenen Rechner bringen und dort
`icu admin.txt --name <name>` ausführen oder den Inhalt ins Erststart-Fenster
einfügen. Das Bundle enthält einen privaten Schlüssel und gilt, bis es
eingelöst oder abgelaufen ist; es gehört nicht in einen Chat, den andere lesen.
Weitere Bundles stellt ein Admin auch im Client aus (⚙ → Allgemein →
Einladung). Die Serveradresse steht in den Zertifikaten (`public_host`,
`server_name`), der Eingeladene braucht nichts weiter.

Läuft **kein** Server, erledigt die CLI dieselbe Arbeit selbst und hält dabei
`flock(/var/lib/icu/state.lock)`. Läuft einer und der Socket fehlt, bricht die
CLI mit `cannot lock … (is a server running?)` ab — das ist Absicht, damit CLI
und Server einander nie eine Änderung überschreiben.

`rotate-server-cert` schreibt das neue Leaf auf Platte; präsentiert wird es
nach einem `systemctl restart icu-server` (quinn hält die rustls-Config aus dem
Start fest). Clients brauchen nichts, die CA bleibt dieselbe.

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
`target/x86_64-unknown-linux-musl/release/icu-server`. Baut man mit dem
`CARGO_TARGET_DIR` von oben, entweder den Pfad im Dockerfile anpassen oder

```bash
mkdir -p target/x86_64-unknown-linux-musl/release
cp target/musl/x86_64-unknown-linux-musl/release/icu-server \
   target/x86_64-unknown-linux-musl/release/
```

Der Admin-Socket liegt im Container unter `/run/icu/admin.sock`; die CLI läuft
deshalb per `podman exec` **im selben Container**, nicht auf dem Host.

## 5. Monitoring

`checkmk/icu_server` ist ein Checkmk-Local-Check. Installation:

```bash
sudo install -m0755 deploy/checkmk/icu_server /usr/lib/check_mk_agent/local/
sudo usermod -aG icu root      # oder den Agent-User in die Gruppe icu
```

Er ruft `icu-server status --json` auf und liefert `online`, `uptime` und die
Relay-Zähler als Metriken. Ist der Server unten, geht der Check auf CRIT.

## 6. Grenzen

* **Kein Prometheus in v1.** `status --json` ist die Schnittstelle.
* **Keine Rechteverwaltung.** Wer den Admin-Socket lesen darf, darf alles.
* **Ein Raum, ein Port, ein Prozess.** Zwei Server auf derselben State-Datei
  sind durch `flock` ausgeschlossen, nicht durch Koordination.
* Der Unix-Socket-Pfad muss kürzer als 108 Byte sein (`SUN_LEN`). Kann der
  Socket nicht angelegt werden, läuft der Server weiter und protokolliert eine
  Warnung; nur die CLI ist dann ohne ihn.
