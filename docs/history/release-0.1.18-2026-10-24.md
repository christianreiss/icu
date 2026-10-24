# Release 0.1.18 — 2026-10-24

Diese Version integriert die [Nachprüfung von Audio, Sicherheit und UI](deep-review-follow-up-2026-10-24.md)
auf dem Privacy-Stand 0.1.17. Das gemeinsame Protokoll bleibt unverändert.

Zusätzlich erkennt der Website-Publisher den aktuellen Fedora-Paketnamen
`icu-client` und weiterhin alte `icu`-Artefakte. Die Windows-Releasevorlage
beschreibt die vorhandene systemweite Installation mit Administratorrechten.
Die Versionsänderung betrifft ausschließlich die drei Workspace-Pakete;
Abhängigkeitspins bleiben erhalten.

Der Quellcode-Review bestand 996 Workspace-Tests, Clippy, Windows-/Headless-Checks
und cargo-deny. Der anschließende Paketbau läuft aus diesem sauberen Release-Commit
mit `./build.sh --plain`. Seine vollständigen Protokolle, Prüfsummen und BUILDINFO
liegen unter `target/build-logs/` und `dist/0.1.18/`. Auslieferungsnachweise werden
unter `target/release-0.1.18/` auf biest.eulie.de gesichert. Diese Notiz selbst
behauptet noch keinen erfolgreichen Paketbau oder Rollout.

Ziele des freigegebenen Rollouts: icu-server auf lab.uggs.io, icu-client auf
biest.eulie.de und bestehende Client-Downloads auf misc.alpha-labs.net/icu
(twingo.alpha-labs.net). Alte Pakete und Downloadversionen bleiben für Rollback
erhalten; es gibt keine Datenmigration.
