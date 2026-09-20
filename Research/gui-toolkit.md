# GUI-Toolkit für den Client

## Frage

Welches Rust-Toolkit trägt ein sehr kleines, poliertes Client-Fenster (Teilnehmerliste mit Speaking-Indicator, Mute/Deafen/PTT-Toggle, Lautstärke pro User, Kamera-Toggle, Settings) nativ auf Windows und Linux (KDE Plasma Wayland), mit Tray-Icon, ohne C/C++-Abhängigkeiten im Cross-Compile nach Windows, mit brauchbarer Lizenz und aktiver Wartung?

## Kandidaten

- egui / eframe
- iced
- Slint
- Tauri (Webview: WebView2 / webkit2gtk)
- Dioxus (Desktop = Webview über wry/tao; nativer Renderer experimentell)
- GPUI (Zed)
- Tray: tray-icon (Backends AppIndicator/GTK oder KSNI), ksni (pure Rust StatusNotifierItem)

## Primärquellen

Alle abgerufen am 2026-09-20. Version/Datum/Lizenz aller Crates ausschließlich aus der crates.io-API per `curl https://crates.io/api/v1/crates/<name>`; docs.rs und READMEs nur für API-Form und Plattform-Aussagen.

- E1 eframe Crate-Root: https://docs.rs/eframe/latest/eframe/
- E2 egui `ViewportBuilder`: https://docs.rs/egui/latest/egui/viewport/struct.ViewportBuilder.html
- E3 egui `WindowLevel`: https://docs.rs/egui/latest/egui/viewport/enum.WindowLevel.html
- I1 iced Crate-Root: https://docs.rs/iced/latest/iced/
- I2 iced `window::Settings`: https://docs.rs/iced/latest/iced/window/struct.Settings.html
- I3 iced `window::Level`: https://docs.rs/iced/latest/iced/window/enum.Level.html
- S1 Slint Crate-Root: https://docs.rs/slint/latest/slint/
- S2 Slint Cargo-Features (Backends/Renderer): https://docs.rs/slint/latest/slint/docs/cargo_features/index.html
- S3 Slint LICENSE.md: https://raw.githubusercontent.com/slint-ui/slint/master/LICENSE.md
- S4 Slint Royalty-free License 2.0: https://raw.githubusercontent.com/slint-ui/slint/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md
- T1 Tauri Prerequisites: https://tauri.app/start/prerequisites/
- D1 Dioxus Crate-Root: https://docs.rs/dioxus/latest/dioxus/
- G1 GPUI Crate-Root: https://docs.rs/gpui/latest/gpui/
- G2 GPUI README: https://raw.githubusercontent.com/zed-industries/zed/main/crates/gpui/README.md
- R1 tray-icon Crate-Root: https://docs.rs/tray-icon/latest/tray_icon/
- R2 tray-icon Cargo-Features: https://docs.rs/crate/tray-icon/latest/features
- R3 ksni Crate-Root: https://docs.rs/ksni/latest/ksni/
- X1 cargo-xwin README: https://raw.githubusercontent.com/rust-cross/cargo-xwin/main/README.md
- W1 winit `src/window.rs` v0.30.13 (Plattform-Notizen): https://raw.githubusercontent.com/rust-windowing/winit/v0.30.13/src/window.rs
- W2 winit Plattform Wayland: https://docs.rs/winit/latest/winit/platform/wayland/index.html

Nicht abrufbar (nichts daraus zitiert): `freedesktop.org/wiki/Specifications/StatusNotifierItem/` (403).

## Befunde

### Versionen, Lizenzen, Aktivität (crates.io-API, 2026-09-20)

| Crate | Stabile Version | Lizenz | Letztes Release |
| --- | --- | --- | --- |
| egui | 0.36.2 | MIT OR Apache-2.0 | 2026-09-08 |
| eframe | 0.36.2 | MIT OR Apache-2.0 | 2026-09-08 |
| iced | 0.14.0 | MIT | 2025-12-07 |
| slint | 1.18.0 | GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0 | 2026-09-16 |
| tauri | 2.11.6 (3.0.0-alpha.1 vorhanden) | Apache-2.0 OR MIT | 2026-09-19 |
| dioxus | 0.7.10 (0.8.0-alpha.1 vorhanden) | MIT OR Apache-2.0 | 2026-07-31 |
| gpui | 0.2.2 | Apache-2.0 | 2025-10-22 |
| tray-icon | 0.25.1 | MIT OR Apache-2.0 | 2026-09-16 |
| ksni | 0.3.6 | Unlicense | 2026-07-15 |
| winit | 0.30.13 (0.31.0-beta.3 vorhanden) | Apache-2.0 | 2026-09-04 |
| wgpu | 30.0.1 | MIT OR Apache-2.0 | 2026-08-22 |

