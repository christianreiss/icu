//! Global hotkeys: Portal (Linux, primary), evdev (Linux, opt-in/fallback),
//! Raw Input (Windows).

#[cfg(any(windows, test))]
mod chord;
#[cfg(target_os = "linux")]
pub mod evdev;
pub mod keys;
#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(target_os = "windows")]
pub mod rawinput;

use std::{future::Future, pin::Pin, time::Duration};

use tokio::sync::{mpsc::UnboundedSender, watch};

#[cfg(target_os = "linux")]
use crate::config::HotkeyBackend;
use crate::{
    config::{HotkeySettings, KeyBinding},
    engine::{AppState, Command},
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
    /// Toggle the camera on press.
    Camera,
}

impl Action {
    /// The command for a key transition.
    pub fn command(self, pressed: bool) -> Option<Command> {
        match (self, pressed) {
            (Action::Ptt, p) => Some(Command::Ptt(p)),
            (Action::Mute, true) => Some(Command::ToggleMute),
            (Action::Deafen, true) => Some(Command::ToggleDeafen),
            (Action::ToggleMode, true) => Some(Command::ToggleMode),
            (Action::Camera, true) => Some(Command::ToggleCamera),
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
            Action::Camera => "camera",
        }
    }

    /// What the portal's dialog shows for this action.
    pub fn description(self) -> &'static str {
        match self {
            Action::Ptt => "ICU Push-to-Talk",
            Action::Mute => "ICU Mute",
            Action::Deafen => "ICU Deafen",
            Action::ToggleMode => "ICU PTT/VAD",
            Action::Camera => "ICU Kamera",
        }
    }
}

/// The bindings and what each does, in the order the backends bind them.
pub fn bindings(settings: &HotkeySettings) -> [(&KeyBinding, Action); 5] {
    [
        (&settings.ptt, Action::Ptt),
        (&settings.mute, Action::Mute),
        (&settings.deafen, Action::Deafen),
        (&settings.toggle_mode, Action::ToggleMode),
        (&settings.camera, Action::Camera),
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

/// `Hotkeys::backend` of the Windows backend.
pub const BACKEND_RAWINPUT: &str = "Raw Input";

/// Waits for the next key or button press on any keyboard or mouse and
/// returns its binding name (`KEY_F12`, `BTN_SIDE`, `code:<n>`) — for the
/// settings' „Taste drücken“. Errors as text for the window.
pub async fn capture_next_key(timeout: Duration) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    return evdev::capture_next_key(timeout).await;
    #[cfg(target_os = "windows")]
    return rawinput::capture_next_key(timeout).await;
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = timeout;
        Err("Tasten lesen geht auf diesem System nicht.".into())
    }
}

/// `AppState::hotkey_backend` from the moment the supervisor is started
/// until the first backend reports: the portal's dialog may be waiting for
/// an answer. The empty string stays for „never started“ (`--no-hotkeys`).
pub const BACKEND_STARTING: &str = "starting";

/// What the desktop assigned in the running portal session, by portal id,
/// in its own words (`trigger_description`, e.g. KDE's
/// „Strg+Umschalt+F12“), tagged with the session that bound it. The dialog
/// lets the user pick another key than the one ICU proposed, so this — not
/// the proposal in the settings — is the key that works. Empty while no
/// portal session runs.
static PORTAL_TRIGGERS: std::sync::Mutex<(u64, Vec<(String, String)>)> =
    std::sync::Mutex::new((0, Vec::new()));

/// The key the desktop assigned to `action` in the running portal session;
/// `None` without one, or when the desktop named no key. The windows show
/// it instead of the proposed trigger (`ui::ptt_key_label`).
pub fn portal_trigger(action: Action) -> Option<String> {
    let bound = PORTAL_TRIGGERS.lock().unwrap_or_else(|e| e.into_inner());
    bound
        .1
        .iter()
        .find(|(id, desc)| id == action.portal_id() && !desc.trim().is_empty())
        .map(|(_, desc)| desc.clone())
}

