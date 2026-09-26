# Feature-Freeze v1

## Frage

Welche Features enthält ICU v1, was heißt für jedes davon „perfekt" (messbar, wo möglich), was wird ausdrücklich abgelehnt und warum, und welche Grauzonen-Features bekommen welche Empfehlung? Fehlt gegenüber Mumble und TeamSpeak etwas Essentielles?

## Kandidaten

Grundsatz (README): „Nur die Features, die wirklich gebraucht werden. Die sind dafür perfekt." Kandidatenmenge = Liste aus der Aufgabenstellung (enthalten, abgelehnt, Grauzone) plus alles, was die Mumble-Audio-/Shortcut-/Overlay-Doku und die TeamSpeak-Feature-Übersicht zusätzlich nennen.

## Primärquellen

Alle abgerufen am 2026-09-20.

- Mumble Homepage (Feature-Überblick): https://www.mumble.info/
- Mumble User Documentation (Index): https://www.mumble.info/documentation/user/
- Mumble Audio Settings: https://www.mumble.info/documentation/user/audio-settings/
- Mumble Global Shortcuts (Plattformhinweise, Maustasten unter KDE Wayland, evdev): https://www.mumble.info/documentation/user/global-shortcuts/
- Mumble In-game Overlay: https://www.mumble.info/documentation/user/in-game-overlay/
- Mumble GitHub README: https://github.com/mumble-voip/mumble
- TeamSpeak Features Overview: https://www.teamspeak.com/en/features/overview/
- xdg-desktop-portal GlobalShortcuts (für Hotkey-Bindung unter Wayland): https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html
- ICU README: /home/chris/Documents/ICU/README.md

Nicht erreichbar (kein Inhalt): Mumble-Wiki-Seiten zu Whisper und Priority Speaker (leiten auf den Doku-Index um), `https://www.teamspeak.com/en/features/` und `/features/security/` (404). Whisper/Priority Speaker sind daher nur aus der Aufgabenstellung und allgemeinem Wissen beschrieben — unbelegt per Vendor-Doku.

## Befunde

### Mumble (Vendor-Doku)

