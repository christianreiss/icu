# ICU Live-Test (Linux-Client gegen lokalen Server; Windows §7)

Stand 2026-09-26 (Pre-Release-Gate). Alles auf einem Rechner (KDE Plasma Wayland, PipeWire). Zwei Client-Instanzen laufen mit getrennten `--config-dir`, damit jede ihre eigene Identität (Zertifikat) und Einstellungen hat; je Konfigurationsverzeichnis läuft höchstens eine Instanz mit Fenster (ein zweiter Start holt nur das Fenster der laufenden nach vorn; `--headless` prüft das nicht).

Was sich ohne Hardware prüfen lässt, prüfen die Tests (`cargo test --workspace`); dieses Dokument ist für das, was Ohren, Augen und Hände braucht.

## 0. Bauen

```sh
cd ~/Documents/ICU
cargo build --release -p icu-client                   # Debug geht auch, dann target/debug/…
cargo build --profile release-server -p icu-server    # wie im Paket: ein Panic beendet nur eine Sitzung
```

Voraussetzungen auf dem Build-Host: `pipewire-devel`, `clang-devel`, `meson`, `ninja-build`, `abseil-cpp-devel`, `llvm`, `cmake`, `nasm`, `libxkbcommon-devel`, `wayland-devel` und der Symlink `/usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-objcopy` → `/usr/bin/llvm-objcopy` (`Client/IMPLEMENTATION.md` §12). Pakete (rpm/deb) baut `packaging/build.sh`.

## 1. Server lokal starten

Minimal-Config, z. B. `~/icu-live/server.toml`. Die Defaults `/var/lib/icu` und `/run/icu` sind als Nutzer nicht beschreibbar; **der Admin-Socket-Pfad muss kürzer als 108 Byte sein**, sonst startet der Server ohne Admin-Socket und `invite` gegen den laufenden Server schlägt fehl. `server_name` darf kein Beispielname (`icu.example.org`) sein — der Server lehnt ihn ab, weil er in die CA und jedes Zertifikat eingebrannt würde; `localhost` ist erlaubt.

```toml
[server]
listen = ["127.0.0.1:4711"]
server_name = "localhost"
public_host = "127.0.0.1:4711"

[paths]
state_dir = "/home/chris/icu-live/server"
runtime_dir = "/run/user/1000/icu-live"
```

```sh
mkdir -p ~/icu-live/server /run/user/1000/icu-live
./target/release-server/icu-server --config ~/icu-live/server.toml run
```

Erwartet: `icu-server listening addrs=[127.0.0.1:4711] max_users=16`, **keine** Warnung „admin socket unavailable“.

## 2. Einladungen erzeugen

In einem zweiten Terminal (Server läuft weiter):

```sh
./target/release-server/icu-server --config ~/icu-live/server.toml invite --name chris1 --out ~/icu-live/chris1.icu
./target/release-server/icu-server --config ~/icu-live/server.toml invite --name chris2 --out ~/icu-live/chris2.icu
```

Jede Datei ist ein **Registrierungs-Bundle** (`-----BEGIN ICU PASSPORT-----`, Modus 0600): kurzlebiges Registrierungszertifikat plus CA, Standard 24 h gültig (`--ttl`, höchstens 7 Tage). `--name` ist nur ein Platzhalter; der Eingeladene wählt seinen Namen selbst. Admin wird, wer als Erster seine Registrierung abschließt. Ohne `--out` steht das Bundle auf stdout.

## 3. Client 1 und 2 registrieren und verbinden

```sh
./target/release/icu ~/icu-live/chris1.icu --config-dir ~/icu-live/c1
./target/release/icu ~/icu-live/chris2.icu --config-dir ~/icu-live/c2 --no-hotkeys
```

Das Einladungsfenster öffnet schon ausgefüllt auf der Seite **Beitreten**: Server, Adresse, CA-Fingerabdruck, darunter das Namensfeld (hat den Fokus). Namen tippen, Enter oder „Beitreten“ — der Client erzeugt einen eigenen Schlüssel, holt sein Zertifikat und startet das Hauptfenster. Ohne Argument zeigt das Fenster ein Einfügefeld (Strg+V wirkt sofort, eine `.icu`-Datei kann aufs Fenster gezogen werden). Ein Doppelklick auf eine `.icu`-Datei öffnet ICU direkt: das Paket registriert den Dateityp `application/x-icu`, ein anders gestartetes Binary (etwa aus `target/`) trägt sich beim Start selbst unter `~/.local/share` ein, außer mit `--no-uri-handler`.

