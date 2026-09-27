//! Raw Input hotkeys (Windows): a hidden message-only window on a thread of
//! its own takes `WM_INPUT` for every keyboard with `RIDEV_INPUTSINK`, so
//! keys reach ICU while another window has the focus — and for every mouse
//! only while a mouse button is bound or „Aufnehmen“ runs: a gaming mouse
//! reports its movement up to 8000 times a second, and each report would
//! wake the reader.
//! Nothing is swallowed: the press still reaches that window. What Windows
//! hands to an elevated window in the foreground (a game run as
//! administrator) stays invisible to a normal process (UIPI).
//!
//! Raw Input is registered per process, not per window: the last
//! `RegisterRawInputDevices` for keyboards takes them from the window that
//! had them. winit registers its own window when its event loop starts, so
//! the reader checks every two seconds that the keyboards (and the mice,
//! while it wants them) are still its own and takes them back if not. One
//! reader serves the backend and „Aufnehmen" alike ([`capture_next_key`],
//! which also takes combinations: `super::chord::Recorder`); it runs from
//! its first use until the process ends.
//!
//! Keys are named as evdev names them (`super::keys`), so the bindings in
//! the settings mean the same keys on Linux and Windows.

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicIsize, AtomicU64, AtomicUsize, Ordering},
        mpsc as std_mpsc, Arc, Mutex,
    },
    time::Duration,
};

use tokio::sync::{mpsc::UnboundedSender, oneshot};
use windows::{
    core::w,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::{
                GetRawInputData, GetRegisteredRawInputDevices,
                KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC_EX, VK_PAUSE},
                RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
                RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW,
                RegisterClassExW, SetTimer, HWND_MESSAGE, MSG, RI_KEY_BREAK, RI_KEY_E0, RI_KEY_E1,
                RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, RI_MOUSE_BUTTON_5_DOWN,
                RI_MOUSE_BUTTON_5_UP, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP,
                RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP, RI_MOUSE_RIGHT_BUTTON_DOWN,
                RI_MOUSE_RIGHT_BUTTON_UP, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_INPUT,
                WM_TIMER, WNDCLASSEXW,
            },
        },
    },
};

use super::{keys, Action, Dispatch};
use crate::{config::HotkeySettings, engine::Command};

/// Generic desktop controls (HID usage page 0x01): mouse and keyboard.
const USAGE_PAGE_GENERIC: u16 = 0x01;
const USAGE_MOUSE: u16 = 0x02;
const USAGE_KEYBOARD: u16 = 0x06;

/// How often the reader makes sure the keys are still its own.
const RECLAIM_MS: u32 = 2000;

/// Posted to the reader's window when the mice are wanted, or no longer.
const WM_MICE: u32 = WM_APP + 1;

/// Gets every key transition, `(evdev code, pressed)`; returns `false` to
/// be removed.
type Listener = Box<dyn FnMut(u16, bool) -> bool + Send>;

/// The listeners of the one reader.
#[derive(Default)]
struct Hub {
    next: AtomicU64,
    listeners: Mutex<Vec<(u64, Listener)>>,
    /// How many want the mice read ([`Mice`]).
    mice: AtomicUsize,
    /// The reader's window, for [`WM_MICE`].
    window: AtomicIsize,
}

impl Hub {
    fn wants_mice(&self) -> bool {
        self.mice.load(Ordering::SeqCst) > 0
    }

    /// Has the reader thread register or drop the mice to match
    /// [`Hub::wants_mice`].
    fn update_mice(&self) {
        let hwnd = HWND(self.window.load(Ordering::SeqCst) as _);
        // SAFETY: posting to the reader's window; a failure (the reader
        // gone) leaves the registration as it is.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_MICE, WPARAM(0), LPARAM(0)) };
    }

    fn add(&self, listener: Listener) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.listeners.lock().unwrap().push((id, listener));
        id
    }

    /// Removes a listener; dropping it (and the `Dispatch` it holds)
    /// releases a held push-to-talk key.
    fn remove(&self, id: u64) {
        let gone: Vec<_> = {
            let mut listeners = self.listeners.lock().unwrap();
            let (gone, keep) = listeners.drain(..).partition(|(i, _)| *i == id);
            *listeners = keep;
            gone
        };
        drop(gone);
    }

    fn emit(&self, code: u16, pressed: bool) {
        let gone: Vec<_> = {
            let mut listeners = self.listeners.lock().unwrap();
            let mut gone = Vec::new();
            let mut i = 0;
            while i < listeners.len() {
                if (listeners[i].1)(code, pressed) {
                    i += 1;
                } else {
                    gone.push(listeners.remove(i));
                }
            }
            gone
        };
        drop(gone);
    }
}