### egui / eframe

- eframe: Backends `wgpu` (Default) und `glow` (OpenGL; "can significantly reduce your binary size"); auf winit aufgebaut; Features `wayland` und `x11` beide Default ("Enables wayland support and fixes clipboard issue"); AccessKit-Integration Default; optionale Persistenz. [E1]
- Viewport-Optionen: `with_transparent`, `with_always_on_top`, `with_window_level`, `with_mouse_passthrough` ("mouse clicks pass through the window, used for non-interactable overlays"), `with_decorations`, `with_taskbar` (Windows). [E2] `WindowLevel::{Normal, AlwaysOnBottom, AlwaysOnTop}` verweist für Plattform-Kompatibilität auf winit. [E3] winit: `set_window_level` "Wayland: Unsupported" [W1] — für das Hauptfenster irrelevant, für ein Overlay entscheidend (siehe `overlay.md`).
- Transparentes Hauptfenster unter Wayland: winit `with_transparent` "on some platforms this is more of a hint" [W1]; eframe/egui nennen keine Wayland-Einschränkung [E2]. Ob ein abgerundetes, transparentes egui-Hauptfenster unter KWin sauber aussieht (Client-Side-Decorations via `wayland-csd-adwaita`-Features [W2]): **unbelegt** → Step 3.
- Immediate-Mode: Speaking-Indicator, Pegelanzeige und Animationen sind pro Frame gezeichnete Widgets; Custom-Widgets sind Funktionen über `Ui`/`Painter` (aus Kenntnis der API; nicht eigens belegt). Fonts: eigene TTF einbindbar (Kenntnis, **unbelegt** aus den gelesenen Seiten).
- Reine Rust-Abhängigkeitskette (winit, wgpu oder glow) — keine C/C++-Bibliothek nötig; das ergibt sich aus E1 und den Abhängigkeiten, ist aber hier **nicht** durch einen Build belegt → Step 3.

### iced

- iced 0.14.0, "cross-platform GUI library focused on simplicity and type-safety. Inspired by Elm"; Renderer über `iced_wgpu`; Fenster über `iced_winit`. [I1] `window::Settings`: `transparent`, `level`, `decorations`, `visible`, `resizable`, `exit_on_close_request`, `platform_specific`. [I2] `Level::{Normal, AlwaysOnBottom, AlwaysOnTop}` ohne Plattform-Notizen. [I3]
- Elm-Architektur (Message/Update/View) ist für ein kleines Statusfenster mehr Struktur als nötig; Custom-Widgets erfordern das Widget-Trait statt einer Zeichenfunktion. Das ist eine Bewertung, keine belegte Tatsache.

### Slint

- Backends `backend-winit` (Default; "Windows, macOS, web browsers, X11 and Wayland"), `backend-winit-x11`, `backend-winit-wayland`, `backend-qt` (Qt 5.15+), `backend-linuxkms`. Renderer `renderer-femtovg` (Default bei winit), `renderer-skia`, `renderer-software` (Default). [S2] Eine C++-Toolchain-Anforderung für `renderer-skia` steht auf der Seite nicht → **unbelegt**; mit dem Default femtovg ist das Thema ohnehin umgangen.
- `.slint`-DSL wird per Makro oder `build.rs` zur Compile-Zeit in Rust übersetzt; Event-Loop im Main-Thread. [S1]
- Lizenz: GPLv3 oder "Royalty-free License ... Permits use in proprietary desktop, mobile, and web applications at no cost" oder kommerziell. [S3] Royalty-free 2.0: erlaubt "Desktop, Mobile, or Web Application"; verboten in "Embedded Systems"; Attribution zwingend: `AboutSlint`-Widget in einem About-Dialog, der "accessible from the top level menu of the Application" ist, oder Slint-Badge auf der Download-Webseite. [S4] Für ICU wäre das machbar, ist aber eine dauerhafte Pflicht in einem bewusst minimalen UI.

### Tauri

- Fedora-Voraussetzungen: `webkit2gtk4.1-devel, openssl-devel, curl, wget, file, libappindicator-gtk3-devel, librsvg2-devel, libxdo-devel` plus `c-development`; Windows: MSVC Build Tools ("Desktop development with C++") und WebView2 Runtime ("Evergreen Bootstrapper"); "Tauri uses Microsoft Edge WebView2 to render content on Windows", auf Linux webkit2gtk. [T1] Laufzeitabhängigkeit vom System-Webview, GTK auf Linux, kein statisches Einzelbinary.

### Dioxus

- Desktop-Renderer nutzt Wry/Tao, also WebView2 auf Windows und WebKitGTK auf Linux; der WGPU-basierte native Renderer (Blitz) ist "experimental". [D1] Gleiche Abhängigkeitslage wie Tauri, solange der native Renderer experimentell ist.

