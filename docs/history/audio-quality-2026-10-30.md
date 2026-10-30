# Sprachqualität und Ducking: Umsetzung und Messung

Stand 2026-10-30; Ausgangspunkt `62b0069719b0602b171304afb390108509c75073`
(0.1.18), Ergebnis 0.1.19, Protokoll-Minor 7 / Audio-Policy 2.
Die tatsächliche Engine einschließlich Lockfile wurde geprüft. Historische
Exporte wurden nicht als laufende Implementierung behandelt. Die unveränderte
Baseline bestand 996 Workspace-Tests (13 ignorierte Hardware/Bench-Tests).

## Findings

| Befund | Bestätigte Ursache und Auswirkung | Änderung / Produktionsstelle | Reproduktion und Nachweis |
|---|---|---|---|
| F01 | Globale Fenstergrenze verdrängt versetzte Reporter | `icu-server/src/audio_feedback.rs`: Verbrauch pro Reporter, zeitlich vergleichbarer Konsens, eigene Hint-Sequenz | 3 versetzte Reporter, Join/Leave, Ausreißer, neue/alte Beiträge; Server-Regressionssuite |
| F02 | Mindestsekunde verwirft natürliche Kurzantworten | `dsp/feedback.rs`, `icu-proto/src/audio_level.rs`: ab 100 ms, begrenzte Historie, Gewicht nach Sprachmenge | 200/400/800-ms-Antworten durch Measurement→Server→Hint→lokales Fenster→Tuner; Zeiten unten |
| F03 | Zwei Totbänder addieren sich | Roher RMS-Fehler zu −22 dBFS; einmalig ±1,5 dB in gemeinsamer Policy | Grenztests und geschlossene Feedback-Kette, akzeptierte Grenzwerte −23,5/−20,5 dBFS |
| F04 | Lokale Bestätigung kann eine andere Phrase betreffen | `Measurement::measure_window`, `PeerTuner::tick_matched`, Engine: passende Sampleclock/Epoche | Leise/laute Wechsel, verspätete Fenster, Epoch-/ID-Regeln, keine passende Historie |
| F05 | Stale Noise-/Sprachdaten; kurze Beobachtung verbraucht Hardwareprüfung | `audio/level.rs`: Ablaufzeiten, erhaltene Prüfung, Engine invalidiert bei Wechsel/Störung | 300 ms→600 ms widersprechende Antwort, Pausen, Quantisierung, manuelle Eingriffe, Restore |
| F06 | Saubere sehr leise Sprache startet nicht | `dsp/normalize.rs`, `chain.rs`: begrenzter Einstieg mit frischer SNR-/Konfidenzbestätigung | Grenz-/Rauschtests und echte Aufnahme bei −55 dBFS; Verbesserung ca. 10 dB, verbleibende Grenze unten |
| F07 | Hangover überträgt Tippen; Reopening umgeht Attack | `gate.rs`, `chain.rs`: neue Attack-Bestätigung, weiche Nichtsprachdämpfung bis 18 dB | 6 Typing-Tests; echte Sprache 216/216 aktive Blöcke, Tastaturrest −17,9 dB |
| F08 | Kanal 0 kann stumm sein; Mono-Referenz löscht Gegenphase | Native Kanalwahl, Profilbindung, bis 8 AEC-Referenzkanäle; AEC3 ohne vorgelagertes Stereo-Wartefenster | Stereo-Gegenphase, 5.1/7.1, Kanal 2; 10 AEC-Tests und native PipeWire-Referenz |
| F09 | Byte-Limit begrenzt kein Alter bei Stillstand | `vendor/quinn-proto`, Client/Relay: 100-ms-TTL bei QUIC-Paketkonstruktion | 306 Vendor-Tests; 200/500/1000/2000-ms-Stalls, langsamer Drain, keine neue Einlieferung, 1/8/32/64 Sprecher |
| F10 | Überhöhtes Jitterziel bleibt bei Dauersprache; Paketkonstanten ändern Zeitwirkung | `dsp/jitter.rs`: Sampledauer, behutsames kontinuierliches Schrumpfen, gealterte Spitzen | 90 s Dauersprache; 10 ms: 140→20 ms, 20 ms: 160→30 ms; WLAN- und Driftläufe |
| F11 Drift | Vorhandene Korrektur ist grob, aber kein unbegrenzter Drift nachgewiesen | Kein unbegründeter zweiter Driftregler | ±50/100/200 ppm, je 10 min und 10/20 ms; 20…60,084 ms Ankunft→Playout, höchstens 10 Korrekturen |
| F11 FEC | Dauer des Folgepakets ist nicht zwingend Dauer des Verlusts | `jitter.rs`, `opus.rs`, Engine: explizite verlorene Sampledauer, reale LBRR-Provenienz | 10↔20 ms und Verlust; FEC-Korrelation 0,97…0,98 statt PLC 0,54 |
| F11 Mix | Soft-Clip ist keine Transparenzgarantie; nichtfinite Werte brauchen Schutz | `mixer.rs`: finite Guard; bestehende Kennlinie nach Matrix beibehalten | 1/2/4/8 Sprecher ×100/200%; 8×100% Residuum −46,3 dB, 8×200% −20,4 dB |
| F11 CPAL | Festes f32/48 kHz und wachsende Callback-Puffer | Native Formate/Raten, begrenzter polyphasiger Resampler und Callback-Speicher | 44,1/48-kHz-/Format-/Chunk-Tests, Feature-Compilechecks; keine AEC-Referenz im Notpfad |
| F11 Clipping | Einzelne starke Impulse können Systempegel herunterziehen | `CaptureWindow` verlangt anhaltende Evidenz | 100 isolierte Clipbursts mit insgesamt 10.000 Clippingsamples senken Hardware nicht; anhaltende Sprache bleibt erkannt |
| F11 Tests | Typing-Tests verwendeten andere Orchestrierung | `Analysis::classify` gemeinsam in Engine, Typing und Probe | Quantitativ erhaltene Sprache nach wirklichem Opus-Decode |
| Zusätzlich | Wiederholte Engine-Ticks können dasselbe gute Fenster mehrfach lernen | `Learner::observe_window` zählt Beobachtungsidentität einmal | Profil-Regression mit wiederholtem Hint |
| Zusätzlich | Privacy-/Denoise-Wechsel können altes Ducking-Sprachbit behalten | Engine verwirft Partials und Sprachdetektor zusammen | Engine-Integration, Muting/PTT/Mikrofontest und Codepfadprüfung |
| Zusätzlich | Große Callback-Batches sehen fälschlich zu wenig Jitterreserve | Engine / `pop_quiet_at`: bereits geplante Wiedergabezeit berücksichtigen | 3 reale Engine-Playouttests ohne Glitches, Dropouts oder künstliche Stretches; eigene Jitterregression |
| Zusätzlich | Automatischer Kanalwechsel löst Reopen aus und setzt die Auswahl zurück | Nichtterminales `CaptureChannelChanged`; native Streams bleiben offen, alte DSP-/Profil-Evidenz verfällt | Selector-Regression und Engine-Test: kein Reopen nach Kanalwechsel, Mute erhalten |
| Neu: Ducking | Native temporäre Absenkung mit korrektem Besitz fehlt | Gemeinsame Hüllkurve, eigener Wächter, bedingtes Journal-Restore, Linux/Windows-Adapter, UI | Engine→echtes QUIC→Decoder→Fake-DAC; native Linux-Abbruchtests und Windows-11-Session-Abnahme |
| Zusätzlich | Terminal-Ctrl+C kann Hauptprozess und Wächter gemeinsam beenden | `audio/ducking/worker.rs`: eigene Unix-Prozessgruppe für den Wächter | Regression mit echtem Kindprozess: vorher falsche PGID, nachher eigene PGID; EOF-Pfad bleibt erhalten |
| Zusätzlich | Paralleler Prozessstart kann eine beendete Profilsperre kurz weiterhalten | `audio/calibration.rs`: explizites Unlock beim Ende der Schreibtransaktion, auch auf Fehlerpfaden | Reproduktion mit dupliziertem Dateideskriptor; vorher blockierte Folgesperre, nachher sofort verfügbar |

