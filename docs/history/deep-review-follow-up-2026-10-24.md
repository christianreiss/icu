# Deep-Review: Nachprüfung des integrierten Clients — 2026-10-24

Ausgangsstand auf `biest.eulie.de`: `0bacf4283969cf4540df760c5527c466d914ee93`
(ICU 0.1.17, einschließlich des ersten Deep-Reviews und des Privacy-Modus).
Drei Subagents prüften Audio, Sicherheit und UI/UX unabhängig. Die folgenden
sechs verbleibenden Fehler wurden im isolierten Arbeitsverzeichnis korrigiert.

## Audio und Lautstärke

- Teilnehmer- und Gesamtlautstärke wechselten innerhalb eines ganzen
  10-ms-Blocks schlagartig. Ein eigener Verlauf je Sprecher glättet nun
  Regleränderungen über diesen Block. Null Prozent und Taubschalten bleiben
  unmittelbar wirksam. Hinweistöne und Mikrofontest behalten ihre Lautstärke.
- Die Empfangsfeedback-Messung konnte über ausgelassene Pakete hinweg getrennte
  Töne als zusammenhängende Sprache bewerten. Eine Lücke im Absenderzeitstempel
  verwirft jetzt die zeitliche Spracherkennung, erhält aber gelernte Pegel.
  Der normale Überlauf des 32-Bit-Zeitstempels unterbricht die Messung nicht.

Die Änderungen liegen in der gemeinsamen DSP-/Mixer-Kette von Linux und
Windows. Der [Audio-Teilbericht](audio-follow-up-2026-10-24.md) beschreibt die
Signalregressionen und deren Grenzen.

## Sicherheit

- Ein Teilnehmer ohne Videoempfang konnte Keyframes anfordern und damit das
  gemeinsame Limit eines Kamerasenders verbrauchen. Der Server prüft jetzt
  Empfängerberechtigung und Limit unter derselben Sperre. Eine administrativ
  taubgeschaltete Sitzung darf ebenfalls nicht anfordern; eine selbst
  taubgeschaltete Sitzung darf weiter Video empfangen und Keyframes anfordern.
- `config::write_atomic` verwendete einen vorhersehbaren `.tmp`-Pfad mit
  Truncate. Eine schon vorhandene Datei mit Modus 0644 behielt diesen Modus
  auch für einen privaten Passport; ein Symlink wurde verfolgt und dessen Ziel
  überschrieben. Gleichzeitig schreibende Aufrufe kollidierten miteinander,
  und bei einem fehlgeschlagenen Rename blieb die temporäre Datei zurück.
  Nun wird eine eigene temporäre Datei exklusiv angelegt, unter Unix mit dem
  passenden Modus bereits vor dem Schreiben. Nach dem Synchronisieren ersetzt
  sie das Ziel atomar; Fehler räumen nur die eigene temporäre Datei auf.
  Windows behält die geerbten Verzeichnis-ACLs und den bisherigen DPAPI-Schutz
  lokaler Identitätsschlüssel. Es wurden keine echten Passports geöffnet,
  migriert oder exportiert.

Der Client verwendet dafür das bereits im Workspace vorhandene `tempfile`
auch zur Laufzeit. Das folgt dem bestehenden Servermuster und dem dokumentierten
[Erstellungs-](https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html)
und [Ersetzungsverhalten](https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist).
Lockdatei und Abhängigkeitsversionen bleiben gleich. Die drei negativen
Keyframe-Fälle und alle vier Dateischreibregressionen scheiterten vor ihrer
Korrektur mit Exitcode 101 und bestanden danach. Die Tests verwenden nur
synthetische Inhalte in temporären Verzeichnissen und einen lokalen QUIC-Server.

## UX und UI-Politur

- Die Sprechfläche zeigte bei gehaltenem PTT trotz System-Stummschaltung
  „Sendet“. Sie folgt jetzt dem Sendezustand und allen Mikrofon-Sperren;
  eigene Teilnehmerdarstellung und Nutzerfenster zeigen denselben Zustand.
- Die Kameraeinstellungen boten während Privacy Aktionen an, die die Engine
  ignoriert. Einschalten und lokale Vorschau sind nun sichtbar gesperrt und
  erklärt. Eine laufende Vorschau endet; die Auswahl des späteren Kamerageräts
  bleibt möglich. Gesperrte Dock-Schalter zeigen ihren Hinweis auch tatsächlich.

Die UI-Regressionen prüfen beide Themes, PTT/VAD, verschiedene Mute-Ursachen,
echte Klicks, Vorschauzustand und Seitenhöhe. Zusätzlich wurde die Anleitung
an den vorhandenen Fedora-Paketnamen und den systemweiten Windows-Installer
mit Administratorrechten angepasst; der Versionshinweis zeigt den Ausgangsstand.

## Verifikation und Auslieferung

Auf dem gemeinsamen Endstand bestanden:

- `cargo test --locked --workspace`: **996 bestanden, 0 fehlgeschlagen,
  13 bewusst ignoriert**. Enthalten sind alle sieben Opus-Kaskaden,
  Geräte-/Privacy-Integration, 40 UI-Tests und 26 Video-Relay-Tests.
- `cargo clippy --locked --workspace --all-targets --features
  icu-client/cpal-fallback -- -D warnings`: bestanden.
- Windows GNU: `cargo check --locked -p icu-client --target
  x86_64-pc-windows-gnu --all-targets --no-default-features --features
  aec,gui,camera,tray,overlay`: bestanden, mit Repository-Toolchain in `PATH`.
- `cargo check --locked -p icu-client --no-default-features`: bestanden.
- `cargo deny --locked check`, `cargo fmt --all -- --check` und
  `git diff --check`: bestanden. Die bestehenden Duplicate-Version-Warnungen
  bleiben; keine neue Ausnahme wurde hinzugefügt.

Die neuen UI-Tests brauchten während der Entwicklung Korrekturen an ihren
Assertions: die aktuelle egui-API, die absichtlich reduzierte Deckkraft eines
gesperrten Widgets und gezeichnete Statusanzeigen ohne Accessibility-Textknoten.
Der abschließende vollständige Lauf prüfte die korrigierten Tests und den
endgültigen Quellstand gemeinsam. Ein weiterer unabhängiger Review prüfte
Export, Gain-Verlauf und Feedback-Reset ohne konkreten Blocker.

Logs und helle/dunkle UI-Bildnachweise entstanden im Arbeitsverzeichnis unter
`target/review-logs/` und `target/review-ui/`; die vier neuen UI-Bilder wurden
zusätzlich visuell geprüft. Bei Integration werden diese Belege unter
`target/deep-review-followup-2026-10-24/` im Hauptcheckout aufbewahrt.

Keine Produktionsdienste oder installierten Pakete wurden in diesem Auftrag
verändert. Physische Hörtests, native Windows-Gerätetests und Paketbau sind
keine Nachweise dieses Quellcode-Reviews. Rücknahme durch Revert des
abschließenden Fix-Commits und Neubau; keine Datenmigration erforderlich.
