# UI und Einstellungen: Review vom 2026-10-23

Geprüft wurden Hauptfenster, Einstellungen, Einrichtung, Beitritt/Passport,
Verbindungsdetails und Nutzerfenster. Schwerpunkt waren Zustandswechsel,
Tastaturbedienung, zutreffende Audiomeldungen und schmale Fenster.

## Behobene Fehler

- Die Einrichtung verwendete bisher den normalen Stummschalter. Schließen vor
  der Engine-Bestätigung konnte stumm zurücklassen; späteres Schließen konnte
  eine inzwischen bewusst gewählte Stumm-/Taubschaltung aufheben. Hotkeys konnten
  während des Mikrofontests wieder senden. Eine eigene vorübergehende Sendesperre
  trennt die Einrichtung jetzt von der gespeicherten Nutzerentscheidung.
- Kürzelaufnahmen überlebten einen Seitenwechsel. Ein späterer Besuch konnte
  unbemerkt eine zwischenzeitlich gedrückte Taste übernehmen. Navigation beendet
  die Aufnahme einschließlich des nativen Readers; Windows gibt dabei auch den
  Raw-Input-Listener frei. Wiederholung und Erfolg räumen alte Fehlermeldungen auf.
- Der Windows-Fallback speicherte bei Modifikatorkombinationen zusätzlich einen
  nackten physischen Tastencode, den das Backend bevorzugte. Die Kombination bleibt
  jetzt vollständig erhalten.
- Der Mikrofontest zeigte nur den verarbeiteten Pegel. Ein übersteuernder Eingang
  konnte nach Pegelregelung unauffällig aussehen. Übersteuerung wird jetzt vor AEC,
  Verstärkung und Denoise erkannt und auch während der Einrichtung angezeigt.
- Enter auf der Einstellungsübernahme im Passport-Beitritt konnte gleichzeitig
  die Identität importieren. Bestätigung erfolgt über das fokussierte Element.
- Die Sprechfläche war als Knopf erreichbar, reagierte aber nur auf Mausdruck.
  Leertaste/Enter halten jetzt PTT; Loslassen und Fokusverlust beenden es.
  Kombinationen mit Modifikatortasten starten keine Übertragung.
- Einstellungen, Einrichtung und Verbindungsdetails teilen Scrollpositionen nicht
  mehr zwischen unabhängigen Seiten. Die Einstellungsregler stapeln sich bei
  schmalen Fenstern. Eigene Knöpfe erhalten sichtbaren Tastaturfokus und Symbolknöpfe
  einen zugänglichen Namen. Kamera, Kürzelaktionen und Hinweistöne passen ihre
  Anordnung an; Overlay-Positionen sind benannt und per Tastatur auswählbar.
- Ohne Denoise bot die UI wirkungslose VAD-Wahrscheinlichkeitsregler an und zeigte
  leere Sprachmeter. Sie erklärt jetzt die feste Energieschwelle und zeigt passende
  Pegel. Die wirksame manuelle Vorverstärkung bleibt unter „Erweitert“ sichtbar,
  auch wenn die Automatik aktiv ist. Normalisierungsbeschreibungen unterscheiden
  automatischen Pegel und Spitzenschutz.
- Geräteauswahl, Profilwiederherstellung und Empfangsfeedback berücksichtigen
  fehlende Geräte bzw. getrennte Verbindungen. Eine nicht bestätigte Auswahl wird
  nicht als laufendes Standardgerät ausgegeben; frühere Empfangserfolge erscheinen
  nach dem Trennen nicht weiterhin als aktuelle Bestätigung.

## Verifikation

Die Produktions-Widgets werden mit egui_kittest gezeichnet und über echte
Fokus-, Tastatur- und Navigationsereignisse geprüft. PNGs in hell/dunkel und
640-Pixel-Fenstern einschließlich 150-%-Skalierung dienen der Sichtprüfung.
Die Tests verwenden lokale Testidentitäten und Audio-Testbackends; sie belegen
keine manuelle Prüfung eines nativen Windows-Desktops oder realer Audiogeräte.

Abschluss am 2026-10-24 (Europe/Berlin) auf `biest.eulie.de`:

- `cargo fmt --all -- --check` und `git diff --check`: bestanden.
- `cargo test --locked --workspace`: 947 bestanden, 0 fehlgeschlagen,
  12 bewusst ignorierte Tests. Darunter 33 UI-Tests und Netztests für die
  Einrichtungssperre, Erhalt von Stumm/Taub und rohe Übersteuerung trotz leisem
  verarbeitetem Signal.
- `cargo clippy --locked --workspace --all-targets --features
  icu-client/cpal-fallback -- -D warnings`: bestanden.
- Windows GNU: `cargo check --locked -p icu-client --target
  x86_64-pc-windows-gnu --all-targets --no-default-features --features
  aec,gui,camera,tray,overlay` mit Repository-Toolchain: bestanden.
- Alle Einstellungsseiten bei 640 logischen Pixeln in beiden Themes und bei
  100/150 Prozent Skalierung: keine seitlich entweichenden Bedienelemente;
  Kürzel-Chips werden nicht von Aktionsknöpfen verdeckt. Scrollwechsel,
  Passport-Enter, Overlay-Positionen, Tastatur-PTT und Modifikatortasten sind
  als Interaktionen geprüft. PNGs wurden zusätzlich visuell geprüft.

Native Windows-Laufzeittests, Tests mit physischen Audiogeräten und ein
Paket-/Release-Build wurden nicht ausgeführt. Es wurde nichts ausgerollt.
Die Änderungen sind als eigener Git-Commit rücknehmbar (`git revert`).
