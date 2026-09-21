//! The main window: status, participants, level, buttons.

use std::{
    net::SocketAddr,
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};

use egui::{Color32, RichText};
use egui_phosphor::regular as icon;

use super::{fonts, settings::SettingsView, theme, widgets};
use crate::{
    config::{Identity, Paths},
    dsp::gate::Mode,
    engine::{AppState, Command, ConnStatus, Engine},
    net::enroll::{enroll, InviteLink},
    video::VideoTier,
};

/// Runs the main window.
pub fn run(engine: Engine, debug_stats: bool) -> Result<(), String> {
    let start_minimized = engine.snapshot().settings.ui.start_minimized;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ICU")
            .with_app_id("icu")
            .with_inner_size([420.0, 680.0])
            .with_min_inner_size([360.0, 480.0])
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
    cmd: tokio::sync::mpsc::UnboundedSender<Command>,
    settings: Option<SettingsView>,
    debug_stats: bool,
    /// Whether the separate debug window is open. Starts `true` when
    /// `--debug-stats` was passed, otherwise toggled from the header's bug
    /// icon; the stats refresh runs whenever either is true, not just the
    /// CLI flag.
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
        let camera = {
            let guard = engine.lock().unwrap();
            let e = guard.as_ref().expect("engine");
            let snapshot = e.snapshot();
            let handle = crate::camera::CameraHandle::new(e.camera_link());
            if snapshot.settings.camera.enabled {
                let _ = cmd.send(Command::SetCamera(true));
            }
            Some(handle)
        };
        App {
            engine,
            state_rx,
            cmd,
            settings: None,
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
            hidden: false,
            quit_requested: false,
        }
    }

    fn send(&self, cmd: Command) {
        let _ = self.cmd.send(cmd);
    }
}

