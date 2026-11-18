# Tiefenprüfung 2026-11-18: Sprache, Video, Synchronität, PKI

Auftrag: den gesamten Code (Linux-Client, Windows-Client, Server) gegen bewährte
Praxis prüfen, jede Funktion auf ihre Absicht, und sicherstellen, dass Sprache
und Video so gut übertragen werden, wie es geht, und die Optimierer tatsächlich
greifen. Gefundene Fehler wurden behoben. Stand vorher: 0.1.30, Commit `881f79e`.

Vorgehen: Ausgangsmessung (46 Testsuiten, 1070 bestanden, Clippy sauber), acht
unabhängige Prüfdurchgänge (PKI/Zertifikate, Engine/Netz, UI, Protokoll und
Server-Medienpfad, DSP, Updater/Konfiguration/Paketierung, Audio-E/A, Video),
dazu das Log eines echten Laufs auf `biest`. Jeder Befund wurde vor einer
Änderung am Code nachvollzogen; jede Korrektur hat einen Test, wo einer möglich
war. Das Protokoll blieb unverändert (Minor 8): alte und neue Clients und Server
arbeiten zusammen.

## Behoben

### Sprache

- **Echtzeit-Priorität wirkte nicht.** Auf Fedora 43 lief kein Audio-Thread je in
  `SCHED_RR`: PipeWires module-rt schickt ohne rlimits nur eine unbeantwortete
  Anfrage, und das Desktop-Portal lehnt sie ab („Could not get pidns“). ICU
  meldete trotzdem Erfolg. Jetzt wird geprüft, was der Scheduler sagt, und ein
  Hilfsthread fragt RTKit direkt — für `icu-dsp`, `icu-mixer` und PipeWires
  Callback-Thread. (`2c09827`, `315915c`, `fb97423`)
- **Sprecher nach langem Taubschalten nicht mehr zu hören.** Wer länger als
  ~11 Minuten nichts von einem Sprecher empfing (taub, Netz weg), bei dem lief
  dessen 16-Bit-`seq` unbemerkt über; seine Pakete galten danach als „zu spät“,
  bis zum nächsten Sprechbeginn nach einer Pause. Der Puffer erkennt das am
  Zeitstempel und beginnt neu. (`e7f483a`)
- **Wortanfang fiel dem Jitter-Puffer zum Opfer.** Ein VAD-Sender beginnt jeden
  Sprechabschnitt mit vier Paketen auf einmal (Pre-Roll). Auf guter Leitung ist
  das Überschuss, den der Puffer eine halbe Sekunde später „an einem leisen
  Frame“ abbaute — geprüft wurde aber der zuletzt gespielte Frame, entfernt das
  ungehörte Folgepaket: nach einer kurzen Pause die ersten 20 ms des nächsten
  Worts. Jetzt dekodiert der Mixer das Paket und lässt es nur aus, wenn dieser
  Frame selbst leise ist. (`4fc3ef7`)
- **Wortanfang 18 dB zu leise.** Die Dämpfung anhaltender Nicht-Sprache im
  VAD-Nachlauf lief auch bei geschlossenem Gate weiter. Zu hören war davon nur
  das Pre-Roll vor dem nächsten Wort — genau der Anlaut, den die Erkennung
  niedrig bewertet. Gezählt wird jetzt nur bei offenem Gate. (`530ecbb`)
- **Sprechbeginn ohne Vorlauf nach einem Nachzügler.** Ein verspätetes Paket
  zwischen zwei Sprechabschnitten startete die Vorlauf-Uhr des Jitter-Puffers.
  Ging danach das erste Paket des nächsten Abschnitts verloren, begann dieser
  ohne Vorlauf. (`83b374b`)
- **Verlustmeldung nach Epochenwechsel.** Die Zählbasis der Verlustberichte
  blieb stehen, wenn der Jitter-Puffer eines Sprechers neu angelegt wurde: ein
  falscher Bericht, im Debug-Build ein Unterlauf. (`066c3b5`)

### Video

- **Bild stand bis zu 2 s, wenn sich zwei Frames überholten.** Jeder Frame
  reist in einem eigenen QUIC-Stream; kam einer vor seinem Vorgänger an, galt
  das als Lücke und der Decoder wartete auf das nächste IDR. Eine
  Sortierstufe hält einen zu frühen Frame jetzt bis zu 60 ms. (`eea1f76`,
  `983abf2`)
- **Server bat nie selbst um ein Keyframe.** Verwarf der Server für einen
  Zuschauer einen Frame, ließ er bis zum nächsten Keyframe nichts mehr durch —
  und der Zuschauer konnte das nicht melden, weil er eine lückenlose Folge sah,
  die einfach aufhörte. Der Server fordert das IDR jetzt selbst an, einmal je
  Aussetzer. Ein vom Empfänger abgebrochener Stream zählt als Verlust.
  (`c7ec7bb`)
- **Verworfene Frames ohne Nummer; IDR-Wiederholung knapp zu früh.** Ein beim
  Sender wegen Zeitüberschreitung verworfener Frame verbrauchte keine `seq`:
  die Empfänger dekodierten das nächste Bild gegen eine Referenz, die sie nie
  hatten. Und die Wiederholung einer IDR-Anfrage kam exakt im Sekundentakt und
  damit beim Server so oft zu früh wie pünktlich. (`983abf2`)
- **Beschädigtes MJPEG-Bild beendete den Client.** libjpeg bricht bei einem
  kaputten Kopf den Prozess ab. Die Köpfe werden jetzt vorher geprüft. Nebenbei:
  Das Stoppen der Kamera gibt das Gerät jetzt ausdrücklich frei, bevor es
  zurückkehrt, statt zufällig über eine 200-ms-Pause. (`fcf4b75`, `0a248a9`)

