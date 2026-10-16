# Bauen

Alle Befehle laufen im Repository-Root. Rust-Version und Features stehen in
`Cargo.toml`, die aufgelösten Abhängigkeiten in `Cargo.lock`. Die gepflegten
Paketrezepte liegen unter `packaging/`; Experimente sind separate Cargo-Projekte.

## Alles mit einem Befehl

```sh
./build.sh
./build.sh --bump      # Gemeinsame Patch-Version erhöhen, dann alles bauen
./build.sh --clean     # Alle kompilierten Ziele entfernen, dann frisch bauen
./build.sh --clean-only  # Build-Speicherplatz freigeben, ohne neu zu bauen
./build.sh --clean --bump  # Beides kombinieren
./build.sh --plain     # Vollständige Logs direkt im Terminal
```

`--bump` erhöht nach erfolgreichem Vorabcheck die gemeinsame Version in
`Cargo.toml`, beispielsweise `0.1.0` → `0.1.1`; Client, Server und die interne
Protokoll-Crate erben diese Version. Ein vorhandener Vorabversions-/Build-Suffix
wird beim Erhöhen entfernt. Cargo gleicht `Cargo.lock` offline ab, ohne andere
Abhängigkeitsversionen zu ändern. Scheitert dieser Abgleich, werden beide Dateien
zurückgesetzt. Scheitert erst der anschließende Bau, bleibt die neue Version
erhalten: dann mit `./build.sh` ohne `--bump` erneut bauen. Pakete der bisherigen
Version bleiben in ihrem eigenen Verzeichnis; neue Pakete landen unter
`dist/<neue-version>/`. Ohne `--bump` bleibt die Version unverändert.

`--clean` entfernt vor dem Bau alle generierten Inhalte unter `target/`:
native Debug-/Release-Builds, musl-Server, sämtliche Distributions-Caches,
Windows-Cross-Builds, statische Windows-Bibliotheken, Experimente, Rustdoc und
Paket-Staging. Die Build-Skripte erstellen benötigte Dateien anschließend neu.
Build-Logs und die aktive Wrapper-Lockdatei bleiben erhalten, ebenso `dist/`
mit Paketen und Backups sowie `.local/`. Installierte Toolchains, heruntergeladene
Cargo-Abhängigkeiten und Podman-Images/-Download-Volumes bleiben verfügbar.
Aktive Builds oder Programme aus diesem Checkout verhindern die Bereinigung;
parallel keine einzelnen Cargo-/Paket-Befehle starten. Symlinks innerhalb von
`target/` werden nur entfernt, ihre Ziele bleiben unangetastet.

`--clean-only` führt dieselbe Bereinigung aus, beendet sich aber ohne Neubau.
Es benötigt nur Bash und Python ab 3.11, keine Cargo-/Podman-/Cross-Toolchain;
Version und Paketausgaben werden nicht verändert. Die Bereinigung verwendet
dieselbe Checkout-Sperre und Prozessprüfung wie `--clean` und schreibt ein Log
unter `target/build-logs/<run>/0-clean.log`. `--plain` ist erlaubt;
`--bump` und `--clean` lassen sich nicht mit `--clean-only` kombinieren.

Der Einstieg baut den aktuellen Arbeitsstand, einschließlich uncommitteter
Änderungen. Es gibt keine Auswahl zwischen Entwicklungs- und Release-Build.
Er erstellt sieben Artefakte in `dist/<version>/`: Server-DEB und -RPM,
Client-RPMs für Fedora 42/43, Client-DEB für Debian 13, Windows-ZIP und
Windows-Setup. AEC, Workspace-Tests, Lizenzprüfungen, Linux-Paket-Smokes und
Windows-Binärprüfungen sind immer aktiv. Geerbte `SKIP_TESTS`, `SKIP_SMOKE`,
`DISTS`, `SERVER_DISTS` und `NO_AEC` werden dafür überschrieben;
`CARGO_TARGET_DIR` und `CARGO_BUILD_TARGET` werden wegen der festen
Paket-Stagingpfade entfernt. Compiler-Caches bleiben erhalten.
`RELEASE` kann weiterhin die positive Paket-Iterationsnummer setzen (Standard 1).