impl eframe::App for App {
    /// Runs before every `ui` pass and also while the window is hidden (on
    /// `request_repaint`): tray events, camera state, close handling.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg_attr(
            not(any(feature = "tray", feature = "camera")),
            allow(unused_variables)
        )]
        let state = self.state_rx.borrow().clone();

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
            tray.update(&state);
        }
        #[cfg(feature = "camera")]
        if let Some(camera) = &mut self.camera {
            camera.sync(&state);
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
        let ctx = &root.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(33));
        let state = self.state_rx.borrow_and_update().clone();

        if (self.debug_stats || self.debug_window_open)
            && self.last_stats.elapsed() > Duration::from_secs(2)
        {
            self.last_stats = Instant::now();
            self.stats_text = self.stats.report();
            if self.debug_stats {
                eprintln!("{}", self.stats_text);
            }
        }

        if self.debug_window_open {
            let mut still_open = true;
            let stats_text = &self.stats_text;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("icu-debug"),
                egui::ViewportBuilder::default()
                    .with_title("ICU — Debug")
                    .with_inner_size([520.0, 420.0]),
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

        if self.settings.is_some() {
            let mut still_open = true;
            let mut cmds = Vec::new();
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("icu-settings"),
                egui::ViewportBuilder::default()
                    .with_title("ICU — Einstellungen")
                    .with_inner_size([620.0, 560.0])
                    .with_min_inner_size([480.0, 420.0])
                    .with_resizable(true),
                |vctx, _class| {
                    egui::CentralPanel::default().show(vctx, |ui| {
                        let view = self.settings.as_mut().expect("checked above");
                        cmds = view.ui(ui, &state);
                    });
                    if vctx.input(|i| i.viewport().close_requested()) {
                        still_open = false;
                    }
                },
            );
            for c in cmds {
                if let Command::UpdateSettings(s) = &c {
                    theme::apply(ctx, &s.ui.theme);
                }
                self.send(c);
            }
            if !still_open {
                self.settings = None;
            }
        }

        egui::Panel::top("status").show(root, |ui| {
            let tok = theme::tokens(ui.visuals());
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (badge, _) =
                    ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                ui.painter().rect_filled(badge, 8, tok.accent_tint_2);
                ui.painter().rect_stroke(
                    badge,
                    8,
                    egui::Stroke::new(1.0, tok.accent.gamma_multiply(0.33)),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    badge.center(),
                    egui::Align2::CENTER_CENTER,
                    icon::WAVEFORM,
                    egui::FontId::new(17.0, egui::FontFamily::Proportional),
                    tok.accent,
                );

                ui.vertical(|ui| {
                    ui.label(theme::strong_text(&state.server_name).size(14.5));
                    if let Some(me) = &state.me {
                        ui.label(RichText::new(&me.name).small().color(tok.muted_text));
                    }
                    let (color, text) = match &state.conn {
                        ConnStatus::Connected => (tok.connected, "verbunden".to_string()),
                        ConnStatus::Connecting => (tok.connecting, "verbinde…".to_string()),
                        ConnStatus::Disconnected { reason, attempt } => (
                            tok.connecting,
                            format!("getrennt ({reason}), Versuch {attempt}"),
                        ),
                        ConnStatus::Fatal(reason) => (tok.error, format!("Fehler: {reason}")),
                    };
                    let trailing = state.rtt_ms.map(|rtt| format!("{rtt:.0} ms"));
                    widgets::status_pill(ui, color, &text, trailing.as_deref()).on_hover_text(
                        match &state.me {
                            Some(me) => format!("user_id {}", me.user_id),
                            None => String::new(),
                        },
                    );
                });

                // `allocate_ui_with_layout` with an explicit height, not
                // `with_layout`: the latter claims the parent's *entire*
                // remaining height, and `Align::Center` then centers the
                // content inside that tall rect, which balloons the row's
                // `min_rect` and pushes everything after it off-screen.
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 36.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let gear = egui::Button::new(RichText::new(icon::GEAR_FINE).size(16.0))
                            .fill(if self.settings.is_some() {
                                ui.visuals().faint_bg_color
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(egui::CornerRadius::same(9))
                            .min_size(egui::vec2(36.0, 36.0));
                        if ui.add(gear).on_hover_text("Einstellungen").clicked() {
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
                                    Some(SettingsView::new(
                                        &state,
                                        state.hotkey_backend.clone(),
                                        self.stats.clone(),
                                        paths,
                                    ))
                                }
                            };
                        }

                        let bug = egui::Button::new(RichText::new(icon::BUG).size(16.0))
                            .fill(if self.debug_window_open {
                                ui.visuals().faint_bg_color
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(egui::CornerRadius::same(9))
                            .min_size(egui::vec2(36.0, 36.0));
                        if ui.add(bug).on_hover_text("Debug-Fenster").clicked() {
                            self.debug_window_open = !self.debug_window_open;
                            if self.debug_window_open {
                                self.stats_text = self.stats.report();
                                self.last_stats = Instant::now();
                            }
                        }

                        // Own outgoing video quality tier, set by
                        // `crate::quality::QualityController`. Automatic, no
                        // settings toggle (`Research/feature-freeze.md`'s bias
                        // against exposing adaptive behavior as a dial) — but
                        // shown, since a tier-down is visible to other people as
                        // this client's video degrading.
                        let tier =
                            VideoTier::from_u8(self.stats.video_tier.load(Ordering::Relaxed) as u8);
                        let label = match tier {
                            VideoTier::T0 => None,
                            VideoTier::T4 => Some("Video pausiert (Netz)"),
                            _ => Some("Video reduziert"),
                        };
                        if let Some(label) = label {
                            ui.label(RichText::new(label).small().color(tok.connecting));
                        }
                    },
                );
            });

            if let Some(min) = &state.update_required {
                let no_action: Option<(&str, fn())> = None;
                widgets::banner(
                    ui,
                    widgets::BannerKind::Warning,
                    "Update nötig",
                    &format!(
                        "Der Server verlangt mindestens Version {min} (diese: {}).",
                        env!("CARGO_PKG_VERSION")
                    ),
                    no_action,
                    None::<fn()>,
                );
            }
            if let Some(banner) = &state.banner {
                let no_action: Option<(&str, fn())> = None;
                widgets::banner(
                    ui,
                    widgets::BannerKind::Error,
                    "Fehler",
                    banner,
                    no_action,
                    Some(|| self.send(Command::DismissBanner)),
                );
            }
        });

        egui::Panel::bottom("controls").show(root, |ui| {
            let tok = theme::tokens(ui.visuals());
            ui.add_space(4.0);
            widgets::level_meter(
                ui,
                state.level.rms_db,
                (state.mode == Mode::Vad).then_some(state.vad_prob),
                state.sending,
            );
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{:.0} dBFS", state.level.rms_db))
                        .monospace()
                        .small()
                        .color(tok.dim_text),
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 16.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if state.sending {
                            ui.label(
                                RichText::new("SENDET")
                                    .size(11.0)
                                    .color(tok.accent)
                                    .font(egui::FontId::new(11.0, fonts::semibold())),
                            );
                            widgets::speaking_bars(ui, true);
                        } else if state.mode == Mode::Vad {
                            ui.label(
                                RichText::new(format!("VAD {:.2}", state.vad_prob))
                                    .monospace()
                                    .small()
                                    .color(tok.dim_text),
                            );
                        }
                    },
                );
            });

            ui.add_space(6.0);
            let ptt_hint = format!("{} halten", state.settings.hotkeys.ptt.key);
            let (label, fill, text_color) = match (state.mode, state.ptt_down) {
                (Mode::Ptt, true) => (
                    format!("{}  Mikrofon offen", icon::MICROPHONE),
                    tok.accent,
                    tok.on_accent,
                ),
                (Mode::Ptt, false) => (
                    format!("{}  Sprechen", icon::MICROPHONE),
                    ui.visuals().faint_bg_color,
                    ui.visuals().text_color(),
                ),
                (Mode::Vad, _) => (
                    format!("{}  Wartet auf Stimme", icon::MICROPHONE),
                    ui.visuals().faint_bg_color,
                    tok.muted_text,
                ),
            };
            // Right-to-left: the small PTT/VAD toggle claims its natural
            // width first (rightmost), then the speak button fills whatever
            // remains to its left — avoids guessing the toggle's width. The
            // height is allocated explicitly (see the header comment above:
            // plain `with_layout` would claim the panel's whole remaining
            // height and push the buttons below it out of the window).
            let ptt = ui
                .allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 46.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        // Push-to-toggle: one small button showing only the
                        // current mode; each tap flips PTT<->VAD. Replaces
                        // the old two-label segmented control to save width.
                        let mode_label = match state.mode {
                            Mode::Ptt => "PTT",
                            Mode::Vad => "VAD",
                        };
                        let mode_btn = egui::Button::new(mode_label)
                            .fill(ui.visuals().faint_bg_color)
                            .corner_radius(egui::CornerRadius::same(11))
                            .min_size(egui::vec2(0.0, 46.0));
                        if ui.add(mode_btn).clicked() {
                            self.send(Command::ToggleMode);
                        }
                        let btn = egui::Button::new(RichText::new(label).color(text_color))
                            .fill(fill)
                            .corner_radius(egui::CornerRadius::same(11))
                            .min_size(egui::vec2(ui.available_width(), 46.0));
                        ui.add(btn)
                    },
                )
                .inner;
            if state.mode == Mode::Ptt {
                ptt.clone().on_hover_ui(|ui| {
                    widgets::kbd(ui, &ptt_hint);
                });
            }
            // Hold semantics: a press that starts on the button keeps the gate
            // open until the primary button is released anywhere, even if the
            // pointer drifts off the widget or egui stops treating it as
            // "down on" (observed in the live test: transmission ended after a
            // few seconds while the button was still held).
            let primary_down = ui.input(|i| i.pointer.primary_down());
            let held = if self.ptt_held {
                primary_down
            } else {
                ptt.is_pointer_button_down_on() && primary_down
            };
            if held {
                ui.ctx().request_repaint();
            }
            if held != self.ptt_held {
                self.ptt_held = held;
                if state.mode == Mode::Ptt {
                    self.send(Command::Ptt(held));
                }
            }

            ui.add_space(6.0);
            ui.columns(4, |cols| {
                if control_button(&mut cols[0], icon::MICROPHONE_SLASH, "Mute", state.muted) {
                    self.send(Command::SetMuted(!state.muted));
                }
                if control_button(&mut cols[1], icon::SPEAKER_SLASH, "Deafen", state.deafened) {
                    self.send(Command::SetDeafened(!state.deafened));
                }
                #[cfg(feature = "camera")]
                if control_button(&mut cols[2], icon::VIDEO_CAMERA, "Video", state.camera_on) {
                    self.send(Command::SetCamera(!state.camera_on));
                }
                #[cfg(feature = "overlay")]
                if control_button(
                    &mut cols[3],
                    icon::SQUARES_FOUR,
                    "Overlay",
                    state.overlay_on,
                ) {
                    self.send(Command::SetOverlay(!state.overlay_on));
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(me) = &state.me {
                        participant_row(
                            ui,
                            &me.name,
                            me.user_id,
                            state.sending,
                            state.muted,
                            state.deafened,
                            state.camera_on,
                            participant_subtitle(
                                state.sending,
                                state.muted,
                                state.camera_on,
                                Some(state.mode),
                            ),
                            None,
                            &mut self.volume_open,
                            &self.cmd,
                            true,
                        );
                    }
                    let mut peers = state.peers.clone();
                    peers.sort_by_key(|a| a.info.name.to_lowercase());
                    for p in &peers {
                        participant_row(
                            ui,
                            &p.info.name,
                            p.info.user_id,
                            p.speaking,
                            p.info.flags.muted(),
                            p.info.flags.deafened(),
                            p.info.flags.camera_on(),
                            participant_subtitle(
                                p.speaking,
                                p.info.flags.muted(),
                                p.info.flags.camera_on(),
                                None,
                            ),
                            Some((p.info.user_id, p.volume)),
                            &mut self.volume_open,
                            &self.cmd,
                            false,
                        );
                    }
                    if state.peers.is_empty() && state.conn == ConnStatus::Connected {
                        let tok = theme::tokens(ui.visuals());
                        ui.vertical_centered(|ui| {
                            ui.add_space(24.0);
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(52.0, 52.0), egui::Sense::hover());
                            ui.painter().circle_filled(
                                rect.center(),
                                26.0,
                                ui.visuals().faint_bg_color,
                            );
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                icon::USERS,
                                egui::FontId::new(22.0, egui::FontFamily::Proportional),
                                tok.dim_text,
                            );
                            ui.add_space(8.0);
                            ui.label(RichText::new("Niemand sonst ist da").strong());
                            ui.label(
                                RichText::new(
                                    "Teile den Einladungslink, um jemanden hierher zu holen.",
                                )
                                .small()
                                .color(tok.dim_text),
                            );
                        });
                    }
                    #[cfg(feature = "camera")]
                    if let Some(camera) = self.camera.as_mut() {
                        if state.camera_on && state.settings.camera.show_self_preview {
                            ui.add_space(4.0);
                            let tok = theme::tokens(ui.visuals());
                            egui::Frame::new()
                                .fill(ui.visuals().faint_bg_color)
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    ui.visuals().widgets.inactive.bg_stroke.color,
                                ))
                                .corner_radius(egui::CornerRadius::same(12))
                                .inner_margin(6)
                                .show(ui, |ui| {
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(ui.available_width(), 22.0),
                                        egui::Layout::right_to_left(egui::Align::TOP),
                                        |ui| {
                                            widgets::status_pill(ui, tok.error, "LIVE", None);
                                        },
                                    );
                                    camera.preview(ui);
                                });
                        }
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

