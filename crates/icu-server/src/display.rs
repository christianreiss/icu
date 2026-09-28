//! Human-facing terminal output. JSON and passport bytes bypass this module.

use std::{fmt::Write as _, io::IsTerminal, path::Path, sync::OnceLock};

use clap::ValueEnum;
use unicode_width::UnicodeWidthStr;

use crate::{admin::AdminUser, backup, error::Result};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

static COLOR_CHOICE: OnceLock<ColorChoice> = OnceLock::new();

/// Set once by the binary, before it starts an admin command.
pub fn configure(choice: ColorChoice) {
    let _ = COLOR_CHOICE.set(choice);
}

fn color_enabled(choice: ColorChoice, terminal: bool, no_color: bool, dumb: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => terminal && !no_color && !dumb,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub color: bool,
    pub width: usize,
}

impl Style {
    pub fn current() -> Self {
        let choice = *COLOR_CHOICE.get().unwrap_or(&ColorChoice::Auto);
        let terminal = std::io::stdout().is_terminal();
        let color = color_enabled(
            choice,
            terminal,
            std::env::var_os("NO_COLOR").is_some(),
            std::env::var("TERM").is_ok_and(|term| term == "dumb"),
        );
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|width| *width > 0)
            .or_else(|| {
                rustix::termios::tcgetwinsize(std::io::stdout())
                    .ok()
                    .map(|size| usize::from(size.ws_col))
                    .filter(|width| *width > 0)
            })
            .unwrap_or(80);
        Self { color, width }
    }

    pub fn paint(self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    pub fn heading(self, text: &str) -> String {
        self.paint("1;36", text)
    }

    pub fn subtle(self, text: &str) -> String {
        self.paint("2", text)
    }

    pub fn good(self, text: &str) -> String {
        self.paint("1;32", text)
    }

    pub fn caution(self, text: &str) -> String {
        self.paint("1;33", text)
    }

    pub fn bad(self, text: &str) -> String {
        self.paint("1;31", text)
    }
}

fn flag(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn state(user: &AdminUser) -> &'static str {
    if user.revoked {
        "revoked"
    } else if user.disabled {
        "disabled"
    } else if user.pending {
        "pending"
    } else if user.online {
        "online"
    } else {
        "offline"
    }
}

fn styled_state(style: Style, user: &AdminUser) -> String {
    let text = state(user);
    match text {
        "online" => style.good(text),
        "revoked" | "disabled" => style.bad(text),
        "pending" => style.caution(text),
        _ => style.subtle(text),
    }
}

/// Render all user fields; a narrow terminal gets records instead of a wide table.
pub fn users(style: Style, users: &[AdminUser]) -> String {
    let mut out = format!("{}\n", style.heading(&format!("Users ({})", users.len())));
    if users.is_empty() {
        out.push_str(&style.subtle("id  name  state  role  expires  fingerprint"));
        out.push('\n');
        out.push_str("No users on record.\n");
        return out;
    }
    if style.width >= 115 {
        out.push_str(&style.subtle(
            "  id  name                  state      role   expires               fingerprint",
        ));
        out.push('\n');
        for user in users {
            let name_width = UnicodeWidthStr::width(user.name.as_str());
            let pad = 20usize.saturating_sub(name_width);
            let role = if user.is_admin { "admin" } else { "user" };
            let state_text = state(user);
            let state = format!("{state_text:<10}");
            let state = match state_text {
                "online" => style.good(&state),
                "revoked" | "disabled" => style.bad(&state),
                "pending" => style.caution(&state),
                _ => style.subtle(&state),
            };
            let _ = writeln!(
                out,
                "{:>4}  {}{}  {} {:<6} {:<21} {}",
                user.user_id,
                user.name,
                " ".repeat(pad),
                state,
                role,
                user.expires,
                user.fingerprint
            );
            let _ = writeln!(
                out,
                "      {}  issued {}  revoked {}  disabled {}  pending {}  online {}  muted {}  deafened {}",
                style.subtle("details"),
                user.issued,
                flag(user.revoked),
                flag(user.disabled),
                flag(user.pending),
                flag(user.online),
                flag(user.muted),
                flag(user.deafened),
            );
        }
    } else {
        for user in users {
            let role = if user.is_admin { "admin" } else { "user" };
            let _ = writeln!(
                out,
                "  {}  {}  {}  {}",
                style.heading(&format!("#{}", user.user_id)),
                user.name,
                styled_state(style, user),
                role
            );
            let _ = writeln!(
                out,
                "      fingerprint {}  expires {}",
                user.fingerprint, user.expires
            );
            let _ = writeln!(out, "      issued {}", user.issued);
            let _ = writeln!(
                out,
                "      revoked {} · disabled {} · pending {} · online {}",
                flag(user.revoked),
                flag(user.disabled),
                flag(user.pending),
                flag(user.online)
            );
            let _ = writeln!(
                out,
                "      muted {} · deafened {}",
                flag(user.muted),
                flag(user.deafened)
            );
        }
    }
    out
}