### GPUI

- "GPUI is a hybrid immediate and retained mode, GPU accelerated, UI framework for Rust"; macOS Metal; Linux/FreeBSD Wayland und/oder X11 (blade-graphics/Vulkan); Windows "Fully supported with Win32 windowing and DirectWrite text". [G1][G2] "GPUI is still in active development as we work on the Zed code editor, and is still pre-1.0. There will often be breaking changes between versions." [G2] Letztes crates.io-Release 2025-10-22.

### Tray-Icon

- tray-icon 0.25.1: Windows über `Shell_NotifyIcon`; macOS Main-Thread; Linux: "The default Linux backend uses GTK, libxdo, and libappindicator or libayatana-appindicator"; "On Windows and the Linux/BSD AppIndicator backend, an event loop must be running on the thread"; KSNI-Backend läuft auf einem Worker-Thread ohne GTK-Loop. [R1] Features: Default `libappindicator`, `muda-libxdo`, `muda-gtk3`; optional `ksni`. [R2] → Auf Linux `default-features = false, features = ["ksni"]`, damit weder GTK3 noch libappindicator gelinkt werden.
- ksni 0.3.6: "A Rust implementation of the KDE/freedesktop StatusNotifierItem specification", über zbus 5, tokio-Default (alternativ async-io oder `blocking`). [R3] KDE Plasma implementiert StatusNotifierItem nativ (Kenntnis; Spezifikationsseite nicht abrufbar → **unbelegt** aus Quellen).
- Ob muda-Menüs im tray-icon-KSNI-Backend vollständig funktionieren: nicht geprüft → Step 3.

### Cross-Compile nach Windows

- cargo-xwin: Cross-Compile nach `x86_64-pc-windows-msvc` von Linux mit MSVC-CRT und Windows-SDK aus `xwin`; braucht clang und `llvm-tools`; Microsoft-Lizenzbedingungen für das SDK sind zu akzeptieren. [X1] Alternative `x86_64-pc-windows-gnu` mit mingw (aus Kenntnis; hier **unbelegt**).
- Solange nur Rust-Crates ohne `cc`/C++-Build-Skripte in der Kette sind (egui/eframe + wgpu/glow + tray-icon KSNI/Shell_NotifyIcon + jpeg-encoder/zune-jpeg aus `camera-video.md`), ist kein C++-Compiler für das Target nötig. Der Beleg dafür ist ein Probe-Build in Step 3.

### Binary-Größe und Startzeit

- Keine belastbaren Zahlen aus Primärquellen. eframe nennt nur, dass `glow` das Binary "significantly" verkleinert [E1]. Alles Weitere wäre Schätzung; Messung in Step 3 (Release-Build, `strip`, LTO, `opt-level = "z"` vs. `3`, Kaltstart bis erstes Frame).

## Entscheidung

**egui/eframe 0.36.2** für das Client-Hauptfenster, Standard-Backend `wgpu` (Fallback `glow` per Feature, falls Binary-Größe oder Treiberprobleme in Step 3 dagegen sprechen). Begründung:

1. Pure Rust bis zum Fenster (winit) und zur GPU (wgpu/glow); keine Webview, kein GTK, kein Qt, keine DSL-Toolchain [E1].
2. Wayland ist Default-Feature; keine dokumentierte Einschränkung für transparente, dekorationslose Hauptfenster [E1][E2].
3. Immediate-Mode passt zu einem Fenster, dessen Inhalt sich pro Frame ändert (Speaking-Indicator, Pegel): kein Widget-Zustand zu synchronisieren, Custom-Look über `Painter` statt Widget-Traits.
4. MIT/Apache, aktivste Release-Kadenz der Kandidaten (0.36.2 am 2026-09-08).
5. Kein Attributionszwang (im Gegensatz zu Slint) [S4].

Tray: **tray-icon 0.25.1**, auf Linux mit `default-features = false, features = ["ksni"]` (pure Rust über zbus, kein GTK) [R1][R2][R3]; auf Windows Default (`Shell_NotifyIcon`) [R1]. Das KSNI-Backend braucht keinen GTK-Loop und stört den eframe-Loop nicht.

Overlay: nicht über egui, sondern natives Fenster in eigenem Thread (siehe `overlay.md`), weil winit/egui auf Wayland kein Always-on-top und kein Layer-Shell bieten [W1][W2]. egui bleibt damit ein reines Hauptfenster-Toolkit.

## Verworfen, mit Grund

