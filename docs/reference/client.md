# ICU Client — Referenz

Stand 2026-09-28. Normativ: [Wire-Protokoll](protocol.md).
Funktionsumfang: [Features](features.md). Pfade ohne `../` beziehen sich auf
das Repository; Abschnittsnummern bleiben für Quellcodeverweise stabil.

## 1. Zweck und Grenzen

Ein Fenster, ein Tray-Icon, ein optionales Overlay. Der Client verbindet sich mit genau einem Server, sendet das eigene Mikrofon per PTT oder VAD, mischt die Stimmen der anderen lokal, zeigt wer spricht, und blendet auf Wunsch Kamerakacheln über ein Borderless-Window-Spiel. Keine Channels, kein Chat, keine Zertifikatsverwaltung im UI.

## 2. Artefakte

| Plattform | Binary | Build | Laufzeit |
| --- | --- | --- | --- |
| Windows 10/11 x86_64 | `icu.exe`, portable, unsigniert (ZIP + SHA256SUMS) | Cross von Fedora, Target `x86_64-pc-windows-gnu`, Rezept `docs/research/cross-compile.md`, statisches libstdc++; Feature `aec` über den Meson-Wrapper `packaging/windows/toolchain` (`docs/research/aec.md` A1, 2026-09-26); Paket `packaging/windows/build.sh` | nur System-DLLs (Allowlist in `packaging/windows/build.sh`), darunter `avrt.dll`, `crypt32.dll`, `winmm.dll` und `mf.dll`, `mfplat.dll`, `mfreadwrite.dll` (MediaFoundation, von nokhwa). Diese werden über `windows` 0.62 per `raw-dylib` **statisch importiert** und beim Prozessstart aufgelöst; ein Delay-Load-Pfad existiert weder in nokhwa noch im mingw-gnu-Build. Folge: **Windows-N/KN ohne Media Feature Pack wird in v1 nicht unterstützt** (das Binary startet dort nicht). |
| Linux x86_64 | `icu`, glibc-dynamisch, `.rpm` (Fedora 42/43) und `.deb` (Debian 13) über `packaging/build.sh` | nativ auf alter Baseline; Build-Host braucht PipeWire-Header und clang (pipewire-sys nutzt pkg-config + bindgen) sowie meson, ninja-build und abseil-Header für `webrtc-audio-processing` (§12) | Wayland/GL per dlopen. Hart gelinkte C-Bibliotheken: **`libpipewire-0.3`** (pipewire-rs linkt sie als `DT_NEEDED`, ein dlopen-Pfad ist in pipewire-rs nicht vorgesehen) und ALSA-lib nur, wenn das cpal-Fallback-Feature gebaut wird. |

Kein Auto-Update: eine Version unter `min_client` weist der Server schon beim `Hello` mit `VERSION_UNSUPPORTED` ab (fatal, „Bitte ICU aktualisieren“); das Banner bei `Welcome.min_client` > eigene Version bleibt als Anzeige.

## 3. Architektur

`crates/icu-client/src/engine.rs` verbindet Netzwerk, Audio und Oberfläche.
`net/` verwaltet QUIC und Registrierung, `audio/` die Betriebssystem-Backends,
`dsp/` die Signalkette. `camera.rs`, `video/` und `overlay/` bilden den Videopfad;
`ui/`, `hotkey/`, `tray.rs` und `instance.rs` die Desktop-Integration.
Die öffentliche Bibliothek ermöglicht Tests ohne ein laufendes Fenster.

## 4. Threads und Datenfluss

| Ausführung | Aufgabe |
| --- | --- |
| Backend-Callbacks | Mikrofon und AEC-Referenz in getrennte Ringpuffer schreiben; Wiedergabe aus dem Playback-Ring lesen |
| `icu-dsp` | 480 Samples je 10 ms: AEC → manuelle Verstärkung → Pegelautomatik → RNNoise/VAD → Stimmoptimierung → Ausgangsnormalisierung, falls keine Pegelautomatik → Limiter → Gate → Opus |
| Tokio, zwei Worker | QUIC, Control, Datagramme und Video-Streams; Befehle und Zustandsänderungen |
| `icu-mixer` | Jitter-Puffer, Decode/FEC/PLC, Empfangsnormalisierung, Lautstärken, Mix und Limiter |
| Kamera/Video-Decode | Capture und Encoding sowie Empfangsdecode auf eigenen Threads |
| UI/Overlay/Hotkeys | eframe-Hauptthread, eigener Overlay-Thread, plattformspezifische Ereignisverarbeitung |

`watch<AppState>` verteilt Zustand, `mpsc` trägt Befehle; Audio nutzt `ringbuf`.
Callbacks warten nicht auf Sperren; der Playback-Zugriff verwendet `try_lock`.
Audio-Threads fordern unter Linux PipeWire/RTKit-Priorität und unter Windows
MMCSS „Pro Audio“ an, mit protokolliertem Best-Effort-Rückfall.
Decoder werden vor dem Sprecher-Lock aufgebaut und nach dessen Freigabe zerstört.
Blockierende Stream-Aufrufe laufen über `block_in_place`.

Gerätewechsel und Fehler bereits laufender Streams öffnen die Stream-Gruppe mit
AEC-Neustart neu. Ein Stream, der sich gar nicht öffnen lässt, wird einzeln
wiederholt; ein fehlendes Mikrofon unterbricht dadurch die Wiedergabe nicht.
Die Capture-Rückstaugrenze berücksichtigt mindestens ein Graph-Quantum plus zwei
Blöcke. Verworfenes Audio zählt im Zeitstempel mit; angebrochene Frames und
Pre-Roll werden bei einem Sprung verworfen.

AEC verarbeitet zuerst die Referenz, dann das Mikrofon. `dsp/refalign.rs` paart
beide über ihre Callback-Zeitpunkte; Windows berücksichtigt zusätzlich
`reference_slack` für den unabhängig gelieferten Loopback. Nach einem Reopen
werden beide Ringe geleert. Ohne Referenz läuft keine AEC; die Oberfläche zeigt
das an. Einzelheiten und Messgrenzen: [AEC](../research/aec.md).

## 5. Verbindung und Identität

