# Globaler PTT-Hotkey (Wayland, X11, Windows)

## Frage

Wie bekommt der ICU-Client Press **und** Release einer frei wählbaren Taste oder Maus-Seitentaste, während ein Spiel den Fokus hat, auf Fedora 43 / KDE Plasma 6 (Wayland) und auf Windows 10/11, ohne die Taste für das Spiel zu blockieren und ohne Code in fremde Prozesse zu injizieren?

Push-to-Talk braucht zwingend beides: Aktivierung beim Drücken, Deaktivierung beim Loslassen. Ein reines "Hotkey gedrückt"-Signal reicht nicht.

## Kandidaten

Linux

- (a) XDG Desktop Portal `org.freedesktop.portal.GlobalShortcuts` (Crate: ashpd)
- (b) evdev direkt über `/dev/input/event*` (Crate: evdev)
- (c) libinput (Crate: input)
- (d) X11-Fallback über XWayland (XGrabKey, XInput2)
- (e) KGlobalAccel (KDE Frameworks, C++/Qt)
- (f) `hyprland_global_shortcuts_v1` (Wayland-Protokoll)
- Crate global-hotkey (Tauri)

Windows

- `RegisterHotKey` / `WM_HOTKEY`
- Low-Level Hooks `WH_KEYBOARD_LL` / `WH_MOUSE_LL` (`SetWindowsHookEx`)
- Raw Input (`RegisterRawInputDevices`, `WM_INPUT`, `RIDEV_INPUTSINK`)

## Primärquellen

Alle abgerufen am 2026-09-20.

Portal, Wayland, KDE, GNOME

- P1 Portal GlobalShortcuts, Interface-Doku: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html
- P2 freedesktop Shortcuts-Spezifikation (Trigger-Format): http://specifications.freedesktop.org/shortcuts/latest/
- P3 xdg-desktop-portal-kde, `src/globalshortcuts.cpp`: https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/raw/master/src/globalshortcuts.cpp
- P4 xdg-desktop-portal-kde, `src/xdgshortcut.cpp` (Trigger-Parser): https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/raw/master/src/xdgshortcut.cpp
- P5 KDE Plasma 5.27.0 Announcement: https://kde.org/announcements/plasma/5/5.27.0/
- P6 xdg-desktop-portal-gnome NEWS: https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/raw/main/NEWS
- P7 GNOME 48 Release Notes: https://release.gnome.org/48/
- P8 KGlobalAccel Framework-Index: https://api.kde.org/kglobalaccel-index.html
- P9 libinput "What is libinput": https://wayland.freedesktop.org/libinput/doc/latest/what-is-libinput.html
- P10 wayland.app hyprland-global-shortcuts-v1 (Compositor-Support-Tabelle): https://wayland.app/protocols/hyprland-global-shortcuts-v1
- P11 wayland-client, `wl_keyboard::Event` (enter/leave/key): https://docs.rs/wayland-client/latest/wayland_client/protocol/wl_keyboard/enum.Event.html
- P12 systemd `50-udev-default.rules.in`: https://raw.githubusercontent.com/systemd/systemd/main/rules.d/50-udev-default.rules.in

Rust-Crates (API-Form aus docs.rs; Version/Datum/Lizenz ausschließlich aus der crates.io-API per `curl https://crates.io/api/v1/crates/<name>`)

- C1 ashpd `global_shortcuts`-Modul: https://docs.rs/ashpd/latest/ashpd/desktop/global_shortcuts/index.html
- C2 ashpd `GlobalShortcuts`-Struct: https://docs.rs/ashpd/latest/ashpd/desktop/global_shortcuts/struct.GlobalShortcuts.html
- C3 evdev Crate-Root: https://docs.rs/evdev/latest/evdev/
- C4 evdev `Device`: https://docs.rs/evdev/latest/evdev/struct.Device.html
- C5 global-hotkey Crate-Root: https://docs.rs/global-hotkey/latest/global_hotkey/
- C6 global-hotkey `HotKeyState`: https://docs.rs/global-hotkey/latest/global_hotkey/enum.HotKeyState.html
- C7 global-hotkey README (dev-Branch): https://raw.githubusercontent.com/tauri-apps/global-hotkey/dev/README.md
- C8 global-hotkey Windows-Implementierung (dev-Branch, nicht der 0.8.0-Tag): https://raw.githubusercontent.com/tauri-apps/global-hotkey/dev/src/platform_impl/windows/mod.rs

