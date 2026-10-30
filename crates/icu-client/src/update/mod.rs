//! Stable update checks are independent of the UI/audio/connection threads.
//! Only the Windows build contains artifact download or installation code.
#[cfg(any(windows, test))]
mod download;
pub mod manifest;
#[cfg(any(windows, test))]
mod transaction;
mod transport;
#[cfg(windows)]
pub mod windows;

use manifest::{newer_stable, Release};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, SystemTime},
};

pub const RELEASE_INFO: &str = "https://misc.alpha-labs.net/icu/";
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, Default)]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    #[cfg(windows)]
    Downloading {
        received: u64,
        total: u64,
    },
    #[cfg(windows)]
    Preparing,
}
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub available: Option<Arc<Release>>,
    pub phase: Phase,
    pub error: Option<String>,
    pub checked_at: Option<SystemTime>,
    pub dismissed: bool,
}
impl Snapshot {
    pub fn busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }
    pub fn status(&self) -> String {
        match self.phase {
            Phase::Checking => "Suche nach Updates …".into(),
            #[cfg(windows)]
            Phase::Downloading { received, total } => format!(
                "Update herunterladen: {:.0} %",
                received as f64 / total.max(1) as f64 * 100.
            ),
            #[cfg(windows)]
            Phase::Preparing => "Update wird vorbereitet …".into(),
            Phase::Idle => {
                if let Some(error) = &self.error {
                    error.clone()
                } else if let Some(release) = &self.available {
                    format!("Version {} ist verfügbar.", release.version)
                } else if self.checked_at.is_some() {
                    "Kein neueres stabiles Update verfügbar.".into()
                } else {
                    "Noch nicht nach Updates gesucht.".into()
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Check,
    Dismiss,
    #[cfg(windows)]
    Install,
}

enum Work {
    Check,
    #[cfg(windows)]
    Install,
}
pub struct Updater {
    tx: mpsc::SyncSender<Work>,
    state: Arc<Mutex<Snapshot>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    canceled: Arc<AtomicBool>,
}
impl Updater {
    pub fn new(config_dir: PathBuf, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::sync_channel(1);
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = canceled.clone();
        let worker_state = state.clone();
        let worker_wake = wake.clone();
        let spawned = std::thread::Builder::new()
            .name("icu-update".into())
            .spawn(move || {
                #[cfg(not(windows))]
                let _ = config_dir;
                let current =
                    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo version");
                check(&worker_state, &worker_wake, &current, false);
                loop {
                    if worker_canceled.load(Ordering::Acquire) {
                        break;
                    }
                    match rx.recv_timeout(INTERVAL) {
                        Ok(Work::Check) => check(&worker_state, &worker_wake, &current, true),
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            check(&worker_state, &worker_wake, &current, false)
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        #[cfg(windows)]
                        Ok(Work::Install) => {
                            let release = worker_state.lock().unwrap().available.clone();
                            let Some(release) = release else {
                                continue;
                            };
                            if !newer_stable(&release.version, &current) {
                                continue;
                            }
                            let result = windows::install(
                                &release,
                                &config_dir,
                                |received, total| {
                                    worker_state.lock().unwrap().phase =
                                        Phase::Downloading { received, total };
                                    worker_wake();
                                },
                                || {
                                    worker_state.lock().unwrap().phase = Phase::Preparing;
                                    worker_wake();
                                },
                            );
                            // Success means the helper is ready and has asked this
                            // explicitly authorized update to close the old client.
                            if result.is_ok() && !worker_canceled.load(Ordering::Acquire) {
                                #[cfg(feature = "gui")]
                                crate::ui::native::dispatch(false);
                            } else {
                                let mut state = worker_state.lock().unwrap();
                                state.phase = Phase::Idle;
                                state.error = result.err();
                                drop(state);
                                worker_wake();
                            }
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error,"update worker unavailable");
            state.lock().unwrap().error = Some("Update-Prüfung momentan nicht verfügbar.".into());
        }
        Self {
            tx,
            state,
            wake,
            canceled,
        }
    }
    pub fn act(&self, action: Option<Action>) {
        match action {
            Some(Action::Check) => self.check(),
            Some(Action::Dismiss) => self.dismiss(),
            #[cfg(windows)]
            Some(Action::Install) => self.install(),
            None => {}
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().clone()
    }
    pub fn check(&self) {
        let _ = self.tx.try_send(Work::Check);
    }
    pub fn dismiss(&self) {
        self.state.lock().unwrap().dismissed = true;
        (self.wake)();
    }
    #[cfg(windows)]
    pub fn install(&self) {
        let mut state = self.state.lock().unwrap();
        if !state.busy() && state.available.is_some() {
            state.phase = Phase::Preparing;
            state.error = None;
            if self.tx.try_send(Work::Install).is_err() {
                state.phase = Phase::Idle;
            }
            drop(state);
            (self.wake)();
        }
    }
}
fn check(
    state: &Mutex<Snapshot>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    current: &semver::Version,
    manual: bool,
) {
    {
        let mut state = state.lock().unwrap();
        state.phase = Phase::Checking;
        state.error = None;
    }
    wake();
    #[cfg(windows)]
    windows::cleanup();
    let result = transport::check();
    #[cfg(windows)]
    let result = result.and_then(|release| {
        if newer_stable(&release.version, current) {
            windows::select(&release)?;
        }
        Ok(release)
    });
    let mut state = state.lock().unwrap();
    state.phase = Phase::Idle;
    match result {
        Ok(release) => {
            let available = newer_stable(&release.version, current).then_some(Arc::new(release));
            let changed = available.as_ref().map(|r| &r.version)
                != state.available.as_ref().map(|r| &r.version);
            state.available = available;
            state.checked_at = Some(SystemTime::now());
            if manual || changed {
                state.dismissed = false;
            }
        }
        Err(error) => state.error = Some(error),
    }
    drop(state);
    wake();
}

impl Drop for Updater {
    fn drop(&mut self) {
        self.canceled.store(true, Ordering::Release);
    }
}
