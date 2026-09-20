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
| `Research/verification.md` | Ergebnisse der empirischen Spikes |
| `Server/` | Server-Spezifikation, später Implementierung |
| `Client/` | Client-Spezifikation, später Implementierung |

## Status

Phase: Research und Spezifikation. Keine Implementierung.