Windows (Microsoft Learn)

- W1 RegisterHotKey: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey
- W2 WM_HOTKEY: https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-hotkey
- W3 SetWindowsHookExW: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowshookexw
- W4 LowLevelKeyboardProc: https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc
- W5 LowLevelMouseProc: https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelmouseproc
- W6 Raw Input Overview: https://learn.microsoft.com/en-us/windows/win32/inputdev/about-raw-input
- W7 RAWINPUTDEVICE (Flags): https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawinputdevice
- W8 RAWKEYBOARD: https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawkeyboard
- W9 RAWMOUSE: https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawmouse
- W10 ChangeWindowMessageFilter (UIPI): https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-changewindowmessagefilter
- W11 Windows Integrity Mechanism Design (archiviert, 2007): https://learn.microsoft.com/en-us/previous-versions/dotnet/articles/bb625963(v=msdn.10)

Nicht abrufbar (nichts daraus zitiert): `https://api.kde.org/frameworks/kglobalaccel/html/index.html` und `.../classKGlobalAccel.html` (404), `https://specifications.freedesktop.org/shortcuts-spec/latest/` (Redirect auf P2).

## Befunde

### (a) Portal GlobalShortcuts

- Interface-Beschreibung: "This portal lets applications create global shortcuts sessions, and register shortcuts to them. These shortcuts are activated regardless of the focused state of the application window." Dokumentiert ist Version 2; `ConfigureShortcuts` "was added in version 2". [P1]
- Methoden: `CreateSession`, `BindShortcuts`, `ListShortcuts`, `ConfigureShortcuts` (v2). Signale: `Activated(session_handle, shortcut_id, timestamp, options)`, `Deactivated(...)` ("Notifies that a shortcut is not active anymore"), `ShortcutsChanged`. Press und Release sind also im Protokoll vorgesehen. [P1]
- `preferred_trigger`: "The preferred shortcut trigger, defined as described by the shortcuts XDG specification. Optional." Der Nutzer kann die Belegung im Portal-Dialog ändern; das Programm bekommt nur die Aktivierung. [P1]
- Shortcuts-Spec: Modifier "CTRL, ALT, SHIFT, NUM and LOGO", Key-Identifier aus `xkbcommon-keysyms.h`. Explizit: "This document contemplates simple keyboard shortcuts right now". **Maustasten sind nicht darstellbar.** [P2]
- KDE-Implementierung: `version()` liefert 2; emittiert `Activated` **und** `Deactivated`, gespeist aus `globalShortcutPressed` / `globalShortcutReleased` des KGlobalAccel-Component-Interface; Registrierung über `KGlobalAccel::self()->setShortcut(...)`. [P3]
- KDE-Trigger-Parser `XdgShortcut::parse`: Modifier `SHIFT, CAPS, CTRL, ALT, NUM, LOGO`, Keys per `xkb_keysym_from_name(...)`; kein Code für Maustasten. [P4]
- KDE Plasma 5.27: "Plasma on Wayland has also gained support for the Global Shortcuts portal." [P5]
- GNOME: xdg-desktop-portal-gnome NEWS 48.rc: "Add global shortcuts portal backend". [P6] GNOME 48 Release Notes: "apps can now create their own system-wide shortcuts". [P7] Ob das GNOME-Backend `Deactivated` liefert: **unbelegt** (nicht geprüft).
- Lokal beobachtet (2026-09-20, read-only, **korrigiert in Step 3**): `busctl --user get-property ... org.freedesktop.portal.GlobalShortcuts version` → `u 1` (nicht 2); Pakete `xdg-desktop-portal-1.20.4`, `xdg-desktop-portal-kde-6.7.5`, `plasma-workspace-6.7.5`; `XDG_SESSION_TYPE=wayland`.
- Ob KWin die gebundene Taste zusätzlich an das fokussierte Spiel durchreicht oder exklusiv konsumiert: **unbelegt** (KGlobalAccel-Semantik nicht dokumentiert gefunden) → Step 3.

