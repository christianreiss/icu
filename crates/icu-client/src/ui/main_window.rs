//! The main window: who is here, whether they can hear you, and the four
//! switches that change that. Everything else lives in the settings window.

use std::{
    net::SocketAddr,
    sync::{atomic::Ordering, Arc, Mutex},
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

/// Header height, and with it the size of the logo badge and the gear.
const HEADER: f32 = 32.0;
/// The speak button and the mode button beside it.
const SPEAK_H: f32 = 46.0;
/// The four icon-over-label switches along the bottom.
const SWITCH_H: f32 = 54.0;

/// Runs the main window.
pub fn run(engine: Engine, debug_stats: bool) -> Result<(), String> {
    let start_minimized = engine.snapshot().settings.ui.start_minimized;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ICU")
            .with_app_id("icu")
            .with_inner_size([400.0, 620.0])
            .with_min_inner_size([360.0, 470.0])
            .with_resizable(true)
            .with_visible(!start_minimized),
        ..Default::default()
    };
    let engine = Arc::new(Mutex::new(Some(engine)));
    let engine_for_app = engine.clone();
    eframe::run_native(
        "ICU",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, engine_for_app, debug_stats)))),
    )
    .map_err(|e| e.to_string())?;
    if let Some(engine) = engine.lock().unwrap().take() {
        engine.shutdown();
    }
    Ok(())
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
    volume_open: Option<icu_proto::UserId>,
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
    #[cfg_attr(not(feature = "tray"), allow(dead_code))]
    hidden: bool,
    quit_requested: bool,
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        engine: Arc<Mutex<Option<Engine>>>,
        debug_stats: bool,
    ) -> App {
        let (state_rx, cmd, stats, theme_name) = {
            let guard = engine.lock().unwrap();
            let e = guard.as_ref().expect("engine");
            (
                e.state(),
                e.command_sender(),
                e.stats(),
                e.snapshot().settings.ui.theme.clone(),
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
            volume_open: None,
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
            hidden: false,
            quit_requested: false,
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
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(33));
        // A local sender, not `self.send`: a closure that calls a `&self`
        // method captures the *whole* `self` and would collide with the
        // `&mut self.settings` / `&mut self.volume_open` these panels need.
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
        egui::Panel::top("status").show(root, |ui| {
            let tok = theme::tokens(ui.visuals());
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
                    let (badge, _) =
                        ui.allocate_exact_size(egui::vec2(HEADER, HEADER), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(badge, theme::R_CTRL, tok.accent_tint_2);
                    ui.painter().text(
                        badge.center(),
                        egui::Align2::CENTER_CENTER,
                        icon::WAVEFORM,
                        egui::FontId::new(16.0, egui::FontFamily::Proportional),
                        tok.accent,
                    );

                    // The server, then one pill for the connection. The own
                    // name is not repeated here — it is the first row of the
                    // list right below.
                    ui.label(RichText::new(&state.server_name));
                    let (color, short, detail) = conn_status(state, &tok);
                    // Clickable in every state, not just Connected: a failed
                    // or retrying connection is exactly when the details are
                    // worth a window instead of a one-line tooltip.
                    let hint = if detail.is_empty() {
                        "Klicken für Verbindungsdetails".to_string()
                    } else {
                        format!("{detail}\n\nKlicken für Verbindungsdetails")
                    };
                    if widgets::status_pill_button(ui, color, &short)
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
                        VideoTier::T4 => Some("Video pausiert — das Netz trägt es gerade nicht."),
                        _ => Some("Video reduziert — weniger Bitrate für eine stabile Stimme."),
                    };
                    if let Some(hint) = hint {
                        widgets::status_pill(ui, tok.connecting, "Video").on_hover_text(hint);
                    }

                    widgets::rtl(ui, HEADER, |ui| {
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
                    });
                },
            );

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
            if let Some(banner) = &state.banner {
                widgets::banner(
                    ui,
                    widgets::BannerKind::Error,
                    "Fehler",
                    banner,
                    Some(|| send(Command::DismissBanner)),
                );
            }
        });

        egui::Panel::bottom("controls").show(root, |ui| {
            ui.add_space(8.0);
            widgets::level_meter(ui, state.level.rms_db, state.level.peak_db, state.sending);
            ui.add_space(6.0);
            input_readout(ui, state);

            ui.add_space(8.0);
            let ptt = speak_row(ui, state, &send);
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

            ui.add_space(8.0);
            ui.columns(4, |cols| {
                if switch(
                    &mut cols[0],
                    icon::MICROPHONE_SLASH,
                    "Mute",
                    state.muted,
                    false,
                    SWITCH_H,
                ) {
                    send(Command::SetMuted(!state.muted));
                }
                if switch(
                    &mut cols[1],
                    icon::SPEAKER_SLASH,
                    "Deafen",
                    state.deafened,
                    false,
                    SWITCH_H,
                ) {
                    send(Command::SetDeafened(!state.deafened));
                }
                #[cfg(feature = "camera")]
                if switch(
                    &mut cols[2],
                    icon::VIDEO_CAMERA,
                    "Video",
                    state.camera_on,
                    state.camera_on,
                    SWITCH_H,
                ) {
                    send(Command::SetCamera(!state.camera_on));
                }
                #[cfg(feature = "overlay")]
                if switch(
                    &mut cols[3],
                    icon::SQUARES_FOUR,
                    "Overlay",
                    state.overlay_on,
                    false,
                    SWITCH_H,
                ) {
                    send(Command::SetOverlay(!state.overlay_on));
                }
            });
            ui.add_space(8.0);
        });

        let volume_open = &mut self.volume_open;
        let videos = &self.videos;
        egui::CentralPanel::default().show(root, |ui| {
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
                                mode: Some(state.mode),
                                volume: None,
                                is_admin: state.is_admin,
                                rtt_ms: state.rtt_ms.map(|ms| ms.round() as u16),
                                video: self_video,
                            },
                            volume_open,
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
                                rtt_ms: p.rtt_ms,
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
                            volume_open,
                            &send,
                        );
                    }
                    if state.peers.is_empty() && state.conn == ConnStatus::Connected {
                        alone(ui);
                    }
                });
        });
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
                .with_inner_size([640.0, 580.0])
                .with_min_inner_size([520.0, 440.0])
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
        if !still_open {
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
            format!("{reason} — Versuch {attempt}"),
        ),
        ConnStatus::Fatal(reason) => (tok.error, "Fehler".to_string(), reason.clone()),
    }
}

