//! The main window: who is here, whether they can hear you, and the four
//! switches that change that. Everything else lives in the settings window.

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use egui::{Color32, RichText};
use egui_phosphor::regular as icon;

use super::{connection::ConnectionView, fonts, settings::SettingsView, theme, widgets};
use crate::{
    config::{Identity, Paths},
    dsp::gate::Mode,
    engine::{AppState, Command, ConnStatus, Engine},
    net::register::register,
    video::VideoTier,
};

/// Header height, and with it the size of the header's icon buttons.
const HEADER: f32 = 32.0;
/// The speak surface and the mode tile beside it.
const SPEAK_H: f32 = 52.0;
/// The control tiles along the bottom.
const TILE_H: f32 = 54.0;
/// The one gap of the control dock's grid, both ways.
const DOCK_GAP: f32 = 8.0;
/// A bottom-row toggle: glyph, tooltip, on, color role, rec dot, hover
/// text, and the command a click sends (`None` when the feature is compiled
/// out).
type Toggle = (
    &'static str,
    &'static str,
    bool,
    widgets::Role,
    widgets::RecDot,
    Option<String>,
    Option<Command>,
);

/// Above this round-trip time a row's ping turns amber.
const SLOW_RTT_MS: u16 = 150;

/// How the main window ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The user quit.
    Quit,
    /// The user logged out; the identity is gone and the caller shows the
    /// invite window again.
    LoggedOut,
}

/// Runs the main window. `show` overrides `start_minimized`: right after
/// logging back in the window must not vanish to the tray.
pub fn run(engine: Engine, debug_stats: bool, show: bool) -> Result<Exit, String> {
    let start_minimized = engine.snapshot().settings.ui.start_minimized && !show;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ICU")
            .with_app_id("icu")
            .with_inner_size([400.0, 620.0])
            .with_min_inner_size([360.0, 320.0])
            .with_resizable(true)
            .with_visible(!start_minimized),
        ..Default::default()
    };
    let paths = engine.paths();
    let engine = Arc::new(Mutex::new(Some(engine)));
    let engine_for_app = engine.clone();
    let logged_out = Arc::new(AtomicBool::new(false));
    let logged_out_app = logged_out.clone();
    eframe::run_native(
        "ICU",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc, engine_for_app, debug_stats);
            app.logged_out = logged_out_app;
            // `window_visible` keeps the engine's view, so `logic` reports
            // the difference.
            app.hidden = start_minimized;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())?;
    if let Some(engine) = engine.lock().unwrap().take() {
        engine.shutdown();
    }
    if !logged_out.load(Ordering::Relaxed) {
        return Ok(Exit::Quit);
    }
    // Only now: a running engine may still save a renewed certificate.
    crate::config::Identity::remove(&paths)
        .map_err(|e| format!("Die Zugangsdaten ließen sich nicht löschen: {e}"))?;
    Ok(Exit::LoggedOut)
}

struct App {
    engine: Arc<Mutex<Option<Engine>>>,
    state_rx: tokio::sync::watch::Receiver<AppState>,
    /// The snapshot every panel draws from. Cloned out of the watch channel
    /// in `logic` — which eframe always runs before `ui` — and only when the
    /// channel actually has something new, so an idle window clones nothing
    /// and `ui` never clones at all.
    state: AppState,
    cmd: tokio::sync::mpsc::UnboundedSender<Command>,
    settings: Option<SettingsView>,
    /// The Verbindung window, opened from the status pill in the header.
    connection: Option<ConnectionView>,
    /// The theme currently installed in the egui context, so a settings
    /// change that isn't a theme change doesn't rebuild every style. A
    /// slider drag emits `UpdateSettings` on every frame it moves.
    theme: String,
    debug_stats: bool,
    /// Whether the separate debug window is open. Starts `true` when
    /// `--debug-stats` was passed, otherwise toggled from Settings → Über;
    /// the stats refresh runs whenever either is true, not just the CLI flag.
    debug_window_open: bool,
    last_stats: Instant,
    stats_text: String,
    stats: Arc<crate::stats::DebugStats>,
    expanded: Option<icu_proto::UserId>,
    ptt_held: bool,
    #[cfg(feature = "tray")]
    tray: Option<crate::tray::Tray>,
    #[cfg(feature = "overlay")]
    overlay: Option<crate::overlay::OverlayHandle>,
    #[cfg(feature = "camera")]
    camera: Option<crate::camera::CameraHandle>,
    /// Live pictures for the participant list. Not feature-gated: without
    /// `camera` nothing decodes, the list stays empty, and every row keeps
    /// its initials.
    videos: crate::video::VideoTextures,
    /// The own row's picture: own video never comes back from the server
    /// (the sender is excluded from the fan-out), so it is read straight
    /// from the capture thread's preview slot.
    #[cfg(feature = "camera")]
    self_video: Option<crate::camera::CameraPreview>,
    /// Hidden to the tray (or never shown: `start_minimized`).
    hidden: bool,
    /// What was last reported as `Command::SetWindowVisible`; the engine
    /// starts from the same `start_minimized` value.
    window_visible: bool,
    /// What the window title says now; set again only when it changes.
    title: String,
    /// The window has been sized to the list once since connecting.
    fitted: bool,
    /// People in the list last frame; one more may grow the window.
    rows_seen: usize,
    quit_requested: bool,
    /// Set when the user logged out from the settings; read by `run`.
    logged_out: Arc<AtomicBool>,
}