### ashpd (Rust)

- ashpd 0.13.13, MIT, veröffentlicht 2026-07-17 (crates.io-API). Modul `desktop::global_shortcuts` hinter Feature `global_shortcuts`; Typen `GlobalShortcuts`, `NewShortcut`, `Activated`, `Deactivated`, `ShortcutsChanged`. [C1]
- Methoden: `create_session`, `bind_shortcuts(session, &[NewShortcut], Option<&WindowIdentifier>, options)`, `list_shortcuts`, `configure_shortcuts`, `receive_activated() -> Stream<Activated>`, `receive_deactivated() -> Stream<Deactivated>`, `version()`. [C2]

### (b) evdev

- evdev 0.13.2, Apache-2.0 OR MIT, veröffentlicht 2025-09-15 (crates.io-API). "Available on Unix only"; liest `/dev/input/eventX`, Key-Events mit press/release/repeat, optionales `tokio`-Feature (`EventStream`). [C3]
- `Device::open(path)`, `fetch_events()` ("Fetches and returns events from the kernel ring buffer, doing synchronization on SYN_DROPPED"), `grab()` ("prevents other clients ... from receiving events from this device"; für PTT **nicht** verwenden, sonst sieht das Spiel die Taste nicht), `supported_keys()`, `get_key_state()`, `into_event_stream()`. [C4]
- Rechte: systemd-Default-Regel `SUBSYSTEM=="input", GROUP="input"`. [P12] Lokal beobachtet: `/dev/input/event0` ist `root:input 0660`, Gruppe `input` hat Mitglied `chris`. Annahme (**unbelegt**): Auf einer Fedora-Standardinstallation ist der Nutzer nicht in `input`; dann wäre `usermod -aG input <user>` plus Re-Login nötig, oder eine udev-Regel mit `TAG+="uaccess"` für ausgewählte Geräte (Regel-Form aus Kenntnis der udev-Konventionen, ebenfalls **unbelegt**).
- Sicherheitsaspekt: Mitgliedschaft in `input` gibt **jedem** Prozess dieses Nutzers Lesezugriff auf alle Tastatur- und Mausereignisse (Keylogger-Fähigkeit), nicht nur ICU. Das ist der Preis für Compositor-Unabhängigkeit und Maustasten.
- Funktioniert unabhängig vom Compositor (kernelseitig), liefert Press und Release; Maus-Seitentasten sind normale Key-Events (Codes `BTN_SIDE`, `BTN_EXTRA` aus `linux/input-event-codes.h` — Kenntnis, **unbelegt** aus den gelesenen Quellen). Keyboard-Layout-Mapping ist Sache des Clients (Keycodes, keine Keysyms).

### (c) libinput

- "libinput is an input stack for processes that need to provide events from commonly used input devices"; Nutzer sind Compositoren (Weston, mutter, KWin) und der X-Server; "Wayland client (your application) does not usually talk directly to the compositor". [P9] Braucht dieselben `/dev/input`-Rechte wie evdev, bringt für einen Hotkey-Listener keinen Mehrwert. Crate `input` 0.10.0, MIT, 2026-04-05 (crates.io-API).

### (d) X11 / XWayland

- Wayland-Tastaturfokus: `wl_keyboard` liefert `enter` ("keyboard focus is on a certain surface"), `leave`, und `key` ("A key was pressed or released"); Key-Events kommen nur bei der fokussierten Surface an. [P11] XWayland ist ein Wayland-Client; daraus folgt (Inferenzkette, kein direktes Zitat): ein X11-Grab über XWayland sieht nur Tasten, solange ein XWayland-Fenster den Fokus hat. Für Wayland-native Spiele ist der X11-Weg damit tot. Ob Proton/Wine-Spiele auf Chris' Box unter XWayland oder nativem Wayland laufen: **unbelegt** → Step 3.

