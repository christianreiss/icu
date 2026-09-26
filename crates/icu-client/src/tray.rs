//! Tray icon (tray-icon 0.25, ksni on Linux): status icon, menu with
//! Fenster anzeigen/Stumm/Taub/Beenden, in the words the main window uses.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
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

/// The tray.
pub struct Tray {
    icon: TrayIcon,
    mute: CheckMenuItem,
    deafen: CheckMenuItem,
    show_id: MenuId,
    quit_id: MenuId,
    cmd: UnboundedSender<Command>,
    menu_rx: mpsc::Receiver<MenuId>,
    tray_rx: mpsc::Receiver<TrayIconEvent>,
    /// `(connected, muted, deafened)` the icon and menu were last drawn
    /// for; anything else in the state is not shown here.
    last: Option<(bool, bool, bool)>,
    /// A StatusNotifier host shows the icon (`host_present`), refreshed by
    /// a watch thread: plasmashell restarts, a GNOME extension comes later.
    host: Arc<AtomicBool>,
}

impl Tray {
    /// Creates the tray icon with its menu.
    pub fn start(cmd: UnboundedSender<Command>, ctx: egui::Context) -> Result<Tray, String> {
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
            .with_tooltip("ICU")
            .with_title("ICU")
            .with_icon(render_icon([0x9e, 0x9e, 0x9e], false))
            .build()
            .map_err(|e| e.to_string())?;
        // With a handler installed, muda/tray-icon deliver events to the
        // handler *instead of* the global receiver; forward them into our
        // own channel and wake the window so `poll` runs promptly (also
        // while the window is hidden, via `App::logic`).
        let (menu_tx, menu_rx) = mpsc::channel();
        let ctx_menu = ctx.clone();
        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
            let _ = menu_tx.send(ev.id);
            ctx_menu.request_repaint();
        }));
        let (tray_tx, tray_rx) = mpsc::channel();
        let ctx_tray = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
            let _ = tray_tx.send(ev);
            ctx_tray.request_repaint();
        }));
        let host = Arc::new(AtomicBool::new(host_present()));
        watch_host(host.clone(), ctx);
        Ok(Tray {
            icon,
            show_id: show.id().clone(),
            quit_id: quit.id().clone(),
            mute,
            deafen,
            cmd,
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
            if let TrayIconEvent::DoubleClick { .. } = ev {
                out = Some(TrayEvent::ShowWindow);
            }
        }
        out
    }

    /// Updates icon and check marks from the state.
    pub fn update(&mut self, state: &AppState) {
        let key = (
            state.conn == ConnStatus::Connected,
            state.muted,
            state.deafened,
        );
        if self.last == Some(key) {
            return;
        }
        self.last = Some(key);
        self.mute.set_checked(state.muted);
        self.deafen.set_checked(state.deafened);
        let color = if !key.0 {
            [0x9e, 0x9e, 0x9e]
        } else if state.deafened {
            [0xf4, 0x43, 0x36]
        } else if state.muted {
            [0xff, 0x98, 0x00]
        } else {
            [0x4c, 0xaf, 0x50]
        };
        let _ = self
            .icon
            .set_icon(Some(render_icon(color, state.muted || state.deafened)));
        let _ = self.icon.set_tooltip(Some(format!(
            "ICU — {}{}",
            match &state.conn {
                ConnStatus::Connected => "verbunden",
                ConnStatus::Connecting => "verbinde…",
                ConnStatus::Disconnected { .. } => "getrennt",
                ConnStatus::Fatal(_) => "Fehler",
            },
            if state.deafened {
                ", taub"
            } else if state.muted {
                ", stumm"
            } else {
                ""
            }
        )));
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
