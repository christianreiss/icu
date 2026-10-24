# Deep-Review: Audio, Security und UI/UX — 2026-10-24

Ausgangsstand auf biest.eulie.de: `cae9a8c82813cd1796e70de7a4a45d95915bd83d`.
Drei unabhängige Teilprüfungen untersuchten Audiopegel und Sprachqualität,
Server-/Client-Sicherheit sowie UI/UX und identifizierten acht verbleibende Fehler.

Die Teilberichte beschreiben Fehlernachweise, Implementierung und Grenzen:

- [Empfangspegel und WASAPI-Zeitlücken](audio-deep-review-2026-10-24.md).
- [Loss-Feedback nur von berechtigten Empfängern](security-deep-review-2026-10-24.md).
- [Moderation, Tastenkürzel und Teilnehmerdarstellung](ui-deep-review-2026-10-24.md).

Zusätzlich zu den acht ursprünglichen Befunden reproduzierte die UI-Prüfung
abgefangene Namensklicks: Die Textauswahl verhinderte das Öffnen von
Nutzerfenster und Kontextmenü. Auch dieser Fehler wurde korrigiert.

Die Fix-Commits sind `1df724b` (Konfiguration), `d04f9ca` (Security),
`08a6d02` (UI), `8d83386` und `ce54149` (Audio). Der gemeinsame Quellstand
nach Integration ist `ce54149ab9b95279cdd13d86b647f8eef8a9aba2`.

## Lokale Konfigurationswerte

`Settings::load` akzeptierte syntaktisch gültiges TOML mit `gain_db = nan` und
ungeprüften Verzögerungen. Die nicht endliche Verstärkung machte den Capture-Pfad
stumm; ein maximaler `u32`-Nachlauf entsprach etwa 49 Tagen. Beim Laden werden
nun dieselben endlichen Zahlen und Grenzen angewendet wie beim Passport-Import.
Die Datei bleibt bis zum normalen Speichern erhalten. Mute, PTT und Gerätewahl
werden durch die Validierung nicht verändert.

Der neue Test `config::tests::loaded_numbers_are_safe_for_audio_processing`
scheiterte vor der Änderung mit Exitcode 101 (`NaN` statt `0.0`). Danach bestanden
alle 23 Konfigurationstests. Die Regression prüft `nan`, positive und negative
Unendlichkeit, übergroße Nachlaufzeiten, ungültige Framegröße und Peer-Lautstärken
sowie den Erhalt von Mute und PTT. Keine echten Audiogeräte werden angesprochen.

## Gemeinsamer Endstand

Auf dem integrierten Quellstand `ce54149` bestanden auf `biest.eulie.de`:

- `cargo test --locked --workspace`: **974 bestanden, 0 fehlgeschlagen,
  13 bewusst ignoriert**. Darunter sind echte QUIC-Regressionen, alle sieben
  Opus-Kaskaden, Geräte-/Feedback-Simulationen und 36 UI-Layouttests.
- `cargo clippy --locked --workspace --all-targets --features
  icu-client/cpal-fallback -- -D warnings`: bestanden, ohne Lint-Ausnahme.
- Windows GNU: `cargo check --locked -p icu-client --target
  x86_64-pc-windows-gnu --all-targets --no-default-features --features
  aec,gui,camera,tray,overlay`: bestanden, mit Repository-Toolchain in `PATH`.
- `cargo fmt --all -- --check` und `git diff --check`: bestanden.
- `cargo deny --locked check`: bestanden unter der bestehenden Repository-Policy,
  mit den bereits vorhandenen Duplicate-Version-Warnungen. Lockdatei,
  Abhängigkeitsmanifeste und Ausnahmen wurden nicht verändert.

Die vollständigen lokalen Gesamtprüflogs liegen unter
`target/deep-review-logs/`. Helle und dunkle UI-Vorschauen liegen unter
`target/deep-review-ui/`; Ladestatus, erhaltene Restriktion und Portal-Warnung
wurden zusätzlich visuell geprüft. Das ersetzt keinen nativen Windows-Desktoptest.

Ein unabhängiges Advisor-Review fand im ersten Entwurf der zusätzlichen
Spracherkennung eine Verbindung getrennter stationärer Töne. Ein gezielter
Tracker-Test reproduziert diesen Fall; nach Korrektur der Laufgrenzen,
Bewegungsbestätigung und Aliasfilterung ergab das erneute Code-Review keinen
weiteren konkreten Blocker. Der vollständige Workspace-Lauf prüfte anschließend
auch die letzte Schutzänderung beim Ersetzen nicht endlicher Samples.

Die abschließende Ergänzung von Bericht und Referenz änderte ausschließlich
Dokumentation. Alle Fix-Commits sind in lokalem `main` enthalten; temporäre
Task-Worktrees und ihre gemergten Zweige wurden nach der Integration entfernt.

## Geltungsbereich

Dies ist eine lokale Quellcodeprüfung. Paketbau, Veröffentlichung, Installation
und Änderungen an laufenden Diensten gehören nicht zu diesem Lauf. Physische
Hörprüfung und native Windows-Hardwaretests bleiben eigenständige Nachweise.
Rücknahme durch Revert der betreffenden Fix-Commits und Neubau; keine Migration
von Identitäten oder Benutzerdaten erforderlich.
