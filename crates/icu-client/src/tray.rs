//! Tray icon (tray-icon 0.25, ksni on Linux): status icon, menu with
//! Fenster anzeigen/Stumm/Taub/Beenden, in the words the main window uses.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Once};
#[cfg(target_os = "linux")]
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem},
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

use crate::engine::{AppState, Command, ConnStatus};

/// Events the window must act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// Show and focus the main window.
    ShowWindow,
    /// Quit the application.
    Quit,
}

// The native libraries install their global handlers only once. Route those
// handlers to the current tray so logout/rejoin cannot leave them connected to
// the dropped window's channels and wake callback.
static ACTIVE_EVENTS: Mutex<Option<Arc<EventTargets>>> = Mutex::new(None);
static INSTALL_HANDLERS: Once = Once::new();

struct EventTargets {
    menu: mpsc::Sender<MenuId>,
    tray: mpsc::Sender<TrayIconEvent>,
    wake: Box<dyn Fn() + Send + Sync>,
}

struct EventRegistration(Arc<EventTargets>);

impl EventRegistration {
    fn new(
        menu: mpsc::Sender<MenuId>,
        tray: mpsc::Sender<TrayIconEvent>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let targets = Arc::new(EventTargets {
            menu,
            tray,
            wake: Box::new(wake),
        });
        *ACTIVE_EVENTS.lock().unwrap() = Some(targets.clone());
        INSTALL_HANDLERS.call_once(|| {
            MenuEvent::set_event_handler(Some(dispatch_menu));
            TrayIconEvent::set_event_handler(Some(dispatch_tray));
        });
        Self(targets)
    }
}

impl Drop for EventRegistration {
    fn drop(&mut self) {
        let mut active = ACTIVE_EVENTS.lock().unwrap();
        if active.as_ref().is_some_and(|a| Arc::ptr_eq(a, &self.0)) {
            *active = None;
        }
    }
}

fn dispatch_menu(event: MenuEvent) {
    let active = ACTIVE_EVENTS.lock().unwrap().clone();
    if let Some(active) = active {
        let _ = active.menu.send(event.id);
        (active.wake)();
    }
}

/// Whether a click on the icon itself shows the window. On Windows a double
/// click, as usual there (a single one does nothing, the right one opens the
/// menu). Elsewhere a click: the StatusNotifier backend (ksni) reports the
/// host's `Activate` as `Click` and never a double click, so waiting for one
/// left the icon dead on Linux.
fn shows_window(event: &TrayIconEvent) -> bool {
    if cfg!(windows) {
        matches!(
            event,
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        )
    } else {
        matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        )
    }
}

fn dispatch_tray(event: TrayIconEvent) {
    if !shows_window(&event) {
        return;
    }
    let active = ACTIVE_EVENTS.lock().unwrap().clone();
    if let Some(active) = active {
        let _ = active.tray.send(event);
        (active.wake)();
    }
}

/// The tray.
pub struct Tray {
    icon: TrayIcon,
    mute: CheckMenuItem,
    deafen: CheckMenuItem,
    show_id: MenuId,
    quit_id: MenuId,
    cmd: UnboundedSender<Command>,
    _events: EventRegistration,
    menu_rx: mpsc::Receiver<MenuId>,
    tray_rx: mpsc::Receiver<TrayIconEvent>,
    /// What icon, tooltip and menu were last drawn as; anything else in the
    /// state is not shown here.
    last: Option<Look>,
    /// A StatusNotifier host shows the icon (`host_present`), refreshed by
    /// a watch thread: plasmashell restarts, a GNOME extension comes later.
    host: Arc<AtomicBool>,
}

