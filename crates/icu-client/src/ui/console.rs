//! Administrative command input for the active desktop session.
use crate::engine::{AppState, Command};
use egui::{Key, Ui};
use icu_proto::UserSummary;
use std::collections::VecDeque;

const HELP: &str = "show users · show status\ninvite [--name NAME] [--ttl SEKUNDEN]\npromote | demote | disable | enable | kick | revoke NAME/ID\nmute | unmute | deafen | undeafen NAME/ID";

#[derive(Default)]
pub(super) struct Console {
    open: bool,
    lost: bool,
    input: String,
    history: VecDeque<String>,
    cursor: usize,
    lines: VecDeque<String>,
    seen: u64,
    epoch: u64,
    list: Option<u64>,
    revoke: Option<(u32, String)>,
}

impl Console {
    pub fn sync(&mut self, ui: &mut Ui, state: &AppState) {
        if self.epoch != state.admin_epoch {
            let was_open = self.open;
            *self = Self {
                epoch: state.admin_epoch,
                ..Default::default()
            };
            self.lost = was_open;
        }
        if self.lost {
            ui.horizontal_wrapped(|ui| {
                ui.label("Serverkonsole geschlossen. Bei Befehlen ohne Serverbestätigung ist der Ausgang unbekannt.");
                if ui.button("Schließen").clicked() { self.lost = false; }
            });
        }
    }

    fn line(&mut self, text: String) {
        if self.lines.len() == 32 {
            self.lines.pop_front();
        }
        self.lines.push_back(text);
    }

    /// Returns true when the invitation card should open.
    pub fn ui(&mut self, ui: &mut Ui, state: &AppState, cmds: &mut Vec<Command>) -> bool {
        if ui.toggle_value(&mut self.open, "Serverkonsole").clicked() && self.open {
            self.seen = state.admin_results.back().map_or(0, |r| r.0);
        }
        if !self.open {
            return false;
        }
        for (seq, text) in &state.admin_results {
            if *seq > self.seen {
                self.line(text.to_string());
                self.seen = *seq;
            }
        }
        if self.list.is_some_and(|g| g != state.user_list_generation) {
            self.list = None;
            self.line(format_users(&state.user_list));
        }
        egui::ScrollArea::vertical()
            .id_salt("console_output")
            .max_height(220.)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.lines {
                    ui.monospace(line);
                }
            });
        if let Some((id, name)) = self.revoke.clone() {
            ui.label(format!(
                "Alle Zertifikate von {name} (ID {id}) dauerhaft widerrufen?"
            ));
            if ui.button("Widerruf bestätigen").clicked() {
                cmds.push(Command::RevokeUser { user_id: id });
                self.revoke = None;
            }
            if ui.button("Abbrechen").clicked() {
                self.revoke = None;
            }
            return false;
        }
        let edit = ui.add(
            egui::TextEdit::singleline(&mut self.input)
                .hint_text("Befehl eingeben · ? für Hilfe")
                .char_limit(256),
        );
        edit.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Konsolenbefehl")
        });
        if edit.has_focus() {
            if ui.input(|i| i.key_pressed(Key::ArrowUp)) {
                self.cursor = self.cursor.saturating_sub(1);
                self.input = self.history.get(self.cursor).cloned().unwrap_or_default();
            }
            if ui.input(|i| i.key_pressed(Key::ArrowDown)) {
                self.cursor = (self.cursor + 1).min(self.history.len());
                self.input = self.history.get(self.cursor).cloned().unwrap_or_default();
            }
        }
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if !ui.button("Ausführen").clicked() && !enter {
            return false;
        }
        edit.request_focus();
        let line = std::mem::take(&mut self.input);
        if line.trim().is_empty() {
            return false;
        }
        if self.history.len() == 32 {
            self.history.pop_front();
        }
        self.history.push_back(line.clone());
        self.cursor = self.history.len();
        self.line(format!("> {line}"));
        match parse(
            &line,
            &state.user_list,
            state.conn_info.as_ref().is_some_and(|c| c.minor >= 9),
        ) {
            Ok(Action::Help) => self.line(HELP.into()),
            Ok(Action::Confirm(id, name)) => self.revoke = Some((id, name)),
            Ok(Action::Send(cmd)) => {
                let invite = matches!(cmd, Command::CreateInvite { .. });
                if matches!(cmd, Command::RequestUserList) {
                    self.list = Some(state.user_list_generation);
                }
                cmds.push(cmd);
                return invite;
            }
            Err(e) => self.line(e.into()),
        }
        false
    }
}

