//! The setup („ICU — Einrichtung“): four steps through what matters on the
//! first evening — microphone and speaker, how to be heard (voice
//! activation, or push-to-talk and its key), how the own voice is processed
//! — and an overview to finish with. The main window opens it on a new
//! install (`[ui] setup_done` unset), Einstellungen → Audio → „Assistent“
//! at any time later.
//!
//! Every choice applies at once, as in the settings window, so the level
//! bar, the mic test and the key test show the real thing. While the setup
//! is open the microphone is muted: what is tried out here stays on this
//! machine.

use std::collections::HashSet;

use egui::{RichText, Ui};
use egui_phosphor::regular as icon;

use super::{
    main_window::CLIP_DB,
    settings::{
        binding_row, device_combo, elevated_notice, evdev_notice, hotkey_notice, mic_test_button,
        BindingAction, Capture, DraftSync,
    },
    theme, widgets,
};
use crate::{
    audio::{DeviceInfo, DeviceKind},
    config::{HotkeyBackend, KeyBinding, ModeSetting, Settings, VoiceStrength},
    engine::{AppState, Command},
};

/// The label of the one key row, as on the Tastenkürzel page.
const PTT_LABEL: &str = "Push-to-Talk";
/// Width of the level bars: the device lists' width, so they line up.
const METER_W: f32 = 240.0;
/// Gap between the two choice cards on the Sprechen step.
const CARD_GAP: f32 = 10.0;
/// Label column of the overview's rows.
const SUMMARY_LABEL_W: f32 = 110.0;

/// The steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Microphone, speaker, volume; level bar and mic test.
    Devices,
    /// Voice activation or push-to-talk, and the key.
    Speak,
    /// Noise suppression, echo, levels, voice optimizer.
    Voice,
    /// What is set now, with a way back to each step.
    Summary,
}

impl Step {
    /// All steps, in order.
    pub const ALL: [Step; 4] = [Step::Devices, Step::Speak, Step::Voice, Step::Summary];

    /// Its name in the stepper.
    fn title(self) -> &'static str {
        match self {
            Step::Devices => "Geräte",
            Step::Speak => "Sprechen",
            Step::Voice => "Klang",
            Step::Summary => "Übersicht",
        }
    }

    fn index(self) -> usize {
        match self {
            Step::Devices => 0,
            Step::Speak => 1,
            Step::Voice => 2,
            Step::Summary => 3,
        }
    }

    /// The step after this one; the last has none.
    pub fn next(self) -> Option<Step> {
        Step::ALL.get(self.index() + 1).copied()
    }

    /// The step before this one; the first has none.
    pub fn prev(self) -> Option<Step> {
        self.index().checked_sub(1).map(|i| Step::ALL[i])
    }
}

/// What the bottom bar was asked for.
enum Nav {
    Back,
    Next,
    Skip,
    Finish,
}

/// The setup window's content. `pub` for `tests/ui_layout.rs`.
pub struct SetupView {
    step: Step,
    /// Working copy, applied on change (as `SettingsView::draft`).
    draft: Settings,
    /// Keeps [`Self::draft`] in step with the engine.
    sync: DraftSync,
    /// A running „Aufnehmen“ for the push-to-talk key.
    capture: Option<Capture>,
    /// Why the last „Aufnehmen“ did not give a usable key.
    capture_error: Option<String>,
    /// The first frame has been drawn: the microphone was muted then.
    started: bool,
    /// The setup muted the microphone, so it unmutes it when it closes.
    muted_by_setup: bool,
    /// The key row's expert fields, which the setup never opens.
    expanded: HashSet<&'static str>,
}

/// What one frame of the setup asks the main window to do.
#[derive(Debug, Default)]
pub struct SetupOutput {
    /// Commands for the engine.
    pub cmds: Vec<Command>,
    /// „Fertig“ or „Überspringen“: close the window through
    /// [`SetupView::close`].
    pub done: bool,
    /// A step with the mic test is on screen; anywhere else a running test
    /// is stopped.
    pub mic_test_page: bool,
    /// How much taller the step is than the window shows (negative:
    /// shorter).
    pub overflow: f32,
}

impl SetupView {
    /// From the engine's current settings, on the first step.
    pub fn new(state: &AppState) -> SetupView {
        SetupView {
            step: Step::Devices,
            draft: state.settings.clone(),
            sync: DraftSync::new(&state.settings),
            capture: None,
            capture_error: None,
            started: false,
            muted_by_setup: false,
            expanded: HashSet::new(),
        }
    }

