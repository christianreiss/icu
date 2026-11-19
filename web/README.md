# icu.uggs.io

Die Webseite zu ICU: Überblick, Screenshots und Download. Reine statische
Dateien, kein Build-Schritt, keine Anfragen an Dritte (Schriften selbst
gehostet, siehe `assets/fonts/LICENSE-*`). Deutsch ist die Quellsprache,
Englisch schaltet `assets/js/site.js` per Schlüssel (`data-i18n`) um; die
Sprache folgt dem Browser und lässt sich oben rechts wechseln.

| Pfad | Inhalt |
| --- | --- |
| `index.html`, `404.html` | die Seiten |
| `assets/` | CSS, JS, Schriften (woff2-Subsets), Bilder; `img/shots/` sind Kopien von `docs/assets/screenshots/` |
| `downloads/` | Pakete und `latest.json`; **nicht im Git**, wird von `tools/publish-release.py` befüllt |
| `tools/` | `publish-release.py` (signieren, lokal veröffentlichen), `upload-release.py` (per API auf den Downloadserver), `make-og.py` (Vorschaubild `assets/img/og.png`) |
| `portal/` | dynamisches Release-Archiv und Release-API (PHP, siehe unten) |
| `deploy/icu.uggs.io.caddy` | Beispiel für eine rein statische Auslieferung mit Caddy |

## Lokal ansehen

```bash
python3 -m http.server 8765 --directory web   # http://127.0.0.1:8765/
```

`file://` reicht nicht: die Paketliste kommt per `fetch` aus `downloads/latest.json`.

## Release veröffentlichen

Nach `./build.sh --plain` auf einem sauberen, committeten Quellstand:

```bash
python3 web/tools/publish-release.py --prepare-only  # dist/<version> signieren
python3 web/tools/publish-release.py                 # lokalen Kanal veröffentlichen
ICU_RELEASE_API_TOKEN=… python3 web/tools/upload-release.py --notes neu.txt  # Downloadserver
```

Alle sieben Pakete und die drei BUILDINFO-Dateien müssen vollständig sein und
zum aktuellen Quellcommit gehören. `DIRTY`, Prüfsummenfehler und Downgrades
werden abgewiesen. `ICU_RELEASE_KEY` bezeichnet den privaten Ed25519-Schlüssel
außerhalb des Repositorys (Standard `~/.ssh/icu-release`). Sein öffentlicher
Anteil muss dem im Client eingebetteten `packaging/update-signing.pub`
entsprechen. Der Publisher signiert `SHA256SUMS` und die exakten Payloadbytes
von `latest.json`; bisherige Webseitenfelder bleiben kompatibel.

Pakete, BUILDINFO, Prüfsummen und Signatur werden zuerst nach
`web/downloads/<version>/` geschrieben. Erst danach wird `latest.json` atomisch
ersetzt. Bereits veröffentlichte Paketbytes sind unveränderlich; ältere
Versionen bleiben liegen. Beim Übertragen auf einen Webserver gilt dieselbe
Reihenfolge: Dateien vollständig prüfen, Manifest zuletzt freischalten.

Vertrauensmodell, Schlüsselwechsel, Windows-Wiederherstellung und Abnahme:
[Client-Updates](../docs/development/updates.md).

## Downloadseite und Release-API (`portal/`)

Das Release-Archiv unter `https://icu.uggs.io/icu/` ist dynamisch: `index.php` liest
bei jedem Aufruf `downloads/latest.json` und die Manifeste aller
`downloads/<version>/` (bei 0.1.28 `latest.json` statt `manifest.json`).
Angezeigt werden die freigegebene Version, je Paket Größe und Prüfsumme,
die Version aus dem Dateinamen (abweichende Versionen und fehlende Dateien
werden markiert), Server-Pakete, die Notizen „Neu in …“ und alle älteren
Versionen. Pro Release wird nichts mehr an der Seite bearbeitet.

Die Website und die neuen Updater nutzen `/downloads/latest.json` und
`/downloads/<ver>/<datei>` direkt unter `https://icu.uggs.io`. Der Webserver
liefert sie aus demselben Bestand wie das Archiv aus. Ältere Clients brauchen
denselben Feed und dieselben Pakete direkt unter
`https://misc.alpha-labs.net/icu/downloads/`. Diese HTTPS-Endpunkte antworten
ohne Weiterleitung. Der Webserver darf Dateien im Downloadverzeichnis nur
statisch ausliefern und muss den Zugriff auf interne Dateien ausschließen.