/// Second line under a participant's name: what they're currently doing,
/// derived only from state already available (never invented). `mode` is
/// `Some` only for the local user, since it's a client-local setting.
fn participant_subtitle(
    speaking: bool,
    muted: bool,
    camera: bool,
    mode: Option<Mode>,
) -> Option<String> {
    if speaking {
        Some(match mode {
            Some(Mode::Ptt) => "sendet · Push-to-Talk".to_string(),
            Some(Mode::Vad) => "sendet · Sprachaktivierung".to_string(),
            None => "spricht".to_string(),
        })
    } else if camera {
        Some("Kamera an".to_string())
    } else if !muted {
        Some("Mikrofon frei".to_string())
    } else {
        None
    }
}

/// One of the four bottom-bar control buttons (Mute/Deafen/Kamera/Overlay):
/// icon over label, active state tinted by role (error for mute/deafen,
/// accent for camera/overlay — callers pass the right glyph and label, the
/// active color follows from the glyph's role below).
fn control_button(ui: &mut egui::Ui, glyph: &str, label: &str, active: bool) -> bool {
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
    let btn = egui::Button::new("")
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(11))
        .min_size(egui::vec2(ui.available_width(), 54.0));
    let resp = ui.add(btn);
    let rect = resp.rect;
    let painter = ui.painter();
    painter.text(
        egui::pos2(rect.center().x, rect.top() + 18.0),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::new(19.0, egui::FontFamily::Proportional),
        text_color,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 11.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::new(10.5, fonts::semibold()),
        text_color,
    );
    resp.clicked()
}

