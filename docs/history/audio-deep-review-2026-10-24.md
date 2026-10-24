# Empfangspegel und WASAPI-Zeitlücken — 2026-10-24

Quellcodeprüfung und Korrekturen auf `biest.eulie.de`, Ausgangsstand
`cae9a8c82813cd1796e70de7a4a45d95915bd83d`. Kein Wire-Format-Wechsel.

## Empfangsnormalisierung und Feedback

Unbestätigte Empfangsblöcke lernten zuerst ihren eigenen Mindestpegel als
Rauschboden. Bei konstantem Block-RMS und digital stillen Pausen lagen sie
anschließend nie 10 dB darüber. Der neue Regressionstest mit intonierten
harmonischen Vokalen bei konstant −34 dBFS scheiterte vorher mit Exitcode 101:
nach 20 s betrug die Korrektur noch 0 dB. Derselbe Test prüft auch −10 dBFS.

Die gemeinsame Schätzung für Wiedergabe und Empfangsfeedback ergänzt jetzt
ein konservatives, pegelunabhängiges Indiz für stimmhafte Sprache. Auf einem
40-ms-Fenster bei 8 kHz werden alle 20 ms lokale Autokorrelationsmaxima im
Bereich 70–444 Hz gesucht. Interpolierte Perioden müssen über mehrere Fenster
kohärent variieren. Eine Lücke oder ein Sprung beginnt einen neuen Lauf;
getrennte feste Töne dürfen keinen gemeinsamen Bewegungsbeleg bilden.
Ein Vorfilter und ein Mindestanteil der Energie im analysierten Band halten
schwache Aliase hochfrequenter Töne aus der Entscheidung heraus.
Periodizität allein reicht nicht: feste Töne, Netzbrumm
und seine Harmonischen sollen keinen Sprachbeleg ergeben. Bestätigte Sprache
umgeht auch einen schon verschmutzten Rauschboden und lernt ihn nicht weiter.
Explizite Sprach-/Nichtsprachentscheidungen des Sendepfads bleiben maßgeblich.
Nicht stimmhafte Sprache verwendet weiterhin die bisherige Rauschbodenlogik.

Das ist ein zusätzliches Indiz, keine universelle Geräuschklassifikation.
Ein völlig stationärer harmonischer Ton bleibt absichtlich mehrdeutig;
Geräusche mit sprachähnlicher Tonhöhenbewegung sind nicht sicher auszuschließen.
Zielpegel, Totband und Verstärkungsgrenzen bleiben gleich. Es werden keine
Empfangssamples zusätzlich entrauscht oder in Dateien gespeichert.

Regressionen prüfen leise/laute Vokale mit stillen Pausen, Wiederanlauf nach
einem hohen Rauschboden, Feedback nach Kennungswechsel, tiefe/mittlere/hohe
Stimmen, beliebig fragmentierte Callback-Blöcke sowie stationäres weißes und
gefärbtes Rauschen, 50/60-Hz-Brumm, harmonische/gewechselte Töne,
hochfrequente Sweeps, langsam veränderlichen Lüfterton und Gleichspannungsanteile.
10/20-ms-Blöcke liefern denselben eingeschwungenen Pegel. Nach expliziten
Sprachhinweisen und ungültigen Samples wird keine alte Periodenbestätigung
in die unbestätigte Auswertung übernommen. Bestehende Pegel-, Gain-, Limiter-,
VAD- und Opus-Kaskadentests prüfen die übrige Kette.

Eine optimierte Messung des unveränderten Produktionsmoduls (`rustc --test
-C opt-level=3`, ignorierter Test `sixty_four_speaker_cpu_budget`) benötigte
auf diesem Host etwa 177 ms für 1 s Audio bei 128 Schätzern, also Normalisierung
und Feedback für 64 Sprecher. Das ist ein Indikator für zusätzlichen CPU-Bedarf,
kein Echtzeitnachweis des vollständigen Mixers oder langsamer Zielhardware.

## Native WASAPI-Unterbrechungen

`IAudioCaptureClient::GetBuffer` liefert bei einer Zeitlücke
`AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`. Der Client beachtete bisher nur
`SILENT` und konnte alte Teilblöcke mit Samples nach der Lücke verbinden.
Eine Lücke in einem bereits laufenden Stream verwirft jetzt die gepufferten
Samples, gibt das erworbene Paket frei und stoppt den betroffenen Pump.
Ein eigenes Backend-Ereignis führt über die vorhandene koordinierte
Wiederöffnung von Mikrofon, AEC-Referenz und Wiedergabe. Der DSP verwirft
dabei die alte Zeitachse. Das erste Paket eines neu geöffneten Streams darf
das Flag tragen, ohne eine Wiederöffnungsschleife auszulösen.

Portable Pakettests prüfen den verworfenen Halbblock und die Startmarkierung.
Ein Engine-/QUIC-Test löst Lücken für Capture und AEC-Referenz aus und prüft
gemeinsames Wiederöffnen, erhaltene Stummschaltung und fehlendes Fehlerbanner.
Der native Windows-Pump wird zusätzlich mit der Repository-Toolchain geprüft.

## Prüfkommandos und Grenzen

Auf dem Audio-Branch bestanden: 143 DSP-Tests (2 bewusst ignoriert), beide
portablen Pakettests, die Engine-Regression, Clippy mit `-D warnings` und
`cpal-fallback`, Windows-GNU-All-Targets-Check sowie Format-/Diff-Prüfungen.
Die sieben Opus-Kaskadentests bestanden vor der abschließenden Verschärfung
des Ton-/Lückenschutzes; der übergeordnete Gesamtprüflauf prüft den Endstand.

- `cargo test --locked -p icu-client --lib dsp::`
- `cargo test --locked -p icu-client --lib audio::packet::tests`
- `cargo test --locked -p icu-client --test engine capture_discontinuities`
- `cargo test --locked -p icu-client --test cascade`
- Windows GNU: `cargo check --locked -p icu-client --target
  x86_64-pc-windows-gnu --all-targets --no-default-features --features
  aec,gui,camera,tray,overlay`, mit Repository-Toolchain in `PATH`.

Die abschließenden Gesamtprüfungen stehen im übergeordneten Review-Bericht.
Kein Paket-/Release-Build, keine Installation, kein Deployment und keine
physische Hörprüfung oder native Windows-Audiogeräteprüfung in diesem Lauf.
Rücknahme durch Revert des Audio-Fix-Commits und Neubau; keine Datenmigration.
