# ICU Live-Test (Linux-Client gegen lokalen Server)

Stand 2026-09-20, Client-Stand M0/M1/M2/M4-Linux. Alles auf einem Rechner (KDE Wayland, PipeWire). Zwei Client-Instanzen laufen mit getrennten `--config-dir`, damit jede ihre eigene Identität (Zertifikat) und Einstellungen hat.

## 0. Bauen

```sh
cd ~/Documents/ICU
cargo build --release -p icu-server -p icu-client      # Debug geht auch, dann target/debug/…
```

Voraussetzungen auf dem Build-Host (alle vorhanden): `pipewire-devel`, `clang-devel`, `meson`, `ninja-build`, `abseil-cpp-devel`, `llvm`, `cmake`, `nasm`, `libxkbcommon-devel`, `wayland-devel` und der Symlink `/usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-objcopy` → `/usr/bin/llvm-objcopy` (siehe `Client/IMPLEMENTATION.md` §12).

## 1. Server lokal starten

Minimal-Config, z. B. `~/icu-live/server.toml` (die Defaults `/var/lib/icu` und `/run/icu` sind als Nutzer nicht beschreibbar; **der Admin-Socket-Pfad muss kürzer als 108 Byte sein**, sonst startet der Server ohne Admin-Socket und `invite` schlägt fehl):

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
./target/release/icu-server --config ~/icu-live/server.toml invite --name chris1
./target/release/icu-server --config ~/icu-live/server.toml invite --name chris2
```

Jeder Aufruf druckt einen Link `icu://127.0.0.1:4711/enroll?t=…&ca=…&n=chris1`. Token sind einmalig und 24 h gültig.

## 3. Client 1 und 2 enrollen und verbinden

```sh
./target/release/icu 'icu://…n=chris1' --config-dir ~/icu-live/c1
./target/release/icu 'icu://…n=chris2' --config-dir ~/icu-live/c2 --no-hotkeys
```

(Link in Anführungszeichen, wegen `&`.) Der erste Aufruf enrollt (`identity/` im Config-Dir) und startet danach sofort das Fenster; spätere Starts brauchen nur noch `icu --config-dir ~/icu-live/c1`.

Beim ersten Start fragt KDE (Portal GlobalShortcuts) nach den Kurzbefehlen für ICU; das Fenster erscheint unabhängig davon sofort. „Abbrechen“ im Dialog ⇒ der Client fällt auf evdev zurück (⚙ → Hotkeys zeigt das aktive Backend, sobald es feststeht).

`--no-hotkeys` bei Client 2: beide Instanzen würden sonst dieselbe evdev-PTT-Taste (Default `KEY_RIGHTCTRL`) sehen und gleichzeitig senden. Alternativ in `~/icu-live/c2/settings.toml` unter `[hotkeys]` `ptt = { key = "KEY_RIGHTALT" }` eintragen oder Client 2 auf VAD stellen (Rechtsklick auf den PTT-Button).