Nach dem Beitreten öffnet sich über dem Hauptfenster **„ICU — Einrichtung“** (nur beim ersten Start einer Konfiguration; `[ui] setup_done` in `settings.toml`). Solange sie offen ist, ist der Client stumm (mit Stumm-Ton), für den Hörtest unten also erst „Fertig“ oder „Überspringen“. Zu prüfen: Schrittleiste Geräte · Sprechen · Klang · Übersicht; im Schritt Geräte bewegt sich der Pegelbalken beim Sprechen, „Testen“ spielt die eigene Stimme; im Schritt Sprechen bei „Taste halten“ „Aufnehmen“ → Taste drücken, KDE fragt nach etwa 1,5 s gegebenenfalls erneut, danach leuchtet „Taste erkannt“, solange die Taste gehalten wird (auch mit einem anderen Fenster vorn), eine Maustaste unter dem Portal zeigt stattdessen den Hinweis auf Buchstaben-/F-Tasten oder evdev; bei „Automatisch“ färbt sich der Balken der Stimmerkennung über dem Schwellenstrich; im Schritt Klang sind die Schalter im Mikrofontest hörbar; die Übersicht springt mit „Ändern“ zurück; „Fertig“ hebt die Stummschaltung auf. Ein zweiter Start derselben Konfiguration zeigt die Einrichtung nicht mehr, ⚙ → Audio → „Assistent“ öffnet sie wieder; das Fenster zu schließen beendet sie wie „Überspringen“.

Ohne Fenster: `icu ~/icu-live/chris1.icu --name chris1 --yes --config-dir ~/icu-live/c1 --headless` (ohne `--yes` fragt das Terminal `[j/N]`).

`--no-hotkeys` bei Client 2: beide Instanzen würden sonst dieselben Tasten sehen und im PTT-Modus gleichzeitig senden. Alternativ in ⚙ → Tastenkürzel bei Client 2 eine andere Taste aufnehmen — Änderungen gelten sofort.

Beim ersten Start fragt KDE (Portal GlobalShortcuts) nach den Kurzbefehlen. Wer den Dialog abbricht, bekommt **nicht** stillschweigend evdev: ⚙ → Tastenkürzel bietet „Erneut fragen“ oder, mit Warnung, das Lesen über evdev an.

**Passport mit Einstellungen**: bei Client 1 ein paar Einstellungen ändern (Senden „Taste halten“, Farben „Dunkel“, die Lautstärke von Client 2), dann ⚙ → Allgemein → „Speichern“. `./target/release/icu ~/Downloads/icu-passport.icu --config-dir ~/icu-live/c3`: die Beitreten-Seite zeigt unter dem Passport „Einstellungen — Von dort übernehmen“ (an). Nach „Beitreten“ öffnet sich die Einrichtung (neuer Rechner), Sendemodus, PTT-Taste, Farben und die Lautstärke von Client 2 sind wie bei Client 1, Mikrofon und Lautsprecher stehen auf dem eigenen Standard. Dieselbe Identität verdrängt dabei Client 1 (`SESSION_REPLACED`); danach c3 schließen und bei Client 1 „Neu verbinden“. Mit dem Schalter aus, oder auf der Kommandozeile mit `--skip-settings`, bleibt c3 auf seinen Einstellungen.

### Was zu sehen sein soll

- **Hauptfenster** (400 px breit, Höhe passt sich nach dem Verbinden der Liste an): Kopf mit Servername, „Admin“ bei Admins, Verbindung als Punkt + Wort (Klick öffnet das Verbindungsfenster), Hell/Dunkel-Schalter, Zahnrad.
- **Teilnehmerliste**: eigene Zeile fett zuoberst, dann alphabetisch. Avatar mit Initialen (`chris1` → `C1`), blauer Ring + Balken = spricht (< 100 ms nach dem ersten Paket). Rechts feste Spalten: Abzeichen (taub, stumm, Kamera, Admin), Modus (Hand = PTT, Welle = Sprachaktivierung), Lautsprecher-Taste, Ping als Balken. Klick auf eine Zeile klappt Details und Lautstärke-Schieber auf, dazu die Ein-Klick-Taste „Für mich stummschalten“; bei 0 % zeigt die Zeile den Lautsprecher mit x. Admins haben per Rechtsklick das Moderationsmenü.
- **Leere Liste** sagt, warum: „Allein hier“, „Verbinde…“, „Getrennt — verbinde gleich neu“ oder „Nicht verbunden“. Allein zeigt die Sprechfläche „Allein“ und bleibt dunkel, auch mit gedrückter Taste oder bei Stimme; es wird weder Audio noch Video gesendet (Verbindungsfenster → Audio: „Pakete ↑“ bleibt stehen), kein Sende-Cue. Tritt jemand bei, sendet ein offenes Gate sofort.
- **Dock** unten: Sprechfläche (PTT: „Halten“ mit der Taste als Tastenkappe; Sprachaktivierung: Sprachwahrscheinlichkeit mit Schwellenstrich) neben der PTT/VAD-Kachel, darunter Stumm / Taub / Kamera / Overlay. Die Overlay-Kachel fehlt auf Arbeitsumgebungen ohne Layer-Shell (GNOME).
  - Stumm und trotzdem sprechen: die Fläche pulsiert rot „Stumm — du sprichst“. Der Pegel läuft auch stumm gedimmt weiter; Übersteuerung (Spitze ≥ −1 dBFS) färbt die Linie rot.
