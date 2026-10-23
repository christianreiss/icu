# Gute Audiopegel und kausale Peer-Hints — 2026-10-23

Umsetzung des bestätigten Plans auf Basis `008b540`. Automatische, lokale
Geräteprofile und gehaltene Pausenkorrektur wurden ausdrücklich gewählt.

## Verhalten

- Gute Pegel werden nach 20 s/fünf geeigneten Fenstern pro tatsächlichem
  Mikrofon gespeichert. Verarbeitung und nativer Ausgangspegel müssen beim
  Wiederanlauf passen; positive Softwareverstärkung startet mit höchstens
  6 dB. Peer-Vorwissen ist nur ein Kandidat hinter frischer lokaler Bestätigung.
- Private, versionierte Datei `audio-calibration.toml`, höchstens 16 Profile,
  Ablauf nach 30 Tagen. Ein separates Lockfile schützt Lesen/Zusammenführen/
  atomisches Ersetzen zwischen Prozessen. Ungültige neue Profile können keine
  guten Einträge löschen. Keine Audiodaten und kein Passport-Export.
- Wiederherstellen verändert Klangverarbeitung, nicht Mute, PTT oder Geräte.
  Neulernen löscht nur das aktuelle Profil und stellt ausschließlich noch
  selbst besessene Systemänderungen zurück. Native Geräteidentität wird für
  diese Aktionen direkt gelesen: ein nach DSP-Neustart kurz leerer Messcache
  darf das Löschen nicht verhindern (durch Engine-Regression nachgewiesen).
- Eine während derselben Sitzung erreichte Korrektur bleibt bei Stille,
  fehlender Messung oder Konflikt erhalten. Nur frische passende Daten dürfen
  nachregeln; explizites Abschalten neutralisiert mit 0,5 dB je zwei Sekunden.
- Eingangs-Clipping, begrenzte Softwareverstärkung und dauerhaft starke
  Ausgangsbegrenzung sind getrennte Zustände. Sie verhindern weitere
  Peer-Anhebungen und ungeeignete Speicherung. Capture-Clipping kann eine
  reine Reaktionsprüfungspause überstimmen, niemals einen manuellen Eingriff
  oder einen Geräte-/Schreibfehler.
- Die Audio-Details bieten Wiederherstellen, Neulernen und samplefreie
  Diagnose. Status, Empfängerzahl, Messalter und Speicherdatum sind sichtbar;
  Symbole besitzen zugänglichen Klartext.

## Protokoll und Kausalität

Minor und Mindest-Minor sind 6. Audioheader: 14 statt 10 Bytes; Opus unverändert.
`gain_epoch` kennzeichnet bewusst angewandte Verarbeitung/Pegel, nicht jede
AGC-Bewegung. DSP-Thread und die Paketkennzeichnung verwenden dieselbe
Generation. Alte Pre-Roll-Frames werden beim Wechsel verworfen. Die ersten
200 ms einer neuen Kennung zählen nicht; dies deckt auch den einen eventuell
angebrochenen 20-ms-Frame mit alter/neuer Hälfte ab.

Berichte tragen Generation, Sample-Zeitgrenzen und Berichtszähler. Server und
Sender lehnen alte Generationen, doppelte und überlappende Fenster ab. Der
Server bezieht den aktuellen Zustand aus zugelassenen Audiodatagrammen;
zusätzlich gelten Zeit- und Größenprüfungen. Bereits für einen Hint verbrauchte
Stimmen zählen auch nicht als alte Mehrheitsstimme für den nächsten. Nach
jedem Schritt ist ein vollständiges Fenster der tatsächlich angewandten
Korrektur nötig. Client und Server müssen gemeinsam aktualisiert werden;
ältere Gegenstellen werden verständlich abgewiesen.

## Corona

Overlay-Rand 3 → 4 Layout-Pixel; GPU-Halo 24 → 30 Pixel. Hauptfenster-Avatare
verwenden 2,5 statt 2 Pixel Strichbreite mit passender Platzreserve. Das
Kamerabild bleibt gleich groß. Hardware-Rendering mit vier Videokacheln bei
120/240/360 px bestand; 60 Frames in jeweils etwa 2,01–2,03 s, CPU-Zeichenzeit
p95 etwa 3,7–4,7 ms. Helle/dunkle Audio-Details und Corona-Vorschau visuell geprüft.

## Verifikation und Grenzen

- Vollständiger Workspace-Testlauf: 911 bestanden, 0 fehlgeschlagen,
  12 absichtlich ignoriert; nach den Abhängigkeits-Patches erneut bestanden.
- Zehn-Minuten-Simulation einschließlich Paket-/Berichtsverlust und Pausen,
  reale Engine-Profilaktionen und QUIC-Hints bestanden.
- Opus → zwei unabhängige Empfänger → Server-Konsens: Empfangsrestfehler beim
  Leise-Fall 2,61 → 1,08 dB, beim Laut-Fall −4,10 → −1,13 dB. Unterschiedliche
  Wiedergabelautstärken erzeugen identische Rückmeldungen.
- Windows-GNU-Kompilation einschließlich aller Client-Testtargets bestanden.
- Nativer PipeWire-Pegeltest auf eigener virtueller Remap-Quelle: −1 dB,
  Readback, exakte Wiederherstellung; Quelle/Sink entfernt, reale Standardquelle
  unverändert. Der zuerst ungeeignete virtuelle Adapter wurde ebenfalls entfernt.
- Formatprüfung und Workspace-Clippy mit `-D warnings -A deprecated` bestanden.
  Ohne die gezielte Ausnahme bleibt die bereits vorhandene veraltete
  `fetch_update`-Verwendung in `icu-server/src/endpoint.rs` rot.

Reale akustische Mikrofon-/Lautsprecherprüfung und native Windows-Hörtests
werden durch Simulation oder Cross-Kompilation nicht ersetzt. Capture-Clipping
kann nicht nachträglich repariert werden. Die laufende Installation wurde nicht
ersetzt; kein Push und kein Deployment gehören zu dieser Änderung.

Rollback: vorherigen Client und Server gemeinsam verwenden; gespeicherte
Geräteprofile enthalten keine System-Schreibbefehle. Abschalten von
`audio.remember_good_levels` verhindert erneutes Speichern/Wiederverwenden,
„Neu lernen“ entfernt das aktuelle Profil. Die bestehenden Regeln für die
bedingte Wiederherstellung des Systempegels bleiben erhalten.

## Abhängigkeiten am Build-Gate

Der kanonische Paketbuild stoppte zunächst bei zwei neu veröffentlichten
RustSec-Advisories. Gezielte Patch-Updates beheben sie ohne Ausnahmen in der
Supply-Chain-Prüfung: Tokio 1.53.1 → 1.53.3
([RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html)) und
rustls-webpki 0.103.15 → 0.103.16
([RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382.html)).

Zusätzlich ersetzt clipboard-win 5.4.3 die inzwischen zurückgezogene
Version 5.4.1; keine Prüfung wird dafür abgeschwächt.

Der vollständige Paketbuild wurde am 2026-10-23 auf Operatorwunsch während
des Debian-Client-Builds gestoppt. Supply-Chain-Gate, Workspace-Tests sowie
Release-Builds für Server, Fedora 42 und Fedora 43 bestanden. Debian-/Windows-
Pakete und Paket-Smoke-Tests wurden in diesem Lauf nicht fertiggestellt.
26 Tests des Paket-Build-Werkzeugs bestanden separat. Kein Paket installiert.