impl App {
    /// Window height follows the people list: once after connecting it
    /// shrinks or grows to exactly fit, afterwards it only grows, and only
    /// when somebody joins — a window the user made smaller stays so.
    fn fit_to_list(&mut self, ctx: &egui::Context, overflow: f32) {
        let state = &self.state;
        let connected = state.conn == ConnStatus::Connected && state.me.is_some();
        if !connected {
            self.fitted = false;
            self.rows_seen = 0;
            return;
        }
        let rows = state.peers.len() + 1;
        let joined = rows > self.rows_seen;
        self.rows_seen = rows;
        let resize = if !self.fitted {
            overflow.abs() > 1.0
        } else {
            joined && overflow > 1.0
        };
        if self.hidden || !resize {
            self.fitted |= !self.hidden;
            return;
        }
        // `content_rect`, not `viewport().inner_rect`: the latter needs the
        // window position, which Wayland never reports, so it stays `None`.
        let inner = ctx.content_rect();
        let monitor = ctx.input(|i| i.viewport().monitor_size);
        let max = monitor.map_or(f32::INFINITY, |m| m.y * 0.9);
        let height = (inner.height() + overflow).min(max).ceil();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            inner.width(),
            height,
        )));
        self.fitted = true;
    }

    fn new(
        cc: &eframe::CreationContext<'_>,
        engine: Arc<Mutex<Option<Engine>>>,
        debug_stats: bool,
    ) -> App {
        let (state_rx, cmd, stats, theme_name, start_minimized) = {
            let guard = engine.lock().unwrap();
            let e = guard.as_ref().expect("engine");
            let snapshot = e.snapshot();
            (
                e.state(),
                e.command_sender(),
                e.stats(),
                snapshot.settings.ui.theme.clone(),
                snapshot.settings.ui.start_minimized,
            )
        };
        fonts::install(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &theme_name);
        #[cfg(feature = "tray")]
        let tray = crate::tray::Tray::start(cmd.clone(), cc.egui_ctx.clone()).ok();
        #[cfg(feature = "overlay")]
        let overlay = {
            let guard = engine.lock().unwrap();
            let e = guard.as_ref().expect("engine");
            match crate::overlay::OverlayHandle::start(e.state(), e.video_frames_source()) {
                Ok(h) => {
                    tracing::info!("overlay thread started (layer shell available)");
                    Some(h)
                }
                Err(err) => {
                    tracing::warn!(%err, "overlay unavailable");
                    None
                }
            }
        };
        #[cfg(feature = "camera")]
        let (camera, self_video) = {
            let guard = engine.lock().unwrap();
            let e = guard.as_ref().expect("engine");
            let snapshot = e.snapshot();
            let handle = crate::camera::CameraHandle::new(e.camera_link());
            let preview = crate::camera::CameraPreview::new(handle.preview_source());
            if snapshot.settings.camera.enabled {
                let _ = cmd.send(Command::SetCamera(true));
            }
            (Some(handle), Some(preview))
        };
        let videos = {
            let guard = engine.lock().unwrap();
            let e = guard.as_ref().expect("engine");
            crate::video::VideoTextures::new(e.video_frames_source())
        };
        let mut state = state_rx.borrow().clone();
        sort_peers(&mut state);
        App {
            engine,
            state,
            state_rx,
            cmd,
            settings: None,
            connection: None,
            theme: theme_name,
            debug_stats,
            debug_window_open: debug_stats,
            last_stats: Instant::now(),
            stats_text: String::new(),
            stats,
            expanded: None,
            ptt_held: false,
            #[cfg(feature = "tray")]
            tray,
            #[cfg(feature = "overlay")]
            overlay,
            #[cfg(feature = "camera")]
            camera,
            videos,
            #[cfg(feature = "camera")]
            self_video,
            hidden: start_minimized,
            window_visible: !start_minimized,
            title: "ICU".into(),
            fitted: false,
            rows_seen: 0,
            quit_requested: false,
            logged_out: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// `sort_by_cached_key` computes each lowercase key once instead of once per
/// comparison. Done where the state arrives, not where it is drawn.
fn sort_peers(state: &mut AppState) {
    state
        .peers
        .sort_by_cached_key(|p| p.info.name.to_lowercase());
}

impl eframe::App for App {
    /// Runs before every `ui` pass and also while the window is hidden (on
    /// `request_repaint`): tray events, camera state, close handling.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.state_rx.has_changed().unwrap_or(false) {
            self.state = self.state_rx.borrow_and_update().clone();
            sort_peers(&mut self.state);
        }
        #[cfg_attr(
            not(any(feature = "tray", feature = "camera")),
            allow(unused_variables)
        )]
        let state = &self.state;

        #[cfg(feature = "tray")]
        if let Some(tray) = &self.tray {
            match tray.poll() {
                Some(crate::tray::TrayEvent::ShowWindow) => {
                    self.hidden = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Some(crate::tray::TrayEvent::Quit) => {
                    self.quit_requested = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                None => {}
            }
            tray.update(state);
        }
        #[cfg(feature = "camera")]
        if let Some(camera) = &mut self.camera {
            camera.sync(state);
        }

        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_requested {
            #[cfg(feature = "tray")]
            if self.tray.is_some() && !self.hidden && !ctx.input(|i| i.modifiers.shift) {
                // Close = hide to tray; Shift+close or the tray menu quits.
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.hidden = true;
            }
        }

        // Window on screen = this user views video (`UserFlags::VIEWING`,
        // together with the overlay). `minimized` is unknown on Wayland and
        // then counts as visible: better a frame too many than a black tile.
        let minimized = ctx.input(|i| i.viewport().minimized).unwrap_or(false);
        let visible = !self.hidden && !minimized;
        if visible != self.window_visible {
            self.window_visible = visible;
            let _ = self.cmd.send(Command::SetWindowVisible(visible));
        }

        // Connected, the title bar (and task bar) says who this window is.
        let title = match (&self.state.conn, &self.state.me) {
            (ConnStatus::Connected, Some(me)) => format!("ICU — {}", me.name),
            _ => "ICU".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(33));
        // A local sender, not `self.send`: a closure that calls a `&self`
        // method captures the *whole* `self` and would collide with the
        // `&mut self.settings` / `&mut self.expanded` these panels need.
        let cmd = self.cmd.clone();
        let send = |c: Command| {
            let _ = cmd.send(c);
        };

        self.debug_window(&ctx);
        self.settings_window(&ctx);
        self.connection_window(&ctx);

        // Here and not in `logic`: a window hidden to tray would upload
        // textures nobody is looking at. `ui` already asks for a repaint
        // every 33 ms against a 15 fps decoder, so every picture is uploaded
        // exactly once.
        self.videos.refresh(&ctx);
        #[cfg(feature = "camera")]
        let self_video = {
            let on = self.state.camera_on;
            self.self_video
                .as_mut()
                .filter(|_| on)
                .and_then(|p| p.texture(&ctx))
        };
        #[cfg(not(feature = "camera"))]
        let self_video = None;

        let state = &self.state;
        let mut new_theme: Option<&'static str> = None;
        egui::Panel::top("status")
            .show_separator_line(false)
            .show(root, |ui| {
                let tok = theme::tokens(ui.visuals());
                ui.add_space(4.0);
                // One row, one explicit height, everything centered on a shared
                // baseline — `allocate_ui_with_layout` with an explicit height,
                // not `with_layout`: the latter claims the parent's *entire*
                // remaining height, and `Align::Center` then centers the content
                // inside that tall rect, which balloons the row's `min_rect` and
                // pushes everything after it off-screen.
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), HEADER),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.add_space(4.0);
                        // The server, then the connection as a dot and a word.
                        // The own name is not repeated here — it is the first
                        // row of the list right below.
                        ui.label(theme::strong_text(&state.server_name));
                        // Own role on this server: an admin sees it at a
                        // glance, not only behind the Benutzer tab.
                        if state.is_admin {
                            ui.label(
                                RichText::new(format!("{} Admin", icon::SHIELD_STAR))
                                    .size(12.0)
                                    .color(tok.accent),
                            )
                            .on_hover_text("Du bist Admin auf diesem Server");
                            ui.add_space(4.0);
                        }
                        let (color, short, detail) = conn_status(state, &tok);
                        // Clickable in every state, not just Connected: a failed
                        // or retrying connection is exactly when the details are
                        // worth a window instead of a one-line tooltip.
                        let hint = if detail.is_empty() {
                            format!("{short}\n\nKlicken für Verbindungsdetails")
                        } else {
                            format!("{short} — {detail}\n\nKlicken für Verbindungsdetails")
                        };
                        if widgets::status_link(ui, color, &short)
                            .on_hover_text(hint)
                            .clicked()
                        {
                            self.connection = match self.connection.take() {
                                Some(_) => None,
                                None => Some(ConnectionView::new(self.stats.clone())),
                            };
                        }

                        // Own outgoing video quality tier, set by
                        // `crate::quality::QualityController`. Automatic, no
                        // settings toggle (`Research/feature-freeze.md`'s bias
                        // against exposing adaptive behavior as a dial) — but
                        // shown, since a tier-down is visible to other people as
                        // this client's video degrading.
                        let tier =
                            VideoTier::from_u8(self.stats.video_tier.load(Ordering::Relaxed) as u8);
                        let hint = match tier {
                            VideoTier::T0 => None,
                            VideoTier::T4 => {
                                Some("Video pausiert — das Netz trägt es gerade nicht.")
                            }
                            _ => Some("Video reduziert — weniger Bitrate für eine stabile Stimme."),
                        };
                        if let Some(hint) = hint {
                            ui.label(
                                RichText::new(icon::VIDEO_CAMERA_SLASH)
                                    .size(15.0)
                                    .color(tok.connecting),
                            )
                            .on_hover_text(hint);
                        }

                        widgets::rtl(ui, HEADER, |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if widgets::icon_button(
                                ui,
                                icon::GEAR_FINE,
                                HEADER,
                                self.settings.is_some(),
                            )
                            .on_hover_text("Einstellungen")
                            .clicked()
                            {
                                self.settings = match self.settings.take() {
                                    Some(_) => None,
                                    None => {
                                        let paths = self
                                            .engine
                                            .lock()
                                            .unwrap()
                                            .as_ref()
                                            .map(|e| e.paths())
                                            .unwrap_or_else(|| Paths::new(None));
                                        #[cfg_attr(not(feature = "camera"), allow(unused_mut))]
                                        let mut view = SettingsView::new(
                                            state,
                                            state.hotkey_backend.clone(),
                                            self.stats.clone(),
                                            paths,
                                        );
                                        #[cfg(feature = "camera")]
                                        if let Some(camera) = &self.camera {
                                            view = view.with_preview(camera.preview_source());
                                        }
                                        Some(view)
                                    }
                                };
                            }
                            // Flips whatever is on screen right now, so from
                            // `system` it always does something visible.
                            let dark = ui.visuals().dark_mode;
                            let (glyph, hint, next) = if dark {
                                (icon::SUN, "Hell", "light")
                            } else {
                                (icon::MOON, "Dunkel", "dark")
                            };
                            if widgets::icon_button(ui, glyph, HEADER, false)
                                .on_hover_text(hint)
                                .clicked()
                            {
                                new_theme = Some(next);
                            }
                        });
                    },
                );
                ui.add_space(4.0);

                if let Some(min) = &state.update_required {
                    widgets::banner(
                        ui,
                        widgets::BannerKind::Warning,
                        "Update nötig",
                        &format!(
                            "Der Server verlangt mindestens Version {min} (diese: {}).",
                            env!("CARGO_PKG_VERSION")
                        ),
                        None::<fn()>,
                    );
                }
                // Non-techies do not hover: a connection that will not come
                // back on its own says why right here, not only in the
                // pill's tooltip.
                match &state.conn {
                    ConnStatus::Fatal(problem) => widgets::banner(
                        ui,
                        widgets::BannerKind::Error,
                        "Keine Verbindung",
                        &problem.text,
                        None::<fn()>,
                    ),
                    ConnStatus::Disconnected { reason, attempt } if *attempt >= 2 => {
                        widgets::banner(
                            ui,
                            widgets::BannerKind::Warning,
                            "Verbindung unterbrochen",
                            &reason.text,
                            None::<fn()>,
                        )
                    }
                    _ => {}
                }
                if let Some(notice) = &state.notice {
                    widgets::banner(
                        ui,
                        widgets::BannerKind::Warning,
                        "Bitte warten",
                        notice,
                        None::<fn()>,
                    );
                }
                if let Some(banner) = &state.banner {
                    widgets::banner(
                        ui,
                        widgets::BannerKind::Error,
                        "Fehler",
                        &banner.text,
                        Some(|| send(Command::DismissBanner)),
                    );
                }
            });

        // As much room under the dock as beside it.
        let dock_frame = egui::Frame::side_top_panel(root.style());
        let side = dock_frame.inner_margin.left;
        egui::Panel::bottom("controls")
            .show_separator_line(false)
            .frame(dock_frame.inner_margin(egui::Margin {
                bottom: side,
                ..dock_frame.inner_margin
            }))
            .show(root, |ui| {
                let (rec, rec_hint) = camera_feedback(
                    state.camera_on,
                    state.conn == ConnStatus::Connected,
                    &state.peers,
                    VideoTier::from_u8(self.stats.video_tier.load(Ordering::Relaxed) as u8),
                );
                let toggles: [Toggle; 4] = [
                    (
                        icon::MICROPHONE_SLASH,
                        "Stumm",
                        state.muted,
                        widgets::Role::Danger,
                        widgets::RecDot::Off,
                        None,
                        Some(Command::SetMuted(!state.muted)),
                    ),
                    (
                        icon::SPEAKER_SLASH,
                        "Taub",
                        state.deafened,
                        widgets::Role::Danger,
                        widgets::RecDot::Off,
                        None,
                        Some(Command::SetDeafened(!state.deafened)),
                    ),
                    (
                        icon::VIDEO_CAMERA,
                        "Kamera",
                        state.camera_on,
                        widgets::Role::Accent,
                        rec,
                        rec_hint,
                        cfg!(feature = "camera").then(|| Command::SetCamera(!state.camera_on)),
                    ),
                    (
                        icon::SQUARES_FOUR,
                        "Overlay",
                        state.settings.overlay.enabled,
                        widgets::Role::Accent,
                        widgets::RecDot::Off,
                        None,
                        cfg!(feature = "overlay")
                            .then(|| Command::SetOverlay(!state.settings.overlay.enabled)),
                    ),
                ];
                let toggles: Vec<_> = toggles.into_iter().filter(|t| t.6.is_some()).collect();

                ui.add_space(6.0);
                // One card, one grid: the tiles share their column width and
                // gap with the speak row above, so every edge in the dock
                // lines up with another.
                let ptt = egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                    .corner_radius(egui::CornerRadius::same(theme::R_CARD))
                    .inner_margin(egui::Margin::same(DOCK_GAP as i8))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(DOCK_GAP, DOCK_GAP);
                        let n = toggles.len().max(1) as f32;
                        let col = ((ui.available_width() - (n - 1.0) * DOCK_GAP) / n).floor();
                        let ptt = ui.horizontal(|ui| speak_row(ui, state, &send, col)).inner;
                        ui.horizontal(|ui| {
                            for (glyph, label, active, role, rec, hint, cmd) in toggles {
                                let size = egui::vec2(col, TILE_H);
                                let mut tile =
                                    widgets::tile(ui, glyph, label, active, role, rec, size);
                                if let Some(hint) = hint {
                                    tile = tile.on_hover_text(hint);
                                }
                                if tile.clicked() {
                                    if let Some(c) = cmd {
                                        send(c);
                                    }
                                }
                            }
                        });
                        ptt
                    })
                    .inner;
                // Hold semantics: a press that starts on the button keeps the
                // gate open until the primary button is released anywhere, even
                // if the pointer drifts off the widget or egui stops treating it
                // as "down on" (observed in the live test: transmission ended
                // after a few seconds while the button was still held).
                let primary_down = ui.input(|i| i.pointer.primary_down());
                let held = match &ptt {
                    Some(_) if self.ptt_held => primary_down,
                    Some(button) => button.is_pointer_button_down_on() && primary_down,
                    None => false,
                };
                if held {
                    ui.ctx().request_repaint();
                }
                if held != self.ptt_held {
                    self.ptt_held = held;
                    send(Command::Ptt(held));
                }
            });

        let expanded = &mut self.expanded;
        let videos = &self.videos;
        // `UserFlags::VIEWING` only says the window or overlay is up; someone
        // actually watches video only while another person's camera is on.
        let cameras = usize::from(state.camera_on)
            + state
                .peers
                .iter()
                .filter(|p| p.info.flags.camera_on())
                .count();
        let watches =
            |viewing: bool, own_camera: bool| viewing && cameras > usize::from(own_camera);
        let list = egui::CentralPanel::default().show(root, |ui| {
            // The people get a card of their own, the same surface as the
            // control dock below, so the window reads as two calm blocks.
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                .corner_radius(egui::CornerRadius::same(theme::R_CARD))
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.add_space(2.0);
                            if let Some(me) = &state.me {
                                participant_row(
                                    ui,
                                    Participant {
                                        name: &me.name,
                                        user_id: me.user_id,
                                        speaking: state.sending,
                                        muted: state.muted,
                                        deafened: state.deafened,
                                        camera: state.camera_on,
                                        mode: Some(state.settings.audio.mode.into()),
                                        volume: None,
                                        is_admin: state.is_admin,
                                        viewing: watches(me.flags.viewing(), state.camera_on),
                                        rtt_ms: state.server_rtt_ms,
                                        client_rtt_ms: state.rtt_ms.map(|ms| ms.round() as u16),
                                        loss_permille: None,
                                        session: me.session,
                                        video: self_video,
                                    },
                                    expanded,
                                    &send,
                                );
                            }
                            for p in &state.peers {
                                participant_row(
                                    ui,
                                    Participant {
                                        name: &p.info.name,
                                        user_id: p.info.user_id,
                                        speaking: p.speaking,
                                        muted: p.info.flags.muted(),
                                        deafened: p.info.flags.deafened(),
                                        camera: p.info.flags.camera_on(),
                                        mode: None,
                                        volume: Some(p.volume),
                                        is_admin: p.info.flags.is_admin(),
                                        viewing: watches(
                                            p.info.flags.viewing(),
                                            p.info.flags.camera_on(),
                                        ),
                                        rtt_ms: p.rtt_ms,
                                        client_rtt_ms: None,
                                        loss_permille: p.recv_loss_permille,
                                        session: p.info.session,
                                        // The flag arrives with `UserState`, so the
                                        // picture goes the moment somebody switches
                                        // off — not three seconds later when the
                                        // last frame goes stale.
                                        video: p
                                            .info
                                            .flags
                                            .camera_on()
                                            .then(|| videos.get(p.info.session))
                                            .flatten(),
                                    },
                                    expanded,
                                    &send,
                                );
                            }
                            if state.peers.is_empty() && state.conn == ConnStatus::Connected {
                                alone(ui);
                            }
                        })
                })
                .inner
        });
        self.fit_to_list(
            &ctx,
            list.inner.content_size.y - list.inner.inner_rect.height(),
        );

        if let Some(next) = new_theme {
            self.theme = next.to_string();
            theme::apply(&ctx, next);
            let mut settings = self.state.settings.clone();
            settings.ui.theme = next.to_string();
            if let Some(view) = &mut self.settings {
                view.draft.ui.theme = next.to_string();
            }
            send(Command::UpdateSettings(Box::new(settings)));
        }
    }

    fn on_exit(&mut self) {
        #[cfg(feature = "camera")]
        {
            self.camera = None;
        }
        #[cfg(feature = "overlay")]
        {
            self.overlay = None;
        }
        if let Some(engine) = self.engine.lock().unwrap().take() {
            engine.shutdown();
        }
    }
}