## Identische Vorher-/Nachher-Messung

Host **biest.eulie.de**, Fedora 43, AMD Ryzen 9 7950X3D, 32 logische CPUs;
Rust 1.99.1, PipeWire 1.4.11, FFmpeg 7.1.5. Bundled Opus 1.6.1 über
`opusic-sys 0.7.5`, RNNoise `nnnoiseless 0.5.2`, Quinn 0.11.12 / Proto 0.11.18.
Opus bleibt mono/48 kHz/VOIP, 48 kbit/s constrained VBR, Complexity 10, FEC an,
DTX aus; Decoder-Complexity 5. DRED wird nicht gesendet. Deep PLC ist gebaut;
der Nutzen gegenüber einem separaten No-Deep-PLC-Build wurde nicht gemessen.

Korpus: 3,4 s echte VOiCES-Sprache und 17,73 s echte Keychron-Tastatur,
CC BY 4.0, Herkunft/Hashes unter
[Fixtures](../../crates/icu-client/tests/fixtures/audio/README.md).
Einzelner Sprecher, kein repräsentativer Stimmen-/Sprachenquerschnitt.
Synthetische Grenzfälle ergänzen die Aufnahmen.

`examples/audio_quality.rs` wurde als dieselbe Probe gegen zwei getrennte
Bibliotheksstände gebaut. Bei der Baseline verwendet der Probe-Zweig exakt die
alte Produktionsklassifikation; aktuell den gemeinsam genutzten Baustein.
Je 24 s, 10/20-ms-Pakete, neutraler Empfang, kein Paketverlust, keine Hardware,
kein Peer-/Systemtuner im Offlinepfad. Senden, Vorlauf, Opus-Decode,
Empfangsnormalisierung und Mischung sind Produktionskomponenten. Das ist keine
Messung über reale Geräte oder ein reales Netzwerk.

