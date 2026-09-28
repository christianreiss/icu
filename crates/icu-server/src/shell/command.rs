//! The shell's command tree: pure, no I/O. Shared by TAB completion, `?`
//! context help, prefix/abbreviation resolution and dispatch
//! (`Server/IMPLEMENTATION.md` §5.1), so all four ways of asking "what can I
//! type here" and "what does this line mean" agree by construction.
//!
//! Grammar: a line is a sequence of literal keyword tokens (matched by exact
//! equality or, failing that, a *unique* prefix — `sh us` resolves to `show
//! users` the same way Cisco IOS resolves abbreviated commands) followed by
//! the arguments the resolved command takes. Prefix matching applies only to
//! literal keywords, never to an argument: a user named `k` is never mistaken
//! for `kick`.

use crate::{admin::AdminUser, AdminRequest};

/// What kind of argument a command takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// A user selector (name, `user_id` or fingerprint; `Store::resolve`).
    /// Required. Completed live from the cached user list.
    UserSelector,
    /// Free text, shown by `?` as a placeholder but never enumerated —
    /// there is nothing to complete it against. Always optional in v1.
    FreeText(&'static str),
}

/// What running a resolved command actually does. Built by a [`Leaf::build`]
/// function from the line's arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send `request` over the admin socket (`cli::dispatch`).
    Admin {
        request: AdminRequest,
        /// `show status`/`show users --json`: print the raw response
        /// instead of the formatted view.
        raw_json: bool,
    },
    /// `show backups`: a local, read-only listing (`crate::backup::list`),
    /// no admin request involved.
    ShowBackups,
    /// `refresh`/`r`: re-fetch the user list used for completion.
    Refresh,
    /// `help`: list every top-level command.
    Help,
    /// `exit`/`quit`: leave the shell.
    Exit,
}

/// One concrete, resolvable command.
pub struct Leaf {
    /// One-line description, shown by `?`, `help`, and completion.
    pub help: &'static str,
    /// The arguments this command takes, in order.
    pub args: &'static [ArgKind],
    /// Whether running it asks `[y/N]` first (`revoke`, `disable`, `kick`:
    /// each closes live connections or is otherwise hard to walk back).
    pub confirm: bool,
    /// Builds the [`Action`] from the line's arguments, already padded to
    /// `args.len()` (a missing optional `FreeText` becomes `""`).
    pub build: fn(&[String]) -> Result<Action, String>,
}

/// A node in the command tree: a runnable command, or a named group of
/// further nodes (only `show` is a group in v1).
pub enum Node {
    Leaf(Leaf),
    Group {
        help: &'static str,
        children: &'static [(&'static str, Node)],
    },
}

impl Node {
    /// The one-line description shown by `?`/`help`, for a leaf or a group.
    pub fn help(&self) -> &'static str {
        match self {
            Node::Leaf(l) => l.help,
            Node::Group { help, .. } => help,
        }
    }
}

fn admin(request: AdminRequest) -> Result<Action, String> {
    Ok(Action::Admin {
        request,
        raw_json: false,
    })
}

/// `true` if `args[0]` (the trailing `[--json]` flag, defaulted to `""`
/// when absent) asks for raw output.
fn wants_json(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "--json")
}

fn user(args: &[String]) -> String {
    args.first().cloned().unwrap_or_default()
}

const SHOW_CHILDREN: &[(&str, Node)] = &[
    (
        "status",
        Node::Leaf(Leaf {
            help: "Uptime, online count and relay counters.",
            args: &[ArgKind::FreeText("[--json]")],
            confirm: false,
            build: |args| {
                Ok(Action::Admin {
                    request: AdminRequest::Status,
                    raw_json: wants_json(args),
                })
            },
        }),
    ),
    (
        "users",
        Node::Leaf(Leaf {
            help: "List all users.",
            args: &[ArgKind::FreeText("[--json]")],
            confirm: false,
            build: |args| {
                Ok(Action::Admin {
                    request: AdminRequest::Users,
                    raw_json: wants_json(args),
                })
            },
        }),
    ),
    (
        "backups",
        Node::Leaf(Leaf {
            help: "List state snapshots (needs read access to the backup directory).",
            args: &[],
            confirm: false,
            build: |_| Ok(Action::ShowBackups),
        }),
    ),
];

