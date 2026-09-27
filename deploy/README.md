# ICU-Server: Bauen und Installieren

Stand 2026-09-20. Normative Grundlage: `../PROTOCOL.md`, Bauplan
`../Server/IMPLEMENTATION.md`.

Der Server ist ein einziges Binary ohne Laufzeitabhängigkeiten: kein libc,
kein CA-Bundle (er ist seine eigene CA), kein Codec, keine Datenbank außer zwei
TOML-Dateien.

## 0. Pakete (rpm/deb)

Fertige Pakete liegen unter https://github.com/christianreiss/icu/releases:
`icu-server` (ein statisches Binary, dasselbe Paket für Fedora, AlmaLinux 10+
und Debian 12+) und der Client `icu` je Distro (Fedora 42, Fedora 43, Debian 13).

```bash
sudo dnf install ./icu-server-<ver>-1.x86_64.rpm        # Fedora, AlmaLinux 10+
sudo apt install ./icu-server_<ver>-1_amd64.deb          # Debian 12+
sudoedit /etc/icu/server.toml                            # server_name, public_host (optional)
sudo systemctl enable --now icu-server
sudo firewall-cmd --permanent --add-service=icu && sudo firewall-cmd --reload
sudo icu-server invite --out admin.txt                   # zuerst sich selbst (§3 „Erster Admin“)
```

Das Paket legt die Gruppe `icu` an (`/usr/lib/sysusers.d/icu-server.conf`),
installiert die Unit nach `/usr/lib/systemd/system/`, eine Beispielkonfiguration
nach `/etc/icu/server.toml` (bleibt bei Updates erhalten), den firewalld-Dienst
`icu` und den Checkmk-Check nach `/usr/share/icu-server/checkmk/`. Der Dienst
wird **nicht** automatisch gestartet: erst `server_name` prüfen. Ohne
`server_name` nimmt der Server den FQDN des Hosts (`hostname --fqdn`), aber
nur, wenn der Name per DNS auf eine Adresse dieses Hosts zeigt, die weder
Loopback noch Link-local ist (Debians `127.0.1.1`-Eintrag und `fe80::` von
nss-myhostname zählen nicht); sonst startet er nicht
und sagt, warum. Gibt es schon eine CA, gilt der Name aus ihrem CN
(`ICU CA <server_name>`), egal wie der Host inzwischen heißt; ein gesetztes
`server_name`, das davon abweicht, lehnt der Server beim Start ab (die Clients
nehmen den Namen aus der CA, ein Zertifikat auf einen anderen sperrte sie
alle aus). Hinter NAT oder unter einem DNS-Alias `server_name` und
`public_host` selbst setzen.
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
Ein mit schlichtem `--release` gebauter Server läuft auch, warnt aber beim
Start (`built with panic = "abort"`): bei ihm beendet ein Panic in einer
Sitzung den ganzen Server.

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
server_name = "icu.example.org"      # eigener Name; weglassen = FQDN des Hosts
public_host = "icu.example.org:4711"
max_users = 16
min_client = "0.1.0"                 # ältere Clients werden beim Hello abgewiesen
admin_socket_group = "icu"

