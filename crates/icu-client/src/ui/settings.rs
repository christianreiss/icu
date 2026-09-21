//! Settings panel, shown as a `CentralPanel` takeover with an icon rail
//! (`main_window.rs`). Every change is applied at once through
//! `Command::UpdateSettings`.

use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use egui::{RichText, Ui};
use egui_phosphor::regular as icon;

use super::{theme, widgets};
use crate::{
    audio::DeviceKind,
    config::{Corner, HotkeyBackend, ModeSetting, Paths, Settings, TileSize},
    engine::{AppState, Command},
    stats::DebugStats,
};

/// Which settings section is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Audio,
    Hotkeys,
    Camera,
    General,
    About,
}

/// Cached, ~2s-refreshed text for the About tab's Diagnose group — avoids
/// locking the three histogram mutexes every frame at 60fps.
#[derive(Default)]
struct DiagnoseCache {
    refreshed: Option<Instant>,
    jitter: String,
    capture_send: String,
    dsp_block: String,
    packet_loss: String,
    fec_plc: String,
    ref_underruns: String,
    reconnects: String,
}

/// Draft state of the settings panel.
pub struct SettingsView {
    /// Working copy, applied on change.
    pub draft: Settings,
    /// Name field (applied with the button, not on every keystroke).
    pub name: String,
    /// Display name for the next invite to mint (admin only).
    pub invite_name: String,
    /// Hotkey backend in use, for display.
    pub hotkey_backend: String,
    /// Portal hint shown once.
    pub show_portal_hint: bool,
    /// Active tab.
    tab: Tab,
    /// Which hotkey rows currently show their editable fields.
    expanded_bindings: std::collections::HashSet<&'static str>,
    stats: Arc<DebugStats>,
    paths: Paths,
    diagnose: DiagnoseCache,
    export_status: Option<String>,
}

impl SettingsView {
    /// From the engine's current settings.
    pub fn new(
        state: &AppState,
        hotkey_backend: String,
        stats: Arc<DebugStats>,
        paths: Paths,
    ) -> SettingsView {
        SettingsView {
            draft: state.settings.clone(),
            name: state
                .me
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_default(),
            invite_name: String::new(),
            hotkey_backend,
            show_portal_hint: true,
            tab: Tab::Audio,
            expanded_bindings: std::collections::HashSet::new(),
            stats,
            paths,
            diagnose: DiagnoseCache::default(),
            export_status: None,
        }
    }

    /// Draws the panel content. Returns commands to send.
    pub fn ui(&mut self, ui: &mut Ui, state: &AppState) -> Vec<Command> {
        let mut cmds = Vec::new();
        let before = self.draft.clone();

        egui::Panel::left("settings_rail")
            .resizable(false)
            .default_size(72.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .stroke(egui::Stroke::new(
                        1.0,
                        ui.visuals().widgets.inactive.bg_stroke.color,
                    ))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(3)
                    .show(ui, |ui| {
                        for (tab, glyph, label) in [
                            (Tab::Audio, icon::SLIDERS, "Audio"),
                            (Tab::Hotkeys, icon::COMMAND, "Hotkeys"),
                            (Tab::Camera, icon::VIDEO_CAMERA, "Kamera"),
                            (Tab::General, icon::GEAR_FINE, "Allgemein"),
                            (Tab::About, icon::QUESTION, "Über"),
                        ] {
                            if rail_button(ui, glyph, label, self.tab == tab) {
                                self.tab = tab;
                            }
                            ui.add_space(3.0);
                        }
                    });
            });

        egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
            Tab::Audio => self.audio_tab(ui, state),
            Tab::Hotkeys => self.hotkeys_tab(ui),
            Tab::Camera => self.camera_tab(ui),
            Tab::General => self.general_tab(ui, state, &mut cmds),
            Tab::About => self.about_tab(ui, state),
        });

        if self.draft != before {
            cmds.push(Command::UpdateSettings(Box::new(self.draft.clone())));
        }
        cmds
    }

    fn audio_tab(&mut self, ui: &mut Ui, state: &AppState) {
        let inputs: Vec<_> = state
            .devices
            .iter()
            .filter(|d| d.kind == DeviceKind::Input)
            .collect();
        let outputs: Vec<_> = state
            .devices
            .iter()
            .filter(|d| d.kind == DeviceKind::Output)
            .collect();

        widgets::group_card(ui, "Geräte", |ui| {
            device_combo(ui, "Mikrofon", &mut self.draft.audio.input_device, &inputs);
            device_combo(
                ui,
                "Wiedergabe",
                &mut self.draft.audio.output_device,
                &outputs,
            );
        });

        widgets::group_card(ui, "Verarbeitung", |ui| {
            widgets::setting_row(ui, false, "Modus", None, |ui| {
                widgets::segmented(
                    ui,
                    &mut self.draft.audio.mode,
                    &[
                        (ModeSetting::Ptt, "Push-to-Talk"),
                        (ModeSetting::Vad, "Sprachaktivierung"),
                    ],
                )
            });
            widgets::slider_row(
                ui,
                true,
                "VAD-Schwelle",
                &mut self.draft.audio.vad_threshold,
                0.0..=1.0,
                "",
            );
            widgets::slider_row(
                ui,
                true,
                "Verstärkung",
                &mut self.draft.audio.gain_db,
                -20.0..=20.0,
                " dB",
            );
            let mut release = self.draft.audio.release_ms as f32;
            if widgets::slider_row(ui, true, "PTT-Nachlauf", &mut release, 0.0..=1000.0, " ms")
                .changed()
            {
                self.draft.audio.release_ms = release as u32;
            }
            let mut hangover = self.draft.audio.hangover_ms as f32;
            if widgets::slider_row(ui, true, "VAD-Nachlauf", &mut hangover, 0.0..=1000.0, " ms")
                .changed()
            {
                self.draft.audio.hangover_ms = hangover as u32;
            }
            widgets::slider_row(
                ui,
                true,
                "Wiedergabe",
                &mut self.draft.audio.output_volume,
                0.0..=200.0,
                " %",
            );
            widgets::setting_row(ui, true, "Opus-Frame", None, |ui| {
                widgets::segmented(
                    ui,
                    &mut self.draft.audio.frame_ms,
                    &[(20u8, "20 ms"), (10u8, "10 ms")],
                )
            });
            widgets::setting_row(ui, true, "Rauschunterdrückung", Some("RNNoise"), |ui| {
                widgets::toggle(ui, &mut self.draft.audio.denoise)
            });
            widgets::setting_row(
                ui,
                true,
                "Echo-Unterdrückung",
                Some(
                    "Entfernt auch den eigenen Spielton aus dem gesendeten Signal (AEC3, ~9 ms Latenz).",
                ),
                |ui| widgets::toggle(ui, &mut self.draft.audio.aec),
            );
            let tok = theme::tokens(ui.visuals());
            if !state.aec_available {
                ui.label(
                    RichText::new("Auf dieser Installation nicht verfügbar")
                        .small()
                        .color(tok.dim_text),
                );
            } else if let (Some(erl), Some(erle)) =
                (state.aec_stats.erl_db, state.aec_stats.erle_db)
            {
                ui.label(
                    RichText::new(format!("ERL {erl:.0} dB · ERLE {erle:.0} dB"))
                        .monospace()
                        .small(),
                );
            }
        });

        widgets::group_card(ui, "Töne", |ui| {
            widgets::setting_row(ui, false, "Beitritt/Verlassen", None, |ui| {
                widgets::toggle(ui, &mut self.draft.audio.cue_join)
            });
            widgets::setting_row(ui, true, "Sendebeginn/-ende", None, |ui| {
                widgets::toggle(ui, &mut self.draft.audio.cue_tx)
            });
            widgets::setting_row(ui, true, "Stummschalten", None, |ui| {
                widgets::toggle(ui, &mut self.draft.audio.cue_mute)
            });
        });
    }

    fn hotkeys_tab(&mut self, ui: &mut Ui) {
        widgets::group_card(ui, "Backend", |ui| {
            widgets::setting_row(ui, false, "Backend", None, |ui| {
                widgets::segmented(
                    ui,
                    &mut self.draft.hotkeys.backend,
                    &[
                        (HotkeyBackend::Auto, "auto"),
                        (HotkeyBackend::Portal, "Portal"),
                        (HotkeyBackend::Evdev, "evdev"),
                    ],
                )
            });
            widgets::setting_row(ui, true, "Aktiv", None, |ui| {
                ui.label(RichText::new(format!("✓ {}", self.hotkey_backend)).monospace())
            });
        });

        widgets::group_card(ui, "Belegung", |ui| {
            binding_row(
                ui,
                false,
                &mut self.expanded_bindings,
                "PTT",
                &mut self.draft.hotkeys.ptt,
            );
            binding_row(
                ui,
                true,
                &mut self.expanded_bindings,
                "Mute",
                &mut self.draft.hotkeys.mute,
            );
            binding_row(
                ui,
                true,
                &mut self.expanded_bindings,
                "Deafen",
                &mut self.draft.hotkeys.deafen,
            );
            binding_row(
                ui,
                true,
                &mut self.expanded_bindings,
                "PTT/VAD",
                &mut self.draft.hotkeys.toggle_mode,
            );
        });

        let no_action: Option<(&str, fn())> = None;
        widgets::banner(
            ui,
            widgets::BannerKind::Warning,
            "evdev liest alle Tasten",
            "evdev-Namen wie KEY_RIGHTCTRL, BTN_SIDE oder code:97; Portal-Trigger wie \
             CTRL+SHIFT+F12. evdev braucht Gruppe input und liest technisch jeden \
             Tastendruck mit (Keylogger-Aspekt). Änderungen gelten ab dem nächsten Start.",
            no_action,
            None::<fn()>,
        );
    }

    fn camera_tab(&mut self, ui: &mut Ui) {
        widgets::group_card(ui, "Kamera", |ui| {
            widgets::setting_row(ui, false, "Beim Start einschalten", None, |ui| {
                widgets::toggle(ui, &mut self.draft.camera.enabled)
            });
            widgets::setting_row(ui, true, "Eigene Vorschau anzeigen", None, |ui| {
                widgets::toggle(ui, &mut self.draft.camera.show_self_preview)
            });
            let mut dev = self.draft.camera.device.clone().unwrap_or_default();
            widgets::setting_row(ui, true, "Gerät (Index)", None, |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut dev).desired_width(120.0));
                if r.changed() {
                    self.draft.camera.device = if dev.is_empty() {
                        None
                    } else {
                        Some(dev.clone())
                    };
                }
                r
            });
        });

        widgets::group_card(ui, "Overlay", |ui| {
            widgets::setting_row(ui, false, "Anzeigen", None, |ui| {
                widgets::toggle(ui, &mut self.draft.overlay.enabled)
            });
            widgets::setting_row(ui, true, "Ecke", None, |ui| {
                corner_picker(ui, &mut self.draft.overlay.corner);
            });
            widgets::setting_row(ui, true, "Abstand", None, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.draft.overlay.offset[0])
                            .prefix("x ")
                            .range(0..=2000),
                    );
                    ui.add(
                        egui::DragValue::new(&mut self.draft.overlay.offset[1])
                            .prefix("y ")
                            .range(0..=2000),
                    );
                });
            });
            widgets::setting_row(ui, true, "Kachelgröße", None, |ui| {
                widgets::segmented(
                    ui,
                    &mut self.draft.overlay.size,
                    &[(TileSize::Small, "klein"), (TileSize::Medium, "mittel")],
                )
            });
            widgets::setting_row(ui, true, "Stille Teilnehmer abdunkeln", None, |ui| {
                widgets::toggle(ui, &mut self.draft.overlay.dim_non_speaking)
            });
        });
    }

    fn general_tab(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        widgets::group_card(ui, "Identität", |ui| {
            widgets::setting_row(ui, false, "Anzeigename", None, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(140.0));
                    if ui.button("Übernehmen").clicked() {
                        cmds.push(Command::SetName(self.name.trim().to_string()));
                    }
                });
            });
            if let Some(err) = &state.name_error {
                ui.colored_label(theme::tokens(ui.visuals()).error, err);
            }
            widgets::setting_row(ui, true, "Server", None, |ui| {
                ui.label(RichText::new(&state.server).monospace())
            });
            widgets::setting_row(ui, true, "user_id", None, |ui| {
                ui.label(
                    RichText::new(
                        state
                            .me
                            .as_ref()
                            .map(|m| m.user_id.to_string())
                            .unwrap_or_else(|| "—".to_string()),
                    )
                    .monospace(),
                )
            });
        });

        if state.is_admin {
            widgets::group_card(ui, "Einladung", |ui| {
                widgets::setting_row(ui, false, "Name der eingeladenen Person", None, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.invite_name).desired_width(140.0),
                        );
                        if ui.button("Erzeugen").clicked() {
                            cmds.push(Command::CreateInvite {
                                name: self.invite_name.trim().to_string(),
                                ttl_s: 0,
                            });
                        }
                    });
                });
                if let Some(err) = &state.invite_error {
                    ui.colored_label(theme::tokens(ui.visuals()).error, err);
                }
                if let Some((blob, expires)) = &state.last_invite {
                    ui.label(format!("Gültig bis {expires}, einmal einfügbar:"));
                    let mut copy = blob.clone();
                    ui.add(
                        egui::TextEdit::multiline(&mut copy)
                            .desired_rows(3)
                            .font(egui::TextStyle::Monospace),
                    );
                }
            });
        }

        widgets::group_card(ui, "Start und Fenster", |ui| {
            widgets::setting_row(ui, false, "Minimiert starten", None, |ui| {
                widgets::toggle(ui, &mut self.draft.ui.start_minimized)
            });
            widgets::setting_row(ui, true, "icu://-Links mit ICU öffnen", None, |ui| {
                widgets::toggle(ui, &mut self.draft.ui.register_uri_handler)
            });
        });

        widgets::group_card(ui, "Darstellung", |ui| {
            ui.horizontal(|ui| {
                for (value, label) in [("system", "System"), ("light", "Hell"), ("dark", "Dunkel")]
                {
                    theme_swatch(ui, &mut self.draft.ui.theme, value, label);
                }
            });
        });
    }

    fn about_tab(&mut self, ui: &mut Ui, state: &AppState) {
        widgets::group_card(ui, "Info", |ui| {
            widgets::setting_row(ui, false, "Version", None, |ui| {
                ui.label(format!("ICU {}", env!("CARGO_PKG_VERSION")))
            });
            widgets::setting_row(ui, true, "Audio-Backend", None, |ui| {
                ui.label(state.backend)
            });
            ui.label(
                RichText::new(
                    "Enthält Opus (BSD-3), RNNoise/nnnoiseless (BSD-3), WebRTC AudioProcessing \
                     (BSD-3), OpenH264 (BSD-2, aus Quelle gebaut — nicht unter Ciscos \
                     Lizenzgewährung für Binaries), Inter (OFL-1.1), Phosphor-Icons (MIT/Apache-2.0). \
                     Lizenztexte liegen dem Release bei.",
                )
                .small(),
            );
        });

        self.refresh_diagnose();

        widgets::group_card(ui, "Diagnose", |ui| {
            widgets::setting_row(ui, false, "RTT", None, |ui| {
                ui.label(
                    RichText::new(
                        state
                            .rtt_ms
                            .map(|r| format!("{r:.0} ms"))
                            .unwrap_or_else(|| "n/a".to_string()),
                    )
                    .monospace(),
                )
            });
            widgets::setting_row(ui, true, "Jitter p50/p95", None, |ui| {
                ui.label(RichText::new(&self.diagnose.jitter).monospace())
            });
            widgets::setting_row(ui, true, "Capture→Send p50/max", None, |ui| {
                ui.label(RichText::new(&self.diagnose.capture_send).monospace())
            });
            widgets::setting_row(ui, true, "DSP-Block p50/max", None, |ui| {
                ui.label(RichText::new(&self.diagnose.dsp_block).monospace())
            });
            widgets::setting_row(ui, true, "Paketverlust", None, |ui| {
                ui.label(RichText::new(&self.diagnose.packet_loss).monospace())
            });
            widgets::setting_row(ui, true, "FEC/PLC", None, |ui| {
                ui.label(RichText::new(&self.diagnose.fec_plc).monospace())
            });
            widgets::setting_row(ui, true, "Referenz-Underruns", None, |ui| {
                ui.label(RichText::new(&self.diagnose.ref_underruns).monospace())
            });
            widgets::setting_row(ui, true, "Reconnects", None, |ui| {
                ui.label(RichText::new(&self.diagnose.reconnects).monospace())
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .button(format!("{} Bericht kopieren", icon::COPY))
                    .clicked()
                {
                    ui.ctx().copy_text(self.stats.report());
                }
                if ui
                    .button(format!("{} Histogramm exportieren", icon::EXPORT))
                    .clicked()
                {
                    let path = self.paths.config_dir.join("histograms.txt");
                    self.export_status = Some(match std::fs::write(&path, self.stats.report()) {
                        Ok(()) => format!("Geschrieben: {}", path.display()),
                        Err(e) => format!("Fehlgeschlagen: {e}"),
                    });
                }
            });
            if let Some(status) = &self.export_status {
                let tok = theme::tokens(ui.visuals());
                ui.label(RichText::new(status).small().color(tok.dim_text));
            }
        });
    }

    /// Refreshes the About tab's cached diagnostics at most every 2s —
    /// called only while the About tab is showing, never every frame.
    fn refresh_diagnose(&mut self) {
        let now = Instant::now();
        if self
            .diagnose
            .refreshed
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(2))
        {
            return;
        }
        self.diagnose.refreshed = Some(now);
        let fmt_ms = |v: f64| {
            if v.is_infinite() {
                ">1000 ms".to_string()
            } else {
                format!("{v:.1} ms")
            }
        };
        {
            let h = self.stats.jitter_delay.lock().unwrap();
            self.diagnose.jitter = format!(
                "{} / {}",
                fmt_ms(h.quantile_ms(0.5)),
                fmt_ms(h.quantile_ms(0.95))
            );
        }
        {
            let h = self.stats.capture_to_send.lock().unwrap();
            self.diagnose.capture_send =
                format!("{} / {}", fmt_ms(h.quantile_ms(0.5)), fmt_ms(h.max_ms()));
        }
        {
            let h = self.stats.dsp_block.lock().unwrap();
            self.diagnose.dsp_block =
                format!("{} / {}", fmt_ms(h.quantile_ms(0.5)), fmt_ms(h.max_ms()));
        }
        self.diagnose.packet_loss =
            format!("{} %", self.stats.packet_loss_perc.load(Ordering::Relaxed));
        self.diagnose.fec_plc = format!(
            "{} / {}",
            DebugStats::get(&self.stats.frames_fec),
            DebugStats::get(&self.stats.frames_plc),
        );
        self.diagnose.ref_underruns = DebugStats::get(&self.stats.reference_underruns).to_string();
        self.diagnose.reconnects = DebugStats::get(&self.stats.reconnects).to_string();
    }
}