Der Vergleich misst nach 12 s eine **unabhängige Quellsprachmaske**: originale
10-ms-Blöcke oberhalb Maximum−25 dB und −100 dBFS. Ein separater
Hüllkurvenabgleich sucht 0…120 ms Versatz. Ausgegeben werden RMS der gehörten
Samples unter dieser Maske und erhaltene aktive Blöcke über −65 dBFS. Die Maske
stammt nicht aus dem zu bewertenden Regler. Der Eingangspegel bezieht sich auf
den ganzen 3,4-s-Sprachclip, nicht nur die aktiven Teile oder den 24-s-Lauf.

| Eingang / 10-ms-Pakete | Vorher: maskierter Ausgang | Nachher | Erhaltene Quellblöcke nachher |
|---|---:|---:|---:|
| −55 dBFS | −35,727 dBFS | −25,718 dBFS | 99,29% |
| −40 dBFS | −21,746 dBFS | −21,731 dBFS | 100% |
| −30 dBFS | −21,408 dBFS | −21,389 dBFS | 100% |
| −20 dBFS | −21,360 dBFS | −21,357 dBFS | 100% |
| −12 dBFS | −21,364 dBFS | −21,339 dBFS | 100% |

Im getesteten Bereich −40…−12 dBFS beträgt die Spannweite nachher **0,392 dB**
(20 ms: 0,401 dB). Das war bereits in der Baseline gut. −55 dBFS verbessert
sich um etwa 10 dB, erreicht im Kaltlauf jedoch nicht die gleiche Lautheit;
Gain-/Sicherheitsgrenzen bleiben wirksam. Sehr leise, verrauschte, falsch
zugeordnete oder bereits vor dem ADC zerstörte Eingänge sind nicht beliebig
automatisch korrigierbar. Der Warmstart braucht eine passende bestätigte
Geräte-/Kanal-/Policy-Konfiguration.

