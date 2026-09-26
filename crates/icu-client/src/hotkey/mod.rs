//! Global hotkeys: Portal (Linux, primary), evdev (Linux, opt-in/fallback),
//! Raw Input (Windows, M3 stub).

#[cfg(target_os = "linux")]
pub mod evdev;
#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(target_os = "windows")]
pub mod rawinput;

use tokio::sync::mpsc::UnboundedSender;

#[cfg(target_os = "linux")]
use crate::config::HotkeyBackend;
use crate::{
    config::{HotkeySettings, KeyBinding},
    engine::Command,
};

/// What a bound key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Push-to-talk (press/release).
    Ptt,
    /// Toggle mute on press.
    Mute,
    /// Toggle deafen on press.
    Deafen,
    /// Toggle PTT/VAD on press.
    ToggleMode,
}

impl Action {
    /// The command for a key transition.
    pub fn command(self, pressed: bool) -> Option<Command> {
        match (self, pressed) {
            (Action::Ptt, p) => Some(Command::Ptt(p)),
            (Action::Mute, true) => Some(Command::ToggleMute),
            (Action::Deafen, true) => Some(Command::ToggleDeafen),
            (Action::ToggleMode, true) => Some(Command::ToggleMode),
            _ => None,
        }
    }

    /// The shortcut id the portal knows this action by.
    pub fn portal_id(self) -> &'static str {
        match self {
            Action::Ptt => "ptt",
            Action::Mute => "mute",
            Action::Deafen => "deafen",
            Action::ToggleMode => "toggle_mode",
        }
    }

    /// What the portal's dialog shows for this action.
    pub fn description(self) -> &'static str {
        match self {
            Action::Ptt => "ICU Push-to-Talk",
            Action::Mute => "ICU Mute",
            Action::Deafen => "ICU Deafen",
            Action::ToggleMode => "ICU PTT/VAD",
        }
    }
}

/// The four bindings and what each does, in the order the backends bind
/// them.
pub fn bindings(settings: &HotkeySettings) -> [(&KeyBinding, Action); 4] {
    [
        (&settings.ptt, Action::Ptt),
        (&settings.mute, Action::Mute),
        (&settings.deafen, Action::Deafen),
        (&settings.toggle_mode, Action::ToggleMode),
    ]
}

/// Turns key transitions into commands. Remembers a held PTT key and
/// releases it when dropped, so a reader that ends mid-press — its device
/// gone, its stream closed, its task aborted — cannot leave the mic open.
pub struct Dispatch {
    cmd: UnboundedSender<Command>,
    ptt_held: bool,
}

impl Dispatch {
    /// Sends through `cmd`.
    pub fn new(cmd: UnboundedSender<Command>) -> Dispatch {
        Dispatch {
            cmd,
            ptt_held: false,
        }
    }

    /// A bound key went down (`pressed`) or up.
    pub fn key(&mut self, action: Action, pressed: bool) {
        if action == Action::Ptt {
            self.ptt_held = pressed;
        }
        if let Some(c) = action.command(pressed) {
            let _ = self.cmd.send(c);
        }
    }
}

impl Drop for Dispatch {
    fn drop(&mut self) {
        if self.ptt_held {
            let _ = self.cmd.send(Command::Ptt(false));
        }
    }
}

/// `Hotkeys::backend` when no backend runs.
pub const BACKEND_NONE: &str = "none";

/// `Hotkeys::backend` when the user turned the portal down: cancelled its
/// dialog, confirmed it without a key, or left it unanswered. Not `none`:
/// the portal is there, and asking again may well work.
pub const BACKEND_PORTAL_DECLINED: &str = "portal-declined";

/// A running hotkey backend.
pub struct Hotkeys {
    /// Human readable backend description for the settings view:
    /// `portal`, `evdev (<n> devices)`, [`BACKEND_PORTAL_DECLINED`] or
    /// [`BACKEND_NONE`].
    pub backend: String,
    _guard: Option<Box<dyn std::any::Any + Send>>,
    /// Resolves when the backend ended by itself and wants binding again
    /// (the portal closed the session). `None` for backends that cannot.
    ended: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl Hotkeys {
    /// No backend running, for the reason `backend` names.
    fn idle(backend: &str) -> Hotkeys {
        Hotkeys {
            backend: backend.into(),
            _guard: None,
            ended: None,
        }
    }

    /// Resolves once the backend ended by itself; never for one that cannot.
    pub async fn ended(&mut self) {
        match self.ended.as_mut() {
            Some(ended) => {
                let _ = ended.await;
                self.ended = None;
            }
            None => std::future::pending().await,
        }
    }
}

/// Starts the configured backend. Never fails hard: without any backend the
/// UI button still works.
///
/// With [`HotkeyBackend::Auto`] the portal comes first. evdev reads every
/// keyboard, so it runs only when chosen (`Evdev`) or, under `Auto`, when
/// the portal is missing or fails — never because the user turned the
/// portal's dialog down: that is their answer, not a failure
/// (`Client/IMPLEMENTATION.md` §7).
pub async fn start(settings: &HotkeySettings, cmd: UnboundedSender<Command>) -> Hotkeys {
    #[cfg(target_os = "linux")]
    {
        let want_portal = matches!(
            settings.backend,
            HotkeyBackend::Auto | HotkeyBackend::Portal
        );
        let want_evdev = matches!(settings.backend, HotkeyBackend::Auto | HotkeyBackend::Evdev);
        if want_portal {
            match portal::start(settings, cmd.clone()).await {
                Ok((guard, ended)) => {
                    return Hotkeys {
                        backend: "portal".into(),
                        _guard: Some(Box::new(guard)),
                        ended: Some(ended),
                    };
                }
                Err(portal::Error::Declined(e)) => {
                    tracing::info!(%e, "portal hotkeys declined; evdev only when chosen");
                    return Hotkeys::idle(BACKEND_PORTAL_DECLINED);
                }
                Err(portal::Error::Unavailable(e)) => {
                    tracing::info!(%e, "portal hotkeys unavailable")
                }
            }
        }
        if want_evdev {
            match evdev::start(settings, cmd).await {
                Ok((guard, n)) => {
                    return Hotkeys {
                        backend: format!("evdev ({n} devices)"),
                        _guard: Some(Box::new(guard)),
                        ended: None,
                    };
                }
                Err(e) => tracing::warn!(%e, "evdev hotkeys unavailable"),
            }
        }
        Hotkeys::idle(BACKEND_NONE)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (settings, cmd);
        Hotkeys::idle("Unsupported (M3)")
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (settings, cmd);
        Hotkeys::idle(BACKEND_NONE)
    }
}
