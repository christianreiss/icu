# Release 0.1.20 — Client-Updates, Oberfläche und Tray

Ausgeliefert am 2026-10-30. Die Pakete stammen aus dem sauberen Quellstand
`4e242e45b04320c6482cf736f4725a4c93ba92f4`; dieses nachträgliche
Auslieferungsprotokoll verändert die gebauten Programme nicht.

## Änderungen

- Authentifizierte, asynchrone Versionsprüfung beim Start, alle sechs Stunden
  und manuell. Windows aktualisiert portable und NSIS-Installationen nach
  ausdrücklicher Bestätigung. Linux zeigt ausschließlich den schließbaren
  Hinweis auf den bisherigen Installationsweg. `min_client` verwendet dieselbe
  Prüfung. Gespräche werden nicht automatisch für ein Update beendet.
- Hauptfenster und Einstellungen übernehmen Farben, Inter-Schrift,
  SVG-Symbole und Buttonzustände der vorgegebenen Claude-Vorlage. Die Haupt-
  und Einstellungsbuttons verwenden deren unterschiedliche Größen/Radien;
  Einstellungen werden sofort gespeichert. Suchergebnisse und die Update-Zeile
  funktionieren auch bei schmalen Fenstern.
- Tray-Aktionen erreichen die native Ereignisschleife auch bei minimiertem
  Fenster. Unter KDE/Wayland wird das Aktivierungstoken des tatsächlichen
  Benutzereingriffs für „Fenster anzeigen“ weitergereicht.

Die Audio-/Ducking-Arbeit war bereits mit 0.1.19 ausgeliefert. Hier wurden
keine weiteren akustischen Langläufe oder Hörtests angehängt.

## Build und automatisierte Prüfungen

| Prüfung | Ergebnis |
| --- | --- |
| Kanonisches `./build.sh --plain` | Erfolgreich; alle sieben Pakete |
| Tests im kanonischen Lauf | 46 Suites, 1.057 bestanden, 0 fehlgeschlagen, 16 ignoriert |
| UI-Prüfungen, im obigen Lauf enthalten | 42 bestanden |
| Distributions-Smokes | Alle fünf bestanden |
| Supply Chain, Lizenz- und Windows-PE-Prüfungen | Bestanden |
| Linux- und Windows-Clippy mit `-D warnings`, Rustfmt | Bestanden |
| Publisher-/Build-Wrapper-Pythontests | 30 bestanden |
| Auf Windows nativ ausgeführte Updater-Unit-Tests | 16 bestanden |

Die Updater-Prüfungen decken unter anderem semantische Versionen,
Artefaktauswahl, ungültige Signaturen, abgebrochene Downloads, Archivpfade,
Dateiwiederherstellung und Windows-Sperren/-Argumente ab. Der Linux-UI-Test
`linux_updates_are_dismissible_information_with_manual_checks_only` weist die
schließbare Meldung und manuelle Prüfung ohne Installationsaktion nach.
Der produktive Linux-Build enthält keinen Windows-Installationspfad.

Die 16 ignorierten Tests wurden nicht als bestanden gezählt. Diese Abnahme
ersetzt keine erneute physische Audio-, Bluetooth-, Hotplug- oder
Suspend/Resume-Abnahme. Für 0.1.20 wurden keine zusätzlichen langen Lasttests
gestartet.

## Tatsächlicher Windows-Ablauf

Auf der Windows-11-VM `icu-win11-gate` wurden echte GUI-Prozesse, UAC und die
originalen Release-Artefakte verwendet. Der ältere Ausgangsclient war ein
privater, als 0.1.19 versionierter Build mit zurückportiertem Updater. Die
veröffentlichte 0.1.19 hat keinen Updater und benötigt deshalb einmalig eine
manuelle Aktualisierung auf 0.1.20. Weder das alte Release noch der öffentliche
Updatekanal wurden für die Tests verändert; der temporäre Testkanal war
getrennt und ebenfalls signiert.

| Ablauf | Beobachtetes Ergebnis |
| --- | --- |
| Portable, Pfad mit Leerzeichen, älteres Fixture → finales 0.1.20-ZIP | Austausch und Neustart erfolgreich; genau ein neuer Client; weiterhin portabel |
| Bestehende NSIS-Installation → finales 0.1.20-Setup | Erfolgreich; `C:\Program Files\ICU`, Uninstall-Eintrag und Verknüpfung erhalten |
| Rechte nach beiden erfolgreichen Updates | Neuer GUI-Prozess nicht erhöht |
| Nutzerdaten | Je sieben Profil-/Identitätsdateien bytegleich; unbekannte portable Datei erhalten |
| Expliziter UAC-Abbruch im sicheren Desktop | Alter Prozess bleibt nutzbar; alte EXE und sieben Profildateien unverändert |
| Erzwungener portabler Austauschfehler durch Dateisperre | Neue EXE kurz nachgewiesen, alte Dateien wiederhergestellt, alter GUI-Prozess neu gestartet; Profil unverändert |
| Erfolgreicher Abschluss | Keine verbliebenen Transaktionsjournale bzw. NSIS-Staging-Ablagen |

