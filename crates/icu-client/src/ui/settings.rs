//! Settings panel content: an icon rail plus tab content, drawn into its own
//! `egui` viewport window (`main_window.rs`, mirrors the Debug window).
//! Every change is applied at once through `Command::UpdateSettings`.

use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use egui::{RichText, Ui};
use egui_phosphor::regular as icon;
use icu_proto::UserId;

use super::{theme, widgets};
use crate::{
    audio::DeviceKind,
    config::{
        self, Corner, Expand, HotkeyBackend, Identity, ModeSetting, Paths, Settings, TileSize,
    },
    engine::{AppState, Command},
    stats::DebugStats,
};

/// Text-field width wherever a row pairs a field with a button, so the
/// fields line up down the panel.
const FIELD_W: f32 = 150.0;
/// Validity choices for an invite; the last is the server's cap.
const INVITE_TTLS: [(u64, &str); 4] = [
    (60 * 60, "1 Stunde"),
    (icu_proto::limits::INVITE_TTL_S, "24 Stunden"),
    (3 * 24 * 60 * 60, "3 Tage"),
    (icu_proto::limits::MAX_INVITE_TTL_S, "7 Tage"),
];
/// One square of the overlay-position picker.
const CORNER_SWATCH: f32 = 46.0;
/// Gap between the picker's squares, and the wider gap before the two
/// full-edge options.
const CORNER_GAP: f32 = 4.0;
const CORNER_SPLIT: f32 = 8.0;
/// Six squares, five gaps, and the split between the corners and the edges.
const CORNER_PICKER_W: f32 = CORNER_SWATCH * 6.0 + CORNER_GAP * 6.0 + CORNER_SPLIT;

