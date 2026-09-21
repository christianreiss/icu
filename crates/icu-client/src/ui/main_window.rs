//! The main window: status, participants, level, buttons.

use std::{
    net::SocketAddr,
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};

use egui::{Color32, RichText};
use egui_phosphor::regular as icon;

use super::{fonts, settings::SettingsView, theme};
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

        if self.debug_stats && self.last_stats.elapsed() > Duration::from_secs(2) {
            self.last_stats = Instant::now();
            self.stats_text = self.stats.report();
            eprintln!("{}", self.stats_text);
        }

        egui::Panel::top("status").show(root, |ui| {
            ui.horizontal(|ui| {
                let (color, text) = match &state.conn {
                    ConnStatus::Connected => (theme::CONNECTED, "verbunden".to_string()),
                    ConnStatus::Connecting => (theme::CONNECTING, "verbinde…".to_string()),
                    ConnStatus::Disconnected { reason, attempt } => (
                        theme::CONNECTING,
                        format!("getrennt ({reason}), Versuch {attempt}"),
                    ),
                    ConnStatus::Fatal(reason) => (theme::ERROR, format!("Fehler: {reason}")),
                };
                let (dot_rect, _) =
                    ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 5.0, color);
                ui.label(
                    RichText::new("ICU")
                        .font(egui::FontId::new(16.0, fonts::semibold()))
                        .strong(),
                );
                ui.label(RichText::new(&state.server).small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(rtt) = state.rtt_ms {
                        ui.label(RichText::new(format!("{rtt:.0} ms")).small());
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
                        ui.label(RichText::new(label).small().color(theme::CONNECTING));
                    }
                });
                ui.label(RichText::new(text).small())
                    .on_hover_text(match &state.me {
                        Some(me) => format!("{} (user_id {})", me.name, me.user_id),
                        None => String::new(),
                    });
            });
            if let Some(min) = &state.update_required {
                ui.colored_label(
                    theme::ERROR,
                    format!(
                        "Update nötig: der Server verlangt mindestens Version {min} (diese: {})",
                        env!("CARGO_PKG_VERSION")
                    ),
                );
            }
            if let Some(banner) = &state.banner {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::ERROR, banner);
                    if ui.small_button(icon::X).clicked() {
                        self.send(Command::DismissBanner);
                    }
                });
            }
        });

        egui::Panel::bottom("controls").show(root, |ui| {
            ui.add_space(4.0);
            // Level meter with the VAD threshold marker.
            let (rect, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 14.0), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
            let frac = theme::meter_fraction(state.level.rms_db);
            let fill =
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * frac, rect.height()));
            let color = if state.sending {
                theme::LEVEL_HOT
            } else {
                theme::LEVEL
            };
            painter.rect_filled(fill, 3.0, color);
            if state.mode == Mode::Vad {
                let x = rect.min.x + rect.width() * state.vad_prob.clamp(0.0, 1.0);
                painter.line_segment(
                    [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
                    egui::Stroke::new(2.0, Color32::WHITE),
                );
            }
            ui.label(
                RichText::new(format!(
                    "{:.0} dBFS · VAD {:.2}{}",
                    state.level.rms_db,
                    state.vad_prob,
                    if state.sending { " · sendet" } else { "" }
                ))
                .small(),
            );

            ui.horizontal_wrapped(|ui| {
                let ptt_label = match state.mode {
                    Mode::Ptt => "PTT (halten)",
                    Mode::Vad => "VAD",
                };
                let mut ptt_btn = egui::Button::new(ptt_label);
                if state.ptt_down {
                    ptt_btn = ptt_btn.fill(theme::SPEAKING);
                }
                let ptt = ui.add_sized([100.0, 28.0], ptt_btn);
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
                if ptt.secondary_clicked() {
                    self.send(Command::ToggleMode);
                }
                ptt.on_hover_text("Rechtsklick: PTT/VAD umschalten");

                let mut mute_btn = egui::Button::new(if state.muted { "Mute ✓" } else { "Mute" });
                if state.muted {
                    mute_btn = mute_btn.fill(theme::MUTED);
                }
                let mute = ui.add_sized([70.0, 28.0], mute_btn);
                if mute.clicked() {
                    self.send(Command::SetMuted(!state.muted));
                }
                let mut deafen_btn = egui::Button::new(if state.deafened {
                    "Deafen ✓"
                } else {
                    "Deafen"
                });
                if state.deafened {
                    deafen_btn = deafen_btn.fill(theme::MUTED);
                }
                let deafen = ui.add_sized([80.0, 28.0], deafen_btn);
                if deafen.clicked() {
                    self.send(Command::SetDeafened(!state.deafened));
                }
            });
            ui.horizontal_wrapped(|ui| {
                #[cfg(feature = "camera")]
                if ui.selectable_label(state.camera_on, "Kamera").clicked() {
                    self.send(Command::SetCamera(!state.camera_on));
                }
                #[cfg(feature = "overlay")]
                if ui.selectable_label(state.overlay_on, "Overlay").clicked() {
                    self.send(Command::SetOverlay(!state.overlay_on));
                }
                if ui
                    .selectable_label(
                        self.settings.is_some(),
                        format!("{} Einstellungen", icon::GEAR),
                    )
                    .clicked()
                {
                    self.settings = match self.settings.take() {
                        Some(_) => None,
                        None => Some(SettingsView::new(&state, state.hotkey_backend.clone())),
                    };
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(root, |ui| {
            if self.settings.is_some() {
                let mut close = false;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Einstellungen")
                            .font(egui::FontId::new(20.0, fonts::semibold())),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(icon::X).clicked() {
                            close = true;
                        }
                    });
                });
                if close {
                    self.settings = None;
                } else {
                    ui.separator();
                    let view = self.settings.as_mut().expect("checked above");
                    let cmds = view.ui(ui, &state);
                    for c in cmds {
                        if let Command::UpdateSettings(s) = &c {
                            theme::apply(ctx, &s.ui.theme);
                        }
                        self.send(c);
                    }
                }
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                if let Some(me) = &state.me {
                    participant_row(
                        ui,
                        &me.name,
                        state.sending,
                        state.muted,
                        state.deafened,
                        state.camera_on,
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
                        p.speaking,
                        p.info.flags.muted(),
                        p.info.flags.deafened(),
                        p.info.flags.camera_on(),
                        Some((p.info.user_id, p.volume)),
                        &mut self.volume_open,
                        &self.cmd,
                        false,
                    );
                }
                if state.peers.is_empty() && state.conn == ConnStatus::Connected {
                    ui.label(RichText::new("Niemand sonst ist da.").italics().weak());
                }
                #[cfg(feature = "camera")]
                if let Some(camera) = self.camera.as_mut() {
                    if state.camera_on {
                        camera.preview(ui);
                    }
                }
                if self.debug_stats && !self.stats_text.is_empty() {
                    ui.separator();
                    ui.label(RichText::new(&self.stats_text).monospace().small());
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

#[allow(clippy::too_many_arguments)]
fn participant_row(
    ui: &mut egui::Ui,
    name: &str,
    speaking: bool,
    muted: bool,
    deafened: bool,
    camera: bool,
    volume: Option<(icu_proto::UserId, f32)>,
    volume_open: &mut Option<icu_proto::UserId>,
    cmd: &tokio::sync::mpsc::UnboundedSender<Command>,
    is_me: bool,
) {
    let row = ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(egui::vec2(4.0, 20.0), egui::Sense::hover());
        ui.painter().rect_filled(
            bar,
            2.0,
            if speaking {
                theme::SPEAKING
            } else {
                Color32::TRANSPARENT
            },
        );
        let mut text = RichText::new(name);
        if is_me {
            text = text.strong();
        }
        ui.label(text);
        if camera {
            ui.label(icon::VIDEO_CAMERA);
        }
        if muted {
            ui.colored_label(theme::MUTED, icon::MICROPHONE_SLASH);
        }
        if deafened {
            ui.colored_label(theme::MUTED, icon::SPEAKER_SLASH);
        }
        if let Some((user_id, vol)) = volume {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(format!("{vol:.0} %"))
                    .on_hover_text("Lautstärke")
                    .clicked()
                {
                    *volume_open = if *volume_open == Some(user_id) {
                        None
                    } else {
                        Some(user_id)
                    };
                }
            });
        }
    });
    let _ = row;
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
