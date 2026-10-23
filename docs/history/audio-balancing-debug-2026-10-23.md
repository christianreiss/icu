# Audio-Pegel und Peer-Hints: Debugging 2026-10-23

Ausgangspunkt: Nutzerbericht über zu leise und übersteuerte Sprache trotz
aktivierter Automatik und Empfangshinweisen. Untersucht wurde der Quellstand
ab `0b424703168d247b7a68d66d2cd6b161a58127e8` auf biest.eulie.de.

## Reproduzierte Fehler und Korrekturen

1. **Bestätigte Sprache konnte ihren eigenen Rauschboden bilden.**
   `Normalizer::observe` verlangte auch bei `Some(true)` einen Abstand von
   10 dB zum Minimum derselben Sprache. Ein konstanter bestätigter Pegel bei
   −32 dBFS erhielt nach 20 s noch 0 dB Verstärkung; auch das Nachregeln
   nach oben/unten konnte einfrieren. Bestätigte Sprache lernt nun nicht
   den Rauschboden und darf oberhalb der absoluten Mindestlautstärke den
   Schätzer bewegen. Ohne Sprachbestätigung bleibt die Rauschbodenprüfung;
   Stille, nicht endliche Werte und negative VAD-Urteile bleiben gesperrt.

2. **Capture-Clipping verlor gegen die Reaktionsprüfung des Systemreglers.**
   Nach einer leisen Passage und Pegelanhebung konnte eine natürlich lautere,
   übersteuerte Passage die Antwortprüfung um mehr als 9 dB verfehlen.
   Der Regler pausierte vor der Absenkung. Ein Geräte-Double reproduziert
   diesen Fehler; Capture-Clipping hat jetzt auch in diesem Fall Vorrang.
   Manuelle Eingriffe und bedingte Wiederherstellung behalten ihre Regeln.

3. **Der Stimmkompressor schwächte die Peer-Korrektur ab.**
   Zuvor veränderte der Peer-Tuner das Mikrofon-Ziel vor RNNoise und dem
   festen Kompressor. In der Opus-Kaskade blieben bei +6 dB nachträglicher
   Überhöhung trotz maximalem Zielversatz −3 dB noch −3,75 dB
   Empfangskorrektur nötig. Die langsame Peer-Korrektur sitzt jetzt hinter
   dem Kompressor und vor dem Ausgangslimiter. Der lokale Mikrofon-AGC
   bleibt unabhängig. Grenze ±6 dB, Schritt 0,5 dB pro frischem Regeltakt,
   weiche Rampe über einen Block und Limiter auch bei abgeschalteter
   normaler Ausgangsnormalisierung. Die Korrektur verändert weder die VAD
   noch den Schätzer für die Übertragung auf den Systemregler.

4. **Der Server konnte gespeicherte Berichte mehrfach bestätigen.**
   Jeder 2-s-Tick wiederholte den Konsens bis zum Ablauf des Berichts.
   Ein einzelnes Fenster durfte dadurch mehrere Bestätigungsschritte
   erzeugen. Jetzt erfordert ein neuer gültiger Hint einen neuen Bericht
   aus der zustimmenden Gruppe. Neue Ausreißer frischen alten Konsens
   nicht auf. `None` bei Ablauf/fehlender Einigung bleibt erhalten;
   das Wire-Format ändert sich nicht.

5. **Eine zusätzliche lokale Totbandprüfung konnte passende Hints stoppen.**
   Bei lokal −1,5 dB und Empfänger −3 dB stimmen Richtung und 2-dB-Abstand,
   trotzdem lehnte die alte Prüfung ab. Das Totband wird jetzt am
   Empfangsfehler geprüft. Gültige lokale Messung, gemeinsames Vorzeichen,
   maximal 2 dB Unterschied und Spitzenreserve für Anhebungen bleiben Pflicht.

## Nachweis und Bedienbarkeit