/// Records what a new portal session bound; returns the session's tag for
/// [`portal_unbound`].
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn portal_bound(triggers: Vec<(String, String)>) -> u64 {
    let mut bound = PORTAL_TRIGGERS.lock().unwrap_or_else(|e| e.into_inner());
    bound.0 += 1;
    bound.1 = triggers;
    bound.0
}

/// Forgets what session `tag` bound — unless a newer one replaced it
/// meanwhile: the old session's task ends after the next one has bound.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn portal_unbound(tag: u64) {
    let mut bound = PORTAL_TRIGGERS.lock().unwrap_or_else(|e| e.into_inner());
    if bound.0 == tag {
        bound.1.clear();
    }
}

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
        match rawinput::start(settings, cmd).await {
            Ok(guard) => Hotkeys {
                backend: BACKEND_RAWINPUT.into(),
                _guard: Some(Box::new(guard)),
                ended: None,
            },
            Err(e) => {
                tracing::warn!(%e, "raw input hotkeys unavailable");
                Hotkeys::idle(BACKEND_NONE)
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (settings, cmd);
        Hotkeys::idle(BACKEND_NONE)
    }
}

/// How long the bindings have to hold still before a changed set is bound:
/// somebody is typing `KEY_F12` into the settings, one letter per update.
const SETTLE: Duration = Duration::from_millis(1500);

/// Pause before binding again after a backend ended by itself.
const REBIND_DELAY: Duration = Duration::from_secs(2);

/// What the supervisor follows: the bindings, and the counter
/// `Command::RetryHotkeys` bumps (`AppState::hotkey_retries`).
#[derive(Debug, Clone, PartialEq)]
struct Wanted {
    hotkeys: HotkeySettings,
    retries: u32,
}

/// Starts a backend for the supervisor; [`start`] outside the tests.
type Starter = Box<
    dyn FnMut(
            HotkeySettings,
            UnboundedSender<Command>,
        ) -> Pin<Box<dyn Future<Output = Hotkeys> + Send>>
        + Send,
>;

/// Runs the hotkey backend for as long as the engine runs, and keeps it in
/// step with the settings, so a change applies without a restart:
///
/// - a changed binding or backend choice is bound once the settings held
///   still for 1.5 s (typing), the old backend dropped first — no two
///   readers at once, and a held PTT key is released (`Dispatch`);
/// - `Command::RetryHotkeys` binds again at once, which makes the portal ask
///   again after its dialog was turned down;
/// - a backend that ended by itself (the portal closed the session, e.g. on
///   a portal restart) is bound again after 2 s.
///
/// Every backend that comes up is reported as `Command::HotkeyBackend`.
/// Ends when the engine's state channel closes.
pub async fn supervise(state: watch::Receiver<AppState>, cmd: UnboundedSender<Command>) {
    supervise_with(
        state,
        |s: &AppState| Wanted {
            hotkeys: s.settings.hotkeys.clone(),
            retries: s.hotkey_retries,
        },
        cmd,
        (SETTLE, REBIND_DELAY),
        Box::new(|settings, cmd| Box::pin(async move { start(&settings, cmd).await })),
    )
    .await;
}

/// [`supervise`] over any watched value, with the timings and the starter
/// passed in (tests).
async fn supervise_with<T>(
    mut state: watch::Receiver<T>,
    view: impl Fn(&T) -> Wanted,
    cmd: UnboundedSender<Command>,
    (settle_for, rebind_delay): (Duration, Duration),
    mut start: Starter,
) {
    let mut wanted = view(&state.borrow_and_update());
    loop {
        let mut hotkeys = start(wanted.hotkeys.clone(), cmd.clone()).await;
        tracing::info!(backend = %hotkeys.backend, "hotkeys");
        let _ = cmd.send(Command::HotkeyBackend(hotkeys.backend.clone()));
        let next = loop {
            tokio::select! {
                changed = state.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let now = view(&state.borrow_and_update());
                    if now.retries != wanted.retries {
                        break now;
                    }
                    if now.hotkeys == wanted.hotkeys {
                        continue;
                    }
                    let Some(settled) = settle(&mut state, &view, now, settle_for).await else {
                        return;
                    };
                    if settled != wanted {
                        break settled;
                    }
                }
                () = hotkeys.ended() => {
                    tracing::info!("hotkey backend ended, binding again");
                    tokio::time::sleep(rebind_delay).await;
                    break view(&state.borrow_and_update());
                }
            }
        };
        // The old backend goes before the new one starts.
        drop(hotkeys);
        wanted = next;
    }
}

