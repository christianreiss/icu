//! Settings panel, shown as a `CentralPanel` takeover with a tab strip
//! (`main_window.rs`). Every change is applied at once through
//! `Command::UpdateSettings`.

use egui::Ui;
use egui_phosphor::regular as icon;

use crate::{
    audio::DeviceKind,
    config::{Corner, HotkeyBackend, ModeSetting, Settings, TileSize},
    engine::{AppState, Command},
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

/// Draft state of the settings panel.
pub struct SettingsView {
    /// Working copy, applied on change.
    pub draft: Settings,
    /// Name field (applied with the button, not on every keystroke).
    pub name: String,
    /// Hotkey backend in use, for display.
    pub hotkey_backend: String,
    /// Portal hint shown once.
    pub show_portal_hint: bool,
    /// Active tab.
    tab: Tab,
}

impl SettingsView {
    /// From the engine's current settings.
    pub fn new(state: &AppState, hotkey_backend: String) -> SettingsView {
        SettingsView {
            draft: state.settings.clone(),
            name: state
                .me
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_default(),
            hotkey_backend,
            show_portal_hint: true,
            tab: Tab::Audio,
        }
    }

    /// Draws the panel content. Returns commands to send.
    pub fn ui(&mut self, ui: &mut Ui, state: &AppState) -> Vec<Command> {
        let mut cmds = Vec::new();
        let before = self.draft.clone();

        ui.horizontal(|ui| {
            for (tab, label) in [
                (Tab::Audio, format!("{} Audio", icon::SLIDERS)),
                (Tab::Hotkeys, format!("{} Hotkeys", icon::KEYBOARD)),
                (Tab::Camera, format!("{} Kamera & Overlay", icon::VIDEO_CAMERA)),
                (Tab::General, format!("{} Allgemein", icon::GEAR)),
                (Tab::About, format!("{} Über", icon::INFO)),
            ] {
                if ui.selectable_label(self.tab == tab, label).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.separator();

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
        device_combo(ui, "Mikrofon", &mut self.draft.audio.input_device, &inputs);
        device_combo(
            ui,
            "Wiedergabe",
            &mut self.draft.audio.output_device,
            &outputs,
        );
        ui.horizontal(|ui| {
            ui.label("Modus");
            ui.selectable_value(&mut self.draft.audio.mode, ModeSetting::Ptt, "Push-to-Talk");
            ui.selectable_value(
                &mut self.draft.audio.mode,
                ModeSetting::Vad,
                "Sprachaktivierung",
            );
        });
        ui.add(
            egui::Slider::new(&mut self.draft.audio.vad_threshold, 0.0..=1.0)
                .text("VAD-Schwelle"),
        );
        ui.add(
            egui::Slider::new(&mut self.draft.audio.gain_db, -20.0..=20.0)
                .text("Verstärkung dB"),
        );
        ui.add(
            egui::Slider::new(&mut self.draft.audio.release_ms, 0..=1000)
                .text("PTT-Nachlauf ms"),
        );
        ui.add(
            egui::Slider::new(&mut self.draft.audio.hangover_ms, 0..=1000)
                .text("VAD-Nachlauf ms"),
        );
        ui.add(
            egui::Slider::new(&mut self.draft.audio.output_volume, 0.0..=200.0)
                .text("Wiedergabe %"),
        );
        ui.horizontal(|ui| {
            ui.label("Opus-Frame");
            ui.selectable_value(&mut self.draft.audio.frame_ms, 20u8, "20 ms");
            ui.selectable_value(&mut self.draft.audio.frame_ms, 10u8, "10 ms");
        });
        ui.checkbox(
            &mut self.draft.audio.denoise,
            "Rauschunterdrückung (RNNoise)",
        );
        let aec = ui.checkbox(&mut self.draft.audio.aec, "Echo-Unterdrückung (AEC3)");
        aec.on_hover_text(
            "Entfernt auch den eigenen Spielton aus dem gesendeten Signal. \
             Bei Headsets ohne Echo abschaltbar, spart 9 ms Latenz.",
        );
        if !state.aec_available {
            ui.label(
                egui::RichText::new("Echo-Unterdrückung auf dieser Installation nicht verfügbar")
                    .small(),
            );
        } else if let (Some(erl), Some(erle)) = (state.aec_stats.erl_db, state.aec_stats.erle_db)
        {
            ui.label(egui::RichText::new(format!("ERL {erl:.0} dB · ERLE {erle:.0} dB")).small());
        }
        ui.checkbox(&mut self.draft.audio.cue_join, "Ton bei Beitritt/Verlassen");
        ui.checkbox(&mut self.draft.audio.cue_tx, "Ton bei Sendebeginn/-ende");
        ui.checkbox(&mut self.draft.audio.cue_mute, "Ton bei Stummschalten");
    }

    fn hotkeys_tab(&mut self, ui: &mut Ui) {
        ui.label(format!("Aktives Backend: {}", self.hotkey_backend));
        ui.horizontal(|ui| {
            ui.label("Backend");
            ui.selectable_value(&mut self.draft.hotkeys.backend, HotkeyBackend::Auto, "auto");
            ui.selectable_value(
                &mut self.draft.hotkeys.backend,
                HotkeyBackend::Portal,
                "Portal",
            );
            ui.selectable_value(
                &mut self.draft.hotkeys.backend,
                HotkeyBackend::Evdev,
                "evdev",
            );
        });
        binding_row(ui, "PTT", &mut self.draft.hotkeys.ptt);
        binding_row(ui, "Mute", &mut self.draft.hotkeys.mute);
        binding_row(ui, "Deafen", &mut self.draft.hotkeys.deafen);
        binding_row(ui, "PTT/VAD", &mut self.draft.hotkeys.toggle_mode);
        ui.label(
            egui::RichText::new(
                "evdev-Namen wie KEY_RIGHTCTRL, BTN_SIDE oder code:97; Portal-Trigger wie \
                 CTRL+SHIFT+F12. evdev braucht Gruppe input und liest alle Tasten \
                 (Keylogger-Aspekt). Hotkey-Änderungen gelten ab dem nächsten Start.",
            )
            .small(),
        );
    }

    fn camera_tab(&mut self, ui: &mut Ui) {
        ui.heading("Kamera");
        ui.checkbox(
            &mut self.draft.camera.enabled,
            "Kamera beim Start einschalten",
        );
        let mut dev = self.draft.camera.device.clone().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("Gerät (Index)");
            if ui.text_edit_singleline(&mut dev).changed() {
                self.draft.camera.device = if dev.is_empty() { None } else { Some(dev.clone()) };
            }
        });

        ui.separator();
        ui.heading("Overlay");
        ui.checkbox(&mut self.draft.overlay.enabled, "Overlay anzeigen");
        ui.horizontal(|ui| {
            ui.label("Ecke");
            ui.selectable_value(
                &mut self.draft.overlay.corner,
                Corner::TopLeft,
                "oben links",
            );
            ui.selectable_value(
                &mut self.draft.overlay.corner,
                Corner::TopRight,
                "oben rechts",
            );
        });
        ui.horizontal(|ui| {
            ui.label("     ");
            ui.selectable_value(
                &mut self.draft.overlay.corner,
                Corner::BottomLeft,
                "unten links",
            );
            ui.selectable_value(
                &mut self.draft.overlay.corner,
                Corner::BottomRight,
                "unten rechts",
            );
        });
        ui.horizontal(|ui| {
            ui.label("Abstand");
            ui.add(egui::DragValue::new(&mut self.draft.overlay.offset[0]).range(0..=2000));
            ui.add(egui::DragValue::new(&mut self.draft.overlay.offset[1]).range(0..=2000));
        });
        ui.horizontal(|ui| {
            ui.label("Größe");
            ui.selectable_value(&mut self.draft.overlay.size, TileSize::Small, "klein");
            ui.selectable_value(&mut self.draft.overlay.size, TileSize::Medium, "mittel");
        });
    }

    fn general_tab(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        ui.horizontal(|ui| {
            ui.label("Anzeigename");
            ui.text_edit_singleline(&mut self.name);
            if ui.button("Übernehmen").clicked() {
                cmds.push(Command::SetName(self.name.trim().to_string()));
            }
        });
        if let Some(err) = &state.name_error {
            ui.colored_label(super::theme::ERROR, err);
        }
        ui.checkbox(&mut self.draft.ui.start_minimized, "Minimiert starten");
        ui.checkbox(
            &mut self.draft.ui.register_uri_handler,
            "icu://-Links mit ICU öffnen",
        );
        ui.horizontal(|ui| {
            ui.label("Theme");
            for t in ["system", "light", "dark"] {
                if ui.selectable_label(self.draft.ui.theme == t, t).clicked() {
                    self.draft.ui.theme = t.to_string();
                }
            }
        });
    }

    fn about_tab(&self, ui: &mut Ui, state: &AppState) {
        ui.label(format!("ICU {}", env!("CARGO_PKG_VERSION")));
        ui.label(format!("Audio-Backend: {}", state.backend));
        ui.label(
            egui::RichText::new(
                "Enthält Opus (BSD-3), RNNoise/nnnoiseless (BSD-3), WebRTC AudioProcessing \
                 (BSD-3), OpenH264 (BSD-2, aus Quelle gebaut — nicht unter Ciscos \
                 Lizenzgewährung für Binaries), Inter (OFL-1.1), Phosphor-Icons (MIT/Apache-2.0). \
                 Lizenztexte liegen dem Release bei.",
            )
            .small(),
        );
    }
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
    ui.horizontal(|ui| {
        ui.label(label);
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

fn binding_row(ui: &mut Ui, label: &str, binding: &mut crate::config::KeyBinding) {
    ui.horizontal(|ui| {
        ui.label(label);
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