[log]
level = "info"
format = "text"
TOML
sudo systemctl daemon-reload
sudo systemctl enable --now icu-server
```

`server_name` und `public_host` landen in der CA und in jedem Zertifikat;
später ändern heißt neue CA und alle registrieren sich neu. Weicht
`server_name` vom Namen der bestehenden CA ab, startet der Server nicht und
nennt beide Werte: zurück auf den CA-Namen oder die Zeile weglassen. Mit den Namen aus
dem Beispiel (`example.org`, `.example`, `.invalid`, `.test`, RFC 2606) legt
der Server deshalb keine CA an — der erste Start scheitert —, und die erste
Einladung eines Servers ohne Nutzer stellt `icu-server invite` nicht aus.
Eine bestehende Installation, deren CA einen solchen Namen schon trägt,
startet weiter und warnt im Journal. `localhost` und IP-Adressen gehen.

`StateDirectory=icu` legt `/var/lib/icu/` an; beim ersten Start entstehen dort
`ca.key` (0600), `ca.crt`, `com.key` (0600), `server.key` (0600), `server.crt`,
`users.toml` und `state.lock`. **`ca.key` und `com.key` sind die Geheimnisse,
die zählen** — geht eines verloren, müssen sich alle Nutzer neu registrieren.
`com.key` ist der Com-Schlüssel (2026-09-27, `PROTOCOL.md` §1): jedes
UDP-Datagramm trägt ein Tag daraus, und jeder Passport trägt ihn. Der Server
sichert den State selbst (unten, „Backup und Restore“); eine Kopie davon außer
Haus gehört zur Inbetriebnahme. Eine neue CA legt der Server nur an,
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

**Backup und Restore** (2026-09-27). Der Server sichert seinen State selbst:
beim Start und wenige Sekunden nach jeder Änderung (Einladung, Registrierung,
Widerruf, Rotation, …) einen Snapshot nach `/var/lib/icu/backups/<UTC-Zeit>/`;
die neuesten 30 bleiben. Ein Snapshot enthält `ca.crt`, `ca.key`, `com.key`,
`users.toml`, das Server-Leaf und ein `MANIFEST` mit Prüfsummen, alles 0600
bzw. 0700 und wie der State nur für root lesbar. Ein Neustart ohne Änderung
legt keinen neuen an. Einstellbar in `server.toml`:

```toml
[backup]
keep = 30                  # so viele bleiben; 0 schaltet die automatischen ab
#dir = "/srv/icu-backups"  # Default: backups/ im State-Verzeichnis
#settle_ms = 5000          # Wartezeit nach einer Änderung
```

```bash
sudo icu-server backup            # Snapshot jetzt, gibt den Pfad aus
sudo icu-server backup --list     # vorhandene Snapshots mit Zeit, Anlass, Nutzerzahl
```

Scheitert ein Snapshot (Platte voll, Rechte), läuft der Server weiter;
`icu-server status --json` meldet `"backup":{"failed":1,"error":…}`, und der
Checkmk-Check zeigt `ICU_Backup` auf WARN.

**Außer Haus kopieren.** Die Snapshots liegen auf derselben Platte wie der
State; gegen Plattenverlust oder einen verlorenen Host das Verzeichnis
regelmäßig woandershin kopieren, etwa per Timer mit
`rsync -a /var/lib/private/icu/backups/ backup-host:/srv/icu/` oder mit
borg/restic. Die Kopie enthält `ca.key` und `com.key`: verschlüsselt oder auf
ein vertrauenswürdiges Ziel. Soll der Server direkt woandershin schreiben (NFS,
zweite Platte), `backup.dir` setzen und der Unit das Verzeichnis freigeben —
sie läuft mit `ProtectSystem=strict`:

```ini
# sudo systemctl edit icu-server
[Service]
ReadWritePaths=/srv/icu-backups
```

**Zurückspielen**, nach einem Fehler (falscher Widerruf, verkorkster Bestand):

```bash
sudo systemctl stop icu-server
sudo icu-server restore latest   # oder ein Snapshot: /var/lib/icu/backups/20260927T120000Z
sudo systemctl start icu-server
```

`restore` prüft den Snapshot gegen sein `MANIFEST` und lädt ihn probehalber so,
wie der Server es täte (`users.toml`, CA-Paar, Com-Schlüssel, Servername); erst
dann ersetzt es etwas. Den ersetzten State legt es vorher als Snapshot
`pre-restore` ab — ein Restore lässt sich also selbst zurücknehmen —, stellt
danach ein neues Server-Leaf aus (siehe „Nach dem Zurückspielen“ unten) und
gibt die Dateien dem Besitzer des State-Verzeichnisses. Läuft der Server, lehnt
es ab. `latest` ist der neueste Snapshot außer `pre-restore`.

**Neuer Host (Disaster Recovery).** Paket installieren, den Dienst am besten
noch nicht starten, einen Snapshot-Ordner aus der Kopie außer Haus auf den Host
bringen, `sudo icu-server restore /pfad/zum/snapshot`, dann `sudo systemctl
enable --now icu-server`. `server_name` in `server.toml` weglassen oder auf den
Namen der CA setzen; `restore` lehnt einen anderen ab und nennt den richtigen.
Zeigt der DNS-Name (`public_host`) auf den neuen Host, verbinden sich alle
Clients wie vorher. Lief der Dienst schon einmal, hat er eine frische CA
angelegt; `restore` ersetzt sie, sie landet als `pre-restore`. Im Container:
Container stoppen, `restore` in einem Wegwerf-Container mit demselben Volume
und dem Snapshot als zweitem Mount, Container wieder starten.

**Von Hand** geht es weiterhin: die Dateien eines Snapshots zurück ins
State-Verzeichnis kopieren, dann wie unten `rotate-server-cert`.
`ca.crt`, `ca.key`, `com.key` und `users.toml` gehören
zusammen und kommen aus demselben Backup zurück, lesbar für den Dienst. Meldet
der Start `no CA in …: ca.crt and ca.key are both missing, but users.toml has
been in use`, fehlt die CA neben einem benutzten Nutzerbestand — ein
unvollständiger Restore, eine Migration ohne die CA: beide Dateien aus dem
Backup zurückholen. Eine neue CA sperrte jeden registrierten Nutzer aus,
deshalb legt der Server dann keine an. Dasselbe gilt für `no com key in …`:
`com.key` aus dem Backup zurückholen; einen neuen Schlüssel legt der Server nur
in einem nie benutzten State an. Umgekehrt ebenso: meldet der Start `no
users.toml in …, but ca.crt, ca.key, … are there`, fehlt der Nutzerbestand
neben CA und Com-Schlüssel — `users.toml` aus demselben Backup zurückholen.
Ein leerer Bestand an seiner Stelle kennte keinen Nutzer und keinen Widerruf
mehr, deshalb legt der Server dann keinen an (2026-09-27). Einzige harmlose
Ausnahme: ein State, in dem vor diesem Stand nur Offline-CLI-Kommandos
(etwa `status`) liefen und nie `run` — dort entstanden CA und `com.key` ohne
`users.toml`, registriert war aber niemand; das Verzeichnis beiseiteschieben
verliert nichts. Wer wirklich neu anfangen will (neue CA, alle registrieren sich
neu), schiebt das ganze State-Verzeichnis beiseite.

**Nach dem Zurückspielen eines Backups** von Hand `icu-server rotate-server-cert`
ausführen (`restore` macht das selbst). Lag zwischen Backup und Restore eine Rotation, präsentiert der Server
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
Reverse-Proxy und kein TLS davor, er terminiert QUIC/TLS 1.3 selbst.

**Der Port schweigt ohne Com-Schlüssel** (2026-09-27, nach dem Vorbild von OpenVPN `tls-auth`, ohne dessen Replay-Schutz,
`PROTOCOL.md` §1): jedes Datagramm trägt ein 16-Byte-HMAC-Tag aus `com.key`,
und was keins trägt, verwirft der Server, bevor QUIC es sieht — kein
Handshake, keine Version Negotiation, kein Stateless Reset. Ein Portscan
bekommt keine Antwort (nmap: `open|filtered`, nicht `closed`). `icu-server status` zählt die verworfenen
Datagramme (`com_dropped`). Der Schlüssel ist ein Gruppengeheimnis, keine
Zulassung: er steht in jedem Passport, auch ein widerrufener Nutzer kennt ihn
noch. Ein Client, dessen Passport einen anderen Schlüssel trägt, bekommt keine
Antwort und sieht dasselbe wie bei einem abgeschalteten Server.

Ohne gültiges Zertifikat der eigenen CA gibt es weder Raumzugang noch Medien.
Unbekannte Zertifikate scheitern an TLS; ohne Zertifikat darf TLS für die
lesbare Fehlermeldung `AUTH_REQUIRED` abschließen, danach wird sofort getrennt.
Dafür gilt
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

### Verschlüsselung, Qualität und Produktionsfreigabe

Audio, Video und Steuerung laufen ausschließlich über QUIC/TLS 1.3; 0-RTT ist
aus. Client und Server prüfen Zertifikate der Server-CA. Der Server ist ein
**vertrauenswürdiger Relay**: TLS endet dort, daher ist das keine
Ende-zu-Ende-Verschlüsselung zwischen Teilnehmern. Zugriff auf den Server oder
seine CA erlaubt Zugriff auf die Vertrauensgrenze des Raums.

Der Server transkodiert nichts und erhält die Codec-Nutzlast bytegenau.
Das aktuelle Client-Profil ist Mono-Opus mit 48 kHz, 48 kbit/s VBR, Komplexität
10 und FEC sowie H.264 mit 320×240 bei 15 fps und Ziel 150 kbit/s. Die Grenzen
von 400 Byte Audio pro Paket und 512 KiB Video pro Frame lassen Spitzen zu;
der Video-Missbrauchsdeckel beträgt 400 kbit/s über 2 s. Das ist das aktuelle
Sprach-/Overlay-Profil, keine HD-Videokonferenz. Höhere Auflösung oder echte
Ende-zu-Ende-Verschlüsselung brauchen abgestimmte Änderungen an Clients und
Protokoll; bloßes Anheben eines Serverlimits genügt nicht.

Die Serverprüfung und reproduzierbaren Tests stehen in
[`Research/server-production-gate-2026-09-26.md`](../Research/server-production-gate-2026-09-26.md).
Eine Freigabe des konkreten Internet-Hosts erfordert zusätzlich den Test von
außerhalb seines Netzes (IPv4/IPv6, Firewall/NAT, Wiederverbindung), ein
wiederherstellbares State-Backup und Messungen mit den vorgesehenen Clients.

## 3. Bedienen

Alle Kommandos außer `run` sprechen über `/run/icu/admin.sock` mit dem
laufenden Server; eine Änderung wirkt damit sofort und nicht erst nach einem
Neustart. Sie brauchen root oder Mitgliedschaft in der Gruppe `icu`: die Unit
läuft mit `Group=icu`, damit gehören `/run/icu` (0750) und der Socket (0660)
dieser Gruppe. `server.admin_socket_group` bleibt deshalb `icu`. Mehr als den
Socket brauchen sie nicht, auch ohne `server_name` in der Konfiguration (der
State unter `/var/lib/private` bleibt root vorbehalten). Wer nicht an den
Socket darf, bekommt das als Fehler mit Pfad und Ursache.

```bash
icu-server invite --ttl 24h --out bob.txt  # Registrierungs-Bundle, einmalig, 24 h gültig
icu-server users                           # Tabelle mit user_id, Cert, online
icu-server revoke alice                    # alle Certs sperren, sofort kicken
icu-server revoke id:7                     # ausdrücklich die user_id (name:… für den Namen)
icu-server status                          # Uptime, online, offene Verbindungen, Zähler
icu-server status --json                   # dasselbe für Monitoring
icu-server rotate-server-cert              # neues Server-Leaf derselben CA
icu-server renew-ca                        # CA-Zertifikat neu, selber Schlüssel
icu-server backup                          # State-Snapshot jetzt (sonst nach jeder Änderung von selbst)
icu-server backup --list                   # Snapshots anzeigen (root)
icu-server restore latest                  # Snapshot zurück, nur bei gestopptem Server (§2 „Backup und Restore“)
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

