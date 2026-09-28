//! Where `icu-server shell` keeps its command history: a per-*user* path,
//! not a per-server-config one — the shell is run interactively by whoever
//! is in the admin socket's group (or root over SSH), not by the daemon —
//! so it lives under the invoking user's own XDG state directory rather
//! than `/var/lib/icu` (the daemon's own `DynamicUser` state, `0700`, not
//! writable by an admin) or `/run/icu` (wiped on every service restart).
//!
//! Mirrors the non-Windows branch of `icu-client`'s `state_dir()`
//! (`crates/icu-client/src/main.rs`) rather than pulling in the
//! `directories` crate for one path computation — `icu-server` is
//! Linux-only, so there is no Windows branch to share it with.

use std::path::PathBuf;

/// `$XDG_STATE_HOME/icu-server/shell_history`, or
/// `~/.local/state/icu-server/shell_history` when that is unset or not
/// absolute (XDG Base Directory Specification). `None` if neither can be
/// resolved (no `$HOME`): the shell then runs with no persisted history
/// rather than failing to start.
pub fn history_path() -> Option<PathBuf> {
    state_dir(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
        .map(|dir| dir.join("shell_history"))
}

fn state_dir(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let absolute = |p: std::ffi::OsString| Some(PathBuf::from(p)).filter(|p| p.is_absolute());
    let base = xdg_state_home
        .and_then(absolute)
        .or_else(|| home.and_then(absolute).map(|h| h.join(".local/state")))?;
    Some(base.join("icu-server"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_state_home_wins_when_absolute() {
        let dir = state_dir(Some("/xdg/state".into()), Some("/home/x".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/xdg/state/icu-server"));
    }

    #[test]
    fn falls_back_to_home_local_state() {
        let dir = state_dir(None, Some("/home/x".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/home/x/.local/state/icu-server"));
    }

    #[test]
    fn a_relative_xdg_state_home_is_ignored() {
        let dir = state_dir(Some("relative".into()), Some("/home/x".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/home/x/.local/state/icu-server"));
    }

    #[test]
    fn neither_set_resolves_to_nothing() {
        assert_eq!(state_dir(None, None), None);
    }
}
