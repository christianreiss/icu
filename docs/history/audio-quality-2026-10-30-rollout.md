# ICU 0.1.19 – Auslieferung am 30. Oktober 2026

Der Audio-/Ducking-Stand wurde aus dem sauberen Quellcommit
`76c6f7a48250f899351b74dd02500aed51d7e060` gebaut. Diese Dokumentation wurde
nach der Betriebsabnahme ergänzt; sie ist kein weiterer Binärbuild.
Die separat beauftragten Änderungen an Updater, Settings, Hauptfenster und
Systray gehören zum folgenden Client-Stand und sind nicht Teil dieser Freigabe.

## Build und gezielte Abnahme

- `./build.sh --plain`: alle sieben Pakete, 46 Workspace-Testsuites mit
  **1.044 bestanden, 0 fehlgeschlagen, 16 explizit ignoriert**; cargo-deny,
  Lizenzermittlung, Windows-Imports/PE-Ressourcen und die fünf Distribution-
  Smokeprüfungen erfolgreich. Linux-/Windows-Clippy und CPAL-Featurecheck
  bestanden zuvor auf dem zugehörigen Quellstand. Ein optionaler externer
  Affinitätswächter brach beim Anheften eines Containerprozesses mit
  `PermissionError` ab; der eigentliche Build lief vollständig weiter.
  Seine Abschlusslogs, BUILDINFO und Paketprüfsummen sind die Build-Evidenz.
- Die endgültige Fedora-43-Paket-EXE bestand die isolierten PipeWire-Tests
  für alle fünf Ducking-Modi, Kanalbalance, manuelle Eingriffe und
  Wiederherstellung einschließlich Prozess-/Guardian-Absturz.
- In der Windows-11-VM `icu-win11-gate` wurde das endgültige NSIS-Setup
  installiert. Version und EXE-Hash stimmen mit dem endgültigen portablen
  ZIP überein. Der native Paket-Guardian-Test bestand (28,13 s; alle Modi,
  Heartbeat-Ablauf, EOF und Absturzwiederherstellung). Das installierte
  Einladungsfenster wurde sichtbar über DX12 gerendert. Die Testaufgaben
  und ICU-Prozesse wurden anschließend beendet/entfernt. Es wurden keine
  produktiven Identitäten in die VM kopiert.
- Die beiden abschließenden 64-Teilnehmer-/600-s-Läufe sowie der gezielte
  WASAPI-Startregressionstest sind im [Audio-Abnahmebericht](audio-quality-2026-10-30.md)
  dokumentiert. Es gab keine weiteren Audio-/Transport-Langlaufwiederholungen.

## Tatsächlich installiert und veröffentlicht

- **lab.uggs.io:** Debian-Serverpaket 0.1.19-1 installiert und Dienst neu
  gestartet, nachdem der unmittelbar vorgeschaltete Guard null verbundene
  Nutzer bestätigt hatte. Dienst aktiv/aktiviert, Doctor ohne Fehler,
  Status gesund. Vier Nutzer und sechs unveränderliche Konfigurations-/
  Schlüsseldateien erhalten; CRL-Zähler monoton fortgeschrieben.
  Laufende EXE und Paketpayload stimmen überein.
- **biest.eulie.de:** Fedora-43-Clientpaket 0.1.19-1 installiert;
  `rpm -V icu-client` ohne Abweichung. Das installierte Programm wurde mit
  privater Profilkopie, stumm und ohne echte Audio-/Kameraübertragung
  sichtbar gestartet und per mTLS mit dem Produktionsserver verbunden.
  Danach wieder beendet, Server erneut bei null Online-Nutzern.
  Alle neun zuvor erfassten Dateien des ursprünglichen Nutzerprofils
  einschließlich Identität, Einstellungen und Kalibrierung sind unverändert.
- **twingo.alpha-labs.net:** fünf endgültige Clientpakete auf der
  [Downloadseite](https://misc.alpha-labs.net/icu/) veröffentlicht.
  Eine zwischenzeitliche fremde Layoutänderung wurde durch den
  Vergleichsguard erkannt und erhalten. Nur Version, fünf Downloadlinks/-größen
  und Release-Hinweise wurden aktualisiert. Seite und Prüfsummen wurden
  über HTTPS abgeglichen; alle fünf Dateien vollständig heruntergeladen
  und nach Größe/SHA-256 geprüft, die Seite zusätzlich im Browser geprüft.
  Keine GitHub-/Testveröffentlichung und keine Änderung an ISPConfig-
  Schutzattributen.

Client und Server müssen gemeinsam auf 0.1.19 aktualisiert werden;
0.1.18 ist auf Protokollebene nicht kompatibel.

## Grenzen dieser Abnahme

Die VM-Prüfung ersetzt keine physische Windows-Audioabnahme. Hotplug,
Suspend/Resume, reale Doppelsprech-/Dauerlauftests und die vollständige
Mund-zu-Ohr-Abnahme bleiben offen. Die Buffear→Luft→USB-Mikrofon-Messung
mit ungefähr 270 ms ist keine ICU-Ende-zu-Ende-Messung. Der Nutzer hörte
im blinden A/B-Beispiel **keinen klaren Unterschied**; daraus wird keine
subjektive Qualitätsverbesserung abgeleitet. Der Produktivserver bleibt
auf 16 Nutzer begrenzt; die 64-Teilnehmer-Prüfung war ein isolierter Relaytest.

## Rückweg und Evidenz

Auf lab.uggs.io liegen altes Serverpaket, private Konfiguration und Zustand
unter `/root/icu-rollback-0.1.19`; das alte Paket kann mit
`dpkg -i --force-confold` wieder installiert und der Dienst neu gestartet
werden. Das lokale alte Fedora-Paket liegt in `dist/0.1.18`, die private
Profilsicherung unter `target/release-0.1.19/local-config-backup.tgz` (0600).
Ein Protokoll-Rollback erfordert wieder passende Server- und Clientversionen.

Auf twingo.alpha-labs.net enthält `/root/icu-rollback-0.1.19` beide
historischen Seitenstände; für den unmittelbaren Rückweg gelten
`index-before-promotion-20261030.html` und
`SHA256SUMS-before-promotion-20261030`. Alte Downloadpakete wurden erhalten.
Es wurden keine Nutzerdateien gelöscht.

Die detaillierten, teilweise privaten Abnahmelogs und Screenshots liegen
lokal unter `target/release-0.1.19`; die vollständigen Buildlogs unter
`target/build-logs/20261030T162639.978978Z-1834450` im Audio-Arbeitsbaum.
Sie werden nicht mit privaten Profilen in Git aufgenommen.