### (e) KGlobalAccel

- "The KGlobalAccel API provides a convenient way to register global shortcuts." C++/Qt-Framework. [P8] Das KDE-Portal-Backend ist eine dünne Schicht darüber [P3]; direkter Zugriff aus Rust hieße C++-Bindings oder das D-Bus-Interface von `kglobalaccel` nachbauen, nur für KDE. Kein Vorteil gegenüber dem Portal.

### (f) hyprland_global_shortcuts_v1

- Support-Tabelle auf wayland.app: nur Hyprland (v1); KWin, Mutter, Sway, wlroots-Compositoren u. a. mit "x" (nicht unterstützt). [P10] Für KDE irrelevant.

### global-hotkey (Tauri)

- global-hotkey 0.8.0, Apache-2.0 OR MIT, 2026-05-01 (crates.io-API). Plattformen laut README: "Windows", "macOS", "Linux (X11 Only)". [C7] `HotKeyState::{Pressed, Released}` existiert. [C6]
- Windows-Implementierung (dev-Branch): `RegisterHotKey` + nach `WM_HOTKEY` ein Thread, der alle 50 ms `GetAsyncKeyState` pollt und bei 0 `Released` sendet. [C8] Release ist also ein Polling-Hack mit bis zu 50 ms Verzögerung, nicht ereignisbasiert.

### Windows: RegisterHotKey

- "Defines a system-wide hot key." `WM_HOTKEY` wird beim Drücken gepostet ("When a key is pressed, the system looks for a match against all hot keys"). `MOD_NOREPEAT` unterdrückt Auto-Repeat. Fehlschlag, wenn die Kombination bereits registriert ist. Die Doku spricht nur von Tasten ("When a key is pressed"); ob `VK_XBUTTON1/2` als `vk` akzeptiert werden, ist **unbelegt** (nur relevant, falls RegisterHotKey je als Fallback nötig wird). [W1]
- `WM_HOTKEY`: "Posted when the user presses a hot key". Weder W1 noch W2 nennen ein Release-Ereignis. **Doku nennt kein Release-Event**; global-hotkey umgeht das per Polling [C8], was das bestätigt.
- Ob der Tastendruck zusätzlich beim Vordergrundfenster (Spiel) ankommt oder vom System konsumiert wird: in W1/W2 nicht beschrieben, **unbelegt** → Step 3.

### Windows: WH_KEYBOARD_LL / WH_MOUSE_LL

- "WH_KEYBOARD_LL" und "WH_MOUSE_LL" sind "Global only". Der LL-Hook "is not injected into another process. Instead, the context switches back to the process that installed the hook"; der installierende Thread "must have a message loop". [W3][W4]
- Keyboard: wParam ist `WM_KEYDOWN`, `WM_KEYUP`, `WM_SYSKEYDOWN` oder `WM_SYSKEYUP` → Press und Release. Rückgabe ungleich 0 schluckt die Taste. [W4]
- Maus: wParam u. a. `WM_XBUTTONDOWN` / `WM_XBUTTONUP` → Seitentasten mit Press und Release. [W5]
- Timeout: `LowLevelHooksTimeout` (max. 1000 ms ab Win10 1709); "on Windows 7 and later, the hook is silently removed without being called. There is no way for the application to know whether the hook is removed." [W4][W5]
- Microsofts eigene Empfehlung: "In most cases where the application needs to use low level hooks, it should monitor raw input instead. This is because raw input can asynchronously monitor mouse and keyboard messages that are targeted for other threads more effectively than low level hooks can." [W4][W5]

### Windows: Raw Input