impl App {
    /// The `--debug-stats` window: a monospace dump, refreshed every 2s.
    fn debug_window(&mut self, ctx: &egui::Context) {
        if (self.debug_stats || self.debug_window_open)
            && self.last_stats.elapsed() > Duration::from_secs(2)
        {
            self.last_stats = Instant::now();
            self.stats_text = self.stats.report();
            if self.debug_stats {
                eprintln!("{}", self.stats_text);
            }
        }
        if !self.debug_window_open {
            return;
        }
        let mut still_open = true;
        let stats_text = &self.stats_text;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("icu-debug"),
            egui::ViewportBuilder::default()
                .with_title("ICU — Debug")
                .with_inner_size([540.0, 440.0]),
            |ctx, _class| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.label(RichText::new(stats_text).monospace().small());
                    });
                });
                if ctx.input(|i| i.viewport().close_requested()) {
                    still_open = false;
                }
            },
        );
        self.debug_window_open = still_open;
    }

    /// The Verbindung window, drawn into its own immediate viewport.
    /// Simpler than [`Self::settings_window`]: the view is read-only, so
    /// there are no commands to drain and no theme change to react to.
    fn connection_window(&mut self, ctx: &egui::Context) {
        if self.connection.is_none() {
            return;
        }
        let mut still_open = true;
        // Plain locals, not `self` fields — the closure would otherwise
        // capture the whole of `self` and collide `&self.state` against
        // `&mut self.connection` (see `settings_window`).
        let state = &self.state;
        let view = &mut self.connection;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("icu-connection"),
            egui::ViewportBuilder::default()
                .with_title("ICU — Verbindung")
                .with_inner_size([700.0, 560.0])
                .with_min_inner_size([560.0, 380.0])
                .with_resizable(true),
            |vctx, _class| {
                egui::CentralPanel::default().show(vctx, |ui| {
                    view.as_mut().expect("checked above").ui(ui, state);
                });
                if vctx.input(|i| i.viewport().close_requested()) {
                    still_open = false;
                }
            },
        );
        if !still_open {
            self.connection = None;
        }
    }

    /// The settings window, drawn into its own immediate viewport.
    fn settings_window(&mut self, ctx: &egui::Context) {
        if self.settings.is_none() {
            return;
        }
        let mut still_open = true;
        let mut cmds = Vec::new();
        // Plain locals, not `self` fields — read/write freely inside the
        // closure with no borrow conflict against `self.settings`.
        let mut debug_open = self.debug_window_open;
        let state = &self.state;
        let settings = &mut self.settings;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("icu-settings"),
            egui::ViewportBuilder::default()
                .with_title("ICU — Einstellungen")
                .with_inner_size([820.0, 640.0])
                .with_min_inner_size([640.0, 480.0])
                .with_resizable(true),
            |vctx, _class| {
                egui::CentralPanel::default().show(vctx, |ui| {
                    let view = settings.as_mut().expect("checked above");
                    view.debug_window_open = debug_open;
                    cmds = view.ui(ui, state);
                    debug_open = view.debug_window_open;
                });
                if vctx.input(|i| i.viewport().close_requested()) {
                    still_open = false;
                }
            },
        );
        for c in cmds {
            if let Command::UpdateSettings(s) = &c {
                // A slider drag emits this every frame it moves; only a real
                // theme change is worth rebuilding all styles for.
                if s.ui.theme != self.theme {
                    self.theme.clone_from(&s.ui.theme);
                    theme::apply(ctx, &self.theme);
                }
            }
            let _ = self.cmd.send(c);
        }
        if self.settings.as_ref().is_some_and(|v| v.logout_requested) {
            self.logged_out.store(true, Ordering::Relaxed);
            self.settings = None;
            self.quit_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !still_open {
            self.settings = None;
        }
        if debug_open != self.debug_window_open {
            self.debug_window_open = debug_open;
            if self.debug_window_open {
                self.stats_text = self.stats.report();
                self.last_stats = Instant::now();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

/// Color, the one word the pill shows, and the long form for its tooltip.
fn conn_status(state: &AppState, tok: &theme::Tokens) -> (Color32, String, String) {
    match &state.conn {
        ConnStatus::Connected => (
            tok.connected,
            "verbunden".to_string(),
            match &state.me {
                Some(me) => format!("{} · user_id {}", state.server, me.user_id),
                None => state.server.clone(),
            },
        ),
        ConnStatus::Connecting => (tok.connecting, "verbinde".to_string(), state.server.clone()),
        ConnStatus::Disconnected { reason, attempt } => (
            tok.connecting,
            "getrennt".to_string(),
            format!("{} (Versuch {attempt})", reason.text),
        ),
        ConnStatus::Fatal(reason) => (tok.error, "Fehler".to_string(), reason.text.clone()),
    }
}

// ---------------------------------------------------------------------------
// Bottom bar
// ---------------------------------------------------------------------------

/// The push-to-talk binding as a person would say it: `KEY_RIGHTCTRL`
/// becomes `RIGHTCTRL`, and the Portal backend shows its own trigger
/// (`CTRL+SHIFT+F12`) because that is what it actually bound.
fn hotkey_label(state: &AppState) -> String {
    let binding = &state.settings.hotkeys.ptt;
    if state.hotkey_backend.contains("portal") {
        if let Some(trigger) = &binding.trigger {
            return trigger.clone();
        }
    }
    binding
        .key
        .strip_prefix("KEY_")
        .or_else(|| binding.key.strip_prefix("BTN_"))
        .unwrap_or(&binding.key)
        .to_string()
}

/// The speak surface plus the PTT/VAD tile beside it, `tile_w` wide. Returns the
/// surface's response only in push-to-talk mode — in voice activation there
/// is nothing to press, so it is a readout instead of a button that would do
/// nothing.
fn speak_row(
    ui: &mut egui::Ui,
    state: &AppState,
    send: &impl Fn(Command),
    tile_w: f32,
) -> Option<egui::Response> {
    let ptt = Mode::from(state.settings.audio.mode) == Mode::Ptt;
    let offline = state.conn != ConnStatus::Connected;
    let open = !offline && if ptt { state.ptt_down } else { state.sending };
    let key = hotkey_label(state);
    let speak = widgets::Speak {
        ptt,
        open,
        muted: state.muted,
        offline,
        level: if ptt {
            theme::meter_fraction(state.level.rms_db)
        } else {
            state.vad_prob
        },
        threshold: (!ptt).then_some(state.vad_threshold),
        key: ptt.then_some(key.as_str()),
    };
    let tooltip = if offline {
        format!(
            "Nicht verbunden — es wird nichts gesendet\nPegel {:.0} dBFS",
            state.level.rms_db
        )
    } else if ptt {
        format!(
            "Push-to-Talk — halten zum Sprechen\nPegel {:.0} dBFS",
            state.level.rms_db
        )
    } else {
        format!(
            "Sprachaktivierung\nStimme {:.2}, Schwelle {:.2}{}\nPegel {:.0} dBFS",
            state.vad_prob,
            state.vad_threshold,
            if state.settings.audio.vad_auto {
                " (automatisch)"
            } else {
                ""
            },
            state.level.rms_db
        )
    };

    let size = egui::vec2(ui.available_width() - tile_w - DOCK_GAP, SPEAK_H);
    let response = widgets::speak_surface(ui, &speak, size).on_hover_text(tooltip);
    let (glyph, label, next) = if ptt {
        (icon::HAND_TAP, "PTT", "Umschalten auf Sprachaktivierung")
    } else {
        (icon::WAVEFORM, "VAD", "Umschalten auf Push-to-Talk")
    };
    let size = egui::vec2(tile_w, SPEAK_H);
    if widgets::tile(
        ui,
        glyph,
        label,
        false,
        widgets::Role::Accent,
        widgets::RecDot::Off,
        size,
    )
    .on_hover_text(next)
    .clicked()
    {
        send(Command::ToggleMode);
    }
    ptt.then_some(response)
}

// ---------------------------------------------------------------------------
// Participants
// ---------------------------------------------------------------------------

/// One row's worth of participant, local or remote. `mode` is `Some` only
/// for the local user (it is a client-local setting); `volume` is `Some`
/// only for everyone else (there is no local playback volume for oneself).
/// The camera tile's dot and hover text: whether this client's picture is
/// going out right now, and to whom. Follows [`crate::engine::send_tier`], the
/// rule the camera thread encodes by, so the dot never claims an audience the
/// encoder does not have.
fn camera_feedback(
    camera_on: bool,
    connected: bool,
    peers: &[crate::engine::Peer],
    tier: VideoTier,
) -> (widgets::RecDot, Option<String>) {
    use widgets::RecDot;
    if !camera_on {
        return (RecDot::Off, None);
    }
    if !connected {
        return (
            RecDot::Idle,
            Some("Nicht verbunden — Video geht an niemanden".into()),
        );
    }
    let viewers: Vec<&str> = peers
        .iter()
        .filter(|p| p.info.flags.viewing())
        .map(|p| p.info.name.as_str())
        .collect();
    if viewers.is_empty() {
        return (
            RecDot::Idle,
            Some("Niemand schaut zu — Kamera pausiert".into()),
        );
    }
    if crate::engine::send_tier(tier, peers).paused() {
        return (
            RecDot::Idle,
            Some("Video pausiert — das Netz trägt es gerade nicht.".into()),
        );
    }
    (
        RecDot::Live,
        Some(format!("Sichtbar für: {}", viewers.join(", "))),
    )
}

struct Participant<'a> {
    name: &'a str,
    user_id: icu_proto::UserId,
    speaking: bool,
    muted: bool,
    deafened: bool,
    camera: bool,
    mode: Option<Mode>,
    volume: Option<f32>,
    is_admin: bool,
    /// Looking at video right now: `UserFlags::VIEWING` while somebody
    /// else's camera is on.
    viewing: bool,
    /// Server-measured RTT, milliseconds. `None` until the first
    /// `UserRtt` arrives (`PROTOCOL.md` §2.2).
    rtt_ms: Option<u16>,
    /// Own `Ping`/`Pong` round trip; `Some` only for the local user.
    client_rtt_ms: Option<u16>,
    /// What this client loses of this person's audio, per mille; remote
    /// participants only.
    loss_permille: Option<u16>,
    session: icu_proto::Session,
    /// This person's live picture, `None` when there is none to show. Goes
    /// into the avatar's circle in place of the initials.
    video: Option<egui::load::SizedTexture>,
}

impl Participant<'_> {
    /// The second line: what this person is doing right now, or nothing.
    /// "Nothing" is the common case and stays empty on purpose — a row per
    /// person saying "Mikrofon frei" is noise, not information.
    fn subtitle(&self) -> Option<&'static str> {
        if self.speaking {
            Some(match self.mode {
                Some(Mode::Ptt) => "sendet · Push-to-Talk",
                Some(Mode::Vad) => "sendet · Sprachaktivierung",
                None => "spricht",
            })
        } else if self.deafened {
            Some("hört nicht zu")
        } else if self.muted {
            Some("stummgeschaltet")
        } else if self.camera {
            Some("Kamera an")
        } else {
            None
        }
    }
}

