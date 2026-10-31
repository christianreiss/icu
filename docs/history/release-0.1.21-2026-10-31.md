# Release 0.1.21 — explizite PipeWire-Gerätewahl

Linux-Hotfix für `defined target not found`: Ein angeschlossenes Mikrofon
konnte ausfallen, wenn es zugleich explizit in ICU ausgewählt und das
Systemstandardgerät war. WirePlumber überschrieb die Gerätewahl mit
`target.node=-1`; der Schutz gegen unerwünschten Geräterückfall führte
anschließend zum Löschen des Streams.

Explizite Capture-, Wiedergabe- und Referenzstreams setzen zusätzlich
`node.dont-move=true`. „Systemstandard“ bleibt beweglich; Hotplug und die
Geräteauswahl werden weiterhin von ICU behandelt. Keine globalen
WirePlumber-Regeln oder Gerätestandards werden dafür verändert.

Der neue native Regressionstest ist bewusst opt-in, öffnet zwei Sekunden
lang das angegebene Standardmikrofon und verwirft alle Samples. Der
ursprüngliche Fehler wurde mit dem C-Media-USB-Mikrofon auf biest vor dem
Fix in 0,06 Sekunden reproduziert; mit dem Fix bestand derselbe Test in
2,04 Sekunden. Die beiden nativen Tests für Capture-/Referenzkanäle und
laufenden Kanalwechsel sowie Rustfmt und Linux-Clippy mit `-D warnings`
bestanden ebenfalls.

## Build und Auslieferung

Quellstand der Pakete: `5d57ad5ffdf8ed52fe84f74f656ca3030e2b819e`.
Das nachträglich ergänzte Auslieferungsprotokoll verändert die Pakete nicht.

- `./build.sh --plain`: alle sieben Pakete erfolgreich gebaut; 46 Testsuites,
  1.057 bestanden, keine Fehler, 17 ausdrücklich ignorierte Hardwaretests.
- Alle fünf Distributions-Smokes sowie Supply-Chain-/Lizenzprüfungen und
  Windows-PE-Prüfungen bestanden. Windows-ZIP und NSIS-Setup gebaut;
  Windows-Clippy und native Windows-Updateabläufe wurden für diesen
  ausschließlich Linux betreffenden Fix nicht erneut ausgeführt. Die
  native Updater-Abnahme gehört zum [Release 0.1.20](release-0.1.20-2026-10-30.md).
- **biest.eulie.de:** Fedora-43-RPM über DNF auf 0.1.21 aktualisiert,
  `rpm -V icu-client` ohne Abweichung. Der laufende Prozess verwendet
  nachweislich die installierte EXE. Die ursprüngliche feste Auswahl des
  C-Media-USB-Mikrofons wurde wiederhergestellt; beide Eingangskanäle sind
  aktiv, `node.dont-move=true`, Verbindung zu lab.uggs.io hergestellt und
  kein Mikrofonfehler im Log oder Fenster. Acht Profil-/Identitätsdateien
  blieben bytegleich; die zwischenzeitliche Einstellung „Systemstandard“
  wurde gezielt zurückgenommen, andere Einstellungen dabei erhalten.
- **twingo.alpha-labs.net:** vollständigen Paketsatz übertragen, signiertes
  Manifest zuletzt atomisch veröffentlicht, bestehendes Webseitenlayout
  erhalten. Alle sieben öffentlichen HTTPS-Downloads vollständig gegen
  Größen/Hashes geprüft und die Manifest-Signatur separat verifiziert.
- Der laufende Server auf **lab.uggs.io** wurde für diesen Client-Audiofix
  nicht neu gestartet oder verändert. Ein neuer Hörtest oder ein Gespräch
  mit einer Gegenstelle war nicht Teil dieser gezielten Fehlerabnahme.

Veröffentlicht unter <https://misc.alpha-labs.net/icu/>. Keine globalen
WirePlumber-Einstellungen wurden dauerhaft geändert; der kurzzeitig
erhöhte Diagnose-Loglevel wurde wieder auf seinen ursprünglichen Wert
zurückgesetzt. Das USB-Audioprofil wurde während der Diagnose neu
initialisiert und behielt seine ursprüngliche Duplex-Konfiguration.

## Nachweise und Rückweg

Logs, Vorher-/Nachher-Screenshots und Installations-/HTTPS-Nachweise liegen
lokal unter `target/microphone-2026-10-31/`; vollständige Buildlogs unter
`target/build-logs/20261031T070938.002057Z-2583257/`.
Die vorherige Webseite und der Kanal liegen auf twingo unter
`/root/icu-rollback-0.1.21-web`. Alte öffentliche Downloads bleiben erhalten.
Das lokale 0.1.20-RPM liegt weiterhin unter `dist/0.1.20/` für einen
bewussten manuellen Rückweg über DNF. Bei diesem Rückweg kann „Systemstandard“
als Zwischenlösung dienen, solange das USB-Mikrofon auch im System Standard
ist. Aktuelle Nutzerdaten nicht mit alten Sicherungen überschreiben.