// ---------------------------------------------------------------------------
// Bottom bar
// ---------------------------------------------------------------------------

/// The line under the level meter: the microphone level on the left, and on
/// the right whatever the current mode makes the meter mean — the key to
/// hold, the voice-activation probability, or that audio is going out.
fn input_readout(ui: &mut egui::Ui, state: &AppState) {
    let tok = theme::tokens(ui.visuals());
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{:.0} dBFS", state.level.rms_db))
                .monospace()
                .size(10.5)
                .color(tok.dim_text),
        );
        widgets::rtl(ui, 16.0, |ui| {
            if state.sending {
                ui.label(
                    RichText::new("SENDET")
                        .size(10.5)
                        .color(tok.accent)
                        .font(egui::FontId::new(10.5, fonts::semibold())),
                );
                widgets::speaking_bars(ui);
            } else if state.mode == Mode::Vad {
                widgets::vad_bar(ui, state.vad_prob, state.vad_threshold).on_hover_text(format!(
                    "Stimme {:.2}, Schwelle {:.2}{}",
                    state.vad_prob,
                    state.vad_threshold,
                    if state.settings.audio.vad_auto {
                        " (automatisch)"
                    } else {
                        ""
                    }
                ));
            } else {
                widgets::chip(ui, &hotkey_label(state), 0.0)
                    .on_hover_text("Diese Taste halten, um zu sprechen");
            }
        });
    });
}

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

