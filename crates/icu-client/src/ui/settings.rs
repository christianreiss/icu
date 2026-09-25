//! Settings panel content: an icon rail plus tab content, drawn into its own
//! `egui` viewport window (`main_window.rs`, mirrors the Debug window).
//! Every change is applied at once through `Command::UpdateSettings`.

use std::{
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
        VoiceStrength,
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
    (icu_proto::limits::INVITE_TTL_S, "1 Tag"),
    (3 * 24 * 60 * 60, "3 Tage"),
    (icu_proto::limits::MAX_INVITE_TTL_S, "7 Tage"),
];
/// Width of the settings sidebar.
const NAV_W: f32 = 184.0;
/// Content column cap: cards wider than this only put more empty space
/// between a label and its control.
const CONTENT_MAX_W: f32 = 600.0;
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
    /// The invite last seen in `AppState`, to notice a new one.
    seen_invite: Option<String>,
    /// Active tab.
    tab: Tab,
    /// Which hotkey rows currently show their editable fields.
    expanded_bindings: std::collections::HashSet<&'static str>,
    stats: Arc<DebugStats>,
    paths: Paths,
    diagnose: DiagnoseCache,
    export_status: Option<String>,
    /// Revoke is destructive: "Entfernen…" in a member's menu swaps that
    /// row's right side for an inline confirmation (no modal per §10); only
    /// its "Entfernen" sends `Command::RevokeUser`.
    confirm_revoke: Option<UserId>,
    /// The invite form above the member list is open.
    invite_open: bool,
    /// An invite was minted while the form was open; show its result there.
    fresh_invite: bool,
    /// The removed-accounts section is expanded.
    show_removed: bool,
    /// When the member list was last asked for. The Nutzer tab re-asks on
    /// its own: a request sent while the connection is down is dropped, and
    /// online status goes stale while the tab stays open.
    users_requested: Option<Instant>,
    /// Logging out is destructive too: same two-click window as revoke.
    confirm_logout: Option<Instant>,
    /// Set once the user confirmed logging out; handed back to `App` as
    /// [`SettingsOutput::logout`], which closes the window and deletes the
    /// identity after the engine is down (`main_window::run`).
    logout_requested: bool,
    /// Whether the debug window is open — `App` owns the window and says so
    /// on every call of [`SettingsView::ui`]; the About tab's button flips
    /// it, and the frame's [`SettingsOutput`] carries the answer back.
    debug_window_open: bool,
    /// The draft as last handed out in `Command::UpdateSettings`, so a
    /// change is noticed by comparison instead of by cloning the whole
    /// draft on every frame.
    applied: Settings,
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

/// What one frame of the settings window asks the main window to do.
#[derive(Default)]
pub struct SettingsOutput {
    /// Commands for the engine.
    pub cmds: Vec<Command>,
    /// Whether the debug window should be open after this frame.
    pub debug_window_open: bool,
    /// The user confirmed logging out.
    pub logout: bool,
}