static READER: Mutex<Option<Arc<Hub>>> = Mutex::new(None);

/// Keeps the mice registered for as long as it lives: a backend with a
/// mouse button bound, or a running „Aufnehmen“.
struct Mice(Arc<Hub>);

impl Mice {
    fn new(hub: Arc<Hub>) -> Mice {
        hub.mice.fetch_add(1, Ordering::SeqCst);
        hub.update_mice();
        Mice(hub)
    }
}

impl Drop for Mice {
    fn drop(&mut self) {
        self.0.mice.fetch_sub(1, Ordering::SeqCst);
        self.0.update_mice();
    }
}

/// The running reader, started on first use; a start that failed is tried
/// again on the next use („Erneut versuchen“, a new binding).
fn hub() -> Result<Arc<Hub>, String> {
    let mut reader = READER.lock().unwrap();
    if let Some(hub) = &*reader {
        return Ok(hub.clone());
    }
    let hub = start_reader()?;
    *reader = Some(hub.clone());
    Ok(hub)
}

/// The hub the window procedure feeds, and the keys held down (to drop
/// auto-repeat). Only touched on the reader thread.
struct ReaderState {
    hub: Arc<Hub>,
    held: HashSet<u16>,
}

thread_local! {
    static STATE: std::cell::RefCell<Option<ReaderState>> = const { std::cell::RefCell::new(None) };
}

fn start_reader() -> Result<Arc<Hub>, String> {
    let hub = Arc::new(Hub::default());
    let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), String>>();
    let for_thread = hub.clone();
    std::thread::Builder::new()
        .name("icu-rawinput".into())
        .spawn(move || reader_thread(for_thread, ready_tx))
        .map_err(|e| e.to_string())?;
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "Raw Input reader did not start".to_string())??;
    Ok(hub)
}

fn reader_thread(hub: Arc<Hub>, ready: std_mpsc::Sender<Result<(), String>>) {
    let hwnd = match unsafe { create_window() } {
        Ok(hwnd) => hwnd,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    hub.window.store(hwnd.0 as isize, Ordering::SeqCst);
    STATE.with(|s| {
        *s.borrow_mut() = Some(ReaderState {
            hub,
            held: HashSet::new(),
        })
    });
    if let Err(e) = register(hwnd, false) {
        let _ = ready.send(Err(e));
        return;
    }
    unsafe { SetTimer(Some(hwnd), 1, RECLAIM_MS, None) };
    let _ = ready.send(Ok(()));
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe { DispatchMessageW(&msg) };
    }
}

unsafe fn create_window() -> Result<HWND, String> {
    let instance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: w!("icu-rawinput"),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(format!(
            "RegisterClassExW: {}",
            windows::core::Error::from_thread()
        ));
    }
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("icu-rawinput"),
            w!("ICU hotkeys"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW: {e}"))
}

/// The usages the reader takes: keyboards, and mice when `mice`.
fn usages(mice: bool) -> &'static [u16] {
    if mice {
        &[USAGE_KEYBOARD, USAGE_MOUSE]
    } else {
        &[USAGE_KEYBOARD]
    }
}

/// Takes keyboards — and mice when `mice` — for `hwnd`, in the background
/// too.
fn register(hwnd: HWND, mice: bool) -> Result<(), String> {
    let devices: Vec<RAWINPUTDEVICE> = usages(mice)
        .iter()
        .map(|usage| RAWINPUTDEVICE {
            usUsagePage: USAGE_PAGE_GENERIC,
            usUsage: *usage,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        })
        .collect();
    unsafe { RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32) }
        .map_err(|e| format!("RegisterRawInputDevices: {e}"))
}

/// Gives the mice up, if `hwnd` has them (not winit's registration).
fn release_mice(hwnd: HWND) {
    if !still_ours_for(hwnd, &[USAGE_MOUSE]) {
        return;
    }
    let device = RAWINPUTDEVICE {
        usUsagePage: USAGE_PAGE_GENERIC,
        usUsage: USAGE_MOUSE,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: HWND::default(),
    };
    if let Err(e) =
        unsafe { RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32) }
    {
        tracing::warn!(%e, "raw input: mice not released");
    }
}

/// Registers or releases the mice to match `mice`.
fn update_mice(hwnd: HWND, mice: bool) {
    if !mice {
        release_mice(hwnd);
    } else if !still_ours(hwnd, true) {
        if let Err(e) = register(hwnd, true) {
            tracing::warn!(%e, "raw input");
        }
    }
}

/// Whether keyboards (and mice when `mice`) still go to `hwnd` with
/// `RIDEV_INPUTSINK`.
fn still_ours(hwnd: HWND, mice: bool) -> bool {
    still_ours_for(hwnd, usages(mice))
}