/// The top-level command tree. Order here is the order `help` lists them in.
pub const TOP: &[(&str, Node)] = &[
    (
        "show",
        Node::Group {
            help: "Read-only status and listings.",
            children: SHOW_CHILDREN,
        },
    ),
    (
        "invite",
        Node::Leaf(Leaf {
            help: "Issue a registration bundle: [name] [ttl, e.g. 24h].",
            args: &[
                ArgKind::FreeText("[name]"),
                ArgKind::FreeText("[ttl, e.g. 24h]"),
            ],
            confirm: false,
            build: |args| {
                let ttl = args.get(1).map(String::as_str).unwrap_or("");
                let ttl_s =
                    crate::cli::invite::parse_duration(if ttl.is_empty() { "24h" } else { ttl })
                        .map_err(|e| e.to_string())?;
                admin(AdminRequest::Invite {
                    name: args.first().cloned().unwrap_or_default(),
                    ttl_s: Some(ttl_s),
                })
            },
        }),
    ),
    (
        "kick",
        Node::Leaf(Leaf {
            help: "Close a user's live connections; they may connect again.",
            args: &[ArgKind::UserSelector],
            confirm: true,
            build: |args| admin(AdminRequest::Kick { user: user(args) }),
        }),
    ),
    (
        "mute",
        Node::Leaf(Leaf {
            help: "Enforce mute: the user's microphone stays off until unmute.",
            args: &[ArgKind::UserSelector],
            confirm: false,
            build: |args| admin(AdminRequest::Mute { user: user(args) }),
        }),
    ),
    (
        "unmute",
        Node::Leaf(Leaf {
            help: "Lift an enforced mute.",
            args: &[ArgKind::UserSelector],
            confirm: false,
            build: |args| admin(AdminRequest::Unmute { user: user(args) }),
        }),
    ),
    (
        "deafen",
        Node::Leaf(Leaf {
            help: "Enforce deafen: the user receives no audio until undeafen.",
            args: &[ArgKind::UserSelector],
            confirm: false,
            build: |args| admin(AdminRequest::Deafen { user: user(args) }),
        }),
    ),
    (
        "undeafen",
        Node::Leaf(Leaf {
            help: "Lift an enforced deafen.",
            args: &[ArgKind::UserSelector],
            confirm: false,
            build: |args| admin(AdminRequest::Undeafen { user: user(args) }),
        }),
    ),
    (
        "promote",
        Node::Leaf(Leaf {
            help: "Grant admin rights.",
            args: &[ArgKind::UserSelector],
            confirm: false,
            build: |args| admin(AdminRequest::Promote { user: user(args) }),
        }),
    ),
    (
        "demote",
        Node::Leaf(Leaf {
            help: "Revoke admin rights. Rejected if this is the sole remaining admin.",
            args: &[ArgKind::UserSelector],
            confirm: false,
            build: |args| admin(AdminRequest::Demote { user: user(args) }),
        }),
    ),
    (
        "disable",
        Node::Leaf(Leaf {
            help: "Reversibly block login without touching certificates.",
            args: &[ArgKind::UserSelector],
            confirm: true,
            build: |args| admin(AdminRequest::Disable { user: user(args) }),
        }),
    ),
    (
        "enable",
        Node::Leaf(Leaf {
            help: "Restore login after disable.",
            args: &[ArgKind::UserSelector],
            confirm: false,
            build: |args| admin(AdminRequest::Enable { user: user(args) }),
        }),
    ),
    (
        "revoke",
        Node::Leaf(Leaf {
            help: "Revoke every certificate of a user and kick it. Irreversible.",
            args: &[ArgKind::UserSelector],
            confirm: true,
            build: |args| admin(AdminRequest::Revoke { user: user(args) }),
        }),
    ),
    (
        "rotate-server-cert",
        Node::Leaf(Leaf {
            help: "Issue a fresh server leaf from the same CA.",
            args: &[],
            confirm: false,
            build: |_| admin(AdminRequest::RotateServerCert),
        }),
    ),
    (
        "renew-ca",
        Node::Leaf(Leaf {
            help: "Re-issue the CA certificate under the same key with a fresh validity.",
            args: &[],
            confirm: false,
            build: |_| admin(AdminRequest::RenewCa),
        }),
    ),
    (
        "backup",
        Node::Leaf(Leaf {
            help: "Take a snapshot of the state now.",
            args: &[],
            confirm: false,
            build: |_| admin(AdminRequest::Backup),
        }),
    ),
    (
        "refresh",
        Node::Leaf(Leaf {
            help: "Re-fetch the user list used for TAB completion.",
            args: &[],
            confirm: false,
            build: |_| Ok(Action::Refresh),
        }),
    ),
    (
        "r",
        Node::Leaf(Leaf {
            help: "Alias of refresh.",
            args: &[],
            confirm: false,
            build: |_| Ok(Action::Refresh),
        }),
    ),
    (
        "help",
        Node::Leaf(Leaf {
            help: "List every command.",
            args: &[],
            confirm: false,
            build: |_| Ok(Action::Help),
        }),
    ),
    (
        "exit",
        Node::Leaf(Leaf {
            help: "Leave the shell.",
            args: &[],
            confirm: false,
            build: |_| Ok(Action::Exit),
        }),
    ),
    (
        "quit",
        Node::Leaf(Leaf {
            help: "Alias of exit.",
            args: &[],
            confirm: false,
            build: |_| Ok(Action::Exit),
        }),
    ),
];