impl Tray {
    /// Creates the tray icon with its menu.
    pub fn start(
        cmd: UnboundedSender<Command>,
        ctx: egui::Context,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Tray, String> {
        let menu = Menu::new();
        let show = MenuItem::new("Fenster anzeigen", true, None);
        let mute = CheckMenuItem::new("Stumm", true, false, None);
        let deafen = CheckMenuItem::new("Taub", true, false, None);
        let quit = MenuItem::new("Beenden", true, None);
        menu.append(&show).map_err(|e| e.to_string())?;
        menu.append(&mute).map_err(|e| e.to_string())?;
        menu.append(&deafen).map_err(|e| e.to_string())?;
        menu.append(&quit).map_err(|e| e.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_id("icu")
            .with_menu(Box::new(menu))
            // On Windows a left-click menu competes with double-click
            // restore and can keep the restored window in the background.
            .with_menu_on_left_click(!cfg!(windows))
            .with_tooltip("ICU")
            .with_title("ICU")
            .with_icon(render_icon([0x9e, 0x9e, 0x9e], false))
            .build()
            .map_err(|e| e.to_string())?;
        let (menu_tx, menu_rx) = mpsc::channel();
        let (tray_tx, tray_rx) = mpsc::channel();
        let events = EventRegistration::new(menu_tx, tray_tx, wake);
        let host = Arc::new(AtomicBool::new(host_present()));
        watch_host(host.clone(), ctx);
        Ok(Tray {
            icon,
            show_id: show.id().clone(),
            quit_id: quit.id().clone(),
            mute,
            deafen,
            cmd,
            _events: events,
            menu_rx,
            tray_rx,
            last: None,
            host,
        })
    }

    /// Whether a panel shows the icon right now; without one, hiding the
    /// window would leave no way back to it.
    pub fn host_available(&self) -> bool {
        self.host.load(Ordering::Relaxed)
    }

    /// Drains pending events; menu actions are turned into commands here.
    pub fn poll(&self) -> Option<TrayEvent> {
        let mut out = None;
        while let Ok(id) = self.menu_rx.try_recv() {
            if id == self.show_id {
                out = Some(TrayEvent::ShowWindow);
            } else if id == self.quit_id {
                out = Some(TrayEvent::Quit);
            } else if id == *self.mute.id() {
                let _ = self.cmd.send(Command::SetMuted(self.mute.is_checked()));
            } else if id == *self.deafen.id() {
                let _ = self
                    .cmd
                    .send(Command::SetDeafened(self.deafen.is_checked()));
            }
        }
        while let Ok(ev) = self.tray_rx.try_recv() {
            if shows_window(&ev) {
                out = Some(TrayEvent::ShowWindow);
            }
        }
        out
    }

    /// Updates icon and check marks from the state.
    pub fn update(&mut self, state: &AppState) {
        let look = Look::of(
            &state.conn,
            state.muted || state.privacy_active,
            state.deafened,
        );
        if self.last.as_ref() == Some(&look) {
            return;
        }
        self.mute.set_checked(look.muted);
        self.deafen.set_checked(look.deafened);
        let _ = self
            .icon
            .set_icon(Some(render_icon(look.color, look.muted || look.deafened)));
        let _ = self.icon.set_tooltip(Some(look.tooltip.clone()));
        self.last = Some(look);
    }
}

/// Everything the tray shows. Compared as a whole before redrawing: a key of
/// only `connected` kept „verbinde…“ in the tooltip after the connection
/// failed for good.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Look {
    color: [u8; 3],
    tooltip: String,
    muted: bool,
    deafened: bool,
}

impl Look {
    fn of(conn: &ConnStatus, muted: bool, deafened: bool) -> Look {
        let color = if *conn != ConnStatus::Connected {
            [0x9e, 0x9e, 0x9e]
        } else if deafened {
            [0xf4, 0x43, 0x36]
        } else if muted {
            [0xff, 0x98, 0x00]
        } else {
            [0x4c, 0xaf, 0x50]
        };
        let tooltip = format!(
            "ICU — {}{}",
            match conn {
                ConnStatus::Connected => "verbunden",
                ConnStatus::Connecting => "verbinde…",
                ConnStatus::Disconnected { .. } => "getrennt",
                ConnStatus::Fatal(_) => "Fehler",
            },
            if deafened {
                ", taub"
            } else if muted {
                ", stumm"
            } else {
                ""
            }
        );
        Look {
            color,
            tooltip,
            muted,
            deafened,
        }
    }
}

/// Whether a StatusNotifier host (KDE's panel, GNOME's AppIndicator
/// extension, …) is registered: without one the ksni icon exists but
/// nobody draws it. Asks the session bus, at most ~300 ms; any error is
/// „no“. Not Linux: the shell always has a notification area.
pub fn host_present() -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return false;
        };
        rt.block_on(async {
            tokio::time::timeout(Duration::from_millis(300), async {
                let conn = zbus::Connection::session().await.ok()?;
                query_host(&conn).await
            })
            .await
            .ok()
            .flatten()
            .unwrap_or(false)
        })
    }
    #[cfg(not(target_os = "linux"))]
    true
}

#[cfg(target_os = "linux")]
async fn query_host(conn: &zbus::Connection) -> Option<bool> {
    let proxy = zbus::Proxy::new(
        conn,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
    )
    .await
    .ok()?;
    proxy
        .get_property::<bool>("IsStatusNotifierHostRegistered")
        .await
        .ok()
}

