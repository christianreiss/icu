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
| `tools/` | `publish-release.py` (Pakete veröffentlichen), `make-og.py` (Vorschaubild `assets/img/og.png`) |
| `deploy/icu.uggs.io.caddy` | Caddy-Vhost (getestet mit `caddy validate` und einem Probelauf) |

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
[Client-Updates](../docs/development/updates.md). Die aktuelle produktive Seite
liegt auf `https://misc.alpha-labs.net/icu/`; deren separat gepflegtes Layout
wird bei einer Paketveröffentlichung erhalten.

## Ausliefern

`web/` ist das Wurzelverzeichnis (`root`) von `icu.uggs.io`; der Vhost
verweigert `tools/`, `deploy/` und `*.py`. Auf crane liegen Vhosts unter
`/etc/caddy/vhosts/<name>.caddy`. Die Seite setzt eine strikte
Content-Security-Policy per `<meta>` (nur eigene Ressourcen); wer Analytics oder
externe Schriften ergänzt, muss sie anpassen. Rechtlich fehlt noch ein
Impressum, falls die Seite öffentlich und nicht rein privat betrieben wird.
