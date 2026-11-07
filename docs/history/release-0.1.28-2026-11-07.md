# ICU 0.1.28 — Kamera-Notfallfix

0.1.27 konnte frische Videobilder aufgrund des Audiozielwerts vollständig
unterdrücken. Im echten Linux-/Windows-Gespräch am 2026-11-07 leitete der
Server beide Streams ohne Video-Drops weiter; Linux zählte 3987 empfangene
Frames, 3185 Timing-Verwerfungen und zuletzt −421342 µs Decode-/Audioziel-Abstand.
Beide Teilnehmer meldeten fehlende Gegenstellenbilder.

0.1.28 entfernt die A/V-Präsentationsfilterung komplett: frische dekodierte
Bilder erscheinen sofort. Audiozielwerte bleiben ausschließlich Diagnosewerte.
Bildkorrektur, Capture-Zeitstempel, H.264-Recovery, Privacy-Sperren und begrenzte
Lese-/Decode-Queues bleiben erhalten. Protokoll Minor 8 bleibt unverändert;
Client 0.1.28 ist mit Server 0.1.27 kompatibel. Ein Serverneustart ist unnötig.

Die Regression mit fortlaufenden Frames bei ±421, ±101, ±40 und 0 ms scheiterte
vor der Änderung (`selected` lieferte `None`). Sie prüft außerdem monotonen
Bildwechsel und das Auslaufen eines gestoppten Streams. Vollständige A/V-
Synchronität ist nach dieser bewussten Notfallrücknahme nicht zugesichert.

Rollback: Clientpaket 0.1.27 aus `dist/0.1.27/` installieren; Identität und
Einstellungen beibehalten. Das stellt auch den hier behobenen Fehler wieder her.

## Verifikation und Auslieferung

- `cargo fmt --all -- --check`, `git diff --check`, vollständiges
  `cargo test --locked --workspace` und striktes Workspace-Clippy bestanden.
  Client-Unit-Suite: 481 bestanden, 12 opt-in Prüfungen ignoriert.
- `./build.sh --plain` auf sauberem Commit
  `7866645170edda5f0eff064c39a02efae6972f50` bestanden: sieben Pakete,
  Linux- und Windows-Release-Build, Supply-Chain-/Lizenzprüfung, Linux-Smokes
  für Fedora 42/43, Debian 12/13 und AlmaLinux 10. Windows-Linker meldete
  COMDAT-Warnungen der vorhandenen C++-Bibliothek; Build erfolgreich.
- Auf biest wurde zunächst derselbe optimierte Fedora-Build atomar eingesetzt,
  danach das offizielle RPM `icu-client-0.1.28-1.fc43` installiert.
  `rpm -V icu-client` ist sauber; installierte, laufende und gebaute Binärdatei
  haben SHA-256 `0768ce2410b29b013eca8da71c531f350d3f80fa07d944af77e50fd54440091a`.
  Der alte Prozess beendete sich erst nach seinem 5-s-Signal-Watchdog;
  der erste Start traf noch seine Singleton-Sperre, der erneute Start gelang.
  ICU läuft als transiente User-Unit `icu-hotfix-0.1.28.service`.
- Live: lab.uggs.io bestätigt Client `icu/0.1.28 linux` gegen unveränderten
  Server 0.1.27. Claymance erschien wieder im echten Desktop-Overlay.
  Empfang bei etwa −302 ms Audioziel-Abstand, Timing-Verwerfungen `late=0`.
  Server wurde nicht neu gestartet. Schlüssel und Einstellungen blieben erhalten.
- Auf twingo wurden alle Pakete unter
  `/var/www/clients/client1/web20/web/icu/downloads/0.1.28/` veröffentlicht.
  Alle serverseitigen Prüfsummen und die SSHSIG-Signatur bestanden vor dem
  atomaren Aktivieren von `downloads/latest.json`. Die Webseite blieb bytegleich.
  Ein hängender SSH-Mkdir-Versuch für einen verworfenen separaten Notfallpfad
  wurde beendet; die endgültige Veröffentlichung lief mit begrenzten SSH-Timeouts.
- Frischer HTTPS-Abruf von `https://misc.alpha-labs.net/icu/downloads/latest.json`
  bestätigt Version und Quellcommit, gültige `icu-update`-Signatur und
  übereinstimmende Größe/SHA-256 sämtlicher sieben ausgelieferter Pakete.
  Evidenz: `target/release-0.1.28/served/`, Buildprotokolle unter
  `target/build-logs/20261107T200626.568549Z-872014/`.

Nicht durchgeführt: Windows-Hardwareinstallation bei Claymance, erneute native
Windows-Clippy-Prüfung und physische Lip-Sync-Messung. Der Windows-Installer ist
veröffentlicht; dessen Ausführung auf seinem Rechner ist keine hier belegte Abnahme.

Der vorherige Updatekanal liegt auf twingo unter
`/root/icu-rollback-0.1.28/latest-before.json`; zum Zurücksetzen atomar nach
`downloads/latest.json` kopieren. Ältere Pakete bleiben verfügbar. Der temporär
ersetzte lokale Binary-Stand liegt unter `target/release-0.1.28/icu-before`;
für einen sauberen Paketrollback das alte RPM installieren. Die transiente
User-Unit kann mit `systemctl --user stop icu-hotfix-0.1.28` beendet und ICU danach
wie gewohnt aus dem Desktop gestartet werden.
