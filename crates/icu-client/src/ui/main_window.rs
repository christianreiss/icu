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

use super::{
    connection::{opt, wrap_fingerprint, ConnectionView, NONE},
    fonts,
    settings::SettingsView,
    theme, widgets,
};
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

/// What the settings window opens with: every page fits without a scroll
/// bar at this size unless expert rows or a long member list make it grow
/// (`tests/ui_layout.rs`).
pub const SETTINGS_SIZE: [f32; 2] = [820.0, 680.0];
/// What the Verbindung window opens with: its first page fits; the longer
/// ones grow it.
pub const CONNECTION_SIZE: [f32; 2] = [720.0, 780.0];

/// A microphone peak at or above this reads as clipping.
const CLIP_DB: f32 = -1.0;

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
            let mut app = App::new(cc, engine_for_app, debug_stats, logged_out_app);
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
    /// Header, people and control dock.
    view: MainView,
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
        // Shrinking is only right once, right after connecting; afterwards
        // the window only grows (`super::grow_to_fit`).
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
        logged_out: Arc<AtomicBool>,
    ) -> App {
        // Everything the window needs from the engine, taken under one
        // lock; the threads started below get their own handles.
        let guard = engine.lock().unwrap();
        let e = guard.as_ref().expect("engine");
        let state_rx = e.state();
        let cmd = e.command_sender();
        let stats = e.stats();
        let frames = e.video_frames_source();
        #[cfg(feature = "camera")]
        let camera_link = e.camera_link();
        drop(guard);
        let mut state = state_rx.borrow().clone();
        sort_peers(&mut state);
        let theme_name = state.settings.ui.theme.clone();
        let start_minimized = state.settings.ui.start_minimized;

        fonts::install(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &theme_name);
        wake_on_change(state_rx.clone(), cc.egui_ctx.clone());
        // A later start of ICU hands over here (`crate::instance`); woken,
        // `logic` runs even while the window is hidden to the tray.
        let ctx = cc.egui_ctx.clone();
        crate::instance::set_waker(move || ctx.request_repaint());
        #[cfg(feature = "tray")]
        let tray = crate::tray::Tray::start(cmd.clone(), cc.egui_ctx.clone()).ok();
        #[cfg(feature = "overlay")]
        let overlay = match crate::overlay::OverlayHandle::start(state_rx.clone(), frames.clone()) {
            Ok(h) => {
                tracing::info!("overlay thread started (layer shell available)");
                Some(h)
            }
            Err(err) => {
                tracing::warn!(%err, "overlay unavailable");
                None
            }
        };
        #[cfg(feature = "camera")]
        let (camera, self_video) = {
            let handle = crate::camera::CameraHandle::new(camera_link);
            let preview = crate::camera::CameraPreview::new(handle.preview_source());
            if state.settings.camera.enabled {
                let _ = cmd.send(Command::SetCamera(true));
            }
            (Some(handle), Some(preview))
        };
        let videos = crate::video::VideoTextures::new(frames);
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
            view: MainView::default(),
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
            logged_out,
        }
    }
}