/// Splits a line on whitespace. No quoting in v1: a selector needing a
/// literal space is out of scope for now (`Store::resolve`'s `name:<name>`
/// form already only needs one token for the common ambiguous case).
pub fn tokenize(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

/// Like [`tokenize`], but for completion: if `line` is empty or ends in
/// whitespace, an empty trailing token is appended, so "the word being
/// completed" is always the last element, even when it is empty (the user
/// just typed a space, or hasn't typed anything yet).
pub fn tokenize_with_partial(line: &str) -> Vec<String> {
    let mut tokens = tokenize(line);
    if line.is_empty() || line.ends_with(char::is_whitespace) {
        tokens.push(String::new());
    }
    tokens
}

/// Why a line didn't resolve to a command.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolveError {
    /// The line was empty.
    Empty,
    /// `typed` matched none of `candidates` (exactly or by prefix).
    Unknown {
        typed: String,
        candidates: Vec<String>,
    },
    /// `typed` prefix-matched more than one of `matches`.
    Ambiguous { typed: String, matches: Vec<String> },
    /// A group (`show`) was typed with nothing after it.
    IncompleteGroup {
        path: Vec<String>,
        candidates: Vec<String>,
    },
    /// A required argument (a user selector) is missing.
    MissingArg { usage: String },
    /// More tokens were typed than the command takes.
    TooManyArgs { usage: String },
    /// The command's own argument validation failed (e.g. a bad ttl).
    Invalid(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Empty => write!(f, "(empty)"),
            ResolveError::Unknown { typed, candidates } => write!(
                f,
                "unknown command {typed:?}; expected one of: {}",
                candidates.join(", ")
            ),
            ResolveError::Ambiguous { typed, matches } => {
                write!(f, "{typed:?} is ambiguous; matches: {}", matches.join(", "))
            }
            ResolveError::IncompleteGroup { path, candidates } => write!(
                f,
                "{} needs one of: {}",
                path.join(" "),
                candidates.join(", ")
            ),
            ResolveError::MissingArg { usage } => write!(f, "usage: {usage}"),
            ResolveError::TooManyArgs { usage } => {
                write!(f, "too many arguments; usage: {usage}")
            }
            ResolveError::Invalid(msg) => write!(f, "{msg}"),
        }
    }
}

/// Resolves `typed` against `keys`: an exact match always wins outright
/// (so the `r` alias is never shadowed by `refresh`/`revoke`/`renew-ca`/
/// `rotate-server-cert`, all of which it also prefixes); otherwise a
/// *unique* prefix match wins; zero or more than one is an error.
fn match_key<'a>(
    typed: &str,
    keys: impl Iterator<Item = &'a str>,
) -> Result<&'a str, (Vec<String>, Vec<String>)> {
    let keys: Vec<&str> = keys.collect();
    if let Some(&exact) = keys.iter().find(|k| **k == typed) {
        return Ok(exact);
    }
    let matches: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|k| !typed.is_empty() && k.starts_with(typed))
        .collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        _ => Err((
            matches.iter().map(|s| s.to_string()).collect(),
            keys.iter().map(|s| s.to_string()).collect(),
        )),
    }
}

fn usage(path: &[&str], leaf: &Leaf) -> String {
    let mut parts: Vec<String> = path.iter().map(|s| s.to_string()).collect();
    for arg in leaf.args {
        parts.push(match arg {
            ArgKind::UserSelector => "<user>".to_string(),
            ArgKind::FreeText(placeholder) => placeholder.to_string(),
        });
    }
    parts.join(" ")
}

