//! XDG GlobalShortcuts portal (ashpd 0.13). `Cancelled` or any error means
//! "not available" and the caller falls back to evdev (Spike 4a).

use std::time::Duration;

use ashpd::desktop::{
    global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut},
    CreateSessionOptions,
};
use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;

use super::Action;
use crate::{config::HotkeySettings, engine::Command};

/// Keeps the portal session alive.
pub struct Guard {
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self._task.abort();
    }
}

/// Binds the configured shortcuts and forwards Activated/Deactivated.
pub async fn start(
    settings: &HotkeySettings,
    cmd: UnboundedSender<Command>,
) -> Result<Guard, String> {
    let wanted: Vec<(&str, &str, Option<String>, Action)> = vec![
        (
            "ptt",
            "ICU Push-to-Talk",
            settings.ptt.trigger.clone(),
            Action::Ptt,
        ),
        (
            "mute",
            "ICU Mute",
            settings.mute.trigger.clone(),
            Action::Mute,
        ),
        (
            "deafen",
            "ICU Deafen",
            settings.deafen.trigger.clone(),
            Action::Deafen,
        ),
        (
            "toggle_mode",
            "ICU PTT/VAD",
            settings.toggle_mode.trigger.clone(),
            Action::ToggleMode,
        ),
    ];
    let shortcuts: Vec<NewShortcut> = wanted
        .iter()
        .filter(|(_, _, trigger, _)| trigger.is_some())
        .map(|(id, desc, trigger, _)| {
            let s = NewShortcut::new(*id, *desc);
            match trigger {
                Some(t) => s.preferred_trigger(t.as_str()),
                None => s,
            }
        })
        .collect();
    if shortcuts.is_empty() {
        return Err("no portal trigger configured".into());
    }
    let portal = GlobalShortcuts::new()
        .await
        .map_err(|e| format!("portal: {e}"))?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| format!("create_session: {e}"))?;
    let bind = tokio::time::timeout(Duration::from_secs(60), async {
        portal
            .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
            .await?
            .response()
    })
    .await
    .map_err(|_| "bind_shortcuts: no answer in 60 s".to_string())?
    .map_err(|e| format!("bind_shortcuts: {e}"))?;
    for s in bind.shortcuts() {
        tracing::info!(id = s.id(), trigger = ?s.trigger_description(), "portal shortcut bound");
    }
    let mut activated = portal
        .receive_activated()
        .await
        .map_err(|e| e.to_string())?;
    let mut deactivated = portal
        .receive_deactivated()
        .await
        .map_err(|e| e.to_string())?;
    let actions: Vec<(String, Action)> = wanted
        .iter()
        .map(|(id, _, _, a)| (id.to_string(), *a))
        .collect();
    let task = tokio::spawn(async move {
        let _session = session;
        let _portal = portal;
        loop {
            let (id, pressed) = tokio::select! {
                Some(a) = activated.next() => (a.shortcut_id().to_string(), true),
                Some(d) = deactivated.next() => (d.shortcut_id().to_string(), false),
                else => return,
            };
            for (bound, action) in &actions {
                if *bound == id {
                    if let Some(c) = action.command(pressed) {
                        let _ = cmd.send(c);
                    }
                }
            }
        }
    });
    Ok(Guard { _task: task })
}