API unter `https://icu.uggs.io/api/v1/` (Antworten JSON):

| Methode und Pfad | Token | Wirkung |
| --- | --- | --- |
| `GET releases` | nein | alle Releases, neueste zuerst, plus `latest` |
| `GET releases/latest`, `GET releases/<ver>` | nein | ein Release mit Dateien, `version_matches`, `present`, `notes` |
| `POST uploads` `{"version":"x.y.z"}` | ja | Upload anlegen → `id` |
| `PUT uploads/<id>/<datei>` | ja | eine Datei als Rohdaten (gestreamt, bis 256 MiB) |
| `POST uploads/<id>/commit` `{"latest":true}` | ja | prüfen und veröffentlichen |
| `DELETE uploads/<id>` | ja | Upload verwerfen (verwaiste nach 24 h automatisch) |
| `PUT releases/<ver>/notes` | ja | Klartext „Neu in …“ setzen |

Schreibende Aufrufe (Token-Spalte) nimmt der Server nur aus den Netzen in
`write_networks` an (`config.php`, geprüft gegen `REMOTE_ADDR`), sonst `403`.
Der Webserver muss PHP-FPM die tatsächliche Client-Adresse übergeben.
Lesen und die Seite bleiben öffentlich. Leere Liste = keine Beschränkung.

Der Token (`Authorization: Bearer …`) entscheidet nur, wer hochladen darf.
Was sichtbar wird, entscheiden die Signaturen: Beim Commit prüft der Server
`SHA256SUMS.sig` (`icu-release`) und den `authenticated`-Payload von
`latest.json` (`icu-update`) mit `ssh-keygen -Y verify` gegen
`private/icu-portal/allowed_signers`, dazu jede Prüfsumme, dass alle sieben
Pakete und drei BUILDINFO mit einem sauberen Commit vorliegen, dass jede
Datei zur Version gehört und die sichtbaren Felder dem signierten Payload
entsprechen. Dann wird das Versionsverzeichnis per Rename freigeschaltet und
`latest.json` zuletzt atomisch ersetzt, Byte für Byte wie signiert.
Downgrades, Überschreiben veröffentlichter Bytes und fremde Dateinamen werden
abgewiesen. Der private Schlüssel bleibt auf dem Release-Rechner.

Auf dem Server, außerhalb des Webroots:
`config.php` (aus `portal/config.example.php`, enthält nur den SHA-256 des
Tokens), `allowed_signers` (Kopie von `packaging/allowed_signers`),
`staging/` und `notes/<ver>.txt`. Den Token selbst hält das Fleet-Secret
`icu.release-api-token`. Ein Schlüsselwechsel muss auch diese
`allowed_signers` ersetzen.

Für PHP-FPM verweist `ICU_PORTAL_CONFIG` auf die private `config.php`.
`downloads` in dieser Konfiguration ist der gemeinsame Downloadbestand.
Staging und Downloads brauchen dasselbe Dateisystem für den atomaren Rename.
Der Webserver leitet `/api/v1/<pfad>` intern an `api.php` mit `path=<pfad>`
weiter. Nur die vorgesehenen PHP-Einstiegspunkte sind öffentlich erreichbar.

Test: `python3 -B -m unittest discover -s packaging/tests -p 'test_release_portal.py'`
startet `php -S` mit einem Wegwerf-Schlüssel und prüft Veröffentlichung,
Idempotenz, Downgrade, falsche Signatur, veränderte Dateien und Felder,
fremde Namen, Token und die Seite; mit `ICU_TEST_DIST=<pfad zu dist>` prüft
er zusätzlich das echte signierte Release gegen `packaging/allowed_signers`.

## Ausliefern

Die statischen Seiten, Favicons und `assets/` aus `web/` liegen unter
`https://icu.uggs.io/`. `tools/`, `deploy/`, `portal/` und die README gehören
nicht zur statischen Auslieferung. Die Seite setzt eine strikte
Content-Security-Policy per `<meta>` (nur eigene Ressourcen); wer Analytics oder
externe Schriften ergänzt, muss sie anpassen. Rechtlich fehlt noch ein
Impressum, falls die Seite öffentlich und nicht rein privat betrieben wird.