- Registrierung per `RegisterRawInputDevices` mit `RAWINPUTDEVICE` (Keyboard: UsagePage 0x01 / Usage 0x06); Empfang über `WM_INPUT`. "An application can receive data when it is in the foreground and when it is in the background (if registered with RIDEV_INPUTSINK)." [W6]
- `RIDEV_INPUTSINK`: "enables the caller to receive the input even when the caller is not in the foreground. Note that hwndTarget must be specified." `RIDEV_NOLEGACY` unterdrückt nur die eigenen Legacy-Messages; Raw Input entfernt Eingaben **nicht** aus der normalen Verarbeitung (kein Schlucken möglich; für PTT erwünscht). [W7]
- `RAWKEYBOARD.Flags`: `RI_KEY_MAKE` (0, "The key is down"), `RI_KEY_BREAK` (1, "The key is up"), plus `MakeCode`, `VKey`, `Message`. → Press und Release. [W8]
- `RAWMOUSE.usButtonFlags`: `RI_MOUSE_BUTTON_4_DOWN/UP` ("XBUTTON1 changed to down/up"), `RI_MOUSE_BUTTON_5_DOWN/UP` (XBUTTON2). → Seitentasten mit Press und Release. [W9]

### Windows: Spiel im Vordergrund, Spiel mit Admin-Rechten (UIPI)

- UIPI: "prevents lower-privilege applications from sending window messages or installing hooks in higher-privilege processes"; ein Prozess niedrigerer Integrität kann nicht "Use thread hooks to attach to a process running with higher rights" und nicht "Perform dynamic link library (DLL) injection". [W11] Nachrichten über `WM_USER` werden von niedrigerer Integrität standardmäßig blockiert. [W10]
- Nur Anwendungen mit `UIAccess="true"` im Manifest (signiert, aus Program Files) haben "read input for all integrity levels using low-level hooks, raw input, GetKeyState, GetAsyncKeyState". [W11]
- **Inferenz** (nicht wörtlich belegt): Ein normaler (Medium-Integrity-)ICU-Client sieht per LL-Hook oder Raw Input keine Eingaben, solange ein als Administrator gestartetes Spiel den Fokus hat. Betroffen wären Spiele, die tatsächlich elevated laufen (Launcher mit UAC-Prompt). Ob `RegisterHotKey` in dieser Situation feuert: **unbelegt** → Step 3.
- Spiel im Vordergrund ohne Elevation: Raw Input mit `RIDEV_INPUTSINK` ist genau für diesen Fall dokumentiert [W6][W7]; LL-Hooks ebenfalls (global). Anti-Cheat-Verhalten gegenüber LL-Hooks oder Raw-Input-Sinks: **unbelegt**.

## Entscheidung

Linux (KDE Plasma Wayland, primäres Ziel)

1. **Primär: Portal GlobalShortcuts über ashpd 0.13.13** für Tastatur-PTT. Begründung: liefert `Activated` und `Deactivated` [P1], KDE emittiert beides [P3], Version 2 lokal vorhanden, der Nutzer sieht und ändert die Belegung in den Systemeinstellungen, keine Sonderrechte, keine Eingriffe in `/dev/input`, funktioniert auch unter GNOME 48+ [P6] und in einem späteren Flatpak.
2. **Zweites Backend, opt-in: evdev 0.13.2** für Maus-Seitentasten und für Compositoren ohne GlobalShortcuts-Backend. Ohne `grab()`. Aktivierung erst nach expliziter Wahl in den Settings; die UI erklärt die `input`-Gruppe und den Keylogger-Aspekt, ICU ändert keine Gruppen oder udev-Regeln selbst. Begründung: Portal und Spec können keine Maustasten [P2][P4]; evdev kann es kernelseitig und compositor-unabhängig [C3][C4].
3. Backend-Auswahl zur Laufzeit: Portal verfügbar (`version() >= 1` und `bind_shortcuts` erfolgreich) → Portal; sonst evdev, falls `/dev/input` lesbar; sonst Fehlermeldung mit Anleitung.

Windows