Zusätzlich wurde FFmpegs `ebur128=peak=true` unabhängig auf **ganze 24-s-Mono-
Ausgaben** angewandt, einschließlich Pausen, mit EBU-Gating und ohne zusätzliche
Sprachmaske. Im Bereich −40…−12 liegt die integrierte Lautheit aktuell zwischen
−23,5 und −22,1 LUFS. Bei −55 steigt sie von −39,0 auf −27,6 LUFS (10 ms).
Maximaler gemessener True Peak: **−0,3 dBTP**; ein −1-dBTP-Ziel ist damit nicht
erreicht und wird nicht behauptet. Auch die Baseline lag maximal bei −0,3.
Die vorhandene Kennlinie bleibt erhalten, statt allein für diese Zahl zusätzliche
ständige Dämpfung oder Lookahead einzuführen.

Reine Tastatur öffnet in beiden Offlinevarianten kein Gate. Der gezielte
unmittelbare Sprache→Tastatur-Test zeigt nach Decode **−36,8→−54,7 dBFS**
Tastaturrest bei −0,1 dB Sprachänderung, 216/216 erhaltenen aktiven Blöcken.
Sprache+Tippen in der separaten 24-s-Probe erhält 99,93% statt 100% der
Quellblöcke: kein Ersatz für eine Konsonanten-/Natürlichkeits-Hörprüfung.

Alle Werte: [vorher.csv](audio-quality-2026-10-30/before.csv),
[nachher.csv](audio-quality-2026-10-30/after.csv),
[LUFS/True Peak](audio-quality-2026-10-30/loudness.csv).
Die CSV-Overheadschätzung zählt 90 Bytes pro Paket: Audioheader 14, ICU-Tag 16,
IPv4/UDP 28, angenommener QUIC-Header 13, AEAD 16, Datagrammrahmen 3. ACKs,
Ethernet, Coalescing und variierende QUIC-Header fehlen: kein gemessener Linkdurchsatz.
CPU-Werte dieser CSV stammen aus Debug-Builds unter paralleler Buildlast und
belegen kein p99-<5-ms-Echtzeitziel.

Eine zusätzliche optimierte Probe ergibt über 18 Fälle maximal **0,308 ms**
Fall-p99 pro 10-ms-Iteration für SendChain, Gate und Opus-Encode/Decode. AEC,
Receive-AGC/Mix, Gerätecallbacks, Transport, Ducking-Klassifikation und echte
Scheduling-Deadlines liegen außerhalb dieser Messung. Sie lief parallel zum
Paketbau auf demselben Host; Details mit Quellcommit unter
[Messaufbau](audio-quality-2026-10-30/release.json) und
[Release-Metriken](audio-quality-2026-10-30/release.csv).

Kurze Antworten mit Pausen ergeben erstmals bei 2 s Wallclock einen verwertbaren
Bericht. Die isolierte geschlossene Feedback-Kette benötigt für 200/400/800-ms-
Antworten 40/24/24 s Wallclock beziehungsweise 4,0/4,8/9,6 s geeignete Sprache,
jeweils bei −28 und −16 dBFS Ausgangslage. Die Grenze ±1,5 dB wird ohne
Maskierung durch Empfangsnormalisierung erreicht. Das ist keine Garantie für
dieselben Zeiten bei beliebigen realen Stimmen/Hardware.