    /// The step on screen.
    pub fn step(&self) -> Step {
        self.step
    }

    /// The header's theme button flipped the theme outside this window:
    /// the draft follows, and nothing is re-sent for it.
    pub fn set_theme(&mut self, theme: &str) {
        self.draft.ui.theme = theme.to_string();
        self.sync.set_theme(theme);
    }

    /// Ends the setup, finished or skipped: from now on it opens only when
    /// asked. What was chosen stays; the microphone is unmuted if the setup
    /// muted it and it still is.
    pub fn close(&mut self, state: &AppState) -> Vec<Command> {
        self.capture = None;
        self.draft.ui.setup_done = true;
        let mut cmds: Vec<Command> = self.sync.commit(&self.draft).into_iter().collect();
        cmds.extend(self.release_mute(state));
        cmds
    }

    /// Unmutes the microphone if the setup muted it and it still is — at
    /// the end of the setup, and when ICU quits with the setup open: the
    /// engine keeps a mute for the next start (`AudioSettings::muted`), and
    /// the setup's own must not be taken for the user's.
    pub fn release_mute(&mut self, state: &AppState) -> Option<Command> {
        (std::mem::take(&mut self.muted_by_setup) && state.muted)
            .then_some(Command::SetMuted(false))
    }

    /// Draws the current step with the stepper above and the bar below.
    pub fn ui(&mut self, ui: &mut Ui, state: &AppState) -> SetupOutput {
        let mut cmds = Vec::new();
        self.sync.adopt(&mut self.draft, &state.settings);
        if !self.started {
            self.started = true;
            // Nothing tried out here is meant for the room.
            if !state.muted {
                cmds.push(Command::SetMuted(true));
                self.muted_by_setup = true;
            }
        }

        let nav = self.actions(ui);
        let mut goto = None;
        let page = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 22,
                        right: 22,
                        top: 16,
                        bottom: 18,
                    })
                    .show(ui, |ui| {
                        stepper(ui, self.step);
                        ui.add_space(18.0);
                        match self.step {
                            Step::Devices => self.devices(ui, state, &mut cmds),
                            Step::Speak => self.speak(ui, state, &mut cmds),
                            Step::Voice => self.voice(ui, state, &mut cmds),
                            Step::Summary => goto = self.summary(ui, state),
                        }
                    });
            });
        cmds.extend(self.sync.commit(&self.draft));
        // A step taller than the window (a banner about the keys) grows the
        // window instead of hiding its end behind a scroll bar.
        let overflow = page.content_size.y - page.inner_rect.height();
        super::grow_to_fit(ui.ctx(), overflow);

        let mut done = false;
        match nav {
            Some(Nav::Back) => self.go(self.step.prev()),
            Some(Nav::Next) => self.go(self.step.next()),
            Some(Nav::Skip | Nav::Finish) => done = true,
            None => self.go(goto),
        }
        SetupOutput {
            cmds,
            done,
            mic_test_page: matches!(self.step, Step::Devices | Step::Voice),
            overflow,
        }
    }

    /// Moves to `step`, if there is one; a running „Aufnehmen“ ends.
    fn go(&mut self, step: Option<Step>) {
        if let Some(step) = step {
            self.step = step;
            self.capture = None;
            self.capture_error = None;
        }
    }

    /// The bottom bar, like the invite window's: back (on the first step:
    /// skip) on the left, on (on the last: done) on the right.
    fn actions(&self, ui: &mut Ui) -> Option<Nav> {
        let mut nav = None;
        egui::Panel::bottom("setup_actions")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin {
                        left: 22,
                        right: 22,
                        top: 12,
                        bottom: 14,
                    }),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let justified =
                        egui::Layout::centered_and_justified(egui::Direction::LeftToRight);
                    let (glyph, label, hint, back) = match self.step.prev() {
                        Some(_) => (icon::ARROW_LEFT, "Zurück", None, Nav::Back),
                        None => (
                            "",
                            "Überspringen",
                            Some(
                                "Die Einrichtung schließen. Unter Einstellungen → Audio → \
                                 „Assistent“ geht es jederzeit weiter.",
                            ),
                            Nav::Skip,
                        ),
                    };
                    let mut left = ui
                        .allocate_ui_with_layout(egui::vec2(130.0, 34.0), justified, |ui| {
                            widgets::button(ui, widgets::ButtonKind::Neutral, glyph, label)
                        })
                        .inner;
                    if let Some(hint) = hint {
                        left = left.on_hover_text(hint);
                    }
                    if left.clicked() {
                        nav = Some(back);
                    }
                    widgets::rtl(ui, 34.0, |ui| {
                        let (text, forward) = match self.step.next() {
                            Some(_) => (format!("Weiter  {}", icon::ARROW_RIGHT), Nav::Next),
                            None => (format!("{}  Fertig", icon::CHECK), Nav::Finish),
                        };
                        let right = ui
                            .allocate_ui_with_layout(egui::vec2(150.0, 34.0), justified, |ui| {
                                widgets::button(ui, widgets::ButtonKind::Primary, "", &text)
                            })
                            .inner;
                        if right.clicked() {
                            nav = Some(forward);
                        }
                    });
                });
            });
        nav
    }

    /// Step 1: microphone with its level and test, speaker and volume.
    fn devices(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        widgets::page_header(
            ui,
            "Mikrofon und Lautsprecher",
            "Worüber du sprichst und hörst. Sag ein paar Worte: der Balken zeigt, was ICU \
             vom Mikrofon hört.",
            |_| {},
        );
        if self.muted_by_setup && state.muted {
            let tok = theme::tokens(ui.visuals());
            ui.label(
                RichText::new(format!(
                    "{}  Solange die Einrichtung offen ist, bist du stummgeschaltet: die \
                     anderen hören deine Tests nicht.",
                    icon::MICROPHONE_SLASH
                ))
                .size(12.0)
                .color(tok.muted_text),
            );
            ui.add_space(12.0);
        }
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
            device_combo(
                ui,
                false,
                "Mikrofon",
                &mut self.draft.audio.input_device,
                &inputs,
                |ui| mic_test_button(ui, state.mic_test, cmds),
            );
            widgets::setting_row(ui, true, "Pegel", None, |ui| {
                let tok = theme::tokens(ui.visuals());
                let clipping = state.level.peak_db >= CLIP_DB;
                meter(
                    ui,
                    theme::meter_fraction(state.level.rms_db),
                    None,
                    if clipping { tok.error } else { tok.level },
                )
                .on_hover_text(if clipping {
                    "Übersteuert — leiser sprechen oder das Mikrofon herunterregeln."
                } else {
                    "Was vom Mikrofon ankommt"
                })
            });
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
    }

    /// Step 2: voice activation or push-to-talk, then what each needs.
    fn speak(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        widgets::page_header(
            ui,
            "Wie willst du sprechen?",
            "Umschalten geht später jederzeit, auch im Hauptfenster.",
            |_| {},
        );
        let width = ((ui.available_width() - CARD_GAP) / 2.0).floor();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            for (mode, glyph, title, text) in [
                (
                    ModeSetting::Vad,
                    icon::WAVEFORM,
                    "Automatisch",
                    "Du bist zu hören, sobald du sprichst. Bequem, wenn es um dich herum \
                     ruhig ist.",
                ),
                (
                    ModeSetting::Ptt,
                    icon::HAND_TAP,
                    "Taste halten",
                    "Du bist nur zu hören, solange du eine Taste hältst. Nichts geht \
                     versehentlich hinaus.",
                ),
            ] {
                let selected = self.draft.audio.mode == mode;
                if choice_card(ui, selected, glyph, title, text, width).clicked() {
                    self.draft.audio.mode = mode;
                }
            }
        });
        ui.add_space(16.0);
        match self.draft.audio.mode {
            ModeSetting::Ptt => self.key_card(ui, state, cmds),
            ModeSetting::Vad => self.vad_card(ui, state),
        }
    }

    /// Push-to-talk: the key, „Aufnehmen“, and a light that shows it
    /// arrives.
    fn key_card(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        let backend = state.hotkey_backend.as_str();
        hotkey_notice(
            ui,
            backend,
            &mut self.draft.hotkeys.backend,
            &mut self.draft.audio.mode,
            cmds,
        );
        if cfg!(target_os = "linux") && self.draft.hotkeys.backend == HotkeyBackend::Evdev {
            evdev_notice(ui);
            ui.add_space(12.0);
        }
        let portal = backend == "portal";
        match self.capture.as_mut().and_then(|c| c.poll(ui)) {
            Some(Ok(key)) => {
                key.apply(&mut self.draft.hotkeys.ptt);
                self.capture = None;
                // The portal is asked with a trigger; a lone modifier or a
                // mouse button has none, so it could never fire there.
                self.capture_error = (portal && !key.has_trigger()).then(|| {
                    "Diese Taste kann ICU der Arbeitsumgebung nicht vorschlagen. Nimm eine \
                     Buchstaben-, Ziffern- oder F-Taste — oder lass ICU die Tasten direkt \
                     lesen (evdev)."
                        .to_string()
                });
            }
            Some(Err(e)) => {
                self.capture = None;
                self.capture_error = Some(e);
            }
            None => {}
        }
        let capturing = self.capture.is_some();
        widgets::group_card(ui, "Deine Taste", |ui| {
            let binding = &mut self.draft.hotkeys.ptt;
            match binding_row(
                ui,
                false,
                PTT_LABEL,
                crate::hotkey::Action::Ptt,
                binding,
                portal,
                capturing,
                false,
                &mut self.expanded,
            ) {
                BindingAction::None => {}
                BindingAction::Record => {
                    self.capture = Some(Capture::start(PTT_LABEL));
                    self.capture_error = None;
                }
                BindingAction::Cancel => self.capture = None,
                BindingAction::Clear => *binding = KeyBinding::default(),
            }
            if let Some(err) = &self.capture_error {
                widgets::card_note(ui, |ui| {
                    ui.colored_label(
                        theme::tokens(ui.visuals()).connecting,
                        RichText::new(err).size(11.5),
                    );
                });
            }
            widgets::setting_row(
                ui,
                true,
                "Ausprobieren",
                Some(
                    "Halte die Taste, gern auch in einem anderen Fenster: leuchtet es, kommt \
                     sie bei ICU an.",
                ),
                |ui| lamp(ui, state.ptt_down, "Taste erkannt", "Taste halten"),
            );
            if portal {
                widgets::help_note(
                    ui,
                    "Bei einer neuen Taste fragt die Arbeitsumgebung nach, ob ICU sie benutzen \
                     darf.",
                );
            }
            if cfg!(target_os = "windows") {
                elevated_notice(ui);
            }
        });
    }

    /// Voice activation: the threshold, and the detector against it.
    fn vad_card(&mut self, ui: &mut Ui, state: &AppState) {
        widgets::group_card(ui, "Sprachaktivierung", |ui| {
            let audio = &mut self.draft.audio;
            ui.add_enabled_ui(audio.denoise, |ui| {
                widgets::setting_row(
                    ui,
                    false,
                    "Schwelle automatisch",
                    Some(
                        "Passt sich laufend an deine Umgebung an. Braucht die Rauschunterdrückung.",
                    ),
                    |ui| widgets::named_toggle(ui, &mut audio.vad_auto, "Schwelle automatisch"),
                )
            });
            if !audio.vad_auto {
                widgets::slider_row(
                    ui,
                    true,
                    "Schwelle",
                    &mut audio.vad_threshold,
                    0.0..=1.0,
                    "",
                );
            }
            widgets::setting_row(
                ui,
                true,
                "Ausprobieren",
                Some(
                    "Sprich: der Balken ist die Stimmerkennung, der Strich die Schwelle. Wird \
                     er farbig, wärst du zu hören.",
                ),
                |ui| {
                    let tok = theme::tokens(ui.visuals());
                    meter(
                        ui,
                        state.vad_prob,
                        Some(state.vad_threshold),
                        if state.voice {
                            tok.accent
                        } else {
                            tok.dim_text
                        },
                    )
                },
            );
        });
    }

    /// Step 3: the Klang page's processing switches, with the mic test to
    /// hear what each one does.
    fn voice(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) {
        widgets::page_header(
            ui,
            "Wie du klingst",
            "Die Vorgaben passen für die meisten. Mit „Testen“ hörst du dich so, wie die \
             anderen dich hören — schalte dabei um und vergleiche.",
            |ui| mic_test_button(ui, state.mic_test, cmds),
        );
        widgets::group_card(ui, "Aufbereitung", |ui| {
            let audio = &mut self.draft.audio;
            widgets::setting_row(
                ui,
                false,
                "Rauschunterdrückung",
                Some("Filtert Tastatur, Lüfter und Hintergrundgeräusche heraus."),
                |ui| widgets::named_toggle(ui, &mut audio.denoise, "Rauschunterdrückung"),
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
                    |ui| widgets::named_toggle(ui, &mut audio.aec, "Echo-Unterdrückung"),
                )
            });
            ui.add_enabled_ui(audio.denoise, |ui| {
                widgets::setting_row(
                    ui,
                    true,
                    "Pegel automatisch",
                    Some("Regelt dein Mikrofon nach, wenn du zu leise oder zu laut bist."),
                    |ui| widgets::autotune_control(ui, audio, &state.tuning_status),
                )
            });
            widgets::setting_row(
                ui,
                true,
                "Selbst gleich laut ankommen",
                Some("Du kommst bei den anderen immer ähnlich laut an."),
                |ui| {
                    widgets::named_toggle(
                        ui,
                        &mut audio.normalize_outgoing,
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
                        &mut audio.normalize_incoming,
                        "Andere gleich laut hören",
                    )
                },
            );
            // The strength beside the switch rather than in a row of its
            // own (the Klang page's): switching it on does not push the
            // step past the window.
            widgets::setting_row(
                ui,
                true,
                "Stimmoptimierung",
                Some("Gleicht leise und laute Stellen aus, damit du gleichmäßig klingst."),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    widgets::named_toggle(ui, &mut audio.voice_opt, "Stimmoptimierung");
                    if audio.voice_opt {
                        widgets::segmented(
                            ui,
                            &mut audio.voice_strength,
                            &[
                                (VoiceStrength::Light, "Leicht"),
                                (VoiceStrength::Medium, "Mittel"),
                                (VoiceStrength::Strong, "Stark"),
                            ],
                        );
                    }
                },
            );
        });
    }

    /// Step 4: what is set now; „Ändern“ goes back to the step in question.
    fn summary(&mut self, ui: &mut Ui, state: &AppState) -> Option<Step> {
        widgets::page_header(
            ui,
            "Alles bereit",
            "So ist ICU jetzt eingestellt. Alles lässt sich später unter Einstellungen ändern.",
            |_| {},
        );
        let lines = summary(
            &self.draft,
            &state.devices,
            &state.hotkey_backend,
            state.aec_available,
        );
        let mut goto = None;
        widgets::group_card(ui, "Deine Einstellungen", |ui| {
            for (i, line) in lines.iter().enumerate() {
                if summary_row(ui, i > 0, line) {
                    goto = Some(line.step);
                }
            }
        });
        if self.muted_by_setup && state.muted {
            let tok = theme::tokens(ui.visuals());
            ui.label(
                RichText::new(format!(
                    "{}  Mit „Fertig“ bist du wieder zu hören.",
                    icon::MICROPHONE
                ))
                .size(12.0)
                .color(tok.muted_text),
            );
        }
        goto
    }
}