fn format_users(users: &[UserSummary]) -> String {
    users
        .iter()
        .map(|u| {
            if u.user_id == 0 {
                return u.name.clone();
            }
            let mut flags = vec![
                if u.is_admin { "Admin" } else { "Mitglied" },
                if u.online { "online" } else { "offline" },
            ];
            for (on, flag) in [
                (u.disabled, "gesperrt"),
                (u.revoked, "widerrufen"),
                (u.pending, "eingeladen"),
                (u.muted, "stumm"),
                (u.deafened, "taub"),
            ] {
                if on {
                    flags.push(flag);
                }
            }
            format!("{}  {}  {}", u.user_id, u.name, flags.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

enum Action {
    Help,
    Confirm(u32, String),
    Send(Command),
}

fn parse(line: &str, users: &[UserSummary], modern: bool) -> Result<Action, &'static str> {
    let line = line.trim();
    let (verb, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let arg = arg.trim();
    let update = "Dieser Befehl benötigt ein Serverupdate auf Protokoll-Minor 9.";
    let cmd = match (verb, arg) {
        ("?" | "help", "") => return Ok(Action::Help),
        ("show", "users") => Command::RequestUserList,
        ("show", "status") if modern => Command::AdminStatus,
        ("show", "status") => return Err(update),
        ("invite", args) => {
            let mut words = args.split_whitespace().peekable();
            let mut name = None;
            let mut ttl = None;
            while let Some(option) = words.next() {
                let value = words.next().ok_or("Optionswert fehlt.")?;
                match option {
                    "--name" if name.is_none() => {
                        let mut full = value.to_owned();
                        while words.peek().is_some_and(|word| !word.starts_with("--")) {
                            full.push(' ');
                            full.push_str(words.next().unwrap());
                        }
                        icu_proto::validate_name(&full)
                            .map_err(|_| "Ungültiger Einladungsname.")?;
                        name = Some(full);
                    }
                    "--ttl" if ttl.is_none() => {
                        ttl = Some(
                            value
                                .parse::<u64>()
                                .ok()
                                .filter(|n| (1..=icu_proto::limits::MAX_INVITE_TTL_S).contains(n))
                                .ok_or("Gültigkeit in Sekunden (1–604800) angeben.")?,
                        )
                    }
                    _ => return Err("invite [--name NAME] [--ttl SEKUNDEN]"),
                }
            }
            Command::CreateInvite {
                name: name.unwrap_or_default(),
                ttl_s: ttl.unwrap_or(0),
            }
        }
        (
            "promote" | "demote" | "disable" | "enable" | "kick" | "revoke" | "mute" | "unmute"
            | "deafen" | "undeafen",
            target,
        ) => {
            let mut matches = users.iter().filter(|u| {
                u.user_id != 0
                    && if let Some(id) = target.strip_prefix("id:") {
                        id.parse::<u32>() == Ok(u.user_id)
                    } else if let Some(name) = target.strip_prefix("name:") {
                        u.name == name
                    } else {
                        u.name == target || target.parse::<u32>() == Ok(u.user_id)
                    }
            });
            let user = matches
                .next()
                .ok_or("Nutzer fehlt in der aktuellen Liste. Mit show users prüfen.")?;
            if matches.next().is_some() {
                return Err("Mehrdeutiges Ziel. id:NUMMER oder name:NAME verwenden.");
            }
            let user_id = user.user_id;
            match verb {
                "promote" | "demote" => Command::SetUserAdmin {
                    user_id,
                    is_admin: verb == "promote",
                },
                "disable" | "enable" => Command::SetUserDisabled {
                    user_id,
                    disabled: verb == "disable",
                },
                "kick" => Command::KickUser { user_id },
                "revoke" => return Ok(Action::Confirm(user_id, user.name.clone())),
                _ if modern => Command::ChangeRestriction {
                    user_id,
                    deafen: verb.ends_with("deafen"),
                    enabled: !verb.starts_with("un"),
                },
                _ => return Err(update),
            }
        }
        _ => return Err("Unbekannter Befehl. ? zeigt die Hilfe."),
    };
    Ok(Action::Send(cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_commands_remain_available_on_minor_eight() {
        assert!(matches!(
            parse("show users", &[], false),
            Ok(Action::Send(Command::RequestUserList))
        ));
        assert!(parse("show status", &[], false).is_err());
        assert!(matches!(
            parse("show status", &[], true),
            Ok(Action::Send(Command::AdminStatus))
        ));
    }

    #[test]
    fn invite_options_validate_before_sending() {
        assert!(matches!(parse("invite --ttl 3600 --name bob",&[],false),
            Ok(Action::Send(Command::CreateInvite { name, ttl_s: 3600 })) if name=="bob"));
        for invalid in [
            "invite --ttl 0",
            "invite --ttl 604801",
            "invite --name",
            "invite --ttl 2 --ttl 3",
        ] {
            assert!(parse(invalid, &[], true).is_err());
        }
    }
}
