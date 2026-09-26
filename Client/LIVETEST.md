# ICU Live-Test (Linux-Client gegen lokalen Server)

Stand 2026-09-26 (Pre-Release-Gate). Alles auf einem Rechner (KDE Plasma Wayland, PipeWire). Zwei Client-Instanzen laufen mit getrennten `--config-dir`, damit jede ihre eigene Identität (Zertifikat) und Einstellungen hat; je Konfigurationsverzeichnis läuft höchstens eine Instanz mit Fenster (ein zweiter Start holt nur das Fenster der laufenden nach vorn; `--headless` prüft das nicht).

Was sich ohne Hardware prüfen lässt, prüfen die Tests (`cargo test --workspace`); dieses Dokument ist für das, was Ohren, Augen und Hände braucht.

## 0. Bauen

```sh
cd ~/Documents/ICU
cargo build --release -p icu-server -p icu-client      # Debug geht auch, dann target/debug/…
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
./target/release/icu-server --config ~/icu-live/server.toml run
```

Erwartet: `icu-server listening addrs=[127.0.0.1:4711] max_users=16`, **keine** Warnung „admin socket unavailable“.

## 2. Einladungen erzeugen

In einem zweiten Terminal (Server läuft weiter):

```sh
./target/release/icu-server --config ~/icu-live/server.toml invite --name chris1 --out ~/icu-live/chris1.icu
./target/release/icu-server --config ~/icu-live/server.toml invite --name chris2 --out ~/icu-live/chris2.icu
```

Jede Datei ist ein **Registrierungs-Bundle** (`-----BEGIN ICU PASSPORT-----`, Modus 0600): kurzlebiges Registrierungszertifikat plus CA, Standard 24 h gültig (`--ttl`, höchstens 7 Tage). `--name` ist nur ein Platzhalter; der Eingeladene wählt seinen Namen selbst. Admin wird, wer als Erster seine Registrierung abschließt. Ohne `--out` steht das Bundle auf stdout.

## 3. Client 1 und 2 registrieren und verbinden

```sh
./target/release/icu ~/icu-live/chris1.icu --config-dir ~/icu-live/c1
./target/release/icu ~/icu-live/chris2.icu --config-dir ~/icu-live/c2 --no-hotkeys
```

Das Einladungsfenster öffnet schon ausgefüllt auf der Seite **Beitreten**: Server, Adresse, CA-Fingerabdruck, darunter das Namensfeld (hat den Fokus). Namen tippen, Enter oder „Beitreten“ — der Client erzeugt einen eigenen Schlüssel, holt sein Zertifikat und startet das Hauptfenster. Ohne Argument zeigt das Fenster ein Einfügefeld (Strg+V wirkt sofort, eine `.icu`-Datei kann aufs Fenster gezogen werden). Ein Doppelklick auf eine `.icu`-Datei öffnet ICU direkt: das Paket registriert den Dateityp `application/x-icu`, ein anders gestartetes Binary (etwa aus `target/`) trägt sich beim Start selbst unter `~/.local/share` ein, außer mit `--no-uri-handler`.

Ohne Fenster: `icu ~/icu-live/chris1.icu --name chris1 --yes --config-dir ~/icu-live/c1 --headless` (ohne `--yes` fragt das Terminal `[j/N]`).

`--no-hotkeys` bei Client 2: beide Instanzen würden sonst dieselben Tasten sehen und im PTT-Modus gleichzeitig senden. Alternativ in ⚙ → Tastenkürzel bei Client 2 eine andere Taste aufnehmen — Änderungen gelten sofort.

Beim ersten Start fragt KDE (Portal GlobalShortcuts) nach den Kurzbefehlen. Wer den Dialog abbricht, bekommt **nicht** stillschweigend evdev: ⚙ → Tastenkürzel bietet „Erneut fragen“ oder, mit Warnung, das Lesen über evdev an.

### Was zu sehen sein soll

- **Hauptfenster** (400 px breit, Höhe passt sich nach dem Verbinden der Liste an): Kopf mit Servername, „Admin“ bei Admins, Verbindung als Punkt + Wort (Klick öffnet das Verbindungsfenster), Hell/Dunkel-Schalter, Zahnrad.
- **Teilnehmerliste**: eigene Zeile fett zuoberst, dann alphabetisch. Avatar mit Initialen (`chris1` → `C1`), blauer Ring + Balken = spricht (< 100 ms nach dem ersten Paket). Rechts feste Spalten: Abzeichen (taub, stumm, Kamera, Admin), Modus (Hand = PTT, Welle = Sprachaktivierung), Lautsprecher-Taste, Ping als Balken. Klick auf eine Zeile klappt Details und Lautstärke-Schieber auf, dazu die Ein-Klick-Taste „Für mich stummschalten“; bei 0 % zeigt die Zeile den Lautsprecher mit x. Admins haben per Rechtsklick das Moderationsmenü.
- **Leere Liste** sagt, warum: „Allein hier“, „Verbinde…“, „Getrennt — verbinde gleich neu“ oder „Nicht verbunden“.
- **Dock** unten: Sprechfläche (PTT: „Halten“ mit der Taste als Tastenkappe; Sprachaktivierung: Sprachwahrscheinlichkeit mit Schwellenstrich) neben der PTT/VAD-Kachel, darunter Stumm / Taub / Kamera / Overlay. Die Overlay-Kachel fehlt auf Arbeitsumgebungen ohne Layer-Shell (GNOME).
  - Stumm und trotzdem sprechen: die Fläche pulsiert rot „Stumm — du sprichst“. Der Pegel läuft auch stumm gedimmt weiter; Übersteuerung (Spitze ≥ −1 dBFS) färbt die Linie rot.
- **Fehler**: „Keine Verbindung“ als Banner mit „Neu verbinden“ und „Passport einspielen“; ab dem zweiten Wiederholversuch „Verbindung unterbrochen“.
- **Tray** (ksni): grau = getrennt, grün = verbunden, orange = stumm, rot = taub; Menü „Fenster anzeigen / Stumm / Taub / Beenden“. Fenster schließen = ins Tray, Shift+Schließen oder Tray → Beenden beendet. Das Symbol folgt auch bei verstecktem Fenster (Hotkey-Stumm, Verbindungsabbruch).
- **Einstellungen** (⚙, 820×680, öffnet auf Audio): Audio, Klang, Tastenkürzel, Kamera, Allgemein, Nutzer (nur Admins), Über ICU — keine Seite braucht einen Scrollbalken; mit „Erweitert“ (unten in der Seitenleiste) wächst das Fenster, statt zu scrollen. Eine Änderung per Hotkey oder Tray, während das Fenster offen ist, bleibt erhalten.
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
- **Windows**: nicht Teil dieses Releases (WASAPI, Raw Input, Layered Window und DPAPI sind nicht implementiert).
- Der Debug-Build braucht ~1,2 ms je 10-ms-Block; für Latenzmessungen den Release-Build nehmen.
