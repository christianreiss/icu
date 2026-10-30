# Audioqualität und Absenkung anderer Anwendungen

Stand: 2026-10-30, Audio-Policy 2, Protokoll-Minor 7. Client und Server müssen
gemeinsam aktualisiert werden. [Messbericht und Grenzen](../history/audio-quality-2026-10-30.md).

## Signalwege und Zuständigkeiten

```text
Capture:  echter Eingangskanal → AEC → manueller Gain → primärer Leveler
          → RNNoise → VAD → Sprachkompressor / Peer-Trim / Limiter
          → Gate/PTT + begrenzter Vorlauf → Opus → QUIC
Feedback: regulärer Opus-Decode → Sprach-RMS/Fenster → Server-Konsens
          → passende lokale Sendemessung → kleine Senderkorrektur
Playout:  Decode/FEC/PLC → Empfangsnormalisierung → Teilnehmerpegel → Summierung
          → Masterpegel/Soft-Limiter → gemeinsame Audio-/Sprachenergie-Ringbuffer
          → tatsächlicher Gerätecallback
Ducking:  akzeptierte lokale Sprachpakete + tatsächlich ausgegebene Fremdsprache
          → Hüllkurve im getrennten Wächter → fremde Sessions am selben Ausgang
```

| Grenze | Format / Zeit | Besitzer / Puffer |
|---|---|---|
| Native Callbacks → DSP | f32, Capture mono, Render bis 8 Kanäle, 48 kHz; native Callbackgröße | feste Ringe je 48.000 Frames; keine Datei-/Session-API im Callback |
| DSP | 480 Samples = 10 ms | `icu-dsp`; Rückstau >100 ms wird auf neue 20 ms gekürzt, bei großen nativen Quanten angepasste Grenze |
| Encoder / Transport | 480 oder 960 Samples, Senderclock 48 kHz | DSP/QUIC; Byte-Limits plus 100-ms-Datagramm-TTL |
| Jitter / Decode / Mix | zeitgestempelte Pakete → f32 mono | `icu-mixer`, Zustand getrennt pro Sprecher; begrenzte Jitter-/Playoutpuffer |
| Mix → Gerät | f32 Audio + Sprachenergie synchron | fester Ring 48.000 Frames; tatsächlicher Vorlauf folgt Callbackbedarf, nicht der Kapazität |
| Regler / Status | Skalare, Epochen, Sampleclockfenster | Net-/Kontrollpfad; keine PCM-Historie für Kalibrierung |
| Native Ducking-APIs | Session-/Streamwerte, 20-ms-Kontrolltakt | eigener Wächter, getrennt von Echtzeitcallbacks |

Rohaufnahme-RMS/Peak, RMS nach AEC und RMS am finalen App-Ausgabecallback
werden getrennt als Momentwerte exportiert. Sendefenster und regulärer
Empfangs-RMS sind die unabhängigen Kalibrierpunkte vor Encoder beziehungsweise
vor Empfangsreglern. Anzeige der letzten Momentmessung bedeutet keine
integrierte Lautheit; Hardware-Masterpegel liegt außerhalb dieser App-Messung.

Die langsame Hardware-Korrektur schafft einen brauchbaren Eingang. Die lokale
Sprachregelung behandelt bestätigte Sprache, der Sendernormalisierer den
Sprachpegel am Encoder. Empfangsfeedback korrigiert kleine verbleibende Fehler.
Die Empfangsnormalisierung ist eine lokale Hörhilfe; Teilnehmer- und Masterregler
sind Benutzerentscheidungen. Der Limiter begrenzt die Mischung. Ducking verändert
ausschließlich andere Anwendungen, keinen dieser Regler und keine Messhistorie.

**Pegeldefinition:** Feedback verwendet RMS geeigneter Sprachsamples vor lokalen
Empfangsreglern, Ziel −22 dBFS, einmalige Toleranz ±1,5 dB. Das ist weder LUFS noch
eine normierte P.56-Messung. Maximaler Sample-Peak und Clippinganteil sind eigene
Indikatoren; Sample-Peak allein beweist kein True-Peak-Limit.