Nutzer gibt man per Name, `user_id` oder Fingerprint an. Ein Name darf nur
aus Ziffern bestehen; passt eine Angabe auf mehr als einen Nutzer (`revoke 1`
bei einem Nutzer namens „1“), lehnt der Server mit `AMBIGUOUS_USER` ab und
nennt beide — dann `id:<user_id>` oder `name:<name>` schreiben. Jede
Bestätigung nennt den getroffenen Nutzer: `revoked alice (user_id 3), closed 1
connection(s)`.

Läuft **kein** Server (Socket fehlt oder verweigert die Verbindung), erledigt
die CLI dieselbe Arbeit selbst und hält dabei `flock(/var/lib/icu/state.lock)`;
dafür braucht sie root. Was sie dabei schreibt, gehört danach dem Besitzer des
State-Verzeichnisses, mit genau dem vorgesehenen Modus, auch unter einer
restriktiven umask wie 027 (vorher blieben die Dateien root-eigen und 0640,
und der Dienst startete nicht mehr). `invite --out` schreibt die Datei für
den Aufrufer (0600). Läuft einer und der Socket fehlt, bricht die CLI mit
`cannot lock … (is a server running?)` ab — das ist Absicht, damit CLI und
Server einander nie eine Änderung überschreiben.

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
  --memory 1g --restart on-failure \
  -p 4711:4711/udp \
  -v icu-state:/var/lib/icu \
  -v ./server.toml:/etc/icu/server.toml:ro \
  icu-server