- Homepage: „Low-latency", „always encrypted communication", „Public/private-key authentication by default", „In-game Overlay – see who is talking, FPS and the current time", „Positional audio", Setup-Wizards; für Admins „Extensive user permission system (ACL)", Ice-Middleware, Web-Interfaces, Channel-Viewer, Authenticators; „multi-channel audio recorder".
- Audio-Einstellungen: Transmission „Voice Activity" (Amplitude oder Signal-to-Noise), „Push To Talk" mit Doppeldruck-Halten und einstellbarer Hold-Zeit, „Continuous"; Quality (kb/s), Audio per Packet (Latenz vs. Bandbreite), Low-Delay-Modus ab 64 kb/s; Maximum Amplification; Echo Cancellation (Disabled/Mixed/Multichannel); Noise Suppression (Disabled/Speex/RNNoise/Both); Audio-Cue bei Sendebeginn/-ende; „Mute cue: Play a sound when Mumble would transmit audio … but you are currently muted"; Idle-Action (Auto-Mute/Deafen); Jitter Buffer; Output Delay; Attenuation anderer Anwendungen beim Sprechen; Positional Audio (Bloom, Min/Max-Distanz); Loopback-Test (lokal/Server).
- Global Shortcuts: Unterstützung „depends on the desktop environment you are using"; Linux event-driven braucht Leserechte auf `/dev/input/event*`; KDE Plasma Wayland: „If you want to use a mouse button as a global shortcut, you must enable Listening for mouse buttons" (seit KDE 6.1); Windows: Xbox-Game-Bar-Trigger blockiert Controller-Eingaben.
- Overlay: hookt sich in die Render-API des Spiels (Windows D3D9/10/11 + OpenGL + Vulkan, Linux/macOS OpenGL), zeigt „connected and/or talking voice participants as well as current FPS and/or the clock time", frei positionierbar; D3D12 „does not display"; Anti-Cheat-Konflikte bei mehreren Spielen. ICU-README schließt genau dieses Hooking aus (Nicht-Ziel „Overlay in exklusivem Fullscreen").
- GitHub-README: „Open Source, low-latency and high-quality voice-chat program written on top of Qt and Opus".

### TeamSpeak (Vendor-Übersicht)

„Crystal-clear audio", „automatic microphone volume adjustment", „background noise reduction, and echo cancellation", „Military-grade AES-based encryption", „Unlimited channels and sub-channels", „Granular permission system", „Server groups and templates", „Ban and kick management", Direct Messaging/Group Chats/Global Chat, Screen Sharing „4K @ 60fps", File Sharing, „custom themes & fonts", Server-Browser/Community-Discovery, Mobile Apps, Skalierung bis Tausende Teilnehmer.

### Ableitung

Beide Produkte sind Mehrzweck-Server mit Hierarchie (Channels, Rechte, Gruppen). Alles Hierarchische fällt für ICU per README weg. Was bleibt und bei beiden als Kern gilt: Audio-Pipeline (Codec, PTT/VAD, Rauschunterdrückung, Jitter-Puffer, Pegel, Cues), Hotkeys inklusive Maustasten, Sprecheranzeige/Overlay, lokale Lautstärke- und Mute-Kontrolle je Teilnehmer, Geräteauswahl, Loopback-Test, Verschlüsselung/Auth. Die Mumble-Liste nennt vier Punkte, die die Aufgabenstellung nicht aufführt und die für ein „perfektes" PTT/VAD nötig sind: Release-Delay (Hold-Zeit), Sende-Cue, Mute-Cue, Jitter-Puffer. Sie sind unten als Attribute enthaltener Features geführt.

## Entscheidung

Tabelle „Feature | v1 | Grund". „Perfekt" = das Kriterium in der Grund-Spalte; Zahlen sind Zielwerte, die Step 3 misst oder Client/IMPLEMENTATION.md als Abnahmekriterium übernimmt.

### Enthalten

| Feature | v1 | Grund / „perfekt" heißt |
| --- | --- | --- |
| Connect per Einladung | ja | **Revidiert 2026-09-24 (Registrierungs-Bundle statt Link).** `icu://`-Links gibt es nicht mehr. Eine Einladung ist ein Registrierungs-Bundle (`PROTOCOL.md` §2.3): eine `.icu`-Datei (MIME `application/x-icu`, Dateizuordnung 2026-09-26) oder derselbe Text `-----BEGIN ICU PASSPORT-----` zum Einfügen. Ein Doppelklick auf die Datei öffnet ICU; das Erststart-Fenster zeigt Server, Adresse und CA-Fingerabdruck und fragt nach dem eigenen Namen, „Beitreten“ holt im selben Schritt das Client-Zertifikat und führt in den Raum. Zweitverbindung: Doppelklick auf das Icon. |
| Reconnect automatisch | ja | Nach Netzverlust oder Server-Neustart verbindet der Client ohne Nutzeraktion neu (Backoff 1 s → max. 15 s, unbegrenzt); Zustand (Mute, Deafen, Lautstärken, Gerätewahl) bleibt erhalten; Audio läuft nach Wiederverbindung ohne Neustart; UI zeigt einen Zustand „verbinde…" und nie einen modalen Fehlerdialog. |
| Teilnehmerliste mit Speaking-Indicator | ja | Anzeige wird primär aus den empfangenen Audioframes abgeleitet; das `speaking`-Flag im Control-Kanal (`protocol/wire.md`) ergänzt sie für deafened Clients und markiert das Ende eines Talk-Spurts; Ziel: Indicator leuchtet ≤ 1 Audioframe (20 ms) + 1 UI-Frame nach Ankunft des ersten Pakets, erlischt nach ≤ 2 Frames Stille; Ende-zu-Ende (PTT-Druck beim Sprecher → Anzeige beim Hörer) ≤ 100 ms im LAN-nahen Fall; nie „hängender" Indicator nach Disconnect (Server-Leave-Event + Timeout). Liste zeigt Name, Mute-/Deafen-Status, Kamera-Status. |
| Mute | ja | Schnitt vor dem Encoder, wirkt ≤ 1 Frame; Status wird an alle gemeldet und in der Liste angezeigt; Hotkey-fähig; Mute-Cue: Sprechen bei aktivem Mute zeigt einen sichtbaren Hinweis (optional Ton), wie Mumbles „Mute cue". |
| Deafen | ja | Impliziert Mute; stoppt die Wiedergabe sofort; Status gemeldet; Hotkey-fähig; Undeafen stellt vorherigen Mute-Zustand wieder her. |
| PTT/VAD-Umschalter mit Hotkey-Bindung inkl. Maustasten | ja | Global (wirkt bei fokussiertem Spiel): Windows per Raw Input (`RIDEV_INPUTSINK`), Linux per Portal GlobalShortcuts, opt-in evdev (Gruppe `input`) für Maustasten und Compositoren ohne Portal (Entscheidung in `hotkey-wayland.md`); Maus-Seitentasten und Tastenkombinationen bindbar; PTT-Latenz Tastendruck → erster Frame gesendet ≤ 1 Frame; Release-Delay konfigurierbar (Default 150 ms); optionaler Sende-Cue (Default aus). VAD: RNNoise-basierte Sprachwahrscheinlichkeit mit Schwelle im Pegelmeter sichtbar, Hangover 1 s, einstellbar 0–3 s (2026-09-24; vorher 200–300 ms); Umschalten PTT↔VAD per Klick und Hotkey. |
| Lautstärke pro User (lokal) | ja | Schieberegler 0–200 % je Teilnehmer, persistent über Sessions (Schlüssel: die stabile `user_id`; 2026-09-20, vorher Cert-Fingerprint — Fingerprints anderer sieht der Client nie, `Client/IMPLEMENTATION.md` §11), Wirkung sofort; 0 % = lokales Mute (deckt Grauzone „Per-User-Mute" ab). |
| Eigene Input-/Output-Device-Wahl | ja | Geräteliste live (Hot-Plug: neues Gerät erscheint, entferntes Gerät fällt ohne Absturz auf Default zurück); Auswahl persistent per Gerätename; Wechsel ohne Reconnect; Default-Gerät des Systems als Option „System-Standard". |
| Input-Pegel-Anzeige | ja | Peak/RMS-Meter ≥ 30 Hz Aktualisierung, mit eingezeichneter VAD-Schwelle und Clipping-Markierung; sichtbar im Hauptfenster (Sprechfläche; im Settings-Dialog kein eigenes Meter, Stand 2026-09-26); Loopback-Test (eigene Stimme lokal hören) als Teil davon — umgesetzt 2026-09-26 als Mikrofontest („Testen“ neben der Mikrofon-Auswahl in ⚙ → Audio): spielt die eigene Stimme hinter der ganzen Sendekette lokal ab, endet nach 60 s von selbst. |
| Rauschunterdrückung | ja (Attribut Audio) | RNNoise (nnnoiseless) standardmäßig an, abschaltbar; kein weiteres Verfahren. Mumble bietet Speex/RNNoise/Both — ICU nur RNNoise. |
| Echo Cancellation | ja (**Revidiert 2026-09-20 (AEC)**) | WebRTC AEC3 (`webrtc-audio-processing` ~2.1) als erste Stufe der Capture-Kette, vor der Rauschunterdrückung; Default an, ein Schalter im Settings-Dialog. „Perfekt“ heißt (Zahlen revidiert 2026-09-25/26, `aec.md` „Korrektur“: die 30-dB-Werte stammten aus Messungen mit fehlerhaftem Rauschgenerator): auf der synthetischen Szene eingeschwungene **ERLE ≥ 25 dB** im Single-Talk (gemessen 28,9/27,4), **≥ 22 dB** in den Sprechpausen während Double-Talk (25,6), **≥ 23 dB nach 10 min** bei 100 ppm Gerätedrift (23,2), akustisch mit echtem Lautsprecher noch zu messen (A2), und das **auch neben einem laufenden Spiel** — dafür ist das Referenzsignal der komplette Systemmix (Linux: Sink-Monitor über `stream.capture.sink`; Windows: WASAPI-Loopback, Pfad offen), nicht nur die eigene Wiedergabe. Preis: 8,96 ms Sendelatenz, 0,47 % eines Kerns. Mumble bietet Disabled/Mixed/Multichannel — ICU hat nur an/aus, weil die Kanalwahl aus dem Backend folgt. Belege in `aec.md`. |
| Jitter-Puffer / Playout | ja (Attribut Audio) | Adaptiver Jitter-Puffer mit Opus-PLC/FEC; Zielwert Playout-Delay im LAN-nahen Fall ≤ 40 ms, Messwerte in Step 3. Unsichtbar für den Nutzer, kein Einstellknopf. |
| Kamera an/aus | ja | Ein Toggle (Hotkey-fähig); feste kleine Auflösung/Framerate (Vorschlag 320×240 @ 15 fps, final im Codec-Dokument); Vorschau vor dem Einschalten; Voice-Latenz und -Qualität messbar unverändert, ob Kamera an oder aus (separater Stream/Datagramm-Priorität). |
| Overlay an/aus mit Position | ja | Nur für Borderless-Windowed-Spiele: eigenes transparentes Always-on-top-Fenster, click-through, zeigt sprechende Teilnehmer (Name + Kamerabild, falls an); Position: vier Ecken + Offset, per Drag im Bearbeitungsmodus; Linux: Wayland layer-shell auf KDE/wlroots, GNOME dokumentiert nicht unterstützt (Overview-Record). Kein Hooking. Stand 2026-09-26: vier Ecken plus Mitte oben/unten und Versatz in ⚙ → Kamera, **kein Drag-Bearbeitungsmodus** (`Client/IMPLEMENTATION.md` §16); ohne Layer-Shell (GNOME) fehlt die Overlay-Kachel. |
| Tray-Icon | ja | Zeigt Zustand (verbunden/getrennt, Mute, Deafen) als Icon-Variante; Menü: Mute, Deafen, Fenster zeigen, Beenden; Schließen des Fensters minimiert wahlweise in den Tray oder in Taskleiste/Dock oder beendet (ohne Tray-Host oder unter Wayland Taskleiste/Dock); Linux via KSNI (kein GTK). |
| Settings-Dialog minimal | ja | Ein Fenster, alle Änderungen wirken sofort (kein OK/Apply), Bereiche: Audio (Geräte, Pegel, Modus, Hotkeys, Release-Delay, Cues, Rauschunterdrückung), Kamera (Gerät, Vorschau), Overlay (an/aus, Position), Anzeigename. Stand 2026-09-26: Seiten Audio (Geräte mit Mikrofontest, Sprechen, Hinweise), Klang (Aufbereitung, AEC), Tastenkürzel, Kamera (mit Vorschau und Overlay), Allgemein (Name, Passport, Gerät), Nutzer (nur Admins), Über ICU; Technisches unter „Erweitert“; keine Seite braucht einen Scrollbalken. |
| Einrichtungsassistent | ja (2026-09-26, Auftraggeber-Wunsch) | Nach dem ersten Beitreten führt ein eigenes Fenster „ICU — Einrichtung“ in vier Schritten durch das, was am ersten Abend zählt: Geräte (Mikrofon mit Pegel und Mikrofontest, Lautsprecher, Lautstärke), Sprechen (Automatisch oder Taste halten; mit Taste „Aufnehmen“ und eine Anzeige, ob sie bei ICU ankommt), Klang (die Aufbereitung aus ⚙ → Klang, mit Mikrofontest zum Vergleichen) und eine Übersicht mit „Ändern“ je Zeile; Zurück/Weiter, „Fertig“. „Perfekt“ heißt: kein Schritt braucht einen Scrollbalken (`tests/ui_layout.rs`), jede Wahl wirkt sofort und lässt sich im Fenster prüfen, solange es offen ist, verlässt nichts den Rechner (stumm), ein Klick überspringt es für immer, und ⚙ → Audio → „Assistent“ holt es jederzeit zurück. Mumble wirbt mit Setup-Wizards (Homepage); ICU fragt nur das Nötige — Kamera und Overlay bleiben in den Einstellungen (`Client/IMPLEMENTATION.md` §10). |
| Join/Leave-Hinweis | ja (Empfehlung Grauzone) | Kurzer Ton + Tray-Benachrichtigung bei Join/Leave, beides einzeln abschaltbar, Default: Ton an. |
| Anzeigename ändern | ja | **Revidiert 2026-09-22 (Name im Zertifikat).** Der Name wird beim Enrollment vom Nutzer gewählt und reist im CSR-CN; eine spätere Änderung im Settings-Dialog löst ein Cert-Renewal aus, das ein neues Zertifikat mit dem neuen CN ausstellt. Identität bleibt der Fingerprint. Das alte Zertifikat gilt, bis sich das neue zum ersten Mal anmeldet (revidiert 2026-09-25, `PROTOCOL.md` §2.2 Nr. 10; vorher bis zum Ablauf). Namen sind serverweit eindeutig; eine Kollision wird mit `NAME_TAKEN` abgelehnt, kein stilles `-2`-Suffix mehr. |
| Versionshinweis | ja | Server meldet Mindest-Protokollversion; Client zeigt „Update nötig" mit Link, kein Auto-Update (packaging-licensing.md). |

### Ausdrücklich abgelehnt

| Feature | v1 | Grund |
| --- | --- | --- |
| Channels | nein | Ein Raum für eine feste Gruppe ist die Definition des Produkts (README). |
| Rechte/Rollen | nein, außer Admin | **Revidiert 2026-09-21 (Admin).** Einzige Rolle neben Mitglied ist Admin (siehe „Rollen-Admin-UI“); darüber hinaus gibt es ohne Channels und ohne Fremdnutzer nichts zu verwalten; Zugang = Zertifikat. |
| Text-Chat | nein | Die Gruppe hat andere Chat-Kanäle; ein Chat zieht Verlauf, Persistenz und Benachrichtigungen nach sich. |
| Server-Browser | nein | Es gibt genau einen Server, erreichbar über die Einladung. |
| Dateitransfer | nein | Kein Bezug zu Sprache; Sicherheits- und Speicherfläche ohne Nutzen. |
| Bots/Plugins | nein | Plugin-API bindet Protokoll und UI an Schnittstellen, die niemand braucht. |
| Soundboard | nein | Unterhaltungsfeature mit Mixing-/Lizenzfolgen; nicht „wirklich gebraucht". |
| Musik-Bot | nein | Braucht Bot-Accounts, Streaming, Lizenzfragen; Musik läuft lokal. |
| Aufzeichnung | nein | Datenschutz in einer privaten Gruppe, Speicherpfade, Format-Support; Mumbles Recorder ist ein Podcast-Feature. |
| Screen-Share | nein | Video mit hoher Auflösung/Bitrate ist ein eigenes Produkt (TeamSpeak bewirbt 4K@60); Kamera-Overlay ist bewusst klein. |
| Spatial Audio | nein | Positional Audio braucht Spielintegration (Mumble-Plugins); ICU liest keine Spielzustände. |
| Web-Client | nein | README-Nicht-Ziel; WebRTC/Browser-Auth mit Client-Zertifikaten widerspricht dem mTLS-Design. |
| Mobile | nein | README-Nicht-Ziel; Hintergrundaudio/Push auf Mobilplattformen ist eigener Aufwand. |
| Server-Mixing | nein | Server bleibt Relay ohne Decoding (Overview-Record, SFU); Mixing würde Server-CPU, Latenz und Schlüsselmaterial für Audio auf den Server bringen. |
| ~~Rollen-Admin-UI~~ | **ja** | **Revidiert 2026-09-21 (Admin), ergänzt 2026-09-25 (Moderation).** Admin ist eine Rolle im Protokoll (`UserFlags` bit4, `AdminSetRole` u. a., `PROTOCOL.md` §2.2); mindestens ein Admin bleibt (`LAST_ADMIN`). Admins verwalten im Client unter ⚙ → Nutzer: einladen, zum Admin machen und Admin-Rechte entziehen, vorübergehend sperren und entsperren, entfernen (Widerruf), erzwungen stumm- und taubschalten, rauswerfen; im Hauptfenster per Rechtsklick auf eine Person stumm-/taubschalten, rauswerfen, rauswerfen und sperren. Dasselbe per Server-CLI (`invite`, `promote`/`demote`, `disable`/`enable`, `revoke`, `kick`, `mute`/`unmute`, `deafen`/`undeafen`). |
| ~~Echo Cancellation~~ | **ja, siehe „Enthalten“** | **Revidiert 2026-09-20 (AEC).** Die Headset-Annahme ist vom Auftraggeber gekippt worden. Begründung, Kandidatenvergleich und Messungen in `aec.md`. |
| Attenuation anderer Apps beim Sprechen | nein | Systemweite Lautstärkeeingriffe (Ducking) sind plattformspezifisch und überraschend; Spiel-Lautstärke regelt das OS. |
| Idle-Auto-Mute/Deafen | nein | Mit PTT irrelevant, mit VAD wird Stille ohnehin nicht gesendet. |
| Text-to-Speech, Avatare, Themes, Fonts | nein | Kosmetik bzw. Barrierefreiheitsfeature ohne Nachfrage in der Gruppe; Themes folgen dem System (hell/dunkel). |
| Zertifikatsverwaltung im Client | nein | **Revidiert 2026-09-22.** Weiterhin keine Zertifikats*verwaltung* (kein Ausstellen, kein Widerrufen, kein Store) — aber der Nutzer sieht sein Zertifikat sehr wohl: das Verbindungsfenster zeigt es lesend an, eine Namensänderung erneuert es still, und der Passport (`PROTOCOL.md` §2.3.1) exportiert und importiert die ganze Identität, seit 2026-09-26 samt Einstellungen (ohne Geräteauswahl; beim Import abwählbar). Widerruf bleibt Sache von Server-CLI und Admin-Tab. |
| Positionierbare Overlay-Layouts (FPS, Uhr) | nein | Overlay zeigt Sprecher und Kamera, sonst nichts; FPS/Uhr liefern Spiele/OS. |

### Grauzone mit Empfehlung

| Feature | v1 | Grund / Empfehlung |
| --- | --- | --- |
| Whisper / Privat-Sprechen | nein | Ein Raum bedeutet: alle hören alle; Whisper führt Adressierung, Ziel-Auswahl-UI und Hotkey-Ziele ein (Mumble-Whisper-Doku nicht erreichbar, unbelegt). Wer privat sprechen will, startet einen zweiten Server-Prozess auf einem anderen Port. |
| Priority Speaker | nein | Setzt Hierarchie voraus, die ICU nicht hat; Lautstärke pro User (lokal) erfüllt den Bedarf „X lauter als alle". |
| Per-User-Mute (lokal) | ja | Als 0 % im Lautstärkeregler plus Mute-Symbol auf dem Regler; kein eigenes Protokoll-Feature, rein lokal. |
| Server-Passwort zusätzlich zum Zertifikat | nein | mTLS ist die Authentifizierung; ein Passwort wäre ein zweiter, schwächerer Faktor mit eigener Verwaltung. Kompromittiertes Gerät → Zertifikat per CLI widerrufen. Das Registrierungs-Bundle ist einmalig und zeitlich begrenzt (Standard 24 h, höchstens 7 Tage; `PROTOCOL.md` §2.3; bis 2026-09-24 ein Invite-Token). |
| Push-Notification bei Join | ja, lokal | Kein mobiler Push (kein Mobile-Client); lokaler Ton + Tray-Benachrichtigung bei Join/Leave, abschaltbar. |
| Loopback-Test | ja | Teil der Pegelanzeige im Settings-Dialog; billigste Antwort auf „hört man mich?". **Umgesetzt 2026-09-26** als Mikrofontest (siehe „Input-Pegel-Anzeige“). |
| Sende-Cue (Ton bei PTT-Start/Ende) | ja, Default aus | Trivial, von Mumble-Nutzern erwartet, Default aus, weil es den Sprecher stört. |
| Automatische Mikrofon-Verstärkung (AGC, TeamSpeak „automatic microphone volume adjustment") | ja, Default an (2026-09-25; vorher „nein“) | Auftraggeber-Wunsch „Pegel automatisch“. Die frühere Sorge (Pumpen, Rauschen in Pausen hochziehen) ist konstruktiv abgefangen: Hüllkurve runter in 0,2 s, rauf in 4 s, ±15 dB (2026-09-26; vorher 0,5 s/+15/−10), bewegt sich nur, solange der VAD Stimme hört **und** der Block ≥ 10 dB über dem Rauschboden liegt; in Pausen hält die Verstärkung. Nur mit Rauschunterdrückung wirksam, weil sonst der feste Energie-VAD (−40 dBFS) auf hochgezogenem Raumrauschen öffnen würde. Dazu „Eigene Lautstärke angleichen“ (Default an): Peak-Limiter vor dem Encoder; die Normalisierung dort läuft nur, wenn „Pegel automatisch“ aus ist — **ein Regler je Richtung** (2026-09-26: zwei hintereinander schnitten bei einer lauter werdenden Stimme zugleich und ließen nacheinander los, 6 dB Delle für Sekunden, `Client/IMPLEMENTATION.md` §16). |
| Stimmoptimierung (Hochpass, Kompressor, Limiter) | ja, Default aus (2026-09-24) | Selbst kein AGC: feste Kennlinie ohne Adaption, also kein Pumpen und kein Hochziehen von Rauschen in Pausen (VAD und Gate sehen das Signal vor der Kette). Der Nutzer entscheidet sich bewusst gegen „unverfälscht“, der Default bleibt glasklar. |
| Lautstärke angleichen (empfangsseitig, je Sprecher) | ja, Default an (2026-09-25) | Auftraggeber-Wunsch: Teilnehmer kommen sehr unterschiedlich laut an. Unabhängig von der Mikrofon-AGC (s. o.): es wird nur das **empfangene** Signal jedes Sprechers vor seinem Lautstärkeregler auf eine gemeinsame Hüllkurve (−22 dBFS RMS, ±12 dB) gebracht. Die Schätzung bewegt sich nur auf Blöcken ≥ 10 dB über dem eigenen Rauschboden des Sprechers (Minimum der letzten 1,5–3 s) und mindestens −50 dBFS; in Pausen hält die Verstärkung, konstantes Rauschen zählt nie als Sprache — also kein Pumpen und kein Hochziehen von Rauschen. Runter 0,6 s, rauf 2 s, ±2 dB um das Ziel bleiben unangetastet (2026-09-26: als Rückfallebene hinter dem Regler des Senders langsamer als der und mit Totband, damit die beiden sich nicht bekämpfen; vorher 80 ms). Abschaltbar unter Audio → Klang. |

## Verworfen, mit Grund

Siehe Tabellen „Ausdrücklich abgelehnt" und „Grauzone": Channels, Rechte/Rollen, Text-Chat, Server-Browser, Dateitransfer, Bots/Plugins, Soundboard, Musik-Bot, Aufzeichnung, Screen-Share, Spatial Audio, Web-Client, Mobile, Server-Mixing, App-Attenuation, Idle-Auto-Mute, TTS/Avatare/Themes, Zertifikats-UI, Overlay-Extras, Whisper, Priority Speaker, Server-Passwort, mobiler Push. Gemeinsamer Nenner: jedes davon führt entweder Hierarchie (Channels, Rollen, Priority, Whisper), ein zweites Produkt (Chat, Dateien, Screen-Share, Musik, Aufzeichnung) oder plattformspezifische Systemeingriffe (Ducking, Hooking) ein, ohne dass die feste Gruppe es braucht.

**Revidiert 2026-09-20 (AEC):** Echo Cancellation steht nicht mehr in dieser Liste. Sie ist jetzt enthalten — und zwar ausdrücklich **ohne** den plattformspezifischen Systemeingriff, der sie ursprünglich mit disqualifiziert hat: ICU nutzt gerade **nicht** die OS-AEC über `AudioCategory_Communications`, weil die als Nebenwirkung alle anderen Streams um standardmäßig 80 % dämpfen würde. Das Ducking bleibt abgelehnt, das AEC nicht. Siehe `aec.md`.

**Revidiert 2026-09-21 (Admin):** Die Rollen-Admin-UI steht nicht mehr in dieser Liste. Es gibt eine Rolle, Admin neben Mitglied, verwaltet unter ⚙ → Nutzer und per Server-CLI; weitere Rollen und eine Rechteverwaltung bleiben abgelehnt.

## Offene Punkte für Step 3

1. Speaking-Indicator-Latenz messen: PTT-Druck (evdev-Timestamp) → Indicator-Frame auf dem zweiten Client (Screen-Capture-Timestamp); Zielwert ≤ 100 ms bestätigen oder korrigieren.
2. PTT-Latenz Tastendruck → erstes Datagramm auf dem Draht (≤ 20 ms) unter Windows (Low-Level-Hook) und Linux (evdev) messen; Portal-GlobalShortcuts-Latenz auf KDE vergleichen.
3. Maustasten-Bindung: unter KDE Plasma Wayland mit „Listening for mouse buttons" über das Portal und alternativ über evdev; unter Windows Seitentasten über Raw Input bei fokussiertem Vollbildspiel (Borderless).
4. Jitter-Puffer-Zielwert (≤ 40 ms Playout-Delay) mit realem WAN-Jitter testen; Opus-FEC/PLC-Verhalten bei 5 % Paketverlust hörbar bewerten.
5. Overlay click-through und Always-on-top über einem Borderless-Windowed-Spiel: Windows (WS_EX_LAYERED/WS_EX_TRANSPARENT, unbelegt) und KDE layer-shell; GNOME als „nicht unterstützt" bestätigen.
6. Hot-Plug-Verhalten von cpal auf beiden Plattformen (Gerät ziehen während des Streams).
7. Kamera-Stream darf Voice nicht beeinflussen: Voice-Latenz und Paketverlust mit und ohne Kamera messen.
8. Whisper/Priority-Speaker-Beschreibung gegen erreichbare Mumble-Doku nachziehen, falls die Seiten wieder verfügbar sind (aktuell Redirect auf Doku-Index).
9. ~~Bedarf an Echo Cancellation in der Gruppe abfragen (Headset-Annahme verifizieren).~~ **Erledigt 2026-09-20 (AEC)**: vom Auftraggeber entschieden, AEC ist verbindlich. Die verbleibenden Prüfungen stehen als A1–A12 in `aec.md`; A1 (Windows-Build-Pfad) ist blockierend.

## Verified against

[x] Step 3 am 2026-09-20 (Details in `verification.md`):

- Keine eigenen Spikes; Abgleich mit den Entscheidungen der anderen Dokumente (Korrekturen unten eingearbeitet): Invite-Link-Format aus `auth-enrollment.md`, PTT-Backends aus `hotkey-wayland.md` (Portal primär, evdev opt-in; Windows Raw Input), Speaking-Indicator kombiniert Audio-Ankunft mit dem `speaking`-Flag aus `protocol/wire.md` (Flag deckt deafened Clients ab), Reconnect-Backoff max. 15 s wird in `PROTOCOL.md` übernommen.
- Offen: 1–9 (alle brauchen zwei Clients bzw. Windows).

Mit WebFetch gelesen (Inhalt erhalten), 2026-09-20:

- https://www.mumble.info/
- https://www.mumble.info/documentation/user/
- https://www.mumble.info/documentation/user/audio-settings/
- https://www.mumble.info/documentation/user/global-shortcuts/
- https://www.mumble.info/documentation/user/in-game-overlay/
- https://www.mumble.info/documentation/
- https://github.com/mumble-voip/mumble
- https://www.teamspeak.com/en/features/overview/
- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html