impl SettingsView {
    /// From the engine's current settings.
    pub fn new(state: &AppState, stats: Arc<DebugStats>, paths: Paths) -> SettingsView {
        let name = state
            .me
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_default();
        SettingsView {
            draft: state.settings.clone(),
            applied: state.settings.clone(),
            name: name.clone(),
            name_synced: name,
            passport_import: String::new(),
            passport_status: None,
            import_preview: None,
            invite_name: String::new(),
            invite_ttl_s: icu_proto::limits::INVITE_TTL_S,
            invite_status: None,
            seen_invite: None,
            tab: Tab::General,
            expanded_bindings: std::collections::HashSet::new(),
            stats,
            paths,
            diagnose: DiagnoseCache::default(),
            export_status: None,
            confirm_revoke: None,
            invite_open: false,
            fresh_invite: false,
            show_removed: false,
            users_requested: None,
            confirm_logout: None,
            logout_requested: false,
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

    /// The header's theme button flipped the theme outside this window:
    /// the draft follows, and nothing is re-sent for it.
    pub fn set_theme(&mut self, theme: &str) {
        self.draft.ui.theme = theme.to_string();
        self.applied.ui.theme = theme.to_string();
    }

    /// Draws the panel content. `debug_window_open` is what `App` knows;
    /// the answer comes back in the output.
    pub fn ui(&mut self, ui: &mut Ui, state: &AppState, debug_window_open: bool) -> SettingsOutput {
        let mut cmds = Vec::new();
        self.debug_window_open = debug_window_open;

        // A live demote can orphan the tab: it is only in the rail while
        // `state.is_admin`, so bounce back to Allgemein instead of rendering
        // a tab that is no longer selectable.
        if self.tab == Tab::Users && !state.is_admin {
            self.tab = Tab::General;
        }
        let tab_before = self.tab;

        let mut tabs = vec![
            (Tab::General, icon::GEAR_FINE, "Allgemein"),
            (Tab::Audio, icon::MICROPHONE, "Audio"),
            (Tab::Hotkeys, icon::COMMAND, "Tastenkürzel"),
            (Tab::Camera, icon::VIDEO_CAMERA, "Kamera"),
            (Tab::About, icon::QUESTION, "Über ICU"),
        ];
        if state.is_admin {
            tabs.insert(4, (Tab::Users, icon::USERS_THREE, "Nutzer"));
        }

        egui::Panel::left("settings_nav")
            .resizable(false)
            .exact_size(NAV_W)
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .inner_margin(egui::Margin::symmetric(10, 12)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (tab, glyph, label) in tabs {
                    if widgets::nav_item(ui, glyph, label, self.tab == tab) {
                        self.tab = tab;
                    }
                }
                // The expert switch sits at the foot of the sidebar: it
                // changes every page, so it belongs to none of them.
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    let tok = theme::tokens(ui.visuals());
                    ui.label(
                        RichText::new("Zeigt technische Einstellungen und Diagnose.")
                            .size(11.0)
                            .color(tok.dim_text),
                    );
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Erweitert").size(13.0));
                        widgets::rtl(ui, 24.0, |ui| {
                            widgets::toggle(ui, &mut self.draft.ui.show_advanced)
                                .on_hover_text("Erweiterte Einstellungen anzeigen")
                        });
                    });
                    ui.add_space(4.0);
                    ui.separator();
                });
            });

        if self.tab != tab_before {
            match self.tab {
                Tab::Users => self.users_requested = None,
                #[cfg(feature = "camera")]
                Tab::Camera => self.cameras_stale = true,
                _ => {}
            }
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 22,
                        right: 22,
                        top: 18,
                        bottom: 24,
                    })
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width().min(CONTENT_MAX_W));
                        match self.tab {
                            Tab::Audio => self.audio_tab(ui, state),
                            Tab::Hotkeys => self.hotkeys_tab(ui, state),
                            Tab::Camera => self.camera_tab(ui, state),
                            Tab::General => self.general_tab(ui, state, &mut cmds),
                            Tab::Users => self.users_tab(ui, state, &mut cmds),
                            Tab::About => self.about_tab(ui, state),
                        }
                    });
            });

        if self.draft != self.applied {
            self.applied.clone_from(&self.draft);
            cmds.push(Command::UpdateSettings(Box::new(self.draft.clone())));
        }
        SettingsOutput {
            cmds,
            debug_window_open: self.debug_window_open,
            logout: std::mem::take(&mut self.logout_requested),
        }
    }

    fn audio_tab(&mut self, ui: &mut Ui, state: &AppState) {
        let adv = self.draft.ui.show_advanced;
        widgets::page_header(
            ui,
            "Audio",
            "Mikrofon, Lautsprecher und wie deine Stimme bei den anderen ankommt.",
            |_| {},
        );
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
            device_combo(ui, false, "Mikrofon", &mut self.draft.audio.input_device, &inputs);
            device_combo(
                ui,
                true,
                "Lautsprecher",
                &mut self.draft.audio.output_device,
                &outputs,
            );
            widgets::slider_row(
                ui,
                true,
                "Lautstärke",
                &mut self.draft.audio.output_volume,
                0.0..=200.0,
                " %",
            );
        });

        widgets::group_card(ui, "Sprechen", |ui| {
            widgets::setting_row(
                ui,
                false,
                "Senden",
                Some(match self.draft.audio.mode {
                    ModeSetting::Ptt => "Du bist nur zu hören, solange du die Taste hältst.",
                    ModeSetting::Vad => "Du bist automatisch zu hören, sobald du sprichst.",
                }),
                |ui| {
                    widgets::segmented(
                        ui,
                        &mut self.draft.audio.mode,
                        &[
                            (ModeSetting::Ptt, "Taste halten"),
                            (ModeSetting::Vad, "Automatisch"),
                        ],
                    )
                },
            );
            if self.draft.audio.mode == ModeSetting::Vad {
                ui.add_enabled_ui(self.draft.audio.denoise, |ui| {
                    widgets::setting_row(
                        ui,
                        true,
                        "Schwelle automatisch",
                        Some("Passt sich laufend an deine Umgebung an. Braucht die Rauschunterdrückung."),
                        |ui| widgets::toggle(ui, &mut self.draft.audio.vad_auto),
                    )
                });
                if !self.draft.audio.vad_auto {
                    widgets::slider_row(
                        ui,
                        true,
                        "Schwelle",
                        &mut self.draft.audio.vad_threshold,
                        0.0..=1.0,
                        "",
                    );
                }
            }
            if adv {
                widgets::advanced_caption(ui);
                let mut first = true;
                let mut divider = || !std::mem::take(&mut first);
                if self.draft.audio.vad_auto {
                    widgets::setting_row(ui, divider(), "Aktuelle Schwelle", None, |ui| {
                        widgets::chip(ui, &format!("{:.2}", state.vad_threshold), 0.0)
                    });
                }
                widgets::slider_row(
                    ui,
                    divider(),
                    "Verstärkung",
                    &mut self.draft.audio.gain_db,
                    -20.0..=20.0,
                    " dB",
                );
                let mut release = self.draft.audio.release_ms as f32;
                if widgets::slider_row(ui, divider(), "PTT-Nachlauf", &mut release, 0.0..=1000.0, " ms")
                    .changed()
                {
                    self.draft.audio.release_ms = release as u32;
                }
                let mut hangover = self.draft.audio.hangover_ms as f32;
                if widgets::slider_row(ui, divider(), "VAD-Nachlauf", &mut hangover, 0.0..=3000.0, " ms")
                    .changed()
                {
                    self.draft.audio.hangover_ms = hangover as u32;
                }
            }
        });

        widgets::group_card(ui, "Klang", |ui| {
            widgets::setting_row(
                ui,
                false,
                "Rauschunterdrückung",
                Some("Filtert Tastatur, Lüfter und Hintergrundgeräusche heraus."),
                |ui| widgets::toggle(ui, &mut self.draft.audio.denoise),
            );
            widgets::setting_row(
                ui,
                true,
                "Stimmoptimierung",
                Some("Gleicht leise und laute Stellen aus, damit du gleichmäßig klingst."),
                |ui| widgets::toggle(ui, &mut self.draft.audio.voice_opt),
            );
            if self.draft.audio.voice_opt {
                widgets::setting_row(ui, true, "Stärke", None, |ui| {
                    widgets::segmented(
                        ui,
                        &mut self.draft.audio.voice_strength,
                        &[
                            (VoiceStrength::Light, "Leicht"),
                            (VoiceStrength::Medium, "Mittel"),
                            (VoiceStrength::Strong, "Stark"),
                        ],
                    )
                });
            }
            ui.add_enabled_ui(state.aec_available, |ui| {
                widgets::setting_row(
                    ui,
                    true,
                    "Echo-Unterdrückung",
                    Some(if state.aec_available {
                        "Verhindert, dass die anderen sich selbst hören, wenn du ohne Kopfhörer sprichst."
                    } else {
                        "Auf dieser Installation nicht verfügbar."
                    }),
                    |ui| widgets::toggle(ui, &mut self.draft.audio.aec),
                )
            });
            let tok = theme::tokens(ui.visuals());
            if state.aec_available && self.draft.audio.aec {
                if !state.aec_reference {
                    widgets::card_note(ui, |ui| {
                        ui.colored_label(
                            tok.connecting,
                            RichText::new(
                                "Gerade wirkungslos: das Audiosystem liefert nicht, was du hörst.",
                            )
                            .size(11.5),
                        );
                    });
                } else if state.aec_suspect {
                    widgets::card_note(ui, |ui| {
                        ui.colored_label(
                            tok.connecting,
                            RichText::new(
                                "Echo erkannt, wird aber nicht entfernt — Lautsprecher-Auswahl prüfen.",
                            )
                            .size(11.5),
                        );
                    });
                }
            }
            if adv {
                widgets::advanced_caption(ui);
                widgets::setting_row(
                    ui,
                    false,
                    "Opus-Frame",
                    Some("10 ms senkt die Latenz, kostet aber etwas Bandbreite."),
                    |ui| {
                        widgets::segmented(
                            ui,
                            &mut self.draft.audio.frame_ms,
                            &[(20u8, "20 ms"), (10u8, "10 ms")],
                        )
                    },
                );
                if let (Some(erl), Some(erle)) = (state.aec_stats.erl_db, state.aec_stats.erle_db) {
                    widgets::setting_row(ui, true, "Echo-Messwerte", None, |ui| {
                        let delay = state
                            .aec_stats
                            .delay_ms
                            .map_or(String::new(), |d| format!(" · {d} ms"));
                        widgets::chip(ui, &format!("ERL {erl:.0} dB · ERLE {erle:.0} dB{delay}"), 0.0)
                    });
                }
            }
        });

        widgets::group_card(ui, "Hinweistöne", |ui| {
            widgets::setting_row(ui, false, "Jemand kommt oder geht", None, |ui| {
                widgets::toggle(ui, &mut self.draft.audio.cue_join)
            });
            widgets::setting_row(ui, true, "Senden beginnt oder endet", None, |ui| {
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
        widgets::group_card(ui, "Auf ein anderes Gerät mitnehmen", |ui| {
            widgets::setting_row(ui, false, "Passport", None, |ui| {
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
                                    Err(e) => crate::problem::storage("Der Passport", &e).text,
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

            if !self.draft.ui.show_advanced {
                self.passport_status_note(ui);
                return;
            }
            widgets::advanced_caption(ui);
            widgets::setting_row(ui, false, "Passport einspielen", None, |ui| {
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
            self.passport_status_note(ui);
        });
    }

    /// The outcome of the last passport copy, save or import, if any.
    fn passport_status_note(&self, ui: &mut Ui) {
        if let Some(status) = &self.passport_status {
            widgets::help_note(ui, status);
        }
    }

    /// Deletes this device's identity and goes back to the invite window.
    fn logout_card(&mut self, ui: &mut Ui) {
        let confirming = self
            .confirm_logout
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        if confirming {
            // Let the armed button fall back on its own after 3 s.
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        widgets::group_card(ui, "Abmelden", |ui| {
            widgets::setting_row(ui, false, "Von diesem Gerät abmelden", None, |ui| {
                let label = if confirming { "Wirklich?" } else { "Abmelden" };
                let width = widgets::button_width(ui, icon::SIGN_OUT, label);
                widgets::row_group(ui, width, |ui| {
                    if widgets::button(ui, widgets::ButtonKind::Danger, icon::SIGN_OUT, label)
                        .on_hover_text("Löscht Schlüssel und Zertifikat auf diesem Gerät")
                        .clicked()
                    {
                        if confirming {
                            self.confirm_logout = None;
                            self.logout_requested = true;
                        } else {
                            self.confirm_logout = Some(Instant::now());
                        }
                    }
                });
            });
            widgets::help_note(
                ui,
                "Löscht Schlüssel und Zertifikat auf diesem Gerät. Zurück nur mit deinem \
                 Passport oder einer neuen Einladung — sichere ihn vorher oben.",
            );
        });
    }

    /// The armored passport for the identity on disk. Disk, not `AppState`:
    /// that one is cloned ten times a second and has no business carrying a
    /// private key.
    fn passport_text(&self) -> Result<String, String> {
        match Identity::load(&self.paths) {
            Ok(Some(id)) => Ok(id.to_passport()),
            Ok(None) => Err("Keine Identität gefunden.".into()),
            Err(e) => {
                tracing::warn!(%e, "identity unreadable");
                Err("Die gespeicherten Zugangsdaten lassen sich nicht lesen.".into())
            }
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

    fn hotkeys_tab(&mut self, ui: &mut Ui, state: &AppState) {
        let adv = self.draft.ui.show_advanced;
        widgets::page_header(
            ui,
            "Tastenkürzel",
            "Sprechen und Stummschalten per Taste – auch wenn ICU im Hintergrund läuft.",
            |_| {},
        );
        // `crate::hotkey::start` reports "portal", "evdev (n devices)" or
        // "none"; the empty string means it was never started
        // (`--no-hotkeys`).
        let backend = state.hotkey_backend.as_str();
        let banner = match backend {
            "" => Some((
                widgets::BannerKind::Warning,
                "Tastenkürzel sind abgeschaltet",
                "ICU wurde ohne Tastenkürzel gestartet. Bedienen geht weiter über das Hauptfenster.",
            )),
            "none" => Some((
                widgets::BannerKind::Error,
                "Tastenkürzel funktionieren gerade nicht",
                "Das System hat keinen Weg angeboten, Tasten im Hintergrund zu lesen. \
                 Unter „Erweitert“ lässt sich ein anderer Weg wählen.",
            )),
            _ => None,
        };
        if let Some((kind, title, body)) = banner {
            widgets::banner(ui, kind, title, body, None::<fn()>);
            ui.add_space(14.0);
        }

        widgets::group_card(ui, "Belegung", |ui| {
            binding_row(
                ui,
                false,
                &mut self.expanded_bindings,
                "Push-to-Talk",
                &mut self.draft.hotkeys.ptt,
            );
            binding_row(
                ui,
                true,
                &mut self.expanded_bindings,
                "Stummschalten",
                &mut self.draft.hotkeys.mute,
            );
            binding_row(
                ui,
                true,
                &mut self.expanded_bindings,
                "Ton aus",
                &mut self.draft.hotkeys.deafen,
            );
            binding_row(
                ui,
                true,
                &mut self.expanded_bindings,
                "Modus wechseln",
                &mut self.draft.hotkeys.toggle_mode,
            );
            widgets::help_note(ui, "Änderungen gelten ab dem nächsten Start von ICU.");
        });

        if adv {
            widgets::group_card(ui, "Tasten lesen über", |ui| {
                widgets::setting_row(ui, false, "Methode", None, |ui| {
                    widgets::segmented(
                        ui,
                        &mut self.draft.hotkeys.backend,
                        &[
                            (HotkeyBackend::Auto, "Automatisch"),
                            (HotkeyBackend::Portal, "Portal"),
                            (HotkeyBackend::Evdev, "evdev"),
                        ],
                    )
                });
                widgets::setting_row(ui, true, "Aktiv", None, |ui| {
                    let tok = theme::tokens(ui.visuals());
                    let (color, text) = match backend {
                        "" => (tok.connecting, "abgeschaltet"),
                        "none" => (tok.error, "keiner"),
                        backend => (tok.connected, backend),
                    };
                    widgets::status_pill(ui, color, text)
                });
                widgets::help_note(
                    ui,
                    "Tastennamen: evdev wie KEY_RIGHTCTRL, BTN_SIDE oder code:97; \
                     Portal-Trigger wie CTRL+SHIFT+F12.",
                );
            });
        }
        if adv || self.draft.hotkeys.backend == HotkeyBackend::Evdev {
            widgets::banner(
                ui,
                widgets::BannerKind::Warning,
                "evdev liest alle Tasten",
                "evdev braucht die Gruppe input und liest technisch jeden Tastendruck mit, \
                 nicht nur die hier belegten.",
                None::<fn()>,
            );
        }
    }

    #[cfg_attr(not(feature = "camera"), allow(unused_variables))]
    fn camera_tab(&mut self, ui: &mut Ui, state: &AppState) {
        let adv = self.draft.ui.show_advanced;
        widgets::page_header(
            ui,
            "Kamera",
            "Deine Kamera und die Video-Kacheln, die über anderen Fenstern schweben.",
            |_| {},
        );
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

        widgets::group_card(ui, "Video-Overlay", |ui| {
            widgets::setting_row(
                ui,
                false,
                "Anzeigen",
                Some("Zeigt die Kameras der anderen als kleine Kacheln über allen Fenstern."),
                |ui| widgets::toggle(ui, &mut self.draft.overlay.enabled),
            );
            ui.add_enabled_ui(self.draft.overlay.enabled, |ui| {
                widgets::setting_row(ui, true, "Position", None, |ui| {
                    widgets::row_group(ui, CORNER_PICKER_W, |ui| {
                        corner_picker(ui, &mut self.draft.overlay.corner)
                    });
                });
                widgets::setting_row(ui, true, "Kachelgröße", None, |ui| {
                    widgets::segmented(
                        ui,
                        &mut self.draft.overlay.size,
                        &[
                            (TileSize::Small, "Klein"),
                            (TileSize::Medium, "Mittel"),
                            (TileSize::Large, "Groß"),
                        ],
                    )
                });
                widgets::setting_row(ui, true, "Sprechende Person umranden", None, |ui| {
                    widgets::toggle(ui, &mut self.draft.overlay.speaking_frame)
                });
                if !adv {
                    return;
                }
                widgets::advanced_caption(ui);
                let edge = matches!(self.draft.overlay.corner, Corner::Top | Corner::Bottom);
                widgets::setting_row(ui, false, "Anordnung", None, |ui| {
                    ui.add_enabled_ui(!edge, |ui| {
                        widgets::segmented(
                            ui,
                            &mut self.draft.overlay.expand,
                            &[
                                (Expand::Vertical, "Untereinander"),
                                (Expand::Horizontal, "Nebeneinander"),
                            ],
                        )
                    })
                    .inner
                });
                widgets::setting_row(ui, true, "Abstand zum Rand", None, |ui| {
                    const DRAG_W: f32 = 84.0;
                    widgets::row_group(ui, DRAG_W * 2.0 + 6.0, |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for (axis, prefix) in [(0, "x "), (1, "y ")] {
                            ui.add_sized(
                                [DRAG_W, 24.0],
                                egui::DragValue::new(&mut self.draft.overlay.offset[axis])
                                    .prefix(prefix)
                                    .suffix(" px")
                                    .range(0..=2000),
                            );
                        }
                    });
                });
                widgets::setting_row(ui, true, "Stille Personen abdunkeln", None, |ui| {
                    widgets::toggle(ui, &mut self.draft.overlay.dim_non_speaking)
                });
                widgets::setting_row(ui, true, "Rahmenfarbe", None, |ui| {
                    ui.add_enabled_ui(self.draft.overlay.speaking_frame, |ui| {
                        ui.color_edit_button_srgb(&mut self.draft.overlay.frame_color)
                    })
                    .inner
                });
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

        let adv = self.draft.ui.show_advanced;
        widgets::page_header(
            ui,
            "Allgemein",
            "Dein Name, wie ICU aussieht und was beim Start passiert.",
            |_| {},
        );
        widgets::group_card(ui, "Profil", |ui| {
            let editable = state.me.is_some() && !state.rename_pending;
            widgets::setting_row(ui, false, "Anzeigename", Some("So sehen dich die anderen."), |ui| {
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
            if let Some(err) = &state.name_error {
                widgets::card_note(ui, |ui| {
                    ui.colored_label(theme::tokens(ui.visuals()).error, err);
                });
            }
            if adv {
                widgets::advanced_caption(ui);
                widgets::help_note(
                    ui,
                    "Der Name steht im Zertifikat. Eine Änderung stellt ein neues aus; das \
                     alte bleibt bis zum Ablauf gültig.",
                );
                widgets::setting_row(ui, true, "Server", None, |ui| {
                    widgets::chip(ui, &state.server, 0.0)
                });
                widgets::setting_row(ui, true, "Nutzer-ID", None, |ui| {
                    let id = state.me.as_ref().map(|m| m.user_id.to_string());
                    widgets::chip(ui, id.as_deref().unwrap_or("—"), 0.0)
                });
            }
        });

        widgets::group_card(ui, "Darstellung", |ui| {
            widgets::card_note(ui, |ui| {
                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    for (value, label) in
                        [("system", "System"), ("light", "Hell"), ("dark", "Dunkel")]
                    {
                        theme_swatch(ui, &mut self.draft.ui.theme, value, label);
                    }
                });
            });
            widgets::setting_row(
                ui,
                true,
                "Erweiterte Einstellungen",
                Some("Zeigt überall zusätzlich die technischen Stellschrauben und die Diagnose."),
                |ui| widgets::toggle(ui, &mut self.draft.ui.show_advanced),
            );
        });

        widgets::group_card(ui, "Start", |ui| {
            widgets::setting_row(
                ui,
                false,
                "Minimiert starten",
                Some("ICU startet unsichtbar im Infobereich der Taskleiste."),
                |ui| widgets::toggle(ui, &mut self.draft.ui.start_minimized),
            );
            widgets::setting_row(
                ui,
                true,
                "Einladungslinks mit ICU öffnen",
                Some("Ein Klick auf einen icu://-Link öffnet direkt ICU."),
                |ui| widgets::toggle(ui, &mut self.draft.ui.register_uri_handler),
            );
        });

        self.passport_card(ui, cmds);
        self.logout_card(ui);
    }

    /// Admin-only "Nutzer" tab: inviting, and managing who is already in
    /// (`ControlMsg::UserListRequest`/`AdminSetRole`/`AdminSetDisabled`/
    /// `AdminRevokeUser`, `PROTOCOL.md` §2.2).
    fn users_tab(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        // A fresh invite adds a pending entry; fetch the list without a click
        // and keep the result in the open form.
        let issued = state.last_invite.as_ref().map(|(b, _)| b.as_str());
        if issued != self.seen_invite.as_deref() {
            self.seen_invite = issued.map(str::to_string);
            if let Some(bundle) = issued {
                cmds.push(Command::RequestUserList);
                self.fresh_invite = self.invite_open;
                // Minted from this form: it goes straight to the clipboard,
                // copying is what comes next anyway.
                if self.fresh_invite {
                    ui.ctx().copy_text(bundle.to_string());
                    self.invite_status = Some("In die Zwischenablage kopiert.".into());
                }
            }
        }

        // Retry quickly while nothing has arrived, then keep online status
        // fresh at a relaxed pace.
        let every = if state.user_list.is_empty() {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(10)
        };
        if self.users_requested.is_none_or(|t| t.elapsed() >= every) {
            self.users_requested = Some(Instant::now());
            cmds.push(Command::RequestUserList);
        }
        ui.ctx().request_repaint_after(every);

        let me = state.me.as_ref().map(|m| m.user_id);
        let (removed, rest): (Vec<_>, Vec<_>) = state.user_list.iter().partition(|u| u.revoked);
        let (mut pending, mut members): (Vec<_>, Vec<_>) =
            rest.into_iter().partition(|u| u.pending);
        // You first, then admins, then everyone else, each by name.
        members.sort_by_cached_key(|u| (Some(u.user_id) != me, !u.is_admin, u.name.to_lowercase()));
        pending.sort_by(|a, b| b.created.cmp(&a.created));

        widgets::page_header(
            ui,
            "Nutzer",
            "Wer auf diesen Server darf. Nur Admins sehen diese Seite.",
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if self.invite_open {
                    if widgets::button(ui, widgets::ButtonKind::Neutral, icon::X, "Schließen")
                        .clicked()
                    {
                        self.invite_open = false;
                        self.fresh_invite = false;
                    }
                } else if widgets::button(
                    ui,
                    widgets::ButtonKind::Primary,
                    icon::USER_PLUS,
                    "Einladen",
                )
                .clicked()
                {
                    self.invite_open = true;
                    self.fresh_invite = false;
                    self.invite_status = None;
                }
            },
        );

        if let Some(err) = &state.user_action_error {
            widgets::banner(
                ui,
                widgets::BannerKind::Error,
                "Das hat nicht geklappt",
                err,
                None::<fn()>,
            );
            ui.add_space(14.0);
        }

        if self.invite_open {
            self.invite_card(ui, state, cmds);
        }

        if state.user_list.is_empty() {
            widgets::group_card(ui, "Mitglieder", |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(12, 22))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.spinner();
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new("Liste wird geladen …")
                                    .color(theme::tokens(ui.visuals()).dim_text),
                            );
                        });
                    });
            });
            return;
        }

        widgets::group_card(ui, &format!("Mitglieder · {}", members.len()), |ui| {
            for (i, user) in members.iter().enumerate() {
                self.member_row(ui, i > 0, user, Some(user.user_id) == me, cmds);
            }
            if members.len() == 1 && pending.is_empty() {
                ui.separator();
                widgets::help_note(
                    ui,
                    "Noch bist du allein hier. Über „Einladen“ holst du jemanden dazu.",
                );
            }
        });

        if !pending.is_empty() {
            widgets::group_card(ui, &format!("Offene Einladungen · {}", pending.len()), |ui| {
                for (i, user) in pending.iter().enumerate() {
                    self.member_row(ui, i > 0, user, false, cmds);
                }
            });
        }

        if !removed.is_empty() {
            let open = self.show_removed;
            let header = ui
                .horizontal(|ui| {
                    let tok = theme::tokens(ui.visuals());
                    widgets::disclosure(ui, open, tok.muted_text);
                    ui.label(theme::caps_text(&format!(
                        "Entfernte Zugänge · {}",
                        removed.len()
                    )));
                })
                .response
                .interact(egui::Sense::click());
            if header.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if header.clicked() {
                self.show_removed = !open;
            }
            ui.add_space(4.0);
            if open {
                let mut names: Vec<_> = removed.iter().map(|u| u.name.as_str()).collect();
                names.sort_by_key(|n| n.to_lowercase());
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .stroke(egui::Stroke::new(
                        1.0,
                        ui.visuals().widgets.inactive.bg_stroke.color,
                    ))
                    .corner_radius(egui::CornerRadius::same(theme::R_CARD))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        widgets::card_note(ui, |ui| {
                            let tok = theme::tokens(ui.visuals());
                            ui.label(
                                RichText::new(
                                    "Diese Zugänge sind endgültig entfernt. Wer wieder dazukommen \
                                     soll, braucht eine neue Einladung.",
                                )
                                .size(11.5)
                                .color(tok.dim_text),
                            );
                            ui.add_space(2.0);
                            ui.label(RichText::new(names.join(", ")).color(tok.muted_text));
                        });
                    });
            }
        }
    }

    /// One person: avatar with presence dot, name with role badges, a quiet
    /// status line, and everything you can do to them behind one "⋯" menu —
    /// or, while a removal waits for its confirmation, that confirmation.
    fn member_row(
        &mut self,
        ui: &mut Ui,
        divider: bool,
        user: &icu_proto::UserSummary,
        is_self: bool,
        cmds: &mut Vec<Command>,
    ) {
        if divider {
            ui.separator();
        }
        let tok = theme::tokens(ui.visuals());
        let confirming = self.confirm_revoke == Some(user.user_id);
        let row = egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let avatar = widgets::avatar(ui, &user.name, user.user_id, false, 32.0, None);
                    if !user.pending {
                        let dot = if user.disabled {
                            tok.error
                        } else if user.online {
                            tok.connected
                        } else {
                            tok.dim_text
                        };
                        widgets::presence_dot(ui, avatar.rect, dot);
                    }
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(
                                RichText::new(&user.name)
                                    .font(egui::FontId::new(13.5, super::fonts::semibold()))
                                    .color(ui.visuals().text_color()),
                            );
                            if is_self {
                                widgets::badge(ui, "Du", widgets::BadgeKind::Neutral);
                            }
                            if user.is_admin {
                                widgets::badge(ui, "Admin", widgets::BadgeKind::Accent)
                                    .on_hover_text("Darf einladen und Mitglieder verwalten");
                            }
                            if user.disabled {
                                widgets::badge(ui, "Gesperrt", widgets::BadgeKind::Danger)
                                    .on_hover_text("Kann sich nicht verbinden, bis jemand entsperrt");
                            }
                        });
                        let since = short_date(&user.created);
                        let status = if user.pending {
                            format!("Eingeladen am {since} · noch nicht eingelöst")
                        } else if user.online {
                            "Online".to_string()
                        } else {
                            format!("Offline · dabei seit {since}")
                        };
                        ui.label(RichText::new(status).size(11.5).color(tok.dim_text));
                    });
                    widgets::rtl(ui, 38.0, |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if is_self {
                            return;
                        }
                        if confirming {
                            let label = if user.pending {
                                "Zurückziehen"
                            } else {
                                "Entfernen"
                            };
                            if widgets::button(ui, widgets::ButtonKind::Danger, icon::TRASH, label)
                                .clicked()
                            {
                                self.confirm_revoke = None;
                                cmds.push(Command::RevokeUser {
                                    user_id: user.user_id,
                                });
                                cmds.push(Command::RequestUserList);
                            }
                            if widgets::button(ui, widgets::ButtonKind::Neutral, "", "Abbrechen")
                                .clicked()
                            {
                                self.confirm_revoke = None;
                            }
                            ui.label(
                                RichText::new(if user.pending {
                                    "Einladung ungültig machen?"
                                } else {
                                    "Endgültig entfernen?"
                                })
                                .size(12.0)
                                .color(tok.error),
                            );
                            return;
                        }
                        if user.pending {
                            if widgets::button(ui, widgets::ButtonKind::Neutral, "", "Zurückziehen")
                                .on_hover_text("Die Einladung wird ungültig")
                                .clicked()
                            {
                                self.confirm_revoke = Some(user.user_id);
                            }
                            return;
                        }
                        let menu = widgets::more_button(ui).on_hover_text("Aktionen");
                        egui::Popup::menu(&menu)
                            .align(egui::RectAlign::BOTTOM_END)
                            .gap(4.0)
                            .show(|ui| {
                                ui.set_min_width(210.0);
                                let (glyph, label) = if user.is_admin {
                                    (icon::CROWN_SIMPLE, "Admin-Rechte entziehen")
                                } else {
                                    (icon::CROWN, "Zum Admin machen")
                                };
                                if widgets::menu_item(ui, glyph, label, false).clicked() {
                                    cmds.push(Command::SetUserAdmin {
                                        user_id: user.user_id,
                                        is_admin: !user.is_admin,
                                    });
                                    cmds.push(Command::RequestUserList);
                                }
                                let (glyph, label) = if user.disabled {
                                    (icon::LOCK_OPEN, "Entsperren")
                                } else {
                                    (icon::LOCK, "Vorübergehend sperren")
                                };
                                if widgets::menu_item(ui, glyph, label, false).clicked() {
                                    cmds.push(Command::SetUserDisabled {
                                        user_id: user.user_id,
                                        disabled: !user.disabled,
                                    });
                                    cmds.push(Command::RequestUserList);
                                }
                                ui.separator();
                                if widgets::menu_item(ui, icon::TRASH, "Entfernen …", true)
                                    .clicked()
                                {
                                    self.confirm_revoke = Some(user.user_id);
                                }
                            });
                    });
                });
            });
        if confirming {
            // Tint the whole row so it is obvious which person is meant.
            ui.painter().rect_filled(
                row.response.rect,
                egui::CornerRadius::ZERO,
                tok.error_tint.gamma_multiply(0.5),
            );
        }
    }

    /// Minting an invitation: an optional placeholder name, how long it
    /// stays valid, and — once the server has issued it — copy or save.
    /// The text itself is never shown: it is a key to this server, and a
    /// settings window is exactly what people have open during a screen
    /// share.
    fn invite_card(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        widgets::group_card(ui, "Neue Person einladen", |ui| {
            let tok = theme::tokens(ui.visuals());
            if let (true, Some((bundle, expires))) = (self.fresh_invite, &state.last_invite) {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(12, 12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icon::CHECK_CIRCLE).size(22.0).color(tok.connected));
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(theme::strong_text("Einladung erstellt"));
                                ui.label(
                                    RichText::new(format!(
                                        "Einmal einlösbar, gültig bis {expires}. Schick sie über \
                                         einen Weg, dem du vertraust – die Person fügt sie beim \
                                         ersten Start von ICU ein."
                                    ))
                                    .size(12.0)
                                    .color(tok.muted_text),
                                );
                            });
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if widgets::button(
                                ui,
                                widgets::ButtonKind::Primary,
                                icon::COPY,
                                "Kopieren",
                            )
                            .clicked()
                            {
                                ui.ctx().copy_text(bundle.clone());
                                self.invite_status = Some("In die Zwischenablage kopiert.".into());
                            }
                            if widgets::button(
                                ui,
                                widgets::ButtonKind::Neutral,
                                icon::EXPORT,
                                "Als Datei speichern",
                            )
                            .clicked()
                            {
                                self.invite_status =
                                    Some(match config::save_invite(&self.paths, bundle) {
                                        Ok(path) => format!("Gespeichert: {}", path.display()),
                                        Err(e) => crate::problem::storage("Die Einladung", &e).text,
                                    });
                            }
                            if widgets::button(
                                ui,
                                widgets::ButtonKind::Neutral,
                                icon::USER_PLUS,
                                "Noch jemanden",
                            )
                            .clicked()
                            {
                                self.fresh_invite = false;
                                self.invite_name.clear();
                                self.invite_status = None;
                            }
                        });
                        if let Some(status) = &self.invite_status {
                            ui.add_space(6.0);
                            ui.label(RichText::new(status).size(11.5).color(tok.dim_text));
                        }
                    });
                return;
            }

            widgets::setting_row(
                ui,
                false,
                "Name",
                Some("Nur ein Platzhalter, bis die Person ihren eigenen Namen wählt."),
                |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.invite_name)
                            .desired_width(200.0)
                            .hint_text("optional"),
                    )
                },
            );
            widgets::setting_row(ui, true, "Gültig für", None, |ui| {
                widgets::segmented(ui, &mut self.invite_ttl_s, &INVITE_TTLS)
            });
            if let Some(err) = &state.invite_error {
                widgets::card_note(ui, |ui| {
                    ui.colored_label(tok.error, RichText::new(err).size(12.0));
                });
            }
            ui.separator();
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    // The name is only a placeholder until the invited
                    // person picks their own; empty means a generated one.
                    let valid = self.invite_name.trim().is_empty()
                        || icu_proto::validate_name(&icu_proto::normalize_name(&self.invite_name))
                            .is_ok();
                    ui.horizontal(|ui| {
                        if !valid {
                            ui.label(
                                RichText::new("Dieser Name geht nicht.")
                                    .size(11.5)
                                    .color(tok.error),
                            );
                        }
                        widgets::rtl(ui, 30.0, |ui| {
                            if ui
                                .add_enabled_ui(valid, |ui| {
                                    widgets::button(
                                        ui,
                                        widgets::ButtonKind::Primary,
                                        icon::TICKET,
                                        "Einladung erstellen",
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
                });
        });
    }

    fn about_tab(&mut self, ui: &mut Ui, state: &AppState) {
        let adv = self.draft.ui.show_advanced;
        widgets::page_header(
            ui,
            "Über ICU",
            if adv {
                "Version, Lizenzen und Messwerte zur Verbindung."
            } else {
                "Version und Lizenzen."
            },
            |_| {},
        );
        widgets::group_card(ui, "ICU", |ui| {
            widgets::setting_row(ui, false, "Version", None, |ui| {
                widgets::chip(ui, env!("CARGO_PKG_VERSION"), 0.0)
            });
            if adv {
                widgets::setting_row(ui, true, "Audio-Backend", None, |ui| {
                    widgets::chip(ui, state.backend, 0.0)
                });
            }
            ui.separator();
            widgets::card_note(ui, |ui| {
                ui.label(
                    RichText::new(
                        "Enthält Opus (BSD-3), RNNoise/nnnoiseless (BSD-3), WebRTC AudioProcessing \
                         (BSD-3), OpenH264 (BSD-2, aus Quelle gebaut — nicht unter Ciscos \
                         Lizenzgewährung für Binaries), Inter (OFL-1.1), Phosphor-Icons (MIT/Apache-2.0). \
                         Lizenztexte liegen dem Release bei.",
                    )
                    .small()
                    .color(theme::tokens(ui.visuals()).dim_text),
                );
            });
        });

        if !adv {
            return;
        }
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
                ui.horizontal(|ui| {
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
                                Err(e) => crate::problem::storage("Die Statistik", &e).text,
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
        let fmt_ms = crate::stats::fmt_ms;
        self.diagnose.jitter = self.stats.jitter_delay.lock().unwrap().p50_p95();
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

/// `2026-09-24T…` → `24.09.2026`; anything unexpected comes back as is.
fn short_date(rfc3339: &str) -> String {
    match rfc3339.get(..10).map(|d| d.split('-').collect::<Vec<_>>()).as_deref() {
        Some([y, m, d]) => format!("{d}.{m}.{y}"),
        _ => rfc3339.to_string(),
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
    widgets::paint_well(ui, rect, theme::R_CHIP);
    let painter = ui.painter();
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
                widgets::paint_well(ui, rect, theme::R_CTRL);
                let tile_color = if selected {
                    tok.accent
                } else {
                    ui.visuals().widgets.inactive.bg_stroke.color
                };
                ui.painter()
                    .rect_filled(tile_rect_fn(rect), 2.0, tile_color);
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
        ui.label(RichText::new(label).size(11.5));
    });
}

fn device_combo(
    ui: &mut Ui,
    divider: bool,
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
    widgets::setting_row(ui, divider, label, None, |ui| {
        egui::ComboBox::from_id_salt(label)
            .selected_text(current)
            .width(240.0)
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

/// Label column of a [`binding_row`], wide enough for "Modus wechseln".
const BINDING_LABEL_W: f32 = 124.0;

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
                widgets::label_column(ui, BINDING_LABEL_W, 30.0, label);
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
                    ui.add_space(BINDING_LABEL_W);
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