Die Offline-Hüllkurvenverschiebung beträgt ungefähr 50…60 ms (10-ms-Pakete)
beziehungsweise 90 ms (20 ms). **Reale Mund-zu-Ohr-Latenz ist offen.**
Die neue deterministische Produktionsmatrix 10/20 ms × 0/1/3/5/10% Paketverlust
behält die exakte Mediendauer, ohne zusätzliche Stretches/Drops, mit Korrelation
0,9962…1,0 zur sauberen Codec-Referenz. Transport-, Drift-, FEC- und Mixmessungen samt Grenzen stehen im
[Transportbericht](../research/voice-transport-validation.md).

## Ducking und native Nachweise

Die [Policy](../reference/audio-quality.md) beschreibt Signalfolge, Regler,
Defaults, sichtbare Fähigkeiten, Journal und Wiederherstellung. Linux-Tests
verändern ausschließlich eigens erzeugte virtuelle Endpunkte/Streams.

Native PipeWire-Abnahme: expliziter Mikrofonkanal 2 und unabhängige gegenphasige
Referenz bestätigt (Korrelation −1,000000). Ein weiterer nativer Test wechselt
von links auf rechts und bestätigt, dass der Stream auf Kanal 2 weiterläuft. Leicht/Mittel/Stark
wurden nativ mit 6,033/12,015/17,997 dB gemessen; Aus stellt die Baseline wieder
her. Baseline 0,63/0,315 und damit Kanalverhältnis bleiben erhalten.
RMS vor/nach eigener Absenkung wird korrekt vergleichbar zurückgerechnet;
Auto blieb bei konstantem Input in einem Steady-State-Fenster bei
13,515…13,519 dB. Manuelle Eingriffe, kaputtes Journal, Heartbeat-Ausfall,
EOF, harter Senderabbruch, harter Wächterabbruch mit Neustart und App-Abbruch
während Absenkung mit anschließendem App-Neustart wurden erfolgreich geprüft.
Der Senderabbruchtest verwendet einen echten separaten IPC-Senderprozess;
die grafische Anwendung selbst wurde dabei nicht beendet.

Die native Abnahme läuft nun auf dem dedizierten Gast `ICU-WIN11-GATE`,
Windows 11, interaktive Konsolensitzung 1. Die vorhandene Testkonfiguration
ermöglichte den Zugang. 357 Windows-Unit-Tests bestanden; der Schreibkonflikt
wurde zusätzlich 20-mal, Session-Restore fünfmal ohne Fehler wiederholt.
Der WASAPI-Test bestätigt Aufnahme, Wiedergabe und nicht-stille Loopback-
Referenz; absichtlich angehaltene Capture-/Render-Streams werden erkannt.
Die VM ersetzt keine physische Windows-Hardware- oder Hörprüfung.

Auch das tatsächlich installierte NSIS-Paket wurde geprüft: Seine EXE stimmt
mit der portablen ZIP überein. Der paketierte Guardian besteht Aus, Leicht,
Mittel, Stark und den Windows-Auto-Fallback, Heartbeat-Ablauf, EOF sowie
Wächterabbruch mit Wiederherstellung aus dem Journal nach Neustart. Das native
Einladungsfenster wurde mit privatem leerem Profil über DX12-Software-Rendering
sichtbar geprüft. Produktionsidentitäten wurden nicht in den Gast kopiert.
Unter Linux bestand die tatsächliche Fedora-Paket-EXE die isolierten PipeWire-
Modus-, manuellen Pegel- und Crash-/Restore-Prüfungen ebenfalls.

Die native Prüfung fand weitere Fehler, die vor Veröffentlichung behoben wurden:

- Ein beendeter Windows-Prozess kann noch ein COM-Volume-Objekt hinterlassen,
  das einen Restore bestätigt, obwohl die nächste Instanz die Absenkung erbt.
  Nur verfügbare, lebende Sessions bestätigen jetzt Restore; ungelöste Journale
  bleiben erhalten. Der reale App-Neustart-Test bestand anschließend fünfmal.
- Eine manuelle Session-Lautstärke beendete bisher die Absenkung für die gesamte
  Lebensdauer der Session. Nach Ende des Sprechintervalls dient sie nun als neue
  Baseline; während des Intervalls bleibt der manuelle Eingriff maßgeblich.