/// Wakes the window whenever the engine's state changes. A visible window
/// repaints on its own timer; one hidden to the tray runs `App::logic` only
/// when asked, and without this the tray icon would keep showing the state
/// from when the window was hidden (a hotkey mute, a lost connection).
/// Ends with the engine, when the sender is dropped.
fn wake_on_change(mut state: tokio::sync::watch::Receiver<AppState>, ctx: egui::Context) {
    let spawned = std::thread::Builder::new()
        .name("icu-ui-wake".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().build() else {
                return;
            };
            rt.block_on(async {
                while state.changed().await.is_ok() {
                    // Not immediate: a visible window repaints every 33 ms
                    // anyway, and the levels change 20 times a second.
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            });
        });
    if let Err(e) = spawned {
        tracing::warn!(%e, "no wake-up thread; the tray may lag while hidden");
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
        if let Some(tray) = &mut self.tray {
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
        // ICU started again: come to the front, like „Fenster anzeigen“. A
        // handed-over invite or passport (`request.bundle`) is not taken
        // over from here yet.
        if let Some(request) = crate::instance::poll() {
            self.hidden = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            if request.bundle.is_some() {
                tracing::info!("a handed-over invite or passport is not taken over here");
            }
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

        let videos = &self.videos;
        let env = MainEnv {
            cmd: &self.cmd,
            video_tier: VideoTier::from_u8(self.stats.video_tier.load(Ordering::Relaxed) as u8),
            video: &|session| videos.get(session),
            self_video,
            settings_open: self.settings.is_some(),
            overlay_available: self.overlay_available(),
        };
        let out = self.view.ui(root, &self.state, &env);

        if out.toggle_settings {
            self.settings = match self.settings.take() {
                Some(_) => None,
                None => Some(self.new_settings_view()),
            };
        }
        if out.open_import {
            if self.settings.is_none() {
                self.settings = Some(self.new_settings_view());
            }
            if let Some(view) = &mut self.settings {
                view.open_import();
            }
        }
        if out.toggle_connection {
            self.connection = match self.connection.take() {
                Some(_) => None,
                None => Some(ConnectionView::new(self.stats.clone())),
            };
        }
        self.fit_to_list(&ctx, out.list_overflow);

        if let Some(next) = out.theme {
            self.theme = next.to_string();
            theme::apply(&ctx, next);
            let mut settings = self.state.settings.clone();
            settings.ui.theme = next.to_string();
            if let Some(view) = &mut self.settings {
                view.set_theme(next);
            }
            let _ = self.cmd.send(Command::UpdateSettings(Box::new(settings)));
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
    /// A fresh settings window over the current state.
    fn new_settings_view(&self) -> SettingsView {
        let paths = self
            .engine
            .lock()
            .unwrap()
            .as_ref()
            .map(|e| e.paths())
            .unwrap_or_else(|| Paths::new(None));
        #[cfg_attr(not(feature = "camera"), allow(unused_mut))]
        let mut view = SettingsView::new(&self.state, self.stats.clone(), paths)
            .with_overlay_available(self.overlay_available());
        #[cfg(feature = "camera")]
        if let Some(camera) = &self.camera {
            view = view.with_preview(camera.preview_source());
        }
        view
    }

    /// Whether the overlay thread is up: compositors without layer shell
    /// (GNOME) have none.
    fn overlay_available(&self) -> bool {
        #[cfg(feature = "overlay")]
        return self.overlay.is_some();
        #[cfg(not(feature = "overlay"))]
        false
    }

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
        let stats_text = &self.stats_text;
        self.debug_window_open = immediate_window(
            ctx,
            "icu-debug",
            "ICU — Debug",
            [540.0, 440.0],
            None,
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(RichText::new(stats_text).monospace().small());
                });
            },
        );
    }

    /// The Verbindung window, drawn into its own immediate viewport.
    /// Simpler than [`Self::settings_window`]: the view is read-only, so
    /// there are no commands to drain and no theme change to react to.
    fn connection_window(&mut self, ctx: &egui::Context) {
        if self.connection.is_none() {
            return;
        }
        // Plain locals, not `self` fields — the closure would otherwise
        // capture the whole of `self` and collide `&self.state` against
        // `&mut self.connection` (see `settings_window`).
        let state = &self.state;
        let view = &mut self.connection;
        let still_open = immediate_window(
            ctx,
            "icu-connection",
            "ICU — Verbindung",
            CONNECTION_SIZE,
            Some([560.0, 420.0]),
            |ui| {
                view.as_mut().expect("checked above").ui(ui, state);
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
        // Plain locals, not `self` fields — read/write freely inside the
        // closure with no borrow conflict against `self.settings`.
        let debug_open = self.debug_window_open;
        let state = &self.state;
        let settings = &mut self.settings;
        let mut out = None;
        let still_open = immediate_window(
            ctx,
            "icu-settings",
            "ICU — Einstellungen",
            SETTINGS_SIZE,
            Some([640.0, 480.0]),
            |ui| {
                out = Some(
                    settings
                        .as_mut()
                        .expect("checked above")
                        .ui(ui, state, debug_open),
                );
            },
        );
        let Some(out) = out else {
            return;
        };
        for c in out.cmds {
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
        if out.logout {
            self.logged_out.store(true, Ordering::Relaxed);
            self.settings = None;
            self.quit_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !still_open {
            self.settings = None;
        }
        if out.debug_window_open != self.debug_window_open {
            self.debug_window_open = out.debug_window_open;
            if self.debug_window_open {
                self.stats_text = self.stats.report();
                self.last_stats = Instant::now();
            }
        }
    }
}

/// One of the app's secondary windows, drawn into its own immediate
/// viewport with `body` as its central panel. `min_size` makes it
/// resizable. Returns `false` once the user closed it.
fn immediate_window(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    size: [f32; 2],
    min_size: Option<[f32; 2]>,
    body: impl FnOnce(&mut egui::Ui),
) -> bool {
    let mut builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(size);
    if let Some(min) = min_size {
        builder = builder.with_min_inner_size(min).with_resizable(true);
    }
    let mut still_open = true;
    // The viewport callback is `FnMut`; `body` runs once per call.
    let mut body = Some(body);
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of(id),
        builder,
        |vctx, _class| {
            if let Some(body) = body.take() {
                egui::CentralPanel::default().show(vctx, body);
            }
            if vctx.input(|i| i.viewport().close_requested()) {
                still_open = false;
            }
        },
    );
    still_open
}

// ---------------------------------------------------------------------------
// Main view
// ---------------------------------------------------------------------------

/// The main window's content — header, people, control dock — drawn from
/// an [`AppState`] and a few handles, so it renders without a running app
/// (`tests/ui_layout.rs`). The windows, tray, overlay and camera around it
/// stay in `App`.
#[derive(Default)]
pub struct MainView {
    /// The participant row whose details are unfolded.
    expanded: Option<icu_proto::UserId>,
    /// The on-screen speak button is being held.
    ptt_held: bool,
}

/// What [`MainView::ui`] needs besides the state.
pub struct MainEnv<'a> {
    /// Where the view's commands go.
    pub cmd: &'a tokio::sync::mpsc::UnboundedSender<Command>,
    /// The own camera's current quality tier (`crate::quality`).
    pub video_tier: VideoTier,
    /// Somebody's live picture, by session; `None` draws the initials.
    pub video: &'a dyn Fn(icu_proto::Session) -> Option<egui::load::SizedTexture>,
    /// The own camera's preview, while it is on.
    pub self_video: Option<egui::load::SizedTexture>,
    /// The settings window is open, which the gear shows.
    pub settings_open: bool,
    /// The overlay could start (a layer-shell compositor): without it the
    /// Overlay tile would switch nothing, so it is left out.
    pub overlay_available: bool,
}

/// What one frame of the main view asks of the app around it.
#[derive(Debug, Default)]
pub struct MainOutput {
    /// The gear was clicked: open or close the settings.
    pub toggle_settings: bool,
    /// The connection status was clicked: open or close its window.
    pub toggle_connection: bool,
    /// The header's theme button picked this theme.
    pub theme: Option<&'static str>,
    /// „Passport einspielen" under a fatal error: open the settings on the
    /// passport field.
    pub open_import: bool,
    /// How much taller the people list is than the room it got; negative
    /// when it is shorter. The window grows by it (`App::fit_to_list`).
    pub list_overflow: f32,
}

impl MainView {
    /// Draws the whole window into `root`.
    pub fn ui(&mut self, root: &mut egui::Ui, state: &AppState, env: &MainEnv<'_>) -> MainOutput {
        let mut out = MainOutput::default();
        let send = |c: Command| {
            let _ = env.cmd.send(c);
        };

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
                        if widgets::status_link(ui, color, &short)
                            .on_hover_ui(|ui| {
                                ui.label(if detail.is_empty() {
                                    format!("{short}\n\nKlicken für Verbindungsdetails")
                                } else {
                                    format!("{short} — {detail}\n\nKlicken für Verbindungsdetails")
                                });
                            })
                            .clicked()
                        {
                            out.toggle_connection = true;
                        }

                        // Own outgoing video quality tier, set by
                        // `crate::quality::QualityController`. Automatic, no
                        // settings toggle (`Research/feature-freeze.md`'s bias
                        // against exposing adaptive behavior as a dial) — but
                        // shown, since a tier-down is visible to other people as
                        // this client's video degrading.
                        let hint = match env.video_tier {
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
                            if widgets::icon_button(ui, icon::GEAR_FINE, HEADER, env.settings_open)
                                .on_hover_text("Einstellungen")
                                .clicked()
                            {
                                out.toggle_settings = true;
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
                                out.theme = Some(next);
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
                    ui.add_space(2.0);
                    ui.hyperlink_to(
                        format!("{}  Neue Version herunterladen", icon::DOWNLOAD_SIMPLE),
                        concat!(env!("CARGO_PKG_REPOSITORY"), "/releases"),
                    );
                }
                // Non-techies do not hover: a connection that will not come
                // back on its own says why right here, not only in the
                // pill's tooltip.
                let reconnect = |ui: &mut egui::Ui| {
                    ui.add_space(4.0);
                    if widgets::button(
                        ui,
                        widgets::ButtonKind::Neutral,
                        icon::ARROW_CLOCKWISE,
                        "Neu verbinden",
                    )
                    .clicked()
                    {
                        send(Command::Reconnect);
                    }
                };
                match &state.conn {
                    ConnStatus::Fatal(problem) => {
                        widgets::banner(
                            ui,
                            widgets::BannerKind::Error,
                            "Keine Verbindung",
                            &problem.text,
                            None::<fn()>,
                        );
                        // A renewed certificate on another device, a new
                        // server: the way back is often a fresh passport,
                        // one click away instead of three pages deep.
                        ui.horizontal(|ui| {
                            reconnect(ui);
                            ui.add_space(4.0);
                            if widgets::button(
                                ui,
                                widgets::ButtonKind::Neutral,
                                icon::DOWNLOAD,
                                "Passport einspielen",
                            )
                            .clicked()
                            {
                                out.open_import = true;
                            }
                        });
                    }
                    ConnStatus::Disconnected { reason, attempt } if *attempt >= 2 => {
                        widgets::banner(
                            ui,
                            widgets::BannerKind::Warning,
                            "Verbindung unterbrochen",
                            &reason.text,
                            None::<fn()>,
                        );
                        reconnect(ui);
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
                    env.video_tier,
                );
                let toggles: [Toggle; 4] = [
                    (
                        icon::MICROPHONE_SLASH,
                        "Stumm",
                        state.muted,
                        widgets::Role::Danger,
                        widgets::RecDot::Off,
                        state
                            .restricted_muted
                            .then(|| "Ein Admin hat dich stummgeschaltet.".to_string()),
                        Some(Command::SetMuted(!state.muted)),
                    ),
                    (
                        icon::SPEAKER_SLASH,
                        "Taub",
                        state.deafened,
                        widgets::Role::Danger,
                        widgets::RecDot::Off,
                        state
                            .restricted_deafened
                            .then(|| "Ein Admin hat dich taubgeschaltet.".to_string()),
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
                        (cfg!(feature = "overlay") && env.overlay_available)
                            .then_some(Command::SetOverlay(!state.settings.overlay.enabled)),
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
        let own = state
            .me
            .as_ref()
            .map(|_| [state.deafened, state.muted, state.camera_on, state.is_admin]);
        let columns = badge_columns(own.into_iter().chain(state.peers.iter().map(|p| {
            let f = p.info.flags;
            [f.deafened(), f.muted(), f.camera_on(), f.is_admin()]
        })));
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
                                        ptt: Mode::from(state.settings.audio.mode) == Mode::Ptt,
                                        volume: None,
                                        is_admin: state.is_admin,
                                        viewing: watches(me.flags.viewing(), state.camera_on),
                                        rtt_ms: state.server_rtt_ms,
                                        client_rtt_ms: state.rtt_ms.map(|ms| ms.round() as u16),
                                        loss_permille: None,
                                        session: me.session,
                                        video: env.self_video,
                                        moderation: None,
                                    },
                                    &columns,
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
                                        ptt: p.info.flags.ptt(),
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
                                        moderation: state.is_admin.then(|| {
                                            let row = state
                                                .user_list
                                                .iter()
                                                .find(|u| u.user_id == p.info.user_id);
                                            Moderation {
                                                muted: row.is_some_and(|u| u.muted),
                                                deafened: row.is_some_and(|u| u.deafened),
                                            }
                                        }),
                                        // The flag arrives with `UserState`, so the
                                        // picture goes the moment somebody switches
                                        // off — not three seconds later when the
                                        // last frame goes stale.
                                        video: p
                                            .info
                                            .flags
                                            .camera_on()
                                            .then(|| (env.video)(p.info.session))
                                            .flatten(),
                                    },
                                    &columns,
                                    expanded,
                                    &send,
                                );
                            }
                            if state.peers.is_empty() {
                                empty(ui, &state.conn);
                            }
                        })
                })
                .inner
        });
        out.list_overflow = list.inner.content_size.y - list.inner.inner_rect.height();
        out
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
    let key = super::ptt_key_label(&state.settings.hotkeys, &state.hotkey_backend);
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
        talking_muted: state.muted && (state.voice || (ptt && state.ptt_down)),
        clipping: !offline && state.level.peak_db >= CLIP_DB,
    };
    // Built only while hovered: three floats formatted per frame otherwise.
    let tooltip = |ui: &mut egui::Ui| {
        if speak.talking_muted {
            ui.label("Du bist stummgeschaltet — niemand hört dich.");
        }
        if speak.clipping {
            ui.label("Übersteuert — leiser sprechen oder das Mikrofon herunterregeln.");
        }
        ui.label(if offline {
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
        });
    };

    let size = egui::vec2(ui.available_width() - tile_w - DOCK_GAP, SPEAK_H);
    let response = widgets::speak_surface(ui, &speak, size).on_hover_ui(tooltip);
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
    /// `Some` only for the local user.
    mode: Option<Mode>,
    /// Talks by Push-to-Talk (`UserFlags::PTT`), voice activation otherwise.
    ptt: bool,
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
    /// `Some` when the viewer is an admin and this is somebody else: what
    /// an admin currently enforces on them, for the moderation menu.
    moderation: Option<Moderation>,
}

