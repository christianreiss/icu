# Packaging, Code-Signing und Lizenzen

## Frage

In welcher Form werden Windows-Client, Linux-Client und Linux-Server für v1 ausgeliefert, was passiert ohne Code-Signing unter Windows und was kostet Signing, gibt es Auto-Update, welche Lizenzen bringen die vermuteten Dependencies mit (inklusive H.264-Patentlage) und unter welcher Lizenz soll ICU selbst stehen?

## Kandidaten

Windows-Client: portable .exe · MSI (cargo-wix / WiX / msitools) · Inno Setup. Signing: keins · Azure Artifact Signing (bis 2026 „Trusted Signing") · OV-Zertifikat einer CA. Auto-Update: ja/nein.

Linux-Client: statisches musl-Binary · glibc-dynamisches Binary als Tarball · AppImage · Flatpak · Distro-Paket (RPM/DEB).

Server: statisches musl-Binary + systemd-Unit · zusätzlich Docker-Image (`FROM scratch`).

ICU-Lizenz: MIT · Apache-2.0 · AGPL-3.0.

## Primärquellen

Alle abgerufen am 2026-09-20.

Windows:

- Microsoft Defender SmartScreen Overview: https://learn.microsoft.com/en-us/windows/security/operating-system-security/virus-and-threat-protection/microsoft-defender-smartscreen/
- SmartScreen reputation for Windows app developers: https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation
- Artifact Signing (ehem. Trusted Signing) Overview: https://learn.microsoft.com/en-us/azure/trusted-signing/overview (leitet auf `/azure/artifact-signing/overview` um)
- Artifact Signing Quickstart (Voraussetzungen, Identity Validation): https://learn.microsoft.com/en-us/azure/trusted-signing/quickstart
- Artifact Signing FAQ: https://learn.microsoft.com/en-us/azure/artifact-signing/faq
- Artifact Signing, Signing-Integrationen: https://learn.microsoft.com/en-us/azure/artifact-signing/how-to-signing-integrations
- Artifact Signing Pricing: https://azure.microsoft.com/en-us/pricing/details/trusted-signing/ und https://azure.microsoft.com/en-us/pricing/details/artifact-signing/
- Azure Artifact Signing GitHub Action: https://github.com/Azure/trusted-signing-action
- cargo-wix: https://github.com/volks73/cargo-wix ; crates.io https://crates.io/api/v1/crates/cargo-wix
- Inno Setup: https://jrsoftware.org/isinfo.php ; Lizenztext https://jrsoftware.org/files/is/license.txt
- jsign (Authenticode-Signing plattformunabhängig): https://ebourg.github.io/jsign/
- osslsigncode: https://github.com/mtrojnar/osslsigncode

Linux:

- Flatpak Sandbox Permissions: https://docs.flatpak.org/en/latest/sandbox-permissions.html
- xdg-desktop-portal GlobalShortcuts: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html
- xdg-desktop-portal Camera: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Camera.html
- ashpd global_shortcuts: https://docs.rs/ashpd/latest/ashpd/desktop/global_shortcuts/index.html
- AppImage Concepts: https://docs.appimage.org/introduction/concepts.html ; FUSE: https://docs.appimage.org/user-guide/troubleshooting/fuse.html ; Best Practices: https://docs.appimage.org/reference/best-practices.html
- winit Wayland (dlopen): https://docs.rs/winit/latest/winit/platform/wayland/index.html
- tray-icon (Linux-Backends): https://github.com/tauri-apps/tray-icon
- cpal (ALSA-Abhängigkeit): https://github.com/RustAudio/cpal
- Mumble Global Shortcuts (KDE-Wayland-Maustasten, evdev): https://www.mumble.info/documentation/user/global-shortcuts/

Server:

- systemd.exec(5), Spiegel der offiziellen Manpage auf man7.org (freedesktop.org lieferte HTTP 403): https://man7.org/linux/man-pages/man5/systemd.exec.5.html
- Docker Base Images (`scratch`): https://docs.docker.com/build/building/base-images/

Lizenzen:

- crates.io-API `https://crates.io/api/v1/crates/<name>` für alle Crates der Matrix (Liste unter „Verified against")
- Opus-Lizenz und Patentgrants: https://opus-codec.org/license/
- RNNoise: https://github.com/xiph/rnnoise ; nnnoiseless: https://github.com/Rikorose/nnnoiseless
- OpenH264: https://www.openh264.org/ ; BINARY_LICENSE: https://www.openh264.org/BINARY_LICENSE.txt ; FAQ: https://www.openh264.org/faq.html ; Repo: https://github.com/cisco/openh264 ; openh264-rs: https://github.com/ralfbiedert/openh264-rs
- Via LA AVC/H.264: https://www.via-la.com/licensing-2/avc-h-264/
- libvpx (BSD + PATENTS): https://github.com/webmproject/libvpx
- Apache-2.0 Text: https://www.apache.org/licenses/LICENSE-2.0
- MIT Text (OSI): https://opensource.org/license/mit
- AGPL-3.0 Text: https://www.gnu.org/licenses/agpl-3.0.html
- Sekundärquelle: choosealicense.com (GitHub) Vergleichstabelle: https://choosealicense.com/licenses/

## Befunde

### SmartScreen ohne Signing

- SmartScreen prüft „downloaded programs and the digital signature used to sign a file. If a URL, a file, an app, or a certificate has an established reputation, users don't see any warnings. If there's no reputation, the item is marked as a higher risk and presents a warning to the user." (SmartScreen Overview)
- Developer-Seite, Tabelle „First-download SmartScreen behavior": Microsoft Store: keine Warnung; „Valid Certificate (OV/EV): Warning — app flagged as unrecognized until reputation accumulates; verified publisher name is displayed"; „No signature: Warning — 'Windows protected your PC'; User must choose 'Run anyway' before the app can run. Enterprise policy can prevent continuation entirely."; „Self-signed Certificate: Same behavior as no signature".
- „EV certificates no longer bypass SmartScreen. […] Paying a premium for EV solely to avoid SmartScreen warnings is no longer justified."
- „When a file is not signed, SmartScreen reputation must build for each new version of your files, starting with zero reputation." Reputation-Aufbau: „no exact threshold, but it can take several weeks and hundreds of clean installs from a wide audience." Keine manuelle Einreichung für Consumer-Endpunkte („no need (or mechanism)").
- Windows 11 Smart App Control: „will block execution of unsigned files unless the file has a positive reputation" — für alle ausführbaren Dateien, nicht nur Downloads.
- Konsequenz für ICU (feste Gruppe, wenige Installationen): eine unsignierte .exe zeigt bei jedem Nutzer und jeder neuen Version „Windows protected your PC"; Reputation wird bei dieser Nutzerzahl nie erreicht. Nutzer mit aktivem Smart App Control können die .exe ohne Signatur gar nicht starten.

### Azure Artifact Signing (ehem. Trusted Signing)

- Umbenannt: alle `trusted-signing`-URLs leiten auf `artifact-signing` um; Developer-Seite: „Artifact Signing (formerly Trusted Signing) is Microsoft's recommended code signing service for non-Store distribution".
- Kosten: „Starts at $9.99/month" (SmartScreen-Developer-Seite). Die Preisseite selbst zeigt nur Platzhalter („$-"), Quoten: Basic 5.000 Signaturen/Monat, 1 Zertifikatsprofil je Typ; Premium 100.000, 10 je Typ. Rechnung „isn't calculated on a pro rata basis" (FAQ). „Artifact Signing doesn't support free, trial, or sponsored Azure subscriptions" (FAQ).
- **Identity Validation, Public Trust**: „Public Trust certificates are available to organizations in the United States, Canada, the European Union, the United Kingdom, Australia, New Zealand, Japan, South Korea, Singapore, Switzerland, Norway, and Israel. **Individual developers must be located in the United States or Canada.**" (Quickstart). Für Organisationen: Legal-Entity-Daten, Website, E-Mail auf eigener Domain, Business Identifier, Adresse; Bearbeitung „1 to 20 business days"; Nachweise nicht älter als 12 Monate. Individuelle Validierung läuft über AU10TIX-Ausweisprüfung + Microsoft Verified ID.
- Zertifikate: „Artifact Signing certificates have a three-day validity, so time stamping is critical" (Integrations-Seite), Timestamp-Server `http://timestamp.acs.microsoft.com`. „Artifact Signing doesn't issue Extended Validation (EV) certificates." Der private Schlüssel wird nie herausgegeben (HSM, FIPS 140-3 Level 3).
- SmartScreen mit Artifact Signing: „SmartScreen reputation builds up automatically. The prompt stops appearing once the file hash has sufficient download history." (FAQ) — also **keine** Sofort-Reputation, aber Reputation überträgt sich auf neue Versionen mit gleichem Zertifikat („Reputation cannot transfer from previous versions unless both were signed using the same publisher identity").
- Signieren von Linux aus: SignTool + dlib ist Windows-only (Windows 10 1809+/Server 2016+, .NET 8, VC-Redist); die GitHub Action ist „Windows only" (windows-2022/2025-Runner). jsign (Apache-2.0, Java, „Platform independent command line tool (Linux, macOS and Windows)") unterstützt Storetype `TRUSTEDSIGNING` mit Endpoint als `keystore` und `account/profile` als `alias`, signiert .exe und .msi. osslsigncode (GPL-3.0, Tool) signiert PE/MSI/CAB mit eigenem Schlüssel/PKCS#11 und Timestamp — nur für Nicht-Azure-Zertifikate relevant.
- Konsequenz: Chris als Privatperson in Deutschland ist für Public Trust **nicht** zugelassen; nur eine eingetragene Organisation in der EU (eigene Domain, Business Identifier) kann das Konto anlegen. OV-Zertifikate klassischer CAs: Kosten und Hardware-Token-Pflicht (CA/B-Forum-Anforderung seit 2023) in diesem Dokument unbelegt.

