# build/win-static

Dieses Verzeichnis existiert fuer den Windows-Cross-Build des **Clients**
(`--target x86_64-pc-windows-gnu`). Es wird von `.cargo/config.toml` per
`-L native=build/win-static` **vor** die System-Suchpfade gelegt und darf
deshalb genau eine Datei enthalten:

    libstdc++.a

Die Datei ist hier bewusst **nicht** eingecheckt (rund 10 MB Binaerartefakt aus
einem Fedora-Paket). Sie wird beim Cross-Build kopiert:

    cp /usr/x86_64-w64-mingw32/sys-root/mingw/lib/libstdc++.a build/win-static/

Voraussetzung: `sudo dnf install mingw64-gcc mingw64-gcc-c++ mingw64-winpthreads-static rust-std-static-x86_64-pc-windows-gnu`.

## Warum

`openh264-sys2` emittiert `cargo:rustc-link-lib=stdc++` **dynamisch**;
`-static-libstdc++` wirkt bei rustc nicht. Ohne diesen Trick importiert die
erzeugte `.exe` `libstdc++-6.dll` und startet auf einem nackten Windows nicht.
Belegt in `Research/verification.md`, Spike 1.

Der **Server** braucht dieses Verzeichnis nicht: er baut gegen
`x86_64-unknown-linux-musl` und hat kein C++ im Baum.
