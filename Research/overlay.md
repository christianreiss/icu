# Kamera-Overlay über dem Spiel

## Frage

Wie zeigt der ICU-Client kleine Kamera-Kacheln (z. B. 4 × 320×180) als transparentes, klick-durchlässiges Always-on-top-Fenster über einem laufenden Spiel, auf Windows 10/11 und auf KDE Plasma Wayland, ohne in den Spielprozess einzugreifen? Und wie spielt dieses Fenster mit dem GUI-Toolkit des Hauptfensters zusammen?

**Scope-Grenze v1 (fest):** Das Overlay funktioniert nur über Spielen im Borderless-Window / Windowed-Fullscreen. Kein DirectX-, Vulkan- oder OpenGL-Hooking, keine DLL-Injection, kein `LD_PRELOAD`. Begründung in den Befunden (Abschnitt "Warum kein exklusiver Fullscreen") und im Vergleich mit Discord, Mumble, Steam.

## Kandidaten

Windows

- Layered Window: `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`, Inhalt per `UpdateLayeredWindow` (per-pixel Alpha, CPU-Bitmap)
- DWM-Pfad: `DwmExtendFrameIntoClientArea` mit "sheet of glass" + GPU-Swapchain
- egui/winit-Viewport mit `with_transparent`, `with_always_on_top`, `with_mouse_passthrough`

X11

- override-redirect-Fenster, ARGB-Visual, XShape `ShapeInput` mit leerer Region

Wayland

- `zwlr_layer_shell_v1`, Layer `overlay`, `keyboard_interactivity = none`, leere Input-Region via `wl_surface.set_input_region`
- winit `xdg_toplevel` mit `WindowLevel::AlwaysOnTop` (zum Vergleich)

Rendering: softbuffer (CPU, `wl_shm` / GDI) vs. wgpu (GPU, `CompositeAlphaMode`)

Rust: wayland-client, smithay-client-toolkit (SCTK), winit, softbuffer, wgpu, windows-rs

## Primärquellen

Alle abgerufen am 2026-09-20.

Windows (Microsoft Learn, Vendor-Blog Microsoft)

- W1 Extended Window Styles: https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles
- W2 Window Features (Abschnitt "Layered Windows"): https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features
- W3 UpdateLayeredWindow: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow
- W4 DwmExtendFrameIntoClientArea: https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmextendframeintoclientarea
- W5 DXGI Overview (Full-screen, Occlusion): https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/d3d10-graphics-programming-guide-dxgi
- W6 For best performance, use DXGI flip model: https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model
- W7 Demystifying Full Screen Optimizations (Vendor-Blog, Microsoft DirectX Developer Blog): https://devblogs.microsoft.com/directx/demystifying-full-screen-optimizations/
- W8 Optimizations for windowed games in Windows 11 (Microsoft Support): https://support.microsoft.com/en-us/windows/optimizations-for-windowed-games-in-windows-11-3f006843-2c7e-4ed0-9a5e-f9389e535952

X11 (X.Org)

- X1 X11 Protocol (override-redirect): https://xorg.freedesktop.org/archive/X11R7.7/doc/xproto/x11protocol.html
- X2 Shape Extension (ShapeInput): https://xorg.freedesktop.org/archive/X11R7.7/doc/xextproto/shape.html

Wayland

- Y1 wayland.app wlr-layer-shell-unstable-v1 (Protokoll + Compositor-Support-Tabelle): https://wayland.app/protocols/wlr-layer-shell-unstable-v1
- Y2 wayland-client `WlSurface::set_input_region` (Protokolltext): https://docs.rs/wayland-client/latest/wayland_client/protocol/wl_surface/struct.WlSurface.html
- Y3 KDE Plasma Changelog 5.19.5 → 5.20.0 (KWin "Implement layer-shell v1 protocol"): https://kde.org/announcements/changelogs/plasma/5/5.19.5-5.20.0/
- Y4 GitLab-API, Mutter Issue #973: https://gitlab.gnome.org/api/v4/projects/GNOME%2Fmutter/issues/973

Rust-Crates (API aus docs.rs / Repos; Version/Datum/Lizenz aus crates.io-API per curl)

