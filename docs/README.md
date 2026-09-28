# Dokumentation

## Einstieg

- [Bauen und Verzeichnisse](development/building.md): Voraussetzungen, Pakete und lokale Dateien.
- [Testen](development/testing.md): Live-Tests und offene Hardwareprüfungen.
- [Server betreiben](../deploy/README.md): Installation, Konfiguration, Betrieb und Wiederherstellung.

## Aktuelle Referenz

- [Wire-Protokoll](reference/protocol.md): normative Protokollfassung.
- [Client](reference/client.md): Architektur, Audio, Video und Desktop-Verhalten.
- [Server](reference/server.md): Konfiguration, CLI und Verbindungsabläufe.
- [Features und Grenzen](reference/features.md): vereinbarter Funktionsumfang.

## Hintergrund

[research/](research/) enthält technische Untersuchungen und ihre Quellen;
Aussagen und Messergebnisse gelten jeweils für den dort genannten Stand.
[history/](history/README.md) hält datierte Reviews, Gate-Ergebnisse und frühere
Entscheidungen fest. Die experimentellen Programme liegen in
[experiments/](../experiments/README.md), außerhalb des Produkt-Workspace.

## Verzeichnisregeln

| Verzeichnis | Inhalt | Aufbewahrung |
| --- | --- | --- |
| `crates/` | Produktcode und Tests | Git |
| `docs/` | Referenz, Anleitungen, Forschung und historische Nachweise | Git |
| `experiments/` | Eigenständige Forschungsprogramme | Git, keine Produkt-Abhängigkeit |
| `packaging/` | Build-Rezepte, Toolchain-Hilfen, Paketdateien und Lizenztexte | Git |
| `deploy/` | Deployment-Dateien und Betriebshandbuch | Git |
| `target/` | Compiler-Ausgabe, Paket-Staging, kopierte Build-Bibliotheken | Ignoriert, regenerierbar |
| `dist/<version>/` | Pakete, BUILDINFO und Prüfsummen | Ignoriert, bewusst aufbewahren |
| `.local/` | Lokale Testumgebungen, Identitäten, VM-Material und Belege | Ignoriert, gezielt verwalten |

Pfade in Befehlen gelten ab Repository-Root, sofern nichts anderes angegeben ist.
Markdown-Links sind relativ zum jeweiligen Dokument. Quellcode-Kommentare nennen
Repository-Pfade. Neue Änderungen gehören direkt in den zuständigen Fachabschnitt;
abgeschlossene Arbeitspläne gehören nicht in die aktuelle Referenz.

## Umzug vom 2026-09-28

| Vorher | Jetzt |
| --- | --- |
| `PROTOCOL.md` | `docs/reference/protocol.md` |
| `Client/IMPLEMENTATION.md`, `Server/IMPLEMENTATION.md` | `docs/reference/client.md`, `docs/reference/server.md` |
| `Client/LIVETEST.md` | `docs/development/testing.md` |
| `Research/` | `docs/research/`, datierte Nachweise in `docs/history/` |
| `Research/feature-freeze.md`, `Research/protocol/wire.md` | `docs/reference/features.md`, `docs/history/wire-draft.md` |
| `Research/spikes/` | `experiments/` |
| `build/win-aec/` | `packaging/windows/toolchain/` |
| `build/win-static/libstdc++.a` | `target/windows-static/libstdc++.a` |
| `.scratch/` | `.local/` |
| `target/dev-server/` | `.local/dev-server/` |
| `target/gate/` | `.local/gate/` |

Historische Befehlsprotokolle behalten ihre damaligen Pfade. Bei erneutem Ausführen
gelten die neue Bauanleitung und diese Zuordnung. Die Inhalte lokaler Testumgebungen
werden beim Umzug erhalten; gespeicherte Konfigurationen erhalten die neuen Pfade.