- **Raw Input** (`RegisterRawInputDevices` mit `RIDEV_INPUTSINK` für Keyboard- und Maus-TLC, eigener versteckter Message-Only-Thread mit `hwndTarget`). Press/Release aus `RI_KEY_MAKE/BREAK` [W8] und `RI_MOUSE_BUTTON_4/5_DOWN/UP` [W9]; Hintergrundempfang dokumentiert [W6][W7]; keine Hook-Timeouts, keine stille Entfernung, kein Prozess-übergreifender Hook, kein Schlucken der Taste (Spiel sieht sie weiterhin). Microsoft empfiehlt Raw Input explizit gegenüber LL-Hooks [W4][W5].

Gemeinsames Interface: `trait PttSource { fn events() -> Stream<PttEvent::{Pressed, Released}> }`, Backends `PortalBackend`, `EvdevBackend`, `RawInputBackend`. Das UI zeigt an, welches Backend aktiv ist.

## Verworfen, mit Grund

- **RegisterHotKey**: kein Release-Ereignis in der Doku [W1][W2]; global-hotkey muss per 50-ms-Polling raten [C8]; Maustasten in der Doku nicht vorgesehen (unbelegt, ob `VK_XBUTTON1/2` funktionieren); schlägt bei belegter Kombination fehl [W1].
- **WH_KEYBOARD_LL / WH_MOUSE_LL**: funktional ausreichend (Press/Release, XBUTTON), aber stille Entfernung nach Timeout ohne Benachrichtigung [W4][W5], Message-Loop-Kopplung, und Microsoft rät selbst zu Raw Input [W4]. Bleibt als dokumentierter Notfall-Fallback, falls Raw Input in Step 3 an einem konkreten Spiel scheitert.
- **global-hotkey (Tauri)**: "Linux (X11 Only)" [C7] → auf Wayland nutzlos; Windows-Release per Polling [C8].
- **X11 / XWayland**: sieht nur Eingaben bei XWayland-Fokus (Inferenz aus [P11]); Wayland-native Spiele unsichtbar.
- **KGlobalAccel direkt**: C++/Qt, KDE-only [P8]; das Portal ist genau die Abstraktion darüber [P3].
- **libinput / Crate input**: für Compositoren gedacht [P9], gleiche Rechte wie evdev, mehr Abhängigkeiten.
- **hyprland_global_shortcuts_v1**: nur Hyprland [P10].

## Offene Punkte für Step 3

Auf Chris' KDE-Wayland-Session (bereits beobachtet 2026-09-20: Portal-Version 1, xdg-desktop-portal-kde 6.7.5, chris in Gruppe `input`, `/dev/input/event0` root:input 0660):

1. ashpd-Spike: `bind_shortcuts` mit `preferred_trigger` (z. B. `CTRL+F13` oder eine Einzeltaste), dann Latenz und Zuverlässigkeit von `Deactivated` messen (Halten 5 s, schnelles Tippen, Auto-Repeat).
2. Konsumiert KWin die gebundene Taste oder reicht er sie an das fokussierte Fenster durch? Test mit einem Texteditor und einem Spiel im Fokus. Wenn konsumiert: für PTT akzeptabel, aber im UI dokumentieren.
3. Portal-Dialog-Verhalten: erscheint bei jedem Start ein Bestätigungsdialog, oder wird die Session persistiert (Session-Handle / `ListShortcuts`)?
4. evdev-Spike: Maus-Seitentaste (`BTN_SIDE`/`BTN_EXTRA`) ohne `grab()` lesen, prüfen, dass das Spiel die Taste weiterhin bekommt; Verhalten bei Hotplug (Device-Enumeration neu).
5. Laufen die relevanten Spiele (Proton/Wine) unter XWayland oder Wayland-nativ (`xprop` / `kwin --debug` / Fensterinfo)? Bestimmt, ob der Portal-Weg auch unter Proton den Fokus-Fall abdeckt (er sollte, da KWin die Taste kernelseitig vor dem Client sieht — **unbelegt**).
6. Verhalten unter GNOME 48 (`Deactivated` vorhanden?) auf einer Test-VM, falls GNOME-Support gewünscht.