- **Fehler**: „Keine Verbindung“ als Banner mit „Neu verbinden“ und „Passport einspielen“; ab dem zweiten Wiederholversuch „Verbindung unterbrochen“.
- **Tray** (ksni): grau = getrennt, grün = verbunden, orange = stumm, rot = taub; Menü „Fenster anzeigen / Stumm / Taub / Beenden“. Fenster schließen nach ⚙ → Allgemein „Beim Schließen“: Infobereich (X11 mit Tray-Host: versteckt; Wayland oder ohne Host, z. B. GNOME ohne AppIndicator: minimiert, die Seite sagt das), Minimieren (Taskleiste/Dock), Beenden; Shift+Schließen, Strg+Q, ⏻ im Kopf oder Tray → Beenden beendet immer. Das Symbol folgt auch bei verstecktem Fenster (Hotkey-Stumm, Verbindungsabbruch).
- **Einrichtung** (600×720, beim ersten Start): vier Schritte ohne Scrollbalken, unten „Zurück“/„Weiter“ bzw. „Überspringen“ und „Fertig“; währenddessen steht im Hauptfenster „Stumm“.
- **Einstellungen** (⚙, 820×680, öffnet auf Audio, dort im Kopf „Assistent“): Audio, Klang, Tastenkürzel, Kamera, Allgemein, Nutzer (nur Admins), Über ICU — keine Seite braucht einen Scrollbalken; mit „Erweitert“ (unten in der Seitenleiste) wächst das Fenster, statt zu scrollen. Eine Änderung per Hotkey oder Tray, während das Fenster offen ist, bleibt erhalten.
- **Verbindungsfenster** (Klick auf den Verbindungspunkt): Server, Netz, Krypto (Zertifikate Server/Eigenes/CA), Audio, Video, Wer; nur lesend, „Bericht kopieren“.
- **Overlay** (Kachel „Overlay“, KDE/wlroots): Kamerakacheln mit Namen, und wer ohne Kamera spricht, erscheint solange als Namensschild; klick-durchlässig, Position in ⚙ → Kamera.

### Was zu hören sein soll

- Client 1 steht ab Werk auf Sprachaktivierung (Kachel „VAD“): sprechen → Client 2 gibt die Stimme aus. Nach einem Klick auf die Kachel („PTT“) sendet er nur, solange die PTT-Taste (⚙ → Tastenkürzel zeigt sie; „Aufnehmen“ übernimmt die nächste gedrückte Taste oder Maustaste) oder die Sprechfläche gedrückt ist. Beide Clients laufen auf demselben Default-Mikrofon und -Sink — für einen echten Hörtest bei Client 2 in ⚙ → Audio ein anderes Gerät wählen (wirkt sofort) oder Client 2 stummschalten (taub schaltet auch seine Wiedergabe ab).
- ⚙ → Audio, „Testen“ neben der Mikrofon-Auswahl (**Mikrofontest**): die eigene Stimme so hören, wie die anderen sie bekommen (nach Rauschunterdrückung, Pegelregelung und Echo-Unterdrückung); schaltet nach 60 s selbst ab.
- Hinweise (⚙ → Audio): Töne, wenn jemand kommt/geht (an), Senden beginnt/endet (aus), beim Stummschalten (an); „Meldung bei Kommen und Gehen“ als Desktop-Benachrichtigung (aus).
- Echo-Unterdrückung ist an (AEC3). ⚙ → Klang zeigt einen Klartext-Hinweis, wenn sie wirkungslos ist; mit „Erweitert“ die ERL/ERLE-Werte.

## 4. Nur Netz/Audio prüfen (ohne Fenster, ohne Mikrofon)

```sh
./target/release/icu --config-dir ~/icu-live/c1 --headless --no-hotkeys --test-tone --debug-stats
./target/release/icu --config-dir ~/icu-live/c2 --headless --no-hotkeys --debug-stats
```

Vorher die Fenster-Instanzen aus Abschnitt 3 beenden (Tray → Beenden), sonst verliert die ältere Sitzung desselben Zertifikats ihre Verbindung (`SESSION_REPLACED`). Client 1 sendet ein synthetisches Sprachsignal (Gate offen; Rauschunterdrückung, AEC, „Pegel automatisch“, „Selbst gleich laut ankommen“ und Stimmoptimierung aus), Client 2 spielt es auf dem Default-Sink aus. `--camera synthetic` schickt zusätzlich ein Testbild (15 fps H.264, `video_tx` zählt hoch); `--camera 0` nimmt die echte Kamera.

