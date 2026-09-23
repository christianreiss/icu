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

## Nicht-Ziele

- Channels, Rechteverwaltung, Rollen, Server-Browser
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

## Status

Phase: Implementierung. Research und Spezifikation abgeschlossen (2026-09-20); Server (`crates/icu-server`), Protokoll (`crates/icu-proto`) und Linux-Client (`crates/icu-client`) implementiert. Offene Review-Befunde: `Research/review-2026-09-23.md`.

Stack (belegt in `Research/`, verifiziert in `Research/verification.md`): Rust; QUIC (quinn + rustls/ring) mit Datagrams für Audio; mutual TLS mit server-eigener CA und Invite-Link-Enrollment; Opus 48 kHz mono 20 ms; Server als reiner Relay; Client mit PipeWire (Linux) bzw. WASAPI IAudioClient3 (Windows), RNNoise, adaptivem Jitter-Buffer, egui-UI, Portal/evdev bzw. Raw-Input-Hotkeys, Layer-Shell bzw. Layered-Window-Overlay, H.264 über openh264.

Offen bis zur Implementierung: echte Windows-Hardware (WASAPI, Raw Input, Overlay, SmartScreen), interaktiver Portal-Hotkey-Test auf KDE, Kamera-Bitrate bei 15 fps.