/// What an admin enforces on a participant (`AdminSetRestriction`), as the
/// last `UserListResponse` says.
#[derive(Clone, Copy)]
struct Moderation {
    muted: bool,
    deafened: bool,
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

/// A status icon that only some people show. Order = `BADGES` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Badge {
    Deafened,
    Muted,
    Camera,
    Admin,
}

const BADGES: [Badge; 4] = [Badge::Deafened, Badge::Muted, Badge::Camera, Badge::Admin];

/// The badge columns of the list, right to left: the badges someone in the
/// room shows right now, the most common one rightmost next to the columns
/// every row has, the rare ones further left. Ties keep `BADGES` order. Every
/// row draws the same columns, so the grid holds while it reorders.
fn badge_columns(rows: impl Iterator<Item = [bool; 4]>) -> Vec<Badge> {
    let mut counts = [0usize; 4];
    for row in rows {
        for (count, on) in counts.iter_mut().zip(row) {
            *count += usize::from(on);
        }
    }
    let mut columns: Vec<(usize, Badge)> = counts
        .into_iter()
        .zip(BADGES)
        .filter(|&(count, _)| count > 0)
        .collect();
    // Stable: equal counts keep `BADGES` order.
    columns.sort_by_key(|&(count, _)| std::cmp::Reverse(count));
    columns.into_iter().map(|(_, badge)| badge).collect()
}