/// The speak button plus the PTT/VAD switch beside it. Returns the speak
/// button's response only in push-to-talk mode — in voice activation there
/// is nothing to press, so it is drawn as a status surface instead of a
/// button that would do nothing.
fn speak_row(
    ui: &mut egui::Ui,
    state: &AppState,
    send: &impl Fn(Command),
) -> Option<egui::Response> {
    let tok = theme::tokens(ui.visuals());
    let open = match state.mode {
        Mode::Ptt => state.ptt_down,
        Mode::Vad => state.sending,
    };
    let (label, fill, text_color) = if open {
        ("Mikrofon offen", tok.accent, tok.on_accent)
    } else if state.mode == Mode::Ptt {
        (
            "Sprechen",
            ui.visuals().faint_bg_color,
            ui.visuals().text_color(),
        )
    } else {
        (
            "Wartet auf Stimme",
            ui.visuals().faint_bg_color,
            tok.muted_text,
        )
    };
    let caption = format!("{}  {label}", icon::MICROPHONE);

    // Right-to-left: the mode switch claims its natural width first
    // (rightmost), then the speak surface fills whatever remains to its
    // left. The height is allocated explicitly — see `widgets::rtl`.
    widgets::rtl(ui, SPEAK_H, |ui| {
        let (glyph, mode_label, next) = match state.mode {
            Mode::Ptt => (icon::HAND_TAP, "PTT", "Sprachaktivierung"),
            Mode::Vad => (icon::WAVEFORM, "VAD", "Push-to-Talk"),
        };
        let mode = ui.allocate_ui(egui::vec2(62.0, SPEAK_H), |ui| {
            switch(ui, glyph, mode_label, false, false, SPEAK_H)
        });
        if mode.inner {
            send(Command::ToggleMode);
        }
        mode.response
            .on_hover_text(format!("Umschalten auf {next}"));

        let size = egui::vec2(ui.available_width(), SPEAK_H);
        match state.mode {
            Mode::Ptt => Some(
                ui.add(
                    egui::Button::new(RichText::new(caption).size(14.5).color(text_color))
                        .fill(fill)
                        .stroke(egui::Stroke::new(
                            1.0,
                            if open {
                                tok.accent
                            } else {
                                ui.visuals().widgets.noninteractive.bg_stroke.color
                            },
                        ))
                        .corner_radius(egui::CornerRadius::same(theme::R_ROW))
                        .min_size(size),
                ),
            ),
            Mode::Vad => {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, theme::R_ROW, fill);
                painter.rect_stroke(
                    rect,
                    theme::R_ROW,
                    egui::Stroke::new(
                        1.0,
                        if open {
                            tok.accent
                        } else {
                            ui.visuals().widgets.noninteractive.bg_stroke.color
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    caption,
                    egui::FontId::new(14.5, fonts::semibold()),
                    text_color,
                );
                None
            }
        }
    })
}

/// One icon-over-label switch: the four along the bottom bar and the
/// PTT/VAD mode button. `active` tints it by role — error for the two that
/// take something away (mute, deafen), accent for the two that add
/// something (camera, overlay). `rec_dot` draws the pulsing "on air" dot
/// that says this client's own video is going out. The window already
/// repaints continuously (`request_repaint_after` in `App::ui`), so the
/// pulse animates for free.
fn switch(
    ui: &mut egui::Ui,
    glyph: &str,
    label: &str,
    active: bool,
    rec_dot: bool,
    height: f32,
) -> bool {
    let tok = theme::tokens(ui.visuals());
    let is_error_role = glyph == icon::MICROPHONE_SLASH || glyph == icon::SPEAKER_SLASH;
    let (accent, tint) = if is_error_role {
        (tok.error, tok.error_tint)
    } else {
        (tok.accent, tok.accent_tint)
    };
    let (fill, border, text_color) = if active {
        (tint, accent.gamma_multiply(0.47), accent)
    } else {
        (
            ui.visuals().faint_bg_color,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
            tok.muted_text,
        )
    };
    let resp = ui.add(
        egui::Button::new("")
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, border))
            .corner_radius(egui::CornerRadius::same(theme::R_ROW))
            .min_size(egui::vec2(ui.available_width(), height)),
    );
    let rect = resp.rect;
    let painter = ui.painter();
    let text_color = if resp.hovered() && !active {
        ui.visuals().text_color()
    } else {
        text_color
    };
    painter.text(
        egui::pos2(rect.center().x, rect.top() + 18.0),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::new(19.0, egui::FontFamily::Proportional),
        text_color,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 12.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::new(10.5, fonts::semibold()),
        text_color,
    );
    if rec_dot {
        let t = ui.input(|i| i.time);
        let pulse = ((t * 2.4).sin() as f32 * 0.5 + 0.5).clamp(0.0, 1.0);
        let pos = egui::pos2(rect.right() - 12.0, rect.top() + 12.0);
        painter.circle_filled(
            pos,
            5.0 + pulse * 2.0,
            tok.error.gamma_multiply(0.18 + pulse * 0.12),
        );
        painter.circle_filled(pos, 3.5, tok.error);
    }
    resp.clicked()
}

// ---------------------------------------------------------------------------
// Participants
// ---------------------------------------------------------------------------