### Synchronität

- **Bild lief dem Ton voraus**, mit Bluetooth-Headset um mehrere Zehntelsekunden
  (im Lauf vom 2026-11-07: 421 ms, `release-0.1.28-2026-11-07.md`). Seit dem
  Notfallfix 0.1.28 wurde jedes Bild sofort gezeigt. Jetzt wartet ein Bild begrenzt auf seinen Ton
  (`video::Hold`): Median der gemeldeten Abstände, höchstens 8 ms Änderung je
  Bild, höchstens 600 ms, und nie wird ein Bild deswegen verworfen. Beschreibung
  in `docs/reference/client.md`, Abnahme in `docs/development/testing.md`.
  Eigener, einzeln rücknehmbarer Commit: `50a99ed`.

### Server und PKI

- **`icu-server setup` verwarf beim zweiten Lauf die Konfiguration** (alles,
  wonach der Assistent nicht fragt) und setzte Lauschadressen zurück. (`8d3a8f0`)
- **Einladung ging verloren, wenn `--out` nicht schreibbar war**: die Einladung
  war ausgestellt, der einzige Abdruck des Einmalschlüssels weg. Sie wird jetzt
  ausgegeben. (`38f9035`)
- **`monitor.sock` war für die Überwachungsgruppe nicht erreichbar**; die
  manuelle Installation legte die Gruppe nicht an. (`254c09c`, `5c04854`)

### Oberfläche, Update, Release

- **Moderationsmenü wechselte den Nutzer.** Das Kontextmenü hing an der
  Zeilenposition; kam oder ging jemand, während es offen war, stand es über der
  nächsten Person. (`8e5c6db`)
- **Einstellungsdatei, die kein Text ist, verhinderte den Start.** Eine
  `settings.toml` mit ungültigem UTF-8 galt als E/A-Fehler statt als unlesbare
  Datei und wurde nicht beiseitegelegt. (`d26de92`)
- **Windows-Update brach nach 20 s Download ab**: das Zeitlimit galt für den
  ganzen Download statt je Lesevorgang. (`d6eefb3`)
- **Secret-Scan des Release-Skripts schlug nie an**: `grep -q` unter `pipefail`
  ließ einen Treffer als „sauber“ durchgehen. (`7449a29`)
- Dokumentation an den Code angeglichen (Protokoll-Minor, Headergrößen,
  Paketname auf der Download-Seite und anderes). (`7449a29`)

## Geprüft und in Ordnung

PKI und Einladungen (Schlüsselerzeugung, Einmalschlüssel, Zertifikatsketten,
CRL, Erneuerung, Widerruf), mTLS und Sitzungsaufbau, Ratenbegrenzung und
Budgets des Servers, Opus-Encoder/-Decoder samt FEC und PLC, AEC-Ausrichtung,
Rauschunterdrückung, Limiter und Resampler, Privacy-Sperren: keine Fehler
gefunden. Die Audio-Callbacks warten auf keine Sperre und allozieren nicht.

## Offen

Gefunden, nachvollzogen, aber nicht behoben — mit Begründung:

- **AEC-Referenz setzt aus** („AEC reference dropped out, reinitializing“,
  im echten Lauf in Schüben). Ursache nicht belegt. Die fehlende
  Echtzeit-Priorität ist ein Kandidat; ob es damit verschwindet, zeigt erst ein
  Lauf mit dem neuen Client. Die Schwelle wurde bewusst nicht verstellt.
- **Neustart des PipeWire-Dienstes** lässt ICU bis zum Neustart ohne Ton
  (`audio/pipewire.rs`: kein Listener für Core-Fehler, kein Wiederverbinden).
  Größerer Umbau, selten.
- **Lippensynchronität unter Windows als Zuschauer**: WASAPI liefert hier nur
  eine geschätzte Wiedergabezeit ohne Geräteverzögerung; dort bleibt es beim
  sofortigen Anzeigen.
- Kleinere Punkte: ein dauerhaft abgelehntes Audioformat öffnet alle Streams alle
  ≤ 30 s neu; `DefaultDeviceChanged` öffnet auch bei fest gewählten Geräten neu;
  evdev-Hotkeys binden nicht nach, wenn beim Start kein Gerät die Taste hat;
  Tray-Häkchen können veralten; unter Windows bleibt ein kopierter Passport nach
  dem Beenden in der Zwischenablage; alte Update-Ordner werden nicht
  aufgeräumt; die Netz-Stufe (`netcond`) vergleicht gegen die kleinste je
  gemessene RTT und kann dadurch auf einer niedrigen Videostufe hängen bleiben;
  der NSIS-Rückweg des Windows-Updaters ließ sich hier nicht prüfen.

## Prüfungen

Stand nach der Prüfung (`50a99ed`): `cargo test --locked --workspace` 46 Suiten,
1091 bestanden, 0 fehlgeschlagen, 18 ignoriert; `cargo clippy … -D warnings`
sauber für Linux und für `x86_64-pc-windows-gnu`; `cargo fmt --check` sauber;
die 30 Paketierungstests bestanden. Einzelnachweise in den Commit-Nachrichten.
Nicht ausgeführt: `./build.sh`
(Pakete, Installations-Smokes), Veröffentlichung, Versionssprung, jeder Lauf
unter Windows, und die Abnahme an echter Hardware — insbesondere der
Klatschtest zur Lippensynchronität und ein Gespräch mit dem neuen Client.