/// Whether every one of `usages` still goes to `hwnd` with
/// `RIDEV_INPUTSINK`.
fn still_ours_for(hwnd: HWND, usages: &[u16]) -> bool {
    let mut devices = [RAWINPUTDEVICE::default(); 16];
    let mut n = devices.len() as u32;
    let got = unsafe {
        GetRegisteredRawInputDevices(
            Some(devices.as_mut_ptr()),
            &mut n,
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        )
    };
    if got == u32::MAX {
        return false;
    }
    usages.iter().all(|usage| {
        devices[..got as usize].iter().any(|d| {
            d.usUsagePage == USAGE_PAGE_GENERIC
                && d.usUsage == *usage
                && d.hwndTarget == hwnd
                && d.dwFlags.0 & RIDEV_INPUTSINK.0 != 0
        })
    })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            if let Some(input) = unsafe { read_input(HRAWINPUT(lparam.0 as _)) } {
                for (code, pressed) in transitions(&input) {
                    deliver(code, pressed);
                }
            }
        }
        WM_MICE => {
            update_mice(hwnd, wants_mice());
            return LRESULT(0);
        }
        WM_TIMER => {
            let mice = wants_mice();
            if !still_ours(hwnd, mice) {
                tracing::info!("raw input taken by another window, taking it back");
                if let Err(e) = register(hwnd, mice) {
                    tracing::warn!(%e, "raw input");
                }
                // A release that went to the other window is lost: let go
                // of every key held, or push-to-talk would stay open and
                // the next press be taken for auto-repeat.
                release_all();
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe fn read_input(handle: HRAWINPUT) -> Option<RAWINPUT> {
    let mut input = RAWINPUT::default();
    let mut size = std::mem::size_of::<RAWINPUT>() as u32;
    let got = unsafe {
        GetRawInputData(
            handle,
            RID_INPUT,
            Some(&mut input as *mut RAWINPUT as *mut _),
            &mut size,
            std::mem::size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    (got != u32::MAX && got != 0).then_some(input)
}

/// The key transitions one raw input carries, as evdev codes.
fn transitions(input: &RAWINPUT) -> Vec<(u16, bool)> {
    if input.header.dwType == RIM_TYPEKEYBOARD.0 {
        let kb = unsafe { input.data.keyboard };
        let flags = u32::from(kb.Flags);
        let pressed = flags & RI_KEY_BREAK == 0;
        // 0xFF: the second half of a multi-code key (Pause sends its E1 1D
        // with VK_PAUSE, then 45 with no key) — not NumLock.
        if kb.VKey == 0xff {
            return Vec::new();
        }
        let code = if kb.VKey == VK_PAUSE.0 || flags & RI_KEY_E1 != 0 {
            (kb.VKey == VK_PAUSE.0).then_some(keys::KEY_PAUSE)
        } else if kb.MakeCode == 0 {
            // Some drivers leave the make code out; ask the layout.
            let sc = unsafe { MapVirtualKeyW(u32::from(kb.VKey), MAPVK_VK_TO_VSC_EX) };
            keys::from_scan_code((sc & 0xff) as u16, sc & 0xff00 == 0xe000)
        } else {
            keys::from_scan_code(kb.MakeCode, flags & RI_KEY_E0 != 0)
        };
        return code.map(|c| vec![(c, pressed)]).unwrap_or_default();
    }
    if input.header.dwType == RIM_TYPEMOUSE.0 {
        let flags = u32::from(unsafe { input.data.mouse.Anonymous.Anonymous.usButtonFlags });
        return [
            (
                RI_MOUSE_LEFT_BUTTON_DOWN,
                RI_MOUSE_LEFT_BUTTON_UP,
                keys::BTN_LEFT,
            ),
            (
                RI_MOUSE_RIGHT_BUTTON_DOWN,
                RI_MOUSE_RIGHT_BUTTON_UP,
                keys::BTN_RIGHT,
            ),
            (
                RI_MOUSE_MIDDLE_BUTTON_DOWN,
                RI_MOUSE_MIDDLE_BUTTON_UP,
                keys::BTN_MIDDLE,
            ),
            (RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, keys::BTN_SIDE),
            (
                RI_MOUSE_BUTTON_5_DOWN,
                RI_MOUSE_BUTTON_5_UP,
                keys::BTN_EXTRA,
            ),
        ]
        .into_iter()
        .flat_map(|(down, up, code)| {
            [
                (flags & down != 0).then_some((code, true)),
                (flags & up != 0).then_some((code, false)),
            ]
        })
        .flatten()
        .collect();
    }
    Vec::new()
}

/// Whether the reader's hub wants the mice (on the reader thread).
fn wants_mice() -> bool {
    STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.hub.wants_mice()))
}

/// Releases every key held (after the keys were someone else's).
fn release_all() {
    let released = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut()?;
        let held: Vec<u16> = s.held.drain().collect();
        Some((s.hub.clone(), held))
    });
    if let Some((hub, held)) = released {
        for code in held {
            hub.emit(code, false);
        }
    }
}

