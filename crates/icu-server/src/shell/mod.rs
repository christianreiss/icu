//! `icu-server shell`: a persistent, Cisco IOS-style admin prompt
//! (`docs/reference/server.md` §5.1). Every command it runs is its own
//! `cli::dispatch()` call over the same one-shot admin socket every other
//! `icu-server` subcommand uses (`crate::cli`, `crate::admin`) — the shell
//! holds no persistent connection and adds nothing to the wire protocol; it
//! is purely a new, repeatedly-invoking client of what is already there.
//!
//! `run`/`restore` are deliberately not available here: `restore` needs the
//! server stopped and takes `flock` directly, which a live session talking
//! to that same server over the socket cannot coexist with.

pub mod command;
mod helper;
mod history;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use rustyline::{error::ReadlineError, history::DefaultHistory, Editor};

use crate::{
    admin::AdminUser,
    cli,
    config::Config,
    display::{self, Style},
    error::{Error, Result},
    shell::command::Action,
    AdminRequest,
};
use helper::ShellHelper;

const PROMPT: &str = "icu-server> ";

type ShellEditor = Editor<ShellHelper, DefaultHistory>;

pub async fn run(config_path: &Path, config: &Config) -> Result<()> {
    let users = Arc::new(Mutex::new(fetch_users(config).await.unwrap_or_else(|e| {
        eprintln!("icu-server: cannot reach the admin socket yet: {e}");
        Vec::new()
    })));

    let mut editor: ShellEditor =
        Editor::new().map_err(|e| Error::msg(format!("cannot start the shell: {e}")))?;
    editor.set_helper(Some(ShellHelper {
        users: users.clone(),
    }));
    let history_path = history::history_path();
    if let Some(path) = &history_path {
        let _ = editor.load_history(path);
    }

    println!(
        "{}  {}",
        Style::current().heading("ICU admin"),
        Style::current().subtle("help or ? for commands · exit to leave")
    );

    loop {
        let (returned, line) = read_line(editor, PROMPT.to_string()).await?;
        editor = returned;
        let line = match line {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(e) => return Err(Error::msg(format!("shell input error: {e}"))),
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _ = editor.add_history_entry(trimmed);

        if trimmed == "?" || trimmed.ends_with('?') {
            print_help_for(
                trimmed.trim_end_matches('?').trim_end(),
                &users.lock().expect("shell user cache poisoned"),
            );
            continue;
        }

        let (action, confirm) = match command::resolve_line(trimmed) {
            Ok(v) => v,
            Err(e) => {
                println!("{} {e}", Style::current().bad("error:"));
                continue;
            }
        };

        if let Some(prompt) = confirm {
            let (returned, answer) = read_line(editor, prompt).await?;
            editor = returned;
            let yes = matches!(answer, Ok(l) if l.trim().to_ascii_lowercase().starts_with('y'));
            if !yes {
                println!("{}", Style::current().subtle("cancelled"));
                continue;
            }
        }

        match action {
            Action::Exit => break,
            Action::Help => print_top_help(),
            Action::Refresh => match fetch_users(config).await {
                Ok(list) => {
                    println!("{} user(s)", list.len());
                    *users.lock().expect("shell user cache poisoned") = list;
                }
                Err(e) => println!(
                    "{} cannot refresh users: {e}",
                    Style::current().bad("error:")
                ),
            },
            Action::ShowBackups => show_backups(config),
            // Local and read-only, like `show backups`: no admin request, and
            // a failing check must not end the session the way it ends the
            // one-shot CLI.
            Action::Doctor { raw_json } => {
                let report = cli::doctor::diagnose(config_path).await;
                if raw_json {
                    match serde_json::to_string(&report.to_json()) {
                        Ok(text) => println!("{text}"),
                        Err(e) => println!("{} {e}", Style::current().bad("error:")),
                    }
                } else {
                    report.print();
                }
            }
            Action::Admin {
                request,
                raw_json,
                out,
            } => run_admin(config, request, raw_json, out, &users).await,
        }
    }

    if let Some(path) = &history_path {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = editor.save_history(path) {
            tracing::debug!(error = %e, path = %path.display(), "cannot save shell history");
        }
    }
    Ok(())
}

/// Reads one line on a blocking task (rustyline's `readline()` blocks on
/// stdin) and hands the editor back so the next call can reuse it — used
/// both for the main prompt and for a `[y/N]` confirmation, so both share
/// one editor's history and state.
async fn read_line(
    mut editor: ShellEditor,
    prompt: String,
) -> Result<(ShellEditor, std::result::Result<String, ReadlineError>)> {
    tokio::task::spawn_blocking(move || {
        let line = editor.readline(&prompt);
        (editor, line)
    })
    .await
    .map_err(|e| Error::msg(format!("shell input task panicked: {e}")))
}

async fn fetch_users(config: &Config) -> Result<Vec<AdminUser>> {
    let mut value = cli::dispatch(config, AdminRequest::Users).await?;
    cli::require_ok(&value)?;
    Ok(serde_json::from_value(value["users"].take())?)
}

fn is_mutating(request: &AdminRequest) -> bool {
    matches!(
        request,
        AdminRequest::Invite { .. }
            | AdminRequest::Revoke { .. }
            | AdminRequest::CancelInvite { .. }
            | AdminRequest::Promote { .. }
            | AdminRequest::Demote { .. }
            | AdminRequest::Disable { .. }
            | AdminRequest::Enable { .. }
            | AdminRequest::Kick { .. }
            | AdminRequest::Mute { .. }
            | AdminRequest::Unmute { .. }
            | AdminRequest::Deafen { .. }
            | AdminRequest::Undeafen { .. }
    )
}

async fn run_admin(
    config: &Config,
    request: AdminRequest,
    raw_json: bool,
    out: Option<PathBuf>,
    users: &Arc<Mutex<Vec<AdminUser>>>,
) {
    let mutating = is_mutating(&request);
    match cli::dispatch(config, request).await {
        Ok(value) => print_response(&value, raw_json, out.as_deref()),
        Err(e) => println!("{} {e}", Style::current().bad("error:")),
    }
    if mutating {
        if let Ok(list) = fetch_users(config).await {
            *users.lock().expect("shell user cache poisoned") = list;
        }
    }
}

fn print_response(value: &serde_json::Value, raw_json: bool, out: Option<&Path>) {
    if let Err(e) = cli::require_ok(value) {
        println!("{} {e}", Style::current().bad("error:"));
        return;
    }
    if raw_json {
        if let Ok(s) = serde_json::to_string_pretty(value) {
            println!("{s}");
        }
        return;
    }
    if let Some(users) = value.get("users").and_then(|v| v.as_array()) {
        match serde_json::from_value::<Vec<AdminUser>>(serde_json::Value::Array(users.clone())) {
            Ok(users) => print!("{}", display::users(Style::current(), &users)),
            Err(e) => println!(
                "{} invalid user response: {e}",
                Style::current().bad("error:")
            ),
        }
        return;
    }
    if let Some(invites) = value.get("invites").and_then(|v| v.as_array()) {
        match serde_json::from_value::<Vec<AdminUser>>(serde_json::Value::Array(invites.clone())) {
            Ok(invites) => print!("{}", display::invites(Style::current(), &invites)),
            Err(e) => println!(
                "{} invalid invite response: {e}",
                Style::current().bad("error:")
            ),
        }
        return;
    }
    if let Some(bundle) = value.get("bundle").and_then(|v| v.as_str()) {
        match out {
            // Mode 0600: the bundle is a bearer credential until it is used.
            Some(path) => match crate::store::write_private_for_caller(path, bundle) {
                Ok(()) => println!("bundle written to {}", path.display()),
                // The invite exists by now and this is the only copy of
                // its one-time key: show it rather than lose it.
                Err(e) => {
                    println!(
                        "{} cannot write {}: {e}; the bundle follows instead",
                        Style::current().bad("error:"),
                        path.display()
                    );
                    println!("{bundle}");
                }
            },
            None => println!("{bundle}"),
        }
        println!(
            "registration for {} (user_id {}), valid until {}",
            value.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
            value.get("user_id").and_then(|v| v.as_u64()).unwrap_or(0),
            value.get("expires").and_then(|v| v.as_str()).unwrap_or("?"),
        );
        return;
    }
    if value.get("uptime_s").is_some() {
        print!("{}", display::status(Style::current(), value));
        return;
    }
    if let Some(path) = value.get("path").and_then(|v| v.as_str()) {
        println!("{} {path}", Style::current().good("state backed up to"));
        return;
    }
    if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
        let id = value.get("user_id").and_then(|v| v.as_u64()).unwrap_or(0);
        match value.get("closed").and_then(|v| v.as_u64()) {
            Some(closed) => println!(
                "{} {name} (user_id {id}), closed {closed} connection(s)",
                Style::current().good("ok:")
            ),
            None => println!("{} {name} (user_id {id})", Style::current().good("ok:")),
        }
        return;
    }
    println!("{}", Style::current().good("ok"));
}