#[allow(clippy::too_many_arguments)]
fn participant_row(
    ui: &mut egui::Ui,
    name: &str,
    user_id: icu_proto::UserId,
    speaking: bool,
    muted: bool,
    deafened: bool,
    camera: bool,
    subtitle: Option<String>,
    volume: Option<(icu_proto::UserId, f32)>,
    volume_open: &mut Option<icu_proto::UserId>,
    cmd: &tokio::sync::mpsc::UnboundedSender<Command>,
    is_me: bool,
) {
    let tok = theme::tokens(ui.visuals());
    egui::Frame::new()
        .fill(if speaking {
            tok.accent_tint
        } else {
            Color32::TRANSPARENT
        })
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::avatar(ui, name, user_id, speaking, 34.0);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let text = if is_me {
                            theme::strong_text(name)
                        } else {
                            RichText::new(name).size(13.5)
                        };
                        ui.label(text);
                        if speaking {
                            widgets::speaking_bars(ui, true);
                        }
                    });
                    if let Some(subtitle) = &subtitle {
                        ui.label(RichText::new(subtitle).size(11.5).color(tok.dim_text));
                    }
                });
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 34.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if let Some((user_id, vol)) = volume {
                            let chip = egui::Frame::new()
                                .fill(ui.visuals().extreme_bg_color)
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    ui.visuals().widgets.inactive.bg_stroke.color,
                                ))
                                .corner_radius(egui::CornerRadius::same(7))
                                .inner_margin(egui::Margin::symmetric(7, 3))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!("{vol:.0} %")).monospace().size(11.0),
                                    )
                                });
                            if chip.response.interact(egui::Sense::click()).clicked() {
                                *volume_open = if *volume_open == Some(user_id) {
                                    None
                                } else {
                                    Some(user_id)
                                };
                            }
                        }
                        if deafened {
                            ui.colored_label(tok.error, icon::SPEAKER_SLASH);
                        }
                        if muted {
                            ui.colored_label(tok.error, icon::MICROPHONE_SLASH);
                        }
                        if camera {
                            ui.colored_label(tok.muted_text, icon::VIDEO_CAMERA);
                        }
                    },
                );
            });
        });
    if let Some((user_id, vol)) = volume {
        if *volume_open == Some(user_id) {
            let mut v = vol;
            ui.indent("vol", |ui| {
                if ui
                    .add(egui::Slider::new(&mut v, 0.0..=200.0).suffix(" %"))
                    .changed()
                {
                    let _ = cmd.send(Command::SetVolume(user_id, v));
                }
            });
        }
    }
    ui.add_space(4.0);
}

