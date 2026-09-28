//! XDG GlobalShortcuts portal (ashpd 0.13).
//!
//! What the user answers in the portal's dialog counts as their decision
//! (`docs/reference/client.md` §7): cancelling it, confirming it without a
//! key, or leaving it unanswered is [`Error::Declined`], and the caller does
//! not fall back to reading every keyboard through evdev. Only a portal that
//! is missing or fails is [`Error::Unavailable`] (Spike 4a).

use std::{collections::HashMap, time::Duration};

use ashpd::{
    desktop::{
        global_shortcuts::{
            Activated, BindShortcutsOptions, Deactivated, GlobalShortcuts, NewShortcut,
        },
        CreateSessionOptions, ResponseError,
    },
    zvariant::{self, OwnedObjectPath},
};
use futures_util::StreamExt;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use super::{Action, Dispatch};
use crate::{config::HotkeySettings, engine::Command};

/// How long the portal's dialog may stay unanswered.
const BIND_TIMEOUT: Duration = Duration::from_secs(60);

/// How far an event may lag behind the last one handled for its shortcut
/// and still count as reordered. Activated and Deactivated reach us on two
/// streams, so a queued pair can be read in either order — but only ever
/// milliseconds apart. A bigger step back is the event clock jumping (a
/// clock set back, a wrapped X11 server time), and dropping everything
/// after it would leave the keys dead.
const REORDER_WINDOW: Duration = Duration::from_secs(5);

/// Why the portal did not bind.
#[derive(Debug)]
pub enum Error {
    /// The user said no: cancelled the dialog, confirmed it without
    /// assigning any key, or left it unanswered for [`BIND_TIMEOUT`].
    Declined(String),
    /// No portal, or it failed.
    Unavailable(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Declined(e) | Error::Unavailable(e) => f.write_str(e),
        }
    }
}

/// Keeps the portal session alive. Dropping it ends the session: the task
/// releases a held PTT key, then closes the session at the portal.
pub struct Guard {
    /// Dropped with the guard, which is the task's signal to stop.
    _stop: oneshot::Sender<()>,
}

/// Binds the configured shortcuts and forwards Activated/Deactivated. The
/// receiver resolves when the binding ended by itself — the portal closed
/// the session, for instance because it restarted — and wants binding
/// again; not when the [`Guard`] was dropped.
pub async fn start(
    settings: &HotkeySettings,
    cmd: UnboundedSender<Command>,
) -> Result<(Guard, oneshot::Receiver<()>), Error> {
    use Error::{Declined, Unavailable};
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
        return Err(Unavailable("no portal trigger configured".into()));
    }
    let portal = GlobalShortcuts::new()
        .await
        .map_err(|e| Unavailable(format!("portal: {e}")))?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| Unavailable(format!("create_session: {e}")))?;
    let bound = tokio::time::timeout(BIND_TIMEOUT, async {
        portal
            .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
            .await?
            .response()
    })
    .await;
    let bind = match bound {
        Ok(Ok(bind)) => Ok(bind),
        // The dialog's answer, or the portal's D-Bus error of that name.
        Ok(Err(
            ashpd::Error::Response(ResponseError::Cancelled)
            | ashpd::Error::Portal(ashpd::PortalError::Cancelled(_)),
        )) => Err(Declined("the portal dialog was cancelled".into())),
        Ok(Err(e)) => Err(Unavailable(format!("bind_shortcuts: {e}"))),
        Err(_) => Err(Declined(format!(
            "the portal dialog was not answered in {} s",
            BIND_TIMEOUT.as_secs()
        ))),
    };
    let bind = match bind {
        Ok(bind) => bind,
        Err(e) => {
            // Closing the session also closes a dialog still on screen.
            close(&session).await;
            return Err(e);
        }
    };
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
        // The user saw the dialog and left the keys empty: their call.
        close(&session).await;
        return Err(Declined(
            "the portal bound the shortcuts without any key".into(),
        ));
    }
    let streams = async {
        Ok::<_, ashpd::Error>((
            portal.receive_activated().await?,
            portal.receive_deactivated().await?,
        ))
    };
    let (activated, deactivated) = match streams.await {
        Ok(streams) => streams,
        Err(e) => {
            close(&session).await;
            return Err(Unavailable(e.to_string()));
        }
    };
    // What the desktop assigned, which the windows show as the key.
    let tag = super::portal_bound(
        bind.shortcuts()
            .iter()
            .map(|s| (s.id().to_string(), s.trigger_description().to_string()))
            .collect(),
    );
    let own = object_path(&session);
    if own.is_none() {
        tracing::warn!("portal session without a readable path; events are not filtered");
    }
    let events = Events {
        actions: wanted.iter().map(|(action, _)| *action).collect(),
        own,
        cmd,
    };
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (ended_tx, ended_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _portal = portal;
        // Subscribed here, not above: the stream borrows the session, which
        // this task owns.
        let end = match session.receive_closed().await {
            Ok(closed) => {
                events
                    .forward(activated, deactivated, closed, stop_rx)
                    .await
            }
            Err(e) => {
                tracing::warn!(%e, "portal: no Closed signal");
                End::Lost
            }
        };
        match end {
            End::Closed => tracing::info!("the portal closed the shortcut session"),
            End::Lost => tracing::info!("the portal shortcut signals ended"),
            End::Stopped => {}
        }
        super::portal_unbound(tag);
        if !matches!(end, End::Closed) {
            close(&session).await;
        }
        if !matches!(end, End::Stopped) {
            // Nobody listens after a stop; otherwise the owner binds again.
            let _ = ended_tx.send(());
        }
    });
    Ok((Guard { _stop: stop_tx }, ended_rx))
}

