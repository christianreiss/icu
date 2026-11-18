# Stabile Client-Updates ab 0.1.20

Windows prüft beim Start und anschließend alle sechs Stunden auf eine neuere
stabile Version. **App → Updates → Nach Updates suchen** prüft sofort. Die
Prüfung läuft auf einem eigenen Thread; Netzfehler verhindern weder Start noch
Audio oder Verbindung. Ein Hinweis installiert nichts. Erst **Jetzt
aktualisieren → Herunterladen und neu starten** bestätigt Download,
Installation und Neustart. Bei einer bestehenden Verbindung benennt die
Bestätigung deren Unterbrechung. Ein vom Server gefordertes `min_client` bleibt
als Anforderung sichtbar und verwendet dieselbe Update-Prüfung.

Linux verwendet dieselbe authentifizierte Versionsprüfung, zeigt aber nur den
schließbaren Hinweis auf Paketverwaltung bzw. bisherigen Installationsweg.
Es gibt dort keine Installationsaktion, keinen Artefakt-Download und keinen
Paketmanager-/sudo-Aufruf. **Später** schließt den Hinweis für die Sitzung;
eine manuelle Prüfung oder eine andere verfügbare Version zeigt ihn wieder.

Die veröffentlichte 0.1.19 enthält noch keinen Updater. Der Einstieg in 0.1.20
benötigt deshalb einmalig den bisherigen Installationsweg. Ein älterer
Abnahmebuild mit zurückportiertem Updater ist ausdrücklich ein Testfixture,
kein nachträglich verändertes 0.1.19-Release.

## Vertrauen und Veröffentlichungsformat

`web/tools/publish-release.py` erhält die bisherigen Felder von `latest.json`
für die Webseite. Zusätzlich enthält `authenticated` die Base64-kodierten,
unveränderten JSON-Payloadbytes und eine OpenSSH-SSHSIG-Signatur im Namespace
`icu-update`. Erst nach deren Prüfung wird der Payload ausgewertet. Er enthält
Schema, stabile semantische Version, vollständigen/sauberen Buildstatus,
Quellcommit sowie Dateiname, Größe, SHA-256, Architektur und Installationsart
jedes Artefakts. Vorabversionen und Buildmetadaten sind keine stabilen Releases;
Versionen werden semantisch verglichen, nicht lexikografisch.

Der öffentliche Ed25519-Schlüssel aus `packaging/update-signing.pub` ist im
Client eingebettet. Fingerabdruck:
`SHA256:TUao//IitLvGBtZsflKZy0TeN4EN0Tu8v8MLuBNB8bE`.
`packaging/allowed_signers` erlaubt die Namespaces `icu-update` und
`icu-release`; letzterer signiert weiterhin `SHA256SUMS`.

Vor diesem Stand fehlten Schlüssel und `allowed_signers`. Für die
Veröffentlichung wurde ein eigener Ed25519-Schlüssel außerhalb des Repositorys
angelegt. `ICU_RELEASE_KEY` bezeichnet die private Datei, standardmäßig
`~/.ssh/icu-release` (nur für den Eigentümer lesbar). Sie gehört in eine sichere
Sicherung/Release-Secret-Verwaltung, niemals in Git, Pakete oder Logs. Ein
abweichender Schlüssel wird vom Publisher abgewiesen. Ein Schlüsselwechsel
braucht zuerst ein mit dem bisherigen Schlüssel autorisiertes Client-Update,
das den Folgeschlüssel einbettet. Bei Schlüsselverlust gibt es keinen
unsignierten Ausweichpfad; die Vertrauensbasis muss über den bisherigen
Installationsweg erneuert werden.

Dies ist keine Authenticode-Signatur. Windows kann bei der erstmaligen
Installation bzw. UAC weiterhin einen unbekannten Herausgeber anzeigen.
Der integrierte Updater authentifiziert das gesamte ZIP/Setup kryptografisch,
bevor es verarbeitet oder gestartet wird.

Die Adresse ist fest auf `https://misc.alpha-labs.net/icu` begrenzt. TLS prüft
die vollständige Zertifikatskette; Weiterleitungen werden abgewiesen.
Metadaten liefern keine ausführbaren URLs, Pfade oder Argumente. Downloads
haben Größenlimits und werden gegen die authentifizierte Größe und Prüfsumme
geprüft. Ein Dateihandle verhindert anschließend Veränderungen oder Löschung
bis zum Ende der Installation. ZIPs dürfen ausschließlich `icu.exe`,
`LICENSE`, `THIRD-PARTY-LICENSES.txt` und `LIESMICH.txt` unter dem exakten
Versionsverzeichnis enthalten; Links, doppelte/abweichende Pfade und zusätzliche
Dateien werden abgewiesen.