1. **Erststart / Registrierung** (revidiert 2026-09-24, `protocol.md` §2.3): es gibt nur noch ein Eingabeformat, den Passport-Block `-----BEGIN ICU PASSPORT-----` — aus dem Argument (Datei oder `-` für die Standardeingabe; den Block selbst als Argument lehnt ICU seit 2026-09-27 ab: der Schlüssel stünde für jeden lokalen Nutzer in `/proc/<pid>/cmdline` und in der Shell-History) oder dem Eingabefeld im Erststart-Fenster. Das Argument öffnet im Fenster-Modus das Erststart-Fenster vorbelegt (seit 2026-09-26, auch wenn schon eine Identität existiert: ersetzt wird erst auf „Diesem Server beitreten“; geschlossen ohne Beitritt startet ICU nicht); mit `--name`/`--yes`/`--headless` läuft der Terminal-Weg. `Identity::from_passport` liest nur Version 4 (1–3 lehnt es mit „älter, neu exportieren“ ab, `protocol.md` §2.3.1; bis 2026-09-27 stand hier „Version 1 und 2“) und prüft vor jedem Ersetzen (`Identity::check_importable`, 2026-09-27): der Schlüssel parst und gehört zum Leaf (`conninfo::leaf_matches_key`), das Leaf ist von der mitgelieferten CA signiert (`conninfo::leaf_signed_by`), eine Einladung ist nicht abgelaufen, und Name, Servername und Host tragen keine Steuer-, Format- oder Bidi-Zeichen (`icu_proto::is_plain_text`) — sonst zeigen Einstellungs- und Erststart-Fenster (unter dem Feld statt „keine gültige Einladung“) und CLI den Grund, und nichts wird ersetzt. Wo der Server ist, steht in den Zertifikaten (`ServerInfo::from_certs`: Host/Port aus dem URI-SAN `icu://…` des Leafs, SNI aus dem CA-CN `ICU CA <server_name>`, sonst der Host).
   - **Registrierungs-Bundle** (Leaf mit `OU=ICU registration`, `Identity::is_registration`): das Fenster zeigt ein Namensfeld, vorbelegt mit dem Platzhalternamen des Bundles; die CLI verlangt `--name`. `net::register::register` verbindet sich mit dem Registrierungszertifikat ganz normal über `icu/1` (`conn::client_config`, die CA aus dem Bundle ist die einzige Wurzel — kein Pinning, kein TOFU), erwartet `Welcome.registration` (fehlt es, endet `register` mit „Der Server hat die Einladung nicht als Anmeldung angenommen“, ohne Quittung und ohne etwas zu speichern; bis 2026-09-27 kam das Bundle zurück, als wäre es registriert, und die CLI meldete „identity stored“), erzeugt einen **eigenen** P-256-Schlüssel und schickt `RenewRequest` mit CN = gewähltem Namen (`register::complete`, `register::csr_for` — die einzige Stelle, an der ein Name in eine CSR geht, von Registrierung **und** Renewal benutzt). Das neue Leaf wird nur übernommen, wenn CN und Schlüssel passen und es von der CA des Bundles signiert ist (`conninfo::check_issued_leaf`, CA-Signatur seit 2026-09-27). `NAME_TAKEN` landet am Namensfeld, das Bundle bleibt gültig. Auf die Antwort wartet der Austausch höchstens 30 s (`register::EXCHANGE_TIMEOUT`, 2026-09-27; der Server hält eine Registrierungssitzung 120 s offen) und endet dann wie ein Verbindungsabbruch — vorher blieb das Erststart-Fenster bei einem Server, der anderes oder nichts schickte, für immer beschäftigt. `register` **speichert die Identität selbst, bevor es die Sitzung schließt**, und schließt nur dann mit `REGISTRATION_STORED` (`register::receipt`; nicht `NO_ERROR`, denn mit Code 0 schließt quinn auch eine bloß fallengelassene Verbindung) — das ist die Quittung, mit der der Server das Bundle widerruft. Scheitert das Speichern, schließt es mit `SHUTDOWN`; das Bundle bleibt gültig und ein zweiter Versuch geht. Dasselbe gilt für die Engine, wenn ein Bundle in den laufenden Client importiert wurde (sie schreibt das Bundle nicht, Punkt 6); bricht dort nur die Verbindung ab, versucht sie es mit Backoff erneut, statt fatal zu enden; schließt der Server mit einem fatalen Code (`SESSION_REPLACED`, `AUTH_*`), endet sie fatal.
   - **Passport** (sonst): kein Server-Kontakt, lokal gespeichert, ohne Namenszeile. Trägt er Einstellungen (seit 2026-09-26), steht an ihrer Stelle der Schalter „Einstellungen übernehmen“ (an), darunter seit 2026-09-27, was sie daran ändern, wann und wie das Gerät zu hören ist (`Settings::send_changes`: Senden, Schwelle, Verstärkung, Rauschunterdrückung, VAD-Nachlauf, VAD-Anlauf, Hinweistöne, „Minimiert starten“, je mit dem bisherigen Wert; ebenso im Einstellungsfenster und auf der CLI); nach der Identität schreibt `config::import_settings` sie in `settings.toml`, um die Geräte dieses Rechners herum (`Settings::take_portable`, §11). Die CLI tut dasselbe, außer mit `--skip-settings`; lassen sie sich nicht schreiben, bleibt es bei der Identität und einer Zeile im Log bzw. auf stderr.

   Gespeichert wird: `identity/key.pk8` (0600; unter Windows seit 2026-09-26 mit DPAPI versiegelt: Magic `ICU-DPAPI/1\n` + `CryptProtectData`-Blob, User-Scope, ohne UI; eine unversiegelte PKCS#8-Datei wird noch gelesen und beim nächsten Speichern versiegelt), `identity/leaf.der`, `identity/ca.der`, `identity/com.key` (seit 2026-09-27: der Com-Schlüssel des Servers aus dem Passport, `protocol.md` §1; 32 Byte roh, 0600, unter Windows versiegelt wie `key.pk8`; eine Identität ohne ihn lädt nicht und braucht einen neuen Passport), `identity/server.toml` (host, port, server_name, name) — als Satz: `Identity::save` schreibt nach `identity.new/` und tauscht das Verzeichnis per Rename ein, `Identity::load` liest nach einem Absturz zwischen den beiden Renames `identity.old/` — nur lesend (2026-09-26; vorher benannte es um und ließ ein gleichzeitiges `save` der Engine mit `ENOTEMPTY` scheitern), das nächste `save` bringt die Verzeichnisse in Ordnung, und ein prozessweiter Lock serialisiert Laden, Speichern und Löschen, damit der Passport-Export des Fensters nie einen Schlüssel mit dem Zertifikat des anderen Satzes paart; Schlüssel und Leaf verschiedener Stände mischen sich nie — beim Import und nach jedem Renewal aus den Zertifikaten neu abgeleitet, sonst nur noch für Zertifikate von vor 2026-09-24 maßgeblich. `settings.toml` hat keinen `[server]`-Abschnitt.
2. **Normalstart**: Identität laden → Verbindung ALPN `icu/1` mit Client-Cert → `Hello{minor:MINOR, caps:icu_proto::CAPS, client:"icu/0.1.0 linux", initial:flags, name}` → `Welcome` → Zustand `Active`. Liegt `Welcome.server_minor` unter `MIN_PEER_MINOR`, schließt `conn::connect` mit `VERSION_UNSUPPORTED` und meldet `ConnectError::ServerTooOld`: fatal, Text `problem::SERVER_TOO_OLD` („Server aktualisieren“), kein Reconnect (`protocol.md` §8, 2026-09-26). Vorher kein Audio, keine Anzeige „verbunden“ (Spike 2: `connect()` ist kein Erfolgskriterium). Aus `Welcome.you: UserInfo` übernimmt der Client die eigene `user_id` und den **serverseitig gespeicherten** Namen; `Hello.name` ist obsolet und wird ignoriert. Vor `Welcome` zeigt der Client den CN seines eigenen Leafs, nicht einen gespeicherten Einstellungswert — so kann die Anzeige dem Zertifikat nie widersprechen. `Welcome.min_client` wird in SemVer-Ordnung gegen `CARGO_PKG_VERSION` verglichen. `Welcome.is_admin` landet in `AppState.is_admin` und schaltet die Einladungs-Erzeugung im Settings-Dialog frei (§10).
3. **Reconnect**: Ist ein Registrierungszertifikat abgelaufen, verbindet `conn::client_config` gar nicht erst und meldet es fatal. Ein abgelaufenes **eigenes** Zertifikat verbindet normal: nach `Welcome` erneuert `run_session` es in der renew-only-Sitzung (`register::renew_expired`, bestehender Schlüssel, gespeicherter Name), speichert es und verbindet sich neu (`protocol.md` §2.3.3). Meldet der Server `RENEW_REQUIRED`, obwohl die eigene Uhr das Zertifikat noch für gültig hält, setzt der Client `renew_only` und verbindet sich direkt in die Erneuerung. Ein TLS-Alert des Servers (QUIC-Crypto-Fehler 0x100–0x1ff) gilt ebenfalls als fatal: dasselbe Zertifikat würde erneut abgelehnt – aber nur, wenn der Server vorher seine Signatur (`CertificateVerify`) vorgelegt hat (`CertFailure::server_proven`, 2026-09-26). Ein Close davor kann jeder auf dem Weg in ein Initial-Paket schreiben; er wird wiederholt (`ConnectError::unauthenticated`), sonst beendete ein einziges gefälschtes `certificate_revoked` die Reconnects bis zum Klick auf „Neu verbinden“. Die Alerts 70 (`protocol_version`) und 120 (`no_application_protocol`, ALPN) sind dabei kein Zertifikatsproblem, sondern eine andere Protokollversion: sie lesen sich wie `VERSION_UNSUPPORTED` („Bitte ICU aktualisieren“, 2026-09-26); da sie vor der Server-Authentifizierung kommen, ohne Abbruch der Reconnects (`ConnectError::VersionHint`). Ebenso wenig fatal ist, was die eigene Prüfung am Server-Zertifikat findet (2026-09-27): rustls prüft es vor der Signatur des Servers, `connect` fragt deshalb zuerst `server_proven` und nimmt erst danach die aufgezeichnete Zertifikatsfehler-Ursache. Fremde CA oder anderer Name → `ConnectError::Unverified` mit `problem::SERVER_UNVERIFIED` bzw. `SERVER_NAME_UNVERIFIED` („… möglicher Angriff oder neu aufgesetzt. Beim Admin auf anderem Weg nachfragen, keine Dateien von Dritten einspielen“) und `conn::slow_backoff` (15, 30, 60, 120, 240 s, dann 300 s; `SessionEnd::RetrySlowly`); noch nicht gültig (`NotValidYet`) → `ConnectError::NotYetValid`, „Systemuhr prüfen“, normaler Backoff wie abgelaufen/widerrufen. Ebenso fatal ist ein Control-Frame nach `Welcome`, der nicht dekodiert oder die Längenregel (1–16384) verletzt (`protocol.md` §2, §8): der Client schließt mit `PROTOCOL_VIOLATION` und zeigt „Update nötig“ (`problem::UPDATE_NEEDED`), statt wie bis 2026-09-25 die Sitzung als Abbruch zu werten und im Sekundentakt in denselben Fehler neu zu verbinden. Sonst Backoff 1, 2, 4, 8, 15 s, unbegrenzt; UI-Zustand „verbinde…“; eigener Zustand (Mute/Deafen/Lautstärken) bleibt. Jedes `Welcome` ersetzt dagegen den **vollständigen Peer-Zustand**: Jitter-Buffer, Decoder, Video-Kacheln und alle Session-Zuordnungen werden verworfen, weil Session-IDs nur innerhalb einer Verbindung gültig sind. Empfangenes Video trägt dafür die Generation seiner Verbindung: was der Decode-Thread einer beendeten Verbindung noch ausliefert, wird verworfen, statt unter einer inzwischen fremden Session-ID ein Bild zu zeigen (2026-09-26). Die Namensauflösung des Servers ist auf 5 s begrenzt und läuft neben den Befehlen: Mute, Deafen und „Identität ersetzen“ warten nicht mehr auf den Resolver. Persistente Daten je Peer hängen an `user_id`. Nach `Welcome` Audio ohne Neustart der Backends.
4. **Renewal**: direkt nach `Welcome` und danach stündlich, höchstens ein Versuch pro 24 h (so viel lässt der Server zu), prüft `renewal_due` zwei Dinge — weicht der CN des eigenen Leafs vom gespeicherten Namen ab (auf einem anderen Gerät umbenannt), wird **nachgezogen**; sonst wird erneuert, sobald weniger als ein Drittel der Laufzeit, mindestens aber 30 volle Tage, bis zum Ablauf bleiben (vorher lehnt der Server ab; `CertInfo.valid_days`). Stündlich, weil eine Sitzung Wochen halten kann und ein dauerhaft laufender Client sonst in den Ablauf liefe. Beides still: kein Banner, keine Fehlermeldung am Namensfeld, Ablehnungen nur ins Log. Die CSR wird mit dem **bestehenden** Schlüssel gebaut (`rcgen::KeyPair::try_from` auf `key.pk8`) — eine Umbenennung darf den Schlüssel nicht verwaisen lassen, sonst passt ein vorher exportierter Passport nicht mehr. Das neue Leaf wird **erst geprüft, dann geschrieben**: trägt es einen anderen CN als angefragt, passt sein Public Key nicht zum eigenen Schlüssel oder stammt es nicht von der eigenen CA (seit 2026-09-27), bleibt das alte in Kraft. Ein unbrauchbares, persistiertes Leaf wäre sonst ein Totalausfall, aus dem nur ein Passport von einem anderen Gerät herausführt. Ein Reconnect ist nicht nötig: die laufende Sitzung ist mit dem alten Leaf authentifiziert und bleibt gültig. Trägt das neue Leaf eine andere Serveradresse (Server umgezogen), übernimmt der Client sie für den nächsten Verbindungsaufbau.
6. **Identität ersetzen** (Passport-Import in ⚙ → Allgemein, `Command::ReplaceIdentity`): wirkt in jedem Zustand sofort — verbunden, beim Verbindungsaufbau, im Backoff und auch nach einem fatalen Close (z. B. `AUTH_REVOKED`), wo es der einzige Ausweg ist; der Zähler des Backoffs beginnt von vorn. Wie ein erneuertes Zertifikat wird die Identität erst gespeichert und dann übernommen (2026-09-26): scheitert das Schreiben, bleibt die bisherige in Kraft, die Verbindung bleibt stehen, ein Banner sagt es. Ist das Eingespielte ein Registrierungs-Bundle, erledigt die Engine die Registrierung nach dem `Welcome` selbst, unter dem Platzhalternamen, und verbindet dann mit dem eigenen Zertifikat neu; umbenennen geht danach wie immer. Die bisherige Identität bleibt, bis die neue sich bewährt hat (2026-09-27, `engine::ImportTrial`): ein Passport wird mit `Identity::save_keeping_previous` geschrieben — der ersetzte Satz wartet in `identity.old/` — und gilt mit seinem ersten `Welcome` (`Identity::drop_previous`); ein Registrierungs-Bundle wird gar nicht geschrieben (wie im Erststart-Fenster), `identity/` behält die bisherige, bis die Registrierung die neue speichert, und Einstellungen nimmt es nie mit. Endet der Import vorher fatal (verbrauchte Einladung, widerrufener Passport, fatale Close-Codes), stellt die Engine die bisherige Identität samt Einstellungen wieder her (`Identity::restore_previous`), zeigt „Der eingespielte Passport ließ sich nicht verwenden: … Die bisherigen Zugangsdaten gelten weiter.“ als Banner und verbindet sofort neu. Ein zweiter Import stellt zuerst die bisherige wieder her; lässt sie sich nicht zurückschreiben, wird er abgelehnt. Nicht fatale Fehler (Server nicht erreichbar, unbestätigtes Server-Zertifikat §5.3) lassen den Versuch laufen. Beendet sich ICU vor dem ersten `Welcome`, startet es mit dem Passport; `identity.old/` bleibt dann bis zum nächsten Speichern oder Abmelden liegen. Trägt der Passport Einstellungen und ist „Einstellungen übernehmen“ an, reisen sie im selben Kommando (`Command::ReplaceIdentity(identity, settings)`, 2026-09-26) und gelten erst, wenn die Identität gespeichert ist — über denselben Weg wie `UpdateSettings` (`replace_settings`: DSP, Streams, `AppState`, entprellt `settings.toml`, Hotkeys), um die Geräte dieses Rechners herum; scheitert das Speichern, bleiben auch die Einstellungen. Das Hauptfenster übernimmt ein so geändertes Farbschema sofort.
7. **Abmelden** (⚙ → Allgemein, Karte „Abmelden“, zweistufig „Wirklich?“ wie beim Entfernen eines Nutzers, 2026-09-24): schließt das Hauptfenster, fährt die Engine herunter und löscht erst dann `identity/` samt `identity.new/`/`identity.old/` (`Identity::remove`) — vorher könnte ein Renewal sie neu schreiben. `Engine::shutdown` wartet dafür (höchstens 3 s) auf das Ende des Netz-Tasks, und ab dem Herunterfahren schreibt die Engine nichts mehr nach `identity/` (2026-09-26; vorher kehrte es nach 100 ms zurück, und ein gerade eintreffendes Renewal konnte `identity/` nach dem Löschen neu anlegen). `main.rs` kehrt danach zum Erststart-Fenster zurück (das neue Hauptfenster startet dann sichtbar, auch bei „Minimiert starten“); wer es schließt, beendet ICU. Kein Widerruf beim Server: derselbe Passport kann auf anderen Geräten weiterleben. Ein exportiertes `icu-passport.icu` und `crl.der` bleiben liegen; gespeicherte Einladungen (`invites/`, jede ein gültiger Registrierungsschlüssel) werden mit gelöscht (2026-09-26).
5. **Dateizuordnung** (revidiert 2026-09-26; vorher URI-Handler `icu://`, tot seit es keine Links mehr gibt): Einladungen und Passports sind `application/x-icu` — Glob `*.icu` (so speichert `config::save_invite`) und Magic `-----BEGIN ICU PASSPORT-----` am Dateianfang (`packaging/files/icu-mime.xml`, Unterklasse von `text/plain`). Die Pakete installieren `/usr/share/mime/packages/icu.xml` und `icu.desktop` mit `MimeType=application/x-icu;` und `Exec=icu %f`; die Datenbanken bauen die Trigger von shared-mime-info/desktop-file-utils neu (Fedora File-Trigger, Debian `interest-noawait`), keine Scriptlets. Ein Binary außerhalb des Pakets registriert dasselbe je Nutzer unter `~/.local/share` (`uri.rs`, nur bei Änderung, dann `update-mime-database`, `update-desktop-database`, `xdg-mime default`), abschaltbar mit `[ui] register_uri_handler` bzw. `--no-uri-handler`. Ein `icu://…`-Argument wird erklärt und ignoriert, ICU startet normal. Eine `.txt`-Datei bleibt `text/plain` (der Glob gewinnt). Windows (2026-09-28, `uri.rs` + `winreg.rs`): Das Setup registriert `.icu` → `ICU.Invite` unter `HKLM\Software\Classes` für alle Benutzer; der installierte Client überschreibt das nicht unter HKCU. Eine portable EXE registriert unter `HKCU\Software\Classes` (`Content Type` `application/x-icu`, `DefaultIcon`, `shell\open\command` `"<exe>" "%1"`); geschrieben werden nur geänderte Werte, danach `SHChangeNotify(SHCNE_ASSOCCHANGED)`; gleicher Schalter. Setup-Registry und ShellExecute einer .icu-Testdatei wurden auf Windows 10 geprüft; der installierte Client erzeugte dabei keinen HKCU-Eintrag.
6. **Laufende Verbindung**: `Ping` alle 5 s (RTT im Debug-Fenster, `Pong.server_time_ms` = Unix-Millisekunden). Jede lokale Änderung von mute/deafen/camera_on/viewing/ptt geht als `SetState` hinaus, begrenzt durch einen Token-Bucket (15/s, Burst 15, drei Viertel des Server-Limits, §16); was das Budget nicht sofort erlaubt, wird nicht verworfen, sondern zusammengefasst: die Sitzung vergleicht nach jedem Ereignis die eigenen Flags mit dem, was der Server von ihr hat, und sendet den neuesten Stand, sobald ein Token frei ist (2026-09-26; vorher verwarf ein festes 20-pro-Sekunde-Fenster alles darüber, ein verlorenes letztes Entstummen ließ den Server das Audio weiter verwerfen). bit2 (`speaking`) sendet der Client immer als 0, weil der Server es autoritativ ableitet. Namensänderung im Settings-Dialog sendet `RenewRequest` mit einer CSR auf den neuen CN (`SetName` ist obsolet); bis die Antwort da ist, sind Feld und Button gesperrt (`AppState.rename_pending`). Endet die Sitzung vorher, gibt ihr Ende die Sperre frei; eine automatische Erneuerung ohne Antwort wird bei der nächsten Verbindung sofort wiederholt statt erst nach 24 h, und eine offene Einladungsanfrage verfällt mit der Sitzung, statt den nächsten fremden `Error` an die Einladungskarte zu hängen (2026-09-26). `Error(NAME_TAKEN)` bzw. `RENEW_REJECTED` zeigt der Client am Eingabefeld und behält den alten Namen — aber nur, wenn der Nutzer die Änderung angestoßen hat; eine abgelehnte Hintergrunderneuerung landet ausschließlich im Log. `UserRenamed` aktualisiert die Liste.

## 6. Audio-Pipeline

### Backends und Formate

Intern: mono, 48 kHz, f32. PipeWire verwendet Capture, Playback und einen
Systemmix-Referenzstream in derselben `node.group`. Die angeforderten 240 Samples
sind kein festes Quantum; Ringpuffer gleichen die tatsächlichen Blockgrößen aus.
Ohne feste Gerätewahl folgt die Referenz dem Default-Sink, andernfalls dem
gewählten Ausgabegerät. Ist PipeWire nicht erreichbar, meldet das stille
Fallback-Backend den Ausfall im Hauptfenster.

Windows nutzt WASAPI Shared Mode, bevorzugt `IAudioClient3` bei float32/48 kHz,
sonst `IAudioClient` mit Systemkonvertierung. Mikrofon: Kanal 0; Wiedergabe: FL/FR,
sonst FC beziehungsweise das erste Kanalpaar. Surround und LFE bleiben still.
Eine Wiedergabeanforderung erreicht den Mixer mit ihrer gesamten Framezahl;
auch große Endpoint-Puffer werden nicht als viele kleine Anforderungen verborgen.
Die Referenz ist der System-Loopback des Ausgabe-Endpoints, alle 2 ms abgefragt.
Default-Wechsel folgen `eConsole`. CPAL ist der Rückfall, wenn der WASAPI-Watcher
nicht startet; eigene Resampling-Planungen gehören nicht zum aktuellen Pfad.
CPAL gibt Stimme ebenfalls nur auf dem ersten Kanalpaar aus und meldet
Streamfehler an die automatische Wiederöffnung. Der Capture-Fallback setzt
weiterhin einen vom Backend unterstützten float32/48-kHz-Eingang voraus.

### Verarbeitung und Senden

- AEC3 verarbeitet 480er-Blöcke in ±1.0, mit eigener Delayschätzung und ohne
  zusätzliches AGC. AEC und Denoise laufen auch bei geschlossenem Sendegate weiter.
- RNNoise arbeitet mit auf ±32768 skalierten Samples; seine Wahrscheinlichkeit
  speist die VAD. Der erste Ausgabeblock nach Filterstart wird durch Stille
  ersetzt. Nach einer Denoise-Pause startet der Filter neu, damit keine alten
  Überlappungssamples wiedergegeben werden. Nicht endliche Eingangswerte werden bereinigt.
- Die automatische VAD-Schwelle verfolgt den Rauschboden. Sprache bei Stumm,
  fehlender Verbindung oder losgelassener PTT-Taste zählt nicht als Rauschen.
  Das gilt auch für Sprachkandidaten während des VAD-Anlaufs: die Schwelle
  darf leise Sprache nicht weglernen, bevor das Gate öffnen kann.
  Nach einem Streamwechsel oder Denoise-Umschalten beginnt die Erkennung
  frisch; Rauschboden und VAD-Anlauf des alten Signals werden verworfen.
  Die Sprachanzeige hält über den VAD-Nachlauf, auch während Stumm.
- Stimmoptimierung ist standardmäßig an, Stärke mittel. Hochpass 80 Hz,
  Kompressor und Limiter folgen AEC und VAD; leicht/mittel/stark verwenden
  −20/−24/−30 dBFS, 2:1/3:1/4:1 und +3/+6/+9 dB Makeup.
- PTT-Nachlauf: standardmäßig 150 ms; VAD-Nachlauf: 1000 ms. VAD sendet einen
  Pre-Roll-Frame, der während Stumm aufgenommene Frames ausschließt. Stummschalten
  verwirft angebrochenes Audio; das abschließende END-Paket enthält Stille.
  Die Stummschaltung gilt für jeden 10-ms-Block. Beim Entstummen werden
  noch gepufferte Capture-Samples, Teilframes und Filter-Audio verworfen;
  die erlernten Verstärkungen bleiben erhalten.
- VAD-Anlauf (Tipp-Filter, `attack_ms`, standardmäßig 40 ms, 0–200, 0 = aus):
  ein geschlossenes VAD-Gate öffnet erst, wenn die Stimme den Anlauf über
  anhält (`Gate`-Zustand `Arming`). Ein Tastenanschlag ist ein oder zwei
  Blöcke Breitbandrauschen, die RNNoise über die Schwelle heben kann; ohne
  Anlauf hielt ein solcher Block das Gate den ganzen VAD-Nachlauf offen. Ein
  Frame ohne Stimme verwirft den Anlauf; ein offenes oder nachlaufendes Gate
  öffnet wie zuvor sofort wieder, und PTT kennt keinen Anlauf. Die
  zurückgehaltenen Frames gehen als Pre-Roll vor dem öffnenden Frame hinaus
  (`dsp::preroll::PreRoll`, Tiefe aus Anlauf und Frame-Dauer; `Gate::withheld`
  zählt sie), der Wortanfang bleibt also erhalten. Währenddessen zählt ein
  Block über der Schwelle bei geschlossenem Gate als Sprachkandidat und
  nicht als Rauschen. Bei großen Hardware-Quanten zählt auch die Dauer der
  bereits verarbeiteten Sprachframes zum Anlauf, damit eine schnelle
  Verarbeitung im Stapel keinen Wortanfang verdrängt.
  Innerhalb von 150 ms nach einem Tastendruck gilt der doppelte
  Anlauf (`hotkey::typing`, nur ein Zeitstempel, nie welche Taste): das sieht
  der Windows-Raw-Input-Leser und der Linux-evdev-Leser (§7). Über das
  Portal sieht ICU nur die belegten Kürzel, dort wirkt allein der Grundanlauf.
  Stimme sperrt nie: sie öffnet das Gate auch beim Tippen, nur später.
- Opus: 48 kbit/s, constrained VBR, VOIP, Complexity 10, DTX aus, In-Band-FEC
  mit mindestens 3 % Verlustannahme, 20-ms-Frames (wahlweise 10 ms). Ein Wechsel
  der Frame-Dauer verwirft den angebrochenen Frame.
- Ohne andere Teilnehmer wird weder Audio codiert noch gesendet; taube Peers
  zählen als Publikum. Gate und Rauschbodenerkennung laufen weiter.
- Capture-Überläufe verwerfen den vollständigen alten Ringinhalt samt
  Teilframe. Die Sample-Uhr zählt auch nicht eingereihte Samples; ein
  CPU-Stall spielt dadurch keine veraltete Aufnahme nach.

### Empfang und Wiedergabe

Je Sprecher existieren Decoder und adaptiver Jitter-Puffer. Die Zieltiefe ist
das Maximum aus Jitter- und Stall-Term, geklemmt auf einen Frame bis 200 ms.
Bereits eingereihte kurze Sprechabschnitte behalten nach einem END ihre
Ankunftszeit für den Vorlauf; auch ohne weitere Pakete werden sie abgespielt.
FEC braucht das folgende Paket; sonst verdeckt PLC den Verlust. Deep PLC nutzt
Decoder-Complexity 5. Ein Sprung außerhalb des Pufferfensters beginnt einen
neuen Sprechabschnitt, statt minutenlang fehlende Frames zu verdecken.

Die Tiefe kann in Sprechpausen sinken; während eines Spurts wächst sie mit
begrenzten Stretch-Schritten. Überschüsse werden bevorzugt an leisen Frames
gekürzt. Der Wiedergabe-Vorlauf folgt den jüngsten Callback-Größen und fällt
nach 2–4 s wieder, wenn ein großes Quantum vorbei ist. Unbekannte Sprecher
bleiben stumm, bis die Teilnehmerliste ihre Lautstärke zuordnet.

Empfangsnormalisierung lernt aus regulär decodierter Sprache, nicht aus PLC/FEC.
Danach folgen lokale Lautstärke, Summierung und Limiter. `LossReport` wird bei
relevanter Änderung und bei anhaltendem Verlust spätestens alle vier Sekunden
aufgefrischt; ein Intervall ohne Audio schickt nichts. Hinweistöne sind in der
Queue auf 0,5 s begrenzt. Der Mikrofontest bleibt auf Audio und Klang aktiv.

### Empfangsfeedback und Systemmikrofon

`audio.peer_assistance` und `audio.tune_system_mic` sind standardmäßig aktiv,
wirken aber nur mit `auto_level && denoise`. Die UI bietet beide Schalter
separat und zeigt Systemkorrektur, digitale Korrektur und Peer-Zielkorrektur.
Die Setup-Zusammenfassung nennt die aktiven Funktionen. Manuelle ICU-Verstärkung
wird nicht überschrieben.

`dsp::feedback::Measurement` nutzt den Schätzer des Empfangsnormalisierers ohne
Samples zu ändern. Empfänger messen direkt nach regulärem Opus-Decode, vor
Normalisierung/Lautstärke; PLC/FEC und künstliche Jitter-Ausgaben zählen nicht.
Der Sender misst denselben Wert vor Opus. Eine Zielkorrektur verlangt drei
übereinstimmende gültige Fenster, lokale Bestätigung innerhalb 2 dB und bei
Verstärkung mindestens 3 dB Spitzenreserve. Totband: 2 dB, Schritt: 0,5 dB
alle 2 s, Ausgangskorrektur maximal ±6 dB. Die erste Entscheidung und ein
Richtungswechsel brauchen drei passende Fenster. Weitere Schritte brauchen
jeweils eine vollständige Messung der bereits angewandten Korrektur.
Sprechpausen, fehlendes Feedback und Konflikte halten die Korrektur im selben
Gespräch; dabei werden keine weiteren Schritte erlaubt. Erst explizites
Abschalten neutralisiert sie in 0,5-dB-Schritten je zwei Sekunden. `SendChain::tune` setzt eine langsame
Peer-Korrektur nach dem festen Stimmkompressor und vor dem Ausgangslimiter;
damit komprimiert der Kompressor die Korrektur nicht wieder weg. Sie wird pro
Block weich eingeblendet. Der lokale Mikrofon-AGC behält sein Ziel; ein zweiter
schneller AGC entsteht nicht. Peer-Korrekturen lernen weder die VAD noch den
Systemregler um. Solange eine Peer-Korrektur wirkt, bleibt der Ausgangslimiter
auch bei abgeschalteter Ausgangsnormalisierung aktiv. Fehlende lokale Sprache,
widersprechende Messung, fehlende Spitzenreserve und die Regelgrenze erscheinen
im Peer-Status statt pauschal als aktive Regelung.
Bei einer neuen Verbindung wird nur das Empfangsfeedback zurückgesetzt.
Die nächste DSP-Verarbeitung erhält eine frische Pegelkennung, damit gültige
Hints auch ohne Einstellungsänderung angenommen werden. Lokale Kalibrierung
und noch ausstehende Kompensation nativer Mikrofonänderungen bleiben erhalten.

### Gute Audiopegel und verständlicher Status

`audio.remember_good_levels` ist standardmäßig aktiv. Die lokale Datei
`audio-calibration.toml` enthält höchstens 16 Profile, 30 Tage gültig,
zugeordnet über Backend und tatsächliche Mikrofon-ID. Sie enthält weder
Audiosamples noch Geräte-/Kontodaten anderer Teilnehmer und geht nicht mit
Passports auf andere Rechner. Dateizugriffe sind privat, atomar und durch eine
prozessübergreifende Sperre geschützt; sie laufen außerhalb der Audiothreads.

Ein Profil wird nach mindestens 20 s und fünf gültigen, lokal bestätigten
Empfangsfenstern im Zielbereich gespeichert. Softwaregain darf höchstens
1 dB, Peer-Korrektur höchstens 0,5 dB schwanken. Capture-Clipping,
Softwareüberlast oder starke Limiter-Eingriffe verhindern die Speicherung
(maximal 1 dB Limiterreduktion und 1 % begrenzte Samples). Gespeicherter
Softwaregain bezieht sich auf den ursprünglichen Systempegel, damit dessen
Wiederherstellung den nächsten Start nicht verfälscht.

Wiederverwendung verlangt dasselbe Mikrofon, passende Verarbeitung und einen
Systempegel innerhalb 0,5 dB des gespeicherten Ausgangswerts. Positive
Startverstärkung ist auf 6 dB begrenzt. Eine gespeicherte Peer-Korrektur ist nur
ein Kandidat: Erst frisches, lokal bestätigtes Feedback in derselben Richtung
erlaubt die üblichen 0,5-dB-Schritte. Hardwarepegel werden nicht aus Profilen
zurückgeschrieben. Ohne stabile Geräteidentität oder bei kaputtem/abgelaufenem
Profil lernt ICU normal neu.

Unter „Pegel automatisch → Details“ liegen „Gute Mikrofonpegel merken“, „Letzten guten Zustand wiederherstellen“, „Für dieses Mikrofon neu
lernen“ und „Audio-Diagnose kopieren“. Wiederherstellen übernimmt nur die
Klangverarbeitung: Mute, PTT, Geräte und Tastenkürzel bleiben erhalten.
„Neu lernen“ verwirft das Geräteprofil und setzt die adaptive Verarbeitung
zurück; eigene Systemänderungen werden weiterhin nur bedingt zurückgestellt.

Der typisierte Audiostatus trennt passenden Pegel, Nachregeln, gehaltene
Korrektur, fehlende/widersprechende Messungen und Überlast. Ein farbiges Symbol
mit zugänglicher Klartextbeschreibung steht direkt beim Regler; die Details
zeigen Empfängerzahl und Korrekturen. Capture-Clipping, vor RNNoise begrenzte
Softwareverstärkung und dauerhaft begrenzter Ausgang werden getrennt benannt.
Mehr als 3 dB Limiterreduktion über eine Sekunde geeigneter Sprache sperrt
weitere Anhebungen. Diagnosen enthalten Zahlen und Zustände, keine Audiosamples.

VAD-bestätigte Sprache steuert Mikrofon-Leveler und lokale Feedbackmessung
auch ohne vorherige Rauschpause. Sie lernt nicht den Rauschboden an; damit
friert die Regelung bei anhaltender Sprache nicht ein. Die absolute
Mindestlautstärke bleibt wirksam. Ohne VAD-Bestätigung (Empfang) gilt weiterhin
der Mindestabstand zum Rauschboden; negative VAD-Urteile halten den Schätzer.

`audio::level::SystemTuner` überträgt dauerhafte digitale Korrektur langsam auf
den Systemregler: erst nach 10 s geeigneter Sprache aufwärts, normalerweise
höchstens alle 5 s; 1 dB oder nächster nativer Schritt bis 3 dB, maximal ±12 dB
vom Ausgangswert. Für Verstärkung muss ein gemessener Rauschabstand von mindestens
10 dB und ausreichend Spitzenreserve vorliegen. Raw-Capture-Clipping hat Vorrang
und erlaubt eine Absenkung pro Sekunde. Stille, Mute und Testton lernen nichts.
Bestätigte Änderungen verschieben Schätzer und digitale Verstärkung invers;
Messfenster werden verworfen. Der Limiter und die bisherigen digitalen Grenzen
bleiben bestehen. Unplausible Reaktion oder Schreibfehler pausieren Systemtuning.
Neu auftretendes Capture-Clipping hat auch gegenüber der Reaktionsprüfung nach
einer vorherigen Pegelanhebung Vorrang: eine plötzlich lautere Passage führt
zur Absenkung und nicht zum Pausieren bei weiter übersteuertem Mikrofon.

PipeWire bindet den tatsächlich mit dem Capture-Stream verbundenen Source-Node,
abonniert dessen Props, schreibt `channelVolumes` und liest nach einem passenden
Core-Sync zurück. Kanalbalance und Mute bleiben erhalten; kein Aufruf von wpctl
oder pactl. Der konservative Regelbereich beträgt −60…0 dB; Hardwaretyp/-schritt
sind dort nicht zuverlässig bekannt und werden nicht als echte Hardware-Gain
beworben. WASAPI verwendet das konkrete geöffnete Capture-Endpoint,
`IAudioEndpointVolume` einschließlich Range/Step und `QueryHardwareSupport`;
Volume-Callbacks unterscheiden eigene Änderungen von externen. CPAL bleibt bei
Softwaretuning. Geräteaufrufe laufen außerhalb der Echtzeit-Callbacks.

Pro Device verhindert ein OS-Dateilock im privaten ICU-Cache mehrere regelnde
ICU-Prozesse. Externe Änderungen geben die Kontrolle ab, auch bei einem später
wiederhergestellten identischen Wert (Änderungszähler). Erneutes Aktivieren des
Schalters oder ein neuer Anruf erlaubt einen neuen Versuch. Bei Disconnect,
Deaktivierung, Gerätewechsel und geordnetem Ende wird nur dann der Originalwert
wiederhergestellt, wenn ICU noch seinen eigenen letzten Wert vorfindet. Externe
Werte werden niemals zurückgesetzt. Ein Prozessabbruch oder verschwundenes Gerät
kann die Wiederherstellung verhindern; es gibt keine spätere blinde Wiederherstellung.

Diese Regelung kann beschädigte Aufnahme nicht reparieren und garantiert keinen
Rauschabstandgewinn bei einem softwarebasierten Systemregler. Physische Akustik,
AEC-Stabilität und herstellerspezifische Hardware benötigen die [Live-Tests](../development/testing.md).


## 7. Hotkeys

| OS | Backend | Details |
| --- | --- | --- |
| Linux | Portal GlobalShortcuts (ashpd 0.13.13, Feature `global_shortcuts`) primär. API 0.13: `GlobalShortcuts::new()`, `create_session(CreateSessionOptions::default())`, `bind_shortcuts(&session, &[NewShortcut::new(id, desc).preferred_trigger(..)], None, BindShortcutsOptions::default())` (vier Argumente), Ergebnis über das synchrone `Request::response()`; Signale über `receive_activated()` / `receive_deactivated()` → Press/Release, je Shortcut nach `timestamp()` geordnet (ein älteres Ereignis als das zuletzt verarbeitete wird verworfen, Rücksprünge über 5 s gelten als Uhrsprung), nur für die eigene Session; beim Beenden wird die Session geschlossen, schließt das Portal sie (Closed), wird neu gebunden. `Cancelled`, Bestätigen ohne Taste oder 60 s ohne Antwort → `portal-declined`: eine Entscheidung des Nutzers, **kein** evdev-Fallback (2026-09-26); fehlt das Portal oder scheitert es → nicht verfügbar, unter „Automatisch“ dann evdev (Spike 4a, allerdings mit ashpd 0.11.1 und ohne bestätigten Dialog — der interaktive Nachweis steht in der Live-Test-Checkliste aus). | Anzeige des aktiven Backends in den Settings; Dialog-Hinweis beim ersten Binden |
| Linux evdev (Wahl oder Rückfall) | evdev 0.13.2 ohne `grab()`, **nur lesend** geöffnet (`File::open` + `Device::from_fd`; `Device::open` versucht erst Lesen/Schreiben) + `into_event_stream()` (Feature `tokio`), alle lesbaren `/dev/input/event*`, Tasten über `KeyCode` (in 0.13 heißt der Typ `KeyCode`, nicht `Key`) inkl. `KeyCode::BTN_SIDE`/`KeyCode::BTN_EXTRA`; Hotplug per inotify auf `/dev/input`. Läuft, wenn der Nutzer es wählt, und unter „Automatisch“, wenn das Portal fehlt oder scheitert — nie nach einem abgelehnten Portal-Dialog (2026-09-26). Kein Opt-in im strengen Sinn also: ohne Portal liest ICU die Geräte, sobald die Gruppe `input` es erlaubt; die UI erklärt die Gruppe und den Keylogger-Aspekt. Gibt es weder Portal noch lesbare Geräte (`none`, typisch X11 ohne GlobalShortcuts wie XFCE, Cinnamon, i3), nennt das Banner `sudo usermod -aG input $USER` plus Ab- und Anmelden und bietet „Stattdessen automatisch senden“ (2026-09-27). Spike 4a: auf Chris' Host ohne Konfiguration lesbar. | Einzige Option für Maustasten unter Wayland |
| Windows | Raw Input (`hotkey/rawinput.rs`, 2026-09-26): ein prozessweiter Leser-Thread mit Message-Only-Window, `RegisterRawInputDevices` Keyboard (0x01/0x06) mit `RIDEV_INPUTSINK`, Mouse (0x01/0x02) nur solange eine Maustaste belegt ist oder „Aufnehmen“ läuft, sonst mit `RIDEV_REMOVE` abgemeldet (2026-09-27: eine 1–8-kHz-Maus weckte den Leser bei jeder Bewegung); `RI_KEY_MAKE/BREAK`, `RI_MOUSE_BUTTON_4/5_DOWN/UP` → `BTN_SIDE`/`BTN_EXTRA`. Die Registrierung gilt je Prozess, und winit registriert beim Start der Event-Loop sein eigenes Fenster: ein 2-s-Timer prüft `GetRegisteredRawInputDevices` und registriert neu. Auto-Repeat fällt über die Menge gehaltener Tasten weg. Backend und „Aufnehmen“ teilen sich den Leser; „Aufnehmen“ nimmt auch Kombinationen (seit 2026-09-27, `hotkey::chord::Recorder`): gehaltene Modifier und die erste andere Taste werden ein Trigger wie `CTRL+SHIFT+F12` ohne `key`, ein allein gedrückter und losgelassener Modifier die Taste selbst (Rechts-Strg), Maustasten und Tasten ohne Trigger-Namen stehen allein. Tastennamen bleiben evdev-Namen (`hotkey/keys.rs`: Scancode Set 1 + E0 → evdev-Code, Pause über `VK_PAUSE`). `[hotkeys] backend` wird ignoriert, Anzeige „Raw Input“; die UI blendet Portal/evdev aus. Kein Schlucken der Taste. Elevated Spiele im Fokus: Eingaben unsichtbar (UIPI), UI-Hinweis (dann ICU als Administrator starten). Weil dabei auch das Loslassen verloren geht, lässt der Leser gehaltene Tasten selbst los (2026-09-27, `hotkey/held.rs`, §16): bei einem Vordergrundfenster höherer oder nicht abfragbarer Integritätsstufe, Desktopwechsel, Sperren/Trennen der Sitzung, entferntem Gerät (`RIDEV_DEVNOTIFY`, `GIDC_REMOVAL`) und nach 5 min ohne Auto-Repeat. Unter Wine gebaut, mit Spielen auf echtem Windows **offen** | `../research/hotkey-wayland.md` |

Bindung: Aufnahme-Modus in den Settings („Taste drücken“), Speicherung als Backend-neutrale Beschreibung (Keycode/Keysym/Maustaste + Modifier). Zweite Bindung für Mute-Toggle, dritte für Deafen-Toggle, vierte für PTT/VAD-Umschalten, fünfte für Kamera an/aus (alle optional; `camera` seit 2026-09-26, `Command::ToggleCamera`).

**Standard-PTT je Backend** (2026-09-27): `HotkeySettings::default()` trägt zwei Beschreibungen derselben Bindung — das Portal bekommt den Trigger `CTRL+SHIFT+F12` vorgeschlagen, evdev und Raw Input lesen `KEY_RIGHTCTRL` (Rechts-Strg). Die Standardtaste ist also je nach Backend eine andere, und ein Passport nimmt beide Felder auf ein anderes System mit. Welche Taste der Portal-Dialog tatsächlich vergeben hat (der Nutzer kann dort eine andere wählen), meldet das Portal als `trigger_description`; `hotkey::portal_trigger` merkt sie für die laufende Sitzung, und Hauptfenster-Taste, Einstellungen und Einrichtung zeigen sie statt des Vorschlags. In die Einstellungen zurückgeschrieben wird sie nicht: sie ist lokalisiert („Strg+Umschalt+F12“) und taugt nicht als `preferred_trigger`.

**Live und Aufnahme** (2026-09-26): `hotkey::supervise` hält das Backend für die Lebensdauer der Engine im Takt mit den Einstellungen — geänderte Bindungen oder Methode gelten, sobald `settings.hotkeys` 1,5 s stillsteht (Tippen), das alte Backend endet vorher (ein gehaltenes PTT wird losgelassen, evdev-Leser werden sofort abgebrochen, nicht erst beim nächsten Aufwachen); `Command::RetryHotkeys` (`AppState::hotkey_retries`) bindet sofort neu, etwa damit das Portal nach `portal-declined` erneut fragt; ein von selbst beendetes Backend wird nach 2 s neu gebunden. Backend-Anzeige (`AppState::hotkey_backend`): `portal`, `evdev (<n> devices)`, `portal-declined`, `none`, leer bei `--no-hotkeys`; `starting` (2026-09-26) setzt `main.rs` vor dem Start des Supervisors, bis das erste Backend meldet — unter KDE, solange der Portal-Dialog offen ist; Tastenkürzel-Seite und Einrichtung zeigen dann „Tastenkürzel werden eingerichtet“ statt „abgeschaltet“. Aufnahme für die Settings: `hotkey::capture_next_key(timeout)` verteilt je Plattform (Windows: Raw-Input-Leser); unter Linux öffnet `hotkey::evdev::capture_next_key` alle lesbaren Geräte nur lesend, ohne grab, und liefert den ersten Tastendruck als Bindungsnamen (`KEY_…`, `BTN_SIDE`, `code:<n>`); Loslassen, Auto-Repeat, Links-/Rechtsklick und Touch-Kontakte zählen nicht.

## 8. Kamera und Video

- Capture hinter `CameraSource`; v1 nokhwa 0.10.11 mit `input-native` und **eingeschaltetem** Default-Feature `decoding` (Spike 5 dekodiert MJPEG über `Buffer::decode_image::<RgbFormat>()`; ohne `decoding` gibt es dafür keinen Dekoder). `Camera::new(CameraIndex::Index(0), RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(..)))`. Ein Lazy-Load des Windows-Backends ist **nicht möglich** (`windows` 0.62 importiert MF per `raw-dylib`); Windows-N ist damit in v1 nicht unterstützt, siehe §2. Linux: Capture-Node explizit (Spike 5: Metadaten-Node überspringen). Format 320×240 @ 15 fps: zuerst `Closest(320×240, MJPEG, 15)`; scheitert das — nokhwa-core 0.1.9 behält dabei nur MJPEG und nimmt Raten nur von genau 320×240 —, öffnet ICU die Kamera mit irgendeinem ihrer Formate, liest die Liste und wählt selbst (`camera::choose_format`): MJPEG, dann YUYV, dann NV12/RGB; darin die kleinste Auflösung ab 320×240 (sonst die größte darunter) und die Rate am nächsten an 15 fps. Größere Bilder schneidet ICU auf die 4:3-Mitte und mittelt sie auf 320×240 herunter (`camera::fit_capture`). Bietet die Kamera kein lesbares Format, sagt das Banner das sofort, ohne die fünf Wiederholversuche (2026-09-27; vorher öffnete eine reine YUYV-Kamera nie).
- Encoder openh264 0.9.8, Feature `source` (ist Default): `EncoderConfig::new().bitrate(BitRate::from_bps(tier.bitrate())).max_frame_rate(FrameRate::from_hz(camera::frame_rate(tier))).usage_type(UsageType::CameraVideoRealTime).rate_control_mode(RateControlMode::Bitrate)` — die Rate der Stufe (15/7,5/5/3 fps), denn openh264 plant jedes Bild mit Bitrate ÷ konfigurierter Rate; bis 2026-09-27 stand dort 15 für jede Stufe, und T1–T3 lieferten nur ≈ 50/20/8 kbit/s (`camera::each_tier_spends_its_bitrate` misst 150/101/58/39). Ausgedünnt wird nach Aufnahmezeit, nicht nach Bildzahl (`camera::Pacer`): eine Kamera, die schneller als 15 fps liefert (ein Modus nur mit 30 fps, den die Formatwahl nehmen kann), verdoppelte sonst die Bitrate — openh264s Frame-Skipping fängt das nicht ab (gemessen 298 statt 150 kbit/s), dann `Encoder::with_api_config(OpenH264API::from_source(), cfg)` (ein `Encoder::with_config` gibt es nicht). Encode über `encode_at(&impl YUVSource, Timestamp)` mit der Aufnahmezeit des Bildes (zugleich der Draht-Zeitstempel) und `YUVBuffer::from_rgb_source(..)`; IDR erzwingen per `force_intra_frame()`, sonst alle 2 s; SPS/PPS vor jedem IDR. 150 kbit/s bei 15 fps (`VideoTier::T0`) ist der v1-Zielwert, kein Messwert — Spike 5 kam lichtbedingt nur auf 3,7–6 fps, die Bitrate bei 15 fps ist laut `../research/camera-video.md` unbelegt und in den Hardwaretests nachzumessen. Liefert die Kamera weniger als 15 fps, plant T0 weiter für 15 und bleibt entsprechend unter 150 kbit/s (offen). Frame → Uni-Stream mit Header (`docs/reference/protocol.md §4`). Bei Flow-Control-Stau (50 ms `open_uni`-Timeout): Frame verwerfen, nicht puffern.
- **Empfangsobergrenze vor dem Decoder** (2026-09-26): `video::decode` liest jedes SPS des Access Units (`video/h264.rs`, Emulation-Prevention entfernt, nur bis `frame_mbs_only_flag`) und verwirft Bilder über 640×480, mehr als 4 Referenzbilder, andere Profile als Baseline/Main und SVC/MVC-NALs, bevor openh264 sie sieht; nach dem Decode prüft es die Größe noch einmal vor der RGBA-Allokation. Ohne das nahm openh264 bis Level 5.2 an (rund 9,4 Mpx, knapp 38 MB RGBA je Bild und Sprecher auf dem einen Decode-Thread). Ein verworfenes Bild zählt wie ein Decode-Fehler.
- **Netzwerk-adaptive Qualität** (neu, 2026-09-21): `crate::netcond::NetworkTracker` sampelt alle 2 s `quinn::Connection::stats()` (`path.{rtt,min_rtt,cwnd,congestion_events,lost_packets,sent_packets}`, bislang in diesem Repo ungenutzt) und `Connection::datagram_send_buffer_space()` und faltet sie mit `LossHint`/`VideoCongestionHint` (`protocol.md` §2.2, neue Variante 16) zu einer `NetworkCondition`. `crate::quality::QualityController` fährt daraus eine Qualitäts-Ladder für `VideoTier` (`crate::video::VideoTier`): T0 150 kbit/s @ 15 fps → T1 100 kbit/s @ 7,5 fps → T2 60 kbit/s @ 5 fps → T3 40 kbit/s @ 3 fps → T4 pausiert. Abstieg 1–2 bestätigende Ticks, Aufstieg 6+ (mit Backoff bei Flapping) — bewusst asymmetrisch, damit ein zu früher Aufstieg nicht dieselbe Stauung reproduziert, die Audio-Datagramme (drop-old-favor-new, `docs/research/transport.md`) gefährden würde. **Audio-Bitrate bleibt von dieser Ladder unberührt** (fest 48 kbit/s, `dsp::opus`); die Wire-Kosten-Rechnung in `docs/research/codec.md` zeigt, dass ein einzelner Video-Tier-Schritt mehr Bandbreite spart als das gesamte erreichbare Opus-Bitratenfenster. `camera::run_sender` liest den Tier einmal pro Iteration (`CameraLink::video_tier()`, ein `AtomicU8` auf `Shared`) und baut den `Encoder` bei Tier-Wechsel neu (openh264s sicheres Rust-Binding hat keinen Laufzeit-Bitraten-Setter); ein frisch gebauter Encoder liefert ohnehin ein IDR als erstes Bild. Erhöhte RTT, Warteschlangenverzögerung, moderater Audioverlust ab 30 ‰ und `VideoCongestionHint` ab 50 ‰ senken die Qualität nur bis T3 (40 kbit/s, 3 fps); dort bleibt ein Bild erhalten, bis der Druck nachlässt (2026-10-10). T4 schützt Audio bei mindestens 150 ‰ Audioverlust oder lokalen `open_uni`-Drop-Bursts in zwei aufeinanderfolgenden 5-s-Fenstern. Ein Burst wird je Fenster nur einmal gezählt: vor dem 2026-10-10 konnte derselbe unveränderte Drop-Zähler beim nächsten Tick fälschlich als zweiter Burst pausieren. Ein pausierter Stream darf bei nur noch moderatem Druck nach sechs Ticks wieder T3 senden. Der Server misst `VideoCongestionHint` an weitergeleiteten Bildern; pausiert käme kein neuer Hinweis, deshalb hält ein alter Viewer-Hinweis T4 nicht fest. Tier-Wechsel loggen die gemessene `NetworkCondition` und den lokalen Drop-Zähler. Bei jedem `Welcome` beginnen `LossHint` (FEC) und Tracker von vorn: auf sauberem Netz kommt kein neuer Hinweis, und ein alter hielt sonst FEC hoch und die Stufe auf T4 (2026-09-27). `T4` (pausiert) encodiert und sendet gar nichts, ohne Audio zu berühren. Kein Nutzer-Schalter (Analogie zum Jitter-Puffer: automatisch, unsichtbar), aber ein kleines Statuslabel neben der RTT-Anzeige ("Video reduziert"/"Video pausiert (Netz)"), weil eine Degradation für die Gegenseite sichtbar ist.
- Decoder je Speaker über `Decoder::new()` und `decode(&[u8]) -> Result<Option<DecodedYUV>>` (das `Option` bedeutet „noch kein vollständiges Bild“, nicht Fehler); erster Frame ohne IDR → `KeyframeRequest`; ≥ 3 s ohne Frame → Kachel ausblenden. Seit 2026-09-26 in `seq`-Reihenfolge: ein Frame, der nicht neuer ist als der zuletzt gesehene (modulo 2^16), wird verworfen — ein Rückschritt über 32 Frames gilt dagegen als neuer Stream auf einer wiederverwendeten Session-ID und beginnt beim nächsten IDR; eine `seq`-Lücke oder ein Decode-Fehler lässt bis zum nächsten IDR nichts mehr dekodieren und fordert einen an (≤ 1/s je Sprecher), statt bis zum periodischen IDR (2 s) kaputtes Bild zu zeigen. Der Sender schickt nach einem nicht gesendeten Bild (Flow Control) von sich aus ein IDR, höchstens eines je Sekunde.
- Vorschau im Settings-Dialog vor dem Einschalten: `CameraHandle::set_preview(true)` lässt die Kamera für die lokale Vorschau laufen, ohne zu encodieren, zu senden oder `CAMERA_ON` zu setzen (2026-09-26; vorher lief sie nur bei `camera_on`, und das Prüfen des eigenen Bilds meldete allen „Kamera an“). Kamera an/aus per Button und Hotkey (`camera`-Bindung); `camera_on` im `SetState`.
- Robustheit (2026-09-26): jedes Bild läuft durch `RgbFrame::checked` — Länge muss Breite × Höhe × 3 sein (bei MJPEG gilt die Größe aus dem SOF-Kopf, wenn sie die Länge erklärt), ungerade Seiten werden um eine Spalte/Zeile gekürzt, sonst wird das Bild verworfen; vorher asserteten openh264 und egui darauf, im Release-Build (`panic = "abort"`) das Ende des Clients. Öffnet die Kamera nicht oder liefert sie 5 s kein Bild, versucht `CameraHandle::sync` es mit Backoff 1, 2, 4 … 30 s erneut (vorher bei jedem UI-Frame); nach fünf Fehlversuchen in Folge `Command::CameraFailed` — die Engine schaltet die Kamera aus und zeigt den Grund im Banner. Gerätewechsel oder erneutes Einschalten fangen von vorn an. `--headless` folgt `camera_on` ebenso.
- **Nur senden, wenn jemand zuschaut** (2026-09-24): der Client setzt `viewing` (`protocol.md` §2.1), solange das Overlay an oder das Hauptfenster sichtbar ist (nicht im Tray, nicht minimiert; `minimized` unbekannt, z. B. Wayland → sichtbar). Die UI meldet Sichtbarkeitswechsel als `Command::SetWindowVisible`; der Startwert folgt `start_minimized`, ein Client ohne Fenster (Tests) zählt als Zuschauer. `camera::run_sender` liest `CameraLink::send_tier()`: das ist `T4` (pausiert), solange kein Peer `viewing` trägt, sonst der Tier der Ladder. Die lokale Vorschau läuft weiter; beim Fortsetzen liefert der frische Encoder ein IDR. Allein im Raum ist das immer so (keine Peers → `T4`); Audio folgt derselben Regel (§6 Sender, 2026-09-27).
- **Bilder in der Teilnehmerliste** (2026-10-12): Jedes neu dekodierte Bild weckt das Hauptfenster vom Decode-Thread aus (`VideoFrames::set_waker`, `ui::repaint_for_video`); während eines Fensterzugs unter Windows nicht. Vorher holte nur der 33-ms-Takt die Bilder ab: gegen 15 fps stand jedes Bild abwechselnd einen und zwei Takte, gelegentlich fiel eines aus.
- **Anzeige beim Empfänger bestätigt** (2026-09-28): Nur das Overlay meldet, nach erfolgreicher Übergabe seines Bildes an das Betriebssystem; das Hauptfenster zeigt Kamerabilder in der Teilnehmerliste nur als Vorschau-Avatar, ohne dies als Anzeige zu melden — sonst zählte jeder mit offenem Fenster und sichtbarer Zeile als Zuschauer, unabhängig davon, ob überhaupt hingesehen wurde. Die Engine begrenzt `VideoDisplayed` auf einen Bericht je Sekunde und Sender. Der Server prüft die Berechtigung und meldet `VideoViewer` an den Sender. Der Kamerabereich zeigt die Zahl der in den letzten drei Sekunden bestätigten Empfänger, Namen im Hovertext; ohne Bestätigung erscheint „Warte auf angezeigtes Video“. Der Bericht belegt keine menschliche Aufmerksamkeit und erlischt bei Bildstillstand, geschlossenem Fenster oder Verbindungsverlust. Protokoll-Minor 4 erfordert ein gemeinsames Client-/Server-Update.
- Kein MJPEG: `codec = 2` darf nicht gesendet werden, weil es dafür keine Aushandlung gibt (`protocol.md` §4); empfangene Frames mit `codec = 2` verwirft `Receivers::accept`. Das frühere Feature `mjpeg` zog nur `zune-jpeg` herein, einen Dekodierpfad gab es nie; es ist seit 2026-09-27 entfernt (§16).

Kamera und Overlay behalten ihre Einstellungen beim nächsten Start (2026-09-28): Gerätewahl, Kamera an/aus, Overlay an/aus und dessen Darstellung stehen in `settings.toml`. Kamera-Button, Hotkey und der Schalter „Kamera einschalten“ in den Einstellungen ändern denselben gespeicherten Zustand; die lokale Vorschau bleibt temporär. Der erste Client-Zustand und `Hello` übernehmen den gespeicherten Kameraschalter.

## 9. Overlay

Eigener Thread, eigene Compositor-Verbindung, kein winit. Inhalt: runde, opake Kacheln der Teilnehmer mit Kamera (mittig quadratisch beschnitten wie die kleinen Avatare, `ui/widgets.rs::avatar`), und kleinere runde Namensschilder für Teilnehmer ohne Kamera, auch während sie schweigen (Initialen statt Bild, `overlay.voice_plates`, Standard an; Kreisdurchmesser 60 % der Kamerakachel); der Name steht dabei nicht mehr auf einem Streifen über dem Bild, sondern außerhalb, auf einem papierfarbenen Schildchen, das bündig an der Kachel sitzt und nur so breit ist wie der Name selbst: der Bogen beginnt um 6 Uhr (unterhalb der Mitte) und läuft von dort ausschließlich gegen den Uhrzeigersinn die rechte Seite hoch, mit „…" gekürzt, wenn er den Bogen sprengt (2026-09-28: vorher ein abgedunkelter Streifen über dem Bild, dazu waren beide Hintergründe teiltransparent; noch am selben Tag ersetzt durch den außenliegenden, um 6 Uhr zentrierten symmetrischen Bogen mit Lücke zur Kachel und fest auf die maximale Bogenlänge reserviertem Hintergrund — jetzt einseitig ab 6 Uhr, bündig und namensbreit). Jede Kachel, mit oder ohne Kamera, wächst sanft auf ihre Sprechgröße (Regler „Aktive Sprecher“, ohne Einstellung 20 % größer), während ihr Teilnehmer spricht, und schrumpft ebenso sanft zurück, sobald er aufhört (2026-09-28: gedämpfte Annäherung an das Ziel, ~300–500 ms je Richtung; der reservierte Platz ist immer der der größeren der beiden Größen, damit das Wachsen kein Neuplatzieren der Kacheln oder ein Resize des Overlay-Fensters auslöst). Namensschilder ohne Kamera folgen denselben Regeln für Abdunkeln bei Stille und Sprechhervorhebung; Sprechbeginn und -ende ändern weder die sichtbaren Teilnehmer noch die Fenstergröße. Rendering (2026-09-28): Bei kompatibler Hardware rendert wgpu die gesamte Overlay-Szene (Kamerabilder, Kreise, Schildchen und Text) in eine transparente GPU-Textur. Ein animierter Nebel in der eingestellten Rahmenfarbe ersetzt den Speaking-Rahmen, mit 150 ms Einblenden und 700 ms Ausblenden; ein erneuter Sprechbeginn setzt die Helligkeit ohne Sprung fort. Die Kamera-Kacheln bleiben flach, Namen und Gesichter bleiben frei. 24 skalierte Pixel zusätzlicher Platz je Seite halten den Nebel innerhalb einer festen Kachelfläche. Text wird aus der eingebetteten Inter mit ab_glyph rasterisiert und als GPU-Textur zwischengespeichert. Video-Dekodierung bleibt unverändert. GPU-Bilder werden über höchstens zwei asynchrone Readback-Aufträge als premultiplied BGRA an die bisherigen Plattformfenster übergeben; das Ziel sind 30 fps unabhängig von der Kamera-Bildrate. Nur tatsächlich präsentierte Bilder lösen `VideoDisplayed` aus. Ohne Hardwareadapter, bei Initialisierungsfehler, Geräteverlust oder Readback-Timeout bleibt beziehungsweise wird die bisherige CPU-Komposition (`overlay::compose`, 15 fps, Speaking-Rahmen) aktiv. Pro Overlay-Start gibt es einen GPU-Initialisierungsversuch; keine neue Einstellung und keine Protokolländerung.

GPU-Prüfung: `cargo test --locked -p icu-client --lib overlay::` prüft unter anderem Ein-/Ausblendung und fehlende Adapter. Der explizite Hardwaretest `cargo test --locked -p icu-client --lib overlay::gpu::tests::hardware_render_and_device_loss -- --ignored --nocapture` prüft vier wechselnde 320×240-Bilder, alle Kachelgrößen, transparente BGRA-Ausgabe, gemischte Kacheln bei 200 %, Abschalten, Geräteverlust und das 30-fps-Zeitbudget (nach Aufwärmen, mindestens 28 fps). Mit `ICU_OVERLAY_SHOTS=<vorhandener Ordner>` schreibt er PNGs. Das ist ein Offscreen-Test; native Windows-/Wayland-Präsentation über Spielen und Audio-Kontinuität unter GPU-Last müssen zusätzlich live geprüft werden.

| OS | Fenster | Belegt |
| --- | --- | --- |
| Linux Wayland | smithay-client-toolkit 0.21.1 Layer-Shell: `LayerShell::bind(&globals, &qh)`, `create_layer_surface(&qh, surface, Layer::Overlay, Some("icu-overlay"), None)`, `set_keyboard_interactivity(KeyboardInteractivity::None)`, **`set_exclusive_zone(-1)`** (so im Spike belegt; damit ignoriert die Surface fremde Exclusive-Zonen und bleibt an der gewählten Ecke), Anker als Bitflags (`Anchor::TOP \| Anchor::RIGHT`) + Margin, leere Input-Region über `compositor::Region::new(..)` und `set_input_region(Some(region.wl_region()))`, Buffer aus `shm::slot::SlotPool` mit `wl_shm::Format::Argb8888`. `commit()` und `set_input_region()` kommen aus dem Trait `shell::WaylandSurface`, der importiert sein muss. Event-Loop calloop mit Timer (Spike 4b: `blocking_dispatch` allein blockiert) — dafür muss das SCTK-Feature `calloop` aktiviert sein, es ist optional. GNOME: Layer-Shell fehlt → Overlay „nicht verfügbar“. | Spike 4b PASS auf KWin 6.7.5, damaliger Spike mit SCTK 0.19.2; heutige Abnahme siehe Live-Test-Checkliste |
| Windows | `overlay/windows.rs` (2026-09-26): `CreateWindowExW(WS_EX_LAYERED\|WS_EX_TRANSPARENT\|WS_EX_TOPMOST\|WS_EX_NOACTIVATE\|WS_EX_TOOLWINDOW, WS_POPUP)`, top-down 32-bit-DIB-Section, je Bild `UpdateLayeredWindow(ULW_ALPHA)` mit 30 fps (GPU) beziehungsweise 15 fps (Software), Fenstergröße = Kachelgruppe; Platz im Arbeitsbereich des primären Monitors (immer dieser, eine Monitorwahl gibt es nicht) nach Ecke + Offset; Kacheln, Namensschilder, Schriftgröße und Offset werden mit der DPI dieses Monitors skaliert (`GetDpiForWindow` ÷ 96, je Bild; 2026-09-27 — vorher physische Pixel, bei 150 %/200 % also ⅔ bzw. ½ so groß); `HWND_TOPMOST` etwa einmal je Sekunde neu gesetzt. Zeichnen (`Scene`) teilt es mit Wayland. Nur über Borderless-/FSO-Spielen sichtbar, exklusives Vollbild verdeckt es wie jedes Fenster; darauf weist der Schalter „Anzeigen“ hin. | Doku belegt, gebaut; über Spielen auf echtem Windows **offen** |

Kachelgröße (2026-10-02, vorher fest klein/mittel/groß = 120/240/360 px): zwei Schieberegler setzen den Kamerakachel-Durchmesser in Layout-Pixeln, je 80–480 (`config::TILE_SIZES`): „Inaktive Sprecher“ (`overlay.size`, Standard 120) und „Aktive Sprecher“ (`overlay.speaking_size`, 2026-10-03; nicht gesetzt 20 % mehr als `overlay.size`, wie vor dem Regler; kleiner als die inaktive Größe ist erlaubt, die Kachel schrumpft dann beim Sprechen). Namensschilder ohne Kamera haben jeweils 60 % davon. Die Schrift des Namens folgt dem gerade gezeichneten Durchmesser, auch während der Sprechanimation: 4 px + Durchmesser/15, mindestens 12 px (12/20/28/36 px bei 120/240/360/480; 2026-10-03, vorher 9 px + Durchmesser/40, bei großen Kacheln verschwindend klein). Eine alte `settings.toml` mit `size = "small"`/`"medium"`/`"large"` wird als 120/240/360 gelesen und als Zahl zurückgeschrieben. Eine Größenänderung legt die Layer-Surface nicht neu an, sie ändert nur ihre Größe, und zwar im selben Commit wie das erste Bild der neuen Größe; der GPU-Pfad wartet dafür auf dieses eine Bild statt es asynchron zurückzulesen (höchstens 100 ms). Vorher (2026-10-03 behoben) ging die neue Größe allein hinaus, während der GPU-Pfad beim Ziehen gar kein Bild lieferte: KWin verschob bei `corner = "top"` das alte Bild seitlich, die Höhe sprang erst nach dem Loslassen nach. Configure-Größen übernimmt die Szene nicht mehr; sie sind ohnehin die angeforderten, beim Ziehen aber teils schon veraltet.

Lage und Abstand (2026-10-06): Die Plätze der Kacheln folgen ihrer gerade animierten Größe, einschließlich Namensband, mit nur 2 Layout-Pixeln Abstand. Beim Sprechen wächst oder schrumpft die Kachel weich; die folgenden Kacheln gleiten mit, statt einen dauerhaft für die maximale Sprechgröße reservierten Abstand zu lassen. Die Reihenfolge bleibt unverändert. Kacheln liegen weiterhin an der Randseite des Overlays (oben/unten und links/rechts, bei oben/unten mittig waagrecht mittig), wachsen also zur Bildschirmmitte. Der GPU-Halo-Rand (30 px) liegt um die ganze Gruppe; die Nebelränder benachbarter Kacheln dürfen überlappen und schaffen keine zusätzlichen Lücken. „Abstand zum Rand“ (0–200 px) setzt `overlay.offset` für beide Achsen gleich; getrennte ältere Werte bleiben bis zum nächsten Bewegen des Reglers erhalten.

Eigene Kachel (`overlay.show_self`, seit 2026-10-23 standardmäßig an, ⚙ → Overlay → „Mich im Overlay zeigen“; 2026-10-02): die eigene Kachel steht nach allen anderen, nach denselben Regeln — mit eingeschalteter Kamera das eigene Bild aus dem Vorschau-Slot des Aufnahme-Threads (`CameraHandle::preview_source`, nicht gespiegelt wie die Vorschau, kein Umweg über Encoder und Server), sonst ein Namensschild, sofern „Teilnehmer ohne Kamera“ an ist; „spricht“, solange das eigene Gate offen ist (`AppState::sending`). Die eigene Kachel löst kein `VideoDisplayed` aus. Allein im Raum zeigt das Overlay mit dieser Option nur sie.

Dem Mauszeiger ausweichen (`overlay.shy`, Standard aus, ⚙ → Overlay → Erweitert; 2026-10-02): Liegt der Mauszeiger auf dem Platz einer Kachel (ihr gerade animierter Platz einschließlich Namensband), blendet sich die ganze Reihe in 120 ms aus und in 300 ms auf der gegenüberliegenden Bildschirmseite wieder ein, mit demselben Abstand zum Rand: aus einer Ecke in die links/rechts gespiegelte (oben rechts ↔ oben links, unten rechts ↔ unten links), von oben mittig nach unten mittig und umgekehrt (`overlay::opposite`). Dort kehrt sie automatisch an die eingestellte Position zurück, nachdem der Zeiger die vollständig eingeblendete Reihe verlassen hat: `overlay.shy_return_ms`, Standard 3000 ms, 0–60000 ms, Regler „Zurück nach“ in Sekunden unter „Dem Mauszeiger ausweichen“. Erreicht der Zeiger die Reihe während der Wartezeit, beginnt die Zeit beim Verlassen erneut; erreicht er sie nach der Wechselsperre, weicht sie wie bisher sofort aus. Die Rückkehr nutzt dieselbe Aus-/Einblendung und Wechselsperre (2026-10-06). Wo sie erscheint (beim Start oder nach einem Seitenwechsel), bleibt sie, sobald sie ganz eingeblendet ist, mindestens die „Wechselsperre“ lang stehen (`overlay.shy_hold_ms`, 0–5000 ms, Standard 1000; Regler unter dem Schalter, nur sichtbar, wenn er an ist), auch unter dem Zeiger, und nimmt so lange keine Eingaben an, Klicks gehen durch; danach weicht sie aus, sobald der Zeiger auf einer Kachel liegt (unter Wayland meldet der Compositor einen dort ruhenden Zeiger womöglich erst bei der nächsten Bewegung). Die Sperre verhindert, dass ein Zeiger über beiden Plätzen die Reihe hin- und herspringen lässt; bei 0 kann genau das im Takt der Blenden passieren. (Noch am 2026-10-03 vorher fest: Eine unter dem Zeiger angekommene Reihe blieb, bis er 100 ms weg war und wiederkam, und nahm dabei Eingaben an.) Die Gegenseite wird nicht gespeichert: eine neue Position, „Dem Mauszeiger ausweichen“ aus oder ein Neustart des Overlays bringt die Reihe an die eingestellte Stelle zurück. (2026-10-03; vorher blendete sie sich an Ort und Stelle aus und kam eine Sekunde, nachdem der Zeiger weg war, zurück, zuerst jede Kachel für sich.) Eine ausgeblendete Reihe gilt nicht als angezeigt (kein `VideoDisplayed`). Windows liest dafür je Bild `GetCursorPos`, das Fenster bleibt durchgehend klick-durchlässig. Wayland kennt keine globale Zeigerposition: mit `shy` bekommt die ganz eingeblendete Reihe eine Input-Region (ihre Slots, ohne Lücken), nur so meldet der Compositor Enter/Motion; erreicht der Zeiger eine Kachel, gibt die Reihe ihre Region sofort ab, Klicks gehen dann wieder durch, und auf der Gegenseite entsteht eine neue Layer-Surface mit dem gespiegelten Anker. Ein Klick genau in dem Bild, in dem der Zeiger eine Kachel der ganz eingeblendeten Reihe erreicht (≤ 33 ms), kann dort ankommen statt im Fenster darunter; über einer Kachel setzt das Overlay den normalen Pfeil (`wp_cursor_shape_v1`, wo vorhanden). Ohne `shy` bleibt die Input-Region leer wie bisher.

Start: der Overlay-Thread meldet sich binnen 5 s; sonst gilt das Overlay als nicht verfügbar und der Thread endet von selbst, sobald sein Setup zurückkehrt (Stop-Flag, 2026-09-26; vorher lief er unbemerkt weiter).

Bedienung: an/aus (Button, Hotkey optional), Ecke (4 Optionen und oben/unten mittig) + Abstand zum Rand (Schieberegler), Kachelgröße still und sprechend (zwei Schieberegler), Bearbeitungsmodus mit Drag im Hauptfenster (nicht im Overlay selbst, das bleibt klick-durchlässig; mit „Dem Mauszeiger ausweichen“ unter Wayland bis auf den Moment, in dem der Zeiger eine Kachel erreicht, siehe oben).

## 10. UI

### Hauptfenster und Nutzeransicht

Das eframe/egui-Fenster startet mit 360×440 px, Minimum 360×340
(`main_window::MAIN_SIZE`, `MAIN_MIN_SIZE`). Vier Teilnehmer passen ohne
Scrollbalken; weitere Teilnehmer oder längere Banner lassen es wachsen.
Der Kopf hat eine Statusleiste mit Verbindungsdauer, RTT, Empfangsbalken und
Rollen-/Videohinweisen; darunter stehen Servername und die gemeinsame Kapsel
für Hell/Dunkel, Einstellungen und Beenden. Die Statusleiste öffnet die
Verbindungsdetails. Inter und Phosphor sind eingebettet, einschließlich Lizenz.

Teilnehmer zeigen Sprechstatus, Stumm/Taub, Kamera, Ping und lokale Lautstärke.
Frisches Video ersetzt den Initialen-Avatar; die eigene Zeile verwendet die
lokale Vorschau. Die Auswahl öffnet eine eigene Nutzeransicht. Admins können
fremde Nutzer stumm-/taubschalten, Einschränkungen aufheben, hinauswerfen oder
sperren. Vom Server erzwungene Einschränkungen lassen sich lokal nicht aufheben.
Die fokussierte Sprechfläche lässt sich mit Leertaste oder Enter halten.
Sie lässt PTT bei Loslassen, Fokusverlust oder ausgeblendetem Fenster los.
Allein im Raum zeigt sie „Allein“ und sendet nichts.

### Einstellungen und Einrichtung

Einstellungen sind ein eigenes Fenster mit Seitenleiste: Audio, Klang,
Tastenkürzel, Kamera, Overlay, Allgemein, Nutzer (nur Admins) und Über ICU.
Kamera enthält Gerätewahl, Ein-/Ausschalten und Vorschau. Overlay bündelt
Anzeige, Position, eigene Kachel, Teilnehmer ohne Kamera, Kachelgrößen und
Randabstand sowie „Sprech-Corona“ mit dem Farbwähler „Coronafarbe“; erweitert
kommen Anordnung, Abdunkeln und
Maus-Ausweichen mit Wechselsperre und Rückkehrzeit hinzu.
„Erweitert“ ergänzt technische Felder innerhalb derselben Karten. Änderungen
wirken sofort; Änderungen aus Tray, Hotkeys und Hauptfenster werden übernommen,
statt durch einen veralteten Entwurf überschrieben zu werden. Der Mikrofontest
bleibt auf Audio und Klang aktiv, damit Klangänderungen hörbar vergleichbar sind.
Jede Seite behält ihre eigene Scrollposition. Bei schmalem Fenster stehen
Beschriftungen über den Reglern; Tastaturfokus ist auch auf selbst gezeichneten
Schaltern und Symbolknöpfen sichtbar. Eine laufende Kürzelaufnahme endet beim
Seitenwechsel. Unter Windows bleiben dabei aufgenommene Modifikatortasten Teil
des Kürzels.

Ohne Rauschunterdrückung arbeitet die Sprachaktivierung mit einer festen
Energieschwelle von −40 dBFS. Hauptfenster und Einrichtung zeigen dann den
Mikrofonpegel mit dieser Schwelle; unwirksame Wahrscheinlichkeitsregler werden
ausgeblendet. Die erweiterte Eingangsverstärkung bleibt auch bei automatischer
Pegelregelung sichtbar, weil sie weiterhin vor dieser wirkt. Mit aktiver
Pegelautomatik schützt die ausgehende Normalisierung nur noch vor Spitzen.

„Meine Coronafarbe“ (2026-10-12, Protokoll-Minor 5) ist die eigene Farbe, nicht
mehr die aller Kacheln: Jeder wählt, in welcher Farbe er für alle im Raum
leuchtet, wenn er spricht. Der Client schickt sie 500 ms nach der letzten Wahl
(`SetCoronaColor`; der Farbwähler meldet jeden Schritt), der Server speichert sie
am Nutzer und verteilt sie (`UserInfo.corona_color`, `UserCoronaColor`). Das
Overlay zeichnet jede Kachel und jedes Namensschild in der Farbe ihres Nutzers,
die eigene sofort in der gerade gewählten; wer keine gewählt hat, leuchtet in
Blau (`DEFAULT_FRAME_COLOR`, `[46, 155, 255]`). Der Sprech-Ring um den Avatar im
Hauptfenster nimmt dieselbe Farbe, ohne Wahl den Akzent des Themes. Der Nebel
variiert die Helligkeit der Farbe ohne violette Beimischung; der CPU-Ersatz
verwendet sie für den Rahmen. Der Schalter „Sprech-Corona“ bleibt lokal: Er
bestimmt nur, ob dieses Overlay Coronas zeichnet, deshalb bleibt der Farbwähler
auch ausgeschaltet bedienbar. Der Pfeil daneben setzt auf Standard zurück
(= `None` auf dem Server). Abgleich beim `Welcome`: Hat der Server eine Farbe,
übernimmt der Client sie in `overlay.frame_color` (zweites Gerät, Neuinstallation);
hat er keine und ist die lokale nicht Blau, geht die lokale hoch (Übernahme der
bis 0.1.14 nur lokalen Wahl); eine offline gewählte Farbe wird hochgeschickt
statt überschrieben. Eine Änderung von einem anderen Gerät desselben Nutzers
übernimmt der Client ebenso, außer während eine eigene Wahl noch unterwegs ist.

Die erste Einrichtung führt durch Geräte, Sprechen, Klang und Übersicht.
Währenddessen sperrt sie die Audioübertragung vorübergehend, lässt Pegel, VAD,
PTT-Probe und Mikrofontest aber weiterlaufen. Diese Sperre ist unabhängig von
Stumm/Taub und kann durch deren Hotkeys nicht aufgehoben werden. Fertig,
Überspringen oder Fensterschließen speichern `setup_done` und lösen nur die
Einrichtungssperre; die eigene Stumm-/Taubwahl bleibt erhalten, auch wenn sie
während der Einrichtung geändert wurde.
Der Einrichtungspegel zeigt das verarbeitete Signal. Übersteuerung wird zusätzlich
am rohen Mikrofoneingang geprüft und eine Sekunde lang angezeigt, auch bei
stummgeschalteter Übertragung und wenn die Verarbeitung den Pegel schon senkt.
Wird ICU mit offener Einrichtung beendet, erscheint diese beim nächsten Start
wieder. Der Assistent ist jederzeit unter Audio erneut erreichbar.

Unter Allgemein werden Passports gespeichert, kopiert und importiert.
Einfügen und Drag & Drop benutzen dieselbe Prüfung und Vorschau; diese nennt
CA-Fingerabdruck und Änderungen am Sendeverhalten. Erst „Übernehmen“ ersetzt die
Identität (§5). Kopieren löscht den eigenen Zwischenablageinhalt nach 60 s,
sofern er noch dort liegt. Windows schließt Geheimnisse vom Verlauf und der
Cloud-Synchronisierung aus; unter Wayland ist das Löschen compositorabhängig.
Enter aktiviert im Beitrittsfenster nur das fokussierte Bedienelement; die
Option zur Einstellungsübernahme importiert dadurch keine Identität.
Exportdateien sind unter Unix 0600, unter Windows durch die Profil-ACL geschützt;
DPAPI schützt die lokale Identität, nicht den zum Weitergeben bestimmten Export.

Der Admin-Tab trennt Mitglieder, offene Einladungen, gesperrte und entfernte
Zugänge. Einladungen lassen sich kopieren oder als `.icu` speichern; ihr Inhalt
steht nicht im Fenster. Entfernen und Zurückziehen werden innerhalb der Zeile
bestätigt. Namen, Status und Aktionen kommen aus dem Serverzustand.

### Fenster, Tray und Fehler

`ui.close_action` wählt Infobereich (Vorgabe), Minimieren oder Beenden.
Shift+Schließen, Strg+Q, Beenden im Kopf und Tray → Beenden schließen ICU immer.
Wo Verstecken nicht verfügbar ist — etwa Wayland oder ohne StatusNotifier-Host —
wird minimiert. „Minimiert starten“ folgt derselben Regel.
Unter Windows wird ein verstecktes Tray-Fenster zusätzlich minimiert, damit es
nicht weiter rendert. Der Tray zeigt den gesamten aktuellen Zustand; ein fataler
Fehler aktualisiert auch seinen Tooltip.

Fenster ziehen unter Windows (2026-10-02, `ui/window_move.rs`): Windows verschiebt
ein Fenster in einer Modalschleife auf dem UI-Thread, es rückt also nur zwischen
zwei Frames weiter. egui zeichnet bei jedem `Moved` neu, dazu kamen ICUs
periodische Repaints (33 ms) — auf der Windows-11-VM folgte das Hauptfenster dem
Zeiger mit 12–16 Positionswechseln/s statt 125 (Notepad: 122/s), eine nackte
eframe-0.36-App genauso. Jetzt gehen reine Verschiebungen während der Schleife an
`DefWindowProc` vorbei an winit (egui liest die Position ohnehin je Frame vom
Fenster), und die periodischen Repaints (`ui::repaint_after`) pausieren, bis
`WM_EXITSIZEMOVE` einen Nachholframe anstößt. Ergebnis auf der VM: 120–123/s,
nur der schon eingeplante Frame am Anfang des Zugs bleibt. Während des Ziehens
stehen Pegel und Zeiten still; Größenänderungen zeichnen weiter bei jedem
Schritt. Messwerkzeug: `experiments/spike-window-drag`.

Diagnose auf echter Hardware (2026-10-12, nach einem Bericht „Ziehen ruckelt“):
Jeder Zug schreibt eine Logzeile `window drag` mit Dauer (`ms`), Positionswechseln
(`moves`) und Frames, die trotzdem darin gezeichnet wurden (`frames`); jeder davon
wartet auf VSync, während das Fenster steht; 0–1 ist normal (der schon eingeplante), `resize=true` markiert eine Größenänderung. Beim Start loggt jedes Fenster
`renderer` mit Adapter, Backend (DX12/Vulkan/GL), Gerätetyp (diskret, integriert,
CPU = WARP) und Present-Mode. `ICU_PRESENT_MODE=vsync|novsync|mailbox` wählt den
Present-Mode für einen A/B-Vergleich; Standard bleibt eframes `AutoVsync` mit
Frame-Latenz 1, denn ohne VSync zeichnen egui-Animationen (die Pegel laufen beim
Sprechen fast durchgehend) so schnell, wie die GPU kann.

Je Konfigurationsverzeichnis gibt es eine Fenster-Instanz: Unix nutzt flock und
Unix-Socket im privaten Laufzeitverzeichnis, Windows Mutex und eine Named Pipe
mit Benutzer-ACL. Weitere Starts zeigen das vorhandene Fenster und übergeben
eine Einladung/einen Passport. Abweichende Versionen verlangen, das laufende ICU
zuerst zu beenden. Die Übergabe hat Fristen, lehnt fremde Pipe-Eigentümer ab und
wird nach dem Senden nicht automatisch wiederholt. `--headless` überspringt den
Fenster-Instanzschutz.

Fehler werden über `Problem { text, technical }` getrennt dargestellt: verständliche
Banner mit Handlungsmöglichkeit, technische Einzelheiten im Log und in der
Verbindungsansicht. Ohne nutzbares Fenster zeigt Windows Startfehler als Dialog.
Die Verbindungsansicht ist nur lesend und kann einen Diagnosebericht kopieren.
Schalter und Schieber sind per Tastatur bedienbar und tragen zugängliche Labels;
verbleibende Desktop-Prüfungen stehen in der Live-Test-Checkliste.

### Nutzerpfad, Zertifikate und Vertrauen

Die Nutzeransicht (`ui/user.rs`) zeigt links den ausgewählten Nutzer, mittig den Server und rechts den lokalen Nutzer. Standard 900×640, Minimum 760×520; lange Inhalte umbrechen, Details dürfen scrollen. Phosphor-Icons, Sprachstatus, RTT je Serverstrecke und Audio-Empfangsverlust stehen direkt im Pfad. Lautstärke und berechtigte Moderation bleiben erhalten. Die CA-Verzweigung öffnet Zertifikatsdetails im selben Fenster; Felder und Fingerabdruck-Kopieren teilen sich den Renderer mit der Verbindungsansicht.

Die drei Karten verwenden dasselbe Raster: Überschrift, Avatar bzw. Server-Icon mit Name, Rolle, Status, Medien bzw. Adresse, Sitzungsdaten und Zertifikatsstatus. Jede Zeile übernimmt die Höhe ihrer längsten Zelle; Umbrüche verschieben alle drei Folgezellen gemeinsam. Kartenabstand und Innenrand betragen 12 px, Zeilenabstände 8 px. RTT-Anzeigen liegen unter den jeweiligen Strecken, Zertifikatsauswahl und CA-Verzweigung auf den Kartenachsen. Transporthinweis und Verbindungsdetails gehören zur Vertrauenskarte. Auch die Nutzeransicht ohne Moderation darf für die gemeinsamen Zeilen und Lautstärkeregelung wachsen; bei begrenzter Bildschirmhöhe bleibt sie scrollbar.

`RequestPeerCertificate` fragt einmal pro Sitzung an, mit sichtbarem Lade-/Fehlerzustand und manuellem Wiederholen nach acht Sekunden. `Peer.certificate` hält geprüfte öffentliche Daten, wird beim Verlassen/Neuverbinden verworfen und bei CRL-Updates neu geprüft. Parsing läuft in der Engine. Gültigkeits- und CRL-Zeitgrenzen werden bei der Anzeige ausgewertet. Bei Auswahl des eigenen Nutzers markieren beide Außenkarten dieselbe Verbindung. Signaturprüfung ersetzt keine direkte Peer-Authentifizierung; die Sitzungszuordnung stammt vom Server.

## 11. Konfiguration

`settings.toml` unter `$XDG_CONFIG_HOME/icu/` bzw. `%APPDATA%\icu\config\`, geschrieben entprellt (2026-09-26: 500 ms nach der letzten Änderung, über `spawn_blocking` statt auf einem der beiden Netz-Worker, beim Beenden sofort — ein Schieberegler löste vorher je UI-Frame ein `fsync` aus). Lässt sie sich nicht lesen, legt der Client sie als `settings.toml.broken-<Unix-Zeit>` beiseite, startet mit den Vorgaben und sagt es im ersten Banner (`EngineConfig.banner`), statt mit einer Zeile auf stderr zu enden, die beim Start vom Desktop niemand sieht (2026-09-26):

```toml
# Serverkoordinaten stehen ausschließlich in identity/server.toml (siehe §5), nicht hier.
[audio]       input_device, output_device, mode = "vad"|"ptt", vad_threshold, vad_auto = true, hangover_ms = 1000, attack_ms = 40, gain_db, denoise = true, voice_opt = true, voice_strength = "medium",
              aec = true, release_ms = 150, frame_ms = 20, cue_join = true, cue_tx = false, cue_mute = true,
              normalize_incoming = true, auto_level = true, normalize_outgoing = true,
              peer_assistance = true, tune_system_mic = true, remember_good_levels = true,
              muted = false   # eigene Stummschaltung, schreibt nur die Engine; gilt beim nächsten Start (2026-09-27)
[hotkeys]     ptt = {...}, mute = {...}, deafen = {...}, toggle_mode = {...}, camera = {...}, backend = "auto"|"portal"|"evdev"
[camera]      device, enabled = false
[overlay]     enabled = false, show_self = true, corner = "top-right", offset = [40, 40], size = 120,
              speaking_size   # nicht gesetzt: size + 20 % (2026-10-03)
[volumes]     "<user_id>" = 1.2                     # pro Peer, persistent; Schlüssel ist die stabile
                                                    # user_id aus UserInfo — Session-IDs gelten nur je
                                                    # Verbindung, Fingerprints sieht der Client nie
[ui]          name, start_minimized = false, register_uri_handler = true, theme = "system", close_action = "tray",
              show_advanced = false, notify_join = false, setup_done = false   # Einrichtung erledigt oder übersprungen (§10)
```

**Im Passport** (2026-09-26, `protocol.md` §2.3.1): ein exportierter Passport trägt diese Einstellungen mit, außer dem, was zum Rechner gehört — `audio.input_device`, `audio.output_device`, `hotkeys.backend`, `[camera]`, `ui.register_uri_handler`, `ui.setup_done` (ein neues Gerät durchläuft die Einrichtung für seine Geräte) und das alte `ui.name`; die Liste steht einmal, in `Settings::keep_machine_fields`. `Settings::portable` setzt sie für den Export auf den Standard, `Settings::take_portable` behält beim Import die eigenen und übernimmt den Rest, `[volumes]` als Ganzes. Zahlen aus einem Passport landen in den Bereichen des Einstellungsfensters (Lautstärken 0–200 %, Schwelle 0–1, Verstärkung ±20 dB, PTT-Nachlauf ≤ 1 s, VAD-Nachlauf ≤ 3 s, VAD-Anlauf ≤ 200 ms, Opus-Frame 10 oder 20 ms), was keine Zahl ist, auf dem Standard.

Die Engine speichert die eigene Stummschaltung in `audio.muted`; während Taub
bleibt die Stummschaltung von vor dem Taubschalten gespeichert. Taub selbst wird
nicht über einen Neustart übernommen. Ein Import von Einstellungen behält die
Stummschaltung der laufenden Engine; auch eine erzwungene Stummschaltung bleibt
für den nächsten Start erhalten.

## 12. Build und Packaging

[Bauanleitung](../development/building.md): Voraussetzungen, Linux- und
Windows-Build, Cross-Toolchain und Paketprüfung. Die Paket-Skripte sind die
maßgeblichen ausführbaren Rezepte. Release-Profile stehen in `Cargo.toml`.

Windows verwendet das GUI-Subsystem; Terminalstarts hängen sich an die
aufrufende Konsole. Ohne Terminal schreibt der Client unter Windows nach
`%LOCALAPPDATA%\icu\data\icu.log`, unter Linux nach
`$XDG_STATE_HOME/icu/icu.log` (Vorgabe `~/.local/state/icu/icu.log`, 0600).
Rotation bei 1 MiB, vorherige Datei `.1`; weitere Instanzen kürzen das Log nicht.
DLL-Suche ist auf Systemverzeichnisse begrenzt. Startfehler werden als Dialog
sichtbar, wenn keine Konsole vorhanden ist. Windows-N/KN braucht das Media
Feature Pack wegen der beim Start importierten MediaFoundation-DLLs.

## 13. Abhängigkeiten und Lizenzen

Versionen und Features: Workspace- und Crate-`Cargo.toml`, aufgelöster Stand:
`Cargo.lock`. `deny.toml` enthält die gemeinsame Lizenz- und Quellenpolitik;
`about.toml` und die Packaging-Notices ergänzen die Lizenztexte im Paket.
ICU ist Apache-2.0. Hintergründe: [Lizenzen](../research/packaging-licensing.md).

## 14. Tests

`cargo test --locked --workspace` prüft Protokoll, Engine, Identitäten,
Medienkaskade, Jitter, UI-Layout und Serverintegration. Die Clienttests mit
In-Process-Server sind Unix-spezifisch; Windows führt die kompatiblen Unit-,
Layout- und Konsolentests nativ aus. Wine ersetzt diese Prüfung nicht.

Der Tipp-Filter (`tests/typing.rs`) prüft mit synthetischen Klicks, dass das
Gate zu bleibt, und mit `testsignal::voice`, dass Stimme es öffnet und der
Wortanfang erhalten bleibt; wie echte Tastaturen mit RNNoise punkten, zeigt
nur der Live-Test.

Synthetische Sprach- und Echotests sind reproduzierbare Regressionstests;
physische Akustik, Geräte, Spiele und Desktop-Interaktion benötigen die
[Live-Test-Checkliste](../development/testing.md). Datierten Nachweisen im
[Verlauf](../history/README.md) kann keine Freigabe eines neueren Builds
entnommen werden.

## 15. Abnahmegrenzen

Offen bleiben die in der [Live-Test-Checkliste](../development/testing.md)
markierten Hardware-, Portal-, Netzwerk- und Installerprüfungen. Ein vorhandener
Build oder VM-Test schließt diese Prüfungen nicht automatisch ab.

## 16. Entscheidungsverlauf

Die früher hier gesammelten Soll/Ist-Vergleiche stehen im
[historischen Entscheidungsprotokoll](../history/client-implementation-decisions.md).
Die Abschnitte oben beschreiben das aktuelle Verhalten; neue Änderungen werden
im jeweiligen Fachabschnitt dokumentiert.