/// One row's worth of participant, local or remote. `mode` is `Some` only
/// for the local user (it is a client-local setting); `volume` is `Some`
/// only for everyone else (there is no local playback volume for oneself).
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
    /// Server-measured RTT, milliseconds. `None` until the first
    /// `UserRtt` arrives (`PROTOCOL.md` §2.2).
    rtt_ms: Option<u16>,
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

fn participant_row(
    ui: &mut egui::Ui,
    p: Participant<'_>,
    volume_open: &mut Option<icu_proto::UserId>,
    send: &impl Fn(Command),
) {
    let tok = theme::tokens(ui.visuals());
    let expanded = p.volume.is_some() && *volume_open == Some(p.user_id);
    let fill = if p.speaking {
        tok.accent_tint
    } else if expanded {
        ui.visuals().faint_bg_color
    } else {
        Color32::TRANSPARENT
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(theme::R_ROW))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::avatar(ui, p.name, p.user_id, p.speaking, 34.0, p.video);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    // Tight enough that a two-line row is still shorter than
                    // the avatar beside it, which is what keeps every row the
                    // same height whether or not there is a second line.
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        ui.label(if p.mode.is_some() {
                            theme::strong_text(p.name)
                        } else {
                            RichText::new(p.name).size(14.0)
                        });
                        if p.speaking {
                            widgets::speaking_bars(ui);
                        }
                    });
                    ui.label(
                        RichText::new(p.subtitle().unwrap_or(" "))
                            .size(11.5)
                            .color(tok.dim_text),
                    );
                });
                widgets::rtl(ui, 34.0, |ui| {
                    // Always the same glyph: the crossed-out speaker is the
                    // badge for "this person hears nothing", and a volume
                    // handle wearing it would read as that.
                    if let Some(vol) = p.volume {
                        if widgets::icon_button(ui, icon::SPEAKER_HIGH, 30.0, expanded)
                            .on_hover_text(format!("Lautstärke {vol:.0} %"))
                            .clicked()
                        {
                            *volume_open = if expanded { None } else { Some(p.user_id) };
                        }
                    }
                    for (on, color, glyph) in [
                        (p.deafened, tok.error, icon::SPEAKER_SLASH),
                        (p.muted, tok.error, icon::MICROPHONE_SLASH),
                        (p.camera, tok.muted_text, icon::VIDEO_CAMERA),
                    ] {
                        if on {
                            ui.label(RichText::new(glyph).size(15.0).color(color));
                        }
                    }
                    if p.is_admin {
                        ui.label(
                            RichText::new(icon::SHIELD_STAR)
                                .size(15.0)
                                .color(tok.accent),
                        )
                        .on_hover_text("Admin");
                    }
                    // Server-measured RTT (`PROTOCOL.md` §2.2 `UserRtt`), not
                    // a client self-report: every viewer sees the same
                    // number for a given peer.
                    if let Some(ms) = p.rtt_ms {
                        widgets::chip(ui, &format!("{ms} ms"), 0.0);
                    }
                });
            });
            if expanded {
                let mut v = p.volume.expect("expanded implies a remote participant");
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(38.0);
                    widgets::rtl(ui, 22.0, |ui| {
                        widgets::chip(ui, &format!("{v:.0} %"), 46.0);
                        if widgets::slider(ui, &mut v, 0.0..=200.0).changed() {
                            send(Command::SetVolume(p.user_id, v));
                        }
                    });
                });
            }
        });
}