### Windows-Auslieferungsformat

- **Portable .exe**: keine Toolchain über den Cross-Build hinaus; von Fedora aus vollständig erzeugbar (siehe cross-compile.md). Settings liegen dann unter `%APPDATA%` (Konvention, unbelegt).
- **MSI via cargo-wix 0.3.9** (MIT/Apache-2.0, letztes Release 2025-03-13): braucht WiX Toolset v3.14.1 oder v4+; „the WiX Toolset is Windows only; thus, this project is only useful when installed on a Windows machine". Ob WiX v4/v5 (`dotnet tool install wix`) selbst unter Linux MSIs baut: in der FireGiant-Doku nicht gefunden (unbelegt). Fedora hat `msitools 0.106.31` (wixl); dessen Grenzen: README nicht abrufbar (unbelegt).
- **Inno Setup**: Lizenz erlaubt Nutzung „for any purpose, including commercial applications" ohne Kauf; Copyright-Hinweise in Binär-Redistributionen erhalten. Läuft auf Windows („every Windows release since 2006"); Betrieb des Compilers unter Wine ist nicht dokumentiert (unbelegt). Unterstützt „digitally signed installs and uninstalls".
- **NSIS** (2026-09-28 nachgetragen, im ursprünglichen Vergleich fehlend): Compiler `makensis` läuft — anders als Inno Setups Compiler und WiX — **nativ unter Linux**; Fedora-Paket `mingw64-nsis` (zieht `mingw-nsis-base`, das eigentliche `makensis`-Binärpaket), Version 3.11-2.fc43. Praxis auf diesem Host: `makensis` initialisiert vor jedem Aufruf (auch `-VERSION`/`-HELP`) einen Default-Stub für x86-unicode; ohne das zusätzliche Paket `mingw32-nsis` (liefert nur diesen Stub, kein x86-Build-Ziel wird genutzt) bricht jeder Aufruf mit „error setting default stub" ab — beide Pakete sind also nötig, obwohl nur `Target amd64-unicode` gebaut wird. Lizenz laut `dnf info mingw-nsis-base`: „Zlib AND CPL-1.0" — die LZMA-Kompression steht unter Common Public License 1.0, zlib/bzip2 je unter eigener permissiver Lizenz; mit `SetCompressor zlib` bleibt der kompilierte Installer-Stub nachweislich frei von LZMA/CPL-Strings (mit `strings` gegen die erzeugte `setup.exe` geprüft, 2026-09-28). `CreateShortcut` in diesem Paket unterstützt nur die Vor-3.08-Parameterliste (Name, Ziel, Parameter, Icon, Icon-Index, Fenstermodus, Hotkey, Kommentar) — kein `workingdir`/`appUserModelId`-Parameter wie in neueren offiziellen NSIS-Builds dokumentiert (Test: `CreateShortCut expects 2-9 parameters, got 10`).