/// What the event task needs besides the streams.
struct Events {
    /// The bound actions, by portal id.
    actions: Vec<Action>,
    /// Our session's path; events for any other session are ignored.
    own: Option<OwnedObjectPath>,
    cmd: UnboundedSender<Command>,
}

impl Events {
    /// Turns the portal's signals into commands until `stop` fires, the
    /// portal closes the session or the signals end. A PTT key held then is
    /// released on the way out (`Dispatch` is dropped).
    async fn forward(
        self,
        mut activated: impl futures_util::Stream<Item = Activated> + Unpin,
        mut deactivated: impl futures_util::Stream<Item = Deactivated> + Unpin,
        mut closed: impl futures_util::Stream + Unpin,
        mut stop: oneshot::Receiver<()>,
    ) -> End {
        let mut dispatch = Dispatch::new(self.cmd);
        let mut order = EventOrder::default();
        loop {
            // Biased: a stop wins over anything queued, and of a queued
            // press/release pair the press goes first — should the two carry
            // the same timestamp, the release is what is left standing.
            let (session_handle, id, at, pressed) = tokio::select! {
                biased;
                _ = &mut stop => return End::Stopped,
                _ = closed.next() => return End::Closed,
                a = activated.next() => match a {
                    Some(a) => (a.session_handle().to_owned(), a.shortcut_id().to_owned(), a.timestamp(), true),
                    None => return End::Lost,
                },
                d = deactivated.next() => match d {
                    Some(d) => (d.session_handle().to_owned(), d.shortcut_id().to_owned(), d.timestamp(), false),
                    None => return End::Lost,
                },
            };
            if self
                .own
                .as_ref()
                .is_some_and(|own| own.as_str() != session_handle.as_str())
            {
                tracing::debug!(%session_handle, id, "portal event for another session");
                continue;
            }
            if !order.accept(&id, at) {
                tracing::debug!(
                    id,
                    pressed,
                    ?at,
                    "portal event older than the last one, dropped"
                );
                continue;
            }
            tracing::debug!(id, pressed, "portal shortcut event");
            for action in &self.actions {
                if action.portal_id() == id {
                    dispatch.key(*action, pressed);
                }
            }
        }
    }
}

/// How the event task ended.
enum End {
    /// The guard was dropped.
    Stopped,
    /// The portal closed the session.
    Closed,
    /// The signal streams ended (the bus connection is gone).
    Lost,
}