/// One row of the overview: what is set, and where it is changed.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    label: &'static str,
    value: String,
    /// Something that will not work the way it probably should.
    warning: Option<&'static str>,
    step: Step,
}

/// The overview's rows for `settings`, in the words of the steps.
fn summary(
    settings: &Settings,
    devices: &[DeviceInfo],
    backend: &str,
    aec_available: bool,
) -> Vec<Line> {
    let device = |id: &Option<String>, kind: DeviceKind| match id {
        None => "System-Standard".to_string(),
        Some(id) => devices
            .iter()
            .find(|d| d.kind == kind && d.id == *id)
            .map(|d| d.description.clone())
            .unwrap_or_else(|| "Fehlt – System-Standard läuft".to_string()),
    };
    let audio = &settings.audio;
    let (send, warning) = match audio.mode {
        ModeSetting::Vad if audio.vad_auto => {
            ("Automatisch · Schwelle automatisch".to_string(), None)
        }
        ModeSetting::Vad => (
            format!("Automatisch · Schwelle {:.2}", audio.vad_threshold),
            None,
        ),
        ModeSetting::Ptt => {
            let key = super::ptt_key_label(&settings.hotkeys, backend);
            let working = !matches!(
                backend,
                "" | crate::hotkey::BACKEND_NONE | crate::hotkey::BACKEND_PORTAL_DECLINED
            );
            if key == "—" {
                (
                    "Taste halten · keine Taste".to_string(),
                    Some("Ohne Taste geht Sprechen nur über die Sprechfläche im Hauptfenster."),
                )
            } else if !working {
                (
                    format!("Taste halten · {key}"),
                    Some(
                        "Tastenkürzel wirken gerade nicht — Sprechen geht nur über die \
                         Sprechfläche im Hauptfenster.",
                    ),
                )
            } else {
                (format!("Taste halten · {key}"), None)
            }
        }
    };
    let strength = match audio.voice_strength {
        VoiceStrength::Light => "Leicht",
        VoiceStrength::Medium => "Mittel",
        VoiceStrength::Strong => "Stark",
    };
    let mut on = Vec::new();
    if audio.denoise {
        on.push("Rauschunterdrückung".to_string());
    }
    if audio.aec && aec_available {
        on.push("Echo-Unterdrückung".to_string());
    }
    if audio.auto_level && audio.denoise {
        on.push("Pegel automatisch".to_string());
        if audio.peer_assistance {
            on.push("Empfangsfeedback".to_string());
        }
        if audio.tune_system_mic {
            on.push("Systemmikrofon automatisch".to_string());
        }
    }
    if audio.normalize_outgoing {
        on.push("Selbst gleich laut ankommen".to_string());
    }
    if audio.normalize_incoming {
        on.push("Andere gleich laut hören".to_string());
    }
    if audio.voice_opt {
        on.push(format!("Stimmoptimierung ({strength})"));
    }
    vec![
        Line {
            label: "Mikrofon",
            value: device(&audio.input_device, DeviceKind::Input),
            warning: None,
            step: Step::Devices,
        },
        Line {
            label: "Lautsprecher",
            value: format!(
                "{} · {:.0} %",
                device(&audio.output_device, DeviceKind::Output),
                audio.output_volume
            ),
            warning: None,
            step: Step::Devices,
        },
        Line {
            label: "Senden",
            value: send,
            warning,
            step: Step::Speak,
        },
        Line {
            label: "Klang",
            value: if on.is_empty() {
                "Unbearbeitet".to_string()
            } else {
                on.join(", ")
            },
            warning: None,
            step: Step::Voice,
        },
    ]
}