Schon 100 ms geeignete Sprache können beitragen. Sechs Sekunden skalare Historie
und Sender-Timestamps verbinden exakt passende lokale und entfernte Fenster.
Epoch-Wechsel entwerten alte Daten. Pro Reporter getrennte Verbrauchsgrenzen
verhindern Verhungern versetzter Beobachter; die Sprachmenge begrenzt die
Bestätigungsgeschwindigkeit. Die [Wire-Semantik](protocol.md) definiert die Details.
Gespeicherte gute Einstellungen passen nur zu Policy, Gerät, tatsächlich
gewähltem Eingangskanal und Verarbeitung; ein Hint zählt höchstens einmal.

Leise Sprache darf bei sehr sicherer Spracherkennung und frischer, ausreichender
SNR-Evidenz einen begrenzten Einstieg erhalten. Unbestätigtes Rauschen oder Tippen
darf keine Gain-Rampe starten. Systemmikrofon-Evidenz verfällt bei Pause,
Geräte-/Verarbeitungswechsel und Diskontinuität. Kurze Antworten bleiben für die
Prüfung einer eigenen Hardware-Korrektur begrenzt erhalten. Einzelne Clipping-
Impulse reichen nicht für eine dauerhafte Hardware-Absenkung.

## Kanäle, Zeit und Transport

Mono-Capture verwendet einen tatsächlichen Eingang, kein L+R-Mittel. Automatik
wählt mit Hysterese einen stabil aktiven Kanal; in den erweiterten Einstellungen
ist der Kanal explizit wählbar (UI zählt ab 1). Ungültige Kanalnummern erzeugen
einen Gerätefehler. Die Kanalwahl ist maschinengebunden und wird nicht mit einem
Passport auf ein anderes Gerät übertragen.

AEC erhält unabhängige Renderkanäle, bis zu acht. Die native AEC3-Konfiguration
verhindert, dass ein vorgeschalteter Stereo-Detektor kurze, gegenphasige Sprache
vorzeitig zusammenmittelt. CPAL berücksichtigt tatsächliches Sampleformat und
Geräterate; ein begrenzter Streaming-Resampler verbindet abweichende Raten mit
48 kHz. Kein Resampler wächst mit der Laufzeit.

QUIC verwirft Datagramme älter als 100 ms auch bei der Paketkonstruktion, wenn
nach einem Stall keine neuen Datagramme eintreffen. Der kleine lokale Patch in
`vendor/quinn-proto` ist absichtlich auf Queue-Alterung und deren Tests begrenzt.
Der Jitterbuffer zählt Sampledauer statt Paketanzahl und reduziert überhöhte
Zielverzögerung auch bei Dauersprache behutsam. FEC versucht die Dauer des
tatsächlich fehlenden Pakets; Diagnosezähler trennen Versuch, enthaltene LBRR-
Redundanz, rekonstruierte Samples und PLC-Fallback.

## Andere Anwendungen beim Sprechen absenken

Standard ist **Automatisch**. Weitere Modi: Aus, Leicht (6 dB), Mittel (12 dB),
Stark (18 dB). Alle Werte beziehen sich absolut auf den unveränderten Ausgangswert
der jeweiligen Anwendung; Wiederholung multipliziert die Absenkung nicht erneut.

Lokaler Trigger erfordert echte Sprache in einem aktuell akzeptierten Sendepaket.
Eine gehaltene PTT-Taste, Vorlauf, Testton, Mikrofontest oder VAD-Hangover allein
reichen nicht. Ferne Sprache zählt erst, wenn der Ausgabecallback die gemeinsam
gepufferten Samples wirklich verbraucht hat, nach Teilnehmer- und Masterpegel.
PLC/FEC, Stummschaltung, Taubschaltung und Lautstärke null lösen nicht aus.
Mehrere Sprecher erzeugen einen gemeinsamen Zustand.

