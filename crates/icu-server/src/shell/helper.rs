//! The rustyline [`Helper`] that gives the shell TAB completion. Everything
//! it does is a thin, synchronous wrapper around the pure functions in
//! [`crate::shell::command`] — no socket access happens from inside a
//! completion callback (it runs on whatever thread rustyline's blocking
//! `readline()` is on); the user list it completes against is a snapshot,
//! refreshed by the run loop (`shell::run`), not fetched per keystroke.

use std::sync::{Arc, Mutex};

use rustyline::{
    completion::{Completer, Pair},
    highlight::Highlighter,
    hint::Hinter,
    validate::Validator,
    Context, Helper, Result as RlResult,
};

use crate::{admin::AdminUser, shell::command};

pub struct ShellHelper {
    pub users: Arc<Mutex<Vec<AdminUser>>>,
}

impl Completer for ShellHelper {
    type Candidate = Pair;

    fn complete(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> RlResult<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];
        let start = prefix
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        let tokens = command::tokenize_with_partial(prefix);
        let token_refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
        let users = self.users.lock().expect("shell user cache poisoned");
        let pairs = command::candidates_at(&token_refs, &users)
            .into_iter()
            .map(|c| Pair {
                display: format!("{:<20} {}", c.text, c.help),
                replacement: c.text,
            })
            .collect();
        Ok((start, pairs))
    }
}

impl Hinter for ShellHelper {
    type Hint = String;
}

impl Highlighter for ShellHelper {}

impl Validator for ShellHelper {}

impl Helper for ShellHelper {}