## Windows-Austausch und Wiederherstellung

Die 64-Bit-Uninstall-Registrierung entscheidet anhand des tatsächlichen
EXE-Pfads zwischen NSIS-Installation und separater portabler Kopie. Ein
Registrierungsfehler führt nicht zu einer geratenen Installationsart. Eine
portable Kopie wird aus dem ZIP aktualisiert; sie erhält keinen
Deinstallationseintrag. NSIS verwendet den bestehenden Installationspfad und
Eintrag. Nutzerdaten und unbekannte Dateien im Programmordner werden nicht
ersetzt.

Ein kopierter, separater ICU-Helfer prüft Auftrag, Signatur, Artefakt und den
ursprünglichen Prozess erneut. Eine installationsbezogene Windows-Sperre
verhindert parallele Updates und weitere Starts während des Austauschs. Der
Helfer hält sie über das Ende des alten Prozesses hinaus. NSIS serialisiert
zusätzlich Setup und Deinstallation.

Alle neuen Dateien und Sicherungen entstehen vor dem Austausch auf demselben
Laufwerk. UAC erfolgt vor dem Schließen des ursprünglichen Clients: beim
maschinenweiten Setup und nur bei fehlendem Schreibzugriff auf einen portablen
Programmordner. Ein UAC-Abbruch lässt den ursprünglichen Prozess laufen.
Der Dateiaustausch wartet auf das konkrete Prozesshandle. Der unelevierte
Helfer startet anschließend den neuen Client mit dem bisherigen Profil und
relevanten Startoptionen; Einladungen werden nicht erneut importiert.
Nach einer Wiederherstellung prüft er die alte EXE erneut und gibt die
Installationssperre frei, bevor er sie als normalen Client startet. Der
Neustart verwendet keine Ereignisse der bereits gescheiterten Update-Sitzung.

Drei UI-Durchläufe und eine anschließende kurze Lebensprüfung bestätigen den
Start. Erst dann dürfen die Sicherungen verschwinden. Der Kandidat wird vor
seiner ersten Instruktion einem Job Object zugeordnet; bei einem Helferabsturz
bleibt kein gescheiterter Kandidat als Dateisperre zurück. Audio-Hilfsprozesse
können ihre Lautstärken über EOF wiederherstellen. Kurz nachlaufende
EXE-Abbildungen werden beim Austausch begrenzt abgewartet. Fremde ICU-Prozesse
werden nicht beendet.

Bei normalem Austausch-/Startfehler wird der eigene Kandidat gestoppt,
die bisherige Installation wiederhergestellt und erneut gestartet. Für einen
Stromausfall bzw. eine extern blockierte Wiederherstellung bleiben Sicherungen:

- Portable: `.icu-update.json` nennt das zugehörige `.icu-update-<nonce>` im
  Programmordner; `old/` enthält die bisherigen Programmdateien. Ein bestätigter
  Abschluss wird vor der Bereinigung markiert, damit eine spätere Bereinigung
  kein erfolgreiches Update zurücknimmt. Ein erneuter bestätigter Updateversuch
  bearbeitet ein ausstehendes Protokoll zuerst.
- NSIS: `.icu-update-stage/old/` enthält die bisherigen Programmdateien und den
  Uninstaller; `previous-version.txt` enthält die bisherigen Versions-/Größenwerte
  des Deinstallationseintrags. Eine verbliebene Staging-Ablage wird nicht
  ungeprüft überschrieben.

Falls der Client nach einem Stromausfall nicht startet: alle ICU-Prozesse
beenden, die vorhandenen festen Programmdateien aus `old/` in denselben
Programmordner zurückkopieren und die Version prüfen. Bei NSIS zusätzlich die
in `previous-version.txt` gesicherten Werte `DisplayVersion`/`EstimatedSize` im
bestehenden ICU-Uninstall-Eintrag wiederherstellen. Erst nach erfolgreichem
Start die zugehörige Staging-Ablage und gegebenenfalls `.icu-update.json`
entfernen. Profilverzeichnisse bleiben unberührt. Bei beschädigten Sicherungen
oder unerwartet veränderten Dateien verweigert der Helfer deren Überschreibung
und nennt den Fehler.