## 5. `--debug-stats` lesen (alle 2 s auf stderr)

Beispiel (Debug-Build, Sender- und Hörerwerte in einer Zeile; die `level`-Zeile gibt es nur mit `--headless`):

```
icu stats: sent=498 recv=500 bad=0 decoded=499 fec=0 plc=0 played=480960 play_underruns=0 jb_late=0 jb_underruns=0 jb_stretched=0 jb_skipped=0 jb_dropped=0 jb_lost=0 dsp_blocks=997 ref_underruns=1 cap_overruns=0 ref_overruns=0 cap_backlog=0 loss_perc=0 rtt=0.40 ms reconnects=0 video_tx=0 video_rx=0 idr_forced=0 video_tier=0 video_tier_changes=0 video_open_uni_drops=0
  dsp block      n=999 p50<1.5 p95<1.5 p99<2.0 mean=1.17 max=1.92 ms
  jitter delay   n=499 …
  capture→send   n=0 …
  level=-100.0 dBFS vad=0.00 sending=false peers=[chris1*]
```

| Feld | Bedeutung | Soll |
| --- | --- | --- |
| `sent` / `recv` | Audio-Datagrams (50/s bei 20-ms-Frames, nur während Sendung) | wächst um ~100 je 2 s beim Sprecher bzw. Hörer |
| `decoded` / `fec` / `plc` | Frames aus echten Paketen / aus FEC / per Verschleierung | im LAN `fec=plc=0` |
| `played` | Samples an die Wiedergabe (48 000/s) | wächst um ~96 000 je 2 s |
| `play_underruns` | Wiedergabe-Callback fand den Puffer zu leer | 0 |
| `jb_late` / `jb_underruns` / `jb_stretched` / `jb_skipped` / `jb_dropped` / `jb_lost` | Jitter-Puffer: zu spät gekommen / leer gelaufen / zum Vertiefen verdeckt / zum Abbau übersprungen / nach Netzhänger verworfen / als verloren abgeschrieben | im LAN 0; `jb_underruns` ist hörbares Stottern |
| `dsp_blocks` | 10-ms-Blöcke der Capture-Kette | ~200 je 2 s |
| `ref_underruns` / `ref_overruns` | AEC-Referenz kam zu spät bzw. lief über | einzelne beim Start oder Gerätewechsel, sonst 0 |
| `cap_overruns` / `cap_backlog` | Capture-Ring voll bzw. nach einem Hänger übersprungene Samples | 0 |
| `loss_perc` | `PACKET_LOSS_PERC` aus `LossHint` | 0 im LAN |
| `rtt` | Ping/Pong | < 1 ms Loopback |
| `video_*` / `idr_forced` | gesendete/empfangene Bilder, Qualitätsstufe (0 = voll, 4 = pausiert), ihre Wechsel, `open_uni`-Zeitüberschreitungen; erzwungene IDR | Stufe 0 im LAN |
| `dsp block` | Zeit je 10-ms-Block (AEC + RNNoise + Opus) | Debug ≈ 1,2 ms, Release ≈ 0,15 ms |
| `jitter delay` | Ankunft → Ausspielen | Testton (ein einziger Spurt, Startziel 40 ms) ≈ 40–50 ms; Sprache im LAN nach der ersten Pause ≈ 25–35 ms |
| `capture→send` | Capture-Block → `send_datagram` | < 2 ms |
| `level` / `vad` / `sending` / `peers` | nur `--headless`: Mikrofonpegel (RMS), Sprachwahrscheinlichkeit (0 ohne Rauschunterdrückung, also mit `--test-tone`), sendet gerade, Teilnehmer (`*` = spricht) | beim Hörer `peers=[chris1*]`, solange Client 1 sendet |

## 6. Was dieser Test (noch) nicht abdeckt

