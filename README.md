# ICU

Minimales Voice-System für eine feste Gruppe. Ein Server, ein Raum, keine Channels.

## Ziel

- Linux Dedicated Server, ein statisches Binary
- Native Clients für Windows und Linux
- Authentifizierung über mutual TLS (Client-Zertifikate, vom Server ausgestellt)
- Push-to-Talk und Voice-Activation
- Optionales Kamera-Overlay über dem Spiel
- Niedrige Latenz, sehr gute Sprachqualität (Opus)
- Eigenes Protokoll
- Nur die Features, die wirklich gebraucht werden. Die sind dafür perfekt.

## Installation

Pakete unter https://github.com/christianreiss/icu/releases: der Client `icu`
für Fedora 42/43 (`.rpm`) und Debian 13 (`.deb`), der Server `icu-server` für
Fedora, AlmaLinux 10+ (`.rpm`) und Debian 12+ (`.deb`) (Einrichtung:
`deploy/README.md` §0).

## Nicht-Ziele

- Channels, Rechteverwaltung, Rollen außer Admin, Server-Browser
- Text-Chat, Dateitransfer, Bots, Plugins
- Overlay in exklusivem Fullscreen (DirectX/Vulkan-Hooking)
- Mobile Clients, Web-Client

## Verzeichnisse

| Pfad | Inhalt |
| --- | --- |
| `Research/` | Recherche je Thema, Entscheidungen mit Primärquellen |
| `Research/protocol/` | Entwurf des Wire-Protokolls |
| `Research/verification.md` | Claim-Check und Ergebnisse der empirischen Spikes |
| `crates/` | Cargo-Workspace: `icu-proto`, `icu-server`, `icu-client` |
| `PROTOCOL.md` | Normatives Wire-Protokoll v1 |
| `Server/IMPLEMENTATION.md` | Server-Spezifikation, später Implementierung |
| `Client/IMPLEMENTATION.md` | Client-Spezifikation, später Implementierung |
| `packaging/` | rpm/deb-Bau (nfpm, podman), Windows-ZIP (`packaging/windows/`) und GitHub-Release |

## Status

Phase: Pre-Release-Gate 2026-09-26 (`Research/review-2026-09-26.md`). Research und Spezifikation abgeschlossen (2026-09-20); Server (`crates/icu-server`), Protokoll (`crates/icu-proto`) und Linux-Client (`crates/icu-client`) implementiert. Der Server ist fürs offene Internet gehärtet (`Research/review-2026-09-24.md`, `deploy/README.md` „Im offenen Internet“), der Linux-Client ist Release-Kandidat. Der Windows-Client ist implementiert und baut als portables ZIP (`packaging/windows/build.sh`, 2026-09-26), ist auf Windows 10 Pro N (VM `10.2.0.201`) geprüft; der aktuelle Windows-Gate-Bericht steht in `Research/windows-gate-2026-09-26.md` (`Client/LIVETEST.md` §7), Veröffentlichung ausstehend. Offene Review-Befunde: `Research/review-2026-09-23.md`, `Research/review-2026-09-24.md`; Readiness-Review aller drei Teile (Sicherheit, Bedienung, Sprache, Video, Parität) mit Reihenfolge der Fixes: `Research/review-2026-09-27.md`.

Stack (belegt in `Research/`, verifiziert in `Research/verification.md`): Rust; QUIC (quinn + rustls/ring) mit Datagrams für Audio; mutual TLS mit server-eigener CA und Registrierung per Einladungs-Bundle (`.icu`-Datei); Opus 48 kHz mono 20 ms; Server als reiner Relay; Client mit PipeWire (Linux) bzw. WASAPI IAudioClient3 (Windows), WebRTC AEC3, RNNoise, adaptivem Jitter-Buffer, egui-UI, Portal/evdev bzw. Raw-Input-Hotkeys, Layer-Shell bzw. Layered-Window-Overlay, H.264 über openh264.

Offen: Windows auf physischer Zielhardware (akustische AEC/Drift, Gaming-Maus/Spiel-Overlay, SmartScreen), interaktiver Portal-Hotkey-Test auf KDE, Kamera-Bitrate bei 15 fps; was der Live-Test noch nicht abdeckt, steht in `Client/LIVETEST.md` §6.

## Lizenz

Apache-2.0, siehe `LICENSE`. Die Pakete enthalten `THIRD-PARTY-LICENSES` für gebündelten Fremdcode. Der Client baut OpenH264 aus dem Quellcode ein; die Patentlizenz, die Cisco für seine eigenen Binaries übernimmt, gilt dafür nicht (`Research/packaging-licensing.md`, H.264-Patentlage).