podman exec icu /icu-server invite --name alice
```

Die Härtung der systemd-Unit (Sandbox, `MemoryMax=1G`, `Restart=on-failure`)
gilt im Container nicht: Speichergrenze und Neustart setzt man dort selbst wie
oben, den Start mit dem Host etwa per Quadlet-Unit.

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

Die Gruppe `icu` ist volle Serververwaltung, kein Lesezugriff: wer den
Admin-Socket erreicht, kann auch einladen, sperren und widerrufen (§6). Einen
Socket nur für Monitoring gibt es nicht.

Er ruft `icu-server status --json` auf. `ICU_Server` liefert die
Momentanwerte `online`, `uptime`, `open_connections` und `handshakes` und aus
den Zählern Raten gegen den vorigen Lauf (`connects_per_s`,
`datagrams_in_per_s`, `datagrams_out_per_s`); der erste Lauf und der erste
nach einem Server-Neustart haben noch keine. Die Metriken `connections`,
`datagrams_in` und `datagrams_out` von vorher (Summen seit Start, als
Momentanwert gegraphed) gibt es nicht mehr. Den vorigen Lauf merkt sich der
Check in `$MK_VARDIR/icu_server.state` (sonst
`/var/lib/check_mk_agent/icu_server.state`, oder `ICU_CHECK_STATE`); ist das
nicht schreibbar, sagt der Text es. `ICU_Video` zeigt Video-Frames je Sekunde
an Zuschauer und die, die für zu langsame Zuschauer verworfen wurden
(`video_slow_receiver`), und geht auf WARN, wenn deren Anteil drei Läufe in
Folge mindestens 10 % beträgt (`ICU_SLOW_WARN_PCT`, `ICU_SLOW_WARN_RUNS`):
dann schaut jemand mit viel zu wenig Bandbreite zu. Ist der Server unten, geht
der Check auf CRIT, ebenso, wenn `status` nicht binnen 10 s antwortet
(`ICU_CHECK_TIMEOUT`; coreutils `timeout`): ein hängender Server hält so nicht
den ganzen Agenten auf. Die CLI selbst gibt nach 30 s ohne Antwort auf.
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