/// Signal bars for a server-measured round trip, 1 (bad) to 5 (great).
fn rtt_bars(ms: u16) -> u8 {
    match ms {
        0..=40 => 5,
        41..=80 => 4,
        81..=SLOW_RTT_MS => 3,
        _ if ms <= 2 * SLOW_RTT_MS => 2,
        _ => 1,
    }
}

fn participant_row(
    ui: &mut egui::Ui,
    p: Participant<'_>,
    open: &mut Option<icu_proto::UserId>,
    send: &impl Fn(Command),
) {
    let tok = theme::tokens(ui.visuals());
    let expanded = *open == Some(p.user_id);
    // Last frame's row rect: the volume handle only shows under the pointer,
    // and in immediate mode the row does not know its own rect until drawn.
    let rect_id = ui.id().with(("row", p.user_id));
    let hovered = ui
        .data(|d| d.get_temp::<egui::Rect>(rect_id))
        .is_some_and(|r| ui.rect_contains_pointer(r));
    let fill = if p.speaking {
        tok.accent_tint
    } else if expanded || hovered {
        tok.surface_hi
    } else {
        Color32::TRANSPARENT
    };
    // The whole row is the click target. The scope senses clicks before its
    // children are added, so the volume button and the slider inside still
    // take their own clicks first.
    let row = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
        egui::Frame::new()
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(theme::R_ROW))
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut hint = String::new();
                    if let Some(s) = p.subtitle() {
                        hint.push_str(s);
                    }
                    let avatar = widgets::avatar(ui, p.name, p.user_id, p.speaking, 30.0, p.video);
                    ui.add_space(2.0);
                    let name = ui.label(if p.mode.is_some() {
                        theme::strong_text(p.name)
                    } else {
                        RichText::new(p.name).size(14.0)
                    });
                    if !hint.is_empty() {
                        avatar.on_hover_text(&hint);
                        name.on_hover_text(&hint);
                    }
                    if p.speaking {
                        widgets::speaking_bars(ui);
                    }
                    widgets::rtl(ui, 36.0, |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        // Server-measured RTT (`PROTOCOL.md` §2.2 `UserRtt`), not
                        // a client self-report: every viewer sees the same number
                        // for a given peer. Signal bars in a fixed box, the exact
                        // number on hover. Far right, the column every row has.
                        let (bars, _) =
                            ui.allocate_exact_size(egui::vec2(22.0, 20.0), egui::Sense::hover());
                        if let Some(ms) = p.rtt_ms {
                            let lit = rtt_bars(ms);
                            let on = match lit {
                                4.. => tok.connected,
                                2..=3 => tok.connecting,
                                _ => tok.error,
                            };
                            let off = tok.dim_text.gamma_multiply(0.25);
                            for i in 0..5u8 {
                                let h = 4.0 + 2.5 * f32::from(i);
                                let x = bars.left() + 2.0 + 4.0 * f32::from(i);
                                let bar = egui::Rect::from_min_max(
                                    egui::pos2(x, bars.center().y + 6.0 - h),
                                    egui::pos2(x + 2.5, bars.center().y + 6.0),
                                );
                                ui.painter().rect_filled(
                                    bar,
                                    egui::CornerRadius::same(1),
                                    if i < lit { on } else { off },
                                );
                            }
                            ui.interact(
                                bars,
                                ui.id().with(("rtt", p.user_id)),
                                egui::Sense::hover(),
                            )
                            .on_hover_text(format!("Ping {ms} ms"));
                        }
                        // Always the same glyph: the crossed-out speaker is the
                        // badge for "this person hears nothing", and a volume
                        // handle wearing it would read as that.
                        if let Some(vol) = p.volume {
                            if widgets::icon_button(ui, icon::SPEAKER_HIGH, 28.0, expanded)
                                .on_hover_text(format!("Lautstärke {vol:.0} %"))
                                .clicked()
                            {
                                *open = if expanded { None } else { Some(p.user_id) };
                            }
                        } else {
                            // Own row has no volume handle; keep its column so
                            // ping and icons line up with every other row.
                            ui.add_space(28.0);
                        }
                        // One fixed slot per glyph, so glyph widths don't jitter
                        // the columns between rows.
                        let slot = |ui: &mut egui::Ui, text: RichText| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(18.0, 20.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| ui.label(text),
                            )
                            .inner
                        };
                        for (on, color, glyph) in [
                            (p.deafened, tok.error, icon::SPEAKER_SLASH),
                            (p.muted, tok.error, icon::MICROPHONE_SLASH),
                            (p.camera, tok.dim_text, icon::VIDEO_CAMERA),
                        ] {
                            if on {
                                slot(ui, RichText::new(glyph).size(15.0).color(color));
                            }
                        }
                        if p.is_admin {
                            slot(
                                ui,
                                RichText::new(icon::SHIELD_STAR)
                                    .size(14.0)
                                    .color(tok.dim_text),
                            )
                            .on_hover_text("Admin");
                        }
                    });
                });
                if expanded {
                    details(ui, &p, &tok, send);
                }
            });
    });
    let r = row.response.rect;
    if row.response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if row.response.clicked() {
        *open = if expanded { None } else { Some(p.user_id) };
    }
    ui.data_mut(|d| d.insert_temp(rect_id, r));
}