fn participant_row(
    ui: &mut egui::Ui,
    p: Participant<'_>,
    columns: &[Badge],
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
                            .on_hover_ui(|ui| {
                                ui.label(format!("Ping {ms} ms"));
                            });
                        }
                        // Not the crossed-out speaker: that is the badge for
                        // "this person hears nothing". At 0 % the handle shows
                        // the speaker with an x — muted for you only.
                        if let Some(vol) = p.volume {
                            let glyph = if vol <= 0.0 {
                                icon::SPEAKER_X
                            } else {
                                icon::SPEAKER_HIGH
                            };
                            if widgets::icon_button(ui, glyph, 28.0, expanded)
                                .on_hover_ui(|ui| {
                                    ui.label(if vol <= 0.0 {
                                        "Für dich stumm".to_string()
                                    } else {
                                        format!("Lautstärke {vol:.0} %")
                                    });
                                })
                                .clicked()
                            {
                                *open = if expanded { None } else { Some(p.user_id) };
                            }
                        } else {
                            // Own row has no volume handle; keep its column so
                            // ping and icons line up with every other row. An
                            // allocation, not `add_space`: only an allocation
                            // gets the item spacing the button gets.
                            ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                        }
                        // A fixed grid: every row reserves every slot, lit or
                        // not, so each icon sits in the same column in every
                        // row and a missing one leaves a gap instead of
                        // shifting its neighbours.
                        let slot = |ui: &mut egui::Ui, text: Option<RichText>| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(18.0, 20.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| match text {
                                    Some(text) => ui.label(text),
                                    None => ui.allocate_response(
                                        ui.available_size(),
                                        egui::Sense::hover(),
                                    ),
                                },
                            )
                            .inner
                        };
                        let (mode_glyph, mode_hint) = if p.ptt {
                            (icon::HAND_TAP, "Push-to-Talk")
                        } else {
                            (icon::WAVEFORM, "Sprachaktivierung")
                        };
                        slot(
                            ui,
                            Some(RichText::new(mode_glyph).size(15.0).color(tok.dim_text)),
                        )
                        .on_hover_text(mode_hint);
                        for &badge in columns {
                            let (on, color, glyph, hint) = match badge {
                                Badge::Deafened => {
                                    (p.deafened, tok.error, icon::SPEAKER_SLASH, "hört nicht zu")
                                }
                                Badge::Muted => (
                                    p.muted,
                                    tok.error,
                                    icon::MICROPHONE_SLASH,
                                    "stummgeschaltet",
                                ),
                                Badge::Camera => {
                                    (p.camera, tok.dim_text, icon::VIDEO_CAMERA, "Kamera an")
                                }
                                Badge::Admin => {
                                    (p.is_admin, tok.dim_text, icon::SHIELD_STAR, "Admin")
                                }
                            };
                            let glyph = RichText::new(glyph).size(15.0).color(color);
                            let r = slot(ui, on.then_some(glyph));
                            if on {
                                r.on_hover_text(hint);
                            }
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
        if !expanded && p.moderation.is_some() {
            send(Command::RequestUserList);
        }
    }
    if let Some(m) = p.moderation {
        if row.response.secondary_clicked() {
            send(Command::RequestUserList);
        }
        row.response
            .context_menu(|ui| moderation_menu(ui, p.user_id, m, send));
    }
    ui.data_mut(|d| d.insert_temp(rect_id, r));
}

