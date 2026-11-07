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