/// The unfolded part of a participant row: what the client knows about this
/// person, and for everyone but oneself the playback volume.
fn details(ui: &mut egui::Ui, p: &Participant<'_>, tok: &theme::Tokens, send: &impl Fn(Command)) {
    const NONE: &str = "—";
    let ms = |v: Option<u16>| v.map_or_else(|| NONE.to_string(), |v| format!("{v} ms"));
    let mut status = Vec::new();
    for (on, word) in [
        (p.speaking, "spricht"),
        (p.muted, "stumm"),
        (p.deafened, "hört nicht zu"),
        (p.camera, "Kamera an"),
        (p.viewing, "schaut Video"),
    ] {
        if on {
            status.push(word);
        }
    }
    let status = if status.is_empty() {
        NONE.to_string()
    } else {
        status.join(" · ")
    };
    let mut rows = vec![
        (
            "Rolle",
            if p.is_admin { "Admin" } else { "Mitglied" }.to_string(),
        ),
        ("Status", status),
        ("Ping (Server)", ms(p.rtt_ms)),
    ];
    match p.mode {
        Some(mode) => {
            rows.push((
                "Modus",
                match mode {
                    Mode::Ptt => "Push-to-Talk",
                    Mode::Vad => "Sprachaktivierung",
                }
                .to_string(),
            ));
            rows.push(("Ping (eigen)", ms(p.client_rtt_ms)));
        }
        None => rows.push((
            "Verlust",
            p.loss_permille
                .map_or_else(|| NONE.to_string(), |l| format!("{l} ‰")),
        )),
    }
    rows.push((
        "Nutzer · Sitzung",
        format!("#{} · {}", p.user_id, p.session),
    ));

    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(40.0);
        egui::Grid::new(("details", p.user_id))
            .num_columns(2)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                for (label, value) in rows {
                    ui.label(RichText::new(label).size(12.0).color(tok.dim_text));
                    ui.label(RichText::new(value).monospace().size(12.0));
                    ui.end_row();
                }
            });
    });
    if let Some(mut v) = p.volume {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(40.0);
            widgets::rtl(ui, 22.0, |ui| {
                widgets::chip(ui, &format!("{v:.0} %"), 46.0);
                if widgets::slider(ui, &mut v, 0.0..=200.0).changed() {
                    send(Command::SetVolume(p.user_id, v));
                }
            });
        });
    }
    ui.add_space(4.0);
}