/// The empty state: connected, but the only person here is you.
fn alone(ui: &mut egui::Ui) {
    let tok = theme::tokens(ui.visuals());
    ui.vertical_centered(|ui| {
        ui.add_space(28.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 28.0, ui.visuals().faint_bg_color);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon::USERS,
            egui::FontId::new(24.0, egui::FontFamily::Proportional),
            tok.dim_text,
        );
        ui.add_space(10.0);
        ui.label(theme::strong_text("Niemand sonst ist da"));
        ui.add_space(2.0);
        ui.label(
            RichText::new("Teile den Einladungslink, um jemanden hierher zu holen.")
                .size(12.0)
                .color(tok.dim_text),
        );
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ICU — Einladung")
            .with_app_id("icu")
            .with_inner_size([440.0, 380.0])
            .with_min_inner_size([400.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ICU",
        options,
        Box::new(move |cc| {
            fonts::install(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx, "dark");
            Ok(Box::new(EnrollApp {
                paths,
                server_addr,
                link: String::new(),
                name: String::new(),
                prefilled_from: None,
                status: None,
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

struct EnrollApp {
    paths: Paths,
    server_addr: Option<SocketAddr>,
    link: String,
    /// The name the user picks. It is what the CSR asks for, so it ends up in
    /// the certificate; the bundle's placeholder only prefills it
    /// (`PROTOCOL.md` §2.3).
    name: String,
    /// The placeholder already taken from the pasted bundle, so a newly
    /// pasted one can refill the field without overwriting what the user
    /// typed.
    prefilled_from: Option<String>,
    status: Option<EnrollStatus>,
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
        let name = self.name.trim().to_string();
        let client = format!("icu/{} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS);
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            let r = rt
                .block_on(register(&identity, &name, &client, addr, &paths))
                .map_err(|e| e.to_string());
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
                        self.status = Some(EnrollStatus::Failed(format!(
                            "Speichern fehlgeschlagen: {e}"
                        )))
                    }
                },
                Err(e) => self.status = Some(EnrollStatus::Failed(e)),
            }
        }
        let busy = self.pending.is_some();

        egui::CentralPanel::default().show(root, |ui| {
            // An error banner wraps to however many lines it needs, so the
            // content can outgrow the window; it scrolls rather than clips.
            egui::ScrollArea::vertical().show(ui, |ui| {
                let tok = theme::tokens(ui.visuals());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let (badge, _) =
                        ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(badge, theme::R_CTRL, tok.accent_tint_2);
                    ui.painter().text(
                        badge.center(),
                        egui::Align2::CENTER_CENTER,
                        icon::WAVEFORM,
                        egui::FontId::new(18.0, egui::FontFamily::Proportional),
                        tok.accent,
                    );
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.label(theme::strong_text("Mit ICU verbinden"));
                        ui.label(
                            RichText::new(
                                "Füge die Einladung ein, die du bekommen hast — oder deinen \
                                 Passport von einem anderen Gerät.",
                            )
                                .size(12.0)
                                .color(tok.muted_text),
                        );
                    });
                });
                ui.add_space(14.0);

                // A passport carries its own certificate; a registration
                // bundle needs a name, prefilled from its placeholder.
                let registration = Identity::from_passport(&self.link)
                    .ok()
                    .filter(Identity::is_registration);
                let prefill = registration.as_ref().map(|id| {
                    crate::conninfo::CertInfo::from_der(&id.leaf_der).subject
                });
                if prefill != self.prefilled_from {
                    if let Some(n) = &prefill {
                        if self.name.trim().is_empty()
                            || Some(self.name.as_str()) == self.prefilled_from.as_deref()
                        {
                            self.name = n.clone();
                        }
                    }
                    self.prefilled_from = prefill;
                }
                let needs_name = registration.is_some();
                let mut submit = false;
                widgets::group_card(ui, "Einladung", |ui| {
                    widgets::card_note(ui, |ui| {
                        ui.add_enabled(
                            !busy,
                            egui::TextEdit::multiline(&mut self.link)
                                .desired_rows(2)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace)
                                .hint_text(icu_proto::passport::ARMOR_BEGIN),
                        );
                    });
                    if needs_name {
                        widgets::setting_row(
                            ui,
                            true,
                            "Dein Name",
                            Some("Steht später in deinem Zertifikat."),
                            |ui| {
                                let field = ui.add_enabled(
                                    !busy,
                                    egui::TextEdit::singleline(&mut self.name)
                                        .desired_width(150.0)
                                        .hint_text("Name"),
                                );
                                if field.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                {
                                    submit = true;
                                }
                            },
                        );
                    }
                });

                ui.horizontal(|ui| {
                    let can_submit = !busy
                        && !self.link.trim().is_empty()
                        && (!needs_name || !self.name.trim().is_empty());
                    if ui
                        .add_enabled_ui(can_submit, |ui| {
                            widgets::button(
                                ui,
                                widgets::ButtonKind::Primary,
                                icon::SIGN_IN,
                                "Beitreten",
                            )
                        })
                        .inner
                        .clicked()
                    {
                        submit = true;
                    }
                    if busy {
                        widgets::status_pill(ui, tok.connecting, "verbinde");
                    }
                });
                if submit && !busy && !self.link.trim().is_empty() {
                    self.submit();
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
            });
        });
    }
}
