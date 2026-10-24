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
        self, Corner, Expand, HotkeyBackend, Identity, ModeSetting, Paths, Settings, VoiceStrength,
        SHY_HOLDS, SHY_RETURNS, TILE_SIZES,
    },
    engine::{AppState, Command},
    stats::DebugStats,
};

/// Text-field width wherever a row pairs a field with a button, so the
/// fields line up down the panel.
const FIELD_W: f32 = 150.0;
/// Inner margin of a single-line field in a settings row: with the 13.5 px
/// body text it makes the field as tall as the buttons beside it (30 px).
const FIELD_MARGIN: egui::Margin = egui::Margin::symmetric(10, 6);
/// Validity choices for an invite; the last is the server's cap.
const INVITE_TTLS: [(u64, &str); 4] = [
    (60 * 60, "1 Stunde"),
    (icu_proto::limits::INVITE_TTL_S, "1 Tag"),
    (3 * 24 * 60 * 60, "3 Tage"),
    (icu_proto::limits::MAX_INVITE_TTL_S, "7 Tage"),
];
/// Height the passport paste field grows to before it scrolls.
const PASTE_MAX_H: f32 = 120.0;
/// Width of the camera preview beside the camera controls: enough to judge
/// framing, small enough that the card stays a settings card.
const PREVIEW_W: f32 = 176.0;
/// The largest distance of the overlay from the screen edge the slider
/// offers, in pixels.
const EDGE_MAX: u32 = 200;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Tab {
    Audio,
    Sound,
    Hotkeys,
    Camera,
    Overlay,
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
    /// which one first (`docs/reference/protocol.md` §2.3.1). With the settings it carries,
    /// if any.
    import_preview: Option<(Box<Identity>, Option<Box<Settings>>, config::ImportSummary)>,
    /// The import option: take the passport's settings along. On for every
    /// new preview.
    import_settings: bool,
    /// The passport paste field is open.
    import_open: bool,
    /// Put the cursor into it on the next frame.
    focus_import: bool,
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
    /// Keeps [`Self::draft`] in step with the engine.
    sync: DraftSync,
    /// „Assistent“ on the Audio page was clicked; handed back to `App` as
    /// [`SettingsOutput::open_setup`].
    open_setup: bool,
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
    /// The overlay thread is running; without layer shell (GNOME) the
    /// overlay card says so instead of offering switches that do nothing.
    overlay_available: bool,
    /// Closing can hide the main window to the tray on this desktop
    /// (tray host, not Wayland); otherwise „Infobereich“ minimizes.
    tray_available: bool,
    /// „Kein Echo feststellbar" was answered; it is not offered again while
    /// this window lives.
    aec_hint_dismissed: bool,
    /// A running „Aufnehmen" on the Tastenkürzel page.
    capture: Option<Capture>,
    /// Why the last „Aufnehmen" failed, shown under the bindings.
    capture_error: Option<String>,
    /// The local camera preview (capture without sending) is requested
    /// while the Kamera page is on screen.
    camera_preview: bool,
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
    /// The camera should capture for the preview without sending
    /// (`CameraHandle::set_preview`): the Kamera page is on screen and the
    /// user asked for it.
    pub camera_preview: bool,
    /// The Audio or Klang page, both with the mic test, is on screen (the
    /// Klang switches are compared while listening); anywhere else a running
    /// test is stopped.
    pub mic_test_page: bool,
    /// How much taller the page is than the window shows (negative:
    /// shorter).
    pub overflow: f32,
    /// „Assistent“ was clicked: open the setup (`super::setup`).
    pub open_setup: bool,
}

/// Keeps a window's draft of the settings in step with the engine: the
/// window's own changes go out as `Command::UpdateSettings`, and changes
/// made elsewhere are taken over instead of being written back with the next
/// edit. The settings window and the setup (`super::setup`) each hold one.
pub(super) struct DraftSync {
    /// The draft as last handed out in `Command::UpdateSettings`, so a
    /// change is noticed by comparison instead of by cloning the whole
    /// draft on every frame.
    applied: Settings,
    /// The engine's settings as last seen. When they move to something
    /// this window did not send (a hotkey switched the mode, the tray muted,
    /// a volume changed in the main window), the draft takes them over —
    /// otherwise the next change made here would write the old values back.
    seen: Settings,
    /// What this window sent and the engine may not have echoed yet: a
    /// slider drag sends a new version every frame, and an echo of an older
    /// one must not pull the slider back.
    sent: std::collections::VecDeque<Settings>,
}

impl DraftSync {
    /// In step with `settings`, the draft's starting point.
    pub(super) fn new(settings: &Settings) -> DraftSync {
        DraftSync {
            applied: settings.clone(),
            seen: settings.clone(),
            sent: std::collections::VecDeque::new(),
        }
    }

    /// Takes over settings the engine changed on its own into `draft`. An
    /// echo of one of our own `UpdateSettings` is left alone; anything else
    /// came from elsewhere and wins.
    pub(super) fn adopt(&mut self, draft: &mut Settings, engine: &Settings) {
        if *engine == self.seen {
            return;
        }
        self.seen.clone_from(engine);
        if let Some(i) = self.sent.iter().position(|s| s == engine) {
            // Our own, possibly an older one: everything sent before it has
            // been superseded by it.
            self.sent.drain(..=i);
            return;
        }
        self.sent.clear();
        draft.clone_from(engine);
        self.applied.clone_from(engine);
    }

    /// The command that applies `draft`, if it changed since it last went
    /// out.
    pub(super) fn commit(&mut self, draft: &Settings) -> Option<Command> {
        if *draft == self.applied {
            return None;
        }
        self.applied.clone_from(draft);
        if self.sent.len() == 32 {
            self.sent.pop_front();
        }
        self.sent.push_back(draft.clone());
        Some(Command::UpdateSettings(Box::new(draft.clone())))
    }