fn number(value: &serde_json::Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

pub fn status(style: Style, value: &serde_json::Value) -> String {
    let mut out = format!("{}\n", style.heading("Server status"));
    let online = number(value, "online");
    let max = number(value, "max_users");
    let _ = writeln!(
        out,
        "  online       {}/{}",
        style.good(&online.to_string()),
        max
    );
    let _ = writeln!(out, "  uptime       {} s", number(value, "uptime_s"));
    let _ = writeln!(
        out,
        "  connects     {} open · {} in handshake · {} since start",
        number(value, "open_connections"),
        number(value, "handshakes"),
        number(value, "connections")
    );
    let dropped = number(value, "com_dropped");
    let dropped_text = dropped.to_string();
    let dropped_text = if dropped > 0 {
        style.caution(&dropped_text)
    } else {
        dropped_text
    };
    let _ = writeln!(
        out,
        "  com tag      {dropped_text} datagrams dropped since start"
    );
    if let Some(relay) = value.get("relay") {
        let _ = writeln!(out, "{}", style.heading("Relay since start"));
        let _ = writeln!(
            out,
            "  audio        {} in · {} out",
            number(relay, "datagrams_in"),
            number(relay, "datagrams_out")
        );
        let _ = writeln!(
            out,
            "  video        {} in · {} out",
            number(relay, "video_frames_in"),
            number(relay, "video_frames_out")
        );
        let _ = writeln!(out, "  control      {} in", number(relay, "control_in"));
        if let Some(dropped) = relay.get("dropped").and_then(serde_json::Value::as_object) {
            let items: Vec<_> = dropped
                .iter()
                .filter_map(|(key, value)| {
                    let count = value.as_u64()?;
                    (count > 0).then(|| format!("{key}={count}"))
                })
                .collect();
            let _ = writeln!(
                out,
                "  dropped      {}",
                if items.is_empty() {
                    "none".to_owned()
                } else {
                    style.caution(&items.join(" "))
                }
            );
        }
    }
    out
}

pub fn backups(style: Style, dir: &Path) -> Result<String> {
    let snapshots = backup::list(dir)?;
    let mut out = format!(
        "{}\n",
        style.heading(&format!("Backups ({})", snapshots.len()))
    );
    if snapshots.is_empty() {
        let _ = writeln!(out, "No snapshots in {}", dir.display());
        return Ok(out);
    }
    if style.width >= 90 {
        out.push_str(
            &style
                .subtle("snapshot             created               reason        users  version"),
        );
        out.push('\n');
    }
    for path in snapshots {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        match backup::read_manifest(&path) {
            Ok(manifest) if style.width >= 90 => {
                let _ = writeln!(
                    out,
                    "{name:<20} {:<21} {:<12} {:>5}  {}",
                    manifest.created, manifest.reason, manifest.users, manifest.server_version
                );
            }
            Ok(manifest) => {
                let _ = writeln!(out, "  {}", style.heading(&name));
                let _ = writeln!(
                    out,
                    "    {} · {} · {} users · version {}",
                    manifest.created, manifest.reason, manifest.users, manifest.server_version
                );
            }
            Err(error) => {
                let _ = writeln!(
                    out,
                    "  {name}  {}",
                    style.bad(&format!("unreadable: {error}"))
                );
            }
        }
    }
    let _ = writeln!(out, "{}", style.subtle(&format!("in {}", dir.display())));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_policy() {
        assert!(color_enabled(ColorChoice::Auto, true, false, false));
        assert!(!color_enabled(ColorChoice::Auto, false, false, false));
        assert!(!color_enabled(ColorChoice::Auto, true, true, false));
        assert!(!color_enabled(ColorChoice::Auto, true, false, true));
        assert!(color_enabled(ColorChoice::Always, false, true, true));
        assert!(!color_enabled(ColorChoice::Never, true, false, false));
    }

    #[test]
    fn status_keeps_labels_and_uses_color_only_when_enabled() {
        let value = serde_json::json!({"online": 2, "max_users": 4, "uptime_s": 5});
        let plain = status(
            Style {
                color: false,
                width: 40,
            },
            &value,
        );
        assert!(plain.contains("online"));
        assert!(plain.contains("2/4"));
        assert!(!plain.contains("\x1b["));
        assert!(status(
            Style {
                color: true,
                width: 40
            },
            &value
        )
        .contains("\x1b["));
    }

    #[test]
    fn users_keep_all_fields_in_narrow_and_wide_views() {
        let user: AdminUser = serde_json::from_value(serde_json::json!({
            "user_id": 7, "name": "Jörg", "fingerprint": "abc123", "issued": "2026-09-01",
            "expires": "2027-09-01", "revoked": false, "online": true, "is_admin": true,
            "disabled": false, "pending": false, "muted": true, "deafened": false
        }))
        .unwrap();
        for width in [60, 160] {
            let plain = users(
                Style {
                    color: false,
                    width,
                },
                std::slice::from_ref(&user),
            );
            for field in [
                "Jörg",
                "abc123",
                "2026-09-01",
                "2027-09-01",
                "online",
                "admin",
                "muted",
                "deafened",
            ] {
                assert!(plain.contains(field), "width {width}: {plain}");
            }
            assert!(!plain.contains("\x1b["));
        }
        assert!(users(
            Style {
                color: true,
                width: 60
            },
            &[user]
        )
        .contains("\x1b["));
    }
}