Auf Windows (später):

7. Raw-Input-Spike: `RIDEV_INPUTSINK` für Keyboard + Maus; Press/Release-Latenz; Spiel im Borderless-Fullscreen im Vordergrund.
8. Als Administrator gestartetes Spiel im Fokus: kommt Raw Input an? kommt `WM_HOTKEY` an? (UIPI-Inferenz prüfen.)
9. Konsumiert `RegisterHotKey` die Taste für das Vordergrundfenster? (Nur relevant, falls RegisterHotKey doch als Fallback nötig wird.)
10. Verhalten mit verbreiteten Anti-Cheat-Lösungen gegenüber Raw-Input-Sinks (Beobachtung, keine Doku erwartet).

## Verified against

- [x] Step 3 am 2026-09-20 (Details in `verification.md`):
  - Spike 4a **bestätigt** evdev: alle 21 `/dev/input/event*` als uid 1000 (Gruppe `input`) lesbar, Tastatur und Maus mit `BTN_SIDE` erkannt, kein root nötig.
  - Spike 4a **offen** für das Portal: `CreateSession` ok, `BindShortcuts` (ashpd 0.11.1, `preferred_trigger CTRL+SHIFT+F12`) kam in der unbeaufsichtigten Session sofort mit `Cancelled` zurück. Punkt 1–3 der Step-3-Liste müssen interaktiv von Chris ausgeführt werden (Quelle: `Research/spikes/spike-wayland/`, `cargo run --bin hotkey`).
  - **Korrektur**: die lokale Portal-Version ist **1**, nicht 2 (`busctl --user get-property … GlobalShortcuts version` → `u 1`, geprüft 2026-09-20). `ConfigureShortcuts` steht damit lokal nicht zur Verfügung; für PTT irrelevant.
  - Entscheidung bleibt: Portal primär, evdev opt-in; die Backend-Auswahl zur Laufzeit muss `Cancelled` als „Portal nicht nutzbar“ werten und auf evdev fallen, wenn `/dev/input` lesbar ist.

Mit WebFetch tatsächlich gelesen (2026-09-20):

- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html
- http://specifications.freedesktop.org/shortcuts/latest/
- https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/raw/master/src/globalshortcuts.cpp
- https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/raw/master/src/xdgshortcut.cpp
- https://kde.org/announcements/plasma/5/5.27.0/
- https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/raw/main/NEWS
- https://release.gnome.org/48/
- https://api.kde.org/kglobalaccel-index.html
- https://wayland.freedesktop.org/libinput/doc/latest/what-is-libinput.html
- https://wayland.app/protocols/hyprland-global-shortcuts-v1
- https://docs.rs/wayland-client/latest/wayland_client/protocol/wl_keyboard/enum.Event.html
- https://raw.githubusercontent.com/systemd/systemd/main/rules.d/50-udev-default.rules.in
- https://docs.rs/ashpd/latest/ashpd/desktop/global_shortcuts/index.html
- https://docs.rs/ashpd/latest/ashpd/desktop/global_shortcuts/struct.GlobalShortcuts.html
- https://docs.rs/evdev/latest/evdev/
- https://docs.rs/evdev/latest/evdev/struct.Device.html
- https://docs.rs/global-hotkey/latest/global_hotkey/
- https://docs.rs/global-hotkey/latest/global_hotkey/enum.HotKeyState.html
- https://raw.githubusercontent.com/tauri-apps/global-hotkey/dev/README.md
- https://raw.githubusercontent.com/tauri-apps/global-hotkey/dev/src/platform_impl/windows/mod.rs
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey
- https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-hotkey
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowshookexw
- https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc
- https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelmouseproc
- https://learn.microsoft.com/en-us/windows/win32/inputdev/about-raw-input
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawinputdevice
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawkeyboard
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawmouse
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-changewindowmessagefilter
- https://learn.microsoft.com/en-us/previous-versions/dotnet/articles/bb625963(v=msdn.10)