/// One icon-rail entry.
fn rail_button(ui: &mut Ui, glyph: &str, label: &str, active: bool) -> bool {
    let tok = theme::tokens(ui.visuals());
    let text_color = if active { tok.accent } else { tok.muted_text };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(64.0, 38.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if active {
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(8),
                ui.visuals().faint_bg_color,
            );
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(8),
                egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
                egui::StrokeKind::Inside,
            );
        }
        painter.text(
            egui::pos2(rect.center().x, rect.top() + 13.0),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::new(17.0, egui::FontFamily::Proportional),
            text_color,
        );
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 9.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::new(9.5, super::fonts::semibold()),
            text_color,
        );
    }
    resp.clicked()
}

/// A 2x2 grid of corner buttons, each drawing a small anchored rectangle to
/// preview where the overlay tile stack would sit.
fn corner_picker(ui: &mut Ui, corner: &mut Corner) {
    let tok = theme::tokens(ui.visuals());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (value, dx, dy) in [
            (Corner::TopLeft, 0.0, 0.0),
            (Corner::TopRight, 1.0, 0.0),
            (Corner::BottomLeft, 0.0, 1.0),
            (Corner::BottomRight, 1.0, 1.0),
        ] {
            let selected = *corner == value;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(46.0, 46.0), egui::Sense::click());
            if ui.is_rect_visible(rect) {
                let painter = ui.painter();
                painter.rect_filled(
                    rect,
                    egui::CornerRadius::same(8),
                    ui.visuals().extreme_bg_color,
                );
                painter.rect_stroke(
                    rect,
                    egui::CornerRadius::same(8),
                    egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
                    egui::StrokeKind::Inside,
                );
                let tile = egui::vec2(22.0, 14.0);
                let pos = egui::pos2(
                    rect.left() + 4.0 + dx * (rect.width() - tile.x - 8.0),
                    rect.top() + 4.0 + dy * (rect.height() - tile.y - 8.0),
                );
                let tile_rect = egui::Rect::from_min_size(pos, tile);
                let tile_color = if selected {
                    tok.accent
                } else {
                    ui.visuals().widgets.inactive.bg_stroke.color
                };
                painter.rect_filled(tile_rect, 2.0, tile_color);
            }
            if resp.clicked() {
                *corner = value;
            }
        }
    });
}

