# Audio-Nachprüfung: Lautstärkewechsel und Feedback-Zeitlücken

Geprüft auf `biest.eulie.de`, Ausgangsstand `0bacf42` (ICU 0.1.17).
Die gemeinsame DSP-Kette gilt für Linux und Windows, einschließlich des
CPAL-Ausweichpfads. Diese Nachprüfung ergänzt den
[vorherigen Audio-Review](audio-deep-review-2026-10-24.md).

## Lautstärkeänderungen

Die Wiedergabe setzte Teilnehmer- und Gesamtlautstärke bislang als konstanten
Faktor für den gesamten 10-ms-Block. Ein Reglerwechsel bei einem von null
verschiedenen Sample erzeugte damit einen Sprung an der Blockgrenze. Die
Verstärkung nähert sich dem neuen Wert jetzt linear über diesen Block.
Der letzte Wert gehört jeweils zum Sprecher; eine andere Reihenfolge der
Teilnehmer im Mixer übernimmt keine fremde Verstärkung.

Null Prozent und Taubschaltung bleiben sofort still. Beim Aufheben läuft die
Verstärkung von null an. Der erste Block eines neuen Sprechers verwendet
unmittelbar dessen eingestellten Wert. Hinweistöne, Mikrofontest, Normalisierung
und Limiter behalten ihre bisherigen Regeln.

Die Signaltests prüfen die reduzierte Sprunghöhe, den erreichten Zielpegel,
sofortige Stille, Wiederanlauf, unabhängige Sprecherzustände sowie leere Blöcke
und nicht endliche Zielwerte.

## Feedback über verlorene Pakete

Der Mixer gibt nur regulär decodierte Pakete an die Pegelmessung weiter.
PLC, FEC und stille Jitter-Pufferabschnitte werden ausgelassen. Die neue
Stimmerkennung behandelte aufeinanderfolgende Aufrufe trotzdem als zeitlich
zusammenhängend. So konnten getrennte stationäre Töne gemeinsam einen
vermeintlichen Tonhöhenverlauf und damit einen Sprachpegelbericht erzeugen.

Ein Regressionstest mit jeweils zwei 20-ms-Tonpaketen und einem ausgelassenen
Paket reproduzierte das vor der Korrektur (Exitcode 101). Bei einer Lücke im
Absenderzeitstempel verwirft die Messung nun ausschließlich die zeitliche
Spracherkennung. Gelernter Pegel und Rauschboden bleiben erhalten; gewöhnlicher
Zeitstempelüberlauf ist keine Lücke. Die Wiedergabenormalisierung erhält bereits
Stille bzw. explizite Nichtsprachhinweise für solche Abschnitte.

Die zusätzlichen Regressionen prüfen getrennte Tonabschnitte, erhaltenen
Pegel und kontinuierliche Sprache über den `u32`-Zeitstempelüberlauf.

## Nachweise und Grenzen

Die Regression für ausgelassene Pakete wurde direkt gegen die unveränderten
Produktionsmodule mit `rustc --test` reproduziert und besteht nach dem Fix.
Die Signaltests des Mixers wurden ebenfalls direkt ausgeführt. Die gemeinsamen
Cargo-Prüfungen werden im übergeordneten Abschlussbericht dokumentiert.

Keine physischen Hörtests, nativen Windows-Gerätetests, Paketinstallation oder
Änderungen an Produktionsdiensten in diesem Teilauftrag. Rücknahme durch Revert
des Fix-Commits und Neubau; keine Migration von Konfiguration oder Identitäten.