/// Resolves a full line to an [`Action`], and, for a command with
/// `confirm: true`, a `[y/N]` prompt describing it. `Ok` only once the line
/// is a complete, valid command — a partial line is [`ResolveError`], never
/// a partial [`Action`].
pub fn resolve_line(line: &str) -> Result<(Action, Option<String>), ResolveError> {
    let tokens = tokenize(line);
    if tokens.is_empty() {
        return Err(ResolveError::Empty);
    }
    let mut node_list: &[(&str, Node)] = TOP;
    let mut idx = 0;
    let mut path: Vec<&str> = Vec::new();
    loop {
        let typed = tokens[idx].to_ascii_lowercase();
        let key =
            match_key(&typed, node_list.iter().map(|(k, _)| *k)).map_err(|(matches, all)| {
                if matches.is_empty() {
                    ResolveError::Unknown {
                        typed: tokens[idx].clone(),
                        candidates: all,
                    }
                } else {
                    ResolveError::Ambiguous {
                        typed: tokens[idx].clone(),
                        matches,
                    }
                }
            })?;
        let (_, node) = node_list
            .iter()
            .find(|(k, _)| *k == key)
            .expect("just matched");
        path.push(key);
        idx += 1;
        match node {
            Node::Group { children, .. } => {
                if idx >= tokens.len() {
                    return Err(ResolveError::IncompleteGroup {
                        path: path.iter().map(|s| s.to_string()).collect(),
                        candidates: children.iter().map(|(k, _)| k.to_string()).collect(),
                    });
                }
                node_list = children;
            }
            Node::Leaf(leaf) => {
                let raw_args = &tokens[idx..];
                let required = leaf
                    .args
                    .iter()
                    .filter(|a| matches!(a, ArgKind::UserSelector))
                    .count();
                if raw_args.len() < required {
                    return Err(ResolveError::MissingArg {
                        usage: usage(&path, leaf),
                    });
                }
                if raw_args.len() > leaf.args.len() {
                    return Err(ResolveError::TooManyArgs {
                        usage: usage(&path, leaf),
                    });
                }
                let mut normalized: Vec<String> = raw_args.to_vec();
                normalized.resize(leaf.args.len(), String::new());
                let action = (leaf.build)(&normalized).map_err(ResolveError::Invalid)?;
                let confirm = leaf.confirm.then(|| {
                    format!(
                        "{} {} — {} [y/N] ",
                        path.join(" "),
                        normalized.join(" "),
                        leaf.help
                    )
                });
                return Ok((action, confirm));
            }
        }
    }
}

/// One completion or `?` candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub text: String,
    pub help: &'static str,
}