- **Zwei Rechner, echtes WLAN/WAN**: Mund-zu-Ohr-Latenz, Jitter-Puffer unter echtem Jitter (`tc netem` 30 ms ± 10 ms, 2 % Verlust), Keepalive über Consumer-NAT. Die Tests simulieren Jitter nur auf Loopback.
- **Akustisches Echo**: ERLE mit echtem Lautsprecher und Mikrofon, Drift über 10 min (`Research/aec.md` A2/A5).
- **Interaktiv**: Portal-Dialog auf KDE (inkl. Maustasten mit „Listening for mouse buttons“), Overlay über einem Borderless-Spiel und seine Klick-Durchlässigkeit, Tray-Menü.
- **Kamera**: `/dev/video0` (C270) braucht ~3 s bis zum ersten Bild (Auto-Belichtung); bei wenig Licht 4–6 fps statt 15.
- **Windows**: implementiert (2026-09-26), auf der Windows-VM geprüft; verbleibende Hardware-Checks in §7.
- Der Debug-Build braucht ~1,2 ms je 10-ms-Block; für Latenzmessungen den Release-Build nehmen.
- **Seit 2026-09-27 nur per Test belegt, am Gerät offen**: Kamera ohne 320×240-MJPEG (reine YUYV-Kamera, nur 640×480) öffnet über den Formatrückfall; `video_tx_kbps` je Stufe bei echten 15 fps (T0 150, T1 100, T2 60, T3 40); Anzeige der vom Portal vergebenen PTT-Taste nach dem KDE-Dialog; Klick aufs Tray-Symbol unter KDE und GNOME mit AppIndicator; Leeren der Zwischenablage 60 s nach „Passport kopieren“ unter nativem Wayland; Banner „kein Audiosystem“ bei gestopptem `pipewire.socket` (Probe: `PIPEWIRE_REMOTE=icu-no-such-daemon cargo test -p icu-client --lib a_failed_connect -- --ignored`).
- **Seit dem Review 2026-09-27 (C-3, C-7), nur per Test belegt**: „Passport kopieren“ und eine frisch erstellte Einladung unter KDE/X11 erscheinen nicht in Klippers Verlauf (`x-kde-passwordManagerHint`) und sind nach 60 s aus der Zwischenablage; unter KDE/GNOME mit nativem Wayland sagt der Status „kann ICU sie nicht sicher wieder leeren“; ein Passport mit anderen Sendeeinstellungen listet die Änderungen im Erststart-Fenster, im Einstellungsfenster und bei `icu datei.icu --name x` im Terminal.
- **Video über WAN**: je Sender/Zuschauer höchstens zwei unbestätigte Frames (`VIDEO_UNACKED_PER_PAIR`); ein Zuschauer mit mehr als ~130 ms RTT bekommt Lücken und danach nur Keyframes. Mit einem ratenbegrenzten, weit entfernten Zuschauer (`tc netem rate 128kbit delay 80ms`) Bildrate und Verzögerung messen.
- **Review-Runde 3 (2026-09-27), nur per Test belegt, am Gerät offen**: großes Graph-Quantum (`pw-metadata -n settings 0 clock.force-quantum 8192`, danach wieder `0`) — `cap_backlog` in `--debug-stats` bleibt 0, die Stimme kommt vollständig an, `play_underruns` bleibt 0 und die Wiedergabe-Verzögerung fällt nach dem Zurückstellen binnen Sekunden; stummgeschaltet beenden → der nächste Start ist vom ersten Bild an stumm (auch beim Gegenüber), nicht stumm beenden → nicht stumm, mit offener Einrichtung beenden → der nächste Start ist nicht stumm; Sprechfläche im Fenster halten und das Fenster minimieren (Super+D) → das Senden endet sofort; Fenster im Tray, Kamera per Hotkey aus → beim Gegenüber kommt kein Bild mehr (`video_sent` steht); Tray-Tooltip nach einem fatalen Verbindungsende „ICU — Fehler“ statt „verbinde…“.

## 7. Windows

Stand 2026-09-26: der Windows-Client ist gebaut (`packaging/windows/build.sh` → `dist/<ver>/icu-<ver>-windows-x86_64.zip`, Features `aec,gui,camera,tray,overlay`) unter Wine 11 getestet (Unit-Tests 216/217, der eine an Wines `WSAENOPROTOOPT` für quinn-Sockets) und am 2026-09-26 erstmals auf echtem Windows gelaufen: VM `win10-test` (Windows 10 Pro N 22H2, Build 19045.6456, QEMU mit HDA-Audio über SPICE, Autologon Buffies, gestartet per `schtasks /it` in der Desktop-Sitzung) gegen einen Server auf dem Linux-Host plus einen Linux-Client `--headless --test-tone`. Der ergänzende native Gate-Lauf ergab 232 bestandene Tests, keine Fehler und vier ignorierte Tests (`Research/windows-gate-2026-09-26.md`). „VM ✓“ = dort belegt; was echte Hardware braucht, ist „offen, Hardware“. Server wie §1, aber auf einer Adresse, die die VM erreicht.

