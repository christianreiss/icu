//! evdev hotkeys: read `/dev/input/event*` read-only and without grabbing
//! (Spike 4a). Requires the user to be in group `input`. Devices are
//! rescanned every few seconds for hot-plug.

use std::{
    collections::HashSet,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use evdev::{Device, EventSummary, KeyCode};
use tokio::sync::mpsc::UnboundedSender;

use super::{Action, Dispatch};
use crate::{config::HotkeySettings, engine::Command};

/// Parses `KEY_F12`, `BTN_SIDE` or `code:<n>`.
pub fn parse_key(name: &str) -> Option<KeyCode> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Some(code) = name.strip_prefix("code:") {
        return code.parse::<u16>().ok().map(KeyCode::new);
    }
    KeyCode::from_str(name).ok()
}

/// The binding name for `code`, in the form [`parse_key`] reads back:
/// `KEY_F12`, `BTN_SIDE`, `BTN_EXTRA`, or `code:<n>` for a code evdev has
/// no name for.
pub fn key_name(code: KeyCode) -> String {
    let name = format!("{code:?}");
    if parse_key(&name) == Some(code) {
        name
    } else {
        format!("code:{}", code.code())
    }
}

/// What [`capture_next_key`] says when there is nothing it may read.
const NO_DEVICE: &str = "ICU kann keine Tastatur und keine Maus lesen. Tasten direkt lesen \
                         darf ICU nur als Mitglied der Gruppe „input“.";

/// Waits for the next key or button press on any input device and returns
/// its binding name ([`key_name`]) — for the settings' „Taste drücken“.
/// Call it from a background task.
///
/// Every readable `/dev/input/event*` is opened read-only and not grabbed,
/// so the press still reaches whatever has the focus, and every device is
/// closed again when this returns. A press counts; releases and auto-repeat
/// do not, nor do left/right clicks and touch contacts ([`captured`]).
/// Errors, as text for the window: no device readable (not in group
/// `input`), or no press within `timeout`.
pub async fn capture_next_key(timeout: Duration) -> Result<String, String> {
    let devices = tokio::task::spawn_blocking(open_all)
        .await
        .map_err(|e| e.to_string())?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<KeyCode>(1);
    // Dropped on return, which aborts every reader and closes its device.
    let mut readers = tokio::task::JoinSet::new();
    for (path, dev) in devices {
        if dev.supported_keys().is_none() {
            continue;
        }
        let mut stream = match dev.into_event_stream() {
            Ok(stream) => stream,
            Err(e) => {
                tracing::debug!(?path, %e, "key capture: no event stream");
                continue;
            }
        };
        let tx = tx.clone();
        readers.spawn(async move {
            while let Ok(ev) = stream.next_event().await {
                if let Some(code) = captured(ev) {
                    let _ = tx.send(code).await;
                    return;
                }
            }
        });
    }
    drop(tx);
    if readers.is_empty() {
        return Err(NO_DEVICE.into());
    }
    match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(code)) => Ok(key_name(code)),
        Ok(None) => Err("Die Eingabegeräte sind verschwunden.".into()),
        Err(_) => Err("Keine Taste gedrückt.".into()),
    }
}

/// The key an event presses, if it is one to offer as a binding. Left and
/// right click operate the window the capture runs in, and touchpads and
/// pens report every contact as a key (`BTN_TOUCH`, `BTN_TOOL_*`): none of
/// those is what the user meant to press.
fn captured(ev: evdev::InputEvent) -> Option<KeyCode> {
    let EventSummary::Key(_, code, 1) = ev.destructure() else {
        return None;
    };
    let click = code == KeyCode::BTN_LEFT || code == KeyCode::BTN_RIGHT;
    let touch =
        (KeyCode::BTN_TOOL_PEN.code()..=KeyCode::BTN_TOOL_QUADTAP.code()).contains(&code.code());
    (!click && !touch).then_some(code)
}

/// One readable input device.
#[derive(Debug, Clone)]
pub struct InputDevice {
    /// `/dev/input/eventN`.
    pub path: PathBuf,
    /// Device name.
    pub name: String,
    /// Has letter keys.
    pub keyboard: bool,
    /// Has mouse buttons.
    pub mouse: bool,
}

/// `/dev/input/event*`.
fn event_paths() -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir("/dev/input") else {
        return Vec::new();
    };
    dir.flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"))
        })
        .collect()
}

/// Opens an input device **read-only**: hotkeys only ever read, and
/// `evdev::enumerate`/`Device::open` try read-write first.
fn open_read_only(path: &std::path::Path) -> Option<Device> {
    let file = std::fs::File::open(path).ok()?;
    Device::from_fd(std::os::fd::OwnedFd::from(file)).ok()
}

