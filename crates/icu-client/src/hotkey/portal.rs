//! XDG GlobalShortcuts portal (ashpd 0.13). `Cancelled` or any error means
//! "not available" and the caller falls back to evdev (Spike 4a).

use std::time::Duration;

use ashpd::desktop::{
    global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut},
    CreateSessionOptions,
};
use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;

use super::{Action, Dispatch};
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
    // Only a binding with a trigger is bound; the others could never fire.
    let wanted: Vec<(Action, String)> = super::bindings(settings)
        .into_iter()
        .filter_map(|(binding, action)| binding.trigger.clone().map(|t| (action, t)))
        .collect();
    let shortcuts: Vec<NewShortcut> = wanted
        .iter()
        .map(|(action, trigger)| {
            NewShortcut::new(action.portal_id(), action.description())
                .preferred_trigger(trigger.as_str())
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
    let mut any_key = false;
    for s in bind.shortcuts() {
        tracing::info!(id = s.id(), trigger = ?s.trigger_description(), "portal shortcut bound");
        if !s.trigger_description().trim().is_empty() {
            any_key = true;
        }
    }
    if !any_key {
        // KDE accepts the binding but stores `none` as the active key when the
        // user does not assign one in the dialog; the portal then never fires.
        // Treat that as "not available" so the caller falls back to evdev.
        return Err("portal bound the shortcuts without any key".into());
    }
    let mut activated = portal
        .receive_activated()
        .await
        .map_err(|e| e.to_string())?;
    let mut deactivated = portal
        .receive_deactivated()
        .await
        .map_err(|e| e.to_string())?;
    let actions: Vec<Action> = wanted.iter().map(|(action, _)| *action).collect();
    let task = tokio::spawn(async move {
        let _session = session;
        let _portal = portal;
        // Dropped when the streams end: a PTT key held then is released.
        let mut dispatch = Dispatch::new(cmd);
        loop {
            let (id, pressed) = tokio::select! {
                Some(a) = activated.next() => (a.shortcut_id().to_string(), true),
                Some(d) = deactivated.next() => (d.shortcut_id().to_string(), false),
                else => return,
            };
            tracing::debug!(id, pressed, "portal shortcut event");
            for action in &actions {
                if action.portal_id() == id {
                    dispatch.key(*action, pressed);
                }
            }
        }
    });
    Ok(Guard { _task: task })
}