/// The empty state: connected, but the only person here is you.
fn alone(ui: &mut egui::Ui) {
    let tok = theme::tokens(ui.visuals());
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.label(RichText::new(icon::USERS).size(30.0).color(tok.dim_text));
        ui.add_space(2.0);
        ui.label(RichText::new("Allein hier").size(12.5).color(tok.dim_text));
    });
}

// ---------------------------------------------------------------------------
// Enrollment window
// ---------------------------------------------------------------------------

/// Shows a window that takes a registration bundle (or a passport) and turns
/// it into an identity.
pub fn run_enroll(
    paths: Paths,
    server_addr: Option<SocketAddr>,
) -> Result<Option<Identity>, String> {
    let result: Arc<Mutex<Option<Identity>>> = Arc::new(Mutex::new(None));
    let result_app = result.clone();
    let enroll_theme = crate::config::Settings::load(&paths)
        .map(|s| s.ui.theme)
        .unwrap_or_else(|_| "system".to_string());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ICU — Einladung")
            .with_app_id("icu")
            .with_inner_size([560.0, 600.0])
            .with_min_inner_size([480.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ICU",
        options,
        Box::new(move |cc| {
            fonts::install(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx, &enroll_theme);
            Ok(Box::new(EnrollApp {
                paths,
                server_addr,
                link: String::new(),
                name: String::new(),
                status: None,
                step: EnrollStep::Paste,
                focus_name: false,
                was_valid: false,
                pending: None,
                result: result_app,
            }))
        }),
    )
    .map_err(|e| e.to_string())?;
    let out = result.lock().unwrap().take();
    Ok(out)
}

/// Where the registration thread drops its result for the UI thread.
type EnrollSlot = Arc<Mutex<Option<Result<Identity, String>>>>;

/// What the enrollment window has to say, if anything.
enum EnrollStatus {
    /// Talking to the server; the button is disabled until this resolves.
    Busy(String),
    /// The attempt failed, with the reason.
    Failed(String),
}

/// The enrollment window walks through one question at a time: paste, check
/// where it leads, pick a name (invites only), confirm.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EnrollStep {
    /// The paste field, nothing else.
    Paste,
    /// Only what the pasted text says: server, address, kind, name.
    Check,
    /// The own name; skipped for a passport, which already carries one.
    Name,
    /// Everything together, and the button that joins.
    Confirm,
}

struct EnrollApp {
    paths: Paths,
    server_addr: Option<SocketAddr>,
    link: String,
    /// The name the user picks. It is what the CSR asks for, so it ends up in
    /// the certificate (`PROTOCOL.md` §2.3). Starts empty and is required.
    name: String,
    status: Option<EnrollStatus>,
    step: EnrollStep,
    /// Put the cursor into the name field on the next frame.
    focus_name: bool,
    /// Whether the pasted text parsed last frame: a paste that turns it valid
    /// moves on by itself, going back to the field does not bounce forward.
    was_valid: bool,
    /// The in-flight registration's result slot. `Some` means a thread is
    /// running: without it a second click would start a second registration
    /// with a bundle the first one is already spending.
    pending: Option<EnrollSlot>,
    result: Arc<Mutex<Option<Identity>>>,
}

impl EnrollApp {
    /// Reads the pasted text and either takes it as it is (a passport) or
    /// starts the registration thread (a registration bundle).
    fn submit(&mut self) {
        let identity = match Identity::from_passport(&self.link) {
            Ok(identity) => identity,
            Err(e) => {
                self.status = Some(EnrollStatus::Failed(e));
                return;
            }
        };
        if !identity.is_registration() {
            // A passport is a local file, not an invite — there is no server
            // round trip to make. Through the same slot as a registration, so
            // the one place that saves and closes the window stays the only
            // one.
            let slot: EnrollSlot = Arc::new(Mutex::new(Some(Ok(identity))));
            self.pending = Some(slot);
            return;
        }
        self.status = Some(EnrollStatus::Busy(format!(
            "Verbinde mit {}:{}…",
            identity.server.host, identity.server.port
        )));
        let slot: EnrollSlot = Arc::new(Mutex::new(None));
        self.pending = Some(slot.clone());
        let addr = self.server_addr;
        let name = icu_proto::normalize_name(&self.name);
        let client = crate::engine::client_string();
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            let r = rt
                .block_on(register(&identity, &name, &client, addr, &paths))
                .map_err(|e| {
                    tracing::warn!(error = %e, "registration failed");
                    e.problem().text
                });
            *slot.lock().unwrap() = Some(r);
        });
    }
}