- [x] **Start** (Windows 10/11 VM ✓; Windows 11: Edge-Download, Explorer-Entpacken mit ZoneId=3, SmartScreen „Weitere Informationen“/„Trotzdem ausführen“): ZIP entpacken, `icu.exe` starten; kein Konsolenfenster; Log unter `%LOCALAPPDATA%\icu\data\icu.log`, aus `cmd` gestartet Log in der Konsole (`AttachConsole`).
- [x] **Registrierung** (VM ✓ über `icu.exe <datei>.icu`, Name tippen, Beitreten; Dateizuordnung in HKCU geschrieben; Doppelklick im Explorer und Icon offen): Einladung per Doppelklick auf die `.icu`-Datei → Erststart-Fenster vorbelegt; Dateizuordnung unter `HKCU\Software\Classes\.icu`/`ICU.Invite` mit Icon im Explorer.
- [x] **Eine Instanz** (VM ✓: zweiter Start übergibt genau einmal und endet; zunächst 30 Übergaben, weil `DisconnectNamedPipe` das `ok` verwarf — behoben): zweiter Start holt das Fenster nach vorn (auch aus dem Tray), Doppelklick auf eine `.icu`-Datei bei laufendem ICU landet im offenen Fenster.
- [x] **DPAPI** (VM ✓ `ICU-DPAPI/1` und Passport mit Einstellungen exportiert/importiert; Altbestand offen): `identity\key.pk8` beginnt mit `ICU-DPAPI/1`; Passport-Export/-Import funktioniert; eine alte, unversiegelte `key.pk8` wird gelesen und beim nächsten Speichern versiegelt.
- [x] **WASAPI** (VM ✓: Mikrofon „Line In“ und Loopback über `AUTOCONVERTPCM`, Wiedergabe `IAudioClient3` mit 256 Frames; Stimme des Linux-Clients kommt an, Sprechanzeige; Debug-Stats geprüft; echtes akustisches Gespräch offen): Geräteliste; Periode im Log (IAudioClient3 bei float32 @ 48 kHz: 240 Frames bzw. nächstes Vielfaches; sonst `AUTOCONVERTPCM` mit 20 ms, z. B. Gerät auf 44,1 kHz stellen); `--debug-stats` wie §5 (`play_underruns=0`, `cap_overruns=0`); zwei Clients sprechen miteinander.
- [x] **Hot-Plug** (Audio-Dienst-Neustart, C270-Mikrofon und Jabra-Ausgabe mit Rückfall auf HDA und Rückkehr VM ✓): Standardgerät in den Windows-Einstellungen wechseln → „Standard“-Streams folgen; USB-Headset ziehen/stecken; fest gewähltes Gerät ziehen → kein Absturz; Ausweichgerät und Rückkehr im Log prüfen.
- [ ] **Ohne Mikrofon** (seit 2026-09-27, offen, Hardware): Desktop ohne Mikrofon, oder alle Aufnahmegeräte in den Soundeinstellungen deaktiviert → Banner „Das Mikrofon lässt sich nicht verwenden …“, die Wiedergabe läuft ohne Lücke weiter (`play_underruns` bleibt 0, im Log kein „reopening audio streams“, nur alle ≤ 30 s ein Debug-Versuch „microphone still does not open“). Mikrofon anstecken bzw. aktivieren → „microphone opened on a retry“, das Banner geht, die Wiedergabe bleibt ungestört. Stoppen und Starten des Audio-Dienstes (unten) nimmt weiter alle drei Streams wieder auf.
- [ ] **AEC-Referenz** (VM: Loopback liefert; in der VM stockt es bis ~35 ms und liefert dann gesammelt — ohne Toleranz „AEC reference dropped out“ alle 10 s, mit `reference_slack` 40 ms in 60 s keiner; Auslöschung selbst offen): Loopback des Ausgabegeräts liefert (`ref_underruns` nur beim Start); Spielton/Musik über Lautsprecher verschwindet aus dem gesendeten Signal; ERL/ERLE unter Erweitert plausibel.
- [ ] **A6 Drift** Loopback ↔ Mikrofon über 10 min in ppm (offen, Hardware; `Research/aec.md` A6).
- [ ] **MMCSS** (VM: `real-time priority acquired` für alle vier Threads; Stottern unter Last offen): Engine-Threads in der Aufgabe „Pro Audio“ (z. B. Process Explorer), kein Stottern unter CPU-Last.
- [x] **Raw Input** (VM ✓: F12 als PTT bei fokussiertem Notepad sendet; winit nimmt Raw Input nach dem Fensterstart, die Rückholung nach 2 s greift; Pause-Press/Release und importiertes CTRL+SHIFT+F12 VM ✓; Maustasten offen): PTT/Mute/Deafen mit fokussiertem anderem Fenster; „Aufnehmen“ erkennt Tasten, Pause, Seitentasten der Maus (`BTN_SIDE`/`BTN_EXTRA`); kein Auto-Repeat; Tasten bleiben nach dem Öffnen weiterer Fenster wirksam (Neuregistrierung gegen winit).
- [ ] **Raw Input im Spiel** mit Gaming-Maus bei fokussiertem Borderless-Spiel (offen, Hardware).
- [ ] **UIPI** (VM ✓ mit erhöhtem Notepad, Vordergrund-PID und Integritätsstufen geprüft; Spiel offen): Vordergrund-App als Administrator → normaler Raw-Input-Test empfängt keine Tasten; Test ebenfalls erhöht → F12/Pause inklusive Loslassen kommen an.
- [ ] **PTT schließt ohne Loslassen** (seit 2026-09-27, `hotkey/held.rs`; Unit-Tests auch nativ auf Windows 11, am Desktop offen): jeweils PTT (Taste und Maustaste) gedrückt halten, dann (a) ein als Administrator gestartetes Fenster (Notepad, Spiel) in den Vordergrund holen, (b) Win+L drücken, (c) eine UAC-Abfrage auslösen, (d) eine Funkmaus bzw. -tastatur ausschalten oder den Empfänger ziehen → Sprechanzeige geht sofort aus, Log `raw input: keys let go without a release` mit Grund; zurück im normalen Fenster bzw. nach dem Entsperren startet ein neuer Druck PTT (wird nicht als Auto-Repeat verschluckt). Gegenprobe: Fenster gleicher Stufe (Explorer, Browser) und Alt-Tab mit gehaltener Taste beenden PTT nicht. Maustaste länger als 5 min halten → PTT endet nach 5–5:02 min.
- [x] **Overlay** (VM ✓ auf dem Desktop: Namensschild oben rechts mit Sprechrahmen; Skalierung ≠ 100 % offen): klick-durchlässig, bleibt oben, Ecke/Offset im Arbeitsbereich des primären Monitors, auch bei Skalierung ≠ 100 %; über einem Borderless-Spiel (offen, Hardware).
- [ ] **Toast** (VM ✓ sichtbarer Join-Toast mit Absender ICU; Ersetzen offen): „Meldung bei Kommen und Gehen“ an → Toast mit Absender „ICU“, ein neuer ersetzt den vorigen, kein Systemton.
- [x] **Tray** (VM ✓ Menü/Stumm/Taub auch bei verstecktem Fenster, Doppelklick, Vordergrund-Wiederherstellung, minimierter Start und alle drei Schließen-Modi; Windows 11: Abmelden/Passport erneut einspielen im selben Prozess, danach Stumm/Taub/Doppelklick/Beenden ✓): Icon-Varianten, Menü, Doppelklick, „Beim Schließen“ Infobereich/Minimieren/Beenden.
- [ ] **Kamera** (C270: 1.856 dekodierte Frames in 150 s, USB-Wiederaufnahme nach 18 s VM ✓; GUI-Vorschau öffnet; Bildqualität bei normalem Licht offen): MediaFoundation-Kamera, Vorschau, Bild beim Gegenüber.
- [x] **Beenden** (VM ✓ Abmelden: `session ends, quitting`, Server `left reason=NO_ERROR` 0,9 s später; Herunterfahren und Strg+C offen): Abmelden/Herunterfahren mit laufendem ICU → sauberes Ende (letzte Einstellung gespeichert), kein „Diese App verhindert das Herunterfahren“; Strg+C in der Konsole.