/// The unfolded part of a participant row: what the client knows about this
/// person, and for everyone but oneself the playback volume.
fn details(ui: &mut egui::Ui, p: &Participant<'_>, tok: &theme::Tokens, send: &impl Fn(Command)) {
    let ms = |v: Option<u16>| opt(v.map(|v| format!("{v} ms")));
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
        None => rows.push(("Verlust", opt(p.loss_permille.map(|l| format!("{l} ‰"))))),
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
            widgets::rtl(ui, 24.0, |ui| {
                widgets::chip(ui, &format!("{v:.0} %"), 46.0);
                // One click to silence somebody for yourself, and back.
                let muted = v <= 0.0;
                let glyph = if muted {
                    icon::SPEAKER_X
                } else {
                    icon::SPEAKER_HIGH
                };
                if widgets::icon_button(ui, glyph, 24.0, muted)
                    .on_hover_text(if muted {
                        "Wieder hören"
                    } else {
                        "Für mich stummschalten"
                    })
                    .clicked()
                {
                    send(Command::SetVolume(
                        p.user_id,
                        if muted { 100.0 } else { 0.0 },
                    ));
                }
                if widgets::slider(ui, &mut v, 0.0..=200.0).changed() {
                    send(Command::SetVolume(p.user_id, v));
                }
            });
        });
    }
    if let Some(m) = p.moderation {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add_space(40.0);
            ui.label(RichText::new("Moderation").size(12.0).color(tok.dim_text));
            widgets::rtl(ui, 30.0, |ui| {
                let menu = widgets::more_button(ui).on_hover_text("Admin-Aktionen");
                egui::Popup::menu(&menu)
                    .align(egui::RectAlign::BOTTOM_END)
                    .gap(4.0)
                    .show(|ui| moderation_menu(ui, p.user_id, m, send));
            });
        });
    }
    ui.add_space(4.0);
}

