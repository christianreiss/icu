# Windows-Cross-Toolchain

`meson` reicht für `TARGET=x86_64-pc-windows-gnu` die Cross-Datei `mingw64.ini`
an den AEC-Build weiter. Es entfernt dort `WEBRTC_ENABLE_SYMBOL_EXPORT` aus den
kopierten WebRTC-Quellen, weil das nachfolgende Umbenennen statischer Symbole sonst
ungültige Exportdirektiven hinterlässt. Der Wrapper leitet andere Aufrufe an das
System-Meson weiter. `packaging/windows/build.sh` setzt dieses Verzeichnis in PATH.

`mingw64.ini` setzt die MinGW-Werkzeuge und statisches libstdc++/libgcc.
`NTDDI_VERSION=0x0A000000` umgeht den Konflikt zwischen Abseils cctz-WinRT-Code
und den MinGW-Headern; die AEC fragt keine lokale Zeitzone ab.

## Statische C++-Bibliothek

OpenH264 fordert `-lstdc++` an; allein `-static-libstdc++` reicht deshalb beim
Rust-Linkschritt nicht. `.cargo/config.toml` durchsucht zuerst
`target/windows-static/`. Dort liegt ausschließlich die kopierte `libstdc++.a`:

```sh
mkdir -p target/windows-static
cp -u /usr/x86_64-w64-mingw32/sys-root/mingw/lib/libstdc++.a target/windows-static/
```

Das Paket-Skript erledigt diese Vorbereitung nach einem sauberen Checkout oder
`cargo clean`. Die Bibliothek bleibt ignorierte Build-Ausgabe. Der Server braucht
sie nicht. Das Windows-Paket darf nur System-DLLs importieren; `build.sh` prüft das.

Voraussetzungen und komplette Befehle: [Bauen](../../../docs/development/building.md).
Hintergrund: [Cross-Compile](../../../docs/research/cross-compile.md) und
[AEC](../../../docs/research/aec.md).