### Auto-Update

Keine Vendor-Quelle nötig; Ableitung aus den SmartScreen-/Signing-Befunden: Ein Auto-Updater, der unsignierte Binaries nachlädt und ausführt, ersetzt die Nutzerentscheidung „Run anyway" durch eine automatische — ohne Signatur gibt es keinen Integritätsnachweis außer TLS zum Download-Host. Bei einer festen Gruppe reicht: Server meldet die minimale Protokollversion, der Client zeigt „neue Version nötig" mit Link; die Verteilung läuft über denselben Kanal wie der Invite-Link.

### Linux-Client

- **Dynamische Laufzeitabhängigkeiten sind unvermeidbar**: winit lädt Wayland-Libs per `dlopen` („By default, Winit loads system libraries using dlopen"); cpal linkt `alsa` (alsa-lib, „alsa-lib-devel on Fedora"); GL/Vulkan-Loader kommen aus dem System; tray-icon Default-Backend braucht GTK3 + libxdo + libappindicator, KSNI-Backend nur D-Bus. Ein vollstatisches musl-Client-Binary ist damit nicht sinnvoll: dlopen aus einem statischen Binary ist nicht vorgesehen (allgemeines glibc/musl-Verhalten, hier unbelegt), und LGPL-Systembibliotheken (alsa-lib, GTK; Lizenzen hier unbelegt) statisch einzubauen würde LGPL-Pflichten auslösen.
- **AppImage**: „every AppImage contains exactly one app with all its dependencies", kein Sandboxing, Build auf der ältesten unterstützten Distro („build on oldest, run on newer"), glibc wird nicht gebündelt. Braucht FUSE 2 (`libfuse2`, auf Ubuntu 24.04 `libfuse2t64`); Fallback `--appimage-extract-and-run`. Keine absoluten Pfade einkompilieren.
- **Flatpak**: Sandbox default „No access to any device nodes (apart from /dev/null, etc)", „No access to host services like X11, system D-Bus, or PulseAudio". Freigaben: `--socket=pulseaudio` (Mikrofon + Wiedergabe), `--socket=wayland`, `--device=input` („Input devices as exposed in /dev/input") für evdev, Kamera per Camera-Portal (`AccessCamera` → `OpenPipeWireRemote`, liefert PipeWire-fd) oder `--device=all`. GlobalShortcuts-Portal v2: `CreateSession`, `BindShortcuts` (Konfigurationsdialog des Desktops), `Activated`/`Deactivated`-Signale; Trigger-Format verweist auf die „shortcuts XDG specification", Maustasten dort nicht dokumentiert (unbelegt). Mumble-Doku für KDE Plasma Wayland: „If you want to use a mouse button as a global shortcut, you must enable Listening for mouse buttons" (seit KDE 6.1). Das Overlay (Wayland layer-shell) braucht in der Sandbox nur `--socket=wayland`; der Flatpak-Nachteil liegt bei evdev (`--device=input` plus Gruppenrechte), nicht beim Overlay. Ob das Portal auch von nicht-sandboxed Apps nutzbar ist (ashpd ohne Flatpak): Doku schweigt, unbelegt — nach allgemeinem Portal-Design ja, in Step 3 prüfen.
- **Distro-Paket**: kein Vendor-Befund nötig; ein RPM für Fedora deckt nur Chris' eigene Distro ab, die Gruppe nutzt unbekannte Distros (Annahme).

### Server

- systemd.exec(5): `DynamicUser=` („A UNIX user and group pair is allocated dynamically when the unit is started"), `ProtectSystem=strict` („entire file system hierarchy is mounted read-only" außer /dev, /proc, /sys), `ProtectHome=`, `NoNewPrivileges=`, `CapabilityBoundingSet=`, `AmbientCapabilities=` (für Port < 1024: `CAP_NET_BIND_SERVICE`), `StateDirectory=` („created below /var/lib/"). `RestrictAddressFamilies=`, `SystemCallFilter=`, `MemoryDenyWriteExecute=`, `LockPersonality=` sind auf der Seite vorhanden, Fetch lieferte keine Zitate; Namen sind Standard-Direktiven derselben Manpage.
- Docker: „The reserved, minimal scratch image serves as a starting point for building containers"; funktioniert „as long as the […] binary doesn't have any runtime dependencies" — exakt der Fall beim musl-Binary (CA-Zertifikate braucht der Server nicht, er ist eigene CA).
- **musl im statischen Server** (2026-09-27, externes Review O-2): Das Binary enthält musl libc, und zwar die `libc.a` des Rust-Targets `x86_64-unknown-linux-musl` (self-contained; der Link-Trace `-Wl,-t` im Build-Image `rust:1.98.1-alpine` nennt sie, nicht Alpines `musl-dev`, das dort 1.2.6 ist): musl **1.2.5** laut deren `version.lo`. musl steht unter MIT, der Hinweis gehört in jede Kopie. `packaging/build.sh` liest die Version beim Server-Build aus `version.lo`, hängt `packaging/notices/musl-<version>-COPYRIGHT` (Text vom musl-Tag `v1.2.5`) an `THIRD-PARTY-LICENSES` an, trägt sie in `BUILDINFO.linux` ein und bricht ab, wenn der Text zu dieser Version fehlt. Aus demselben Verzeichnis kommt LLVMs `libunwind.a` (Apache-2.0 WITH LLVM-exception: die Ausnahme erlässt für eingebetteten Objektcode §4(a), (b) und (d), also keine Hinweispflicht). Der Linux-Client linkt glibc, libstdc++ und abseil dynamisch aus der Distribution.

### Lizenz-Matrix

Crate-Lizenz = crates.io-Feld der `max_stable_version` am 2026-09-20. „Native Lib" = Lizenz der gebündelten/gelinkten C-Bibliothek.

| Crate | Version | Crate-Lizenz | Native Lib / Bemerkung | Copyleft-Risiko | Attribution |
| --- | --- | --- | --- | --- | --- |
| quinn | 0.11.12 | MIT OR Apache-2.0 | keine | keins | Notice |
| rustls | 0.23.45 | Apache-2.0 OR ISC OR MIT | keine | keins | Notice |
| aws-lc-rs | 1.18.1 | ISC AND (Apache-2.0 OR ISC) | AWS-LC (C, gebündelt): ISC plus OpenSSL/BoringSSL-abgeleitete Teile — Lizenzdatei nicht gelesen, unbelegt | keins (permissiv) | Notice, inkl. OpenSSL-Attribution (unbelegt, prüfen) |
| ring | 0.17.14 | Apache-2.0 AND ISC | enthält BoringSSL/OpenSSL-abgeleiteten Code (README) | keins | Notice, OpenSSL-Attribution |
| opus | 0.4.0 | MIT/Apache-2.0 | — | keins | Notice |
| opusic-sys | 0.7.5 | BSD-3-Clause | libopus 1.6.1: BSD 3-Clause; Patente: Xiph/Broadcom/Microsoft „royalty-free, irrevocable" mit defensiver Terminierung bei Patentklage gegen Opus-Implementierungen | keins | BSD-Notice in Binärdistribution (Klausel 2) |
| audiopus_sys | 0.2.2 | ISC | libopus 1.3 (veraltet) | keins | verworfen |
| cpal | 0.18.2 | Apache-2.0 | Linux: alsa-lib dynamisch (LGPL-2.1, unbelegt); Windows: keine | LGPL nur bei statischem Linken; dynamisch: keine Pflicht über Hinweis hinaus | Notice |
| egui / eframe | 0.36.2 | MIT OR Apache-2.0 | keine | keins | Notice |
| iced | 0.14.0 | MIT | keine | keins | Notice |
| wgpu | 30.0.1 | MIT OR Apache-2.0 | keine (Loader zur Laufzeit) | keins | Notice |
| winit | 0.30.13 | Apache-2.0 (nicht dual) | Wayland/X11-Libs per dlopen | keins | Notice + NOTICE-Datei falls vorhanden |
| nokhwa | 0.10.11 | Apache-2.0 (crates.io; README nennt „Apache-2.0 and MIT") | Windows: `windows` 0.43; Linux: v4l; Default-Feature `decoding` = mozjpeg (Lizenz unbelegt) | keins | Notice |
| openh264 / openh264-sys2 | 0.9.8 | BSD-2-Clause | OpenH264 (Cisco) BSD-2-Clause, gebündelt und aus Quelle gebaut → **H.264-Patente, siehe unten** | keins (Lizenz), Patentfrage offen | BSD-Notice |
| env-libvpx-sys | 5.1.3 (2023) | **MPL-2.0** | libvpx: BSD-3-Clause + Patent Grant (PATENTS) | MPL = datei-bezogenes Copyleft: Änderungen an MPL-Dateien müssen offengelegt werden; Linken ist frei | Notice; nur relevant, falls libvpx gewählt |
| nnnoiseless | 0.5.2 | BSD-3-Clause | Port von RNNoise (BSD-3-Clause); Modelle „trained using only the publicly available datasets" | keins | BSD-Notice |
| webrtc-audio-processing (+ -sys, -config) | 2.1.0 | crates.io meldet „non-standard" (`license-file = COPYING`); der Text ist **wörtlich die BSD-3-Clause** („Copyright (c) 2011, Google Inc.", drei Klauseln inkl. Namensklausel) | Gebündelte C++-Quellen: PulseAudios Repackaging des WebRTC AudioProcessing Module, ebenfalls BSD-3-Clause (Google); im `bundled`-Build werden sie **mit ins Binary gelinkt**. Zusätzlich abseil-cpp (Apache-2.0) — entweder aus dem System oder als meson-Subproject | keins (permissiv) | **BSD-Notice ist Pflicht**: „Redistributions in binary form must reproduce the above copyright notice … in the documentation and/or other materials provided with the distribution." Der `COPYING`-Text aus dem Crate **und** der abseil-Apache-2.0-Hinweis gehören in `THIRD-PARTY-LICENSES`. Patente: keine gesonderte Patentklausel im BSD-3-Text; ob WebRTC-Codecpatente hier greifen, ist **unbelegt** und für ein reines DSP-Modul ohne Codec unwahrscheinlich | **Neu 2026-09-20 (AEC)** |
| rcgen | 0.14.10 | MIT OR Apache-2.0 | keine | keins | Notice |
| tokio | 1.53.1 | MIT | keine | keins | Notice |
| postcard | 1.1.3 | MIT OR Apache-2.0 | keine | keins | Notice |
| tray-icon | 0.25.1 | MIT OR Apache-2.0 | Linux-Default: GTK3/libappindicator dynamisch (LGPL, unbelegt); KSNI: keine | keins bei KSNI | Notice |
| ashpd | 0.13.13 | MIT | keine | keins | Notice |
| evdev | 0.13.2 | Apache-2.0 OR MIT | keine | keins | Notice |
| windows (windows-rs) | 0.62.2 | MIT OR Apache-2.0 | Import-Libs | keins | Notice |
| rubato | 5.0.0 | MIT OR Apache-2.0 | keine | keins | Notice |

Werkzeuge (nicht gelinkt, keine Auswirkung auf ICU-Lizenz): cargo-xwin MIT, xwin Apache-2.0 OR MIT, cargo-zigbuild MIT, cargo-wix MIT/Apache-2.0, jsign Apache-2.0, osslsigncode GPL-3.0, Inno Setup eigene Lizenz (kostenfrei), Windows SDK/CRT via xwin proprietär (EULA-Text unbelegt, siehe cross-compile.md). **Ausnahme**: NSIS 3.11 (Fedora `mingw-nsis-base`/`mingw64-nsis`/`mingw32-nsis`) `Zlib AND CPL-1.0` ist zwar ebenfalls ein Werkzeug (nicht in `icu.exe` gelinkt), aber der von `makensis` erzeugte Installer-Stub selbst wird mit `icu-<ver>-windows-x86_64-setup.exe` an Endnutzer ausgeliefert und braucht daher — anders als die übrigen hier gelisteten Werkzeuge — eine eigene Notice: `packaging/windows/notices/nsis-COPYING` (zlib/libpng-Text; `SetCompressor zlib` hält den Stub nachweislich frei vom CPL-1.0-Anteil, siehe „Windows-Auslieferungsformat"). Die mingw-w64-Runtime ist dagegen statisch ins `icu.exe` gelinkt; gelesen 2026-09-27: CRT ZPL-2.1 plus Public Domain (`DISCLAIMER.PD`, Fedora `mingw64-crt` 13.0.0), winpthreads MIT plus BSD-3-Clause (Lockless Inc., mingw-w64 v13.0.0), libgcc/libstdc++ GPL-3.0-or-later mit GCC Runtime Library Exception 3.1 (Fedora `mingw64-libstdc++` 15.2.1); die Texte liegen dem Windows-ZIP bei (Punkt 11 unten).

GPL: kein einziges Crate der Matrix. LGPL: nur dynamisch gelinkte Linux-Systembibliotheken (alsa-lib, ggf. GTK) — Lizenzen nicht per Quelle geprüft; dynamisches Linken und kein Bündeln vermeidet Pflichten. MPL-2.0: nur env-libvpx-sys.

Attribution: MIT („The above copyright notice and this permission notice shall be included in all copies or substantial portions"), BSD-2/3 (Klausel 2: Binärdistribution muss Hinweis „in the documentation and/or other materials" reproduzieren), ISC (analog), Apache-2.0 §4 (Lizenzkopie, Änderungshinweise, NOTICE-Datei-Inhalte weitergeben). Praktisch: eine generierte `THIRD-PARTY-LICENSES`-Datei im Release-Archiv und im „Über"-Dialog; Generatoren wie cargo-about/cargo-deny wurden nicht geprüft (unbelegt).

### H.264-Patentlage

- OpenH264-Quelle: „BSD license terms". Cisco: „We will not pass on our MPEG-LA licensing costs for this module, and based on the current licensing environment, this will effectively make H.264 free for use on supported platforms."
- BINARY_LICENSE: Cisco-Deckung gilt „provided that the requirements and conditions […] are met", darunter „The Cisco-provided binary is separately downloaded to an end user's device, and not integrated into or combined with third party software prior to being downloaded to the end user's device"; erlaubte Nutzung „personal use of a consumer or other uses in which it does not receive remuneration".
- FAQ: „If I use the source code in my product, and then distribute that product on my own, will Cisco cover the MPEG LA licensing fees which I'd otherwise have to pay? **No.**" Produkte müssen das Binary „at the time the product or project is installed on the user's computer or device" von Cisco herunterladen.
- Via LA (ehem. MPEG LA) AVC-Programm: für Codec-Produkte „For the first 1 to 100,000 units" pro Jahr „$ 0.00", danach $0.20 bzw. $0.10 pro Einheit, Enterprise-Cap „$9.75M per year in 2017 and after". Ob unterhalb der 100.000 Einheiten ein unterschriebener Lizenzvertrag nötig ist, um die $0-Stufe in Anspruch zu nehmen: unbelegt. Patentlaufzeiten der AVC-Pool-Patente: nicht geprüft, unbelegt.
- Einordnung für ICU: openh264 aus Quelle gebaut und statisch eingebettet = keine Cisco-Deckung; ICU ist nicht-kommerziell mit weit unter 100.000 Installationen. Alternative mit Cisco-Deckung: das Cisco-Binary beim ersten Kamera-Start vom Cisco-GitHub-Release nachladen (Firefox-Modell); ob die openh264-Crate ein Runtime-Loading-Feature bietet: unbelegt (Step 3). Alternative ohne Patentfrage: VP8/VP9 (libvpx, BSD + Patent Grant) — build-technisch verworfen (cross-compile.md).

### ICU-Lizenz, Konsequenzen

Aus den Lizenztexten und der choosealicense-Tabelle (sekundär):

- **MIT**: Bedingungen nur „License and copyright notice"; kein Patentgrant; jeder darf geschlossene Forks bauen.
- **Apache-2.0**: zusätzlich §3 Patentgrant mit Terminierung bei Patentklage, §4 Änderungshinweise + NOTICE-Datei, Markenschutz-Limitation. Kompatibel mit allen Dependencies der Matrix.
- **AGPL-3.0**: Copyleft; §13: wer das Programm modifiziert und „users interacting with it remotely through a computer network" anbietet, muss den Corresponding Source dieser Version über Netz anbieten. Bei ICU träfe §13 nur Server-Betreiber, die den Server ändern; für eine private Gruppe ohne Fremdnutzer belanglos. Kombination mit permissiven Dependencies ist zulässig (Apache-2.0 → AGPL ist einseitig kompatibel); alle Crates bleiben unter ihrer Lizenz. Kostet Beitragende/Forker Freiheit und schreckt Abhängigkeiten in Firmenumgebungen ab.

## Entscheidung

- **Windows v1: portable .exe, unsigniert**, Release als ZIP mit `SHA256SUMS` und Anleitung zum „Run anyway"-Dialog. Begründung: einziger Pfad, der ohne Windows-Rechner vollständig von Fedora aus produzierbar ist; Signing ist für Chris als Privatperson in Deutschland bei Artifact Signing nicht verfügbar (Individuals nur US/Kanada), und Signing beseitigt die Warnung ohnehin erst nach Reputationsaufbau. Dokumentierte Einschränkung: Nutzer mit Smart App Control können v1 nicht starten. Sobald eine Organisation (eigene Domain, Business Identifier) verfügbar ist: Artifact Signing Basic (ab $9.99/Monat), signiert von Fedora aus mit jsign, Timestamp `http://timestamp.acs.microsoft.com`.
- **Installer: NSIS** (2026-09-28, ersetzt die vorherige Empfehlung „Inno Setup, sobald ein Windows-Rechner da ist"). Gewählt anstelle von Inno Setup und WiX/MSI, weil `makensis` — anders als deren Compiler — nativ unter Linux läuft: passt zum eigentlichen Kriterium dieses Dokuments (vollständig von Fedora aus baubar), ohne dass ein Windows-Rechner oder Wine für den *Build* nötig wäre. Die inzwischen verfügbare Windows-VM/Hardware (`docs/history/windows-gate-2026-09-26.md`) wird nur noch zum *Testen* des Installers gebraucht. Für alle Benutzer mit UAC (`RequestExecutionLevel admin`, Installation unter `%ProgramFiles%\ICU`), ausgeliefert **zusätzlich** zum portablen ZIP, nicht als Ersatz — Umsetzung: `packaging/windows/installer.nsi` + `packaging/windows/installer.sh`.
- **Auto-Update: nein in v1**. Server meldet Mindest-Protokollversion, Client zeigt Hinweis + Link. Auto-Update ohne Signatur wäre ein unbeaufsichtigter Ausführungspfad; mit fester Gruppe ist manuelles Update zumutbar.
- **Linux-Client v1: ein glibc-dynamisches Binary als Tarball**, gebaut auf einer alten Baseline (AppImage-Prinzip „build on oldest"); tray-icon mit KSNI-Backend (keine GTK-Abhängigkeit); ALSA über cpal als einzige gelinkte C-Lib; Wayland/GL per dlopen. AppImage als optionale Verpackung desselben Binaries, wenn FUSE-Pflicht akzeptabel ist. Flatpak und Distro-Pakete nicht in v1.
- **Server: statisches musl-Binary** + systemd-Unit mit `DynamicUser=yes`, `StateDirectory=icu`, `ProtectSystem=strict`, `ProtectHome=yes`, `NoNewPrivileges=yes`, `CapabilityBoundingSet=` leer (Port ≥ 1024) bzw. `AmbientCapabilities=CAP_NET_BIND_SERVICE`, `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`, `SystemCallFilter=@system-service`, `MemoryDenyWriteExecute=yes`, `LockPersonality=yes`. Optional ein `FROM scratch`-Image mit demselben Binary; Admin-CLI ist dasselbe Binary (Subcommands), damit kein zweites Artefakt entsteht.
- **Lizenz-Hygiene**: `THIRD-PARTY-LICENSES` im Release (alle Notices, OpenSSL/BoringSSL-Hinweise aus aws-lc bzw. ring, Opus-Patent-Hinweis), `deny.toml`/cargo-deny-Allowlist: MIT, Apache-2.0, ISC, BSD-2/3, MPL-2.0 (nur falls libvpx), Zlib; alles andere bricht den Build.
- **Revidiert 2026-09-20 (AEC)**: `webrtc-audio-processing` ~2.1 kommt dazu. Zwei Konsequenzen für das Packaging. Erstens gibt crates.io die Lizenz als „non-standard" aus, weil das Crate `license-file` statt `license` setzt — **cargo-deny wird darüber stolpern** und braucht einen expliziten `[licenses.clarify]`-Eintrag, der das Crate als BSD-3-Clause führt (der `COPYING`-Text belegt das). Zweitens zieht der `bundled`-Build fremden C++-Code ins Binary, dessen BSD-3-Notice cargo-about **nicht** automatisch findet: der `COPYING`-Text aus dem Crate und der abseil-Apache-2.0-Hinweis müssen von Hand in `THIRD-PARTY-LICENSES` aufgenommen werden. Beides ist vor M5 zu verankern.
- **Empfehlung ICU-Lizenz (Chris entscheidet): Apache-2.0.** Patentgrant von Beitragenden, NOTICE-Mechanismus für die Attribution, keine Copyleft-Reibung. MIT wäre gleichwertig einfach ohne Patentklausel; AGPL nur, wenn geschlossene Forks verhindert werden sollen — für ein Werkzeug einer festen Gruppe ohne Fremdbetreiber bringt §13 nichts und kostet Reichweite.
- **H.264**: Arbeitsannahme bleibt openh264 aus Quelle; die Patentfrage wird nicht durch Lizenz, sondern durch die nicht-kommerzielle Nutzung unterhalb der $0-Stufe entschärft — mit dem offenen Punkt, ob dafür ein Vertrag nötig ist. Codec-Entscheidung finalisiert Step 3 (Build-Erfolg) und ein eigenes Codec-Research-Dokument (Cisco-Runtime-Download vs. VP8).

## Verworfen, mit Grund

- **MSI via cargo-wix**: WiX Toolset Windows-only; msitools-Grenzen unbelegt; Mehrwert eines MSI (Gruppenrichtlinien-Deployment) für eine private Gruppe null.
- **Inno Setup**: bleibt verworfen, auch nachdem ein Installer gebaut wurde — Compiler weiterhin Windows-only, Wine-Betrieb weiterhin unbelegt, während NSIS' `makensis` nativ unter Linux läuft. Das ist der entscheidende Unterschied, der NSIS statt Inno Setup zum Zug kommen ließ (siehe „Entscheidung").
- **Code-Signing in v1**: Artifact Signing Public Trust für Privatpersonen nur US/Kanada; OV-Zertifikat einer CA nicht recherchiert (Kosten/Token unbelegt); Reputation wird bei der Nutzerzahl ohnehin nicht erreicht.
- **Auto-Update**: siehe Entscheidung; unsignierte Auto-Updates sind ein Sicherheitsrückschritt.
- **Statisches musl-Client-Binary**: winit/Wayland dlopen, ALSA, GL/Vulkan-Loader, ggf. LGPL-Statik.
- **Flatpak in v1**: Sandbox zwingt zu `--device=input` für evdev-PTT und Portal-Umbau der Kamera; der Portal-Vorteil (GlobalShortcuts, Camera) ist auch ohne Flatpak nutzbar, falls Portale unsandboxed funktionieren (Step 3). Zweiter Auslieferungspfad ohne Nutzen für v1.
- **Distro-Pakete**: Gruppe nutzt unbekannte Distros; Paketpflege für mehrere Distros ist Aufwand ohne v1-Nutzen.
- **AGPL für ICU**: §13 greift nur bei Betreibern modifizierter Server mit Fremdnutzern — nicht das Szenario.
- **libvpx** (lizenzseitig unproblematisch): build-technisch verworfen, MPL-Binding von 2023.

## Offene Punkte für Step 3

1. SmartScreen-Verhalten der unsignierten .exe auf einem echten Windows-11-Rechner dokumentieren (Screenshots, Smart-App-Control-Status).
2. Prüfen, ob eine Organisation für Artifact Signing verfügbar ist (Domain, Business Identifier); falls ja, jsign-Signing von Fedora aus gegen ein Public-Trust-Test-Profil durchspielen.
3. Preisseite von Artifact Signing zeigt Platzhalter; den tatsächlichen Basic-Preis im Azure-Portal/Preisrechner bestätigen (nur $9.99 „starts at" belegt).
4. Ob xdg-desktop-portal GlobalShortcuts/Camera von einer nicht-sandboxed ashpd-App auf KDE Plasma nutzbar sind, und ob Maustasten über das Portal bindbar sind (Mumble-Hinweis „Listening for mouse buttons", KDE 6.1+).
5. Laufzeitabhängigkeiten des Linux-Client-Binaries per `ldd` und `strace -e openat` (dlopen) auflisten; Baseline-Distro für den Build festlegen.
6. openh264-Crate: gibt es ein Feature zum Laden des Cisco-Binarys zur Laufzeit? Wenn ja: Größe, Versionierung, Download-URL aus RELEASES.
7. Via LA: Klären, ob die $0-Stufe (≤ 100.000 Einheiten) einen Lizenzvertrag voraussetzt; Patentablaufdaten des AVC-Pools prüfen.
8. Lizenzdateien lesen, die hier unbelegt sind: AWS-LC (`LICENSE`), ~~mingw-w64-Runtime~~ (gelesen 2026-09-27, Punkt 11), alsa-lib, mozjpeg, GTK/libappindicator (falls doch GTK-Backend).
9. Nicht erreichbar am 2026-09-20: `https://learn.microsoft.com/en-us/azure/trusted-signing/concept-trusted-signing-cert-management` (404), `https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html` (403, man7-Spiegel genutzt), `https://docs.flatpak.org/en/latest/portal-api-reference.html` (nur Navigation), `https://gitlab.gnome.org/GNOME/msitools` und dessen README (kein Inhalt), WiX-Doku ohne OS-Aussage, `https://docs.rs/rustls/latest/rustls/server/struct.ServerConfig.html` (keine Client-Auth-Passage; WebPkiClientVerifier-Seite stattdessen).
10. THIRD-PARTY-LICENSES-Generator auswählen (cargo-about vs. cargo-deny) und cargo-deny-Allowlist im Build verankern.
11. **Neu 2026-09-20 (AEC)**: `[licenses.clarify]` für `webrtc-audio-processing` (+ `-sys`, `-config`) in `deny.toml` eintragen und prüfen, dass `THIRD-PARTY-LICENSES` den BSD-3-Text der gebündelten WebRTC-Quellen sowie den abseil-Apache-2.0-Hinweis enthält — beides findet kein Generator von allein (`aec.md` A11). **Erledigt 2026-09-27** (externes Review O-2): die Klarstellungen greifen erst seit ihren echten Hashes (`hash = 0` ignorierte cargo-deny still); das Windows-ZIP (`packaging/windows/build.sh`) hängt an `THIRD-PARTY-LICENSES.txt` die Texte für alles statisch Gelinkte an, das kein Crate mitbringt: abseil-cpp (Apache-2.0, aus dem meson-Subproject des AEC-Builds), mingw-w64-CRT (ZPL-2.1 + Public Domain) und winpthreads (MIT + BSD-3-Clause, `packaging/windows/notices/`), libgcc/libstdc++ (GPL-3.0-or-later mit GCC Runtime Library Exception 3.1). Die Linux-Pakete linken abseil und libstdc++ dynamisch aus der Distribution.

## Verified against

[x] Step 3 am 2026-09-20 (Details in `../history/verification.md`):

- Spike 1 **bestätigt** die Aussage zu Windows-DLL-Abhängigkeiten: nach dem libstdc++-Fix nur System-DLLs; die `.exe` ist als portable Datei ohne Installer lauffähig (Wine-Smoke-Test).
- Spike 1 **ergänzt**: nokhwa importiert `mf.dll`, `mfplat.dll`, `mfreadwrite.dll` statisch; auf Windows-N/KN ohne Media Feature Pack startet das Binary dann nicht. Punkt für `docs/reference/client.md`: Kamera-Backend als separates, zur Laufzeit geladenes Modul oder dokumentierte Voraussetzung.
- Codec-Entscheidung ist in `camera-video.md` (revidiert) gefallen: openh264 aus Quelle; Patent-Einordnung dieses Dokuments gilt unverändert (0 $-Stufe, Hinweis in Doku, Punkt 7 offen).
- Offen: 1–5, 7–10.

Mit WebFetch gelesen (Inhalt erhalten), 2026-09-20:

- https://learn.microsoft.com/en-us/windows/security/operating-system-security/virus-and-threat-protection/microsoft-defender-smartscreen/
- https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation
- https://learn.microsoft.com/en-us/azure/trusted-signing/overview
- https://learn.microsoft.com/en-us/azure/trusted-signing/quickstart
- https://learn.microsoft.com/en-us/azure/artifact-signing/faq
- https://learn.microsoft.com/en-us/azure/artifact-signing/how-to-signing-integrations
- https://azure.microsoft.com/en-us/pricing/details/trusted-signing/
- https://azure.microsoft.com/en-us/pricing/details/artifact-signing/
- https://github.com/Azure/trusted-signing-action
- https://github.com/volks73/cargo-wix
- https://jrsoftware.org/isinfo.php
- https://jrsoftware.org/files/is/license.txt
- https://ebourg.github.io/jsign/
- https://github.com/mtrojnar/osslsigncode
- https://docs.flatpak.org/en/latest/sandbox-permissions.html
- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html
- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Camera.html
- https://docs.rs/ashpd/latest/ashpd/desktop/global_shortcuts/index.html
- https://docs.appimage.org/introduction/concepts.html
- https://docs.appimage.org/user-guide/troubleshooting/fuse.html
- https://docs.appimage.org/reference/best-practices.html
- https://docs.rs/winit/latest/winit/platform/wayland/index.html
- https://docs.rs/wayland-sys/latest/wayland_sys/
- https://github.com/tauri-apps/tray-icon
- https://github.com/RustAudio/cpal
- https://www.mumble.info/documentation/user/global-shortcuts/
- https://man7.org/linux/man-pages/man5/systemd.exec.5.html
- https://docs.docker.com/build/building/base-images/
- https://opus-codec.org/license/
- https://github.com/xiph/rnnoise
- https://github.com/Rikorose/nnnoiseless
- https://www.openh264.org/
- https://www.openh264.org/BINARY_LICENSE.txt
- https://www.openh264.org/faq.html
- https://github.com/cisco/openh264
- https://raw.githubusercontent.com/cisco/openh264/master/README.md
- https://github.com/ralfbiedert/openh264-rs
- https://www.via-la.com/licensing-2/avc-h-264/
- https://github.com/webmproject/libvpx
- https://www.apache.org/licenses/LICENSE-2.0
- https://opensource.org/license/mit
- https://www.gnu.org/licenses/agpl-3.0.html
- https://choosealicense.com/licenses/ (Sekundärquelle)
- https://github.com/l1npengtul/nokhwa
- https://raw.githubusercontent.com/l1npengtul/nokhwa/senpai/nokhwa-bindings-windows/Cargo.toml
- https://raw.githubusercontent.com/l1npengtul/nokhwa/senpai/nokhwa-bindings-linux/Cargo.toml
- crates.io-API (`https://crates.io/api/v1/crates/<name>`) für: quinn, rustls, aws-lc-rs, ring, opus, opusic-sys, audiopus_sys, cpal, egui, eframe, iced, wgpu, winit, nokhwa, openh264, openh264-sys2, env-libvpx-sys, nnnoiseless, rcgen, tokio, postcard, tray-icon, ashpd, evdev, windows, rubato, cargo-xwin, cargo-zigbuild, cargo-wix