/// An admin's actions on somebody else: enforce or lift mute and deafen,
/// kick, kick and ban. Every one of them is undone in one click (a kicked
/// person reconnects, a ban is lifted under Einstellungen → Nutzer), so
/// none asks for confirmation.
fn moderation_menu(
    ui: &mut egui::Ui,
    user_id: icu_proto::UserId,
    m: Moderation,
    send: &impl Fn(Command),
) {
    ui.set_min_width(220.0);
    let restrict = |muted: bool, deafened: bool| {
        send(Command::SetUserRestriction {
            user_id,
            muted,
            deafened,
        });
        send(Command::RequestUserList);
    };
    let (glyph, label) = if m.muted {
        (icon::MICROPHONE, "Stummschaltung aufheben")
    } else {
        (icon::MICROPHONE_SLASH, "Stummschalten")
    };
    if widgets::menu_item(ui, glyph, label, false)
        .on_hover_text("Gilt, bis ein Admin es aufhebt – auch nach Neuverbinden")
        .clicked()
    {
        restrict(!m.muted, m.deafened);
    }
    let (glyph, label) = if m.deafened {
        (icon::SPEAKER_HIGH, "Taubschaltung aufheben")
    } else {
        (icon::SPEAKER_SLASH, "Taubschalten")
    };
    if widgets::menu_item(ui, glyph, label, false)
        .on_hover_text("Hört niemanden, bis ein Admin es aufhebt")
        .clicked()
    {
        restrict(m.muted, !m.deafened);
    }
    ui.separator();
    if widgets::menu_item(ui, icon::SIGN_OUT, "Rauswerfen", true)
        .on_hover_text("Trennt die Verbindung; die Person kann sich neu verbinden")
        .clicked()
    {
        send(Command::KickUser { user_id });
    }
    if widgets::menu_item(ui, icon::PROHIBIT, "Rauswerfen & sperren", true)
        .on_hover_text("Trennt und sperrt den Zugang, bis jemand entsperrt")
        .clicked()
    {
        send(Command::SetUserDisabled {
            user_id,
            disabled: true,
        });
        send(Command::RequestUserList);
    }
}

/// The empty list: alone in the room, or not in it at all — never a blank
/// card that looks like something failed to draw.
fn empty(ui: &mut egui::Ui, conn: &ConnStatus) {
    let tok = theme::tokens(ui.visuals());
    let (glyph, text) = match conn {
        ConnStatus::Connected => (icon::USERS, "Allein hier"),
        ConnStatus::Connecting => (icon::PLUGS, "Verbinde…"),
        ConnStatus::Disconnected { .. } => (icon::PLUGS, "Getrennt — verbinde gleich neu"),
        ConnStatus::Fatal(_) => (icon::PLUGS, "Nicht verbunden"),
    };
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.label(RichText::new(glyph).size(30.0).color(tok.dim_text));
        ui.add_space(2.0);
        ui.label(RichText::new(text).size(12.5).color(tok.dim_text));
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
    text: Option<String>,
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
            let mut app = EnrollApp::new(paths, server_addr, result_app);
            if let Some(text) = text {
                app.paste(text);
            }
            Ok(Box::new(app))
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

/// The enrollment window has two steps: paste, then join. Everything the
/// user has to check (where the text leads) and decide (their name) sits on
/// the second page, next to the one button that joins.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EnrollStep {
    /// The paste field, nothing else.
    Paste,
    /// Where it leads, the own name (invites only), and the join button.
    Join,
}