- C1 SCTK `shell::wlr_layer`: https://docs.rs/smithay-client-toolkit/latest/smithay_client_toolkit/shell/wlr_layer/index.html
- C2 SCTK `LayerSurface`: https://docs.rs/smithay-client-toolkit/latest/smithay_client_toolkit/shell/wlr_layer/struct.LayerSurface.html
- C3 winit `WindowAttributes`: https://docs.rs/winit/latest/winit/window/struct.WindowAttributes.html
- C4 winit `src/window.rs` v0.30.13 (Doc-Comments zu `set_cursor_hittest`, `set_window_level`, `set_transparent`): https://raw.githubusercontent.com/rust-windowing/winit/v0.30.13/src/window.rs
- C5 winit Plattform Wayland: https://docs.rs/winit/latest/winit/platform/wayland/index.html
- C6 softbuffer Crate-Root: https://docs.rs/softbuffer/latest/softbuffer/
- C7 softbuffer README (Plattform-Tabelle): https://raw.githubusercontent.com/rust-windowing/softbuffer/master/README.md
- C8 wgpu Crate-Root: https://docs.rs/wgpu/latest/wgpu/
- C9 wgpu `CompositeAlphaMode`: https://docs.rs/wgpu/latest/wgpu/enum.CompositeAlphaMode.html
- C10 egui `ViewportBuilder`: https://docs.rs/egui/latest/egui/viewport/struct.ViewportBuilder.html

Vergleich Discord / Mumble / Steam

- D1 Discord Game SDK (Overlay-Abschnitt): https://docs.discord.com/developers/developer-tools/game-sdk
- M1 Mumble Repo `overlay/` (Windows): https://github.com/mumble-voip/mumble/tree/master/overlay
- M2 Mumble Repo `overlay_gl/` (Linux): https://github.com/mumble-voip/mumble/tree/master/overlay_gl
- M3 Mumble `overlay_gl/init_unix.c`: https://raw.githubusercontent.com/mumble-voip/mumble/master/overlay_gl/init_unix.c

Nicht abrufbar (nichts daraus zitiert): `partner.steamgames.com/doc/features/overlay` (DNS nicht auflösbar), `support.discord.com/.../Game-Overlay-101` (403), `mumble.info/documentation/user/overlay/` (404), `gitlab.freedesktop.org/wayland/wayland/-/raw/main/protocol/wayland.xml` (Anubis-Block; Protokolltext stattdessen aus Y2), `gitlab.gnome.org/GNOME/mutter/-/issues/973` als HTML (nur Issue-Text, Status daher aus Y4).

## Befunde

### Windows: Fensterstile und Klick-Durchlässigkeit

- `WS_EX_LAYERED`: "The window is a layered window"; ab Windows 8 auch für Child-Windows. `WS_EX_TOPMOST`: "placed above all non-topmost windows and should stay above them, even when the window is deactivated". `WS_EX_NOACTIVATE`: "does not become the foreground window when the user clicks it ... does not appear on the taskbar by default". `WS_EX_TOOLWINDOW`: "does not appear in the taskbar or in the dialog that appears when the user presses ALT+TAB". `WS_EX_TRANSPARENT` ist in W1 nur über die Malreihenfolge definiert ("should not be painted until siblings beneath the window ... have been painted"). [W1]
- Klick-Durchlässigkeit steht in W2: "Hit testing of a layered window is based on the shape and transparency of the window. This means that the areas of the window that are color-keyed or whose alpha value is zero will let the mouse messages through. However, if the layered window has the WS_EX_TRANSPARENT extended window style, the shape of the layered window will be ignored and the mouse events will be passed to other windows underneath the layered window." [W2]
- Per-pixel Alpha: "For faster and more efficient animation or if per-pixel alpha is needed, call UpdateLayeredWindow." Nach `SetLayeredWindowAttributes` schlagen `UpdateLayeredWindow`-Aufrufe fehl, bis das Layered-Bit neu gesetzt wird. [W2] `UpdateLayeredWindow` mit `ULW_ALPHA` und `BLENDFUNCTION`; "always updates the entire window"; "the layered window should be as small as possible". Die Quell-Bitmap kommt aus einem `CreateCompatibleDC`-DC. [W3] Konsequenz: pro Kachel ein kleines Fenster oder ein Fenster in Kachelgruppen-Größe, nicht bildschirmgroß.
- `DwmExtendFrameIntoClientArea`: negative Margins = "sheet of glass" ("client area is rendered as a solid surface with no window border"); muss bei `WM_DWMCOMPOSITIONCHANGED` neu aufgerufen werden. [W4] Dass ein D3D-Swapchain mit Alpha=0 in diesem Modus transparent durchscheint, steht **nicht** auf der Seite → **unbelegt**. Ebenso `WS_EX_NOREDIRECTIONBITMAP` + DirectComposition: nicht recherchiert, **unbelegt**.

