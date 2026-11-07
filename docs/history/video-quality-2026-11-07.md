# ICU 0.1.27 — automatische Videoklarheit und Medienzeit

## Verhalten

Videokorrektur ist ohne Einrichtung aktiv: schwache räumliche Entrauschung,
begrenzte Schattenaufhellung und vorsichtiger Weißabgleich nur bei belastbaren
neutralen Bildanteilen. Es gibt keine zusätzlichen Einstellungen, Modelle oder
Cloud-Aufrufe. Preview und Encoder verwenden dasselbe Bild. Bildgröße, Codec
und Netzwerktiers bleiben unverändert. Wiederholte Überschreitung von 5 ms
Verarbeitungszeit pausiert die Korrektur automatisch für fünf Sekunden.

Capture läuft unabhängig von Encode/Send und hält nur das neueste Rohbild.
Die gemeinsame monotone Capture-Zeit bleibt über Kamerastarts erhalten. Native
Kamerazeitstempel werden vor RGB-Decode übernommen und auf Plausibilität geprüft.
Audio-Capture trägt Zeit und Epoche mit jedem Sample im festen Ring, damit ein
späterer Callback keine schon gepufferten Daten umdatieren kann.

Die Präsentation orientiert sich je Sprecher an bestätigten Ausgabespannen:
Nur tatsächlich vom Audio-Callback konsumierte App-Samples erhalten eine
Zuordnung zur nativen beziehungsweise geschätzten Gerätezeit. Unverbrauchte
Samples bekommen keine vorläufige Zuordnung; Unterläufe und Gerätewechsel
entwerten alte Anker. Frühe Bilder warten begrenzt, späte Bilder werden verworfen.
Audio wird nicht künstlich verzögert. Ohne frische Audiozuordnung gilt best effort.

Senderarbeit und Empfangslesen/Decode-Queue sind zeitlich begrenzt. Die Queue
hält höchstens zwei Frames je Sprecher, 64 insgesamt und 8 MiB. H.264-Lücken
verlangen weiterhin IDR-Recovery. Privacysperren widerrufen auch laufende Writes.

## Protokoll und Kompatibilität

Minor 8 erweitert beide Header um Capture-Mikrosekunden und Medien-Epoche.
Audioheader: 26 Byte, Datagramme 27–426 Byte; Videoheader: 24 Byte. Bit 1
kennzeichnet geschätzte Capture-Zeit. Audio-Samplezähler und Gain-Epochen behalten
ihre bisherigen Aufgaben. Client und Server müssen gemeinsam aktualisiert
werden; ältere Minors werden beidseitig zurückgewiesen. Rollback benötigt
ebenfalls ein zusammenpassendes Client-/Serverpaar.

## Prüfungen und Grenzen

Regressionen behandeln Camera-Start nach Audio-Start, Camera-Restart, über
Relay/Decode erhaltene Zeitstempel, Epochen, Clock-Sprünge, Wrap-unabhängige
Langlaufzeit, frühen/späten Bildwechsel, Queuegrenzen und vollständige
Write-Timeouts samt Stream-Reset. Unterlauf-Tests verbieten Zuordnungen für
unverbrauchte Audiosamples.

Die bekannte 64×48-Grauflächen-/Rauschfixture wurde über echte H.264-Encoder
und -Decoder in T0–T3 verglichen: 30 Frames pro Tier; Grünkanal-MSE sank von
etwa 40,1–40,3 auf 27,3. Die codierte Datenmenge sank von ungefähr 32,6–33,2 kB
auf 25,6 kB. Das ist synthetische Codec-Evidenz, kein Gesichts- oder
Hardwarequalitätsurteil.

Physische Flash/Klick-Lip-Sync-Messung unter Linux und Windows sowie ein
30-minütiger Hardwarelauf sind getrennte Abnahmen; Quelltests und Cross-Builds
belegen sie nicht. Insbesondere WASAPI-Ausgabe und CPAL-Fallback verwenden
explizit geschätzte Gerätezeit. Der Diagnosewert av_decode_offset_us ist
Decodezeit gegen Audioziel, keine gemessene Schall-/Displaylatenz.

Lokaler Hardware-Capture-Lauf: `/dev/video0`, 20 s, Loopback-Testserver,
synthetische Sprache und Fake-Audioausgabe, keine Bildaufzeichnung: 239 Bilder
präsentiert, native Kamera-Timestamps verfügbar, Decode-Queue maximal 0,20 ms.
Der Debug-Build benötigte für aktive Korrektur im Mittel 9,95 ms und pausierte sie
229-mal automatisch; das belegt die Budget-Sicherung, keine Release-Performance.
Der letzte geschätzte Decode-/Audioziel-Abstand betrug −619 µs und ist kein
physischer Lip-Sync-Nachweis. Native Linux-/Windows-Clippy-Prüfungen bestanden;
481 Client-Unit-Tests und die gezielten Video-/Medienzeitprüfungen bestanden.

Auslieferung und endgültige Build-Evidenz werden nach dem Paketgate dokumentiert.