/// The first-start window: paste an invite (or a passport), or drop the
/// file on it; check where it leads, pick a name, join. `pub` for
/// `tests/ui_layout.rs`.
pub struct EnrollApp {
    paths: Paths,
    server_addr: Option<SocketAddr>,
    link: String,
    /// The name the user picks. It is what the CSR asks for, so it ends up in
    /// the certificate (`PROTOCOL.md` §2.3). Starts empty and is required.
    name: String,
    status: Option<EnrollStatus>,
    step: EnrollStep,
    /// Put the cursor into the paste field: on the first frame, so Strg+V
    /// works without a click, and again after going back.
    focus_paste: bool,
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
    /// How much taller the last frame's content was than the window.
    overflow: f32,
}

impl EnrollApp {
    /// An empty window; a joined identity is saved to `paths` and put into
    /// `result` before the window closes itself.
    pub fn new(
        paths: Paths,
        server_addr: Option<SocketAddr>,
        result: Arc<Mutex<Option<Identity>>>,
    ) -> EnrollApp {
        EnrollApp {
            paths,
            server_addr,
            link: String::new(),
            name: String::new(),
            status: None,
            step: EnrollStep::Paste,
            focus_paste: true,
            focus_name: false,
            was_valid: false,
            pending: None,
            result,
            overflow: 0.0,
        }
    }

    /// How much taller the last frame's content was than the window
    /// (negative: shorter). The window grows by it itself; tests read it.
    pub fn overflow(&self) -> f32 {
        self.overflow
    }

    /// Fills in an invite or passport as if it had been pasted: the window
    /// opens on what it leads to.
    pub fn paste(&mut self, text: String) {
        self.link = text;
    }

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
            let r = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt
                    .block_on(register(&identity, &name, &client, addr, &paths))
                    .map_err(|e| {
                        tracing::warn!(error = %e, "registration failed");
                        e.problem().text
                    }),
                Err(e) => Err(format!("ICU konnte nicht starten: {e}")),
            };
            *slot.lock().unwrap() = Some(r);
        });
    }

    /// Takes a file dropped onto the window: an `.icu` invite or passport,
    /// read as text. Anything unreadable is left for the field's verdict.
    fn take_dropped(&mut self, ctx: &egui::Context) {
        // A passport decodes to at most 8 KiB; anything much larger is not
        // one and is not read into memory.
        const MAX_DROP: u64 = 64 * 1024;
        let dropped = ctx.input(|i| {
            i.raw.dropped_files.iter().find_map(|f| {
                let path = f.path();
                let small = std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_DROP);
                small.then(|| std::fs::read_to_string(path).ok()).flatten()
            })
        });
        if let Some(text) = dropped {
            self.link = text;
            self.status = None;
            self.step = EnrollStep::Paste;
            self.was_valid = false;
        }
    }
}