Ohne `--no-uri-handler` schreibt der Client beim Start `~/.local/share/applications/icu.desktop` und ruft `xdg-mime default icu.desktop x-scheme-handler/icu` auf — danach öffnen `icu://`-Links in Browser/Chat direkt ICU. Abschaltbar in den Einstellungen („icu://-Links mit ICU öffnen“) oder per `--no-uri-handler`.

### Was zu sehen sein soll

- Fenster 400×620: oben das Logo-Badge, der Servername aus dem Zertifikat (`localhost`) und eine Pille „verbunden" mit grünem Punkt; Adresse, `user_id` und der Grund einer Trennung stehen im Tooltip der Pille, nicht in der Zeile. Rechts das Zahnrad.
- Teilnehmerliste: eigener Name fett zuoberst, danach die anderen alphabetisch. Avatar mit Initialen (`chris1` → `C1`, `chris2` → `C2`), blauer Ring + blau getönte Zeile + Balken = spricht (kommt aus dem Datagram-Fluss, < 100 ms nach dem ersten Paket). Eine zweite Zeile erscheint nur, wenn es etwas zu sagen gibt („sendet · Push-to-Talk", „Kamera an", „stummgeschaltet", „hört nicht zu"). Rechts je Peer eine Lautsprecher-Taste, die einen Lautstärke-Schieber in der Zeile aufklappt. Sendet jemand Video, steht sein Bild rund beschnitten an der Stelle der Initialen — auch in der eigenen Zeile, dort aus der lokalen Vorschau.
- Unten: 28-Segment-Pegelmeter (grün, blau während gesendet wird, rote Spitzensegmente) mit einem gedimmten Segment als Peak-Marker; darunter links der Pegel in dBFS, rechts je nach Modus die PTT-Taste als Chip (`RIGHTCTRL`), der VAD-Balken mit Schwellen-Markierung oder „SENDET".
- Darunter **Sprechen** (halten) neben der PTT/VAD-Umschalttaste, dann die vier Schalter Mute / Deafen / Video / Overlay als Icon-über-Label-Tasten (aktiv: rot getönt bei Mute/Deafen, blau bei Video/Overlay).
- Tray-Icon (ksni): grau = getrennt, grün = verbunden, orange = Mute, rot = Deafen; Menü Mute/Deafen/Fenster anzeigen/Beenden. Fenster schließen = ins Tray; Shift+Schließen oder Tray → Beenden beendet.

### Was zu hören sein soll

- Client 1: rechte Strg-Taste (evdev, Backend-Anzeige in ⚙ → Hotkeys) oder den PTT-Button gedrückt halten und sprechen → Client 2 gibt die Stimme aus (Default-Sink, im Test das Jabra-Headset). Achtung: beide Clients laufen auf demselben Default-Mikrofon (C-Media) und demselben Default-Sink — für einen echten Hörtest in ⚙ → Audio bei Client 2 ein anderes Gerät wählen (Gerätewechsel wirkt sofort) oder Client 2 deafen.
- Mute-Cue (zwei Töne) beim Stummschalten, Join/Leave-Cue wenn der andere Client kommt/geht.
- Echo-Unterdrückung ist an (AEC3); ⚙ → Audio zeigt ERL/ERLE, sobald das APM einen Echopfad sieht.

## 4. Nur Netz/Audio prüfen (ohne Fenster, ohne Mikrofon)

```sh
./target/release/icu --config-dir ~/icu-live/c1 --headless --no-hotkeys --test-tone --debug-stats
./target/release/icu --config-dir ~/icu-live/c2 --headless --no-hotkeys --debug-stats
```

Client 1 sendet ein synthetisches Sprachsignal (Gate offen, RNNoise/AEC aus), Client 2 spielt es auf dem Default-Sink aus. Auf diesem Host am 2026-09-20 gemessen (10 s, Debug-Build): Client 1 `sent=498`, Client 2 `recv=500 decoded=499 played=480960` Samples, `peers=[chris1*]` (Speaking-Indicator), RTT 0,4 ms.

`--camera synthetic` schickt zusätzlich ein Testbild (15 fps H.264, `video_tx` zählt hoch); `--camera 0` nimmt die echte Kamera.

## 5. `--debug-stats` lesen (alle 2 s auf stderr)

```
icu stats: sent=498 recv=500 bad=0 decoded=499 fec=0 plc=0 played=480960 dsp_blocks=997 ref_underruns=1 cap_overruns=0 loss_perc=0 rtt=0.40 ms reconnects=0 video_tx=0 video_rx=0 idr_forced=0
  dsp block      n=999 p50<1.5 p95<1.5 p99<2.0 mean=1.17 max=1.92 ms
  jitter delay   n=499 p50<30.0 p95<30.0 p99<30.0 mean=28.18 max=30.03 ms
  capture→send   n=0 …
  level=-100.0 dBFS vad=0.00 sending=false peers=[chris1*]
```

| Feld | Bedeutung | Soll |
| --- | --- | --- |
| `sent` / `recv` | Audio-Datagrams (50/s bei 20-ms-Frames, nur während Sendung) | wächst um ~100 je 2 s beim Sprecher bzw. Hörer |
| `decoded` / `fec` / `plc` | Frames aus echten Paketen / aus FEC / per PLC | im LAN `fec=plc=0` |
| `played` | Samples an die Wiedergabe (48 000/s) | wächst um ~96 000 je 2 s |
| `dsp_blocks` | 10-ms-Blöcke der Capture-Kette | ~200 je 2 s |
| `ref_underruns` | AEC-Referenz kam nicht rechtzeitig | einzelne beim Start ok, sonst 0 |
| `loss_perc` | `PACKET_LOSS_PERC` aus `LossHint` | 0 im LAN |
| `rtt` | Ping/Pong | < 1 ms Loopback |
| `dsp block` | Zeit je 10-ms-Block (AEC + RNNoise + Opus) | Debug ≈ 1,2 ms, Release ≈ 0,15 ms |
| `jitter delay` | Ankunft → Ausspielen | ≈ Jitter-Ziel (Start 40 ms, adaptiv, Untergrenze 5 ms) |
| `capture→send` | Capture-Block → `send_datagram` | < 2 ms |
| `level` / `vad` | Mikrofonpegel, RNNoise-Sprachwahrscheinlichkeit | Sprache: −30…−10 dBFS, VAD > 0,5 |

## 6. Bekannte Einschränkungen (Stand heute)

- **Nicht unbeaufsichtigt prüfbar, bleibt für diesen Test:** Portal-Hotkey-Dialog (KDE fragt beim ersten Binden; bei „Abbrechen“ fällt der Client auf evdev zurück), Tastendruck, echte Sprache/Echo, Overlay über einem Vollbild-Spiel, Klick-Durchlässigkeit des Overlays.
- Beide Instanzen teilen sich Default-Mikrofon und Default-Sink; Gerätewahl in ⚙ → Audio.
- Hotkey-Änderungen in den Einstellungen gelten erst nach Neustart des Clients (Bindungen werden beim Start gelesen).
- Das Overlay zeigt Kamerakacheln (Rahmen orange = spricht), aber keine Namen (kein Textrenderer im Overlay); Teilnehmer ohne Kamera erscheinen nicht.
- „Stille Teilnehmer abdunkeln" (⚙ → Kamera → Overlay, Standard: an) dimmt Kacheln nicht sprechender Teilnehmer auf ~50 % Helligkeit.
- Video in der Teilnehmerliste (zwei Clients): B schaltet Video an → binnen ~1 s steht B's Bild im Kreis der Zeile, die Zeilenhöhe bleibt gegenüber Zeilen ohne Video unverändert; B spricht → der blaue Ring liegt um das Bild. Aus/an, Trennen und neu Verbinden müssen Bild und Initialen sauber wechseln (kein eingefrorenes Standbild, kein zurückbleibendes Bild eines Fremden). Overlay und Liste laufen gleichzeitig. In hell und dunkel prüfen.
- Die eigene Kamera-Vorschau steht in ⚙ → Kamera, zusammen mit der Geräte-Auswahlliste; sie zeigt jedes aufgenommene Bild, auch wenn das Video fürs Netz gerade pausiert ist. Ist die Kamera aus, steht dort „Video ist aus"; direkt nach dem Einschalten „Kamera startet…", bis das erste Bild da ist. Im Hauptfenster markiert stattdessen ein pulsierender roter Punkt auf der Video-Taste, dass gesendet wird.
- Kamera: `/dev/video0` (C270) braucht ~3 s bis zum ersten Bild (Auto-Belichtung); bei wenig Licht 4–6 fps statt 15 (Spike 5).
- Windows-Pfade (WASAPI, Raw Input, Layered Window, DPAPI) sind Stubs mit `Unsupported` — M3.
- Zertifikats-Renewal (`RenewRequest`) ist nicht implementiert (Leaf gilt 365 Tage).
- Der Debug-Build braucht ~1,2 ms je 10-ms-Block; für Latenzmessungen den Release-Build nehmen.
