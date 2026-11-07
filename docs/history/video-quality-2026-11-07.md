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

## Release-Performance

Der optimierte Release-Client lief am 2026-11-07 für 25 Sekunden mit derselben
UVC-Kamera und isoliertem Loopback-Server: 315 Frames weitergeleitet, 299
Korrektur-Messungen, Mittel 1,06 ms, p95-Obergrenze 1,5 ms, Maximum 1,57 ms.
Keine Budget-Bypässe und keine Late-Drops. Audio war synthetisch mit Fake-Ausgabe;
der geschätzte Decode-/Audioziel-Abstand von −6698 µs bleibt deshalb ausdrücklich
kein physischer Lip-Sync-Nachweis. Es wurden keine Bilder aufgezeichnet.

Die geprüfte Binärdatei ist bytegleich mit `/usr/bin/icu` aus dem ausgelieferten
Fedora-43-RPM (SHA-256
`642b67373f39227a38d71b5eefbde2dc5e4cd178893c6d72edf3db913647c61e`).

## Auslieferung am 2026-11-07

Version 0.1.27 wurde aus dem sauberen Source-Commit
`1a2e8fe748c125af0bb9fc85ae730a39bd6a64fc` gebaut. Das vollständige
`./build.sh --plain` bestand: 46 Testsuiten mit 1072 bestandenen, null
fehlgeschlagenen und 18 ignorierten Tests; Installations-Smokes für Fedora
42/43, Debian 12/13 und AlmaLinux 10; Windows-ZIP und NSIS-Installer. Alle sieben
Pakete und drei BUILDINFO-Dateien bestanden die SHA-256-Prüfung. Linux- und
Windows-Clippy sowie Formatprüfung bestanden ebenfalls.

Auf `lab.uggs.io` wurde das Debian-Serverpaket von 0.1.20 auf 0.1.27
aktualisiert. Unmittelbar vor dem vom Paket ausgelösten Service-Neustart gab
es keine Nutzer, Verbindungen oder Handshakes. Der laufende Server meldet
0.1.27; `doctor --json` meldet null Fehler und null Warnungen. Konfiguration,
CA, Server- und COM-Schlüssel blieben bytegleich; die semantische Nutzerdatenbank
blieb ohne ihren erwartbar fortgeschriebenen CRL-Zähler gleich.

Auf `biest.eulie.de` wurde das Fedora-43-Clientpaket von 0.1.26 auf 0.1.27
aktualisiert. `rpm -V icu-client` ist sauber. Die tatsächlich installierte
`/usr/bin/icu` läuft im vorhandenen Desktop und ist per bestehender mTLS-Identität
mit dem Produktionsserver verbunden; live bestätigt: ein Online-Client.
Identität und Einstellungen sind erhalten, nur die gecachte CRL wurde erneuert.

Alle sieben Pakete liegen unter
`https://misc.alpha-labs.net/icu/downloads/0.1.27/`. Das signierte Update-Manifest
wurde erst nach vollständigem Upload und Hash-/Signaturprüfung atomar aktiviert.
Ein frischer HTTPS-Abruf prüfte die Manifest- und Checksum-Signaturen sowie
Größe und SHA-256 jedes Pakets. Die Download-Seite selbst blieb bytegleich;
ältere Versionen sind erhalten. Hängende SSH-Transporte wurden nach
Bestätigung der Remote-Ergebnisse beendet; die Wiederholung der Veröffentlichung
wurde erfolgreich geprüft. Source-Commit und veröffentlichter Manifest-Commit
sind identisch; spätere Dokumentationscommits ändern diese Provenienz nicht.

## Rückweg und Aufräumen

Rollback benötigt das kompatible Paar: Server 0.1.20 und Client 0.1.26.
Auf `lab.uggs.io` liegen vorheriges Debian-Paket sowie Konfigurations- und
State-Backups in `/root/icu-rollback-0.1.27/`. Lokal bleiben das 0.1.26-RPM
unter `dist/0.1.26/` und das Profilbackup unter
`target/release-0.1.27/local-config-before.tgz` erhalten. Beim Paket-Downgrade
Schlüssel und Nutzerdaten weiterverwenden; Backups nur bei tatsächlich
beschädigtem State zurückspielen. Der bisherige Download-Kanal ist auf
`twingo.alpha-labs.net` als
`/root/icu-rollback-0.1.27/latest-before.json` gesichert und kann atomar
zurückgeschaltet werden.

Wegwerfidentitäten, Probeprozesse, temporäre Release-Helfer und Upload-Staging
wurden entfernt. Pakete, Build-/Verifikationsevidenz und Rückweg bleiben erhalten.
Es wurde kein zusätzlicher Git-Worktree angelegt.
