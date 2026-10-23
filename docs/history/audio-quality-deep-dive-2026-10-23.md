# Sprachqualität, Wiedergabe und Peer-Tuning — 2026-10-23

Vertiefte Prüfung auf biest.eulie.de, Ausgangsstand
`6bc3befa5d732469e6a08fec8accf1a4f406d6d6`. Untersucht wurden Capture/AEC,
RNNoise, VAD, Mikrofon- und Empfangsnormalisierung, native Lautstärkeregelung,
Opus/Jitter/FEC/PLC, Mischung, Geräteprofile und Empfangsfeedback.

## Reproduzierte Fehler und Änderungen

1. **Leise Sprache verlor gegen ihre eigene automatische VAD-Schwelle.**
   Während des 40-ms-Anlaufs (80 ms beim Tippen) war das Gate noch geschlossen.
   Sprachwahrscheinlichkeiten knapp über der Schwelle lernten deshalb den
   Rauschboden nach oben, bis die Sprache vor dem Öffnen verschwand.
   Der Test mit konstant 0,25 Wahrscheinlichkeit reproduziert das. Jetzt
   bleiben auch Sprachkandidaten aus dem Rauschboden heraus. Anlaufzeit,
   Klickunterdrückung und der Watchdog gegen dauerhafte Störgeräusche bleiben
   aktiv; die Kombination aus Schwellenregler und Gate wird gemeinsam geprüft.

2. **RNNoise setzte nach einer Abschaltpause alte Audiodaten fort.**
   Überlappungspuffer und interner Zustand blieben während des Bypasses erhalten.
   Nach 30 Tonblöcken und einer Sekunde Stille mit ausgeschaltetem Denoise
   erschien beim Einschalten noch alter Ton. Der Filter startet nun frisch;
   die folgenden 20 Blöcke und Wahrscheinlichkeiten entsprechen einem neuen
   Filter. Der bekannte erste stumme Aufwärmblock bleibt erhalten.

3. **WASAPI versteckte große Wiedergabeanforderungen.**
   Der Backend-Code zerlegte einen Endpoint-Refill in 240-Sample-Callbacks.
   Der Mixer sah damit höchstens 5 ms Bedarf und hielt nur seine minimale
   Reserve von 20 ms, auch wenn der Endpoint mehr auf einmal abholte.
   Jetzt erreicht eine Anforderung den Mixer vollständig; der Mono-Puffer
   wird vor Streamstart auf Endpoint-Größe angelegt. Tests prüfen 240, 480,
   1440 und 1920 Frames sowie unverändertes Frontkanal-Routing und stumme
   Surround-/LFE-Kanäle. Dies folgt dem
   [WASAPI-Puffermodell](https://learn.microsoft.com/en-us/windows/win32/coreaudio/rendering-a-stream).

4. **Ein Empfängerwechsel konnte Peer-Regelung stoppen.**
   Der Server reichte zuvor den Berichtszähler des ausgewählten Empfängers
   durch. Zähler verschiedener Empfänger haben keinen gemeinsamen Ursprung:
   nach Empfänger A mit Zähler 100 sahen frische Fenster von B mit 0/1 wie
   Wiederholungen aus. Hints erhalten jetzt eine Server-Sequenz pro Sender
   und Pegelkennung. Die separaten Anti-Replay-Prüfungen der Empfängerberichte
   bleiben bestehen. Der Integrationstest reicht echte Server-Hints an den
   Client-Tuner und erreicht nach drei passenden Fenstern die erwarteten
   +0,5 dB statt bei 0 dB stehenzubleiben.

5. **Die Auswahl des jüngsten Messfensters behandelte Vorlauf falsch.**
   Ein zulässiges Fensterende kurz nach der zuletzt gesehenen Sample-Clock
   wurde durch vorzeichenloses Wrapping als sehr alt sortiert. Der Vergleich
   verwendet nun dieselbe vorzeichenbehaftete Altersdifferenz wie die
   Eingangsprüfung. Regressionen decken Sample-Clock-Überlauf, Hint-Zähler-
   Überlauf und Neustart der Zählung bei einer neuen Pegelkennung ab.

## Verifikation

Alle fünf neuen Fehlernachweise scheiterten vor der jeweiligen Korrektur
mit Test-Exitcode 101. Die Windows-Regression prüft den extrahierten
Callback-/Routing-Pfad portabel; sie ersetzt keinen nativen WASAPI-Lauf.

- `cargo test --locked --workspace`: 918 bestanden, 0 fehlgeschlagen,
  12 absichtlich ignoriert. Enthalten sind Opus-Kaskade, zehn Minuten
  simulierte Regelung, echter QUIC-Feedback-Pfad, Klickfilter und Playback
  mit großem Quantum/Netzwerk-Jitter.
- Nach der abschließenden, rein testseitigen Iterator-Anpassung wurde der
  betroffene Callback-/Routing-Test erneut erfolgreich ausgeführt.
- Windows-GNU: `cargo check --locked -p icu-client --target
  x86_64-pc-windows-gnu --all-targets --no-default-features --features
  aec,gui,camera,tray,overlay` mit der dokumentierten Cross-Toolchain bestanden.
- Die strikte Clippy-Prüfung bleibt wegen des bereits vorhandenen
  `AtomicUsize::fetch_update` in `icu-server/src/endpoint.rs:432` rot
  (Exitcode 101). Die gezielte Wiederholung mit `-D warnings -A deprecated`
  bestand und prüft alle übrigen Warnungen; keine dauerhafte Lint-Ausnahme
  wurde eingeführt.
- `cargo fmt --all -- --check` und `git diff --check` bestanden.

## Grenzen und Rücknahme

Keine Änderung von Zielpegeln, maximaler Verstärkung oder Peer-Schrittweite.
Kein Wire-Layout-Wechsel und keine neue Protokollversion. Der korrigierte
Server wird für die beiden serverseitigen Feedback-Fixes benötigt.

Nicht durchgeführt: Paket-/Release-Build, Paket-Smokes, Installation,
physische Mikrofon-/Lautsprecher-Hörprüfung oder native Windows-Hardwaretests.
Kein Push, kein Deployment und keine Änderung an laufenden Diensten oder
Systemlautstärken. Rücknahme durch Revert des zugehörigen Commits und
Neubau; bestehende Geräteprofile bleiben kompatibel.
