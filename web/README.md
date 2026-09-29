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

Nach `./build.sh` (Pakete unter `dist/<version>/`):

```bash
web/tools/publish-release.py            # Version aus Cargo.toml
web/tools/publish-release.py 0.1.3      # oder eine andere
```

Das Skript prüft jede Datei gegen `dist/<version>/SHA256SUMS`, kopiert sie samt
`SHA256SUMS` und `BUILDINFO.*` nach `web/downloads/<version>/` und schreibt
`web/downloads/latest.json`. Die Download-Sektion, die Versionsnummern in den
Befehlen und die Prüfsummen kommen ausschließlich aus dieser Datei. Ältere
Versionen bleiben liegen. Gibt es `SHA256SUMS.sig`, wird sie mitkopiert und in
`latest.json` vermerkt; die Seite behauptet aber keine Signaturprüfung, solange
`packaging/allowed_signers` fehlt (siehe `deploy/README.md` §0).

Steht in `BUILDINFO.*` `DIRTY`, meldet das Skript es und die Seite zeigt es
unter „Downloads prüfen“ an. `packaging/release.sh` würde so einen Build
ablehnen.

## Ausliefern

`web/` ist das Wurzelverzeichnis (`root`) von `icu.uggs.io`; der Vhost
verweigert `tools/`, `deploy/` und `*.py`. Auf crane liegen Vhosts unter
`/etc/caddy/vhosts/<name>.caddy`. Die Seite setzt eine strikte
Content-Security-Policy per `<meta>` (nur eigene Ressourcen); wer Analytics oder
externe Schriften ergänzt, muss sie anpassen. Rechtlich fehlt noch ein
Impressum, falls die Seite öffentlich und nicht rein privat betrieben wird.