/// A small preview-swatch theme picker, replacing the old `selectable_label`
/// row. Swatch colors are fixed representative values for each theme (not
/// `theme::tokens`, which only knows the *current* theme's palette).
fn theme_swatch(ui: &mut Ui, current: &mut String, value: &str, label: &str) {
    let selected = current == value;
    let dark = value == "dark" || (value == "system" && ui.visuals().dark_mode);
    let (panel, accent) = if dark {
        (
            egui::Color32::from_rgb(0x0e, 0x0f, 0x13),
            egui::Color32::from_rgb(0x2e, 0x9b, 0xff),
        )
    } else {
        (
            egui::Color32::from_rgb(0xf4, 0xf6, 0xf9),
            egui::Color32::from_rgb(0x16, 0x68, 0xc7),
        )
    };
    ui.vertical(|ui| {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(64.0, 44.0), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            painter.rect_filled(rect, egui::CornerRadius::same(8), panel);
            let border = if selected {
                egui::Stroke::new(2.0, accent)
            } else {
                egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color)
            };
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(8),
                border,
                egui::StrokeKind::Inside,
            );
            let dot = egui::Rect::from_min_size(
                rect.left_top() + egui::vec2(6.0, 6.0),
                egui::vec2(10.0, 10.0),
            );
            painter.rect_filled(dot, 5.0, accent);
        }
        if resp.clicked() {
            *current = value.to_string();
        }
        ui.label(RichText::new(label).size(10.5));
    });
}

