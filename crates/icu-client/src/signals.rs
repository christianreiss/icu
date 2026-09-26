//! Ending on SIGTERM, SIGINT and SIGHUP — on Windows on logging off,
//! shutting down and the console's Ctrl+C/close — the way „Beenden“ does. A desktop
//! that logs out terminates a client sitting in the tray with SIGTERM;
//! killed without a close, it stayed online for everyone else until the
//! server's idle timeout (60 s) ran out, and a setting changed just before
//! was lost.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::time::Duration;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static WAKER: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

/// How long a requested quit may take before the process ends anyway: the
/// engine waits up to 3 s for its network task, the rest is headroom.
const GRACE: Duration = Duration::from_secs(5);

/// Starts listening, once per process. The first signal asks the running
/// window or headless loop to quit ([`requested`], and the waker); a second
/// one, or a quit that takes longer than [`GRACE`], ends the process.
pub fn listen() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("icu-signal".into())
            .spawn(|| {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                rt.block_on(wait_and_quit());
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "no signal thread: SIGTERM ends ICU without a goodbye");
        }
    });
}

/// A signal asked ICU to quit.
pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

/// Called once a signal arrives: a window hidden to the tray runs its logic
/// only when woken (`egui::Context::request_repaint`).
pub fn set_waker(wake: impl Fn() + Send + 'static) {
    *crate::sync::lock(&WAKER) = Some(Box::new(wake));
}

fn request() {
    REQUESTED.store(true, Ordering::SeqCst);
    if let Some(wake) = crate::sync::lock(&WAKER).as_ref() {
        wake();
    }
}

#[cfg(unix)]
async fn wait_and_quit() {
    use tokio::signal::unix::{signal, SignalKind};
    let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
        signal(SignalKind::hangup()),
    ) else {
        tracing::warn!("cannot listen for signals");
        return;
    };
    // The usual exit codes: 128 + signal number.
    let code = tokio::select! {
        _ = term.recv() => 143,
        _ = int.recv() => 130,
        _ = hup.recv() => 129,
    };
    tracing::info!(code, "signal received, quitting");
    request();
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
        _ = hup.recv() => {}
        _ = tokio::time::sleep(GRACE) => tracing::warn!("quitting took too long"),
    }
    std::process::exit(code);
}

#[cfg(windows)]
async fn wait_and_quit() {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};
    end_session::watch();
    // Console events reach ICU only when it runs attached to a console
    // (started from a terminal); logging off reaches the window instead
    // (`end_session`).
    let (Ok(mut c), Ok(mut brk), Ok(mut close)) = (ctrl_c(), ctrl_break(), ctrl_close()) else {
        tracing::warn!("cannot listen for console events");
        return;
    };
    tokio::select! {
        _ = c.recv() => {}
        _ = brk.recv() => {}
        _ = close.recv() => {}
    }
    tracing::info!("console event received, quitting");
    request();
    tokio::select! {
        _ = c.recv() => {}
        _ = brk.recv() => {}
        _ = tokio::time::sleep(GRACE) => tracing::warn!("quitting took too long"),
    }
    std::process::exit(130);
}

/// Logging off or shutting down: Windows asks every top-level window with
/// `WM_QUERYENDSESSION` and then ends the session with `WM_ENDSESSION`;
/// once that returns, the process may be killed at any moment. A hidden
/// window of its own (message-only windows get no broadcasts) asks for the
/// quit there and holds `WM_ENDSESSION` until ICU has said goodbye — at
/// most [`GRACE`].
#[cfg(windows)]
mod end_session {
    use std::time::Instant;

    use windows::{
        core::w,
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, WPARAM},
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassExW,
                MSG, WINDOW_STYLE, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSEXW,
                WS_EX_TOOLWINDOW,
            },
        },
    };

    pub(super) fn watch() {
        let spawned = std::thread::Builder::new()
            .name("icu-endsession".into())
            .spawn(|| unsafe {
                let Ok(instance) = GetModuleHandleW(None) else {
                    return;
                };
                let class = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(wndproc),
                    hInstance: instance.into(),
                    lpszClassName: w!("icu-endsession"),
                    ..Default::default()
                };
                RegisterClassExW(&class);
                let created = CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    w!("icu-endsession"),
                    w!("ICU"),
                    WINDOW_STYLE(0),
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                );
                if let Err(e) = created {
                    tracing::warn!(%e, "no end-session window: logging off ends ICU without a goodbye");
                    return;
                }
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "no end-session thread");
        }
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    tracing::info!("session ends, quitting");
                    super::request();
                    // The window thread quits the process once the engine is
                    // closed; until then, keep Windows waiting.
                    let deadline = Instant::now() + super::GRACE;
                    while Instant::now() < deadline {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    std::process::exit(0);
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn wait_and_quit() {
    if tokio::signal::ctrl_c().await.is_err() {
        return;
    }
    request();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = tokio::time::sleep(GRACE) => {}
    }
    std::process::exit(130);
}
