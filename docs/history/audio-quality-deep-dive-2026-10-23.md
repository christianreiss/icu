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

## Nachprüfung: Verbindungs- und Capture-Grenzen

Weitere Prüfung auf biest.eulie.de ab `c6bff487ac5f55a412aee34c3ed9e5ba464fffa0`,
mit unabhängigen Reviews für Capture, Empfang und Peer-Regelung. Der unveränderte
Ausgangsstand bestand erneut 918 Tests (12 ignoriert); die vorhandenen Tests
deckten die folgenden Übergänge nicht ausreichend ab.

- **Peer-Regelung nach Reconnect:** `Welcome` ersetzte den Tuner durch Generation
  0, obwohl der DSP seine laufende Generation behielt. Ohne weitere Änderung
  wurden lokale Messfenster und echte Server-Hints dauerhaft verworfen. Jetzt
  werden nur verbindungsbezogene Hinweise gelöscht und eine frische DSP-Kennung
  angefordert. Ausstehende native Pegelkompensation und lokale Kalibrierung bleiben
  erhalten. Ein echter Engine-/QUIC-Test mit Serverneustart scheiterte vorher
  mit Exitcode 101 und nimmt anschließend den Bericht desselben Audiostroms an.
- **Neues Mikrofon mit altem Rauschboden:** Reopen leert jetzt auch Teilframe,
  Pre-Roll, VAD-Anlauf, Sprach-Haltewerte und automatische Schwelle. Alte
  AEC-Diagnosen werden zurückgesetzt. Ein während des Capture-Wartens erfolgter
  Wechsel wird vor Verarbeitung des nächsten Blocks berücksichtigt. Denoise-
  Umschalten verwirft ebenfalls die Schwelle der alten Wahrscheinlichkeitsquelle.
- **Wortanfang bei großen Hardware-Quanten:** Der VAD-Anlauf zählt neben der
  Wanduhr auch verarbeitete Audiodauer. Acht gebündelte Frames bleiben deshalb
  nicht bis zum nächsten Callback geschlossen und verdrängen keinen Wortanfang
  aus dem Pre-Roll. Tests prüfen 10/20-ms-Frames mit 40/80-ms-Anlauf; PTT-Nachlauf
  und VAD-Hangover verwenden weiterhin die Wanduhr.
- **Stale Audio nach Capture-Überlauf:** Der volle Ring enthielt die ältesten
  Samples, während neue verloren gingen. Nun werden verlorene Samplezahlen
  erfasst und der ganze alte Ring samt Teilframe verworfen. Die Sample-Uhr zählt
  alle übersprungenen Samples mit. Regressionen prüfen volle/teilweise Writes,
  einen angebrochenen Frame, Zeitstempelüberlauf und ausschließlich frisches
  Audio nach Erholung. Lücken verwerfen auch unvollständige Feedback-Messungen.
- **Entstummen zwischen Framehälften:** Ein während Stumm verarbeiteter
  10-ms-Block wird vor der Netzwerk-Akkumulation stumm. Beim Entstummen werden
  Capture-Restdaten und Filter-Audio geleert, bei gehaltenen AGC-Verstärkungen.
  Ein manueller Test-Backend schiebt einen lauten Halbframe während Stumm und
  danach nur Stille ein; die tatsächlich übertragenen Opus-Pakete enthalten
  kein lautes Restsignal. Ein separater Kettentest prüft Filterhistorie und
  erhaltene Verstärkung. Der Mikrofontest verarbeitet weiterhin lokale Samples.
- **Kurzer Sprechabschnitt hinter END:** Ein bereits eingereihter Abschnitt
  verlor seinen Vorlauf-Zeitstempel und konnte ohne weitere Pakete für immer
  stumm bleiben. Der Jitter-Puffer erhält nun die früheste noch eingereihte
  Ankunftszeit. Der neue Regressionstest scheiterte vorher mit Exitcode 101.
- **CPAL-Rückfall:** Stimme geht wie bei WASAPI nur auf das erste Kanalpaar;
  LFE/Surround bleiben still. Streamfehler erreichen nun den vorhandenen
  Wiederöffnungsmechanismus statt ausschließlich das Log. Der Capture-Fallback
  setzt weiterhin native Unterstützung für float32/48 kHz voraus.

Die bereits dokumentierte Clippy-Blockade wurde durch die vom verwendeten
Rust-Compiler vorgesehene Umbenennung `AtomicUsize::fetch_update` → `try_update`
behoben; Verhalten und Connection-Limit bleiben gleich, ohne Lint-Ausnahme.

Der erste vollständige Nachprüfungslauf stoppte zusätzlich beim bestehenden
Mute-END-Test (Exitcode 101): Die Startup-Verschiebung legte dessen festen
Vergleichsframe in eine künstliche Silbenpause. Nach Korrektur dieses Vergleichs
wurde ein zweiter Testfehler sichtbar: Der Anfang eines decodierten END-Frames
enthält wegen der Codec-Verzögerung noch Audio vor dem Mute. Der Test prüft nun
den tatsächlich hörbaren Vorlauf und richtet den unveränderten Stille-Grenzwert
an der abgefragten Codec-Verzögerung aus. `Encoder::lookahead_samples()` verwendet
dazu [OPUS_GET_LOOKAHEAD](https://www.opus-codec.org/docs/opus_api-1.6/group__opus__encoderctls.html),
keine fest angenommene Verzögerung. Die Kodierung des END-Frames bleibt unverändert.

### Verifikationsumfang dieser Nachprüfung

- `cargo test --locked --workspace`: 926 bestanden, 0 fehlgeschlagen,
  12 absichtlich ignoriert; einschließlich echter Engine-/QUIC-Regressionsfälle,
  Opus-Kaskade, Zehn-Minuten-Regelungssimulation und Jitter-/Playout-Prüfungen.
- `cargo clippy --locked --workspace --all-targets --features
  icu-client/cpal-fallback -- -D warnings`: bestanden, ohne Ausnahmen;
  der optionale Linux-CPAL-Pfad ist ausdrücklich mitgeprüft.
- Windows-GNU: `cargo check --locked -p icu-client --target
  x86_64-pc-windows-gnu --all-targets --no-default-features --features
  aec,gui,camera,tray,overlay`: mit der dokumentierten Cross-Toolchain bestanden.
- `cargo fmt --all -- --check` und `git diff --check`: bestanden.

Es wurden keine laufenden Dienste, Pakete, Geräteprofile oder Systemlautstärken
geändert. Physische Hörprüfung, nativer Windows-Lauf sowie Paket-/Release-Build
und Paket-Smokes gehören nicht zum Nachweis dieses Quellcode-Laufs.
Kein Wire-Layout-Wechsel. Rücknahme durch Revert des Nachprüfungs-Commits und
Neubau; keine Benutzerdatenmigration erforderlich.