- Gleichzeitiges atomisches Ersetzen von Dateien kann unter Windows kurzzeitig
  mit `ACCESS_DENIED` scheitern. Der temporäre Handle wird vor Veröffentlichung
  geschlossen, die vollständige temporäre Datei bei transienten Sperren bis zu
  250 ms erneut atomisch veröffentlicht. Die Zieldatei wird dabei niemals vorher
  gelöscht. Die bestehende Parallel-Writer-Regression wurde nativ wiederholt.
- Die Windows-Driftprobe zählte die neue mehrkanalige Referenz noch wie Mono.
  Sie akzeptiert nun die Kanalzahl und zählt Frames statt interleavter Samples.

Auto fällt unter Windows sichtbar auf Mittel zurück, da Peak allein kein
belastbares fremdes RMS liefert. Exclusive-/ASIO-Streams können über diese
Shared-Mode-Schnittstelle nicht gesteuert werden.

## Ergänzende 64-Teilnehmer-Abnahme

Der reale mTLS-/QUIC-Relaytest verwendet 64 gleichzeitige Sender, je 50 Audio-
Pakete/s mit 400-Byte-Payload und 15 Video-Frames/s mit 3.000 Byte. Es sind
Codec-große Testdaten, keine 64 grafischen Clients oder akustischen Endgeräte.
Die bestehende opt-in Suite ist jetzt über `ICU_MEDIA_CLIENTS` auf 2…64 skalierbar.
Ihre Duplikatprüfung verwendet feste Sequenz-Bitmaps, die Latenzmessung ein
Histogramm mit konservativen 0,1-ms-Obergrenzen. Dadurch wachsen die Messdaten
während eines Langlaufs nicht mehr. Alle Leser werden vor dem gemeinsamen
Start initialisiert; der Videogenerator kennzeichnet regelmäßig Keyframes.
Die strikten Zustellungs-, Duplikat- und Latenzprüfungen bleiben erhalten.
Die Testclients verwenden nun dieselben Transportgrenzen wie echte Clients:
132 Video-Streams, 100-ms-Datagramm-Altersgrenze und COM-kompatible MTU-Suche.
Zuvor begrenzte der gemeinsame Testhelfer trotz 64 Teilnehmern noch auf die
36 Streams des 16er-Defaults. Der absichtlich blockierte Empfänger überschreibt
nur die Streamzahl mit null und behält die übrigen Clientgrenzen bei.

Vorher verlor ein zehnsekündiger Lauf 4.076 von 32.000 Audio-Eingangspaketen;
der Kernel zählte 48.475 übergelaufene UDP-Empfangspakete. Kein Client-Datagramm
war an seiner QUIC-Deadline verfallen. Im identischen Test mit angefordertem
1-MiB-Empfangspuffer verschwanden diese Kernel-Verluste und alle Zustellungs-
prüfungen bestanden. Die Änderung begrenzt ausschließlich ICU-Sockets.

Ein anschließender 180-s-Diagnoselauf zeigte zusätzlich 158 durch den
QUIC-Sendepuffer verdrängte und 581 im Client-Empfangspuffer verdrängte
Audiopakete, bei null verfallenen Datagrammen und null QUIC-Paketverlusten auf
dem Serverpfad. Beide 64-KiB-Fanout-Puffer waren für kurze Bursts aller Sprecher
zu klein. Sie sind jetzt auf 512 KiB begrenzt; die 100-ms-Sendefrist bleibt
unverändert. Ein deterministischer Transporttest legt 630 maximale Audiopakete
vor dem ersten Scheduler-Yield ein und prüft anschließend deren vollständige
Zustellung durch beide Warteschlangen, einschließlich Sequenz und Nutzlast.