impl eframe::App for EnrollApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(100));

        // ICU started again (`crate::instance`): come to the front, with the
        // invite or passport it was given, unless a join is under way.
        if let Some(request) = crate::instance::poll() {
            if let Some(text) = request.bundle.filter(|_| self.pending.is_none()) {
                self.paste(text);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

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
        if !busy {
            self.take_dropped(&ctx);
        }
        let dragging = ctx.input(|i| !i.raw.hovered_files.is_empty());
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
        let needs_name = parsed.as_ref().is_some_and(Identity::is_registration);
        let valid = summary.is_some();
        // The join page only makes sense for text that parses; if it stops
        // parsing, start over at the field. A paste that parses moves on.
        if !valid {
            self.step = EnrollStep::Paste;
        } else if !self.was_valid && self.step == EnrollStep::Paste {
            self.step = EnrollStep::Join;
            self.focus_name = needs_name;
        }
        self.was_valid = valid;
        let name_ok = !icu_proto::normalize_name(&self.name).is_empty();
        let step = self.step;
        let join_ok = !busy && (!needs_name || name_ok);
        let mut join = false;
        let mut back = false;

        // The actions sit at the bottom edge, so the window has a floor
        // instead of trailing off into empty panel. The paste step has none:
        // a valid paste moves on by itself.
        if step == EnrollStep::Join {
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
                        let (kind, label) = if busy {
                            (widgets::ButtonKind::Neutral, "Verbinde…")
                        } else if join_ok {
                            (widgets::ButtonKind::Primary, "Beitreten")
                        } else {
                            (widgets::ButtonKind::Neutral, "Beitreten")
                        };
                        join |= ui
                            .allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), 36.0),
                                justified,
                                |ui| {
                                    ui.add_enabled_ui(join_ok, |ui| {
                                        widgets::button(ui, kind, icon::SIGN_IN, label)
                                    })
                                    .inner
                                },
                            )
                            .inner
                            .clicked();
                    });
                });
        }

        let subtitle = match step {
            EnrollStep::Paste => {
                "Füge die Einladung ein, die du bekommen hast, oder zieh die Datei hierher — \
                 auch ein Passport von einem anderen Gerät geht."
            }
            EnrollStep::Join if needs_name => {
                "Prüfe, wohin die Einladung führt, und wähle deinen Namen."
            }
            EnrollStep::Join => "Prüfe, wohin der Passport führt.",
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
                // content can outgrow the window; it grows the window rather
                // than scroll.
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
                    ui.add_space(16.0);

                    match step {
                        EnrollStep::Paste => self.paste_step(ui, &tok, busy, valid, needs_name),
                        EnrollStep::Join => {
                            if let Some(summary) = &summary {
                                let mut rows = vec![
                                    ("Server", summary.server_name.clone()),
                                    ("Adresse", summary.address.clone()),
                                ];
                                // An invite's placeholder name is not the
                                // user's; they pick their own below.
                                if !needs_name {
                                    rows.push(("Name", summary.name.clone()));
                                }
                                if let Some(ca) = &ca {
                                    // 32 pairs on two lines, so it reads in
                                    // halves when compared with the operator.
                                    rows.push(("CA", wrap_fingerprint(&ca.fingerprint)));
                                }
                                let title = if needs_name { "Einladung" } else { "Passport" };
                                enroll_card(ui, &tok, title, &rows, Some(JOIN_NOTE));
                            }
                            if needs_name {
                                ui.label(theme::caps_text("Dein Name"));
                                ui.add_space(4.0);
                                let field = ui.add_enabled(
                                    !busy,
                                    egui::TextEdit::singleline(&mut self.name)
                                        .desired_width(f32::INFINITY)
                                        .margin(egui::Margin::symmetric(10, 8))
                                        .hint_text("So sehen dich alle anderen"),
                                );
                                if std::mem::take(&mut self.focus_name) {
                                    field.request_focus();
                                }
                                if join_ok
                                    && field.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                {
                                    join = true;
                                }
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new("Steht später in deinem Zertifikat.")
                                        .size(11.5)
                                        .color(tok.dim_text),
                                );
                            } else if join_ok && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                join = true;
                            }
                            ui.add_space(10.0);
                            match &self.status {
                                Some(EnrollStatus::Busy(text)) => {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(
                                            RichText::new(text).size(12.0).color(tok.muted_text),
                                        );
                                    });
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
                let overflow = out.content_size.y - out.inner_rect.height();
                self.overflow = overflow;
                if step != EnrollStep::Paste {
                    super::grow_to_fit(&ctx, overflow);
                }
            });

        // Dragging a file over the window: say what letting go will do.
        if dragging && !busy {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("enroll-drop"),
            ));
            painter.rect_filled(screen, 0.0, tok.accent_tint_2.gamma_multiply(0.92));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                format!(
                    "{}  Datei loslassen, um sie zu öffnen",
                    icon::FILE_ARROW_DOWN
                ),
                egui::FontId::new(16.0, super::fonts::semibold()),
                tok.accent,
            );
        }

        if back && !busy {
            self.status = None;
            self.step = EnrollStep::Paste;
            self.focus_paste = true;
        } else if join && join_ok && step == EnrollStep::Join {
            self.submit();
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
                                    RichText::new(
                                        "Einladung hier einfügen (Strg+V) oder die .icu-Datei \
                                         hierher ziehen",
                                    )
                                    .size(13.0)
                                    .color(tok.dim_text),
                                ),
                        )
                    })
                    .inner
            });
        if std::mem::take(&mut self.focus_paste) {
            well.inner.request_focus();
        }
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
        Peer::new(
            UserInfo {
                user_id: u32::from(session),
                session,
                name: name.into(),
                flags: UserFlags(flags),
            },
            100.0,
        )
    }

    #[test]
    fn badge_columns_put_the_common_ones_right() {
        use super::{badge_columns, Badge};
        //       deafened muted  camera admin
        let rows = [
            [false, true, false, true],
            [false, true, true, false],
            [false, true, false, false],
        ];
        // Right to left: muted (3), then camera and admin tied (1) in
        // `BADGES` order; deafened nobody shows, so no column.
        assert_eq!(
            badge_columns(rows.into_iter()),
            [Badge::Muted, Badge::Camera, Badge::Admin]
        );
        assert!(badge_columns(std::iter::empty()).is_empty());
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