/// Every input device this user may read.
fn open_all() -> Vec<(PathBuf, Device)> {
    event_paths()
        .into_iter()
        .filter_map(|path| open_read_only(&path).map(|dev| (path, dev)))
        .collect()
}

/// Lists devices that can be opened.
pub fn list_devices() -> Vec<InputDevice> {
    open_all()
        .into_iter()
        .map(|(path, dev)| {
            let keys = dev.supported_keys();
            InputDevice {
                path,
                name: dev.name().unwrap_or("?").to_string(),
                keyboard: keys.map(|k| k.contains(KeyCode::KEY_A)).unwrap_or(false),
                mouse: keys.map(|k| k.contains(KeyCode::BTN_LEFT)).unwrap_or(false),
            }
        })
        .collect()
}

/// The tasks of one running evdev backend: the readers (one per device)
/// and the hot-plug rescan. Stopping aborts them all at once, and a task
/// added after the stop — a rescan that was still running — is aborted
/// right away.
#[derive(Clone, Default)]
struct Tasks(Arc<Mutex<TaskSet>>);

#[derive(Default)]
struct TaskSet {
    stopped: bool,
    tasks: Vec<tokio::task::AbortHandle>,
}

impl Tasks {
    fn add(&self, task: &tokio::task::JoinHandle<()>) {
        let mut set = self.0.lock().unwrap();
        if set.stopped {
            task.abort();
        } else {
            // Readers of unplugged devices have ended by themselves.
            set.tasks.retain(|t| !t.is_finished());
            set.tasks.push(task.abort_handle());
        }
    }

    fn stop(&self) {
        let mut set = self.0.lock().unwrap();
        set.stopped = true;
        for task in set.tasks.drain(..) {
            task.abort();
        }
    }
}

/// Stops the reader tasks when dropped — at once, not at their next
/// wake-up: an old and a new backend reading side by side would fire every
/// toggle key twice. An aborted reader drops its `Dispatch`, which releases
/// a held PTT key.
pub struct Guard {
    tasks: Tasks,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.tasks.stop();
    }
}

/// Starts readers for every device that has one of the bound keys. Must be
/// called from within a tokio runtime.
pub async fn start(
    settings: &HotkeySettings,
    cmd: UnboundedSender<Command>,
) -> Result<(Guard, usize), String> {
    let bindings: Vec<(KeyCode, Action)> = super::bindings(settings)
        .into_iter()
        .filter_map(|(b, a)| parse_key(&b.key).map(|k| (k, a)))
        .collect();
    if bindings.is_empty() {
        return Err("no evdev key bound".into());
    }
    let tasks = Tasks::default();
    let opened = Arc::new(Mutex::new(HashSet::<PathBuf>::new()));
    let n = scan_blocking(&bindings, &cmd, &tasks, &opened).await;
    if n == 0 {
        tasks.stop();
        return Err("no readable input device has the bound keys".into());
    }
    // Hot-plug: rescan periodically.
    let rescan = {
        let tasks = tasks.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                scan_blocking(&bindings, &cmd, &tasks, &opened).await;
            }
        })
    };
    tasks.add(&rescan);
    Ok((Guard { tasks }, n))
}

/// [`scan`] on the blocking pool: it opens and queries every input device,
/// which must not stall the runtime's workers — they read the audio
/// datagrams and time their arrival.
async fn scan_blocking(
    bindings: &[(KeyCode, Action)],
    cmd: &UnboundedSender<Command>,
    tasks: &Tasks,
    opened: &Arc<Mutex<HashSet<PathBuf>>>,
) -> usize {
    let (bindings, cmd, tasks, opened) = (
        bindings.to_vec(),
        cmd.clone(),
        tasks.clone(),
        opened.clone(),
    );
    tokio::task::spawn_blocking(move || scan(&bindings, &cmd, &tasks, &opened))
        .await
        .unwrap_or(0)
}

fn scan(
    bindings: &[(KeyCode, Action)],
    cmd: &UnboundedSender<Command>,
    tasks: &Tasks,
    opened: &Arc<Mutex<HashSet<PathBuf>>>,
) -> usize {
    let mut n = 0;
    for path in event_paths() {
        if opened.lock().unwrap().contains(&path) {
            continue;
        }
        let Some(dev) = open_read_only(&path) else {
            continue;
        };
        let Some(keys) = dev.supported_keys() else {
            continue;
        };
        if !bindings.iter().any(|(k, _)| keys.contains(*k)) {
            continue;
        }
        opened.lock().unwrap().insert(path.clone());
        n += 1;
        tracing::info!(
            ?path,
            name = dev.name().unwrap_or("?"),
            "evdev hotkey device"
        );
        let reader = spawn_reader(dev, path, bindings.to_vec(), cmd.clone(), opened.clone());
        tasks.add(&reader);
    }
    n
}