    /// The header's theme button changed the theme outside the window:
    /// counts as sent, so it is not sent again.
    pub(super) fn set_theme(&mut self, theme: &str) {
        self.applied.ui.theme = theme.to_string();
    }
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
            sync: DraftSync::new(&state.settings),
            open_setup: false,
            name: name.clone(),
            name_synced: name,
            passport_import: String::new(),
            passport_status: None,
            import_preview: None,
            import_settings: true,
            import_open: false,
            focus_import: false,
            invite_name: String::new(),
            invite_ttl_s: icu_proto::limits::INVITE_TTL_S,
            invite_status: None,
            seen_invite: None,
            tab: Tab::Audio,
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
            overlay_available: cfg!(feature = "overlay"),
            tray_available: cfg!(feature = "tray"),
            aec_hint_dismissed: false,
            capture: None,
            capture_error: None,
            camera_preview: false,
        }
    }

    /// Opens Allgemein with the passport paste field ready: the way back
    /// after a certificate was renewed on another device.
    pub fn open_import(&mut self) {
        self.select_tab(Tab::General);
        self.import_open = true;
        self.focus_import = true;
    }

    fn select_tab(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        // A reader can finish after the user leaves. Dropping its result
        // slot here prevents that old key from changing a binding later.
        self.capture = None;
        self.capture_error = None;
        match tab {
            Tab::Users => self.users_requested = None,
            #[cfg(feature = "camera")]
            Tab::Camera => self.cameras_stale = true,
            _ => {}
        }
    }

    /// Opens Allgemein with `text` (a passport or invite handed over by a
    /// second start, `crate::instance`) already in the field and checked:
    /// one click on „Übernehmen" takes it.
    pub fn import_text(&mut self, text: String) {
        self.open_import();
        self.focus_import = false;
        self.passport_import = text;
        self.preview_passport();
    }

    /// Whether the overlay could start on this desktop.
    pub fn with_overlay_available(mut self, available: bool) -> SettingsView {
        self.overlay_available = available;
        self
    }

    /// Whether closing can hide the window to the tray on this desktop.
    pub fn with_tray_available(mut self, available: bool) -> SettingsView {
        self.tray_available = available;
        self
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
        self.sync.set_theme(theme);
    }

    /// Draws the panel content. `debug_window_open` is what `App` knows;
    /// the answer comes back in the output.
    pub fn ui(&mut self, ui: &mut Ui, state: &AppState, debug_window_open: bool) -> SettingsOutput {
        let mut cmds = Vec::new();
        self.debug_window_open = debug_window_open;
        self.sync.adopt(&mut self.draft, &state.settings);
        if state.privacy_active {
            // Do not restart a local preview automatically when Privacy ends.
            self.camera_preview = false;
        }

        // A live demote can orphan the tab: it is only in the rail while
        // `state.is_admin`, so bounce back to Allgemein instead of rendering
        // a tab that is no longer selectable.
        if self.tab == Tab::Users && !state.is_admin {
            self.select_tab(Tab::General);
        }

        // The pages people come for first; Nutzer only for admins.
        let mut tabs = vec![
            (Tab::Audio, icon::MICROPHONE, "Audio"),
            (Tab::Sound, icon::WAVEFORM, "Klang"),
            (Tab::Hotkeys, icon::COMMAND, "Tastenkürzel"),
            (Tab::Camera, icon::VIDEO_CAMERA, "Kamera"),
            (Tab::Overlay, icon::SQUARES_FOUR, "Overlay"),
            (Tab::General, icon::GEAR_FINE, "Allgemein"),
            (Tab::About, icon::QUESTION, "Über ICU"),
        ];
        if state.is_admin {
            tabs.insert(6, (Tab::Users, icon::USERS_THREE, "Nutzer"));
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
                        self.select_tab(tab);
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
                            widgets::named_toggle(ui, &mut self.draft.ui.show_advanced, "Erweitert")
                                .on_hover_text("Erweiterte Einstellungen anzeigen")
                        });
                    });
                    ui.add_space(4.0);
                    ui.separator();
                });
            });

        let page = egui::ScrollArea::vertical()
            .id_salt(("settings_page", self.tab))
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
                            Tab::Audio => self.audio_tab(ui, state, &mut cmds),
                            Tab::Sound => self.sound_tab(ui, state, &mut cmds),
                            Tab::Hotkeys => self.hotkeys_tab(ui, state, &mut cmds),
                            Tab::Camera => self.camera_tab(ui, state),
                            Tab::Overlay => self.overlay_tab(ui),
                            Tab::General => self.general_tab(ui, state, &mut cmds),
                            Tab::Users => self.users_tab(ui, state, &mut cmds),
                            Tab::About => self.about_tab(ui, state),
                        }
                    });
            });

        cmds.extend(self.sync.commit(&self.draft));
        // A page taller than the window (expert rows, a long member list)
        // grows the window instead of hiding its end behind a scroll bar.
        let overflow = page.content_size.y - page.inner_rect.height();
        super::grow_to_fit(ui.ctx(), overflow);
        SettingsOutput {
            cmds,
            debug_window_open: self.debug_window_open,
            logout: std::mem::take(&mut self.logout_requested),
            camera_preview: self.tab == Tab::Camera && self.camera_preview,
            mic_test_page: matches!(self.tab, Tab::Audio | Tab::Sound),
            overflow,
            open_setup: std::mem::take(&mut self.open_setup),
        }
    }

    fn audio_tab(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        let adv = self.draft.ui.show_advanced;
        widgets::page_header(
            ui,
            "Audio",
            "Mikrofon, Lautsprecher und wann du zu hören bist.",
            |ui| {
                // The step-by-step way through the same choices, the one a
                // new install opens with (`super::setup`).
                if widgets::button(
                    ui,
                    widgets::ButtonKind::Neutral,
                    icon::MAGIC_WAND,
                    "Assistent",
                )
                .on_hover_text("Mikrofon, Sprechen und Klang Schritt für Schritt einrichten")
                .clicked()
                {
                    self.open_setup = true;
                }
            },
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
            // The mic test sits next to the microphone: hearing yourself
            // answers „bin ich zu leise, klinge ich blechern?" without a
            // second person; it plays what the others get, after the whole
            // send chain.
            device_combo(
                ui,
                false,
                "Mikrofon",
                &mut self.draft.audio.input_device,
                &inputs,
                |ui| mic_test_button(ui, state.mic_test, cmds),
            );
            device_combo(
                ui,
                true,
                "Lautsprecher",
                &mut self.draft.audio.output_device,
                &outputs,
                |_| {},
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
            if self.draft.audio.mode == ModeSetting::Ptt {
                // The key is what people look for next to the mode, so it is
                // shown here and changed with one click on its own page.
                let key = super::ptt_key_label(&self.draft.hotkeys, &state.hotkey_backend);
                widgets::setting_row(ui, true, "Taste", None, |ui| {
                    let width = widgets::button_width(ui, icon::PENCIL_SIMPLE, "Ändern")
                        + 8.0
                        + widgets::chip_width(ui, std::slice::from_ref(&key));
                    widgets::row_group(ui, width, |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        widgets::chip(ui, &key, 0.0);
                        if widgets::button(
                            ui,
                            widgets::ButtonKind::Neutral,
                            icon::PENCIL_SIMPLE,
                            "Ändern",
                        )
                        .clicked()
                        {
                            self.select_tab(Tab::Hotkeys);
                            self.expanded_bindings.insert("Push-to-Talk");
                        }
                    });
                });
            } else {
                ui.add_enabled_ui(self.draft.audio.denoise, |ui| {
                    widgets::setting_row(
                        ui,
                        true,
                        "Schwelle automatisch",
                        Some("Passt sich laufend an deine Umgebung an. Braucht die Rauschunterdrückung."),
                        |ui| widgets::named_toggle(ui, &mut self.draft.audio.vad_auto, "Schwelle automatisch"),
                    )
                });
                if self.draft.audio.denoise && !self.draft.audio.vad_auto {
                    widgets::slider_row(
                        ui,
                        true,
                        "Schwelle",
                        &mut self.draft.audio.vad_threshold,
                        0.0..=1.0,
                        "",
                    );
                } else if !self.draft.audio.denoise {
                    widgets::help_note(
                        ui,
                        "Ohne Rauschunterdrückung öffnet ein fester Lautstärkepegel das \
                         Mikrofon. Geräusche können dabei ebenfalls auslösen; die \
                         Schwellen-Regler greifen nur mit Rauschunterdrückung.",
                    );
                }
            }
            let auto_level = self.draft.audio.auto_level && self.draft.audio.denoise;
            if adv {
                widgets::advanced_caption(ui);
                let mut first = true;
                let mut divider = || !std::mem::take(&mut first);
                if self.draft.audio.mode == ModeSetting::Vad
                    && self.draft.audio.denoise
                    && self.draft.audio.vad_auto
                {
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
                )
                .on_hover_text(if auto_level {
                    "Manuelle Eingangsverstärkung vor der Pegelautomatik. \
                     Die Automatik kann diese Änderung wieder ausgleichen."
                } else {
                    "Manuelle Eingangsverstärkung vor der Klangaufbereitung."
                });
                let mut release = self.draft.audio.release_ms as f32;
                if widgets::slider_row(
                    ui,
                    divider(),
                    "PTT-Nachlauf",
                    &mut release,
                    0.0..=1000.0,
                    " ms",
                )
                .changed()
                {
                    self.draft.audio.release_ms = release as u32;
                }
                let mut hangover = self.draft.audio.hangover_ms as f32;
                if widgets::slider_row(
                    ui,
                    divider(),
                    "VAD-Nachlauf",
                    &mut hangover,
                    0.0..=3000.0,
                    " ms",
                )
                .changed()
                {
                    self.draft.audio.hangover_ms = hangover as u32;
                }
                let mut attack = self.draft.audio.attack_ms as f32;
                if widgets::slider_row(ui, divider(), "VAD-Anlauf", &mut attack, 0.0..=200.0, " ms")
                    .changed()
                {
                    self.draft.audio.attack_ms = attack as u32;
                }
            }
        });

        widgets::group_card(ui, "Hinweise", |ui| {
            let audio = &mut self.draft.audio;
            let cues = [
                (
                    &mut audio.cue_join,
                    "Kommen und Gehen",
                    "Ein kurzer Ton, wenn jemand den Raum betritt oder verlässt",
                ),
                (
                    &mut audio.cue_tx,
                    "Senden",
                    "Ein hoher Ton, wenn du zu hören bist, ein tieferer, wenn nicht mehr",
                ),
                (
                    &mut audio.cue_mute,
                    "Stummschalten",
                    "Zwei Töne beim Stummschalten und wieder Einschalten",
                ),
            ];
            widgets::setting_row(ui, false, "Töne", None, |ui| {
                const GAP: f32 = 6.0;
                let width = cues
                    .iter()
                    .map(|(_, label, _)| widgets::chip_toggle_width(ui, label))
                    .sum::<f32>()
                    + GAP * (cues.len() - 1) as f32;
                // Flush right in reading (and tab) order. Narrow rows
                // wrap rather than extending back into the sidebar.
                ui.allocate_ui_with_layout(
                    egui::vec2(width.min(ui.available_width()), 28.0),
                    egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                    |ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
                        for (on, label, hover) in cues {
                            widgets::chip_toggle(ui, on, label).on_hover_text(hover);
                        }
                    },
                );
            });
            widgets::setting_row(
                ui,
                true,
                "Meldung bei Kommen und Gehen",
                Some("Zusätzlich zum Ton eine Benachrichtigung – hilfreich, solange ICU im Tray liegt."),
                |ui| {
                    widgets::named_toggle(
                        ui,
                        &mut self.draft.ui.notify_join,
                        "Meldung bei Kommen und Gehen",
                    )
                },
            );
        });
    }

    /// What the voice goes through on the way out and in, and the cue
    /// sounds: switches that are right by default and rarely touched.
    fn sound_tab(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        let adv = self.draft.ui.show_advanced;
        widgets::page_header(
            ui,
            "Klang",
            "Wie deine Stimme aufbereitet wird und wie die anderen bei dir ankommen.",
            // The test keeps running across Audio and Klang, so every switch
            // below is heard the moment it flips.
            |ui| mic_test_button(ui, state.mic_test, cmds),
        );
        widgets::group_card(ui, "Aufbereitung", |ui| {
            widgets::setting_row(
                ui,
                false,
                "Rauschunterdrückung",
                Some("Filtert Tastatur, Lüfter und Hintergrundgeräusche heraus."),
                |ui| {
                    widgets::named_toggle(ui, &mut self.draft.audio.denoise, "Rauschunterdrückung")
                },
            );
            ui.add_enabled_ui(state.aec_available, |ui| {
                widgets::setting_row(
                    ui,
                    true,
                    "Echo-Unterdrückung",
                    Some(if state.aec_available {
                        "Damit die anderen sich nicht selbst hören, wenn du ohne Kopfhörer sprichst."
                    } else {
                        "Auf dieser Installation nicht verfügbar."
                    }),
                    |ui| widgets::named_toggle(ui, &mut self.draft.audio.aec, "Echo-Unterdrückung"),
                )
            });
            let tok = theme::tokens(ui.visuals());
            if state.aec_available && self.draft.audio.aec {
                // `dsp::aec::Diagnosis`: what the canceller's own statistics
                // say about the echo path it sees.
                let problem = if !state.aec_reference || state.aec_reference_silent {
                    Some("Gerade wirkungslos: das Audiosystem liefert nicht, was du hörst.")
                } else if state.aec_suspect {
                    Some("Echo erkannt, wird aber nicht entfernt — Lautsprecher-Auswahl prüfen.")
                } else {
                    None
                };
                if let Some(problem) = problem {
                    widgets::card_note(ui, |ui| {
                        ui.colored_label(tok.connecting, RichText::new(problem).size(11.5));
                    });
                } else if state.aec_no_echo && !self.aec_hint_dismissed {
                    // Once, dismissible (§6): with a headset the canceller
                    // only costs latency.
                    widgets::card_note(ui, |ui| {
                        ui.label(
                            RichText::new(
                                "Kein Echo feststellbar — mit Kopfhörern kann die \
                                 Echo-Unterdrückung aus, das spart 9 ms Verzögerung.",
                            )
                            .size(11.5)
                            .color(tok.dim_text),
                        );
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if widgets::button(
                                ui,
                                widgets::ButtonKind::Neutral,
                                icon::SPEAKER_SLASH,
                                "Ausschalten",
                            )
                            .clicked()
                            {
                                self.draft.audio.aec = false;
                                self.aec_hint_dismissed = true;
                            }
                            if widgets::button(
                                ui,
                                widgets::ButtonKind::Neutral,
                                icon::X,
                                "Anlassen",
                            )
                            .clicked()
                            {
                                self.aec_hint_dismissed = true;
                            }
                        });
                    });
                }
            }
            ui.add_enabled_ui(true, |ui| {
                widgets::setting_row(
                    ui,
                    true,
                    "Pegel automatisch",
                    Some("Regelt dein Mikrofon nach, wenn du zu leise oder zu laut bist."),
                    |ui| widgets::autotune_control(ui, &mut self.draft.audio, state, cmds),
                )
            });
            widgets::setting_row(
                ui,
                true,
                "Selbst gleich laut ankommen",
                Some(if self.draft.audio.auto_level && self.draft.audio.denoise {
                    "Die Pegelautomatik gleicht deine Stimme aus; dieser Schalter \
                     begrenzt zusätzlich Lautstärkespitzen."
                } else {
                    "Gleicht deine Stimme vor dem Senden aus und begrenzt Lautstärkespitzen."
                }),
                |ui| {
                    widgets::named_toggle(
                        ui,
                        &mut self.draft.audio.normalize_outgoing,
                        "Selbst gleich laut ankommen",
                    )
                },
            );
            widgets::setting_row(
                ui,
                true,
                "Andere gleich laut hören",
                Some("Bringt alle auf eine ähnliche Lautstärke, leise wie laute."),
                |ui| {
                    widgets::named_toggle(
                        ui,
                        &mut self.draft.audio.normalize_incoming,
                        "Andere gleich laut hören",
                    )
                },
            );
            widgets::setting_row(
                ui,
                true,
                "Stimmoptimierung",
                Some("Gleicht leise und laute Stellen aus, damit du gleichmäßig klingst."),
                |ui| widgets::named_toggle(ui, &mut self.draft.audio.voice_opt, "Stimmoptimierung"),
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
                        widgets::chip(
                            ui,
                            &format!("ERL {erl:.0} dB · ERLE {erle:.0} dB{delay}"),
                            0.0,
                        )
                    });
                }
            }
        });
    }

    /// Export and import of the whole identity, and of the settings that may
    /// move with it (`docs/reference/protocol.md` §2.3.1).
    ///
    /// The armored text is never rendered: it carries the private key in the
    /// clear, and a settings window is exactly the thing people have open
    /// during a screen share.
    fn device_card(&mut self, ui: &mut Ui, cmds: &mut Vec<Command>) {
        widgets::group_card(ui, "Dieses Gerät", |ui| {
            widgets::setting_row(ui, false, "Auf ein anderes Gerät mitnehmen", None, |ui| {
                let width = widgets::button_width(ui, icon::COPY, "Kopieren")
                    + 6.0
                    + widgets::button_width(ui, icon::EXPORT, "Speichern");
                widgets::row_group(ui, width, |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if widgets::button(ui, widgets::ButtonKind::Neutral, icon::COPY, "Kopieren")
                        .on_hover_text(if super::native_wayland() {
                            "Als Text in die Zwischenablage, mit deinen Einstellungen; unter \
                             Wayland kann ICU sie nicht sicher wieder leeren"
                        } else {
                            "Als Text in die Zwischenablage, mit deinen Einstellungen; ICU leert \
                             sie nach einer Minute wieder"
                        })
                        .clicked()
                    {
                        match self.passport_text() {
                            Ok((text, with_settings)) => {
                                let copied = copy_secret(ui.ctx(), text);
                                self.passport_status = Some(format!(
                                    "{}{}",
                                    copied_status(copied),
                                    without_settings(with_settings)
                                ));
                            }
                            Err(e) => self.passport_status = Some(e),
                        }
                    }
                    if widgets::button(ui, widgets::ButtonKind::Neutral, icon::EXPORT, "Speichern")
                        .on_hover_text(SAVE_HELP)
                        .clicked()
                    {
                        self.passport_status = Some(match self.passport_text() {
                            Ok((text, with_settings)) => {
                                let path = passport_file();
                                match crate::config::write_atomic(&path, text.as_bytes(), true) {
                                    Ok(()) => format!(
                                        "Gespeichert: {} — enthält deinen privaten Schlüssel, \
                                         nach dem Einspielen löschen.{}",
                                        path.display(),
                                        without_settings(with_settings)
                                    ),
                                    Err(e) => crate::problem::storage("Der Passport", &e).text,
                                }
                            }
                            Err(e) => e,
                        });
                    }
                });
            });
            widgets::setting_row(ui, true, "Passport von einem anderen Gerät", None, |ui| {
                let (glyph, label) = if self.import_open {
                    (icon::X, "Schließen")
                } else {
                    (icon::DOWNLOAD, "Einspielen")
                };
                let width = widgets::button_width(ui, glyph, label);
                widgets::row_group(ui, width, |ui| {
                    if widgets::button(ui, widgets::ButtonKind::Neutral, glyph, label).clicked() {
                        self.import_open = !self.import_open;
                        self.import_preview = None;
                        self.passport_import.clear();
                    }
                });
            });
            if self.import_open {
                self.import_area(ui, cmds);
            }
            self.logout_row(ui);
            widgets::card_note(ui, |ui| {
                let tok = theme::tokens(ui.visuals());
                ui.colored_label(
                    tok.error,
                    RichText::new(
                        "Der Passport enthält deinen privaten Schlüssel — nur sicher \
                         weitergeben, lieber als Datei: ein Zwischenablage-Verlauf behält \
                         ihn. Erneuert ein Gerät sein Zertifikat (neuer Name, jährlich), \
                         braucht das andere einen neuen Passport.",
                    )
                    .size(11.5),
                );
            });
            self.passport_status_note(ui);
        });
    }

    /// Takes a file dropped onto the window while the import area is open:
    /// an `.icu` invite or passport, read as text — the same funnel a paste
    /// uses (mirrors `EnrollApp::take_dropped`, `ui/main_window.rs`).
    /// Anything unreadable is left for the field's own verdict.
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
            self.passport_import = text;
            self.preview_passport();
        }
    }

    /// The paste field for a passport and, as soon as it reads as one,
    /// where it leads and the button that takes it: no separate „Prüfen".
    /// A file dropped here works the same as pasting it.
    fn import_area(&mut self, ui: &mut Ui, cmds: &mut Vec<Command>) {
        let ctx = ui.ctx().clone();
        self.take_dropped(&ctx);
        let dragging = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let mut edited = false;
        widgets::card_note(ui, |ui| {
            // A passport with settings runs to some 50 lines: the field stops
            // growing at a few, so the preview and „Übernehmen" stay in view.
            egui::ScrollArea::vertical()
                .id_salt("passport-paste")
                .max_height(PASTE_MAX_H)
                .show(ui, |ui| {
                    let field = ui.add(
                        egui::TextEdit::multiline(&mut self.passport_import)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text(if dragging {
                                "Datei loslassen, um sie zu öffnen"
                            } else {
                                "Passport hier einfügen (Strg+V) oder eine Datei hierher ziehen"
                            })
                            .font(egui::TextStyle::Monospace),
                    );
                    if std::mem::take(&mut self.focus_import) {
                        field.request_focus();
                    }
                    edited = field.changed();
                });
        });
        // A confirmation belongs to the text it was made for.
        if edited {
            self.import_preview = None;
            if !self.passport_import.trim().is_empty() {
                self.preview_passport();
            }
        }
        let Some((_, settings, summary)) = &self.import_preview else {
            return;
        };
        let lines = summary.lines();
        let warn = summary.ca_changed;
        let carries_settings = settings.is_some();
        let changes = settings
            .as_ref()
            .map(|incoming| self.draft.send_changes(incoming))
            .unwrap_or_default();
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
        // The import option: the passport's settings come along unless
        // switched off here.
        if carries_settings {
            widgets::setting_row(
                ui,
                true,
                "Einstellungen übernehmen",
                Some(SETTINGS_HELP),
                |ui| {
                    widgets::named_toggle(ui, &mut self.import_settings, "Einstellungen übernehmen")
                },
            );
        }
        // What decides when this device can be heard is spelled out, not
        // hidden in „Einstellungen“ (review 2026-09-27, C-3).
        if self.import_settings && !changes.is_empty() {
            widgets::card_note(ui, |ui| {
                let tok = theme::tokens(ui.visuals());
                ui.label(
                    RichText::new("Ändert, wann und wie du zu hören bist:")
                        .size(11.5)
                        .color(tok.connecting),
                );
                for line in &changes {
                    ui.label(RichText::new(line).size(11.5));
                }
            });
        }
        let replaces = if carries_settings && self.import_settings {
            "Ersetzt Schlüssel, Zertifikat, Serverdaten und Einstellungen dieses Geräts"
        } else {
            "Ersetzt Schlüssel, Zertifikat und Serverdaten dieses Geräts"
        };
        widgets::setting_row(ui, true, "Diesen Server übernehmen?", None, |ui| {
            let width = widgets::button_width(ui, icon::CHECK, "Übernehmen");
            widgets::row_group(ui, width, |ui| {
                if widgets::button(ui, widgets::ButtonKind::Danger, icon::CHECK, "Übernehmen")
                    .on_hover_text(replaces)
                    .clicked()
                {
                    self.import_passport(cmds);
                    self.import_open = false;
                }
            });
        });
    }

    /// The outcome of the last passport copy, save or import, if any.
    fn passport_status_note(&self, ui: &mut Ui) {
        if let Some(status) = &self.passport_status {
            widgets::help_note(ui, status);
        }
    }

    /// Deletes this device's identity and goes back to the invite window.
    fn logout_row(&mut self, ui: &mut Ui) {
        let confirming = self
            .confirm_logout
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        if confirming {
            // Let the armed button fall back on its own after 3 s.
            super::repaint_after(ui.ctx(), Duration::from_millis(250));
        }
        widgets::setting_row(ui, true, "Von diesem Gerät abmelden", None, |ui| {
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
    }

    /// The armored passport for the identity on disk. Disk, not `AppState`:
    /// that one is cloned ten times a second and has no business carrying a
    /// private key. The settings come from the draft, which is ahead of the
    /// debounced `settings.toml`; the flag says whether they fit in.
    fn passport_text(&self) -> Result<(String, bool), String> {
        match Identity::load(&self.paths) {
            Ok(Some(id)) => Ok(id.to_passport_with_settings(&self.draft)),
            Ok(None) => Err("Keine Identität gefunden.".into()),
            Err(e) => {
                tracing::warn!(%e, "identity unreadable");
                Err("Die gespeicherten Zugangsdaten lassen sich nicht lesen.".into())
            }
        }
    }

    /// Reads the pasted text and shows what it would switch to.
    fn preview_passport(&mut self) {
        // `from_passport` also checks key, certificate and names: a passport
        // that cannot work would replace the one that does. A registration
        // bundle is taken as well; the engine registers it under its
        // placeholder name.
        match Identity::from_passport_with_settings(&self.passport_import) {
            Ok((identity, settings)) => {
                // An invitation brings no settings: whatever one carries
                // stays behind, as on the command line and in the
                // first-start window (review 2026-09-27, C-3).
                let settings = settings.filter(|_| !identity.is_registration());
                let current = Identity::load(&self.paths).ok().flatten();
                let summary = config::ImportSummary::of(&identity, current.as_ref());
                self.import_preview = Some((Box::new(identity), settings.map(Box::new), summary));
                self.import_settings = true;
                self.passport_status = None;
            }
            Err(e) => self.passport_status = Some(e),
        }
    }

    /// Takes the confirmed passport into use, with its settings unless the
    /// import option says otherwise.
    fn import_passport(&mut self, cmds: &mut Vec<Command>) {
        if let Some((identity, settings, _)) = self.import_preview.take() {
            let settings = settings.filter(|_| self.import_settings);
            self.passport_status = Some(if settings.is_some() {
                "Wird samt Einstellungen übernommen, ICU verbindet neu …".into()
            } else {
                "Wird übernommen, ICU verbindet neu …".into()
            });
            cmds.push(Command::ReplaceIdentity(identity, settings));
            self.passport_import.clear();
            // The engine saves it first; a failed save keeps the old one
            // (settings included) and says so in the main window's banner.
        }
    }

    fn hotkeys_tab(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        let adv = self.draft.ui.show_advanced;
        widgets::page_header(
            ui,
            "Tastenkürzel",
            "Sprechen, Stummschalten und mehr per Taste – auch wenn ICU im Hintergrund läuft. \
             Änderungen gelten sofort.",
            |_| {},
        );
        let backend = state.hotkey_backend.as_str();
        let backend_before = self.draft.hotkeys.backend;
        hotkey_notice(
            ui,
            backend,
            &mut self.draft.hotkeys.backend,
            &mut self.draft.audio.mode,
            cmds,
        );
        if self.draft.hotkeys.backend != backend_before
            || cmds.iter().any(|cmd| matches!(cmd, Command::RetryHotkeys))
        {
            self.capture = None;
            self.capture_error = None;
        }

        let portal = backend == "portal";
        let capture_label = self.capture.as_ref().map(|c| c.label);
        let capture = self.poll_capture(ui);
        // Complete the previous attempt before handling row actions: a
        // retry clicked in this frame must not be cleared by that result.
        if let Some(result) = &capture {
            self.capture = None;
            self.capture_error = result.as_ref().err().cloned();
        }
        widgets::group_card(ui, "Belegung", |ui| {
            let hotkeys = &mut self.draft.hotkeys;
            use crate::hotkey::Action;
            let rows: [(&'static str, Action, &mut crate::config::KeyBinding); 5] = [
                ("Push-to-Talk", Action::Ptt, &mut hotkeys.ptt),
                ("Stummschalten", Action::Mute, &mut hotkeys.mute),
                ("Ton aus", Action::Deafen, &mut hotkeys.deafen),
                (
                    "Modus wechseln",
                    Action::ToggleMode,
                    &mut hotkeys.toggle_mode,
                ),
                ("Kamera", Action::Camera, &mut hotkeys.camera),
            ];
            for (i, (label, key_action, binding)) in rows.into_iter().enumerate() {
                let capturing = self.capture.as_ref().is_some_and(|c| c.label == label);
                if capture_label == Some(label) {
                    if let Some(Ok(result)) = &capture {
                        result.apply(binding);
                        self.capture_error = result.portal_warning(portal);
                    }
                }
                let action = binding_row(
                    ui,
                    i > 0,
                    label,
                    key_action,
                    binding,
                    portal,
                    capturing,
                    adv,
                    &mut self.expanded_bindings,
                );
                match action {
                    BindingAction::None => {}
                    BindingAction::Record => {
                        self.capture_error = None;
                        self.capture = Some(Capture::start(label));
                    }
                    BindingAction::Cancel => {
                        self.capture = None;
                        self.capture_error = None;
                    }
                    BindingAction::Clear => {
                        *binding = crate::config::KeyBinding::default();
                        self.capture_error = None;
                    }
                }
            }
            if let Some(err) = &self.capture_error {
                widgets::card_note(ui, |ui| {
                    ui.colored_label(
                        theme::tokens(ui.visuals()).connecting,
                        RichText::new(err).size(11.5),
                    );
                });
            }
        });

        if adv {
            widgets::group_card(ui, "Tasten lesen über", |ui| {
                let linux = cfg!(target_os = "linux");
                if linux {
                    let changed = widgets::setting_row(ui, false, "Methode", None, |ui| {
                        widgets::segmented(
                            ui,
                            &mut self.draft.hotkeys.backend,
                            &[
                                (HotkeyBackend::Auto, "Automatisch"),
                                (HotkeyBackend::Portal, "Portal"),
                                (HotkeyBackend::Evdev, "evdev"),
                            ],
                        )
                    })
                    .changed();
                    if changed {
                        self.capture = None;
                        self.capture_error = None;
                    }
                }
                widgets::setting_row(ui, linux, "Aktiv", None, |ui| {
                    let tok = theme::tokens(ui.visuals());
                    let (color, text) = match backend {
                        "" => (tok.connecting, "abgeschaltet"),
                        crate::hotkey::BACKEND_STARTING => (tok.connecting, "wird eingerichtet"),
                        crate::hotkey::BACKEND_NONE => (tok.error, "keiner"),
                        crate::hotkey::BACKEND_PORTAL_DECLINED => (tok.connecting, "abgelehnt"),
                        backend => (tok.connected, backend),
                    };
                    widgets::status_pill(ui, color, text)
                });
                widgets::help_note(
                    ui,
                    if linux {
                        "Tastennamen: evdev wie KEY_RIGHTCTRL, BTN_SIDE oder code:97; \
                         Portal-Trigger wie CTRL+SHIFT+F12."
                    } else {
                        "Tastennamen wie KEY_RIGHTCTRL, BTN_SIDE (Maus-Seitentaste) oder \
                         code:97."
                    },
                );
            });
        }
        if cfg!(target_os = "linux") && (adv || self.draft.hotkeys.backend == HotkeyBackend::Evdev)
        {
            evdev_notice(ui);
        }
        if cfg!(target_os = "windows") {
            elevated_notice(ui);
        }
    }

    /// The running „Aufnehmen", if its answer is in ([`Capture::poll`]).
    fn poll_capture(&mut self, ui: &Ui) -> Option<Result<Captured, String>> {
        self.capture.as_mut()?.poll(ui)
    }

    #[cfg_attr(not(feature = "camera"), allow(unused_variables))]
    fn camera_tab(&mut self, ui: &mut Ui, state: &AppState) {
        widgets::page_header(
            ui,
            "Kamera",
            "Deine Kamera auswählen, einschalten und das Bild prüfen.",
            |_| {},
        );
        widgets::group_card(ui, "Kamera", |ui| {
            // Keep the preview alongside the controls where it fits;
            // compact windows put it underneath instead of squeezing the
            // picker beyond its column.
            let stacked = ui.available_width() < 500.0 || !cfg!(feature = "camera");
            let layout = if stacked {
                egui::Layout::top_down(egui::Align::Min)
            } else {
                egui::Layout::left_to_right(egui::Align::Min)
            };
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 0.0), layout, |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let left = if stacked {
                    ui.available_width()
                } else {
                    ui.available_width() - PREVIEW_W - 24.0
                };
                ui.vertical(|ui| {
                    ui.set_width(left);
                    #[cfg(feature = "camera")]
                    {
                        if self.cameras_stale {
                            // Opens every `/dev/video*` in turn, so: once per
                            // visit to this tab, never per frame.
                            self.cameras_stale = false;
                            self.cameras = crate::camera::list();
                        }
                        widgets::setting_row(ui, false, "Gerät", None, |ui| {
                            camera_combo(ui, &mut self.draft.camera.device, &self.cameras)
                        });
                    }
                    widgets::setting_row(
                        ui,
                        cfg!(feature = "camera"),
                        "Kamera einschalten",
                        None,
                        |ui| {
                            ui.add_enabled_ui(!state.privacy_active, |ui| {
                                widgets::named_toggle(
                                    ui,
                                    &mut self.draft.camera.enabled,
                                    "Kamera einschalten",
                                )
                            })
                            .inner
                            .on_disabled_hover_text("Während Privacy gesperrt.")
                        },
                    );
                });
                #[cfg(feature = "camera")]
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        // One column: the picture, the button under it.
                        ui.vertical(|ui| {
                            let live = self
                                .preview
                                .as_mut()
                                .is_some_and(|preview| preview.ui(ui, PREVIEW_W));
                            if !live {
                                preview_placeholder(
                                    ui,
                                    PREVIEW_W,
                                    state.camera_on || self.camera_preview,
                                );
                            }
                            // Checking the picture before anyone sees it: the
                            // preview captures without sending
                            // (`CameraHandle::set_preview`), and ends with the
                            // page.
                            if !state.camera_on {
                                ui.add_space(6.0);
                                let (glyph, label) = if self.camera_preview {
                                    (icon::X, "Vorschau beenden")
                                } else {
                                    (icon::EYE, "Vorschau")
                                };
                                let r = ui
                                    .add_enabled_ui(!state.privacy_active, |ui| {
                                        ui.add_sized(
                                            [PREVIEW_W, 28.0],
                                            egui::Button::new(format!("{glyph}  {label}")),
                                        )
                                    })
                                    .inner
                                    .on_disabled_hover_text("Während Privacy gesperrt.");
                                if r.on_hover_text("Nur du siehst das Bild; gesendet wird nichts")
                                    .clicked()
                                {
                                    self.camera_preview = !self.camera_preview;
                                }
                            }
                        });
                    });
            });
            if state.privacy_active {
                widgets::help_note(
                    ui,
                    "Privacy ist an: Einschalten und Vorschau sind gesperrt. Eine aktive Kamera zeigt TV-Rauschen. Beende Privacy im Hauptfenster, um die Kamera wieder zu nutzen.",
                );
            }
        });
    }

    fn overlay_tab(&mut self, ui: &mut Ui) {
        let adv = self.draft.ui.show_advanced;
        widgets::page_header(
            ui,
            "Overlay",
            "Teilnehmer-Kacheln, die über anderen Fenstern schweben.",
            |_| {},
        );
        widgets::group_card(ui, "Anzeige", |ui| {
            if !self.overlay_available {
                widgets::help_note(
                    ui,
                    if cfg!(target_os = "linux") {
                        "Das Overlay braucht eine Wayland-Sitzung, deren Arbeitsumgebung \
                         Fenster über Spielen erlaubt (Layer-Shell, etwa KDE Plasma oder \
                         Sway). Hier gibt es das nicht — unter X11 nie, unter GNOME ebenso \
                         wenig. Die Kameras erscheinen dann nur in der Teilnehmerliste."
                    } else {
                        "Das Overlay ließ sich hier nicht starten. Die Kameras erscheinen \
                         dann nur in der Teilnehmerliste."
                    },
                );
                return;
            }
            // Where it shows sits beside the switch: a row of its own would
            // push this page past the screen once the sliders are there.
            widgets::setting_row(
                ui,
                false,
                "Anzeigen",
                Some(
                    "Über allen Fenstern; über Spielen im Fenster- oder randlosen Modus, \
                     nicht im exklusiven Vollbild.",
                ),
                |ui| {
                    let toggle =
                        widgets::named_toggle(ui, &mut self.draft.overlay.enabled, "Anzeigen");
                    ui.add_space(16.0);
                    ui.add_enabled_ui(self.draft.overlay.enabled, |ui| {
                        widgets::row_group(ui, CORNER_PICKER_W, |ui| {
                            corner_picker(ui, &mut self.draft.overlay.corner)
                        })
                    })
                    .response
                    .on_hover_text("Position");
                    toggle
                },
            );
            ui.add_enabled_ui(self.draft.overlay.enabled, |ui| {
                widgets::setting_row(ui, true, "Mich im Overlay zeigen", None, |ui| {
                    widgets::named_toggle(
                        ui,
                        &mut self.draft.overlay.show_self,
                        "Mich im Overlay zeigen",
                    )
                })
                .on_hover_text(
                    "Deine eigene Kachel erscheint im Overlay nach allen anderen: \
                     dein Kamerabild, ohne Kamera ein Kreis mit deinen Initialen",
                );
                widgets::setting_row(ui, true, "Teilnehmer ohne Kamera", None, |ui| {
                    widgets::named_toggle(
                        ui,
                        &mut self.draft.overlay.voice_plates,
                        "Teilnehmer ohne Kamera",
                    )
                })
                .on_hover_text("Teilnehmer ohne Kamera bleiben im Overlay als kleinere Kreise mit Initialen und Namen sichtbar und werden beim Sprechen hervorgehoben");
                // The camera tile's diameter, silent and speaking; plates and
                // names follow it. Until its slider is touched the speaking
                // size stays 20 % above the other one.
                let sizes = *TILE_SIZES.start() as f32..=*TILE_SIZES.end() as f32;
                let mut size = self.draft.overlay.size as f32;
                widgets::slider_row(
                    ui,
                    true,
                    "Inaktive Sprecher",
                    &mut size,
                    sizes.clone(),
                    " px",
                )
                .on_hover_text("Durchmesser einer Kachel, solange die Person schweigt");
                self.draft.overlay.size = size.round() as u32;
                let mut speaking = self.draft.overlay.speaking_diameter() as f32;
                if widgets::slider_row(ui, true, "Aktive Sprecher", &mut speaking, sizes, " px")
                    .on_hover_text("Durchmesser einer Kachel, während die Person spricht")
                    .changed()
                {
                    self.draft.overlay.speaking_size = Some(speaking.round() as u32);
                }
                // One distance for both edges of a corner; separate ones
                // from an older file stay until the slider is touched.
                let [x, y] = self.draft.overlay.offset;
                let mut edge = x.max(y).min(EDGE_MAX) as f32;
                if widgets::slider_row(
                    ui,
                    true,
                    "Abstand zum Rand",
                    &mut edge,
                    0.0..=EDGE_MAX as f32,
                    " px",
                )
                .on_hover_text("Wie weit die Kacheln vom Bildschirmrand entfernt bleiben")
                .changed()
                {
                    let edge = edge.round() as u32;
                    self.draft.overlay.offset = [edge, edge];
                }
                // Keep the corona and the own color together in the everyday
                // settings, without adding a separate color row. The switch
                // only decides whether this overlay draws coronas; the color
                // is how everybody else sees this user, so it stays usable.
                widgets::setting_row(ui, true, "Sprech-Corona", None, |ui| {
                    let toggle = widgets::named_toggle(
                        ui,
                        &mut self.draft.overlay.speaking_frame,
                        "Sprech-Corona",
                    );
                    let own = &mut self.draft.overlay.frame_color;
                    if *own != crate::config::DEFAULT_FRAME_COLOR {
                        let reset = ui.small_button(icon::ARROW_COUNTER_CLOCKWISE);
                        reset.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Standardfarbe")
                        });
                        if reset.on_hover_text("Zurück zur Standardfarbe").clicked() {
                            *own = crate::config::DEFAULT_FRAME_COLOR;
                        }
                    }
                    let color = ui.color_edit_button_srgb(own);
                    color.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::ColorButton,
                            true,
                            "Meine Coronafarbe",
                        )
                    });
                    color.on_hover_text(
                        "Meine Coronafarbe – so sehen mich alle im Raum, wenn ich spreche",
                    );
                    toggle
                });
                if !adv {
                    return;
                }
                widgets::advanced_caption(ui);
                let edge = matches!(self.draft.overlay.corner, Corner::Top | Corner::Bottom);
                widgets::setting_row(ui, true, "Anordnung", None, |ui| {
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
                widgets::setting_row(ui, true, "Stille Personen abdunkeln", None, |ui| {
                    widgets::named_toggle(
                        ui,
                        &mut self.draft.overlay.dim_non_speaking,
                        "Stille Personen abdunkeln",
                    )
                });
                widgets::setting_row(ui, true, "Dem Mauszeiger ausweichen", None, |ui| {
                    widgets::named_toggle(
                        ui,
                        &mut self.draft.overlay.shy,
                        "Dem Mauszeiger ausweichen",
                    )
                })
                .on_hover_text("Liegt der Mauszeiger auf einer Kachel, blendet sich die ganze Reihe aus und auf der gegenüberliegenden Bildschirmseite wieder ein; Klicks gehen zum Fenster darunter durch");
                if self.draft.overlay.shy {
                    ui.separator();
                    ui.columns(2, |columns| {
                        let holds = SHY_HOLDS;
                        let mut hold = self.draft.overlay.shy_hold_ms as f32;
                        if widgets::slider_row(
                            &mut columns[0],
                            false,
                            "Wechselsperre",
                            &mut hold,
                            *holds.start() as f32..=*holds.end() as f32,
                            " ms",
                        )
                        .on_hover_text("So lange bleibt die Reihe nach dem Erscheinen oder einem Seitenwechsel stehen, auch unter dem Mauszeiger, bevor sie wieder ausweicht; Klicks gehen in der Zeit durch")
                        .changed()
                        {
                            self.draft.overlay.shy_hold_ms = hold.round() as u32;
                        }
                        let mut delay = self.draft.overlay.shy_return_ms as f32 / 1000.0;
                        if widgets::slider_row(
                            &mut columns[1],
                            false,
                            "Zurück nach",
                            &mut delay,
                            *SHY_RETURNS.start() as f32 / 1000.0..=*SHY_RETURNS.end() as f32 / 1000.0,
                            " s",
                        )
                        .on_hover_text("So lange wartet die Reihe auf der Gegenseite ohne Mauszeiger darüber, bevor sie an die eingestellte Position zurückkehrt; Standard: 3 Sekunden")
                        .changed()
                        {
                            self.draft.overlay.shy_return_ms = (delay * 1000.0).round() as u32;
                        }
                    });
                }
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
            "Dein Name, wie ICU aussieht und was dieses Gerät betrifft.",
            |_| {},
        );
        widgets::group_card(ui, "Du und ICU", |ui| {
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
                                    .margin(FIELD_MARGIN)
                                    .hint_text("Name"),
                            )
                        })
                        .inner;
                    let entered =
                        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let changed = icu_proto::normalize_name(&self.name)
                        != state.me.as_ref().map_or("", |m| &m.name);
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
                        cmds.push(Command::RequestRename(icu_proto::normalize_name(
                            &self.name,
                        )));
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
                    "Der Name steht im Zertifikat: eine Änderung stellt ein neues aus. \
                     Geräte mit einem älteren Passport brauchen danach einen neuen.",
                );
                widgets::setting_row(ui, true, "Server", None, |ui| {
                    widgets::chip(ui, &state.server, 0.0)
                });
                widgets::setting_row(ui, true, "Nutzer-ID", None, |ui| {
                    let id = state.me.as_ref().map(|m| m.user_id.to_string());
                    widgets::chip(ui, id.as_deref().unwrap_or("—"), 0.0)
                });
                // A desktop-integration knob, on by default: here rather
                // than among the everyday rows, which „Beim Schließen“ fills.
                widgets::setting_row(
                    ui,
                    true,
                    "Einladungen mit ICU öffnen",
                    Some(
                        "Ein Doppelklick auf eine Einladung oder einen Passport (.icu) öffnet ICU.",
                    ),
                    |ui| {
                        widgets::named_toggle(
                            ui,
                            &mut self.draft.ui.register_uri_handler,
                            "Einladungen mit ICU öffnen",
                        )
                    },
                );
                ui.separator();
            }
            widgets::setting_row(ui, !adv, "Farben", None, |ui| {
                let mut theme = match self.draft.ui.theme.as_str() {
                    "light" => "light",
                    "dark" => "dark",
                    _ => "system",
                };
                let r = widgets::segmented(
                    ui,
                    &mut theme,
                    &[
                        ("system", "Wie das System"),
                        ("light", "Hell"),
                        ("dark", "Dunkel"),
                    ],
                );
                if r.changed() {
                    self.draft.ui.theme = theme.to_string();
                }
                r
            });
            widgets::setting_row(ui, true, "Beim Schließen", None, |ui| {
                let mut action = match self.draft.ui.close_action.as_str() {
                    "minimize" => "minimize",
                    "quit" => "quit",
                    _ => "tray",
                };
                let r = widgets::segmented(
                    ui,
                    &mut action,
                    &[
                        ("tray", "Infobereich"),
                        ("minimize", "Minimieren"),
                        ("quit", "Beenden"),
                    ],
                );
                if r.changed() {
                    self.draft.ui.close_action = action.to_string();
                }
                r
            });
            if self.draft.ui.close_action == "tray" && !self.tray_available {
                widgets::help_note(
                    ui,
                    "Hier kann ICU nicht in den Infobereich (Wayland oder kein \
                     Infobereich, unter GNOME z. B. ohne die Erweiterung \
                     „AppIndicator“) und minimiert stattdessen in die Taskleiste.",
                );
            }
            widgets::setting_row(
                ui,
                true,
                "Minimiert starten",
                Some("ICU startet im Infobereich, wo es keinen gibt, minimiert in der Taskleiste."),
                |ui| {
                    widgets::named_toggle(
                        ui,
                        &mut self.draft.ui.start_minimized,
                        "Minimiert starten",
                    )
                },
            );
        });

        self.device_card(ui, cmds);
    }

    /// Admin-only "Nutzer" tab: inviting, and managing who is already in
    /// (`ControlMsg::UserListRequest`/`AdminSetRole`/`AdminSetDisabled`/
    /// `AdminRevokeUser`, `docs/reference/protocol.md` §2.2).
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
                    let copied = copy_secret(ui.ctx(), bundle.to_string());
                    self.invite_status = Some(copied_status(copied));
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
        super::repaint_after(ui.ctx(), every);

        let me = state.me.as_ref().map(|m| m.user_id);
        // A list too long for one frame ends in a row with `user_id` 0 that
        // only counts the rest (`docs/reference/protocol.md` §2.2 Nr. 27): no member, no
        // buttons.
        let (more, listed): (Vec<_>, Vec<_>) = state.user_list.iter().partition(|u| u.user_id == 0);
        let (removed, rest): (Vec<_>, Vec<_>) = listed.into_iter().partition(|u| u.revoked);
        let (mut pending, rest): (Vec<_>, Vec<_>) = rest.into_iter().partition(|u| u.pending);
        // The ban list: blocked, but not removed for good.
        let (mut banned, mut members): (Vec<_>, Vec<_>) =
            rest.into_iter().partition(|u| u.disabled);
        banned.sort_by_cached_key(|u| u.name.to_lowercase());
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
            for row in &more {
                ui.separator();
                widgets::help_note(ui, &row.name);
            }
        });

        if !banned.is_empty() {
            widgets::group_card(ui, &format!("Gesperrt · {}", banned.len()), |ui| {
                for (i, user) in banned.iter().enumerate() {
                    self.member_row(ui, i > 0, user, false, cmds);
                }
            });
        }

        if !pending.is_empty() {
            widgets::group_card(
                ui,
                &format!("Offene Einladungen · {}", pending.len()),
                |ui| {
                    for (i, user) in pending.iter().enumerate() {
                        self.member_row(ui, i > 0, user, false, cmds);
                    }
                },
            );
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
            header.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    open,
                    "Entfernte Zugänge",
                )
            });
            widgets::focus_ring(ui, &header, theme::R_CHIP);
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

    /// One person on one line: avatar with presence dot, name with role
    /// badges, a quiet status on the right, and everything you can do to them
    /// behind one "⋯" menu — or, while a removal waits for its confirmation,
    /// that confirmation.
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
            .inner_margin(egui::Margin::symmetric(12, 5))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let avatar =
                        widgets::avatar(ui, &user.name, user.user_id, false, None, 26.0, None);
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
                    if user.muted {
                        widgets::badge(ui, "Stumm", widgets::BadgeKind::Danger)
                            .on_hover_text("Von einem Admin stummgeschaltet");
                    }
                    if user.deafened {
                        widgets::badge(ui, "Taub", widgets::BadgeKind::Danger)
                            .on_hover_text("Von einem Admin taubgeschaltet");
                    }
                    let since = short_date(&user.created);
                    let status = if user.pending {
                        format!("eingeladen am {since}")
                    } else if user.online {
                        "online".to_string()
                    } else {
                        format!("offline · dabei seit {since}")
                    };
                    widgets::rtl(ui, 30.0, |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let status = |ui: &mut Ui| {
                            ui.label(RichText::new(&status).size(11.5).color(tok.dim_text));
                        };
                        if is_self {
                            status(ui);
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
                            status(ui);
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
                                let (glyph, label) = if user.muted {
                                    (icon::MICROPHONE, "Stummschaltung aufheben")
                                } else {
                                    (icon::MICROPHONE_SLASH, "Stummschalten")
                                };
                                if widgets::menu_item(ui, glyph, label, false).clicked() {
                                    cmds.push(Command::SetUserRestriction {
                                        user_id: user.user_id,
                                        muted: !user.muted,
                                        deafened: user.deafened,
                                    });
                                    cmds.push(Command::RequestUserList);
                                }
                                let (glyph, label) = if user.deafened {
                                    (icon::SPEAKER_HIGH, "Taubschaltung aufheben")
                                } else {
                                    (icon::SPEAKER_SLASH, "Taubschalten")
                                };
                                if widgets::menu_item(ui, glyph, label, false).clicked() {
                                    cmds.push(Command::SetUserRestriction {
                                        user_id: user.user_id,
                                        muted: user.muted,
                                        deafened: !user.deafened,
                                    });
                                    cmds.push(Command::RequestUserList);
                                }
                                if user.online
                                    && widgets::menu_item(ui, icon::SIGN_OUT, "Rauswerfen", false)
                                        .clicked()
                                {
                                    cmds.push(Command::KickUser {
                                        user_id: user.user_id,
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
                        status(ui);
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
                            ui.label(
                                RichText::new(icon::CHECK_CIRCLE)
                                    .size(22.0)
                                    .color(tok.connected),
                            );
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
                                let copied = copy_secret(ui.ctx(), bundle.clone());
                                self.invite_status = Some(copied_status(copied));
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
                            .margin(FIELD_MARGIN)
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
            widgets::setting_row(ui, true, "FEC-Auslegung", None, |ui| {
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

/// What the import option takes along, next to its switch in the settings
/// window and the first-start window (`Settings::take_portable`).
pub(crate) const SETTINGS_HELP: &str =
    "Audio, Klang, Tastenkürzel, Overlay, Lautstärken und Aussehen von dort; Geräte und \
     Kamera bleiben, wie sie hier sind.";

/// The export status' note when the settings could not come along (they
/// would not fit into the passport's 8 KiB).
fn without_settings(with_settings: bool) -> &'static str {
    if with_settings {
        ""
    } else {
        " Ohne Einstellungen: dieser Passport kann sie nicht mitnehmen."
    }
}

/// What „Speichern“ does with the passport, true on each platform: only
/// Unix has the file mode that keeps other accounts out (Windows: the
/// profile's ACL, and DPAPI seals `key.pk8` alone), and no mode keeps a
/// synced folder from uploading it (review 2026-09-27, C-6).
#[cfg(unix)]
const SAVE_HELP: &str = "Als .icu-Datei samt Einstellungen in den Download-Ordner, nur für dich \
     lesbar — auf dem anderen Gerät doppelklicken. Nicht in einem synchronisierten Ordner \
     (Nextcloud, Dropbox …) liegen lassen: die Datei enthält deinen privaten Schlüssel.";
#[cfg(not(unix))]
const SAVE_HELP: &str = "Als .icu-Datei samt Einstellungen in den Download-Ordner deines \
     Benutzerkontos — auf dem anderen Gerät doppelklicken. Nicht in einem synchronisierten \
     Ordner (OneDrive …) liegen lassen: die Datei enthält deinen privaten Schlüssel, \
     unverschlüsselt.";

/// How long a copied passport or invitation may stay on the clipboard.
const PASSPORT_CLIPBOARD: std::time::Duration = std::time::Duration::from_secs(60);

/// How a secret reached the clipboard ([`copy_secret`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretCopy {
    /// Windows or X11: marked for clipboard managers to leave out of their
    /// history (Windows: also out of the cloud clipboard), and taken off
    /// again after [`PASSPORT_CLIPBOARD`].
    Marked,
    /// Windows or X11 without the mark (the clipboard would not take it):
    /// still taken off again.
    Plain,
    /// Native Wayland: through the compositor, unmarked, and whether ICU
    /// can take it off again is up to the compositor.
    Wayland,
}

/// Puts a passport or an invitation — a key that lets anybody in — onto the
/// clipboard, and takes it off again after [`PASSPORT_CLIPBOARD`]. Marked as
/// a secret where the platform has a way: on Windows the formats that keep
/// it out of clipboard history, the cloud clipboard and clipboard monitors,
/// under X11 the `x-kde-passwordManagerHint` that Klipper and others
/// honour. egui's clipboard has neither under native Wayland (review
/// 2026-09-27, C-7), so there the status says what ICU cannot promise.
fn copy_secret(ctx: &egui::Context, text: String) -> SecretCopy {
    let copied = if super::native_wayland() {
        ctx.copy_text(text.clone());
        SecretCopy::Wayland
    } else {
        match set_marked(&text) {
            Ok(()) => SecretCopy::Marked,
            Err(e) => {
                tracing::info!(%e, "clipboard mark not set, copying plainly");
                ctx.copy_text(text.clone());
                SecretCopy::Plain
            }
        }
    };
    clear_clipboard_later(text);
    copied
}

/// Sets `text` with the platform's "do not keep" marks. A handle of its
/// own is enough: egui's handle keeps arboard's process-wide X11 owner
/// alive, and Windows keeps the data itself.
fn set_marked(text: &str) -> Result<(), arboard::Error> {
    let mut clipboard = arboard::Clipboard::new()?;
    let set = clipboard.set();
    #[cfg(windows)]
    let set = {
        use arboard::SetExtWindows;
        set.exclude_from_monitoring()
            .exclude_from_cloud()
            .exclude_from_history()
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let set = {
        use arboard::SetExtLinux;
        set.exclude_from_history()
    };
    set.text(text)
}

/// The status line after [`copy_secret`]: „für 60 s“ only where ICU takes
/// it off again for sure.
fn copied_status(copied: SecretCopy) -> String {
    match copied {
        SecretCopy::Marked | SecretCopy::Plain => format!(
            "In die Zwischenablage kopiert, für {} s.",
            PASSPORT_CLIPBOARD.as_secs()
        ),
        SecretCopy::Wayland => "In die Zwischenablage kopiert. Unter Wayland kann ICU sie nicht \
                                sicher wieder leeren, und ein Zwischenablage-Verlauf behält den \
                                Text – nach dem Einfügen bitte etwas anderes kopieren."
            .into(),
    }
}

/// Takes `secret` off the clipboard after [`PASSPORT_CLIPBOARD`] — only
/// if it is still what the clipboard holds; whatever was copied since stays.
/// Its own thread and its own clipboard handle, so it happens with the
/// settings closed and the window in the tray. X11 and Windows clear for
/// real; under Wayland this goes through Xwayland, and whether that reaches
/// the compositor's clipboard is up to the compositor (an unfocused Wayland
/// client cannot set the selection itself).
fn clear_clipboard_later(secret: String) {
    let spawned = std::thread::Builder::new()
        .name("icu-clipboard".into())
        .spawn(move || {
            std::thread::sleep(PASSPORT_CLIPBOARD);
            let cleared = arboard::Clipboard::new().and_then(|mut clipboard| {
                if clipboard.get_text()? != secret {
                    return Ok(false);
                }
                clipboard.clear()?;
                Ok(true)
            });
            match cleared {
                Ok(true) => tracing::info!("secret taken off the clipboard"),
                Ok(false) => {}
                Err(e) => tracing::info!(%e, "clipboard not checked for the secret"),
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(%e, "no clipboard thread");
    }
}

/// Where „Speichern" puts the passport: the download folder, where a file
/// to be carried to another device is looked for first; the home directory
/// or the config directory without one. `.icu`, so a double-click on the
/// other device opens it in ICU.
fn passport_file() -> std::path::PathBuf {
    let dir = directories::UserDirs::new()
        .and_then(|u| {
            u.download_dir()
                .map(std::path::Path::to_path_buf)
                .or_else(|| Some(u.home_dir().to_path_buf()))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    dir.join("icu-passport.icu")
}

/// `2026-09-24T…` → `24.09.2026`; anything unexpected comes back as is.
fn short_date(rfc3339: &str) -> String {
    match rfc3339
        .get(..10)
        .map(|d| d.split('-').collect::<Vec<_>>())
        .as_deref()
    {
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
        .width(200.0_f32.min(ui.available_width()))
        .truncate()
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
            let label = match value {
                Corner::TopLeft => "Oben links",
                Corner::TopRight => "Oben rechts",
                Corner::BottomLeft => "Unten links",
                Corner::BottomRight => "Unten rechts",
                Corner::Top => "Oben mittig",
                Corner::Bottom => "Unten mittig",
            };
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::RadioButton,
                    ui.is_enabled(),
                    selected,
                    label,
                )
            });
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
            widgets::focus_ring(ui, &resp, theme::R_CTRL);
            resp.on_hover_text(label);
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

/// What the Tastenkürzel page and the setup say while the hotkeys do not
/// work: still being set up, never started, the portal's dialog turned down
/// (with the two ways on), or no way to read keys at all (with what to do,
/// and the way around it). Nothing while they work. `backend` is what
/// `crate::hotkey` reports — "starting", "portal", "evdev (n devices)",
/// "portal-declined" or "none"; the empty string means it was never started
/// (`--no-hotkeys`). `choice` is the draft's backend setting, `mode` its
/// sending mode.
pub(super) fn hotkey_notice(
    ui: &mut Ui,
    backend: &str,
    choice: &mut HotkeyBackend,
    mode: &mut ModeSetting,
    cmds: &mut Vec<Command>,
) {
    match backend {
        crate::hotkey::BACKEND_STARTING => {
            widgets::banner(
                ui,
                widgets::BannerKind::Warning,
                "Tastenkürzel werden eingerichtet",
                if cfg!(target_os = "linux") {
                    "Fragt die Arbeitsumgebung in einem eigenen Dialog nach, dort bestätigen — \
                     danach wirken die Tasten."
                } else {
                    "Einen Moment, danach wirken die Tasten."
                },
                None::<fn()>,
            );
            ui.add_space(12.0);
        }
        "" => {
            widgets::banner(
                ui,
                widgets::BannerKind::Warning,
                "Tastenkürzel sind abgeschaltet",
                "ICU wurde ohne Tastenkürzel gestartet. Bedienen geht weiter über das \
                 Hauptfenster.",
                None::<fn()>,
            );
            ui.add_space(12.0);
        }
        crate::hotkey::BACKEND_PORTAL_DECLINED => {
            widgets::banner(
                ui,
                widgets::BannerKind::Warning,
                "Tastenkürzel wurden abgelehnt",
                "Der Dialog der Arbeitsumgebung wurde abgebrochen oder ohne Taste bestätigt. \
                 ICU kann erneut fragen oder die Tasten selbst lesen.",
                None::<fn()>,
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if widgets::button(
                    ui,
                    widgets::ButtonKind::Primary,
                    icon::ARROW_CLOCKWISE,
                    "Erneut fragen",
                )
                .clicked()
                {
                    cmds.push(Command::RetryHotkeys);
                }
                if widgets::button(
                    ui,
                    widgets::ButtonKind::Neutral,
                    icon::COMMAND,
                    "Tasten direkt lesen (evdev)",
                )
                .on_hover_text("Liest jeden Tastendruck mit; braucht die Gruppe input")
                .clicked()
                {
                    *choice = HotkeyBackend::Evdev;
                }
            });
            ui.add_space(12.0);
        }
        crate::hotkey::BACKEND_NONE => {
            widgets::banner(
                ui,
                widgets::BannerKind::Error,
                "Tastenkürzel funktionieren gerade nicht",
                if cfg!(target_os = "linux") {
                    "Das System bietet keinen Weg an, Tasten im Hintergrund zu lesen, und ICU \
                     darf sie nicht selbst lesen. Abhilfe: im Terminal\n\
                     sudo usermod -aG input $USER\n\
                     eingeben, dann ab- und wieder anmelden (ICU kann dann jede Taste \
                     mitlesen). Oder ohne Taste senden; bedienen geht auch über das \
                     Hauptfenster."
                } else {
                    "ICU kann Tastatur und Maus gerade nicht lesen, oder es ist keine Taste \
                     belegt. Bedienen geht weiter über das Hauptfenster."
                },
                None::<fn()>,
            );
            if *mode == ModeSetting::Ptt {
                ui.add_space(6.0);
                if widgets::button(
                    ui,
                    widgets::ButtonKind::Primary,
                    icon::WAVEFORM,
                    "Stattdessen automatisch senden",
                )
                .on_hover_text("Sprachaktivierung: du bist zu hören, sobald du sprichst")
                .clicked()
                {
                    *mode = ModeSetting::Vad;
                }
            }
            ui.add_space(12.0);
        }
        _ => {}
    }
}

/// Windows keeps keys pressed in a program run as administrator from
/// every normal one (UIPI): push-to-talk goes deaf while such a game has
/// the focus.
pub(super) fn elevated_notice(ui: &mut Ui) {
    widgets::help_note(
        ui,
        "Läuft ein Spiel als Administrator, sieht ICU dessen Tasten nicht, solange es im \
         Vordergrund ist — dann ICU ebenfalls als Administrator starten.",
    );
}

/// What choosing evdev costs, wherever it can be chosen.
pub(super) fn evdev_notice(ui: &mut Ui) {
    widgets::banner(
        ui,
        widgets::BannerKind::Warning,
        "evdev liest alle Tasten",
        "evdev braucht die Gruppe input und liest technisch jeden Tastendruck mit, \
         nicht nur die hier belegten.",
        None::<fn()>,
    );
}

/// „Testen“, or „Beenden“ while `on`: the mic test, which plays the own
/// voice the way the others get it, after the whole send chain.
pub(super) fn mic_test_button(ui: &mut Ui, on: bool, cmds: &mut Vec<Command>) {
    let (kind, glyph, label) = if on {
        (widgets::ButtonKind::Primary, icon::STOP, "Beenden")
    } else {
        (widgets::ButtonKind::Neutral, icon::HEADPHONES, "Testen")
    };
    if widgets::button(ui, kind, glyph, label)
        .on_hover_text(
            "Mikrofontest: du hörst dich so, wie die anderen dich hören. \
             Endet von selbst nach einer Minute.",
        )
        .clicked()
    {
        cmds.push(Command::SetMicTest(!on));
    }
}

/// A device choice; `extra` goes left of the list, in the same row.
pub(super) fn device_combo(
    ui: &mut Ui,
    divider: bool,
    label: &str,
    selected: &mut Option<String>,
    devices: &[&crate::audio::DeviceInfo],
    extra: impl FnOnce(&mut Ui),
) {
    let (current, hint) = device_choice_text(selected.as_deref(), devices);
    widgets::setting_row(ui, divider, label, None, |ui| {
        let combo = egui::ComboBox::from_id_salt(label)
            .selected_text(current)
            .width(240.0)
            .truncate()
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(selected.is_none(), "System-Standard")
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
        if let Some(hint) = hint {
            combo.response.on_hover_text(hint);
        }
        extra(ui);
    });
}

/// Match the engine's device choice without claiming that opening it
/// succeeded. With an empty list, the engine keeps the saved device.
fn device_choice_text(
    selected: Option<&str>,
    devices: &[&crate::audio::DeviceInfo],
) -> (String, Option<String>) {
    let Some(id) = selected else {
        return ("System-Standard".into(), None);
    };
    if let Some(device) = devices.iter().find(|d| d.id == id) {
        return (device.description.clone(), None);
    }
    if devices.is_empty() {
        (
            "Auswahl nicht bestätigt".into(),
            Some(format!(
                "Das Audiosystem meldet keine Geräte dieses Typs. ICU behält die Auswahl \
                 „{id}“ bei; ein Wechsel zum System-Standard ist nicht bestätigt."
            )),
        )
    } else {
        (
            "Fehlt – System-Standard gewählt".into(),
            Some(format!(
                "„{id}“ ist nicht in der Geräteliste. ICU wählt vorübergehend den \
                 System-Standard und kehrt zurück, sobald das Gerät wieder verfügbar ist."
            )),
        )
    }
}

/// A running „Aufnehmen" for one binding row.
pub(super) struct Capture {
    /// Which row (its label).
    pub(super) label: &'static str,
    started: Instant,
    /// Where the device reader drops its answer
    /// (`crate::hotkey::capture_next_key`).
    slot: Arc<std::sync::Mutex<Option<Result<String, String>>>>,
    /// Why the device reader could not help (not in group `input`); the
    /// window's own key events are then the only source.
    evdev_failed: Option<String>,
    /// A key this window saw, and when.
    window: Option<(Captured, Instant)>,
    /// Closing this channel on drop stops the native reader as well as
    /// discarding its eventual result.
    _cancel: tokio::sync::oneshot::Sender<()>,
}

async fn capture_until_cancelled(
    cancelled: tokio::sync::oneshot::Receiver<()>,
    capture: impl std::future::Future<Output = Result<String, String>>,
) -> Option<Result<String, String>> {
    tokio::select! {
        biased;
        _ = cancelled => None,
        result = capture => Some(result),
    }
}

impl Capture {
    /// Starts reading the input devices on a thread of its own: the UI
    /// thread has no runtime, and opening `/dev/input/*` blocks.
    pub(super) fn start(label: &'static str) -> Capture {
        let slot: Arc<std::sync::Mutex<Option<Result<String, String>>>> = Arc::default();
        let out = slot.clone();
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("icu-key-capture".into())
            .spawn(move || {
                let result = match tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .enable_io()
                    .build()
                {
                    Ok(rt) => rt.block_on(capture_until_cancelled(
                        cancelled,
                        crate::hotkey::capture_next_key(Duration::from_secs(10)),
                    )),
                    Err(e) => Some(Err(e.to_string())),
                };
                if let Some(result) = result {
                    if let Ok(mut s) = out.lock() {
                        *s = Some(result);
                    }
                }
            });
        if let Err(e) = spawned {
            *slot.lock().unwrap() = Some(Err(e.to_string()));
        }
        Capture {
            label,
            started: Instant::now(),
            slot,
            evdev_failed: None,
            window: None,
            _cancel: cancel,
        }
    }

    /// The answer, once it is in: the key read straight from the input
    /// devices (exact, also Right Ctrl or a mouse side button), or — when
    /// ICU may not read them — a key pressed while this window has the
    /// focus. Keeps the window repainting while it waits.
    pub(super) fn poll(&mut self, ui: &Ui) -> Option<Result<Captured, String>> {
        super::repaint_after(ui.ctx(), Duration::from_millis(50));
        let evdev = self.slot.lock().ok().and_then(|mut s| s.take());
        if let Some(Ok(key)) = evdev {
            return Some(Ok(Captured::from_evdev(key)));
        }
        if let Some(Err(e)) = evdev {
            self.evdev_failed = Some(e);
        }
        if self.window.is_none() {
            self.window = window_key(ui).map(|k| (k, Instant::now()));
        }
        // A key seen by the window waits a moment for the exact one from
        // the devices, which knows left from right Ctrl.
        if let Some((key, at)) = &self.window {
            if self.evdev_failed.is_some() || at.elapsed() > Duration::from_millis(250) {
                return Some(Ok(key.clone()));
            }
        }
        if self.started.elapsed() > Duration::from_secs(10) {
            return Some(Err(self
                .evdev_failed
                .clone()
                .unwrap_or_else(|| "Keine Taste gedrückt.".to_string())));
        }
        None
    }
}

/// A captured key: its evdev name, and the portal trigger when one can be
/// written for it (`CTRL+SHIFT+F12`; a lone Right Ctrl or a mouse side
/// button has none).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Captured {
    key: String,
    trigger: Option<String>,
}

impl Captured {
    fn from_evdev(key: String) -> Captured {
        // Raw Input records a combination as a trigger (`CTRL+F12`): bound
        // as one, without a key, which would win (`hotkey::chord`).
        if key.contains('+') {
            return Captured {
                key: String::new(),
                trigger: Some(key),
            };
        }
        let trigger = key
            .strip_prefix("KEY_")
            .filter(|k| {
                // Letters, digits and F-keys read the same as portal triggers.
                (k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()))
                    || (k.starts_with('F') && k[1..].parse::<u8>().is_ok())
            })
            .map(str::to_string);
        Captured { key, trigger }
    }

    fn from_window_key(name: &str, modifiers: egui::Modifiers, windows: bool) -> Captured {
        let mut trigger = String::new();
        for (on, modifier) in [
            (modifiers.ctrl, "CTRL+"),
            (modifiers.alt, "ALT+"),
            (modifiers.shift, "SHIFT+"),
            (modifiers.mac_cmd, "LOGO+"),
        ] {
            if on {
                trigger.push_str(modifier);
            }
        }
        // Raw Input must parse the complete chord; a physical key would
        // take priority and silently discard its modifiers. Linux keeps
        // the evdev key and portal trigger as independent alternatives.
        let key = if windows && !trigger.is_empty() {
            String::new()
        } else {
            format!("KEY_{name}")
        };
        trigger.push_str(name);
        Captured {
            key,
            trigger: Some(trigger),
        }
    }

    /// Whether the portal can be asked for this key: a lone modifier or a
    /// mouse button has no trigger it would take.
    pub(super) fn has_trigger(&self) -> bool {
        self.trigger.is_some()
    }

    pub(super) fn portal_warning(&self, portal: bool) -> Option<String> {
        (portal && !self.has_trigger()).then(|| {
            "Diese Taste kann ICU der Arbeitsumgebung nicht vorschlagen. Die bisherige \
             Portal-Belegung bleibt erhalten. Nimm eine Buchstaben-, Ziffern- oder F-Taste \
             — oder lass ICU die Tasten direkt lesen (evdev)."
                .to_string()
        })
    }

    /// Writes itself into `binding`. A trigger is only replaced when there
    /// is a new one: evdev and portal bindings live side by side.
    pub(super) fn apply(&self, binding: &mut crate::config::KeyBinding) {
        binding.key.clone_from(&self.key);
        if let Some(t) = &self.trigger {
            binding.trigger = Some(t.clone());
        }
    }
}

/// The first key or mouse side button pressed in this window this frame,
/// with its modifiers — the fallback when the devices cannot be read.
fn window_key(ui: &Ui) -> Option<Captured> {
    ui.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => {
                let name = egui_key_name(*key)?;
                Some(Captured::from_window_key(name, *modifiers, cfg!(windows)))
            }
            egui::Event::PointerButton {
                button,
                pressed: true,
                ..
            } => {
                let key = match button {
                    egui::PointerButton::Extra1 => "BTN_SIDE",
                    egui::PointerButton::Extra2 => "BTN_EXTRA",
                    _ => return None,
                };
                Some(Captured {
                    key: key.into(),
                    trigger: None,
                })
            }
            _ => None,
        })
    })
}

/// evdev's name (without `KEY_`) for the keys a push-to-talk or toggle is
/// usually put on; `None` for the rest.
fn egui_key_name(key: egui::Key) -> Option<&'static str> {
    use egui::Key as K;
    Some(match key {
        K::A => "A",
        K::B => "B",
        K::C => "C",
        K::D => "D",
        K::E => "E",
        K::F => "F",
        K::G => "G",
        K::H => "H",
        K::I => "I",
        K::J => "J",
        K::K => "K",
        K::L => "L",
        K::M => "M",
        K::N => "N",
        K::O => "O",
        K::P => "P",
        K::Q => "Q",
        K::R => "R",
        K::S => "S",
        K::T => "T",
        K::U => "U",
        K::V => "V",
        K::W => "W",
        K::X => "X",
        K::Y => "Y",
        K::Z => "Z",
        K::Num0 => "0",
        K::Num1 => "1",
        K::Num2 => "2",
        K::Num3 => "3",
        K::Num4 => "4",
        K::Num5 => "5",
        K::Num6 => "6",
        K::Num7 => "7",
        K::Num8 => "8",
        K::Num9 => "9",
        K::F1 => "F1",
        K::F2 => "F2",
        K::F3 => "F3",
        K::F4 => "F4",
        K::F5 => "F5",
        K::F6 => "F6",
        K::F7 => "F7",
        K::F8 => "F8",
        K::F9 => "F9",
        K::F10 => "F10",
        K::F11 => "F11",
        K::F12 => "F12",
        K::F13 => "F13",
        K::F14 => "F14",
        K::F15 => "F15",
        K::F16 => "F16",
        K::F17 => "F17",
        K::F18 => "F18",
        K::F19 => "F19",
        K::F20 => "F20",
        K::F21 => "F21",
        K::F22 => "F22",
        K::F23 => "F23",
        K::F24 => "F24",
        K::Space => "SPACE",
        K::Tab => "TAB",
        K::Insert => "INSERT",
        K::Delete => "DELETE",
        K::Home => "HOME",
        K::End => "END",
        K::PageUp => "PAGEUP",
        K::PageDown => "PAGEDOWN",
        K::ArrowUp => "UP",
        K::ArrowDown => "DOWN",
        K::ArrowLeft => "LEFT",
        K::ArrowRight => "RIGHT",
        _ => return None,
    })
}

/// What a click in a [`binding_row`] asks for.
pub(super) enum BindingAction {
    None,
    /// Start „Aufnehmen" for this row.
    Record,
    /// Stop a running „Aufnehmen".
    Cancel,
    /// Remove the binding.
    Clear,
}

/// Label column of a [`binding_row`], wide enough for „Modus wechseln".
const BINDING_LABEL_W: f32 = 124.0;

/// One binding: the key as a chip (the portal's trigger while the portal
/// reads the keys, else the evdev key), „Aufnehmen" to take the next key
/// pressed, and for experts the raw text fields behind the pencil.
#[allow(clippy::too_many_arguments)]
pub(super) fn binding_row(
    ui: &mut Ui,
    divider: bool,
    label: &'static str,
    shortcut: crate::hotkey::Action,
    binding: &mut crate::config::KeyBinding,
    portal: bool,
    capturing: bool,
    advanced: bool,
    expanded: &mut std::collections::HashSet<&'static str>,
) -> BindingAction {
    if divider {
        ui.separator();
    }
    let mut action = BindingAction::None;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            let stacked = ui.available_width() < 500.0;
            let layout = if stacked {
                egui::Layout::top_down(egui::Align::Min)
            } else {
                egui::Layout::left_to_right(egui::Align::Center)
            };
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 30.0), layout, |ui| {
                ui.horizontal(|ui| {
                    widgets::label_column(ui, BINDING_LABEL_W, 30.0, label);
                    // Through the portal, the key the desktop assigned: the
                    // dialog may have taken another than the one proposed.
                    let assigned = portal
                        .then(|| crate::hotkey::portal_trigger(shortcut))
                        .flatten();
                    let shown = match (
                        portal || (cfg!(windows) && binding.key.trim().is_empty()),
                        binding.trigger.as_deref(),
                    ) {
                        (true, _) if assigned.as_deref() == Some("") => "—".to_string(),
                        (true, _) if assigned.is_some() => assigned.unwrap_or_default(),
                        (true, Some(t)) => t.to_string(),
                        _ if binding.key.is_empty() => "—".to_string(),
                        _ => binding
                            .key
                            .strip_prefix("KEY_")
                            .unwrap_or(&binding.key)
                            .to_string(),
                    };
                    if capturing {
                        let tok = theme::tokens(ui.visuals());
                        ui.label(
                            RichText::new("Taste drücken …")
                                .size(12.5)
                                .color(tok.accent),
                        );
                    } else {
                        widgets::chip(ui, &shown, 0.0).on_hover_text(if portal {
                            "Über die Arbeitsumgebung (Portal)"
                        } else if cfg!(target_os = "windows") {
                            "Direkt gelesen (Raw Input)"
                        } else {
                            "Direkt gelesen (evdev)"
                        });
                    }
                });
                widgets::rtl(ui, 30.0, |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if advanced {
                        let is_open = expanded.contains(label);
                        if widgets::icon_button(
                            ui,
                            icon::PENCIL_SIMPLE,
                            &format!("{label}: Kürzel bearbeiten"),
                            30.0,
                            is_open,
                        )
                        .on_hover_text(if is_open {
                            "Fertig"
                        } else {
                            "Von Hand eintragen"
                        })
                        .clicked()
                        {
                            if is_open {
                                expanded.remove(label);
                            } else {
                                expanded.insert(label);
                            }
                        }
                    }
                    if capturing {
                        if widgets::button(ui, widgets::ButtonKind::Neutral, icon::X, "Abbrechen")
                            .clicked()
                        {
                            action = BindingAction::Cancel;
                        }
                    } else {
                        // The slot stays when there is nothing to remove, so
                        // „Aufnehmen" lines up in every row.
                        let bound = !binding.key.is_empty() || binding.trigger.is_some();
                        if !bound {
                            ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
                        } else if widgets::icon_button(
                            ui,
                            icon::TRASH,
                            &format!("{label}: Kürzel entfernen"),
                            30.0,
                            false,
                        )
                        .on_hover_text("Belegung entfernen")
                        .clicked()
                        {
                            action = BindingAction::Clear;
                        }
                        if widgets::button(
                            ui,
                            widgets::ButtonKind::Neutral,
                            icon::RECORD,
                            "Aufnehmen",
                        )
                        .on_hover_text(if cfg!(windows) {
                            "Die nächste gedrückte Taste, Tastenkombination oder Maustaste \
                             übernehmen"
                        } else {
                            "Die nächste gedrückte Taste oder Maustaste übernehmen"
                        })
                        .clicked()
                        {
                            action = BindingAction::Record;
                        }
                    }
                });
            });
            if advanced && expanded.contains(label) {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if !stacked {
                        ui.add_space(BINDING_LABEL_W);
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut binding.key)
                            .desired_width(120.0)
                            .margin(FIELD_MARGIN)
                            .hint_text(if cfg!(target_os = "windows") {
                                "Taste"
                            } else {
                                "evdev"
                            }),
                    );
                    let mut trigger = binding.trigger.clone().unwrap_or_default();
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut trigger)
                                .desired_width(120.0)
                                .margin(FIELD_MARGIN)
                                .hint_text(if cfg!(windows) {
                                    "Kombination (ohne Taste)"
                                } else {
                                    "Portal"
                                }),
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
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui_context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::ui::fonts::install(&ctx);
        ctx
    }

    fn capture_fixture() -> (AppState, SettingsView) {
        let state = AppState {
            conn: crate::engine::ConnStatus::Connecting,
            server: String::new(),
            server_name: String::new(),
            me: None,
            peers: Vec::new(),
            video_viewers: Vec::new(),
            muted: false,
            setup_active: false,
            privacy_active: false,
            privacy_epoch: 0,
            deafened: false,
            system_muted: false,
            ptt_down: false,
            sending: false,
            voice: false,
            level: Default::default(),
            input_clipping: false,
            vad_prob: 0.0,
            vad_threshold: 0.5,
            rtt_ms: None,
            server_rtt_ms: None,
            clock_offset_ms: None,
            conn_info: None,
            conn_stats: None,
            update_required: None,
            banner: None,
            notice: None,
            name_error: None,
            rename_pending: false,
            is_admin: false,
            last_invite: None,
            invite_error: None,
            user_list: Vec::new(),
            user_list_generation: 0,
            user_action_error: None,
            restricted_muted: false,
            restricted_deafened: false,
            devices: Vec::new(),
            active_input: None,
            active_output: None,
            backend: "test",
            tuning_status: String::new(),
            audio_health: Default::default(),
            can_restore_audio: false,
            aec_available: false,
            aec_stats: Default::default(),
            aec_reference: false,
            aec_suspect: false,
            aec_no_echo: false,
            aec_reference_silent: false,
            mic_test: false,
            camera_on: false,
            hotkey_backend: "test".into(),
            settings: Settings::default(),
            hotkey_retries: 0,
        };
        let view = SettingsView::new(
            &state,
            Arc::default(),
            Paths::new(Some("unused-settings-test".into())),
        );
        (state, view)
    }

    /// Deliberately bypasses `Capture::start`: these regressions only
    /// complete a result slot and never open the host's keyboard devices.
    fn pending_capture() -> Capture {
        Capture {
            label: "Push-to-Talk",
            started: Instant::now(),
            slot: Arc::default(),
            evdev_failed: None,
            window: None,
            _cancel: tokio::sync::oneshot::channel().0,
        }
    }

    #[tokio::test]
    async fn dropping_capture_cancels_and_drops_the_pending_reader() {
        struct ReaderDropped(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for ReaderDropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let mut capture = pending_capture();
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        capture._cancel = cancel;
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = ReaderDropped(dropped.clone());
        let (started, running) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(capture_until_cancelled(cancelled, async move {
            let _reader = reader;
            let _ = started.send(());
            std::future::pending().await
        }));
        running.await.unwrap();
        assert!(!dropped.load(Ordering::SeqCst));

        drop(capture);
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("dropping the capture must stop a pending reader")
            .unwrap();
        assert!(result.is_none());
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn leaving_hotkeys_discards_late_capture_results_and_errors() {
        for via_import in [false, true] {
            let (state, mut view) = capture_fixture();
            let original = view.draft.hotkeys.ptt.clone();
            view.select_tab(Tab::Hotkeys);
            let capture = pending_capture();
            let late_result = capture.slot.clone();
            view.capture = Some(capture);
            view.capture_error = Some("Previous attempt failed".into());

            if via_import {
                view.open_import();
            } else {
                view.select_tab(Tab::Audio);
            }
            assert!(view.capture.is_none());
            assert!(view.capture_error.is_none());

            // The native reader can finish after the page was closed.
            // Returning to Hotkeys must not adopt its now-obsolete key.
            *late_result.lock().unwrap() = Some(Ok("KEY_F9".into()));
            view.select_tab(Tab::Hotkeys);
            let mut output = ui_context().run_ui(Default::default(), |ui| {
                view.hotkeys_tab(ui, &state, &mut Vec::new());
            });
            output.textures_delta.clear();
            assert_eq!(view.draft.hotkeys.ptt, original);
            assert!(view.capture_error.is_none());
        }
    }

    #[test]
    fn successful_capture_retry_clears_the_previous_error() {
        let (state, mut view) = capture_fixture();
        view.select_tab(Tab::Hotkeys);
        let mut failed = pending_capture();
        failed.started = Instant::now() - Duration::from_secs(11);
        *failed.slot.lock().unwrap() = Some(Err("No keyboard available".into()));
        view.capture = Some(failed);
        let ctx = ui_context();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            view.hotkeys_tab(ui, &state, &mut Vec::new());
        });
        output.textures_delta.clear();
        assert!(view.capture.is_none());
        assert_eq!(view.capture_error.as_deref(), Some("No keyboard available"));

        let retry = pending_capture();
        *retry.slot.lock().unwrap() = Some(Ok("KEY_F9".into()));
        view.capture = Some(retry);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            view.hotkeys_tab(ui, &state, &mut Vec::new());
        });
        output.textures_delta.clear();
        assert!(view.capture.is_none());
        assert!(view.capture_error.is_none());
        assert_eq!(view.draft.hotkeys.ptt.key, "KEY_F9");
        assert_eq!(view.draft.hotkeys.ptt.trigger.as_deref(), Some("F9"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn changing_the_advanced_backend_discards_late_capture_results() {
        use egui_kittest::{kittest::Queryable, Harness};

        let (state, mut view) = capture_fixture();
        view.draft.ui.show_advanced = true;
        let original = view.draft.hotkeys.ptt.clone();
        let capture = pending_capture();
        let late_result = capture.slot.clone();
        view.capture = Some(capture);
        view.capture_error = Some("Previous attempt failed".into());
        let mut h = Harness::builder()
            .with_size(egui::vec2(850.0, 1100.0))
            .build_ui_state(
                |ui, (state, view, initialized): &mut (AppState, SettingsView, bool)| {
                    if !*initialized {
                        crate::ui::fonts::install(ui.ctx());
                        *initialized = true;
                        return;
                    }
                    view.hotkeys_tab(ui, state, &mut Vec::new());
                },
                (state, view, false),
            );
        h.run_steps(4);
        h.get_by_label("Portal").click();
        h.run_steps(4);
        assert_eq!(h.state().1.draft.hotkeys.backend, HotkeyBackend::Portal);
        assert!(h.state().1.capture.is_none());
        assert!(h.state().1.capture_error.is_none());
        *late_result.lock().unwrap() = Some(Ok("KEY_F9".into()));
        h.run_steps(4);
        assert_eq!(h.state().1.draft.hotkeys.ptt, original);
    }

    #[test]
    fn unsupported_portal_capture_explains_why_the_old_trigger_remains() {
        use egui_kittest::{kittest::Queryable, Harness};

        for (theme_name, key) in [("light", "BTN_SIDE"), ("dark", "KEY_RIGHTCTRL")] {
            let (mut state, mut view) = capture_fixture();
            state.hotkey_backend = "portal".into();
            let old_trigger = view.draft.hotkeys.ptt.trigger.clone();
            let capture = pending_capture();
            *capture.slot.lock().unwrap() = Some(Ok(key.into()));
            view.capture = Some(capture);
            let mut h = Harness::builder()
                .with_size(egui::vec2(850.0, 800.0))
                .with_theme(if theme_name == "light" {
                    egui::Theme::Light
                } else {
                    egui::Theme::Dark
                })
                .build_ui_state(
                    move |ui, (state, view, initialized): &mut (AppState, SettingsView, bool)| {
                        if !*initialized {
                            crate::ui::fonts::install(ui.ctx());
                            theme::apply(ui.ctx(), theme_name);
                            *initialized = true;
                            return;
                        }
                        view.hotkeys_tab(ui, state, &mut Vec::new());
                    },
                    (state, view, false),
                );
            h.run();
            assert_eq!(h.state().1.draft.hotkeys.ptt.trigger, old_trigger);
            assert_eq!(h.state().1.draft.hotkeys.ptt.key, key);
            let warning = h
                .state()
                .1
                .capture_error
                .as_deref()
                .expect("unsupported Portal capture must warn");
            assert!(warning.contains("Arbeitsumgebung"));
            assert!(warning.contains("evdev"));
            h.get_by_label(warning);
            if let Ok(dir) = std::env::var("ICU_UI_SHOTS") {
                std::fs::create_dir_all(&dir).unwrap();
                h.render()
                    .unwrap()
                    .save(format!("{dir}/portal-capture-warning-{theme_name}.png"))
                    .unwrap();
            }
        }
    }

    /// „für 60 s“ only where ICU takes a copied secret off the clipboard
    /// for sure (review 2026-09-27, C-7).
    #[test]
    fn the_copy_status_promises_clearing_only_where_it_happens() {
        for copied in [SecretCopy::Marked, SecretCopy::Plain] {
            assert!(copied_status(copied).contains("60 s"), "{copied:?}");
        }
        let wayland = copied_status(SecretCopy::Wayland);
        assert!(!wayland.contains("60 s"), "{wayland}");
        assert!(wayland.contains("nicht sicher"), "{wayland}");
    }

    /// A key read from the devices gets a portal trigger where one reads the
    /// same (letters, digits, F-keys); a lone modifier or a mouse button
    /// keeps the trigger that is there.
    #[test]
    fn captured_keys_carry_a_trigger_where_one_exists() {
        let f12 = Captured::from_evdev("KEY_F12".into());
        assert_eq!(f12.trigger.as_deref(), Some("F12"));
        assert_eq!(
            Captured::from_evdev("KEY_P".into()).trigger.as_deref(),
            Some("P")
        );
        for key in ["KEY_RIGHTCTRL", "BTN_SIDE", "code:700", "KEY_FN"] {
            assert_eq!(Captured::from_evdev(key.into()).trigger, None, "{key}");
        }
        let mut binding = crate::config::KeyBinding {
            key: "KEY_F12".into(),
            trigger: Some("CTRL+SHIFT+F12".into()),
        };
        Captured::from_evdev("BTN_SIDE".into()).apply(&mut binding);
        assert_eq!(binding.key, "BTN_SIDE");
        assert_eq!(binding.trigger.as_deref(), Some("CTRL+SHIFT+F12"));
        // A combination recorded by Raw Input replaces the key, which
        // would otherwise win over it.
        Captured::from_evdev("CTRL+ALT+P".into()).apply(&mut binding);
        assert_eq!(binding.key, "");
        assert_eq!(binding.trigger.as_deref(), Some("CTRL+ALT+P"));
    }

    #[test]
    fn window_fallback_keeps_modifiers_in_windows_chords() {
        let modifiers = egui::Modifiers {
            ctrl: true,
            alt: true,
            shift: true,
            ..Default::default()
        };
        let mut binding = crate::config::KeyBinding::default();
        Captured::from_window_key("F12", modifiers, true).apply(&mut binding);
        assert!(binding.key.is_empty(), "a physical key overrides the chord");
        assert_eq!(binding.trigger.as_deref(), Some("CTRL+ALT+SHIFT+F12"));

        Captured::from_window_key("F12", egui::Modifiers::default(), true).apply(&mut binding);
        assert_eq!(binding.key, "KEY_F12");
        assert_eq!(binding.trigger.as_deref(), Some("F12"));

        // Linux has separate evdev and portal bindings, so its physical
        // key remains usable if the portal is unavailable.
        Captured::from_window_key("F12", modifiers, false).apply(&mut binding);
        assert_eq!(binding.key, "KEY_F12");
        assert_eq!(binding.trigger.as_deref(), Some("CTRL+ALT+SHIFT+F12"));
    }

    #[test]
    fn device_labels_distinguish_unknown_inventory_from_default_selection() {
        let device = crate::audio::DeviceInfo {
            id: "mic-1".into(),
            description: "Headset".into(),
            kind: DeviceKind::Input,
        };
        assert_eq!(
            device_choice_text(None, &[]),
            ("System-Standard".into(), None)
        );
        assert_eq!(
            device_choice_text(Some("mic-1"), &[&device]),
            ("Headset".into(), None)
        );
        let (text, hint) = device_choice_text(Some("mic-1"), &[]);
        assert_eq!(text, "Auswahl nicht bestätigt");
        assert!(hint.unwrap().contains("behält die Auswahl „mic-1“ bei"));
        let (text, hint) = device_choice_text(Some("missing-mic"), &[&device]);
        assert_eq!(text, "Fehlt – System-Standard gewählt");
        assert!(hint.unwrap().contains("kehrt zurück"));
    }

    /// The window's own key names match what the evdev backend parses.
    #[test]
    fn window_keys_are_evdev_names() {
        for (key, name) in [
            (egui::Key::A, "KEY_A"),
            (egui::Key::Num7, "KEY_7"),
            (egui::Key::F9, "KEY_F9"),
            (egui::Key::Space, "KEY_SPACE"),
            (egui::Key::PageDown, "KEY_PAGEDOWN"),
        ] {
            let evdev = format!("KEY_{}", egui_key_name(key).expect("mapped"));
            assert_eq!(evdev, name);
            #[cfg(target_os = "linux")]
            assert!(
                crate::hotkey::evdev::parse_key(&evdev).is_some(),
                "{evdev} is not a name the evdev backend reads"
            );
        }
        assert_eq!(egui_key_name(egui::Key::Escape), None);
    }
}