/// One overview row: label, the value wrapping beside it, „Ändern“ at the
/// right edge. Returns whether „Ändern“ was clicked.
fn summary_row(ui: &mut Ui, divider: bool, line: &Line) -> bool {
    if divider {
        ui.separator();
    }
    let mut clicked = false;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            // Top-aligned: a value that wraps runs down from the label's
            // line, with the label and „Ändern“ beside its first line.
            ui.horizontal_top(|ui| {
                widgets::label_column(ui, SUMMARY_LABEL_W, 30.0, line.label);
                let button_w = widgets::button_width(ui, icon::PENCIL_SIMPLE, "Ändern");
                let value_w = (ui.available_width() - button_w - 12.0).max(80.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(value_w, 30.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(value_w);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        // Down to the label column's text line.
                        ui.add_space(6.0);
                        ui.add(egui::Label::new(RichText::new(&line.value).size(13.0)).wrap());
                        if let Some(warning) = line.warning {
                            ui.label(
                                RichText::new(warning)
                                    .size(11.5)
                                    .color(theme::tokens(ui.visuals()).connecting),
                            );
                        }
                    },
                );
                widgets::rtl(ui, 30.0, |ui| {
                    clicked = widgets::button(
                        ui,
                        widgets::ButtonKind::Neutral,
                        icon::PENCIL_SIMPLE,
                        "Ändern",
                    )
                    .on_hover_text(format!("Zurück zu „{}“", line.step.title()))
                    .clicked();
                });
            });
        });
    clicked
}