Mit dem tatsächlichen Produktionscode bestanden anschließend je 30 Sekunden
mit und ohne blockierten Videoempfänger: jeweils 6.048.000 Audiopakete vollständig
zugestellt, Audio-p99 17,124 / 17,499 ms; sämtliche erwarteten Videoframes kamen
bei gesunden Empfängern an. Referenz ist der oben genannte Linux-Host über
Loopback; dies belegt keine WAN-Kapazität oder Mund-zu-Ohr-Latenz des Servers.

## Blindhörprobe

Ein achtsekündiger Ausschnitt Sprache mit Tastaturgeräuschen (Sekunden 12–20,
10-ms-Profil) wurde bei unverändertem Abhörpegel blind vorgelegt. A war die neue,
B die bisherige Verarbeitung. Die Rückmeldung lautete **„Kein klarer Unterschied“**.
Für dieses Beispiel wird kein subjektiv hörbarer Qualitätsgewinn behauptet;
diese einzelne Bewertung ersetzt keine breit angelegte Hörabnahme.

## Physischer Geräteweg

Nach ausdrücklicher Freigabe wurde auf `biest.eulie.de` Buffear (Bose-Bluetooth-Kopfhörer) vor die Kapsel des C-Media-USB-Mikrofons
gelegt. Die 48-kHz-Stereoaufnahme wurde nur im RAM verarbeitet; es wurde keine
Mikrofonaufnahme gespeichert und kein Systempegel verändert.

Unterschiedliche, leise 60-ms-Bandrauschsignale verhindern eine Verwechslung
wiederholter Töne. Zunächst wurden 20/20, anschließend 94/100 Signale eindeutig
korreliert. Der zweite Lauf ergibt unter den auswertbaren Signalen p50/p95
gerundet **270/270 ms** vom Pulse-Ausgabemonitor über den Kopfhörer und die Luft
bis zur USB-Aufnahme. Sechs Signale waren nicht zuverlässig auswertbar.
Die Streuung der zugrunde liegenden Stream-Zeitachsen lag bei etwa 7 bzw. 5 ms;
die Messung beansprucht keine Genauigkeit unter einer Millisekunde.

Das ist eine Gerätewegmessung mit PipeWire/Pulse-Zeitbezug, keine vollständige
ICU-Mund-zu-Ohr-Abnahme: ICU-DSP, Codec und Netzwerk fehlen. Der zunächst
wiederholte Chirp lieferte einen falschen Versatz um eine ganze Wiederholung;
dieser Wert wurde verworfen und durch eindeutige Testsignale überprüft.
Die verbleibende Ende-zu-Ende- und subjektive Hörprüfung wird unten ausdrücklich
weitergeführt; aus diesem Aufbau wird kein bestandenes 150-ms-Ziel abgeleitet.

## Automatisierter integrierter Stand

`cargo test --locked --workspace`: **1044 bestanden, 0 fehlgeschlagen,
16 ignorierte native/optionale Tests, 46 Suites**. Die oben genannten nativen
Linux-Tests wurden separat bewusst ausgeführt. Workspace-Clippy mit
`--all-targets -- -D warnings`, Formatprüfung, CPAL-Featureprüfung sowie Windows-
Compilechecks (einschließlich Tests, mit und ohne GUI/AEC) bestanden. Die
Einstellungsseiten wurden in hellem/dunklem Schema gerendert und auf Geometrie
geprüft; die neue Absenkungsseite und erweiterte Kanalwahl wurden visuell geprüft.

## Reproduktion und offene Abnahme