/// Which settings section is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Audio,
    Hotkeys,
    Camera,
    General,
    Users,
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
    /// What [`Self::name`] was last synced to, so a rename that happened
    /// elsewhere (another device catching up) can refill the field without
    /// overwriting what the user is typing.
    name_synced: String,
    /// Pasted passport waiting to be imported.
    passport_import: String,
    /// Result of the last passport export or import.
    passport_status: Option<String>,
    /// A parsed passport waiting for confirmation: importing replaces the
    /// identity, and the passport brings its own server, so the user sees
    /// which one first (`PROTOCOL.md` §2.3.1).
    import_preview: Option<(Box<Identity>, config::ImportSummary)>,
    /// Display name for the next invite to mint (admin only).
    pub invite_name: String,
    /// Validity of the next invite, seconds.
    invite_ttl_s: u64,
    /// Result of the last "Speichern" of an invite.
    invite_status: Option<String>,
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
    /// Revoke is destructive: first click starts a ~3s confirm window
    /// (in-row, no modal per §10), second click within it actually sends
    /// `Command::RevokeUser`.
    confirm_revoke: HashMap<UserId, Instant>,
    /// Debug window open state, mirrored in/out by `main_window.rs` each
    /// frame (the window itself is owned by `App`, not this view — moved
    /// here from a top-bar button so it doesn't compete for header space).
    pub debug_window_open: bool,
    /// Live picture for the Kamera tab, fed by the running capture thread.
    #[cfg(feature = "camera")]
    preview: Option<crate::camera::CameraPreview>,
    /// Cameras found by `crate::camera::list()`. Refreshed when the Kamera
    /// tab is opened, never per frame: enumerating V4L2 devices opens every
    /// `/dev/video*` in turn.
    #[cfg(feature = "camera")]
    cameras: Vec<(u32, String)>,
    /// Whether [`Self::cameras`] still has to be filled. Set when the tab is
    /// opened, so a camera plugged in meanwhile shows up on the next visit.
    #[cfg(feature = "camera")]
    cameras_stale: bool,
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
            name_synced: state
                .me
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_default(),
            passport_import: String::new(),
            passport_status: None,
            import_preview: None,
            invite_name: String::new(),
            invite_ttl_s: icu_proto::limits::INVITE_TTL_S,
            invite_status: None,
            hotkey_backend,
            show_portal_hint: true,
            tab: Tab::Audio,
            expanded_bindings: std::collections::HashSet::new(),
            stats,
            paths,
            diagnose: DiagnoseCache::default(),
            export_status: None,
            confirm_revoke: HashMap::new(),
            debug_window_open: false,
            #[cfg(feature = "camera")]
            preview: None,
            #[cfg(feature = "camera")]
            cameras: Vec::new(),
            #[cfg(feature = "camera")]
            cameras_stale: true,
        }
    }

    /// Gives the Kamera tab a live picture from the running capture thread.
    /// Only the GUI binary has a camera handle to hand over.
    #[cfg(feature = "camera")]
    pub fn with_preview(mut self, slot: Arc<crate::camera::PreviewSlot>) -> SettingsView {
        self.preview = Some(crate::camera::CameraPreview::new(slot));
        self
    }

    /// Draws the panel content. Returns commands to send.
    pub fn ui(&mut self, ui: &mut Ui, state: &AppState) -> Vec<Command> {
        let mut cmds = Vec::new();
        let before = self.draft.clone();

        // A live demote can orphan the tab: it is only in the rail while
        // `state.is_admin`, so bounce back to Allgemein instead of rendering
        // a tab that is no longer selectable.
        if self.tab == Tab::Users && !state.is_admin {
            self.tab = Tab::General;
        }
        let tab_before = self.tab;

        let mut tabs = vec![
            (Tab::Audio, icon::SLIDERS, "Audio"),
            (Tab::Hotkeys, icon::COMMAND, "Hotkeys"),
            (Tab::Camera, icon::VIDEO_CAMERA, "Kamera"),
            (Tab::General, icon::GEAR_FINE, "Allgemein"),
            (Tab::About, icon::QUESTION, "Über"),
        ];
        if state.is_admin {
            tabs.insert(3, (Tab::Users, icon::USERS_THREE, "Nutzer"));
        }

        egui::Panel::left("settings_rail")
            .resizable(false)
            // 64px buttons + 3px frame inset each side + the panel's own
            // 8px side margins = 86px; 72px clipped the rail frame's right
            // edge and rounded corner.
            .default_size(88.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .stroke(egui::Stroke::new(
                        1.0,
                        ui.visuals().widgets.inactive.bg_stroke.color,
                    ))
                    .corner_radius(egui::CornerRadius::same(theme::R_CARD))
                    .inner_margin(3)
                    .show(ui, |ui| {
                        for (tab, glyph, label) in tabs {
                            if widgets::rail_button(ui, glyph, label, self.tab == tab) {
                                self.tab = tab;
                            }
                            ui.add_space(3.0);
                        }
                    });
            });

        if self.tab != tab_before {
            match self.tab {
                Tab::Users => cmds.push(Command::RequestUserList),
                #[cfg(feature = "camera")]
                Tab::Camera => self.cameras_stale = true,
                _ => {}
            }
        }

        egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
            Tab::Audio => self.audio_tab(ui, state),
            Tab::Hotkeys => self.hotkeys_tab(ui),
            Tab::Camera => self.camera_tab(ui, state),
            Tab::General => self.general_tab(ui, state, &mut cmds),
            Tab::Users => self.users_tab(ui, state, &mut cmds),
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
            ui.add_enabled_ui(!self.draft.audio.vad_auto, |ui| {
                widgets::slider_row(
                    ui,
                    true,
                    "VAD-Schwelle",
                    &mut self.draft.audio.vad_threshold,
                    0.0..=1.0,
                    "",
                )
            });
            ui.add_enabled_ui(self.draft.audio.denoise, |ui| {
                widgets::setting_row(
                    ui,
                    true,
                    "VAD automatisch",
                    Some("Kalibriert die Schwelle laufend auf den Umgebungspegel; benötigt Rauschunterdrückung."),
                    |ui| widgets::toggle(ui, &mut self.draft.audio.vad_auto),
                )
            });
            if self.draft.audio.vad_auto {
                widgets::setting_row(ui, true, "Aktuelle Schwelle", None, |ui| {
                    widgets::chip(ui, &format!("{:.2}", state.vad_threshold), 0.0)
                });
            }
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
            widgets::card_note(ui, |ui| {
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

    /// Export and import of the whole identity (`PROTOCOL.md` §2.3.1).
    ///
    /// The armored text is never rendered: it carries the private key in the
    /// clear, and a settings window is exactly the thing people have open
    /// during a screen share.
    fn passport_card(&mut self, ui: &mut Ui, cmds: &mut Vec<Command>) {
        widgets::group_card(ui, "Passport", |ui| {
            widgets::setting_row(ui, false, "Auf ein anderes Gerät mitnehmen", None, |ui| {
                let width = widgets::button_width(ui, icon::COPY, "Kopieren")
                    + 6.0
                    + widgets::button_width(ui, icon::EXPORT, "Speichern");
                widgets::row_group(ui, width, |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if widgets::button(ui, widgets::ButtonKind::Neutral, icon::COPY, "Kopieren")
                        .on_hover_text("Als Text in die Zwischenablage")
                        .clicked()
                    {
                        match self.passport_text() {
                            Ok(text) => {
                                ui.ctx().copy_text(text);
                                self.passport_status =
                                    Some("In die Zwischenablage kopiert.".into());
                            }
                            Err(e) => self.passport_status = Some(e),
                        }
                    }
                    if widgets::button(ui, widgets::ButtonKind::Neutral, icon::EXPORT, "Speichern")
                        .on_hover_text("Als Datei neben die Konfiguration, nur für dich lesbar")
                        .clicked()
                    {
                        self.passport_status = Some(match self.passport_text() {
                            Ok(text) => {
                                let path = self.paths.config_dir.join("icu-passport.txt");
                                match crate::config::write_atomic(&path, text.as_bytes(), true) {
                                    Ok(()) => format!("Gespeichert: {}", path.display()),
                                    Err(e) => format!("Speichern fehlgeschlagen: {e}"),
                                }
                            }
                            Err(e) => e,
                        });
                    }
                });
            });
            widgets::card_note(ui, |ui| {
                let tok = theme::tokens(ui.visuals());
                ui.colored_label(
                    tok.error,
                    RichText::new(
                        "Enthält deinen privaten Schlüssel im Klartext — wer diesen Text hat, \
                         kann sich als du anmelden.",
                    )
                    .size(11.5),
                );
                ui.label(
                    RichText::new(
                        "Nur über einen sicheren Kanal weitergeben, die Kopie danach löschen. \
                         Auf dem anderen Gerät hier oder im Einladungsfeld einfügen.",
                    )
                    .size(11.5)
                    .color(tok.dim_text),
                );
            });

            widgets::setting_row(ui, true, "Passport einspielen", None, |ui| {
                let width = widgets::button_width(ui, icon::MAGNIFYING_GLASS, "Prüfen");
                widgets::row_group(ui, width, |ui| {
                    let clicked = ui
                        .add_enabled_ui(
                            !self.passport_import.trim().is_empty() && self.import_preview.is_none(),
                            |ui| {
                                widgets::button(
                                    ui,
                                    widgets::ButtonKind::Neutral,
                                    icon::MAGNIFYING_GLASS,
                                    "Prüfen",
                                )
                            },
                        )
                        .inner
                        .on_hover_text("Zeigt, zu welchem Server der Passport gehört")
                        .clicked();
                    if clicked {
                        self.preview_passport();
                    }
                });
            });
            if let Some((_, summary)) = &self.import_preview {
                let lines = summary.lines();
                let warn = summary.ca_changed;
                widgets::card_note(ui, |ui| {
                    let tok = theme::tokens(ui.visuals());
                    for (i, line) in lines.iter().enumerate() {
                        let text = RichText::new(line).size(11.5).monospace();
                        if warn && i == lines.len() - 1 {
                            ui.colored_label(tok.error, text);
                        } else {
                            ui.label(text);
                        }
                    }
                });
                widgets::setting_row(ui, true, "Diesen Server übernehmen?", None, |ui| {
                    let width = widgets::button_width(ui, icon::CHECK, "Übernehmen")
                        + 6.0
                        + widgets::button_width(ui, icon::X, "Abbrechen");
                    widgets::row_group(ui, width, |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if widgets::button(ui, widgets::ButtonKind::Danger, icon::CHECK, "Übernehmen")
                            .on_hover_text("Ersetzt Schlüssel, Zertifikat und Serverdaten dieses Geräts")
                            .clicked()
                        {
                            self.import_passport(cmds);
                        }
                        if widgets::button(ui, widgets::ButtonKind::Neutral, icon::X, "Abbrechen")
                            .clicked()
                        {
                            self.import_preview = None;
                            self.passport_status = Some("Import abgebrochen.".into());
                        }
                    });
                });
            }
            let mut edited = false;
            widgets::card_note(ui, |ui| {
                edited = ui
                    .add(
                        egui::TextEdit::multiline(&mut self.passport_import)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text(icu_proto::passport::ARMOR_BEGIN)
                            .font(egui::TextStyle::Monospace),
                    )
                    .changed();
            });
            // A confirmation belongs to the text it was made for.
            if edited {
                self.import_preview = None;
            }
            if let Some(status) = &self.passport_status {
                widgets::card_note(ui, |ui| {
                    ui.label(
                        RichText::new(status)
                            .size(11.5)
                            .color(theme::tokens(ui.visuals()).dim_text),
                    );
                });
            }
        });
    }

    /// The armored passport for the identity on disk. Disk, not `AppState`:
    /// that one is cloned ten times a second and has no business carrying a
    /// private key.
    fn passport_text(&self) -> Result<String, String> {
        match Identity::load(&self.paths) {
            Ok(Some(id)) => Ok(id.to_passport()),
            Ok(None) => Err("Keine Identität gefunden.".into()),
            Err(e) => Err(format!("Fehlgeschlagen: {e}")),
        }
    }

    /// Reads the pasted text and shows what it would switch to.
    fn preview_passport(&mut self) {
        // `from_passport` also checks the key: a passport whose key does not
        // parse would brick the next connection. A registration bundle is
        // taken as well; the engine registers it under its placeholder name.
        match Identity::from_passport(&self.passport_import) {
            Ok(identity) => {
                let current = Identity::load(&self.paths).ok().flatten();
                let summary = config::ImportSummary::of(&identity, current.as_ref());
                self.import_preview = Some((Box::new(identity), summary));
                self.passport_status = None;
            }
            Err(e) => self.passport_status = Some(e),
        }
    }

    /// Takes the confirmed passport into use.
    fn import_passport(&mut self, cmds: &mut Vec<Command>) {
        if let Some((identity, _)) = self.import_preview.take() {
            cmds.push(Command::ReplaceIdentity(identity));
            self.passport_import.clear();
            self.passport_status = Some("Identität ersetzt, verbinde neu…".into());
        }
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
                let tok = theme::tokens(ui.visuals());
                // `crate::hotkey::start` reports "portal", "evdev (n devices)"
                // or "none"; the empty string means it was never started
                // (`--no-hotkeys`).
                let (color, text) = match self.hotkey_backend.as_str() {
                    "" => (tok.connecting, "abgeschaltet"),
                    "none" => (tok.error, "keiner"),
                    backend => (tok.connected, backend),
                };
                widgets::status_pill(ui, color, text)
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

        widgets::banner(
            ui,
            widgets::BannerKind::Warning,
            "evdev liest alle Tasten",
            "evdev-Namen wie KEY_RIGHTCTRL, BTN_SIDE oder code:97; Portal-Trigger wie \
             CTRL+SHIFT+F12. evdev braucht Gruppe input und liest technisch jeden \
             Tastendruck mit (Keylogger-Aspekt). Änderungen gelten ab dem nächsten Start.",
            None::<fn()>,
        );
    }

    #[cfg_attr(not(feature = "camera"), allow(unused_variables))]
    fn camera_tab(&mut self, ui: &mut Ui, state: &AppState) {
        widgets::group_card(ui, "Kamera", |ui| {
            widgets::setting_row(ui, false, "Beim Start einschalten", None, |ui| {
                widgets::toggle(ui, &mut self.draft.camera.enabled)
            });
            #[cfg(feature = "camera")]
            {
                if self.cameras_stale {
                    // Opens every `/dev/video*` in turn, so: once per visit
                    // to this tab, never per frame.
                    self.cameras_stale = false;
                    self.cameras = crate::camera::list();
                }
                widgets::setting_row(ui, true, "Gerät", None, |ui| {
                    camera_combo(ui, &mut self.draft.camera.device, &self.cameras)
                });
                ui.separator();
                widgets::card_note(ui, |ui| {
                    // Wide enough to judge framing, small enough that the
                    // card stays a settings row and not a video window.
                    let width = ui.available_width().min(240.0);
                    let live = self
                        .preview
                        .as_mut()
                        .is_some_and(|preview| preview.ui(ui, width));
                    if !live {
                        preview_placeholder(ui, width, state.camera_on);
                    }
                });
            }
        });

        widgets::group_card(ui, "Overlay", |ui| {
            widgets::setting_row(ui, false, "Anzeigen", None, |ui| {
                widgets::toggle(ui, &mut self.draft.overlay.enabled)
            });
            widgets::setting_row(ui, true, "Ecke", None, |ui| {
                widgets::row_group(ui, CORNER_PICKER_W, |ui| {
                    corner_picker(ui, &mut self.draft.overlay.corner)
                });
            });
            let edge = matches!(self.draft.overlay.corner, Corner::Top | Corner::Bottom);
            widgets::setting_row(ui, true, "Ausrichtung", None, |ui| {
                ui.add_enabled_ui(!edge, |ui| {
                    widgets::segmented(
                        ui,
                        &mut self.draft.overlay.expand,
                        &[
                            (Expand::Vertical, "vertikal"),
                            (Expand::Horizontal, "horizontal"),
                        ],
                    )
                })
                .inner
            });
            widgets::setting_row(ui, true, "Abstand", None, |ui| {
                const DRAG_W: f32 = 62.0;
                widgets::row_group(ui, DRAG_W * 2.0 + 6.0, |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (axis, prefix) in [(0, "x "), (1, "y ")] {
                        ui.add_sized(
                            [DRAG_W, 24.0],
                            egui::DragValue::new(&mut self.draft.overlay.offset[axis])
                                .prefix(prefix)
                                .range(0..=2000),
                        );
                    }
                });
            });
            widgets::setting_row(ui, true, "Kachelgröße", None, |ui| {
                widgets::segmented(
                    ui,
                    &mut self.draft.overlay.size,
                    &[
                        (TileSize::Small, "klein"),
                        (TileSize::Medium, "mittel"),
                        (TileSize::Large, "groß"),
                    ],
                )
            });
            widgets::setting_row(ui, true, "Stille Teilnehmer abdunkeln", None, |ui| {
                widgets::toggle(ui, &mut self.draft.overlay.dim_non_speaking)
            });
            widgets::setting_row(ui, true, "Rahmen um sprechende Person", None, |ui| {
                widgets::toggle(ui, &mut self.draft.overlay.speaking_frame)
            });
            widgets::setting_row(ui, true, "Rahmenfarbe", None, |ui| {
                ui.add_enabled_ui(self.draft.overlay.speaking_frame, |ui| {
                    ui.color_edit_button_srgb(&mut self.draft.overlay.frame_color)
                })
                .inner
            });
        });
    }

    fn general_tab(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        // A rename that happened on another device arrives as `UserRenamed`;
        // refill the field unless the user is editing it.
        let current = state.me.as_ref().map_or("", |m| m.name.as_str());
        if current != self.name_synced && self.name == self.name_synced {
            self.name = current.to_string();
            self.name_synced = current.to_string();
        } else if current != self.name_synced && !state.rename_pending {
            self.name_synced = current.to_string();
        }

        widgets::group_card(ui, "Identität", |ui| {
            let editable = state.me.is_some() && !state.rename_pending;
            widgets::setting_row(ui, false, "Anzeigename", None, |ui| {
                let width = FIELD_W + 6.0 + widgets::button_width(ui, icon::CHECK, "");
                widgets::row_group(ui, width, |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let field = ui
                        .add_enabled_ui(editable, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.name)
                                    .desired_width(FIELD_W)
                                    .hint_text("Name"),
                            )
                        })
                        .inner;
                    let entered =
                        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let changed = icu_proto::normalize_name(&self.name) != state.me.as_ref().map_or("", |m| &m.name);
                    let apply = ui
                        .add_enabled_ui(editable && changed, |ui| {
                            widgets::button(ui, widgets::ButtonKind::Primary, icon::CHECK, "")
                        })
                        .inner
                        .on_hover_text(if state.rename_pending {
                            "Wird beim Server angefragt…"
                        } else {
                            "Namen übernehmen"
                        });
                    if editable && (apply.clicked() || entered) && changed {
                        cmds.push(Command::RequestRename(icu_proto::normalize_name(&self.name)));
                    }
                });
            });
            widgets::card_note(ui, |ui| {
                ui.label(
                    RichText::new(
                        "Der Name steht im Zertifikat. Eine Änderung stellt ein neues aus; \
                         das alte bleibt bis zum Ablauf gültig.",
                    )
                    .size(11.5)
                    .color(theme::tokens(ui.visuals()).dim_text),
                );
            });
            if let Some(err) = &state.name_error {
                widgets::card_note(ui, |ui| {
                    ui.colored_label(theme::tokens(ui.visuals()).error, err);
                });
            }
            widgets::setting_row(ui, true, "Server", None, |ui| {
                widgets::chip(ui, &state.server, 0.0)
            });
            widgets::setting_row(ui, true, "user_id", None, |ui| {
                let id = state.me.as_ref().map(|m| m.user_id.to_string());
                widgets::chip(ui, id.as_deref().unwrap_or("—"), 0.0)
            });
        });

        self.passport_card(ui, cmds);

        if state.is_admin {
            widgets::group_card(ui, "Einladung", |ui| {
                widgets::setting_row(ui, false, "Gültig für", None, |ui| {
                    let current = INVITE_TTLS
                        .iter()
                        .find(|(s, _)| *s == self.invite_ttl_s)
                        .map_or("", |(_, l)| l);
                    egui::ComboBox::from_id_salt("invite_ttl")
                        .selected_text(current)
                        .width(FIELD_W)
                        .show_ui(ui, |ui| {
                            for (secs, label) in INVITE_TTLS {
                                ui.selectable_value(&mut self.invite_ttl_s, secs, label);
                            }
                        });
                });
                widgets::setting_row(ui, true, "Wer soll dazukommen?", None, |ui| {
                    let width = FIELD_W + 6.0 + widgets::button_width(ui, icon::TICKET, "Einladen");
                    widgets::row_group(ui, width, |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.add(
                            egui::TextEdit::singleline(&mut self.invite_name)
                                .desired_width(FIELD_W)
                                .hint_text("Name (optional)"),
                        );
                        // The name is only a placeholder until the invited
                        // person picks their own; empty means a generated one.
                        let valid = self.invite_name.trim().is_empty()
                            || icu_proto::validate_name(&icu_proto::normalize_name(&self.invite_name)).is_ok();
                        if ui
                            .add_enabled_ui(valid, |ui| {
                                widgets::button(
                                    ui,
                                    widgets::ButtonKind::Primary,
                                    icon::TICKET,
                                    "Einladen",
                                )
                            })
                            .inner
                            .clicked()
                        {
                            cmds.push(Command::CreateInvite {
                                name: icu_proto::normalize_name(&self.invite_name),
                                ttl_s: self.invite_ttl_s,
                            });
                            self.invite_status = None;
                        }
                    });
                });
                if let Some(err) = &state.invite_error {
                    widgets::card_note(ui, |ui| {
                        ui.colored_label(theme::tokens(ui.visuals()).error, err);
                    });
                }
                if let Some((bundle, expires)) = &state.last_invite {
                    ui.separator();
                    widgets::card_note(ui, |ui| {
                        let tok = theme::tokens(ui.visuals());
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Einmal einlösbar, gültig bis {expires}"))
                                    .size(11.5)
                                    .color(tok.dim_text),
                            );
                            widgets::rtl(ui, 30.0, |ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if widgets::button(
                                    ui,
                                    widgets::ButtonKind::Neutral,
                                    icon::COPY,
                                    "Kopieren",
                                )
                                .clicked()
                                {
                                    ui.ctx().copy_text(bundle.clone());
                                }
                                if widgets::button(
                                    ui,
                                    widgets::ButtonKind::Neutral,
                                    icon::EXPORT,
                                    "Speichern",
                                )
                                .on_hover_text("Als Datei neben die Konfiguration schreiben")
                                .clicked()
                                {
                                    self.invite_status =
                                        Some(match config::save_invite(&self.paths, bundle) {
                                            Ok(path) => format!("Gespeichert: {}", path.display()),
                                            Err(e) => format!("Fehlgeschlagen: {e}"),
                                        });
                                }
                            });
                        });
                        if let Some(status) = &self.invite_status {
                            ui.label(RichText::new(status).small().color(tok.dim_text));
                        }
                        ui.add_space(4.0);
                        let mut copy = bundle.clone();
                        ui.add(
                            egui::TextEdit::multiline(&mut copy)
                                .desired_rows(4)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace),
                        );
                    });
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
            widgets::card_note(ui, |ui| {
                widgets::ltr(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    for (value, label) in
                        [("system", "System"), ("light", "Hell"), ("dark", "Dunkel")]
                    {
                        theme_swatch(ui, &mut self.draft.ui.theme, value, label);
                    }
                });
            });
        });
    }

    /// Admin-only "Nutzer" tab: promote/demote, disable/enable, revoke
    /// (`ControlMsg::UserListRequest`/`AdminSetRole`/`AdminSetDisabled`/
    /// `AdminRevokeUser`, `PROTOCOL.md` §2.2).
    fn users_tab(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        widgets::group_card(ui, "Nutzer", |ui| {
            widgets::setting_row(ui, false, "Registrierte Nutzer", None, |ui| {
                if widgets::button(
                    ui,
                    widgets::ButtonKind::Neutral,
                    icon::ARROWS_CLOCKWISE,
                    "Aktualisieren",
                )
                .clicked()
                {
                    cmds.push(Command::RequestUserList);
                }
            });
            if let Some(err) = &state.user_action_error {
                widgets::card_note(ui, |ui| {
                    ui.colored_label(theme::tokens(ui.visuals()).error, err);
                });
            }
            let me = state.me.as_ref().map(|m| m.user_id);
            let mut users: Vec<_> = state.user_list.iter().collect();
            users.sort_by_key(|u| u.name.to_lowercase());
            for (i, user) in users.into_iter().enumerate() {
                self.user_row(ui, i > 0, user, me, cmds);
            }
            if state.user_list.is_empty() {
                widgets::card_note(ui, |ui| {
                    ui.label(
                        RichText::new("Noch keine Liste geladen.")
                            .color(theme::tokens(ui.visuals()).dim_text),
                    );
                });
            }
        });
    }

    fn user_row(
        &mut self,
        ui: &mut Ui,
        divider: bool,
        user: &icu_proto::UserSummary,
        me: Option<UserId>,
        cmds: &mut Vec<Command>,
    ) {
        let is_self = Some(user.user_id) == me;
        let status = if user.revoked {
            "widerrufen"
        } else if user.disabled {
            "deaktiviert"
        } else if user.pending {
            "eingeladen"
        } else {
            "aktiv"
        };
        let help = format!("{status}{}", if user.is_admin { " · Admin" } else { "" });
        let confirming = self
            .confirm_revoke
            .get(&user.user_id)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        let revoke_label = if confirming { "Wirklich?" } else { "" };
        widgets::setting_row(ui, divider, &user.name, Some(&help), |ui| {
            let width = widgets::button_width(ui, icon::SHIELD_STAR, "")
                + widgets::button_width(ui, icon::PROHIBIT, "")
                + widgets::button_width(ui, icon::TRASH, revoke_label)
                + 12.0;
            widgets::row_group(ui, width, |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let action = |ui: &mut Ui, kind, glyph, label: &str, hover: &str| {
                    ui.add_enabled_ui(!is_self, |ui| widgets::button(ui, kind, glyph, label))
                        .inner
                        .on_hover_text(hover.to_string())
                        .clicked()
                };
                if action(
                    ui,
                    widgets::ButtonKind::Neutral,
                    icon::SHIELD_STAR,
                    "",
                    if user.is_admin {
                        "Admin-Rechte entziehen"
                    } else {
                        "Zum Admin machen"
                    },
                ) {
                    cmds.push(Command::SetUserAdmin {
                        user_id: user.user_id,
                        is_admin: !user.is_admin,
                    });
                }
                if action(
                    ui,
                    widgets::ButtonKind::Neutral,
                    icon::PROHIBIT,
                    "",
                    if user.disabled {
                        "Wieder aktivieren"
                    } else {
                        "Deaktivieren — darf sich nicht mehr verbinden"
                    },
                ) {
                    cmds.push(Command::SetUserDisabled {
                        user_id: user.user_id,
                        disabled: !user.disabled,
                    });
                }
                // Two clicks, because this cannot be undone: the first arms
                // the button for three seconds, the second sends it.
                if action(
                    ui,
                    widgets::ButtonKind::Danger,
                    icon::TRASH,
                    revoke_label,
                    "Zugang endgültig widerrufen",
                ) {
                    if confirming {
                        self.confirm_revoke.remove(&user.user_id);
                        cmds.push(Command::RevokeUser {
                            user_id: user.user_id,
                        });
                    } else {
                        self.confirm_revoke.insert(user.user_id, Instant::now());
                    }
                }
            })
        });
    }

    fn about_tab(&mut self, ui: &mut Ui, state: &AppState) {
        widgets::group_card(ui, "Info", |ui| {
            widgets::setting_row(ui, false, "Version", None, |ui| {
                widgets::chip(ui, env!("CARGO_PKG_VERSION"), 0.0)
            });
            widgets::setting_row(ui, true, "Audio-Backend", None, |ui| {
                widgets::chip(ui, state.backend, 0.0)
            });
            widgets::card_note(ui, |ui| {
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
        });

        self.refresh_diagnose();

        widgets::group_card(ui, "Diagnose", |ui| {
            widgets::setting_row(ui, false, "RTT", None, |ui| {
                let rtt = state.rtt_ms.map(|r| format!("{r:.0} ms"));
                widgets::chip(ui, rtt.as_deref().unwrap_or("n/a"), 0.0)
            });
            widgets::setting_row(ui, true, "Jitter p50/p95", None, |ui| {
                widgets::chip(ui, &self.diagnose.jitter, 0.0)
            });
            widgets::setting_row(ui, true, "Capture→Send p50/max", None, |ui| {
                widgets::chip(ui, &self.diagnose.capture_send, 0.0)
            });
            widgets::setting_row(ui, true, "DSP-Block p50/max", None, |ui| {
                widgets::chip(ui, &self.diagnose.dsp_block, 0.0)
            });
            widgets::setting_row(ui, true, "Paketverlust", None, |ui| {
                widgets::chip(ui, &self.diagnose.packet_loss, 0.0)
            });
            widgets::setting_row(ui, true, "FEC/PLC", None, |ui| {
                widgets::chip(ui, &self.diagnose.fec_plc, 0.0)
            });
            widgets::setting_row(ui, true, "Referenz-Underruns", None, |ui| {
                widgets::chip(ui, &self.diagnose.ref_underruns, 0.0)
            });
            widgets::setting_row(ui, true, "Reconnects", None, |ui| {
                widgets::chip(ui, &self.diagnose.reconnects, 0.0)
            });

            ui.separator();
            widgets::card_note(ui, |ui| {
                widgets::ltr(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if widgets::button(ui, widgets::ButtonKind::Neutral, icon::COPY, "Bericht")
                        .on_hover_text("Den vollen Statistikbericht in die Zwischenablage")
                        .clicked()
                    {
                        ui.ctx().copy_text(self.stats.report());
                    }
                    if widgets::button(
                        ui,
                        widgets::ButtonKind::Neutral,
                        icon::EXPORT,
                        "Histogramme",
                    )
                    .on_hover_text("Als Datei neben die Konfiguration schreiben")
                    .clicked()
                    {
                        let path = self.paths.config_dir.join("histograms.txt");
                        self.export_status =
                            Some(match std::fs::write(&path, self.stats.report()) {
                                Ok(()) => format!("Geschrieben: {}", path.display()),
                                Err(e) => format!("Fehlgeschlagen: {e}"),
                            });
                    }
                    if widgets::button(
                        ui,
                        if self.debug_window_open {
                            widgets::ButtonKind::Primary
                        } else {
                            widgets::ButtonKind::Neutral
                        },
                        icon::BUG,
                        "Debug-Fenster",
                    )
                    .clicked()
                    {
                        self.debug_window_open = !self.debug_window_open;
                    }
                });
                if let Some(status) = &self.export_status {
                    let tok = theme::tokens(ui.visuals());
                    ui.label(RichText::new(status).small().color(tok.dim_text));
                }
            });
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

/// The camera picker, fed by `crate::camera::list()`. An index the machine
/// no longer has (a webcam that was unplugged) stays selected and shows its
/// raw value, so a saved setting is never silently dropped.
#[cfg(feature = "camera")]
fn camera_combo(ui: &mut Ui, selected: &mut Option<String>, cameras: &[(u32, String)]) {
    let current = match selected.as_deref() {
        None => "Erste gefundene".to_string(),
        Some("synthetic") => "Testbild".to_string(),
        Some(id) => cameras
            .iter()
            .find(|(index, _)| index.to_string() == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| format!("{id} (nicht vorhanden)")),
    };
    egui::ComboBox::from_id_salt("camera-device")
        .selected_text(current)
        .width(200.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(selected.is_none(), "Erste gefundene")
                .clicked()
            {
                *selected = None;
            }
            for (index, name) in cameras {
                let id = index.to_string();
                if ui
                    .selectable_label(selected.as_deref() == Some(id.as_str()), name)
                    .clicked()
                {
                    *selected = Some(id);
                }
            }
        });
}

/// Stands in for the live picture while there is none: the camera is off,
/// or still waking up.
#[cfg(feature = "camera")]
fn preview_placeholder(ui: &mut Ui, width: f32, camera_on: bool) {
    let tok = theme::tokens(ui.visuals());
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(width, width * 3.0 / 4.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(theme::R_CHIP),
        ui.visuals().extreme_bg_color,
    );
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(theme::R_CHIP),
        egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center() - egui::vec2(0.0, 10.0),
        egui::Align2::CENTER_CENTER,
        icon::VIDEO_CAMERA,
        egui::FontId::new(24.0, egui::FontFamily::Proportional),
        tok.dim_text,
    );
    painter.text(
        rect.center() + egui::vec2(0.0, 16.0),
        egui::Align2::CENTER_CENTER,
        if camera_on {
            "Kamera startet…"
        } else {
            "Video ist aus"
        },
        egui::FontId::new(11.5, egui::FontFamily::Proportional),
        tok.dim_text,
    );
}

/// A row of position buttons — the 4 corners plus centered top/bottom edges
/// — each drawing a small preview of where the overlay tile stack would sit.
fn corner_picker(ui: &mut Ui, corner: &mut Corner) {
    let tok = theme::tokens(ui.visuals());
    let swatch = egui::vec2(CORNER_SWATCH, CORNER_SWATCH);
    let mut button =
        |ui: &mut Ui, value: Corner, tile_rect_fn: &dyn Fn(egui::Rect) -> egui::Rect| {
            let selected = *corner == value;
            let (rect, resp) = ui.allocate_exact_size(swatch, egui::Sense::click());
            if ui.is_rect_visible(rect) {
                let painter = ui.painter();
                painter.rect_filled(
                    rect,
                    egui::CornerRadius::same(theme::R_CTRL),
                    ui.visuals().extreme_bg_color,
                );
                painter.rect_stroke(
                    rect,
                    egui::CornerRadius::same(theme::R_CTRL),
                    egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
                    egui::StrokeKind::Inside,
                );
                let tile_color = if selected {
                    tok.accent
                } else {
                    ui.visuals().widgets.inactive.bg_stroke.color
                };
                painter.rect_filled(tile_rect_fn(rect), 2.0, tile_color);
            }
            if resp.clicked() {
                *corner = value;
            }
        };
    widgets::ltr(ui, |ui| {
        ui.spacing_mut().item_spacing.x = CORNER_GAP;
        for (value, dx, dy) in [
            (Corner::TopLeft, 0.0, 0.0),
            (Corner::TopRight, 1.0, 0.0),
            (Corner::BottomLeft, 0.0, 1.0),
            (Corner::BottomRight, 1.0, 1.0),
        ] {
            let tile = egui::vec2(22.0, 14.0);
            button(ui, value, &|rect| {
                let pos = egui::pos2(
                    rect.left() + 4.0 + dx * (rect.width() - tile.x - 8.0),
                    rect.top() + 4.0 + dy * (rect.height() - tile.y - 8.0),
                );
                egui::Rect::from_min_size(pos, tile)
            });
        }
        ui.add_space(CORNER_SPLIT);
        for (value, dy) in [(Corner::Top, 0.0), (Corner::Bottom, 1.0)] {
            let tile = egui::vec2(30.0, 8.0);
            button(ui, value, &|rect| {
                let pos = egui::pos2(
                    rect.center().x - tile.x / 2.0,
                    rect.top() + 4.0 + dy * (rect.height() - tile.y - 8.0),
                );
                egui::Rect::from_min_size(pos, tile)
            });
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
            painter.rect_filled(rect, egui::CornerRadius::same(theme::R_CTRL), panel);
            let border = if selected {
                egui::Stroke::new(2.0, accent)
            } else {
                egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color)
            };
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(theme::R_CTRL),
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
                // A full-height, left-aligned label column: it sets the row
                // height before the chips arrive, so they center on it too.
                let (col, _) = ui.allocate_exact_size(egui::vec2(92.0, 30.0), egui::Sense::hover());
                ui.painter().text(
                    col.left_center(),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::new(13.5, egui::FontFamily::Proportional),
                    ui.visuals().text_color(),
                );
                // An unset binding shows one dash, not two empty boxes.
                let key = if binding.key.is_empty() {
                    "—"
                } else {
                    &binding.key
                };
                widgets::chip(ui, key, 0.0).on_hover_text("evdev-Taste");
                if let Some(trigger) = binding.trigger.as_deref() {
                    widgets::chip(ui, trigger, 0.0).on_hover_text("Portal-Trigger");
                }
                widgets::rtl(ui, 30.0, |ui| {
                    let is_open = expanded.contains(label);
                    if widgets::icon_button(ui, icon::PENCIL_SIMPLE, 30.0, is_open)
                        .on_hover_text(if is_open { "Fertig" } else { "Ändern" })
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
                ui.add_space(6.0);
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