Abgeschlossene Downloadaufträge unter `%LOCALAPPDATA%\icu\updates` werden bei
der nächsten Prüfung bereinigt; eine noch laufende Helfer-EXE kann bis zur
folgenden Prüfung liegen bleiben.

Die Windows-Primitiven folgen den dokumentierten
[ReplaceFileW-Fehlerfällen](https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-replacefilew)
und [Job-Object-Lebenszyklen](https://learn.microsoft.com/windows/win32/procthread/job-objects).

## Künftiges Release veröffentlichen

1. Version erhöhen, Änderungen und Dokumentation committen; der Quellbaum muss
   sauber sein. `./build.sh --plain` führt die kanonischen Prüfungen aus und
   erzeugt alle sieben Pakete. `BUILDINFO.*` müssen denselben Quellcommit tragen.
2. `python3 web/tools/publish-release.py --prepare-only` erzeugt und prüft
   `dist/<version>/SHA256SUMS.sig` und `latest.json`. Ohne passenden privaten
   Schlüssel, bei `DIRTY`, unvollständigen Artefakten oder falschem Quellcommit
   bricht die Veröffentlichung ab. `packaging/release.sh` verwendet denselben
   Vorbereitungsschritt für den bisherigen Release-Upload.
3. `ICU_RELEASE_API_TOKEN=… python3 web/tools/upload-release.py --notes <datei>`
   lädt `dist/<version>/` über die Release-API auf
   `https://misc.alpha-labs.net/icu/` hoch (Token: Fleet-Secret
   `icu.release-api-token`). Der Server prüft beide Signaturen
   (`icu-release`, `icu-update`) gegen seine Kopie von
   `packaging/allowed_signers`, jede Prüfsumme, Vollständigkeit, Dateinamen
   und Downgrades, veröffentlicht dann das Versionsverzeichnis mit einem
   Rename und ersetzt `downloads/latest.json` zuletzt. Veröffentlichte Bytes
   sind unveränderlich; `--no-latest` stellt eine ältere Version nachträglich
   bereit, ohne den Kanal umzuschalten. Die Downloadseite rendert sich aus den
   Dateien auf dem Server; sie wird pro Release nicht mehr bearbeitet. Die
   Notizen (`--notes`, Klartext) erscheinen als „Neu in <version>“.
   Details: [web/README.md](../../web/README.md).
4. Lokal ohne Server: `python3 web/tools/publish-release.py` erzeugt
   `web/downloads/<version>/` und schaltet `web/downloads/latest.json` erst nach
   allen Dateien atomisch frei; dieselben Regeln wie oben.
5. Manifest und Artefakte über HTTPS verifizieren. Build-Erfolg, tatsächlich
   geprüften Windows-Ablauf sowie Server-/Clientinstallation getrennt im
   Auslieferungsprotokoll festhalten.

## Gezielte Abnahme

`cargo test --locked -p icu-client --lib update::` prüft Versionen,
Artefaktauswahl, Signaturen, abgebrochene Streams und die Dateitransaktion.
Auf Windows kommen Archiv-, Mutex-, Ereignis- und Argumenttests hinzu.
`python3 -m unittest discover -s packaging/tests` prüft den Publisher und den
Build-Wrapper. Der Linux-UI-Test
`linux_updates_are_dismissible_information_with_manual_checks_only` prüft die
schließbare Meldung und die ausschließlich manuelle Versionsprüfung.

Für echte Updates eine ältere, ausdrücklich als Fixture markierte Version mit
Updater gegen ein vollständiges neues ZIP und Setup prüfen: portabler Pfad mit
Leerzeichen, bestehende NSIS-Installation, UAC-Abbruch aus einem **nicht erhöhten**
Client, fehlgeschlagener Austausch/Start sowie Hashvergleich der Nutzerdaten.
Ein als Administrator gestarteter Client belegt keinen UAC-Abbruch.

Das nicht standardmäßig aktivierte Feature `update-acceptance` akzeptiert beim
Bauen `ICU_UPDATE_ACCEPTANCE_ROOT` für einen getrennten, temporären Testkanal
unter derselben HTTPS-Domain. Signatur-, TLS-, Pfad- und Artefaktprüfungen bleiben
aktiv. Es verändert keinen Produktivfeed und gehört nicht in Release-Builds.
Nach der Abnahme Testkanal und eigene Testprozesse/-aufgaben entfernen.