Beide final installierten EXEs wurden gegen die tatsächlich veröffentlichten
Paketbytes geprüft, SHA-256:
`6bfd6a402b7ceadc0ad88ae402d6a8c8407d6091c3c1efb58153a3936cecbeda`.
Der explizite UAC-Abbruch und die Fehlereinjektion erfolgten mit früheren
privaten Kandidaten; die erfolgreichen portablen und NSIS-Durchläufe wurden
anschließend mit den endgültigen Paketen wiederholt.

Die native Abnahme fand zwei Fehler, die vor dem finalen Build behoben wurden:
Der Rollback-Neustart durfte keine bereits beendete `--update-ready`-Sitzung
wiederverwenden und musste die Installationssperre zuvor freigeben. Außerdem
musste `/S` für NSIS ohne zusätzliche Anführungszeichen übergeben werden;
validierte Updateaufrufe erzwingen zusätzlich den stillen Setupmodus.

Ein echter Stromausfall wurde nicht simuliert. Persistente Sicherungen und
die manuelle Wiederherstellung sind in der
[Update-Anleitung](../development/updates.md#windows-austausch-und-wiederherstellung)
beschrieben. Authenticode ist weiterhin nicht eingerichtet; die eingebettete
Ed25519-Vertrauensbasis authentifiziert Metadaten und Artefakte unabhängig davon.

## Linux, Tray und produktiver Betrieb

- **biest.eulie.de:** Fedora-43-RPM mit DNF auf 0.1.20 aktualisiert;
  `rpm -V` ohne Abweichung. Die neun ursprünglichen Profil-/Identitätsdateien
  blieben bytegleich. Der installierte Client verband sich mit dem echten
  Server, mit einer privaten Profilkopie und stummem Testaudio; Beenden über
  den Tray funktionierte auch bei minimiertem Fenster, Prozessende 0.
- „Fenster anzeigen“ und „Beenden“ wurden zuvor mit tatsächlicher KDE-
  Tray-Eingabe und frischem Aktivierungstoken am implementierten Stand geprüft.
  Der abschließende installierte Client lief bei gesperrtem Desktop: ein
  synthetischer Show-Aufruf ohne Aktivierungstoken wurde erwartungsgemäß von
  Wayland nicht als Benutzeraktivierung behandelt. Dies ist kein zusätzlicher
  erfolgreicher Show-Nachweis; die Bildschirmsperre wurde nicht umgangen.
- **lab.uggs.io:** Serverpaket auf 0.1.20-1 aktualisiert, Dienst aktiv und
  aktiviert. Unmittelbar vor dem Neustart waren keine Nutzer online.
  `icu-server doctor` meldete 18 OK, 1 Info und 1 Skip wegen der vom laufenden
  Dienst belegten Ports. Vier Nutzer und sechs unveränderliche Zustandsdateien
  blieben erhalten; die CRL stieg regulär. Laufende EXE und Datei auf Platte
  entsprechen dem Paket. Nach dem GUI-Test: Status OK, keine Nutzer online.
- **twingo.alpha-labs.net:** Alle sieben Artefakte zuerst veröffentlicht,
  anschließend das signierte `latest.json` atomisch freigeschaltet. Bestehendes
  Webseitenlayout erhalten; Versions-, Download- und Releasehinweise auf
  0.1.20 angepasst. Öffentliche HTTPS-Downloads aller sieben Dateien wurden
  vollständig gegen Größe/Hash geprüft, die Manifest-Signatur separat geprüft.

Veröffentlichung: <https://misc.alpha-labs.net/icu/>. Es wurde kein produktives
Release allein für einen Test erzeugt und kein GitHub-Mirror-Upload behauptet.

## Nachweise, Rückweg und nächste Veröffentlichung

Die lokalen Logs, Screenshots, Prüfskripte und geschützten Profilsicherungen
liegen nach der Worktree-Bereinigung unter `target/release-0.1.20/`, die Pakete
unter `dist/0.1.20/`. Maßgebliche Nachweise sind `public-verification.json`,
`local-acceptance.json`, `server-final-state.log`, `portable-final-result.log`,
`nsis-final-result.log`, die jeweiligen Token-/EXE-Hashprüfungen sowie
`uac-cancel-result.log` und `rollback-fixed-result.log`. Die vollständigen
kanonischen Buildlogs werden dort ebenfalls aufbewahrt. Private Profile und
Schlüssel sind nicht Teil des Repositorys.

Vorheriger Server und Zustand: `/root/icu-rollback-0.1.20` auf **lab.uggs.io**.
Vorherige Webseite: `/root/icu-rollback-0.1.20-web` auf **twingo.alpha-labs.net**.
Das 0.1.19-RPM und die Profilsicherung stehen lokal für einen ausdrücklich
manuellen Rückweg bereit. Der automatische Updatekanal führt keine Downgrades
aus. Beim Rückweg keine inzwischen geänderten Nutzerdaten mit alten Backups
überschreiben.

Die [Release-Anleitung](../development/updates.md#künftiges-release-veröffentlichen)
beschreibt Versionserhöhung, sauberen Build, Signierung, vollständige
Artefaktübertragung und atomische Veröffentlichung. Den privaten Releaseschlüssel
außerhalb von Git sicher aufbewahren; ohne ihn gibt es keinen unsignierten
Ausweichweg. Frühere veröffentlichte Artefakte bleiben unverändert.