impl eframe::App for EnrollApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(100));

        // Taken out of the slot first: holding the lock across the match
        // would keep `self.pending` borrowed while it is being cleared.
        let done = self
            .pending
            .as_ref()
            .and_then(|slot| slot.lock().unwrap().take());
        if let Some(done) = done {
            self.pending = None;
            match done {
                Ok(identity) => match identity.save(&self.paths) {
                    Ok(()) => {
                        *self.result.lock().unwrap() = Some(identity);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Err(e) => {
                        self.status = Some(EnrollStatus::Failed(
                            crate::problem::storage("Die Anmeldung", &e).text,
                        ))
                    }
                },
                Err(e) => self.status = Some(EnrollStatus::Failed(e)),
            }
        }
        let busy = self.pending.is_some();
        let tok = theme::tokens(root.visuals());

        // A passport carries its own certificate; a registration bundle
        // needs a name, which the user types in: the placeholder is the
        // inviter's, not theirs.
        let parsed = Identity::from_passport(&self.link).ok();
        // Where the pasted text leads, shown before joining: it brings its
        // own CA and address (`PROTOCOL.md` §2.3.1).
        let summary = parsed
            .as_ref()
            .map(|id| crate::config::ImportSummary::of(id, None));
        // The CA the client will hold the server to: the one thing in the
        // pasted text worth checking with whoever sent it.
        let ca = parsed
            .as_ref()
            .map(|id| crate::conninfo::CertInfo::from_der(&id.ca_der));
        let registration = parsed.filter(Identity::is_registration);
        let needs_name = registration.is_some();
        let valid = summary.is_some();
        let has_text = !self.link.trim().is_empty();
        // The later steps only make sense for text that parses; if it stops
        // parsing, start over at the field.
        if !valid {
            self.step = EnrollStep::Paste;
        } else if !self.was_valid && self.step == EnrollStep::Paste {
            self.step = EnrollStep::Check;
        }
        self.was_valid = valid;
        let name_ok = !icu_proto::normalize_name(&self.name).is_empty();
        let step = self.step;
        let (forward_ok, forward_glyph, forward_label) = match step {
            // No button: a valid paste moves on by itself.
            EnrollStep::Paste => (false, icon::ARROW_RIGHT, ""),
            EnrollStep::Check => (true, icon::CHECK, "Stimmt, weiter"),
            EnrollStep::Name => (name_ok, icon::ARROW_RIGHT, "Weiter"),
            EnrollStep::Confirm if busy => (false, icon::SIGN_IN, "Verbinde…"),
            EnrollStep::Confirm => (
                !needs_name || name_ok,
                icon::SIGN_IN,
                "Diesem Server beitreten",
            ),
        };
        let mut forward = false;
        let mut back = false;

        // The actions sit at the bottom edge, so the window has a floor
        // instead of trailing off into empty panel. The paste step has none.
        if step != EnrollStep::Paste {
            egui::Panel::bottom("enroll_actions")
                .frame(
                    egui::Frame::new()
                        .fill(root.visuals().panel_fill)
                        .inner_margin(egui::Margin {
                            left: 24,
                            right: 24,
                            top: 14,
                            bottom: 18,
                        }),
                )
                .show(root, |ui| {
                    ui.horizontal(|ui| {
                        let justified =
                            egui::Layout::centered_and_justified(egui::Direction::LeftToRight);
                        back = ui
                            .allocate_ui_with_layout(egui::vec2(120.0, 36.0), justified, |ui| {
                                ui.add_enabled_ui(!busy, |ui| {
                                    widgets::button(
                                        ui,
                                        widgets::ButtonKind::Neutral,
                                        icon::ARROW_LEFT,
                                        "Zurück",
                                    )
                                })
                                .inner
                            })
                            .inner
                            .clicked();
                        // Not-ready is neutral grey rather than a faded accent,
                        // which read as a broken button.
                        let kind = if forward_ok {
                            widgets::ButtonKind::Primary
                        } else {
                            widgets::ButtonKind::Neutral
                        };
                        forward = ui
                            .allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), 36.0),
                                justified,
                                |ui| {
                                    ui.add_enabled_ui(forward_ok, |ui| {
                                        widgets::button(ui, kind, forward_glyph, forward_label)
                                    })
                                    .inner
                                },
                            )
                            .inner
                            .clicked();
                    });
                });
        }

        let steps = if needs_name { 4 } else { 3 };
        let (n, subtitle) = match step {
            EnrollStep::Paste => (
                1,
                "Füge die Einladung ein, die du bekommen hast — oder deinen Passport \
                 von einem anderen Gerät.",
            ),
            EnrollStep::Check => (2, "Das steht darin. Stimmt das?"),
            EnrollStep::Name => (3, "Wie sollen dich die anderen sehen?"),
            EnrollStep::Confirm => (steps, "Alles noch einmal zusammen. Dann beitreten."),
        };

        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(root.style()).inner_margin(egui::Margin {
                    left: 24,
                    right: 24,
                    top: 22,
                    bottom: 8,
                }),
            )
            .show(root, |ui| {
                // An error banner wraps to however many lines it needs, so the
                // content can outgrow the window; it scrolls rather than clips.
                let out = egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (badge, _) =
                            ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                        ui.painter()
                            .rect_filled(badge, theme::R_CARD, tok.accent_tint_2);
                        ui.painter().text(
                            badge.center(),
                            egui::Align2::CENTER_CENTER,
                            icon::WAVEFORM,
                            egui::FontId::new(22.0, egui::FontFamily::Proportional),
                            tok.accent,
                        );
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 3.0;
                            ui.label(
                                RichText::new("Mit ICU verbinden")
                                    .text_style(egui::TextStyle::Heading),
                            );
                            ui.label(RichText::new(subtitle).size(12.5).color(tok.muted_text));
                        });
                    });
                    ui.add_space(14.0);
                    ui.label(
                        theme::caps_text(&format!("Schritt {n} von {steps}")).color(tok.dim_text),
                    );
                    ui.add_space(6.0);

                    match step {
                        EnrollStep::Paste => self.paste_step(ui, &tok, busy, valid, needs_name),
                        EnrollStep::Check => {
                            if let Some(summary) = &summary {
                                let mut rows = vec![
                                    ("Server", summary.server_name.clone()),
                                    ("Adresse", summary.address.clone()),
                                    (
                                        "Art",
                                        if needs_name { "Einladung" } else { "Passport" }.into(),
                                    ),
                                ];
                                // An invite's placeholder name is not the
                                // user's; they pick their own next.
                                if !needs_name {
                                    rows.push(("Name", summary.name.clone()));
                                }
                                enroll_card(ui, &tok, "Einladung", &rows, Some(JOIN_NOTE));
                            }
                            if let Some(ca) = &ca {
                                ui.add_space(12.0);
                                // 32 pairs on two lines, so it reads in halves.
                                let pairs: Vec<&str> = ca.fingerprint.split(':').collect();
                                let fingerprint = pairs
                                    .chunks(16)
                                    .map(|c| c.join(":"))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                let until = if ca.expires_in_days < 0 {
                                    format!("{} (abgelaufen)", ca.not_after)
                                } else {
                                    format!("{} (noch {} Tage)", ca.not_after, ca.expires_in_days)
                                };
                                let rows = [
                                    ("Name", ca.subject.clone()),
                                    ("SHA-256", fingerprint),
                                    ("Gültig ab", ca.not_before.clone()),
                                    ("Gültig bis", until),
                                    ("Signatur", ca.sig_alg.clone()),
                                ];
                                enroll_card(
                                    ui,
                                    &tok,
                                    "Zertifizierungsstelle (CA)",
                                    &rows,
                                    Some(
                                        "Der Server muss sich beim Beitreten mit einem Zertifikat \
                                         dieser CA ausweisen, sonst bricht ICU ab. Den \
                                         Fingerabdruck im Zweifel beim Betreiber gegenprüfen.",
                                    ),
                                );
                            }
                        }
                        EnrollStep::Name => {
                            ui.label(theme::caps_text("Dein Name"));
                            ui.add_space(4.0);
                            let field = ui.add_enabled(
                                !busy,
                                egui::TextEdit::singleline(&mut self.name)
                                    .desired_width(f32::INFINITY)
                                    .margin(egui::Margin::symmetric(10, 8))
                                    .hint_text("Name"),
                            );
                            if std::mem::take(&mut self.focus_name) {
                                field.request_focus();
                            }
                            if name_ok
                                && field.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                forward = true;
                            }
                            ui.label(
                                RichText::new(
                                    "So sehen dich alle anderen. Steht später in deinem \
                                     Zertifikat.",
                                )
                                .size(11.5)
                                .color(tok.dim_text),
                            );
                        }
                        EnrollStep::Confirm => {
                            if let Some(summary) = &summary {
                                let name = if needs_name {
                                    icu_proto::normalize_name(&self.name)
                                } else {
                                    summary.name.clone()
                                };
                                let rows = [
                                    ("Server", summary.server_name.clone()),
                                    ("Adresse", summary.address.clone()),
                                    ("Name", name),
                                ];
                                enroll_card(ui, &tok, "Zusammenfassung", &rows, Some(JOIN_NOTE));
                            }
                            ui.add_space(10.0);
                            match &self.status {
                                Some(EnrollStatus::Busy(text)) => {
                                    ui.label(RichText::new(text).size(12.0).color(tok.muted_text));
                                }
                                Some(EnrollStatus::Failed(text)) => {
                                    widgets::banner(
                                        ui,
                                        widgets::BannerKind::Error,
                                        "Beitritt fehlgeschlagen",
                                        text,
                                        None::<fn()>,
                                    );
                                }
                                None => {}
                            }
                        }
                    }
                });
                // Past the paste field the content has a natural height: grow
                // the window to show all of it instead of a scroll bar.
                let overflow = out.content_size.y - out.inner_rect.height();
                if step != EnrollStep::Paste && overflow > 1.0 {
                    // Not `viewport().inner_rect`: `None` on Wayland.
                    {
                        let r = ctx.content_rect();
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                            r.width(),
                            (r.height() + overflow).ceil(),
                        )));
                    }
                }
            });

        if back && !busy {
            self.status = None;
            self.step = match step {
                EnrollStep::Paste | EnrollStep::Check => EnrollStep::Paste,
                EnrollStep::Name => EnrollStep::Check,
                EnrollStep::Confirm if needs_name => EnrollStep::Name,
                EnrollStep::Confirm => EnrollStep::Check,
            };
        } else if forward && !busy {
            match step {
                EnrollStep::Paste if valid => self.step = EnrollStep::Check,
                EnrollStep::Check if needs_name => {
                    self.step = EnrollStep::Name;
                    self.focus_name = true;
                }
                EnrollStep::Check => self.step = EnrollStep::Confirm,
                EnrollStep::Name if name_ok => self.step = EnrollStep::Confirm,
                EnrollStep::Confirm if has_text => self.submit(),
                _ => {}
            }
        }
    }
}