### Windows: Warum kein exklusiver Fullscreen (FSE), und warum Borderless geht

- DXGI: "If mode switching is allowed, a swap chain will relinquish full-screen mode whenever its output window is occluded by another window. The occlusion check is performed during IDXGISwapChain1::Present1". Im Full-Screen "the swap chain flips (as opposed to blits) the contents of the back buffer to the front buffer". [W5] Ein Topmost-Overlay über einem echten FSE-Spiel ist also entweder nicht sichtbar (Flip am DWM vorbei) oder wirft das Spiel aus dem Fullscreen. Beides ist inakzeptabel; Abhilfe wäre Hooking der Present-Kette im Spielprozess, und genau das ist Nicht-Ziel.
- Flip-Model: "Flip model presents go as far as making windowed mode effectively equivalent or better when compared to the classic 'fullscreen exclusive' mode." Independent Flip: "If other desktop contents come on top, the DWM can either seamlessly transition back to composed mode, efficiently 'reverse compose' the contents on top of the application before flipping it, or leverage MPO to maintain the independent flip mode." [W6] Das ist der Mechanismus, der ein Overlay über Borderless-Spielen ohne Leistungsverlust für das Spiel sichtbar macht.
- Fullscreen Optimizations (Vendor-Blog): "Fullscreen Optimizations takes full screen exclusive games and runs them instead in a highly optimized borderless windowed format that takes up the entire screen." "your game believes that it is running in Fullscreen Exclusive, but behind the scenes, Windows has the game running in borderless windowed mode." Vorteile: "faster PC commands such as alt-tab, multiple monitor set ups and overlays". Eingeführt mit Windows 10. [W7] Damit sind viele "exklusive" Spiele de facto borderless; ob FSO für ein konkretes Spiel aktiv ist, hängt von Kompatibilitätseinstellungen ab (pro Spiel abschaltbar) → Step 3.
- Windows 11 "Optimizations for windowed games": "transitions compatible games from the legacy blt-model presentation to the newer flip-model", für DirectX-10/11-Spiele im Windowed/Borderless-Modus. [W8] Der Trend geht weiter Richtung Flip-Model/Borderless.

### X11 (nur als dokumentierter Fallback)