fn spawn_reader(
    dev: Device,
    path: PathBuf,
    bindings: Vec<(KeyCode, Action)>,
    cmd: UnboundedSender<Command>,
    opened: Arc<Mutex<HashSet<PathBuf>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut stream = match dev.into_event_stream() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?path, %e, "evdev stream");
                opened.lock().unwrap().remove(&path);
                return;
            }
        };
        // Dropped on every way out, abort included: a PTT key held then is
        // released, or the mic would stay open.
        let mut dispatch = Dispatch::new(cmd);
        loop {
            let ev = match stream.next_event().await {
                Ok(ev) => ev,
                Err(_) => {
                    // Device gone; let the rescan pick it up again.
                    opened.lock().unwrap().remove(&path);
                    return;
                }
            };
            if let EventSummary::Key(_, code, value) = ev.destructure() {
                // Any key going down (a mouse button is not typing), for the
                // voice gate's attack; just the time, never the key.
                if value == 1 && code.code() < 0x100 {
                    super::typing::note();
                }
                let pressed = match value {
                    1 => true,
                    0 => false,
                    _ => continue,
                };
                for (k, action) in &bindings {
                    if *k == code {
                        dispatch.key(*action, pressed);
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_names_and_codes() {
        assert_eq!(parse_key("KEY_F12"), Some(KeyCode::KEY_F12));
        assert_eq!(parse_key("BTN_SIDE"), Some(KeyCode::BTN_SIDE));
        assert_eq!(parse_key("code:97"), Some(KeyCode::KEY_RIGHTCTRL));
        assert_eq!(parse_key(""), None);
        assert_eq!(parse_key("KEY_NOPE"), None);
    }

    /// What the capture returns, the evdev backend reads back as the same
    /// key — for every code there is, named or not.
    #[test]
    fn captured_names_read_back() {
        for code in 0..0x300 {
            let code = KeyCode::new(code);
            let name = key_name(code);
            assert_eq!(parse_key(&name), Some(code), "{name}");
        }
        assert_eq!(key_name(KeyCode::KEY_F12), "KEY_F12");
        assert_eq!(key_name(KeyCode::KEY_RIGHTCTRL), "KEY_RIGHTCTRL");
        assert_eq!(key_name(KeyCode::BTN_SIDE), "BTN_SIDE");
        assert_eq!(key_name(KeyCode::BTN_EXTRA), "BTN_EXTRA");
        assert_eq!(key_name(KeyCode::new(0x2f0)), "code:752", "no name");
    }

    #[test]
    fn only_presses_of_real_keys_are_captured() {
        use evdev::{EventType, InputEvent};
        let key = |code: KeyCode, value| InputEvent::new(EventType::KEY.0, code.code(), value);
        assert_eq!(captured(key(KeyCode::KEY_F12, 1)), Some(KeyCode::KEY_F12));
        assert_eq!(captured(key(KeyCode::BTN_SIDE, 1)), Some(KeyCode::BTN_SIDE));
        assert_eq!(
            captured(key(KeyCode::BTN_MIDDLE, 1)),
            Some(KeyCode::BTN_MIDDLE)
        );
        assert_eq!(captured(key(KeyCode::KEY_F12, 0)), None, "release");
        assert_eq!(captured(key(KeyCode::KEY_F12, 2)), None, "auto-repeat");
        assert_eq!(captured(key(KeyCode::BTN_LEFT, 1)), None, "click");
        assert_eq!(captured(key(KeyCode::BTN_RIGHT, 1)), None, "click");
        assert_eq!(captured(key(KeyCode::BTN_TOUCH, 1)), None, "touchpad");
        assert_eq!(captured(key(KeyCode::BTN_TOOL_FINGER, 1)), None, "touchpad");
        let motion = InputEvent::new(EventType::RELATIVE.0, 0, 5);
        assert_eq!(captured(motion), None);
    }

    /// Dropping the guard ends a reader at once — not at its next wake-up —
    /// and the reader's `Dispatch` releases the PTT key it held.
    #[tokio::test]
    async fn dropping_the_guard_releases_a_held_key_at_once() {
        let (cmd, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let guard = Guard {
            tasks: Tasks::default(),
        };
        let reader = tokio::spawn(async move {
            let mut dispatch = Dispatch::new(cmd);
            dispatch.key(Action::Ptt, true);
            std::future::pending::<()>().await;
        });
        guard.tasks.add(&reader);
        assert!(matches!(commands.recv().await, Some(Command::Ptt(true))));
        let tasks = guard.tasks.clone();
        drop(guard);
        let released = tokio::time::timeout(Duration::from_millis(500), commands.recv())
            .await
            .expect("released at once");
        assert!(matches!(released, Some(Command::Ptt(false))));
        // A reader a late rescan still spawns is stopped on arrival.
        let late = tokio::spawn(std::future::pending::<()>());
        tasks.add(&late);
        assert!(late.await.unwrap_err().is_cancelled());
    }
}
