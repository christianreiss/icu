# Loss-Feedback: berechtigte Empfänger — 2026-10-24

Quellcodeprüfung und lokale QUIC-Regressionen auf `biest.eulie.de`,
Ausgangsstand `cae9a8c82813cd1796e70de7a4a45d95915bd83d`.

## Fehler und Korrektur

Ein angemeldeter, selbst oder administrativ taubgeschalteter Teilnehmer konnte
weiter `LossReport` für einen anderen Sprecher schicken. Der Server zählte ihn
nicht als Audioempfänger, nahm seine Stimme aber in den Konsens auf. Mit zwei
gesunden Empfängern, die unveränderte Nullverluste nicht melden, genügte sein
Bericht über 1000 ‰ für einen entsprechenden `LossHint`. Der Client pausiert
bei diesem Wert seine Kamera für den ganzen Raum. Wiederholte Berichte hielten
die Störung aufrecht. Vor dem Taubschalten gespeicherte Berichte blieben außerdem
bis zum regulären Ablauf von sechs Sekunden gültig.

Der Server akzeptiert nun nur Berichte aktueller, nicht taubgeschalteter
Audioempfänger und nur Werte zwischen 0 und 1000 ‰. Selbst- und Admin-
Taubschaltung entfernen unberechtigte Stimmen sofort und berechnen den Konsens
neu. Die periodische 100-ms-Prüfung entfernt ebenfalls abgelaufene oder nicht
mehr berechtigte Stimmen und berücksichtigt eine geänderte Empfängerzahl.
Entfernte Stimmen erscheinen beim Enttaubschalten nicht erneut; dafür ist ein
neuer Bericht erforderlich.

Die Veröffentlichung bleibt unter derselben Loss-Sperre wie die Berechnung.
So kann ein gleichzeitig bearbeiteter alter Bericht die Korrektur nach einer
Taubschaltung nicht durch eine später veröffentlichte alte Meldung überschreiben.
Wire-Layout, Protokollversion, Konsensschwelle und TTL bleiben gleich.

## Verifikation

Vier neue echte QUIC-Tests scheiterten vor der Implementierung mit Exitcode 101:
unberechtigter Bericht bei zwei gesunden Empfängern, Selbst-/Admin-Taubschaltung
nach einem gültigen Bericht sowie Werte über 1000 ‰. Sie bestehen nach der
Korrektur. Die Taubschaltung muss innerhalb einer Sekunde einen Korrektur-Hint
liefern, deutlich vor dem normalen TTL-Ablauf. Weiteres gültiges Feedback wird
im Grenzwerttest ausdrücklich angenommen.

Unit-Regressionen prüfen zusätzlich, dass ungültige Werte einen gültigen Bericht
weder ersetzen noch verlängern, entfernte Stimmen nach Enttaubschaltung nicht
zurückkehren und eine reine Änderung der Empfängerzahl den Konsens neu berechnet.

Abschließend bestanden:

- `cargo test --locked -p icu-server --lib --test loss_feedback --test control
  --test admin --test audio_feedback`: 228 Tests bestanden, 0 fehlgeschlagen.
- `cargo clippy --locked -p icu-server --all-targets -- -D warnings`.
- `cargo fmt --all -- --check` und `git diff --check`.

Der übergreifende Workspace-Lauf folgt bei der Integration. Kein Paket-/Release-
Build und kein Test mit physischen Clients wurde in diesem Teilauftrag ausgeführt.

Keine Produktionsdienste, Pakete oder Benutzerdaten wurden verändert. Es gab
keinen Push und kein Deployment. Rücknahme durch Revert des zugehörigen Commits
und Neubau des Servers; keine Datenmigration erforderlich.