- override-redirect: "specifies whether map and configure requests on this window should override a SubstructureRedirect on the parent, typically to inform a window manager not to tamper with the window"; mit `True` werden keine `MapRequest`/`ConfigureRequest` an den WM erzeugt. [X1]
- Shape-Extension: "provides arbitrary window and border shapes within the X11 protocol"; `ShapeInput` (ab 1.1) ist die Region, die Pointer-Events empfängt; Events außerhalb "will be delivered as if the window was not occluding the event position". Leere Input-Region = vollständig klick-durchlässig. [X2]
- ARGB-Visual und Compositing-Anforderung: aus diesen Quellen **unbelegt** (nicht recherchiert; X11 ist für Chris' Wayland-Session nicht Ziel).

### Wayland: wlr-layer-shell

- `zwlr_layer_shell_v1` Version 5. Layer: `background`, `bottom`, `top`, `overlay` (höchste). "Multiple surfaces can share a single layer, and ordering within a single layer is undefined." `keyboard_interactivity`: `none` (Default, kein Tastaturfokus), `exclusive`, `on_demand` (ab v4). `get_layer_surface` aus einer `wl_surface`; `set_anchor`, `set_exclusive_zone` (0 = kein Platz reservieren; für ein Overlay 0 setzen, damit Fenster darunter nicht verschoben werden). [Y1]
- Compositor-Support laut Tabelle: KWin 6.7 (v5), Hyprland 0.52.1 (v5), Sway 1.11 (v4), niri, river, Wayfire, labwc, COSMIC, GameScope, Mir, Jay, Louvre, phoc, Treeland, Muffin, Cage (partial). **Mutter (GNOME) und Weston: "x" (nicht unterstützt).** [Y1]
- KWin seit Plasma 5.20.0: Changelog-Eintrag "Implement layer-shell v1 protocol." (KWin-Commit d3cca65d39ef1e01a7f6202837a8d032593fda05, KWayland-Server b0582f21...). [Y3] Lokal: plasma-workspace 6.7.5, also weit darüber.
- Mutter Issue #973 "implement layer_shell protocol": state `closed`, created 2019-12-14T13:41, closed 2019-12-14T14:44, Label "Wayland Protocol Request". [Y4] Geschlossen ohne Umsetzung (Maintainer-Kommentar nicht abgerufen). GNOME bleibt für v1 ohne Overlay.
- Input-Region: `wl_surface.set_input_region`: "This request sets the region of the surface that can receive pointer and touch events. Input events happening outside of this region will try the next surface in the server surface stack." "A NULL wl_region causes the input region to be set to infinite." [Y2] Eine leere `wl_region` (erzeugt, nichts hinzugefügt) ergibt damit ein vollständig klick-durchlässiges Overlay. Layer `overlay` mit `keyboard_interactivity = none` [Y1] → weder Tastatur- noch Pointer-Fokus, das Spiel behält beides.
- Ob KWin Layer-`overlay`-Surfaces auch über **Fullscreen**-xdg_toplevels (Borderless-Spiel, `set_fullscreen`) zeichnet: Protokoll sagt "overlay ... typically for fullscreen surfaces"-nahe Semantik, aber die konkrete KWin-Stapelreihenfolge ist aus diesen Quellen **unbelegt** → Step 3 (ich erwarte ja, da Panels/OSDs so gebaut sind).

### Rust: Wayland-Client-Seite

- smithay-client-toolkit 0.21.1, MIT, 2026-07-23 (crates.io-API). Modul `shell::wlr_layer` mit `LayerShell`, `LayerSurface`, `Layer`, `KeyboardInteractivity`, `Anchor`, `LayerSurfaceHandler`. [C1] `LayerSurface`-Methoden: `set_size`, `set_anchor`, `set_exclusive_zone`, `set_margin`, `set_keyboard_interactivity`, `set_layer`, `wl_surface()`, `commit`, `get_popup`. Keine eigene Input-Region-Methode; `wl_surface().set_input_region(Option<&WlRegion>)` benutzen. [C2]
- wayland-client 0.31.15, MIT, 2026-07-22 (crates.io-API).
- winit 0.30.13 (stabil; 0.31.0-beta.3 vorhanden), Apache-2.0 (crates.io-API). `with_transparent`: "on some platforms this is more of a hint". `with_window_level`: "This is just a hint to the OS, and the system could ignore it." Plattform-Notiz zu `set_window_level`: "iOS / Android / Web / Wayland: Unsupported." `set_cursor_hittest(false)`: "events are passed through the window such that any other window behind it receives them"; als nicht unterstützt nur "iOS / Android / Web / Orbital" gelistet, d. h. für Windows, X11 und Wayland **nicht** als unsupported markiert. `set_transparent` auf X11 nur beim Bauen des Fensters. [C3][C4] winit bietet nur `xdg_shell`; Wayland-Extensions (`WindowAttributesExtWayland` usw.) enthalten kein Layer-Shell. [C5] → Ein winit-Overlay kann auf Wayland nicht über einem Fullscreen-Spiel gehalten werden.

### Rendering

- softbuffer 0.4.8, MIT OR Apache-2.0, 2025-12-13 (crates.io-API). Tier 1: Win32, Wayland, X11 (XCB/Xlib), AppKit; "does not rely on the GPU". [C6][C7] Alpha-Kanal/Transparenz wird in Root-Doku und README **nicht** behandelt → **unbelegt**, ob per softbuffer ein ARGB-`wl_shm`-Buffer mit echter Transparenz entsteht → Step 3.
- wgpu 30.0.1, MIT OR Apache-2.0, 2026-08-22 (crates.io-API). Backends Vulkan/DX12/Metal/GL; Surface aus `raw-window-handle`. [C8] `CompositeAlphaMode::PreMultiplied`: "The alpha channel ... is respected in the compositing process. The non-alpha channels ... are expected to already be multiplied by the alpha channel"; `PostMultiplied` analog ohne Premultiply; `Opaque` ignoriert Alpha. [C9] Das ist der belegte GPU-Pfad für transparente Surfaces.

### egui/winit als Overlay (zum Vergleich)

- egui `ViewportBuilder`: `with_transparent` ("colors with alpha values different than 1.0 will create a transparent window"), `with_always_on_top`, `with_mouse_passthrough` ("mouse clicks pass through the window, used for non-interactable overlays. Generally you would use this in conjunction with with_transparent and with_always_on_top"), `with_decorations(false)`, `with_taskbar(false)` (Windows). [C10] Auf Windows sind alle Bausteine über winit belegt [C4]; auf Wayland scheitert es an `WindowLevel` [C4].

### Was Discord, Mumble, Steam tun

- Discord Game SDK: "The overlay is only supported on Windows for DirectX or OpenGL games. Linux, Mac, and games using Vulkan are not supported." Das Overlay wird vom Discord-Client in das Spiel gerendert (OverlayManager, Lock/Unlock Input). [D1] Also: In-Process-Rendering, Windows-only, API-abhängig.
- Mumble Windows-Overlay: Quellen `d3d9.cpp`, `d3d10.cpp`, `d3d11.cpp`, `dxgi.cpp`, `opengl.cpp`, `HardHook*.cpp` (u. a. MinHook), `overlay11.ps/.vs`, `overlay_blacklist.h`, `overlay_whitelist.h`, `excludecheck.cpp`. [M1] Mumble Linux-Overlay: `overlay_gl/init_unix.c` hookt `glXSwapBuffers`, fängt `dlsym` ab, findet das Original-`dlsym` über manuelles ELF-Parsing (DT_HASH/DT_GNU_HASH). [M2][M3] Deployment per `LD_PRELOAD` ist die naheliegende Inferenz (im Code nicht kommentiert). Fazit: Funktions-Hooking im Spielprozess, eigene Shader pro API, Black-/Whitelists gegen Abstürze und Anti-Cheat.
- Steam: Primärquelle nicht erreichbar (DNS). Mechanismus hier **unbelegt**; nicht als Fakt verwendet.
- Schluss für ICU: Alle drei tragen die Kosten von In-Process-Hooking (pro Grafik-API eigener Code, Bitness-Varianten, Kompatibilitätslisten, Anti-Cheat-Risiko, Vulkan bei Discord gar nicht). Für ein minimales System mit fester Gruppe ist der Compositor-Weg (Layered Window / Layer-Shell) die einzige Option, die ohne diesen Apparat auskommt. Der Preis ist die Borderless-Einschränkung, die durch FSO [W7] und den Flip-Model-Trend [W6][W8] in der Praxis klein ist.

## Entscheidung

Gemeinsames Modell: Das Overlay ist ein **eigenes natives Fenster in einem eigenen Thread**, unabhängig vom GUI-Toolkit. Die Kacheln (dekodierte Kamera-Frames, Name, Speaking-Rahmen) werden auf der CPU in einen premultiplied-BGRA-Buffer komponiert (bei 4 × 320×180 @ 15 fps sind das ~3,5 MPixel/s, CPU-trivial). Ein Frame-Kanal (`std::sync::mpsc` / `tokio::sync::watch`) verbindet Decoder → Overlay-Thread. Kein Eingriff in fremde Prozesse.

Windows

- Fenster: `CreateWindowExW` mit `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` (Style `WS_POPUP` für ein rahmenloses Top-Level-Fenster: Kenntnis, nicht aus den gelesenen Seiten belegt). Inhalt per `UpdateLayeredWindow(ULW_ALPHA, AC_SRC_ALPHA)` aus einem 32-bit-DIB. Klick-Durchlässigkeit durch `WS_EX_TRANSPARENT` [W2]. Fenstergröße = Kachelgruppe, nicht Bildschirm [W3]. Umsetzung mit windows-rs 0.62.2 (crates.io-API), ohne wgpu.
- Sichtbar über Borderless/Windowed-Fullscreen und über FSO-Spielen [W6][W7]. Über echtem FSE nicht sichtbar bzw. schädlich [W5]; das UI dokumentiert das ("Spiel auf Borderless stellen").
- Alternative für Step 3 im Vergleich: egui-Viewport mit `with_transparent + with_always_on_top + with_mouse_passthrough` [C10]; wird nur gewählt, wenn der native Pfad im Spike Probleme macht, da er dieselbe Winit-Hint-Semantik ("just a hint") erbt.

Wayland (KDE Plasma, wlroots-Compositoren, Hyprland)

- `zwlr_layer_shell_v1` über smithay-client-toolkit 0.21.1: Layer `overlay`, `keyboard_interactivity = none`, `set_exclusive_zone(0)`, Anker + Margins für die Ecke, leere Input-Region über `wl_surface().set_input_region(Some(&leere_region))` [Y1][Y2][C2]. Eigene `wayland-client`-Verbindung im Overlay-Thread, getrennt vom winit-Event-Loop des Hauptfensters (zwei Verbindungen zum Compositor sind protokollseitig normal).
- Buffer: `wl_shm` ARGB8888 über softbuffer, sofern Step 3 echte Transparenz zeigt; sonst wgpu-Surface mit `CompositeAlphaMode::PreMultiplied` [C9].
- Kein Overlay unter GNOME/Mutter (kein Layer-Shell [Y1][Y4]); der Client meldet "Overlay auf diesem Desktop nicht verfügbar" statt einen halbfunktionalen xdg_toplevel-Weg anzubieten.

X11

- Nicht in v1. Dokumentierter Fallback: override-redirect [X1] + ARGB-Visual + `ShapeInput` leer [X2] + `_NET_WM_STATE_ABOVE`. Wird erst gebaut, wenn ein X11-Nutzer es braucht.

Zusammenspiel mit dem GUI-Toolkit (siehe `gui-toolkit.md`): Das Hauptfenster (egui/eframe über winit) und das Overlay teilen keinen Event-Loop. Steuerbefehle (Overlay an/aus, Position, Kachelgröße, Reihenfolge) gehen als Messages an den Overlay-Thread; der Overlay-Thread liefert nichts zurück außer Fehlern. Damit bleibt das Toolkit frei austauschbar, und Layer-Shell-Code kommt nie mit winit in Berührung.

## Verworfen, mit Grund

- **DirectX/Vulkan/OpenGL-Hooking, DLL-Injection, LD_PRELOAD** (der Discord/Mumble-Weg): pro API eigener Renderer und Hook, Bitness-Varianten, Kompatibilitäts-Black-/Whitelists [M1], Windows-only bei Discord und ohne Vulkan [D1], Anti-Cheat-Risiko. Nicht-Ziel laut README.
- **Overlay über exklusivem Fullscreen (FSE) auf Windows**: Occlusion wirft das Spiel aus dem Fullscreen oder das Overlay bleibt unsichtbar [W5]. Durch FSO [W7] praktisch selten nötig.
- **DWM "sheet of glass" + D3D-Swapchain** (`DwmExtendFrameIntoClientArea`): Transparenzverhalten aus der Doku nicht belegt [W4]; `UpdateLayeredWindow` ist der vollständig dokumentierte Per-Pixel-Pfad [W2][W3].
- **winit-Fenster als Wayland-Overlay**: `set_window_level` auf Wayland "Unsupported" [C4], kein Layer-Shell [C5]; über einem Fullscreen-Spiel nicht haltbar.
- **xdg_toplevel als GNOME-Fallback**: gleiche Einschränkung; lieber ehrliche Nichtverfügbarkeit.
- **Bildschirmgroßes Layered Window auf Windows**: `UpdateLayeredWindow` aktualisiert immer das ganze Fenster und soll klein sein [W3].
- **X11 in v1**: Zielsystem ist Wayland; X11-Fallback nur dokumentiert.

## Offene Punkte für Step 3

Auf Chris' KDE-Wayland-Session (plasma-workspace 6.7.5, KWin mit Layer-Shell v5 laut [Y1]):

1. SCTK-Spike: Layer `overlay`, leere Input-Region, ARGB-Buffer mit Alpha 0 im Hintergrund und einer halbtransparenten Kachel; über einem `set_fullscreen`-xdg_toplevel (z. B. mpv/Spiel im Borderless) prüfen: sichtbar? Klicks und Tastatur gehen ans Spiel? Bleibt der Fokus beim Spiel?
2. Bleibt das Overlay über einem Fullscreen-Fenster sichtbar, wenn KWin Direct-Scanout aktiviert (Fullscreen ohne Compositing-Overhead)? Verhalten mit "Fenster im Vollbild dürfen Compositing blockieren" (KWin-Einstellung) testen.
3. softbuffer auf Wayland: liefert `Surface::buffer_mut` ARGB mit wirksamem Alpha, oder wird XRGB verwendet? Sonst wgpu mit `PreMultiplied`.
4. Multi-Monitor: Overlay auf dem Monitor des Spiels (Layer-Surface pro `wl_output`); Skalierung (fractional scale) der Kacheln.
5. Proton/Wine-Spiele unter XWayland: liegt das Layer-Surface auch über XWayland-Fullscreen-Fenstern?
6. CPU-Last der Komposition und Frame-Timing bei 4 Kacheln @ 15 fps.

Auf Windows (später):

7. Layered-Window-Spike mit `UpdateLayeredWindow`: über Borderless-Spiel (DX11/DX12/Vulkan) sichtbar und klick-durchlässig? Bleibt das Spiel im Independent-Flip (PresentMon prüfen [W6])?
8. Spiel mit FSE und FSO aktiv vs. FSO per Kompatibilitätseinstellung deaktiviert: Overlay sichtbar? Fliegt das Spiel aus dem Fullscreen?
9. Vergleich: egui-Viewport (`with_mouse_passthrough`) vs. nativer Pfad, Aufwand und Verhalten.
10. HDR-Desktop / Auto HDR: Farbdarstellung eines SDR-Layered-Windows.
11. Verhalten mit als Administrator laufenden Spielen (Topmost-Fenster einer Medium-Integrity-App darüber, dokumentiert in W11 der Hotkey-Recherche als "Painting to the screen is ... not blocked by UIPI").

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `verification.md`):
  - Spike 4b **bestätigt** auf KWin 6.7.5 (smithay-client-toolkit 0.19.2, wl_shm ARGB8888 premultiplied): Layer `overlay`, `keyboard_interactivity none`, leere Input-Region, Exclusive-Zone −1 → Overlay mit transparentem Loch sichtbar über allen Fenstern (Screenshot), kein Fokuswechsel. Punkt 3 (softbuffer-Alpha) ist damit umgangen: direkter `wl_shm`-Buffer mit Alpha funktioniert.
  - Falle: `blocking_dispatch` ohne Frame-Loop blockiert; Overlay-Thread braucht calloop/Timer.
  - Offen: Punkt 1 (über `set_fullscreen`-Toplevel), 2 (Direct-Scanout), 4–6, Windows 7–11.