/// Closes a session at the portal; a portal that does not answer within
/// two seconds is not waited for.
async fn close<T: ashpd::desktop::SessionPortal>(session: &ashpd::desktop::Session<T>) {
    match tokio::time::timeout(Duration::from_secs(2), session.close()).await {
        Ok(Ok(())) => tracing::debug!("portal session closed"),
        Ok(Err(e)) => tracing::debug!(%e, "portal session close"),
        Err(_) => tracing::debug!("portal session close timed out"),
    }
}

/// The D-Bus object path a value serializes to. `Session` keeps its own
/// path crate-private in ashpd, but serializes as exactly that path, which
/// is what the Activated and Deactivated signals name.
fn object_path<T: serde::Serialize + zvariant::Type>(value: &T) -> Option<OwnedObjectPath> {
    let ctxt = zvariant::serialized::Context::new_dbus(zvariant::LE, 0);
    let data = zvariant::to_bytes(ctxt, value).ok()?;
    data.deserialize::<OwnedObjectPath>()
        .ok()
        .map(|(path, _)| path)
}

/// Puts Activated and Deactivated back in order. They come on two
/// independent signal streams; when both have an event queued, the release
/// can be read before its press, and a press applied last would hold PTT —
/// an open microphone. Per shortcut, an event older than the last one
/// handled is dropped. Equal timestamps keep arrival order: a portal that
/// sends none (all zero) behaves as before.
#[derive(Debug, Default)]
struct EventOrder(HashMap<String, Duration>);

impl EventOrder {
    /// Whether the event for `id` at `at` is to be handled.
    fn accept(&mut self, id: &str, at: Duration) -> bool {
        match self.0.get_mut(id) {
            Some(last) if at < *last && *last - at < REORDER_WINDOW => false,
            Some(last) => {
                *last = at;
                true
            }
            None => {
                self.0.insert(id.to_owned(), at);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn events_in_order_all_count() {
        let mut order = EventOrder::default();
        assert!(order.accept("ptt", ms(1000)), "press");
        assert!(order.accept("ptt", ms(1180)), "release");
        assert!(order.accept("ptt", ms(2500)), "press");
    }

    /// Both queued, the release read first: the press behind it is stale and
    /// must not open the microphone for good.
    #[test]
    fn a_press_read_after_its_release_is_dropped() {
        let mut order = EventOrder::default();
        assert!(order.accept("ptt", ms(1180)), "release, read first");
        assert!(!order.accept("ptt", ms(1000)), "its press, read second");
        assert!(order.accept("ptt", ms(2500)), "the next press counts");
    }

    #[test]
    fn shortcuts_are_ordered_separately() {
        let mut order = EventOrder::default();
        assert!(order.accept("ptt", ms(5000)));
        assert!(order.accept("mute", ms(4000)), "another shortcut");
        assert!(!order.accept("mute", ms(3999)));
    }

    /// A portal without timestamps sends zeros: arrival order, as before.
    #[test]
    fn equal_timestamps_keep_arrival_order() {
        let mut order = EventOrder::default();
        for _ in 0..4 {
            assert!(order.accept("ptt", Duration::ZERO));
        }
    }

    /// A clock set back must not silence the keys until it catches up.
    #[test]
    fn a_large_step_back_is_a_clock_jump() {
        let mut order = EventOrder::default();
        assert!(order.accept("ptt", Duration::from_secs(7200)));
        assert!(
            order.accept("ptt", Duration::from_secs(10)),
            "clock set back"
        );
        assert!(order.accept("ptt", Duration::from_secs(11)));
        assert!(!order.accept("ptt", ms(10_500)), "reordered again");
    }

    /// The session's own path is read through its serialized form; checked
    /// here on a plain object path, which serializes the same way.
    #[test]
    fn object_paths_round_trip() {
        let path =
            zvariant::ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_42/ashpd_x")
                .unwrap();
        let read = object_path(&path).expect("a path");
        assert_eq!(read.as_str(), path.as_str());
    }
}