Bekannte Abweichungen von Linux (Paritäts-Audit 2026-09-26), gezielt prüfen:

- [ ] **AEC-Paarung** (offen, Hardware): `dsp::refalign` erwartet Mikrofon und Referenz im selben Graph-Zyklus (PipeWire `node.group`); unter WASAPI laufen Capture (Event) und Loopback (2-ms-Abfrage) unabhängig; das Backend schiebt das Mikrofon deshalb je Geräteperiode (≥ 480 Samples) und nennt 40 ms `reference_slack`. ERLE und Verzögerung messen, auch direkt nach einem Wechsel des Standardgeräts (Gerätewechsel öffnen jetzt die gesamte Stream-Gruppe mit AEC-Neustart über die Engine neu; Regressionstest vorhanden).
- [x] **Loopback ohne Wiedergabe**: bei fehlgeschlagener WASAPI-Wiedergabe wird die Referenz geschlossen und die DSP wartet nicht auf einen stillstehenden Loopback; das Öffnen wird mit Backoff wiederholt.
- [x] **Tastenkombinationen**: Windows wertet einen Portal-Trigger aus, wenn `key` leer ist; Modifier- und Release-Semantik durch Regressionstests geprüft, Desktop-Prüfung im Gate-Bericht.
- [ ] **Standardgerät**: gefolgt wird der Rolle „Standardgerät“ (eConsole), nicht „Standardkommunikationsgerät“.
- [ ] **Seit 2026-09-27, nur per Test belegt**: Symbol in Taskleiste, Alt-Tab, Explorer und an `.icu`-Dateien, Versionsinfo in den Eigenschaften; Dialog „Zugangsdaten verwerfen?“, wenn DPAPI den Schlüssel nicht entsiegelt (Profil auf einen anderen PC kopieren); „Aufnehmen“ einer Kombination (Strg+Umschalt+F12) und eines einzelnen Modifiers (Rechts-Strg); Stimme nur vorne bei einem auf 5.1 gestellten Ausgabegerät; Overlay bei 150 % und 200 % so groß wie unter Linux; ältere Version im Tray + neue Exe starten → Meldung „laufendes ICU beenden“.
- [ ] **Zwischenablage** (seit dem Review 2026-09-27, C-7, offen): „Passport kopieren“ und eine frisch erstellte Einladung tauchen nicht im Win+V-Verlauf auf und werden nicht in die Cloud-Zwischenablage synchronisiert (Einstellungen → System → Zwischenablage: Verlauf und Synchronisierung vorher einschalten); nach 60 s ist der Text aus der Zwischenablage. Der Hinweis an „Speichern“ sagt nicht „nur für dich lesbar“.
- [ ] **Abmelden**: `WM_ENDSESSION` hält bis zu 5 s, seit 2026-09-27 mit `ShutdownBlockReasonCreate` („ICU meldet sich vom Server ab …“); `taskkill /PID <pid>` ohne `/F` beendet ICU samt Abmeldung beim Server wie SIGTERM, danach verabschiedet sich ein neu gestartetes ICU beim Abmelden weiterhin (Server-Log `left reason=NO_ERROR`).


