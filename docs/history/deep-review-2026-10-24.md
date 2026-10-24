# Deep-Review: Audio, Security und UI/UX — 2026-10-24

Ausgangsstand auf biest.eulie.de: `cae9a8c82813cd1796e70de7a4a45d95915bd83d`.
Die erste Quellcodeprüfung mit drei unabhängigen Subagenten identifizierte acht
verbleibende Fehler. Die Umsetzung begann nach Wiederherstellung der verwalteten
Sitzung; der vorherige Leselauf hatte keine Quelldateien verändert.

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

## Geltungsbereich

Dies ist eine lokale Quellcodeprüfung. Paketbau, Veröffentlichung, Installation
und Änderungen an laufenden Diensten gehören nicht zu diesem Lauf. Physische
Hörprüfung und native Windows-Hardwaretests bleiben eigenständige Nachweise.
Rücknahme durch Revert der betreffenden Fix-Commits und Neubau; keine Migration
von Identitäten oder Benutzerdaten erforderlich.