/// Candidates for the last token of `tokens` (which may be an empty "just
/// typed a space" placeholder, from [`tokenize_with_partial`]), given the
/// already-completed tokens before it. Silently empty for a line whose
/// completed prefix does not resolve — there is nothing sensible to offer.
pub fn candidates_at(tokens: &[&str], users: &[AdminUser]) -> Vec<Candidate> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let (completed, last) = tokens.split_at(tokens.len() - 1);
    let partial = last[0].to_ascii_lowercase();

    let mut node_list: &[(&str, Node)] = TOP;
    let mut in_leaf: Option<&Leaf> = None;
    let mut consumed_args = 0usize;
    for &tok in completed {
        if let Some(leaf) = in_leaf {
            let _ = leaf;
            consumed_args += 1;
            continue;
        }
        let typed = tok.to_ascii_lowercase();
        let Ok(key) = match_key(&typed, node_list.iter().map(|(k, _)| *k)) else {
            return Vec::new();
        };
        let (_, node) = node_list
            .iter()
            .find(|(k, _)| *k == key)
            .expect("just matched");
        match node {
            Node::Group { children, .. } => node_list = children,
            Node::Leaf(leaf) => in_leaf = Some(leaf),
        }
    }

    match in_leaf {
        None => node_list
            .iter()
            .filter(|(k, _)| k.starts_with(&partial))
            .map(|(k, n)| Candidate {
                text: k.to_string(),
                help: n.help(),
            })
            .collect(),
        Some(leaf) => match leaf.args.get(consumed_args) {
            Some(ArgKind::UserSelector) => users
                .iter()
                .filter(|u| u.name.starts_with(&partial))
                .map(|u| Candidate {
                    text: u.name.clone(),
                    help: if u.online { "online" } else { "offline" },
                })
                .collect(),
            _ => Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: u32, name: &str, online: bool) -> AdminUser {
        AdminUser {
            user_id: id,
            name: name.to_string(),
            fingerprint: String::new(),
            issued: String::new(),
            expires: String::new(),
            revoked: false,
            online,
            is_admin: false,
            disabled: false,
            pending: false,
            muted: false,
            deafened: false,
        }
    }

    #[test]
    fn full_verbs_resolve() {
        let (action, confirm) = resolve_line("show users").unwrap();
        assert_eq!(
            action,
            Action::Admin {
                request: AdminRequest::Users,
                raw_json: false
            }
        );
        assert!(confirm.is_none());
    }

    #[test]
    fn abbreviations_resolve_like_cisco() {
        let (action, _) = resolve_line("sh us").unwrap();
        assert_eq!(
            action,
            Action::Admin {
                request: AdminRequest::Users,
                raw_json: false
            }
        );
    }

    #[test]
    fn json_flag_is_recognised() {
        let (action, _) = resolve_line("show status --json").unwrap();
        assert_eq!(
            action,
            Action::Admin {
                request: AdminRequest::Status,
                raw_json: true
            }
        );
    }

    #[test]
    fn user_selector_is_never_prefix_matched() {
        let (action, confirm) = resolve_line("kick alice").unwrap();
        assert_eq!(
            action,
            Action::Admin {
                request: AdminRequest::Kick {
                    user: "alice".to_string()
                },
                raw_json: false,
            }
        );
        assert!(confirm.unwrap().starts_with("kick alice"));
    }

    #[test]
    fn the_r_alias_wins_outright_over_its_own_prefix_matches() {
        // "r" is an exact match for the `r` alias, even though it is also a
        // prefix of refresh/revoke/renew-ca/rotate-server-cert.
        let (action, _) = resolve_line("r").unwrap();
        assert_eq!(action, Action::Refresh);
    }

    #[test]
    fn missing_required_arg_is_an_error() {
        assert!(matches!(
            resolve_line("kick"),
            Err(ResolveError::MissingArg { .. })
        ));
    }

    #[test]
    fn too_many_args_is_an_error() {
        assert!(matches!(
            resolve_line("kick alice bob"),
            Err(ResolveError::TooManyArgs { .. })
        ));
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert!(matches!(
            resolve_line("kikc alice"),
            Err(ResolveError::Unknown { .. })
        ));
    }

    #[test]
    fn ambiguous_abbreviation_lists_every_match() {
        // "d" prefixes deafen, demote and disable.
        match resolve_line("d alice") {
            Err(ResolveError::Ambiguous { matches, .. }) => {
                assert_eq!(matches.len(), 3);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_group_without_a_child_is_incomplete() {
        assert!(matches!(
            resolve_line("show"),
            Err(ResolveError::IncompleteGroup { .. })
        ));
    }

    #[test]
    fn invite_defaults_ttl_to_24h() {
        let (action, _) = resolve_line("invite").unwrap();
        assert_eq!(
            action,
            Action::Admin {
                request: AdminRequest::Invite {
                    name: String::new(),
                    ttl_s: Some(86400),
                },
                raw_json: false,
            }
        );
    }

    #[test]
    fn invite_takes_an_explicit_name_and_ttl() {
        let (action, _) = resolve_line("invite bob 7d").unwrap();
        assert_eq!(
            action,
            Action::Admin {
                request: AdminRequest::Invite {
                    name: "bob".to_string(),
                    ttl_s: Some(604800),
                },
                raw_json: false,
            }
        );
    }

    #[test]
    fn invite_rejects_a_bad_ttl() {
        assert!(matches!(
            resolve_line("invite bob soon"),
            Err(ResolveError::Invalid(_))
        ));
    }

    #[test]
    fn exit_and_quit_are_distinct_full_words_not_prefixes() {
        assert_eq!(resolve_line("exit").unwrap().0, Action::Exit);
        assert_eq!(resolve_line("quit").unwrap().0, Action::Exit);
    }

    #[test]
    fn top_level_candidates_offer_group_and_leaves() {
        let cands = candidates_at(&["k"], &[]);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].text, "kick");
    }

    #[test]
    fn show_group_candidates_are_its_children() {
        let cands = candidates_at(&["show", ""], &[]);
        let texts: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["status", "users", "backups"]);
    }

    #[test]
    fn user_selector_completes_from_the_live_cache() {
        let users = vec![user(1, "alice", true), user(2, "bob", false)];
        let cands = candidates_at(&["kick", "al"], &users);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].text, "alice");
        assert_eq!(cands[0].help, "online");
    }

    #[test]
    fn free_text_args_offer_no_candidates() {
        let cands = candidates_at(&["invite", "bob", ""], &[]);
        assert!(cands.is_empty());
    }

    #[test]
    fn an_unresolved_prefix_offers_nothing() {
        assert!(candidates_at(&["nope", ""], &[]).is_empty());
    }
}
