//! Ending on SIGTERM, SIGINT and SIGHUP the way „Beenden“ does. A desktop
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

#[cfg(not(unix))]
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