/// The four steps as numbered dots with their names: done ones checked,
/// the current one filled, the rest outlined.
fn stepper(ui: &mut Ui, current: Step) {
    let tok = theme::tokens(ui.visuals());
    let line = ui.visuals().widgets.inactive.bg_stroke.color;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for step in Step::ALL {
            if step != Step::Devices {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(20.0, 22.0), egui::Sense::hover());
                ui.painter().line_segment(
                    [rect.left_center(), rect.right_center()],
                    egui::Stroke::new(1.0, line),
                );
            }
            let done = step.index() < current.index();
            let now = step == current;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
            let painter = ui.painter();
            if done || now {
                painter.circle_filled(rect.center(), 11.0, tok.accent);
            } else {
                painter.circle_stroke(rect.center(), 10.5, egui::Stroke::new(1.0, line));
            }
            let mark = if done {
                icon::CHECK.to_string()
            } else {
                (step.index() + 1).to_string()
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                mark,
                egui::FontId::new(11.5, super::fonts::semibold()),
                if done || now {
                    tok.on_accent
                } else {
                    tok.dim_text
                },
            );
            let (family, color) = if now {
                (super::fonts::semibold(), ui.visuals().text_color())
            } else if done {
                (egui::FontFamily::Proportional, tok.muted_text)
            } else {
                (egui::FontFamily::Proportional, tok.dim_text)
            };
            ui.label(
                RichText::new(step.title())
                    .font(egui::FontId::new(13.0, family))
                    .color(color),
            );
        }
    });
}