/// Passes a transition on, without the auto-repeat a held key sends.
fn deliver(code: u16, pressed: bool) {
    let hub = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut()?;
        let changed = if pressed {
            s.held.insert(code)
        } else {
            s.held.remove(&code)
        };
        changed.then(|| s.hub.clone())
    });
    if let Some(hub) = hub {
        hub.emit(code, pressed);
    }
}

/// What [`capture_next_key`] says when there is no reader.
const NO_READER: &str = "ICU kann Tastatur und Maus nicht lesen.";

/// Waits for the next key, combination or button and returns its binding —
/// a key name (`super::keys::key_name`) or a trigger such as `CTRL+F12`
/// (`super::chord::Recorder`) — for the settings' „Taste drücken“. Left
/// and right click operate the window the capture runs in and do not
/// count.
pub async fn capture_next_key(timeout: Duration) -> Result<String, String> {
    let hub = hub().map_err(|e| {
        tracing::warn!(%e, "raw input");
        NO_READER.to_string()
    })?;
    let _mice = Mice::new(hub.clone());
    let (tx, rx) = oneshot::channel();
    let mut tx = Some(tx);
    let mut recorder = super::chord::Recorder::default();
    let id = hub.add(Box::new(move |code, pressed| {
        let Some(binding) = recorder.feed(code, pressed) else {
            return true;
        };
        if let Some(tx) = tx.take() {
            let _ = tx.send(binding);
        }
        false
    }));
    let result = tokio::time::timeout(timeout, rx).await;
    hub.remove(id);
    match result {
        Ok(Ok(binding)) => Ok(binding),
        Ok(Err(_)) => Err(NO_READER.into()),
        Err(_) => Err("Keine Taste gedrückt.".into()),
    }
}

/// Stops the backend when dropped: its listener goes, and with it the
/// `Dispatch`, which releases a held push-to-talk key; and the mice, if a
/// mouse button was bound.
pub struct Guard {
    hub: Arc<Hub>,
    id: u64,
    _mice: Option<Mice>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.hub.remove(self.id);
    }
}

/// Starts reading the bound keys. Must be called from within a tokio
/// runtime.
pub async fn start(
    settings: &HotkeySettings,
    cmd: UnboundedSender<Command>,
) -> Result<Guard, String> {
    let mut bindings: Vec<(super::chord::Chord, Action, bool)> = super::bindings(settings)
        .into_iter()
        .filter_map(|(b, a)| super::chord::Chord::parse(b).map(|k| (k, a, false)))
        .collect();
    if bindings.is_empty() {
        return Err("no key bound".into());
    }
    let hub = tokio::task::spawn_blocking(hub)
        .await
        .map_err(|e| e.to_string())??;
    let mice = bindings
        .iter()
        .any(|(chord, _, _)| chord.uses_mouse())
        .then(|| Mice::new(hub.clone()));
    let mut dispatch = Dispatch::new(cmd);
    let mut held = HashSet::new();
    let id = hub.add(Box::new(move |code, pressed| {
        if pressed {
            held.insert(code);
        } else {
            held.remove(&code);
        }
        for (chord, action, active) in &mut bindings {
            let now = chord.held(&held);
            if now != *active {
                *active = now;
                dispatch.key(*action, now);
            }
        }
        true
    }));
    Ok(Guard {
        hub,
        id,
        _mice: mice,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In the VM, inject F12 and Pause through its virtual keyboard during
    /// this test (not SendInput, which is not a physical Raw Input device).
    #[test]
    #[ignore = "needs an interactive desktop and F12/Pause keyboard input"]
    fn live_raw_input_releases_keys() {
        let hub = hub().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let events = seen.clone();
        let id = hub.add(Box::new(move |code, pressed| {
            if code == 88 || code == keys::KEY_PAUSE {
                events.lock().unwrap().push((code, pressed));
            }
            true
        }));
        std::thread::sleep(Duration::from_secs(8));
        hub.remove(id);
        let events = seen.lock().unwrap();
        eprintln!("F12/Pause transitions: {events:?}");
        for key in [88, keys::KEY_PAUSE] {
            assert!(events.contains(&(key, true)), "key {key} was not pressed");
            assert!(events.contains(&(key, false)), "key {key} remained held");
        }
    }
}
