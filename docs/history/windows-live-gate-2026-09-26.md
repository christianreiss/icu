# Ergänzender Windows-Live-Gate 2026-09-26

Historischer Befehls- und Ergebnisnachweis aus der früheren Live-Test-Anleitung.
Die Befehle und Pfade bleiben unverändert; heutige Pfade stehen in der
[Umzugstabelle](../README.md#umzug-vom-2026-09-28), aktuelle Befehle unter
[Bauen](../development/building.md). Abschnittsverweise gelten für den damaligen Stand.

### Ergänzender Gate-Lauf auf 10.2.0.201

[Windows-Gate](windows-gate-2026-09-26.md) enthält die Ergebnisse des aktuellen
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