Automatisch strebt etwa 10 dB Abstand zwischen gehörtem Hintergrund und Sprache
an, begrenzt auf 0…24 dB. Hintergrundenergie wird geglättet; die eigene Absenkung
wird bei der Messung herausgerechnet. Die Hüllkurve hat ungefähr 50 ms Attack,
300 ms Hold und 750 ms Release. Keine sprechende Person bedeutet keine
dauerhafte Absenkung, auch in einem lauten Raum. Ein Raumklassifikator ist nicht
angeschlossen: der vorgesehene 12-dB-Zweig bleibt inaktiv, statt Rauschen ungeprüft
als Sprache zu interpretieren. Fehlt eine belastbare Hintergrundmessung, gilt
sichtbar **Automatisch: Mittel**. Ohne aktive fremde Anwendung beträgt sie null.

Unter **PipeWire** werden aktive Verbindungen zum tatsächlich gewählten Sink
aufgelöst. Mehrdeutige Ausgänge werden übersprungen. Passive, begrenzte Monitore
messen die RMS-Energie fremder Ausgabestreams; Sample-Peaks dienen nicht als RMS.
Bis zu 32 gleichzeitig frische Monitore unterstützen Automatisch; fehlen Daten
für einen aktiven Stream, wird auf Mittel zurückgefallen. Verändert wird das
transiente SPA-`softVolumes`-Attribut. Persistierte `channelVolumes`, Balance und
Mute bleiben Eigentum des Benutzers.

Unter **Windows** werden fremde Audio-Sessions des gewählten WASAPI-Endpunkts
mit `ISimpleAudioVolume` abgesenkt. ICU-Sessions werden ausgeschlossen; Kanal-
Balance und Mute bleiben unverändert. Der Peak-Meter erkennt nur Aktivität;
belastbares fremdes Session-RMS ist dort noch nicht verfügbar, deshalb verwendet
Automatisch den sichtbaren Mittel-Fallback. ICU meldet nach Möglichkeit Opt-out
aus Windows-Kommunikationsducking an. Der CPAL-Notpfad zeigt Ducking als nicht
verfügbar, weil seine Gerätebezeichner keine sichere native Routing-Zuordnung
garantieren.

## Restore und Ausfälle

Ein einzelner separater Wächter pro Benutzer hält die nativen Sessions. Der
Hauptprozess liefert alle 40 ms ein kleines Kontrollpaket, kein PCM. Nach 750 ms
ohne frischen Heartbeat wird die Absenkung aufgehoben. EOF, Disconnect, Aus,
Gerätewechsel und suspendähnliche Zeitlücken veranlassen ebenfalls Restore.
Nach EOF versucht der Wächter Restore bis zu 30 s und behält ungelöste Einträge.
Native Verbindungs- und Wächterfehler werden mit Abstand erneut verbunden.

Vor einer persistenten Änderung schreibt der Wächter ein begrenztes Journal in
das lokale ICU-Datenverzeichnis unter `ducking/`. Ein Benutzer-Lock verhindert
konkurrierende Wächter. Journal und Sitzungsidentität erlauben nach einem
Wächterabbruch einen bedingten Restore beim nächsten Start. Es gibt keine
absolute Crash-Garantie, wenn der Wächter selbst beendet wird und niemals wieder
läuft; besonders Windows benötigt dann diesen Wiederanlauf.

Restore ist nur zulässig, wenn der aktuelle Wert noch dem eigenen letzten oder
dem im Journal erfassten vorherigen Wert entspricht. Eine fremde Lautstärke-
änderung übernimmt sofort die Kontrolle; ICU wartet bis zum Ende des Sprechens,
bevor es einen neuen Ausgangswert verwendet. Windows nutzt zusätzlich den
Notification-Kontext, damit manuelle Hin-und-zurück-Änderungen erkannt werden.
Unlesbare Journale führen zu keiner neuen Absenkung. Offene Restore-Verantwortung
wird nicht wegen eines temporären API-Fehlers als erledigt markiert.

Die normale UI zeigt Modus, aktuelle dB, betroffene Anwendungen und einen
Fallback-/Fehlergrund. Erweiterte Diagnose ergänzt Sprachquelle, Kanal, Policy,
Fenster, Reporter, Konsens und Gain-Epoche. Es werden keine Gespräche aufgenommen;
die opt-in Messwerkzeuge verwenden ausschließlich bekannte Testsignale/Fixtures.
