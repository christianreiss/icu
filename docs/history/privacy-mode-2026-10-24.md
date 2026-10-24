# Privacy-Modus — 2026-10-24

Der neue Button **Privacy** unter den Medienschaltern sperrt die ausgehende
Mikrofonübertragung und ersetzt eine eingeschaltete Kamera durch animiertes
TV-Rauschen mit Scanlines und eingebranntem **PRIVACY MODE ON**. Self-Video und
Kameravorschau nutzen dieselbe Quelle wie die Gegenstelle. Die echte Kamera
wird freigegeben. Eine vorher ausgeschaltete Kamera bleibt aus.

Privacy ist ein eigener Sitzungszustand, damit sein Ende keine vorherige
Stummschaltung oder Admin-Sperre aufhebt. Taubschalten bleibt unabhängig nutzbar;
Mikrofon- und Kamera-Hotkeys umgehen den Schutz nicht. Reconnects erhalten
Privacy; ein vollständiger Neustart beginnt wieder mit den gespeicherten
normalen Einstellungen. Das Hauptfenster bleibt 360 px breit und erhält 52 px
zusätzliche Standard- und Mindesthöhe für den Button.

Jeder Wechsel invalidiert Capture-Blöcke und Kameraquellen über eine Epoche.
Der finale Audio-Versand und Video-Abschluss prüfen diese unter derselben
Sperre wie der Privacy-Wechsel. Noch unvollständige Video-Streams werden auch
bei Schreibfehlern und Task-Abbruch zurückgesetzt: Quinn würde sie beim bloßen
Fallenlassen sonst implizit fertigstellen. Beide Empfängerpfade (Relay und
Client) verarbeiten nur vollständig abgeschlossene Frames. Vor dem Entsperren
werden Capture-Ring, Teilframes, Pre-Roll und Filterhistorie verworfen. Bereits
vor dem Umschalten fertig übertragene Medien lassen sich nicht zurückrufen.

Die Tests verwenden einen lokalen Server, zwei echte Client-Engines, Opus und
H.264 sowie Fake-Audio und eine beobachtbare Kameraquelle. Sie prüfen gesendetes
und Self-Video, Kamerafreigabe, fortgesetzten Audioempfang, ausbleibende
Mikrofondatagramme, Wiederherstellung, Admin-/Setup-Sperren und Reconnects.
Ein QUIC-Test blockiert den Versand per kleinem Empfangsfenster und prüft
Stream-Reset sowohl bei Privacy-Abbruch als auch beim Abbruch der Sendetask.
UI-Tests prüfen Buttonaktionen, gesperrte Schalter, Mindestgröße und Self-Video
in beiden Themes.

Reproduzierbare Bildnachweise: `ICU_PRIVACY_SHOTS=<Ordner>` mit dem Test
`privacy` speichert Quelle und empfangenes Bild; `ICU_UI_SHOTS=<Ordner>` mit
`ui_layout privacy -- --test-threads=1` speichert die UI-Ansichten. Der parallele
Screenshot-Lauf der gesamten UI-Suite endete mit SIGSEGV; die gezielten
seriellen Bildtests bestanden. Die komplette Suite wird ohne Screenshot-Export
geprüft. Echte Webcam-Hardware und native Windows-Desktop-Bedienung wurden
hier nicht geprüft. Es erfolgte kein Deployment und keine Protokolländerung.

## Lokale Verifikation

- `cargo test --locked --workspace`: 961 bestanden, 0 fehlgeschlagen,
  12 explizit ignoriert (vor Integration der parallelen Audio-/UI-Fixes).
- `cargo clippy --locked --workspace --all-targets -- -D warnings`,
  `cargo fmt --all -- --check` und `git diff --check`: bestanden.
- `cargo check --locked -p icu-client --no-default-features` sowie mit
  `--features gui`: bestanden, ohne Warnungen.
- Windows-Cross-Check mit `aec,gui,camera,tray,overlay`: bestanden.
- Die zwei gezielten Privacy-UI-Tests samt seriellen Screenshots: bestanden.