```sh
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo check --locked -p icu-client --all-targets --features cpal-fallback
cargo test --manifest-path vendor/quinn-proto/Cargo.toml --lib
python3 crates/icu-client/tests/native_channels.py
cargo build --locked -p icu-client
python3 experiments/test-ducking-linux.py --worker-binary target/debug/icu

# Unveränderte Baseline mit eigenem target bauen, danach dieselbe Probe:
git worktree add --detach /tmp/icu-audio-before 62b0069
cargo test --locked --manifest-path /tmp/icu-audio-before/Cargo.toml --workspace
python3 experiments/audio-quality.py --baseline /tmp/icu-audio-before
ffmpeg -i target/audio-quality/after/speech-12-10ms-heard.wav \
  -af ebur128=peak=true -f null -
# Optimierte Teilpfad-Laufzeitmessung, kein vollständiger Echtzeitnachweis:
cargo build --locked --release -p icu-client --example audio_quality --no-default-features
target/release/examples/audio_quality target/audio-quality/release

# 64 gleichzeitige A/V-Teilnehmer, jeweils zehn Minuten:
ICU_MEDIA_CLIENTS=64 ICU_MEDIA_SECONDS=600 cargo test --locked --release \
  -p icu-server --test media_load -- --ignored --nocapture --test-threads=1

# Auf einem nativen Windows-Rechner im Checkout:
cargo test --locked -p icu-client --lib audio::ducking::windows::tests::live_native_session_restore_and_manual_override -- --exact --ignored --nocapture

# Alle sieben Pakete, inklusive kanonischer Packaging-/Smokeprüfungen:
./build.sh --plain
```

Die verbleibende physische/hörbezogene Abnahme ist konkret:

1. Zwei native Clients und Relay, benannte Mikrofone/Lautsprecher; 20 ms RTT
   kontrollieren und akustische Loopback-Aufzeichnung bewusst starten. Anlaut und
   laufende Sprache separat über Kreuzkorrelation messen, mindestens 100 Spurts;
   p50/p95 und Messunsicherheit berichten, Ziel p95≤150 ms.
2. Die generierten `before/after/*heard.wav` bei unverändertem Abhörpegel blind
   vergleichen. Zusätzliche bewusst aufgenommene Flüster-, Lach-, Atem-,
   Konsonanten-/Wortende- und verschiedene Tonhöhen-Beispiele ergänzen. Verständ-
   lichkeit und Natürlichkeit mit und ohne gleichzeitiges Tippen bewerten.
3. Die isolierten Windows-Paketprüfungen bestanden; zusätzlich alle fünf Modi mit
   Musik/Spiel, mehreren Sessions, zweitem Ausgang, manueller Lautstärke/Mute,
   zwei ICU-Instanzen, Hauptprozess- und Wächterabbruch prüfen. Aktuelle Baseline
   vor jedem Test erfassen; nach Ende, App-Neustart und Service-Neustart vergleichen.
4. Auf beiden Plattformen tatsächliche Default-Device-Wechsel, Hotplug,
   Suspend/Resume, Audio-Service-Neustart und akustischen AEC-Double-Talk während
   Ducking prüfen. Virtuelle Endpunkte beweisen die Kanal-/Steuerlogik, nicht diese
   Hardwarekombinationen oder jede externe System-Ducking-Politik. Ein voller
   64-Client-Langlauf mit gleichzeitigem Video wird mit dem obigen Befehl
   separat protokolliert. Eine akustische Verlust-/Reordering-/Burstmatrix
   bleibt offen; synthetische QUIC-Teilnehmer ersetzen keine akustischen Endgeräte.

Paketinstallation und Betriebsabnahme werden nach dem Build getrennt protokolliert.
Für diese Implementierungsprüfung wurden keine regulären Gespräche aufgenommen
und keine globalen Audio-/WirePlumber-/Windows-Policies geändert.

Primärquellen: [RMS-/AGC-Bezug RFC 7874 §4](https://datatracker.ietf.org/doc/html/rfc7874#section-4),
[Windows-Sessionpersistenz und Shared-Mode-Grenze](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nn-audioclient-isimpleaudiovolume),
[WirePlumber Stream-Restore](https://pipewire.pages.freedesktop.org/wireplumber/daemon/configuration/stream.html).
Die konkrete libopus-Version und CTLs wurden zusätzlich direkt im gebauten
`opusic-sys`-Quellbaum und ICU-Wrapper geprüft.