### Ergänzender Gate-Lauf auf 10.2.0.201

`Research/windows-gate-2026-09-26.md` enthält die Ergebnisse des aktuellen
Quellstands, Artefakt-Hash und Grenzen. Die Desktop-Tests laufen als
`buffies` in Sitzung 1 über interaktive geplante Aufgaben; SSH allein läuft
in einer anderen Sitzung und ersetzt keinen Desktop-Test.

Native Tests auf Linux cross-bauen und auf Windows ausführen:

```sh
PATH="$PWD/build/win-aec:$PATH" cargo test --locked -p icu-client \
  --target x86_64-pc-windows-gnu --lib --no-run
```

Die ausgegebene Test-EXE auf Windows mit `--test-threads=2` starten.
Drei opt-in Tests benötigen die interaktive Windows-Sitzung:

- `live_wasapi_capture_playback_and_loopback --ignored --nocapture`: drei Sekunden leiser Prüfton;
  Capture, Wiedergabe und nicht-stille Loopback-Referenz müssen liefern.
- `live_wasapi_detects_stalled_streams --ignored --nocapture`: stoppt die
  drei Test-Streams ohne Endpoint-Verlust; Capture/Playback müssen den
  Stillstand melden, ein ruhender Loopback darf offen bleiben.
- `live_raw_input --ignored --nocapture`: innerhalb von acht Sekunden F12
  und Pause auf einer echten/virtuellen Tastatur drücken und loslassen;
  beide Übergänge müssen ankommen (SendInput ist kein Raw-Input-Gerät).

Zusätzlich prüfen: Audio-Dienst fünf Sekunden stoppen und wieder starten
(nur Testmaschine), Wiederaufnahme aller drei Streams und Verschwinden der
alten Audiofehlermeldung; Setup bei 1024×768 vollständig durchgehen;
Passport exportieren und in einem getrennten Konfigurationsordner importieren.

Konsolen-Regression (echte Windows-Konsole, keine Wine-Abnahme):

```sh
PATH="$PWD/build/win-aec:$PATH" cargo test --locked -p icu-client \
  --target x86_64-pc-windows-gnu --test windows_console --no-run
```

Auf Windows `ICU_TEST_EXE` auf die zu prüfende `icu.exe` setzen und die
ausgegebene Test-EXE starten. Geprüft werden `--help > Datei`, Fehler nach
`2> Datei`, `> NUL` und `--version | findstr`.

Physische Kamera vom Windows-Client (`--headless --camera 0`) an eine eigene
Linux-Testidentität senden. Der Empfänger zählt erfolgreich dekodierte
Bilder, speichert aber keine Kameraaufnahmen:

```sh
cargo run --locked -p icu-client --example video_receive_probe -- CONFIG_DIR 150
```

Währenddessen USB-Kamera für 18 Sekunden abziehen und wieder anschließen;
Frame-Zähler muss nach der Pause weiterlaufen und die Verbindung bestehen
bleiben. `PASS` allein bestätigt frische Bilder am Ende; die Wiederaufnahme
zusätzlich am Verlauf und am Sender prüfen.

Drift-Probe auf Windows mit zwei unterschiedlichen physischen Endpunkten:

```sh
PATH="$PWD/build/win-aec:$PATH" cargo build --locked -p icu-client \
  --target x86_64-pc-windows-gnu --example wasapi_drift_probe
```

`wasapi_drift_probe.exe C270 Jabra 600` wählt Eingabe/Ausgabe nach eindeutigem
Namensbestandteil. Die Probe gibt nur Sample-Zähler aus und spielt Stille;
sie speichert kein Mikrofon-Audio. Ein Stillstand oder mehr als 100 ppm
Abweichung der gelieferten Samples lässt den Lauf fehlschlagen. Verlorene
Pakete und VM-Stalls können diese Messung verfälschen: ein abgebrochener
Lauf ist keine Messung des Hardware-Taktdrifts.