impl EnrollApp {
    /// Step one: the paste field, whose border says whether what is in it
    /// works.
    fn paste_step(
        &mut self,
        ui: &mut egui::Ui,
        tok: &theme::Tokens,
        busy: bool,
        valid: bool,
        needs_name: bool,
    ) {
        let has_text = !self.link.trim().is_empty();
        ui.label(theme::caps_text("Einladung"));
        ui.add_space(4.0);
        // The field takes whatever height the window has left, minus the
        // verdict line under it and the well's own margins; a pasted bundle
        // (~20 armored lines) scrolls inside it.
        const VERDICT_H: f32 = 32.0;
        let height = (ui.clip_rect().bottom() - ui.cursor().top() - VERDICT_H - 20.0).max(120.0);
        let well = egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(egui::CornerRadius::same(theme::R_ROW))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("enroll-link")
                    .max_height(height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_enabled(
                            !busy,
                            egui::TextEdit::multiline(&mut self.link)
                                .frame(egui::Frame::NONE)
                                .margin(egui::Margin::ZERO)
                                .desired_width(f32::INFINITY)
                                .min_size(egui::vec2(0.0, height))
                                .font(egui::FontId::new(12.0, egui::FontFamily::Monospace))
                                .hint_text(
                                    RichText::new("Einladung hier einfügen (Strg+V)")
                                        .size(13.0)
                                        .color(tok.dim_text),
                                ),
                        )
                    })
                    .inner
            });
        let border = if valid {
            tok.connected
        } else if has_text {
            tok.error.gamma_multiply(0.6)
        } else if well.inner.has_focus() {
            tok.accent
        } else {
            ui.visuals().widgets.inactive.bg_stroke.color
        };
        ui.painter().rect_stroke(
            well.response.rect,
            egui::CornerRadius::same(theme::R_ROW),
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let (glyph, text, color) = if valid {
            let what = if needs_name {
                "Einladung erkannt"
            } else {
                "Passport erkannt"
            };
            (icon::CHECK, what, tok.connected)
        } else if has_text {
            (
                icon::WARNING_CIRCLE,
                "Das ist keine gültige ICU-Einladung.",
                tok.error,
            )
        } else {
            (
                icon::QUESTION,
                "Noch keine Einladung? Frag die Person, die den Server betreibt.",
                tok.dim_text,
            )
        };
        ui.add_space(2.0);
        ui.label(
            RichText::new(format!("{glyph}  {text}"))
                .size(12.0)
                .color(color),
        );
    }
}

/// The reminder under the server cards.
const JOIN_NOTE: &str = "Nur beitreten, wenn das der Server ist, den du erwartest.";

/// Key/value rows in a card, with an optional note under a separator.
fn enroll_card(
    ui: &mut egui::Ui,
    tok: &theme::Tokens,
    title: &str,
    rows: &[(&str, String)],
    note: Option<&str>,
) {
    widgets::group_card(ui, title, |ui| {
        widgets::card_note(ui, |ui| {
            egui::Grid::new(("enroll-card", title))
                .num_columns(2)
                .spacing(egui::vec2(16.0, 6.0))
                .show(ui, |ui| {
                    for (key, value) in rows {
                        ui.label(RichText::new(*key).size(12.0).color(tok.dim_text));
                        ui.label(RichText::new(value).size(12.5).monospace());
                        ui.end_row();
                    }
                });
        });
        if let Some(note) = note {
            ui.separator();
            widgets::card_note(ui, |ui| {
                ui.label(
                    RichText::new(format!("{}  {note}", icon::SHIELD_STAR))
                        .size(11.5)
                        .color(tok.dim_text),
                );
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Peer;
    use icu_proto::{UserFlags, UserInfo};
    use widgets::RecDot;

    fn peer(session: u16, name: &str, flags: u8) -> Peer {
        Peer {
            info: UserInfo {
                user_id: u32::from(session),
                session,
                name: name.into(),
                flags: UserFlags(flags),
            },
            speaking: false,
            volume: 100.0,
            rtt_ms: None,
            recv_loss_permille: None,
        }
    }

    #[test]
    fn rtt_bars_fall_with_the_round_trip() {
        assert_eq!(rtt_bars(1), 5);
        assert_eq!(rtt_bars(40), 5);
        assert_eq!(rtt_bars(41), 4);
        assert_eq!(rtt_bars(SLOW_RTT_MS), 3);
        assert_eq!(rtt_bars(SLOW_RTT_MS + 1), 2);
        assert_eq!(rtt_bars(2 * SLOW_RTT_MS + 1), 1);
    }

    #[test]
    fn rec_dot_is_live_only_while_somebody_sees_the_picture() {
        let watching = [
            peer(1, "Anna", UserFlags::VIEWING),
            peer(2, "Carl", 0),
            peer(3, "Bob", UserFlags::VIEWING),
        ];
        assert_eq!(
            camera_feedback(false, true, &watching, VideoTier::T0),
            (RecDot::Off, None)
        );
        assert_eq!(
            camera_feedback(true, false, &watching, VideoTier::T0).0,
            RecDot::Idle
        );
        let away = [peer(2, "Carl", UserFlags::CAMERA_ON)];
        let (dot, hint) = camera_feedback(true, true, &away, VideoTier::T0);
        assert_eq!(dot, RecDot::Idle);
        assert!(hint.unwrap().starts_with("Niemand schaut zu"));
        assert_eq!(
            camera_feedback(true, true, &watching, VideoTier::T4).0,
            RecDot::Idle
        );
        assert_eq!(
            camera_feedback(true, true, &watching, VideoTier::T1),
            (RecDot::Live, Some("Sichtbar für: Anna, Bob".into()))
        );
    }
}