Benötigt werden ein Linux-x86-64-Host, Bash, Python ab 3.11, Git, Podman sowie
die unten beschriebenen nativen und Windows-Cross-Build-Abhängigkeiten inklusive
cargo-deny, cargo-about und NSIS. Der Vorabcheck sammelt fehlende Werkzeuge,
Bibliotheken und Podman-Zugriffsprobleme, bevor der eigentliche Bau startet.

Das Dashboard zeigt Stufen, laufenden Arbeitsschritt, Zeiten und die letzten
Logzeilen. Bei umgeleiteter Ausgabe oder `TERM=dumb` werden Logs direkt
geschrieben; `NO_COLOR` schaltet Farben ab. Vollständige Logs und ein
Quellstand-Fingerabdruck liegen in `target/build-logs/<run>/`.
Wenn sich Quelldateien während des Baus ändern, endet der Lauf mit Fehler.

Vorhandene Ausgaben derselben Version werden vor dem Bau nach
`dist/backups/<version>-<run>/` verschoben. Bei Fehlern bleiben Backup, Logs
und Teilergebnisse erhalten. Zum Wiederherstellen die Teilergebnisse separat
weglegen und das Backup zurück nach `dist/<version>/` verschieben.
Ein zweiter Wrapper-Lauf im selben Checkout wird abgewiesen; die einzelnen
Paket-Skripte nicht gleichzeitig starten. Strg+C beendet die Prozessgruppe
und die mit diesem Lauf markierten Container.

Erfolg wird erst nach Prüfung aller sieben Artefakte, ihrer BUILDINFO-Dateien
und SHA-256-Prüfsummen gemeldet. Uncommitteter Code ist in BUILDINFO als
`DIRTY` gekennzeichnet; die bestehenden Veröffentlichungsregeln bleiben
erhalten. Der Befehl veröffentlicht und installiert nichts auf dem Host.
Interaktive Windows-Installation und Hardwareprüfungen bleiben eigene Tests.

Tests des Wrappers (ohne Compiler-/Container-Build):

```sh
python3 -B -m unittest discover -s packaging/tests -v
```

## Linux

Der native Client benötigt auf Fedora `pipewire-devel`, `clang-devel`, `meson`,
`ninja-build`, `abseil-cpp-devel`, `llvm`, `cmake`, `nasm`, `libxkbcommon-devel`
und `wayland-devel`. Die Containerrezepte unter `packaging/containers/` beschreiben
die vollständigen Umgebungen für unterstützte Paketplattformen.

Das gebündelte AEC-Buildskript erwartet `rust-objcopy` unter
`<rustc-sysroot>/lib/rustlib/<host>/bin/`. Beim Fedora-Rust-Paket muss dort
`rust-objcopy` auf das installierte `llvm-objcopy` zeigen; prüfen mit
`rustc --print sysroot` und `rustc -vV`. Der Build lädt keine zusätzliche
Abseil-Kopie, wenn die System-Header vorhanden sind.

```sh
cargo build --locked --release -p icu-client
cargo build --locked --profile release-server -p icu-server
cargo fmt --all -- --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Der native Server kann dynamisch gelinkt sein. Den statischen musl-Server baut
das Paket-Skript im Container; der vollständige manuelle Weg steht im
[Betriebshandbuch](../../deploy/README.md). `release-server` verwendet Unwinding,
damit der Panic eines Verbindungstasks nicht den gesamten Server beendet.

## Windows

Cross-Build auf Fedora: `mingw64-gcc`, `mingw64-gcc-c++`,
`mingw64-winpthreads-static`, `mingw64-binutils`,
`rust-std-static-x86_64-pc-windows-gnu`, Meson, Ninja und zip; dazu cargo-about
0.9.2 und cargo-deny 0.20.2 wie in den Linux-Baucontainern.

```sh
packaging/windows/build.sh
packaging/windows/installer.sh
```

Der Installer braucht zusätzlich `mingw64-nsis` und `mingw32-nsis`.
Das Build-Skript aktiviert `aec,gui,camera,tray,overlay`, bereitet die statische
C++-Bibliothek vor und setzt den Meson-Wrapper in PATH. Es prüft System-DLLs,
Ressourcen und Lizenzen. Details: [Toolchain](../../packaging/windows/toolchain/README.md).
Windows-N/KN benötigt das Media Feature Pack.

Für einen direkten Cross-Build oder Cross-Test nach `cargo clean` zuerst:

```sh
mkdir -p target/windows-static
cp -u /usr/x86_64-w64-mingw32/sys-root/mingw/lib/libstdc++.a target/windows-static/
PATH="$PWD/packaging/windows/toolchain:$PATH" cargo build --locked --release \
  -p icu-client --target x86_64-pc-windows-gnu \
  --no-default-features --features aec,gui,camera,tray,overlay