Mit WebFetch tatsächlich gelesen (2026-09-20):

- https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles
- https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow
- https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmextendframeintoclientarea
- https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/d3d10-graphics-programming-guide-dxgi
- https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model
- https://devblogs.microsoft.com/directx/demystifying-full-screen-optimizations/
- https://support.microsoft.com/en-us/windows/optimizations-for-windowed-games-in-windows-11-3f006843-2c7e-4ed0-9a5e-f9389e535952
- https://xorg.freedesktop.org/archive/X11R7.7/doc/xproto/x11protocol.html
- https://xorg.freedesktop.org/archive/X11R7.7/doc/xextproto/shape.html
- https://wayland.app/protocols/wlr-layer-shell-unstable-v1
- https://docs.rs/wayland-client/latest/wayland_client/protocol/wl_surface/struct.WlSurface.html
- https://kde.org/announcements/changelogs/plasma/5/5.19.5-5.20.0/
- https://gitlab.gnome.org/api/v4/projects/GNOME%2Fmutter/issues/973
- https://docs.rs/smithay-client-toolkit/latest/smithay_client_toolkit/shell/wlr_layer/index.html
- https://docs.rs/smithay-client-toolkit/latest/smithay_client_toolkit/shell/wlr_layer/struct.LayerSurface.html
- https://docs.rs/winit/latest/winit/window/struct.WindowAttributes.html
- https://raw.githubusercontent.com/rust-windowing/winit/v0.30.13/src/window.rs
- https://docs.rs/winit/latest/winit/platform/wayland/index.html
- https://docs.rs/softbuffer/latest/softbuffer/
- https://raw.githubusercontent.com/rust-windowing/softbuffer/master/README.md
- https://docs.rs/wgpu/latest/wgpu/
- https://docs.rs/wgpu/latest/wgpu/enum.CompositeAlphaMode.html
- https://docs.rs/egui/latest/egui/viewport/struct.ViewportBuilder.html
- https://docs.discord.com/developers/developer-tools/game-sdk
- https://github.com/mumble-voip/mumble/tree/master/overlay
- https://github.com/mumble-voip/mumble/tree/master/overlay_gl
- https://raw.githubusercontent.com/mumble-voip/mumble/master/overlay_gl/init_unix.c
