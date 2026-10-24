# Moderation und Tastenkürzel: Nachprüfung vom 2026-10-24

Ausgangsstand `cae9a8c82813cd1796e70de7a4a45d95915bd83d`, geprüft auf
`biest.eulie.de`. Diese Nachprüfung ergänzt den UI-Review vom 2026-10-23.

## Änderungen

- Ein noch nicht geladener Moderationsstatus bedeutete bisher irrtümlich
  „keine Einschränkung“. Der Lautstärkeknopf öffnete das Nutzerfenster zudem
  ohne Listenabfrage. Eine Stummschaltung konnte dadurch eine bereits gesetzte
  Taubschaltung aufheben. Nutzerfenster und Kontextmenü fordern nun ihre eigene
  Aktualisierung an und bieten erst nach einer tatsächlichen Antwort Aktionen
  an. Die lokale `user_list_generation` zählt auch unveränderte Antworten;
  eine gefüllte, aber alte Liste schaltet keine Aktionen frei. Fehlende Einträge
  bleiben ausdrücklich unbekannt. Das geöffnete Fenster aktualisiert nach zehn
  Sekunden, während des Ladens nach zwei Sekunden. Ein Klick ändert weiterhin
  nur die gewählte Einschränkung und erhält die andere aus der geladenen Liste.
- Die Namen in der Teilnehmerliste blockierten durch ihre Textauswahl den
  Klick auf die Zeile und das Rechtsklickmenü. Sie überlassen diese Ereignisse
  jetzt der Zeile. Die eigenständigen Lautstärkeknöpfe bleiben bedienbar.
- Der erweiterte Wechsel der Kürzelmethode lag hinter der bisherigen
  Abbruchprüfung. Er beendet nun sofort eine laufende Aufnahme samt Reader
  und verwirft alte Fehlermeldungen; verspätete Ergebnisse werden nicht übernommen.
- Eine vom Portal nicht unterstützte Maustaste oder einzelne Modifikatortaste
  bekommt dieselbe Erklärung in Einstellungen und Einrichtung. Der Hinweis sagt
  ausdrücklich, dass die vorherige Portal-Belegung erhalten bleibt; die direkte
  evdev-Belegung darf weiterhin unabhängig davon gespeichert werden.
- Die eigene Teilnehmerzeile zeigt auch die Stummschaltung des Systemmikrofons.

## Nachweise

Die neuen Tests steuern die Produktionsoberflächen mit echten egui-Klicks,
Fokus- und Hover-Ereignissen. Backendwechsel, fehlende Portal-Erklärung und
fehlende initiale Listenabfrage scheiterten vor den jeweiligen Korrekturen mit
Test-Exitcode 101. Die Kontextmenüprüfung reproduzierte zusätzlich den vom
Namensfeld abgefangenen Rechtsklick, bevor dessen Textauswahl abgeschaltet wurde.

Die Moderationsfälle prüfen sowohl leere als auch bereits gefüllte alte Listen:
Ohne neue Antwort gibt es keine Einschränkungsaktion. Nach der Antwort enthält
der tatsächliche Stummschaltbefehl weiterhin die bestehende Taubschaltung.
Ein eigener Test über QUIC prüft, dass zwei inhaltlich identische Nutzerlisten
die Antwortgeneration zweimal fortschreiben.

Helle und dunkle PNGs für Ladestatus, erhaltene Taubschaltung und Portal-Warnung
liegen bei gesetztem `ICU_UI_SHOTS` im gewählten Verzeichnis. Diese Nachprüfung
verwendete `target/ui-deep-review-shots/`; Ladestatus und Warnung wurden visuell
auf vollständige, lesbare Darstellung geprüft.

Erfolgreiche Prüfungen auf dem UI-Zweig:

- `cargo test --locked -p icu-client --lib ui::settings::tests::`: 10 bestanden.
- `cargo test --locked -p icu-client --test ui_layout`: 36 bestanden,
  einschließlich beider Themes und vorhandener Skalierungs-/Fensterprüfungen.
- `cargo test --locked -p icu-client --test moderation`: 1 bestanden,
  zwei unveränderte Antworten über die echte QUIC-Verbindung.
- `cargo clippy --locked -p icu-client --all-targets -- -D warnings`:
  bestanden, ohne Lint-Ausnahme.
- `cargo fmt --all -- --check` und `git diff --check`: bestanden.

## Grenzen

Die beiden Restriktionswerte werden weiterhin gemeinsam gesendet. Gleichzeitige
Änderungen mehrerer Admins zwischen frischer Antwort und Klick sind durch diese
UI-Korrektur nicht atomar; dafür wäre ein feldweiser Serverbefehl erforderlich.
Es gibt keine Änderung am Wire-Format. Ein fehlgeschlagener Refresh erlaubt
keine anfängliche Aktion mit veraltetem Zustand.

Native Windows-Desktoptests, physische Audio-/Hotkeygeräte, Paketbau und
Deployment gehören nicht zu diesem Quellcode-Nachweis. Es wurden keine
laufenden Dienste oder installierten Pakete geändert. Rücknahme durch Revert
des UI-Commits und Neubau; keine Benutzerdatenmigration erforderlich.

## Nachprüfung des integrierten Privacy-Modus

Auf `0bacf42` blieben zwei Darstellungsfehler: Die Kamera-Seite erlaubte
weiterhin Einschalten und Vorschau während Privacy, obwohl diese Bedienung
nicht die normale Kamera freigab. Die Sprechfläche wertete außerdem eine
gehaltene PTT-Taste als Sendebeleg und übersah die System-Stummschaltung.
Dadurch konnte sie bei ausgeschaltetem Systemmikrofon „Sendet“ anzeigen;
bei lokalem Mute oder Privacy blieb die aktive Sendefarbe sichtbar.

Die Kamera-Seite sperrt jetzt Einschalten und Vorschau mit einer sichtbaren
Erklärung. Eine bereits laufende lokale Vorschau endet und startet nach
Privacy nicht von selbst neu. Die Gerätewahl bleibt für die spätere Nutzung
verfügbar. Auch die gesperrten Schalter im Hauptfenster zeigen ihren Hinweis
beim Darüberfahren. Die Sprechfläche verwendet den tatsächlichen Sendezustand
und berücksichtigt lokale, System-, Setup- und Privacy-Stummschaltung. Die
eigene Teilnehmerzeile und das Nutzerfenster berücksichtigen dieselbe Sperre,
damit sie nicht gleichzeitig „Spricht“ und „Stummgeschaltet“ anzeigen. Auch
bei anderen Teilnehmern haben gemeldete Mute-/Taub-Zustände Vorrang vor einem
noch nachlaufenden Sprechindikator.

Die ergänzten UI-Regressionen prüfen in beiden Themes die gerenderte
Beschriftung und Flächenfarbe für PTT/VAD und alle Stummschaltungen sowie
PTT ohne gesendete Pakete und den noch sendenden Nachlauf. Der Kameratest
bedient eine Vorschau, aktiviert Privacy, prüft die gesperrten Controls und
verifiziert, dass die Vorschau anschließend erst nach einem neuen Klick
beginnt. Der gemeinsame Endbericht dokumentiert die ausgeführten Prüfungen.