/// Refreshes `host` every 5 s and wakes the window when it flips.
fn watch_host(host: Arc<AtomicBool>, ctx: egui::Context) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::thread::Builder::new()
            .name("icu-tray-host".into())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                rt.block_on(async {
                    let mut conn = None;
                    loop {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        if conn.is_none() {
                            conn = zbus::Connection::session().await.ok();
                        }
                        let now = match &conn {
                            Some(c) => tokio::time::timeout(Duration::from_secs(1), query_host(c))
                                .await
                                .ok()
                                .flatten()
                                .unwrap_or(false),
                            None => false,
                        };
                        if host.swap(now, Ordering::Relaxed) != now {
                            tracing::info!(host = now, "tray host changed");
                            ctx.request_repaint();
                        }
                    }
                });
            });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (host, ctx);
}

/// A 32×32 filled circle, optionally with a diagonal bar.
fn render_icon(rgb: [u8; 3], bar: bool) -> Icon {
    let n = 32usize;
    let mut rgba = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let inside = dx * dx + dy * dy <= 14.0 * 14.0;
            let on_bar = bar && (x as i32 - y as i32).abs() <= 2;
            let i = (y * n + x) * 4;
            if inside {
                let c = if on_bar { [0xff, 0xff, 0xff] } else { rgb };
                rgba[i..i + 3].copy_from_slice(&c);
                rgba[i + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, n as u32, n as u32).expect("icon")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A left click on the icon shows the window on Linux (ksni knows only
    /// `Click`); on Windows the double click does, and a single click not.
    #[test]
    fn a_click_on_the_icon_shows_the_window() {
        use tray_icon::{dpi::PhysicalPosition, Rect, TrayIconId};
        let click = |button, button_state| TrayIconEvent::Click {
            id: TrayIconId::new("icu"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
            button_state,
        };
        let double = |button| TrayIconEvent::DoubleClick {
            id: TrayIconId::new("icu"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
        };
        let left_up = click(MouseButton::Left, MouseButtonState::Up);
        assert_eq!(shows_window(&left_up), !cfg!(windows));
        assert_eq!(shows_window(&double(MouseButton::Left)), cfg!(windows));
        assert!(!shows_window(&click(
            MouseButton::Left,
            MouseButtonState::Down
        )));
        assert!(!shows_window(&click(
            MouseButton::Right,
            MouseButtonState::Up
        )));
        assert!(!shows_window(&click(
            MouseButton::Middle,
            MouseButtonState::Up
        )));
        assert!(!shows_window(&double(MouseButton::Right)));
    }

    /// A connection attempt that ends for good changes the tooltip from
    /// „verbinde…“ to „Fehler“, although the icon stays grey.
    #[test]
    fn a_fatal_error_reaches_the_tooltip() {
        let connecting = Look::of(&ConnStatus::Connecting, false, false);
        let fatal = Look::of(
            &ConnStatus::Fatal(crate::problem::Problem::plain("weg")),
            false,
            false,
        );
        assert_eq!(connecting.color, fatal.color);
        assert_ne!(connecting, fatal, "redrawn");
        assert_eq!(fatal.tooltip, "ICU — Fehler");
        assert_eq!(
            Look::of(&ConnStatus::Connected, true, false).tooltip,
            "ICU — verbunden, stumm"
        );
    }

    #[test]
    fn native_handlers_follow_the_replacement_tray() {
        let (first_tx, first_rx) = mpsc::channel();
        let (first_tray, _) = mpsc::channel();
        let first = EventRegistration::new(first_tx, first_tray, || {});
        dispatch_menu(MenuEvent {
            id: MenuId::new("first"),
        });
        assert_eq!(first_rx.try_recv().unwrap(), "first");

        let (next_tx, next_rx) = mpsc::channel();
        let (next_tray, _) = mpsc::channel();
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        let next = EventRegistration::new(next_tx, next_tray, move || {
            count.fetch_add(1, Ordering::Relaxed);
        });
        // Finishing destruction of an older window must not unregister
        // the replacement's handlers.
        drop(first);
        dispatch_menu(MenuEvent {
            id: MenuId::new("replacement"),
        });
        assert_eq!(next_rx.try_recv().unwrap(), "replacement");
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert!(first_rx.try_recv().is_err());

        drop(next);
        dispatch_menu(MenuEvent {
            id: MenuId::new("closed"),
        });
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert!(matches!(
            next_rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
}