/// One of the two ways to be heard, picked by a click anywhere on it:
/// accent border and tint while chosen.
fn choice_card(
    ui: &mut Ui,
    selected: bool,
    glyph: &str,
    title: &str,
    text: &str,
    width: f32,
) -> egui::Response {
    const MARGIN: i8 = 14;
    let tok = theme::tokens(ui.visuals());
    let (fill, stroke) = if selected {
        (tok.accent_tint, egui::Stroke::new(1.5, tok.accent))
    } else {
        (
            ui.visuals().faint_bg_color,
            egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
        )
    };
    let card = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
        egui::Frame::new()
            .fill(fill)
            .stroke(stroke)
            .corner_radius(egui::CornerRadius::same(theme::R_CARD))
            .inner_margin(egui::Margin::same(MARGIN))
            .show(ui, |ui| {
                // Top-down inside, whatever row the cards sit in.
                ui.vertical(|ui| {
                    ui.set_width(width - 2.0 * f32::from(MARGIN));
                    // Both cards as tall as the taller text needs, so
                    // their edges line up.
                    ui.set_min_height(64.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(glyph).size(18.0).color(if selected {
                            tok.accent
                        } else {
                            tok.muted_text
                        }));
                        ui.label(theme::strong_text(title));
                        if selected {
                            widgets::rtl(ui, 20.0, |ui| {
                                ui.label(
                                    RichText::new(icon::CHECK_CIRCLE)
                                        .size(18.0)
                                        .color(tok.accent),
                                );
                            });
                        }
                    });
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(RichText::new(text).size(12.0).color(tok.muted_text))
                            .wrap(),
                    );
                });
            });
    });
    let response = card.response;
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::RadioButton,
            ui.is_enabled(),
            selected,
            title,
        )
    });
    if response.hovered() {
        ui.painter().rect_filled(
            response.rect,
            egui::CornerRadius::same(theme::R_CARD),
            tok.hover_overlay,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A level bar [`METER_W`] wide, eased like the main window's, filled in
/// `color`, with a tick at `threshold` where one applies. `value` is 0…1.
fn meter(ui: &mut Ui, value: f32, threshold: Option<f32>, color: egui::Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(METER_W, 22.0), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let value = ui
        .ctx()
        .animate_value_with_time(response.id, value.clamp(0.0, 1.0), 0.12);
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 6.0));
    let painter = ui.painter();
    painter.rect_filled(track, 3.0, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        track,
        3.0,
        egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    if value > 0.0 {
        let mut bar = track;
        bar.max.x = track.left() + track.width() * value;
        painter.rect_filled(bar, 3.0, color);
    }
    if let Some(th) = threshold {
        let x = track.left() + track.width() * th.clamp(0.0, 1.0);
        painter.line_segment(
            [
                egui::pos2(x, track.top() - 4.0),
                egui::pos2(x, track.bottom() + 4.0),
            ],
            egui::Stroke::new(1.5, theme::tokens(ui.visuals()).muted_text),
        );
    }
    response
}

/// A dot and a word in a box of fixed width, so the row does not move when
/// the word changes: lit while `on`.
fn lamp(ui: &mut Ui, on: bool, lit: &str, idle: &str) -> egui::Response {
    let tok = theme::tokens(ui.visuals());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(140.0, 26.0), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let (fill, dot, text, color) = if on {
        (
            tok.accent_tint,
            tok.connected,
            lit,
            ui.visuals().text_color(),
        )
    } else {
        (
            ui.visuals().extreme_bg_color,
            tok.dim_text,
            idle,
            tok.dim_text,
        )
    };
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(theme::R_CTRL), fill);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(theme::R_CTRL),
        egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    let center = egui::pos2(rect.left() + 14.0, rect.center().y);
    painter.circle_filled(center, 4.0, dot);
    painter.text(
        egui::pos2(rect.left() + 26.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::new(12.5, super::fonts::semibold()),
        color,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, description: &str, kind: DeviceKind) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            description: description.into(),
            kind,
        }
    }

    #[test]
    fn steps_run_from_devices_to_the_overview() {
        assert_eq!(Step::Devices.prev(), None);
        assert_eq!(Step::Devices.next(), Some(Step::Speak));
        assert_eq!(Step::Speak.next(), Some(Step::Voice));
        assert_eq!(Step::Voice.next(), Some(Step::Summary));
        assert_eq!(Step::Summary.next(), None);
        assert_eq!(Step::Summary.prev(), Some(Step::Voice));
        for step in Step::ALL {
            assert_eq!(Step::ALL[step.index()], step);
        }
    }

    /// The overview names devices as the lists do, the key as the main
    /// window does, and warns where speaking would not work.
    #[test]
    fn the_overview_says_what_is_set() {
        let devices = [
            device("mic", "Jabra Evolve2 65 Mono", DeviceKind::Input),
            device("hs", "Jabra Evolve2 65 Mono", DeviceKind::Output),
        ];
        let mut s = Settings::default();
        s.audio.input_device = Some("mic".into());
        s.audio.output_device = Some("gone".into());
        s.audio.output_volume = 80.0;
        let lines = summary(&s, &devices, "portal", true);
        let steps: Vec<_> = lines.iter().map(|l| (l.label, l.step)).collect();
        assert_eq!(
            steps,
            [
                ("Mikrofon", Step::Devices),
                ("Lautsprecher", Step::Devices),
                ("Senden", Step::Speak),
                ("Klang", Step::Voice),
            ]
        );
        assert_eq!(lines[0].value, "Jabra Evolve2 65 Mono");
        assert_eq!(lines[1].value, "Fehlt – System-Standard läuft · 80 %");
        assert_eq!(lines[2].value, "Automatisch · Schwelle automatisch");
        assert_eq!(lines[2].warning, None);
        assert_eq!(
            lines[3].value,
            "Rauschunterdrückung, Echo-Unterdrückung, Pegel automatisch, Empfangsfeedback, Systemmikrofon automatisch, Selbst gleich laut ankommen, Andere gleich laut hören, Stimmoptimierung (Mittel)"
        );

        s.audio.vad_auto = false;
        s.audio.vad_threshold = 0.62;
        assert_eq!(
            summary(&s, &devices, "portal", true)[2].value,
            "Automatisch · Schwelle 0.62"
        );

        // Push-to-talk: the portal shows its trigger, evdev the key.
        s.audio.mode = ModeSetting::Ptt;
        let ptt = &summary(&s, &devices, "portal", true)[2];
        assert_eq!(ptt.value, "Taste halten · CTRL+SHIFT+F12");
        assert_eq!(ptt.warning, None);
        assert_eq!(
            summary(&s, &devices, "evdev (3 devices)", true)[2].value,
            "Taste halten · RIGHTCTRL"
        );
        assert!(summary(&s, &devices, crate::hotkey::BACKEND_NONE, true)[2]
            .warning
            .is_some());
        s.hotkeys.ptt = KeyBinding::default();
        let none = &summary(&s, &devices, "evdev (3 devices)", true)[2];
        assert_eq!(none.value, "Taste halten · keine Taste");
        assert!(none.warning.is_some());

        // Echo only where it exists; the optimizer with its strength.
        s.audio.denoise = false;
        s.audio.normalize_outgoing = false;
        s.audio.normalize_incoming = false;
        s.audio.voice_opt = true;
        s.audio.voice_strength = VoiceStrength::Strong;
        assert_eq!(
            summary(&s, &devices, "portal", false)[3].value,
            "Stimmoptimierung (Stark)"
        );
        s.audio.voice_opt = false;
        s.audio.aec = false;
        assert_eq!(
            summary(&s, &devices, "portal", true)[3].value,
            "Unbearbeitet"
        );
    }
}