fn show_backups(config: &Config) {
    let dir = config.backup_dir();
    match display::backups(Style::current(), &dir) {
        Ok(text) => print!("{text}"),
        Err(e) => println!("{} {e}", Style::current().bad("error:")),
    }
}

fn print_help_for(remainder: &str, users: &[AdminUser]) {
    let tokens = command::tokenize_with_partial(remainder);
    let refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
    let candidates = command::candidates_at(&refs, users);
    if candidates.is_empty() {
        println!("  {}", Style::current().subtle("(no further completions)"));
        return;
    }
    for c in candidates {
        println!(
            "  {}{} {}",
            Style::current().heading(&c.text),
            " ".repeat(20usize.saturating_sub(c.text.chars().count())),
            c.help
        );
    }
}

fn print_top_help() {
    println!("{}", Style::current().heading("Commands"));
    println!(
        "{}",
        Style::current()
            .subtle("TAB completion and unique abbreviations work everywhere, e.g. sh us")
    );
    for (word, node) in command::TOP {
        println!(
            "  {}{} {}",
            Style::current().heading(word),
            " ".repeat(20usize.saturating_sub(word.chars().count())),
            node.help()
        );
    }
    println!("`<user>` is a name, user_id or fingerprint (`id:<id>`/`name:<name>` if ambiguous).");
    println!("append `?` to a line for context help. `run` and `restore` are not available here.");
}
