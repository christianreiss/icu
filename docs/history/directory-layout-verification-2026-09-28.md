# Verzeichnisumzug: Prüfung vom 2026-09-28

Geprüft auf `biest.eulie.de`, Ausgangscommit
`37b639ed12244c3279270cedc699fb854b4971bf`, mit den uncommitteten
Verzeichnis- und Dokumentationsänderungen. Die [Umzugstabelle](../README.md#umzug-vom-2026-09-28)
nennt die neuen Pfade. Produktverhalten und Wire-Format wurden nicht geändert;
Änderungen in Rust-Dateien betreffen ausschließlich Dokumentationsverweise.

## Ergebnis

| Prüfung | Ergebnis |
| --- | --- |
| Workspace-Tests, `cargo test --locked --workspace` | 832 bestanden, 0 fehlgeschlagen, 9 ignoriert; einschließlich Doc-Tests |
| `cargo fmt --all -- --check` | Bestanden |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Bestanden |
| Cargo-Metadaten | Produkt-Workspace mit drei Mitgliedern; alle sechs Experimente eigenständig; Aufrufe in ihren Verzeichnissen schreiben nach `target/experiments/<name>/` |
| Shell-Syntax | Alle Packaging-Skripte und der umgezogene Meson-Wrapper gültig |
| Dokumentverweise | Lokale Markdown-Links, Anker und Dokumentpfade aufgelöst |
| Windows-ZIP mit AEC | Gebaut; System-DLL-Allowlist, Ressourcen und Lizenzen geprüft |
| Windows-Build ohne `target/windows-static/` | Verzeichnis und Bibliothek automatisch neu angelegt; Paket erfolgreich |
| Windows-Installer | NSIS-Bau und PE-Prüfung bestanden |
| Linux-Pakete | Statischer musl-Server, Clients für Fedora 42/43 und Debian 13 gebaut |
| Paket-Smokes | Installation, erneute Installation und Entfernung auf Fedora 42/43, Debian 12/13 und AlmaLinux 10 bestanden |
| Build-Isolation | Quellbaum im laufenden Container nur lesend; `/src/.local` als leeres tmpfs geprüft |
| SHA256SUMS | Sowohl Prüfbuilds als auch wiederhergestellte ursprüngliche Pakete vollständig gültig |

Die Cargo-Exclusions nennen jedes Experiment explizit. Der vorher verwendete
Wildcard-Eintrag isolierte fünf der sechs Projekte nicht zuverlässig:
`cargo metadata` endete dort zunächst mit Code 101 („believes it's in a workspace“).
Nach der Korrektur bestehen alle sechs Prüfungen.

ShellCheck endet für die beiden bestehenden Build-Skripte weiterhin mit Code 1:
drei SC2016-Hinweise in `packaging/build.sh` und eine SC2115-Warnung in
`packaging/windows/build.sh`. Ein Vergleich mit den Skripten aus dem
Ausgangscommit bestätigt identische Befunde; der Meson-Wrapper ist ohne Befund.
Es wurden keine Warnungen im Quellcode unterdrückt.

## Aufbewahrung

Vor dem Umzug wurden Inhalte und Dateimodi inventarisiert. Die 175 Einträge der
früheren `.scratch/` und 80 Einträge des früheren `target/dev-server/` blieben
erhalten. Danach wurden nur die absoluten Pfade in zwei lokalen
Serverkonfigurationen angepasst; deren Originale liegen unter
`.local/reorganization-backup/local-configs/`. Ein zusätzlicher Gate-Nachweis
wurde unverändert von `target/gate/` nach `.local/gate/` verschoben.

Keine VM-Definition verwies auf die verschobenen Dateien. Der laufende
Desktop-Client verwendet `/usr/bin/icu` mit seinem regulären Benutzerprofil;
er wurde nicht neu gestartet oder umkonfiguriert.

Die ursprünglichen zwölf `dist/`-Inventareinträge (Verzeichnis und Dateien)
sind mit identischen Inhalten und Modi wiederhergestellt. Die mit
`ALLOW_DIRTY=1` gebauten Prüfpakete liegen separat unter
`target/reorganization-validation/dist/`; sie sind nicht zur Veröffentlichung
bestimmt. Prüfprotokolle liegen unter
`.local/verification/directory-layout-2026-09-28/`.

Kein Commit, Push, Release oder Deployment wurde ausgeführt. Hardware-, WAN-
und Desktop-Abnahmen wurden durch diesen Strukturumbau nicht erneut geprüft;
die offenen [Live-Tests](../development/testing.md) bleiben bestehen.
