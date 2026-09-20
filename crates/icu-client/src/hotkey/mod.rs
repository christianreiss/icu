//! Global hotkeys: Portal (Linux, primary), evdev (Linux, opt-in/fallback),
//! Raw Input (Windows, M3 stub).

#[cfg(target_os = "linux")]
pub mod evdev;
#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(target_os = "windows")]
pub mod rawinput;

use tokio::sync::mpsc::UnboundedSender;

use crate::{
    config::{HotkeyBackend, HotkeySettings},
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
}

/// A running hotkey backend.
pub struct Hotkeys {
    /// Human readable backend description for the settings view.
    pub backend: String,
    _guard: Option<Box<dyn std::any::Any + Send>>,
}

/// Starts the configured backend. Never fails hard: without any backend the
/// UI button still works.
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
                Ok(guard) => {
                    return Hotkeys {
                        backend: "portal".into(),
                        _guard: Some(Box::new(guard)),
                    };
                }
                Err(e) => tracing::info!(%e, "portal hotkeys unavailable"),
            }
        }
        if want_evdev {
            match evdev::start(settings, cmd) {
                Ok((guard, n)) => {
                    return Hotkeys {
                        backend: format!("evdev ({n} devices)"),
                        _guard: Some(Box::new(guard)),
                    };
                }
                Err(e) => tracing::warn!(%e, "evdev hotkeys unavailable"),
            }
        }
        Hotkeys {
            backend: "none".into(),
            _guard: None,
        }
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (settings, cmd);
        Hotkeys {
            backend: "Unsupported (M3)".into(),
            _guard: None,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (settings, cmd);
        Hotkeys {
            backend: "none".into(),
            _guard: None,
        }
    }
}

#[allow(dead_code)]
fn _assert_backend_used(_: HotkeyBackend) {}