```

## Pakete und Veröffentlichung

Das Fedora-Clientpaket heißt seit 2026-10-16 `icu-client`; der Programmaufruf
bleibt `icu`. Der bisherige RPM-Name `icu` kollidiert mit Fedoras Unicode-Paket:
PackageKit ersetzte am 2026-10-15 den Voice-Client 0.1.15 durch Unicode 77.1.
Das neue Paket lässt sich parallel zum Fedora-Paket installieren. Bei einer
noch vorhandenen alten Voice-Installation zuerst deren Paket entfernen;
`~/.config/icu/` mit Identität und Einstellungen bleibt erhalten. Debians
Paketname bleibt unverändert.

```sh
packaging/build.sh       # Linux-Pakete, Workspace-Tests und Container-Smokes
packaging/smoke.sh       # Bereits gebaute Linux-Pakete prüfen
```

Ergebnisse: `dist/<version>/`. `BUILDINFO.*` nennt Commit, Toolchain und die
Artefakte des Laufs; `SHA256SUMS` erfasst diese Dateien. Paket-Staging und
plattformabhängige Cargo-Caches liegen in `target/`. `ALLOW_DIRTY=1` ist nur für
Entwicklungsprüfungen: solche BUILDINFO-Dateien sind als DIRTY markiert und nicht
veröffentlichbar. Vor einem Prüflauf vorhandene, aufzubewahrende Pakete sichern,
da derselbe Versionsname dieselben Ausgabedateien verwendet.

Veröffentlichen ist ein eigener Schritt über `packaging/release.sh`; es verlangt
einen sauberen, signierten Release-Stand und die konfigurierte Signaturprüfung.
Ein erfolgreicher Paketbau veröffentlicht nichts.

## Lokale Dateien und Bereinigung

- `target/` enthält ausschließlich regenerierbare Ausgaben. `cargo clean` löscht
  auch die vorbereitete Windows-Bibliothek; das Paket-Skript stellt sie wieder her.
- `dist/` enthält aufzubewahrende Auslieferungsdateien, keine Compiler-Caches.
- `.local/` enthält lokale Testzustände und Belege. Nicht pauschal mit Build-Caches
  löschen; laufende Prozesse und VM-Dateiverweise vor Umzügen prüfen.
- Cargo-Caches verschiedener Worktrees bleiben getrennt. Vor manueller Bereinigung
  alle betroffenen Builds beenden und den Cargo-Lock beachten.
- Container-Builds sehen `.local/` nur als leeres tmpfs; der Quellbaum ist lesend,
  nur `target/` schreibbar. Das Runtime-Image erhält über `.dockerignore` allein
  das statische Server-Binary.

Beim Aufruf im Experimentverzeichnis setzt dessen `.cargo/config.toml` den
Zielpfad auf `target/experiments/<name>/` im Repository-Root. Bei einem Aufruf
vom Root mit `--manifest-path` wird diese innere Konfiguration nicht geladen;
dafür explizit beispielsweise
`CARGO_TARGET_DIR="$PWD/target/experiments/spike-aec" cargo build --manifest-path experiments/spike-aec/Cargo.toml`.
Das Manifest und seine Lockdatei bleiben beim Experiment; neue Ergebnisse werden
mit Datum und Messbedingungen dokumentiert.