fn device_combo(
    ui: &mut Ui,
    label: &str,
    selected: &mut Option<String>,
    devices: &[&crate::audio::DeviceInfo],
) {
    let current = match selected {
        None => "Standard".to_string(),
        Some(id) => devices
            .iter()
            .find(|d| &d.id == id)
            .map(|d| d.description.clone())
            .unwrap_or_else(|| format!("{id} (nicht vorhanden)")),
    };
    widgets::setting_row(ui, false, label, None, |ui| {
        egui::ComboBox::from_id_salt(label)
            .selected_text(current)
            .width(200.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(selected.is_none(), "Standard")
                    .clicked()
                {
                    *selected = None;
                }
                for d in devices {
                    let is = selected.as_deref() == Some(d.id.as_str());
                    if ui.selectable_label(is, &d.description).clicked() {
                        *selected = Some(d.id.clone());
                    }
                }
            });
    });
}

/// A hotkey binding row: two read-only `kbd` chips plus an "Ändern" button
/// that reveals the editable evdev/Portal text fields.
fn binding_row(
    ui: &mut Ui,
    divider: bool,
    expanded: &mut std::collections::HashSet<&'static str>,
    label: &'static str,
    binding: &mut crate::config::KeyBinding,
) {
    if divider {
        ui.separator();
    }
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 11))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [92.0, 0.0],
                    egui::Label::new(RichText::new(label).size(13.5)),
                );
                widgets::kbd(ui, &binding.key);
                widgets::kbd(ui, binding.trigger.as_deref().unwrap_or("—"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let is_open = expanded.contains(label);
                    if ui
                        .small_button(if is_open { "Fertig" } else { "Ändern" })
                        .clicked()
                    {
                        if is_open {
                            expanded.remove(label);
                        } else {
                            expanded.insert(label);
                        }
                    }
                });
            });
            if expanded.contains(label) {
                ui.horizontal(|ui| {
                    ui.add_space(92.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut binding.key)
                            .desired_width(120.0)
                            .hint_text("evdev"),
                    );
                    let mut trigger = binding.trigger.clone().unwrap_or_default();
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut trigger)
                                .desired_width(120.0)
                                .hint_text("Portal"),
                        )
                        .changed()
                    {
                        binding.trigger = if trigger.is_empty() {
                            None
                        } else {
                            Some(trigger)
                        };
                    }
                });
            }
        });
}