/// Waits until the bindings held still for `quiet` and returns them, or
/// returns at once on a retry request; `None` once the engine is gone.
async fn settle<T>(
    state: &mut watch::Receiver<T>,
    view: &impl Fn(&T) -> Wanted,
    mut latest: Wanted,
    quiet: Duration,
) -> Option<Wanted> {
    let deadline = tokio::time::sleep(quiet);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return Some(latest),
            changed = state.changed() => {
                changed.ok()?;
                let now = view(&state.borrow_and_update());
                if now != latest {
                    let retry = now.retries != latest.retries;
                    latest = now;
                    if retry {
                        return Some(latest);
                    }
                    deadline.as_mut().reset(tokio::time::Instant::now() + quiet);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Instant,
    };

    use tokio::sync::{mpsc, oneshot};

    use super::*;

    /// The key the desktop assigned is what the windows show while its
    /// session runs; a session that ended after the next one bound does not
    /// take the newer keys with it. The only test touching the registry,
    /// and without push-to-talk, whose label other tests read.
    #[test]
    fn the_portal_assigned_key_lasts_as_long_as_its_session() {
        let first = portal_bound(vec![
            ("camera".into(), "Strg+Umschalt+K".into()),
            ("mute".into(), " ".into()),
        ]);
        assert_eq!(
            portal_trigger(Action::Camera).as_deref(),
            Some("Strg+Umschalt+K")
        );
        assert_eq!(portal_trigger(Action::Mute), None, "no key named");
        assert_eq!(portal_trigger(Action::Deafen), None, "not bound");
        let second = portal_bound(vec![("camera".into(), "Pause".into())]);
        portal_unbound(first);
        assert_eq!(portal_trigger(Action::Camera).as_deref(), Some("Pause"));
        portal_unbound(second);
        assert_eq!(portal_trigger(Action::Camera), None);
    }

    /// What the fake backends did, in order.
    type Log = Arc<Mutex<Vec<String>>>;

    /// The switches that end each fake backend, in start order.
    type Ends = Arc<Mutex<Vec<oneshot::Sender<()>>>>;

    /// A fake backend's guard: logs being dropped.
    struct Probe(Log, String);

    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.lock().unwrap().push(format!("drop {}", self.1));
        }
    }

    /// Starts fake backends named after the PTT key they were given.
    fn fake(log: Log, ends: Ends) -> Starter {
        Box::new(move |settings: HotkeySettings, _cmd| {
            let key = settings.ptt.key.clone();
            log.lock().unwrap().push(format!("start {key}"));
            let (end, ended) = oneshot::channel();
            ends.lock().unwrap().push(end);
            let probe = Probe(log.clone(), key.clone());
            Box::pin(async move {
                Hotkeys {
                    backend: key,
                    _guard: Some(Box::new(probe)),
                    ended: Some(ended),
                }
            })
        })
    }

    fn wanted(key: &str, retries: u32) -> Wanted {
        let mut hotkeys = HotkeySettings::default();
        hotkeys.ptt.key = key.into();
        Wanted { hotkeys, retries }
    }

    struct Harness {
        state: watch::Sender<Wanted>,
        log: Log,
        ends: Ends,
        reported: mpsc::UnboundedReceiver<Command>,
        task: tokio::task::JoinHandle<()>,
    }

    fn harness(first: Wanted, settle_for: Duration) -> Harness {
        let (state, rx) = watch::channel(first);
        let (log, ends) = (Log::default(), Ends::default());
        let (cmd, reported) = mpsc::unbounded_channel();
        let task = tokio::spawn(supervise_with(
            rx,
            Wanted::clone,
            cmd,
            (settle_for, Duration::from_millis(50)),
            fake(log.clone(), ends.clone()),
        ));
        Harness {
            state,
            log,
            ends,
            reported,
            task,
        }
    }

    impl Harness {
        /// The next backend the supervisor reports.
        async fn next_backend(&mut self) -> String {
            match tokio::time::timeout(Duration::from_secs(5), self.reported.recv()).await {
                Ok(Some(Command::HotkeyBackend(backend))) => backend,
                other => panic!("no backend reported: {other:?}"),
            }
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    /// Typing a key name changes the settings once per letter; the backend
    /// is bound once, with what stands when the typing stopped, and the old
    /// one is gone before the new one starts.
    #[tokio::test]
    async fn a_changed_binding_is_bound_once_it_settles() {
        let settle_for = Duration::from_millis(200);
        let mut h = harness(wanted("KEY_A", 0), settle_for);
        assert_eq!(h.next_backend().await, "KEY_A");
        // Anything else in the state changing is no reason to bind again.
        h.state.send_replace(wanted("KEY_A", 0));
        let typed = Instant::now();
        for key in ["KEY_F", "KEY_F1", "KEY_F12"] {
            h.state.send_replace(wanted(key, 0));
        }
        assert_eq!(h.next_backend().await, "KEY_F12");
        assert!(typed.elapsed() >= settle_for, "waited for the typing");
        tokio::time::sleep(settle_for * 2).await;
        assert!(h.reported.try_recv().is_err(), "bound once");
        assert_eq!(h.log(), ["start KEY_A", "drop KEY_A", "start KEY_F12"]);
    }

    /// „Erneut fragen“: the same settings, bound again right away.
    #[tokio::test]
    async fn a_retry_binds_again_at_once() {
        let settle_for = Duration::from_secs(5);
        let mut h = harness(wanted("KEY_A", 0), settle_for);
        h.next_backend().await;
        let asked = Instant::now();
        h.state.send_replace(wanted("KEY_A", 1));
        assert_eq!(h.next_backend().await, "KEY_A");
        assert!(asked.elapsed() < settle_for, "no settling for a retry");
        assert_eq!(h.log(), ["start KEY_A", "drop KEY_A", "start KEY_A"]);
    }

    /// The portal closed the session (it restarted): bind again.
    #[tokio::test]
    async fn a_backend_that_ended_is_bound_again() {
        let mut h = harness(wanted("KEY_A", 0), Duration::from_secs(5));
        h.next_backend().await;
        let end = h.ends.lock().unwrap().remove(0);
        let _ = end.send(());
        assert_eq!(h.next_backend().await, "KEY_A");
        assert_eq!(h.log(), ["start KEY_A", "drop KEY_A", "start KEY_A"]);
    }

    /// A settings file from before the camera key has none bound; a key
    /// bound to it toggles the camera on press only.
    #[test]
    fn the_camera_key_is_unset_by_default_and_toggles() {
        let old: crate::config::Settings =
            toml::from_str("[hotkeys]\nptt = { key = \"KEY_F12\" }\n").unwrap();
        assert_eq!(old.hotkeys.camera, KeyBinding::default());
        assert_eq!(old.hotkeys.ptt.key, "KEY_F12");
        assert_eq!(HotkeySettings::default().camera, KeyBinding::default());
        assert!(matches!(
            Action::Camera.command(true),
            Some(Command::ToggleCamera)
        ));
        assert!(Action::Camera.command(false).is_none());
        assert_eq!(Action::Camera.portal_id(), "camera");
        let mut settings = HotkeySettings::default();
        settings.camera.trigger = Some("CTRL+SHIFT+F10".into());
        assert!(bindings(&settings)
            .iter()
            .any(|(b, a)| *a == Action::Camera && b.trigger.as_deref() == Some("CTRL+SHIFT+F10")));
    }

    #[tokio::test]
    async fn the_supervisor_ends_with_the_engine() {
        let mut h = harness(wanted("KEY_A", 0), Duration::from_secs(5));
        h.next_backend().await;
        let Harness {
            state, task, log, ..
        } = h;
        drop(state);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("ends")
            .unwrap();
        assert_eq!(log.lock().unwrap().last().unwrap(), "drop KEY_A");
    }
}