- **Tauri**: Laufzeit-Webview (WebView2, webkit2gtk), GTK und libappindicator auf Linux, MSVC-C++-Tools auf Windows [T1]; widerspricht "ein statisches Binary" und minimaler Abhängigkeitskette; UI in HTML/JS statt Rust.
- **Dioxus**: Desktop = Webview über Wry/Tao [D1], nativer Renderer "experimental" [D1]; gleiche Gründe wie Tauri.
- **GPUI**: "pre-1.0 ... breaking changes between versions" [G2], kein crates.io-Release seit 2025-10-22; primär Zed-Werkzeug.
- **Slint**: technisch geeignet (winit/femtovg Default [S2]), aber Royalty-free-Lizenz verlangt dauerhaft `AboutSlint`-Dialog oder Badge [S4], sonst GPLv3 [S3]; zusätzlich eigene DSL und Build-Schritt. Kein Vorteil, der diese Pflicht rechtfertigt.
- **iced**: geeignet und MIT [I1], aber Elm-Architektur und Widget-Trait sind für ein winziges, animiertes Statusfenster mehr Gerüst als nötig; weniger Release-Aktivität (0.14.0 vom 2025-12-07). Bewertung, nicht Beleg. Bleibt zweite Wahl, falls egui in Step 3 am Look scheitert.
- **tray-icon Default-Features auf Linux**: zieht GTK3 + libappindicator [R2]; KSNI-Backend vermeidet das.

## Offene Punkte für Step 3

Auf Chris' KDE-Wayland-Session:

1. eframe-Spike: dekorationsloses Fenster mit `with_transparent(true)`, abgerundete Ecken per egui-Frame, eigene TTF, Speaking-Indicator-Animation; prüfen, wie KWin es darstellt (CSD/SSD, Schatten, Blur), ob Fenster verschiebbar bleibt (`drag_window`).
2. Tray über tray-icon `ksni`: Icon, Menü (muda) und Klick-Events unter Plasma 6.7; Verhalten bei Neustart von plasmashell.
3. Messung Binary-Größe (Release, LTO, strip) und Kaltstartzeit für `wgpu` vs. `glow`.
4. Probe-Build nach `x86_64-pc-windows-msvc` per cargo-xwin und nach `x86_64-pc-windows-gnu`: bestätigen, dass kein `cc`/C++-Schritt in der Abhängigkeitskette liegt (`cargo tree -e build`).
5. Skalierung: fractional scaling (z. B. 125 %) unter KWin, Schriftrendering-Qualität von egui bei kleinen Fonts.
6. Eingabe: Tastaturfokus des Hauptfensters darf PTT nicht beeinflussen (PTT kommt vom Portal/evdev, siehe `hotkey-wayland.md`).

Auf Windows (später):

7. eframe unter Windows 11: transparentes, dekorationsloses Fenster, DPI-Skalierung, Tray via `Shell_NotifyIcon`.
8. Startzeit und Größe des Windows-Binaries; WebView2 ist irrelevant (nicht benötigt) — nur bestätigen, dass keine Laufzeit-Abhängigkeit außer System-DLLs besteht.

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `verification.md`):
  - Spike 1 **bestätigt** Punkt 4 für `x86_64-pc-windows-gnu`: eframe 0.33 (glow) im vollen Dependency-Satz cross-kompiliert ohne eigenen C/C++-Schritt (die C++-Deps kamen nur von openh264). eframe 0.36.2 ist zu erwarten gleichwertig, in der Implementierung erneut prüfen. cargo-xwin/msvc nicht getestet und nach `cross-compile.md` nicht nötig.
  - Offen: Punkt 1–3, 5–6 (Look, Tray, Größe), Windows 7–8.

Mit WebFetch tatsächlich gelesen (2026-09-20):

- https://docs.rs/eframe/latest/eframe/
- https://docs.rs/egui/latest/egui/viewport/struct.ViewportBuilder.html
- https://docs.rs/egui/latest/egui/viewport/enum.WindowLevel.html
- https://docs.rs/iced/latest/iced/
- https://docs.rs/iced/latest/iced/window/struct.Settings.html
- https://docs.rs/iced/latest/iced/window/enum.Level.html
- https://docs.rs/slint/latest/slint/
- https://docs.rs/slint/latest/slint/docs/cargo_features/index.html
- https://raw.githubusercontent.com/slint-ui/slint/master/LICENSE.md
- https://raw.githubusercontent.com/slint-ui/slint/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md
- https://tauri.app/start/prerequisites/
- https://docs.rs/dioxus/latest/dioxus/
- https://docs.rs/gpui/latest/gpui/
- https://raw.githubusercontent.com/zed-industries/zed/main/crates/gpui/README.md
- https://docs.rs/tray-icon/latest/tray_icon/
- https://docs.rs/crate/tray-icon/latest/features
- https://docs.rs/ksni/latest/ksni/
- https://raw.githubusercontent.com/rust-cross/cargo-xwin/main/README.md
- https://raw.githubusercontent.com/rust-windowing/winit/v0.30.13/src/window.rs
- https://docs.rs/winit/latest/winit/platform/wayland/index.html
