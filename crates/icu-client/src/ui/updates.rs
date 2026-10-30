//! Update controls shared by the main window and settings. Linux has no
//! installation action in its type/API, even when metadata names Windows files.
use super::{design, widgets};
use crate::update::{Action, Snapshot};

pub const LINUX_HINT: &str = "Neue Version verfügbar. Bitte über deine Paketverwaltung bzw. den bisherigen Installationsweg aktualisieren.";

pub fn notice(
    ui: &mut egui::Ui,
    state: &Snapshot,
    required: Option<&str>,
    connected: bool,
    confirm: &mut bool,
) -> Option<Action> {
    if required.is_none() && (state.available.is_none() || state.dismissed) && !*confirm {
        return None;
    }
    let mut action = None;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            if let Some(required) = required {
                ui.label(format!(
                    "Der Server verlangt mindestens Version {required} (diese: {}).",
                    env!("CARGO_PKG_VERSION")
                ));
            }
            if state.available.is_some() && !state.dismissed || *confirm {
                action = available(ui, state, connected, confirm, true);
            } else if !state.busy()
                && design::button(ui, widgets::ButtonKind::Neutral, "", "Nach Updates suchen")
                    .clicked()
            {
                action = Some(Action::Check);
            }
        });
    action
}
pub fn settings(
    ui: &mut egui::Ui,
    state: &Snapshot,
    connected: bool,
    confirm: &mut bool,
) -> Option<Action> {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width().min(260.0), 34.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            let mut action = None;
            ui.label(state.status());
            ui.add_enabled_ui(!state.busy(), |ui| {
                if design::button(ui, widgets::ButtonKind::Neutral, "", "Nach Updates suchen")
                    .clicked()
                {
                    action = Some(Action::Check);
                }
            });
            if state.available.is_some() {
                action = available(ui, state, connected, confirm, false).or(action);
            }
            action
        },
    )
    .inner
}
fn available(
    ui: &mut egui::Ui,
    state: &Snapshot,
    connected: bool,
    confirm: &mut bool,
    dismissible: bool,
) -> Option<Action> {
    let mut action = None;
    #[cfg(not(windows))]
    {
        let _ = (state, connected, confirm);
        ui.label(LINUX_HINT);
        ui.horizontal_wrapped(|ui| {
            ui.hyperlink_to("Release-Informationen", crate::update::RELEASE_INFO);
            if dismissible
                && design::button(ui, widgets::ButtonKind::Neutral, "", "Später").clicked()
            {
                action = Some(Action::Dismiss);
            }
        });
    }
    #[cfg(windows)]
    {
        if state.busy() {
            ui.label(state.status());
            if let crate::update::Phase::Downloading { received, total } = state.phase {
                ui.add(
                    egui::ProgressBar::new(received as f32 / total.max(1) as f32).show_percentage(),
                );
            }
        } else if *confirm {
            ui.label(if connected {"ICU wird nach dem Download neu gestartet. Die aktuelle Verbindung wird dabei beendet."} else {"ICU wird nach dem Download neu gestartet. Deine Einstellungen und Zugangsdaten bleiben erhalten."});
            ui.horizontal_wrapped(|ui| {
                if design::button(
                    ui,
                    widgets::ButtonKind::Primary,
                    "",
                    "Herunterladen und neu starten",
                )
                .clicked()
                {
                    *confirm = false;
                    action = Some(Action::Install);
                }
                if design::button(ui, widgets::ButtonKind::Neutral, "", "Abbrechen").clicked() {
                    *confirm = false;
                }
            });
        } else {
            if let Some(release) = &state.available {
                ui.label(format!("Neue Version {} verfügbar.", release.version));
            }
            if let Some(error) = &state.error {
                ui.label(error);
            }
            ui.horizontal_wrapped(|ui| {
                if design::button(ui, widgets::ButtonKind::Primary, "", "Jetzt aktualisieren")
                    .clicked()
                {
                    *confirm = true;
                }
                if dismissible
                    && design::button(ui, widgets::ButtonKind::Neutral, "", "Später").clicked()
                {
                    action = Some(Action::Dismiss);
                }
                ui.hyperlink_to("Release-Informationen", crate::update::RELEASE_INFO);
            });
        }
    }
    action
}