// ---------------------------------------------------------------------------
// Enrollment window
// ---------------------------------------------------------------------------

/// Shows a window that takes an invite link.
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
            .with_inner_size([460.0, 240.0]),
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
                status: String::new(),
                busy: Arc::new(Mutex::new(None)),
                result: result_app,
            }))
        }),
    )
    .map_err(|e| e.to_string())?;
    let out = result.lock().unwrap().take();
    Ok(out)
}

struct EnrollApp {
    paths: Paths,
    server_addr: Option<SocketAddr>,
    link: String,
    /// Only used for a `ICU1-…` blob or a bare `host[:port]` bootstrap — an
    /// `icu://` link already carries its own name.
    name: String,
    status: String,
    busy: Arc<Mutex<Option<Result<Identity, String>>>>,
    result: Arc<Mutex<Option<Identity>>>,
}

impl eframe::App for EnrollApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(100));
        if let Some(done) = self.busy.lock().unwrap().take() {
            match done {
                Ok(identity) => match identity.save(&self.paths) {
                    Ok(()) => {
                        *self.result.lock().unwrap() = Some(identity);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Err(e) => self.status = format!("Speichern fehlgeschlagen: {e}"),
                },
                Err(e) => self.status = e,
            }
        }
        egui::CentralPanel::default().show(root, |ui| {
            ui.heading("Einladung");
            ui.label("icu://host:port/enroll?t=…&ca=…&n=…, ICU1-… oder host:port");
            ui.text_edit_singleline(&mut self.link);
            let needs_name = !self.link.trim_start().starts_with("icu://");
            if needs_name {
                ui.label("Dein Name");
                ui.text_edit_singleline(&mut self.name);
            }
            if ui.button("Beitreten").clicked() {
                match InviteLink::parse(&self.link) {
                    Ok(link) => {
                        let (host, port) = link.host_port();
                        self.status = format!("Verbinde mit {host}:{port}…");
                        let busy = self.busy.clone();
                        let addr = self.server_addr;
                        let name = self.name.clone();
                        std::thread::spawn(move || {
                            let rt = tokio::runtime::Runtime::new().expect("runtime");
                            let r = rt
                                .block_on(enroll(&link, &name, addr))
                                .map_err(|e| e.to_string());
                            *busy.lock().unwrap() = Some(r);
                        });
                    }
                    Err(e) => self.status = e.to_string(),
                }
            }
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
        });
    }
}