Die Tests prüfen den realen SendChain-Pfad durch Opus, zwei unabhängige
Decoder und `icu_server::audio_feedback::Feedback`. Unterschiedliche lokale
Wiedergabelautstärken müssen identische Rückmeldungen ergeben. Weitere
Regressionen prüfen kurze Sprachpausen, Halten gelernter Korrekturen bei
frischen passenden Hints, Ablauf, Gegenrichtung und Clipping-Reserve.
Ein separater QUIC-Test prüft echte Control-Zustellung, Deafen und alte
Clients ohne Capability. Befehle stehen in [testing.md](../development/testing.md).

Mit der korrigierten Kette und frischen Server-Hints beträgt die abschließende
Empfangskorrektur beim −6-dB-Störfall +1,14 dB statt +2,61 dB ohne Assistenz.
Beim +6-dB-Störfall nach 80 s Simulation sind es −1,65 dB statt −4,10 dB;
die gelernte Ausgangskorrektur beträgt −2,5 dB. Die früher genannten
−5,05/−3,75 dB stammen aus dem kürzeren 40-s-Gegenbeispiel, nicht aus
derselben Messdauer. Beide finalen Fälle liegen im 2-dB-Totband.

Abschlussprüfung: `cargo test --locked --workspace` — 900 bestanden,
0 fehlgeschlagen, 12 ignoriert (geräte-/plattformabhängige Prüfungen).
Der anschließend geänderte Standard für die eigene Overlay-Kachel wurde
zusätzlich mit dem vorhandenen Konfigurations-/Migrationsfall geprüft.
`cargo fmt --all -- --check` und `git diff --check` bestanden.
Striktes Clippy ohne Ausnahme scheitert am unveränderten, inzwischen
veralteten `AtomicUsize::fetch_update` in `icu-server/src/endpoint.rs:432`.
Mit `-D warnings -A deprecated` besteht der gesamte Workspace mit allen Targets.

Peer-Status und Debug-Logs unterscheiden jetzt fehlende lokale Sprache,
widersprechende Messungen, fehlende Spitzenreserve, Zielbereich und
Regelgrenze. „Aktiv“ wird nicht mehr pauschal aus der Bestätigungszahl abgeleitet.

## Betriebsgrenzen

Auf biest.eulie.de lief während der Diagnose `/usr/bin/icu` aus
`icu-client-0.1.16-1.fc43.x86_64`. Die lokale Audio-Konfiguration hatte
Auto-Level, RNNoise, Peer-Assistenz, Systemregelung, Stimmoptimierung und
Ein-/Ausgangsnormalisierung eingeschaltet; manueller ICU-Gain 0 dB.
Die Standardquelle meldete über `wpctl get-volume` 0,50. Diese Angaben
sind Momentaufnahmen, kein Nachweis der tatsächlichen Capture-Spitzen.

Das laufende Log enthielt auch AEC-Referenz-Neustarts. Daraus folgt kein
Nachweis, dass AEC die gemeldeten Pegelprobleme verursacht; während der
Codeprüfung wurden Gerät, AEC und laufende Instanz nicht umkonfiguriert.

Kein physischer Mikrofon-/Hörtest und kein Windows-Lauf sind damit ersetzt.
Peer-Trim kann bereits im Mikrofon/ADC abgeschnittene Wellenformen nicht
wiederherstellen. Zu wenig geeignete Sprache erzeugt weiterhin keine Messung;
die Assistenz verlangt mehrere unabhängige Fenster und reagiert über Sekunden.
Fehlendes Feedback lässt die Korrektur nach sechs Sekunden langsam zurücklaufen.
Die Quellcodeänderung installiert oder veröffentlicht keine Pakete.

Rücknahme: Commit rückgängig machen und vorige Client-/Server-Pakete verwenden;
für die Peer-Ausgangskorrektur genügt vorübergehend `audio.peer_assistance=false`.
Systempegel nur bedingt wiederherstellen, solange ICU noch seinen letzten Wert besitzt.
