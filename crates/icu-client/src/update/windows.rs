//! Windows-only update broker. The download is authenticated before launch and
//! pinned against writes/deletion throughout installation. The old GUI exits
//! only after staging (and, where needed, UAC) succeeded. A separate unelevated
//! broker starts the new GUI; the file worker may be elevated, never the client.
mod archive;
mod progress;
mod system;
use super::{
    download,
    manifest::{self, Installation, Release},
    transaction, transport,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufRead, Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc, Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use system::{Candidate, Event, Handle};

#[derive(Clone, Serialize, Deserialize)]
struct Job {
    schema: u32,
    nonce: String,
    parent: u32,
    created: u64,
    exe: PathBuf,
    config: PathBuf,
    args: Vec<OsString>,
    installation: Installation,
    version: String,
}
struct Loaded {
    job: Job,
    dir: PathBuf,
    release: Release,
    artifact: manifest::Artifact,
    parent: Handle,
    _lease: Handle,
    _job_pin: File,
    _manifest_pin: File,
    _artifact_pin: File,
}
struct Events {
    staged: Event,
    parent_exited: Event,
    replaced: Event,
    started: Event,
    healthy: Event,
    rollback: Event,
    failed: Event,
}
impl Events {
    fn make(nonce: &str, create: bool) -> Result<Self, String> {
        let event = |kind| {
            if create {
                Event::create(nonce, kind)
            } else {
                Event::open(nonce, kind)
            }
        };
        Ok(Self {
            staged: event("staged")?,
            parent_exited: event("parent-exited")?,
            replaced: event("replaced")?,
            started: event("started")?,
            healthy: event("healthy")?,
            rollback: event("rollback")?,
            failed: event("failed")?,
        })
    }
}
fn pin(path: &Path) -> Result<File, String> {
    transaction::reject_link(path)?;
    OpenOptions::new()
        .read(true)
        .share_mode(windows::Win32::Storage::FileSystem::FILE_SHARE_READ.0)
        .open(path)
        .map_err(|e| format!("Update-Datei konnte nicht gesperrt werden: {e}"))
}
fn bounded(file: &mut File, max: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err("Update-Auftrag ist zu groß.".into());
    }
    Ok(bytes)
}
fn kind(exe: &Path) -> Result<Installation, String> {
    let Some(registered) = crate::winreg::update_install_location()? else {
        return Ok(Installation::Portable);
    };
    // A registration for another existing ICU does not turn a separate portable
    // directory into an installed client. An unreadable/stale registration is
    // an error, never an excuse to update Program Files as a portable copy.
    let installed = registered
        .canonicalize()
        .map_err(|e| format!("Installationspfad ist nicht zugänglich: {e}"))?;
    let actual = exe.parent().ok_or("ICU hat keinen Installationspfad.")?;
    Ok(
        if actual
            .to_string_lossy()
            .eq_ignore_ascii_case(&installed.to_string_lossy())
        {
            Installation::Nsis
        } else {
            Installation::Portable
        },
    )
}
pub(super) fn select(release: &Release) -> Result<(), String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| e.to_string())?;
    release.windows_artifact(std::env::consts::ARCH, kind(&exe)?)?;
    Ok(())
}
fn cache() -> Result<PathBuf, String> {
    let dir = directories::BaseDirs::new()
        .ok_or("Windows-Benutzerverzeichnis fehlt.")?
        .data_local_dir()
        .join("icu")
        .join("updates");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    dir.canonicalize().map_err(|e| e.to_string())
}
/// Only updater-owned completed folders. A still mapped helper cannot be
/// removed on Windows; the next check/start retries that harmless cleanup.
pub(super) fn cleanup() {
    let Ok(dir) = cache() else { return };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(s) = name.to_str() else { continue };
        if !s.strip_prefix("job-").is_some_and(system::valid_nonce) {
            continue;
        }
        let path = entry.path();
        if transaction::reject_link(&path).is_ok() && path.join("finished").is_file() {
            let _ = fs::remove_dir_all(path);
        }
    }
}
fn restart_args(config: &Path) -> Vec<OsString> {
    let mut result = vec!["--config-dir".into(), config.as_os_str().to_owned()];
    let mut original = std::env::args_os().skip(1);
    while let Some(arg) = original.next() {
        match arg.to_str() {
            Some(
                "--no-hotkeys" | "--no-uri-handler" | "--fake-audio" | "--test-tone"
                | "--debug-stats",
            ) => result.push(arg),
            Some("--server-addr" | "--camera") => {
                if let Some(value) = original.next() {
                    result.push(arg);
                    result.push(value)
                }
            }
            Some(value)
                if value.starts_with("--server-addr=") || value.starts_with("--camera=") =>
            {
                result.push(arg);
            }
            _ => {}
        }
    }
    result
}
pub(super) fn install(
    release: &Release,
    config: &Path,
    progress: impl Fn(u64, u64),
    preparing: impl Fn(),
) -> Result<(), String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| e.to_string())?;
    if !exe.file_name().is_some_and(|s| {
        s.to_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("icu.exe"))
    }) {
        return Err("Die ausführbare ICU-Datei muss icu.exe heißen.".into());
    }
    let root = exe.parent().ok_or("ICU-Pfad fehlt.")?;
    let _lease = system::acquire(root)?;
    let installation = kind(&exe)?;
    let artifact = release.windows_artifact(std::env::consts::ARCH, installation)?;
    let nonce = system::nonce();
    let dir = cache()?.join(format!("job-{nonce}"));
    system::private_directory(&dir)?;
    let result = (|| {
        let url = transport::artifact_url(release, artifact)?;
        let mut response = transport::agent(Duration::from_secs(15 * 60))
            .get(&url)
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|e| format!("Update-Download fehlgeschlagen: {e}"))?;
        if response.status() != 200 {
            return Err("Der Update-Server lieferte keine vollständige Datei.".into());
        }
        download::verified_stream(
            response.body_mut().as_reader(),
            &dir.join(&artifact.name),
            artifact,
            progress,
        )?;
        preparing();
        let config = config.canonicalize().map_err(|e| e.to_string())?;
        let job = Job {
            schema: 1,
            nonce,
            parent: std::process::id(),
            created: system::creation()?,
            exe: exe.clone(),
            args: restart_args(&config),
            config,
            installation,
            version: release.version.to_string(),
        };
        fs::write(dir.join("manifest.json"), release.envelope.as_slice())
            .map_err(|e| e.to_string())?;
        fs::write(
            dir.join("job.json"),
            serde_json::to_vec(&job).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let helper = dir.join("icu-update.exe");
        fs::copy(&exe, &helper).map_err(|e| e.to_string())?;
        if transaction::digest(&helper)? != transaction::digest(&exe)? {
            return Err("Der Update-Prozess konnte nicht sicher kopiert werden.".into());
        }
        let mut child = Command::new(helper)
            .arg("--icu-update-helper")
            .arg(dir.join("job.json"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Update-Prozess konnte nicht starten: {e}"))?;
        let output = child
            .stdout
            .take()
            .ok_or("Update-Prozess hat keinen Rückkanal.")?;
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut line = String::new();
            let read = std::io::BufReader::new(output)
                .take(8192)
                .read_line(&mut line);
            let _ = tx.send(read.map(|_| line));
        });
        let response = rx
            .recv_timeout(Duration::from_secs(240))
            .map_err(|_| "Der Update-Prozess antwortet nicht. ICU bleibt geöffnet.")?
            .map_err(|e| e.to_string())?;
        match serde_json::from_str::<Reply>(&response)
            .map_err(|_| "Der Update-Prozess hat keine gültige Antwort geliefert.")?
        {
            Reply::Ready => Ok(()),
            Reply::Error(error) => Err(error),
        }
    })();
    if result.is_err() {
        let _ = fs::write(dir.join("finished"), b"failed before handoff\n");
    }
    result
}
#[derive(Serialize, Deserialize)]
enum Reply {
    Ready,
    Error(String),
}
fn reply(value: Reply) {
    if let Ok(text) = serde_json::to_string(&value) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
}
fn load(path: &Path) -> Result<Loaded, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    let dir = path.parent().ok_or("Update-Verzeichnis fehlt.")?.to_owned();
    transaction::reject_link(&dir)?;
    let mut job_pin = pin(&path)?;
    let job: Job = serde_json::from_slice(&bounded(&mut job_pin, 32 * 1024)?)
        .map_err(|_| "Ungültiger Update-Auftrag.")?;
    if job.schema != 1
        || !system::valid_nonce(&job.nonce)
        || dir.file_name() != Some(OsString::from(format!("job-{}", job.nonce)).as_os_str())
        || job.args.len() > 24
        || job.exe != job.exe.canonicalize().map_err(|e| e.to_string())?
    {
        return Err("Ungültiger Update-Auftrag oder Installationspfad.".into());
    }
    let root = job.exe.parent().ok_or("ICU-Pfad fehlt.")?;
    let lease = system::join(root)?;
    let parent = system::parent(job.parent, job.created, &job.exe)?;
    let helper = std::env::current_exe().map_err(|e| e.to_string())?;
    if transaction::digest(&helper)? != transaction::digest(&job.exe)? {
        return Err("Update-Prozess und ursprünglicher Client stimmen nicht überein.".into());
    }
    let mut manifest_pin = pin(&dir.join("manifest.json"))?;
    let release = manifest::verify(&bounded(&mut manifest_pin, manifest::MAX_MANIFEST)?)?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    if release.version.to_string() != job.version
        || !manifest::newer_stable(&release.version, &current)
        || kind(&job.exe)? != job.installation
    {
        return Err("Das Update passt nicht zum ursprünglichen Client.".into());
    }
    let artifact = release
        .windows_artifact(std::env::consts::ARCH, job.installation)?
        .clone();
    let mut artifact_pin = pin(&dir.join(&artifact.name))?;
    download::recheck(&mut artifact_pin, &artifact)?;
    Ok(Loaded {
        job,
        dir,
        release,
        artifact,
        parent,
        _lease: lease,
        _job_pin: job_pin,
        _manifest_pin: manifest_pin,
        _artifact_pin: artifact_pin,
    })
}
fn writable(root: &Path) -> Result<bool, String> {
    let probe = root.join(format!(".icu-write-{}", system::nonce()));
    match OpenOptions::new().create_new(true).write(true).open(&probe) {
        Ok(file) => {
            drop(file);
            fs::remove_file(probe).map_err(|e| e.to_string())?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(false),
        Err(e) => Err(format!("Installationsordner ist nicht beschreibbar: {e}")),
    }
}
fn wait_event(event: &Event, events: &Events, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while !event.ready() {
        if events.rollback.ready() || Instant::now() >= deadline {
            return Err(
                "Update abgebrochen oder Zeitüberschreitung; die Sicherung bleibt erhalten.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
/// File worker, optionally elevated. It never launches a client as administrator.
fn apply(path: &Path) -> Result<(), String> {
    let loaded = load(path)?;
    let events = Events::make(&loaded.job.nonce, false)?;
    let root = loaded.job.exe.parent().ok_or("ICU-Pfad fehlt.")?;
    let payload = loaded.dir.join("payload");
    fs::create_dir(&payload).map_err(|e| e.to_string())?;
    archive::extract(
        pin(&loaded.dir.join(&loaded.artifact.name))?,
        &loaded.release.version,
        &payload,
    )?;
    let mut transaction = if transaction::pending(root) {
        None
    } else {
        Some(transaction::Transaction::prepare(root, &payload)?)
    };
    events.staged.set()?;
    wait_event(&events.parent_exited, &events, Duration::from_secs(45))?;
    // Recheck the retained process handle, rather than trusting only an event.
    loaded.parent.wait(0)?;
    let outcome: Result<(), String> = (|| {
        if transaction.is_none() {
            transaction::recover(root)?;
            transaction = Some(transaction::Transaction::prepare(root, &payload)?);
        }
        transaction
            .as_ref()
            .ok_or("Update-Vorbereitung fehlt.")?
            .commit()?;
        events.replaced.set()?;
        wait_event(&events.healthy, &events, Duration::from_secs(60))?;
        transaction
            .take()
            .ok_or("Update-Vorbereitung fehlt.")?
            .finish()
    })();
    if let Err(error) = outcome {
        let _ = events.failed.set();
        // The broker first stops only its new candidate, then permits rollback.
        if events.replaced.ready() {
            let _ = wait_event(&events.rollback, &events, Duration::from_secs(15));
        }
        let recovery = transaction::recover(root);
        return Err(match recovery {
            Ok(_) => format!("{error} Die bisherige Installation wurde wiederhergestellt."),
            Err(recovery) => format!("{error} Wiederherstellung benötigt Hilfe: {recovery}"),
        });
    }
    Ok(())
}
enum Worker {
    Process(Handle),
    Thread {
        rx: mpsc::Receiver<Result<(), String>>,
        outcome: Option<Result<(), String>>,
    },
}
impl Worker {
    fn poll(&mut self) -> Option<Result<(), String>> {
        match self {
            Self::Process(process) if process.exited() => {
                Some(process.exit_code().and_then(|code| {
                    if code == 0 {
                        Ok(())
                    } else {
                        Err(format!("Das Installationsprogramm meldet Fehler {code}."))
                    }
                }))
            }
            Self::Process(_) => None,
            Self::Thread { rx, outcome } => {
                if outcome.is_none() {
                    *outcome = match rx.try_recv() {
                        Ok(value) => Some(value),
                        Err(mpsc::TryRecvError::Empty) => None,
                        Err(_) => Some(Err("Der Update-Prozess wurde unerwartet beendet.".into())),
                    };
                }
                outcome.clone()
            }
        }
    }
    fn wait(&mut self, seconds: u64) -> Result<(), String> {
        let end = Instant::now() + Duration::from_secs(seconds);
        loop {
            if let Some(result) = self.poll() {
                return result;
            }
            if Instant::now() > end {
                return Err(
                    "Das Installationsprogramm antwortet nicht; die Sicherung bleibt erhalten."
                        .into(),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
fn milestone(
    event: &Event,
    events: &Events,
    worker: &mut Worker,
    seconds: u64,
) -> Result<(), String> {
    let end = Instant::now() + Duration::from_secs(seconds);
    while !event.ready() {
        if events.failed.ready() {
            return Err("Der Dateiaustausch ist fehlgeschlagen.".into());
        }
        if let Some(result) = worker.poll() {
            return result.and(Err("Das Installationsprogramm endete vorzeitig.".into()));
        }
        if Instant::now() > end {
            return Err("Zeitüberschreitung beim Installationsprogramm.".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
fn launch_args(job: &Job) -> Vec<OsString> {
    let mut args = job.args.clone();
    args.extend(["--update-ready".into(), job.nonce.clone().into()]);
    args
}
fn broker(path: &Path, status: &progress::Status) -> Result<(), String> {
    let loaded = load(path)?;
    let events = Events::make(&loaded.job.nonce, true)?;
    status.set("Update vorbereiten …");
    let mut worker = if loaded.job.installation == Installation::Nsis {
        Worker::Process(system::elevate(
            &loaded.dir.join(&loaded.artifact.name),
            &[
                "/S".into(),
                format!("/ICUUpdate={}", loaded.job.nonce).into(),
            ],
        )?)
    } else if writable(loaded.job.exe.parent().ok_or("ICU-Pfad fehlt.")?)? {
        let path = path.to_owned();
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = tx.send(apply(&path));
        });
        Worker::Thread { rx, outcome: None }
    } else {
        status.set("Windows-Freigabe für den Installationsordner …");
        Worker::Process(system::elevate(
            &std::env::current_exe().map_err(|e| e.to_string())?,
            &["--icu-update-apply".into(), path.as_os_str().to_owned()],
        )?)
    };
    if let Err(error) = milestone(&events.staged, &events, &mut worker, 90) {
        let _ = events.rollback.set();
        let _ = worker.wait(20);
        return Err(error);
    }
    status.set("ICU wird für den bestätigten Neustart geschlossen …");
    reply(Reply::Ready);
    status.handed_off();
    let mut candidate: Option<Candidate> = None;
    let outcome: Result<(), String> = (|| {
        loaded.parent.wait(30_000)?;
        events.parent_exited.set()?;
        status.set("Programmdateien austauschen …");
        milestone(&events.replaced, &events, &mut worker, 90)?;
        status.set("Neue Version starten und prüfen …");
        candidate = Some(Candidate::start(
            &loaded.job.exe,
            &launch_args(&loaded.job),
        )?);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !events.started.ready() {
            if candidate.as_ref().is_none_or(Candidate::exited) || events.failed.ready() {
                return Err("Die neue Version konnte nicht starten.".into());
            }
            if Instant::now() > deadline {
                return Err("Die neue Version hat ihren Start nicht bestätigt.".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // First frames must stay alive before the worker discards the backup.
        let grace = Instant::now() + Duration::from_secs(2);
        while Instant::now() < grace {
            if candidate.as_ref().is_none_or(Candidate::exited) {
                return Err("Die neue Version wurde beim Start beendet.".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        events.healthy.set()?;
        worker.wait(30)?;
        candidate.take().ok_or("Neuer ICU-Prozess fehlt.")?.keep()?;
        Ok(())
    })();
    if let Err(error) = outcome {
        status.set("Bisherige Installation wiederherstellen …");
        if let Some(child) = candidate.take() {
            child.stop()
        }
        let _ = events.rollback.set();
        let recovery = worker.wait(30);
        let restart = if loaded.parent.exited() && worker.poll().is_some() {
            restart_previous(loaded, events)
        } else {
            Ok(())
        };
        let mut message = match recovery {
            Ok(()) => format!("{error} Die bisherige Installation wurde wiederhergestellt."),
            Err(detail) => format!("{error} {detail}"),
        };
        if let Err(detail) = restart {
            message.push_str(&format!(" Automatischer Neustart nicht möglich: {detail}"));
        }
        return Err(message);
    }
    Ok(())
}
fn restart_previous(loaded: Loaded, events: Events) -> Result<(), String> {
    // Never relaunch a partially installed candidate as the restored version.
    let helper = std::env::current_exe().map_err(|e| e.to_string())?;
    if transaction::digest(&loaded.job.exe)? != transaction::digest(&helper)? {
        return Err("Die bisherige EXE wurde noch nicht vollständig wiederhergestellt.".into());
    }
    let exe = loaded.job.exe.clone();
    let args = loaded.job.args.clone();
    // This is a normal start, after the finished worker and broker have released
    // the installation lease. --update-ready would race the soon-closed events
    // and join the failed transaction instead of acquiring a new instance.
    drop(events);
    drop(loaded);
    Command::new(exe)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
/// Called before Clap, profile loading, single-instance IPC or the audio engine.
pub fn special_entry() -> Option<Result<(), String>> {
    let args: Vec<_> = std::env::args_os().collect();
    let apply_mode = args.get(1).is_some_and(|a| a == "--icu-update-apply");
    if !apply_mode && !args.get(1).is_some_and(|a| a == "--icu-update-helper") {
        return None;
    }
    if args.len() != 3 {
        return Some(Err("Ungültiger Update-Prozessaufruf.".into()));
    }
    let path = PathBuf::from(&args[2]);
    if apply_mode {
        return Some(apply(&path));
    }
    Some(progress::run(path))
}
static START_EVENT: OnceLock<Mutex<Option<(Event, u8)>>> = OnceLock::new();
pub fn startup(nonce: Option<&str>) -> Result<(), String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| e.to_string())?;
    if let Some(nonce) = nonce {
        let event = Event::open(nonce, "started")?;
        let _ = START_EVENT.set(Mutex::new(Some((event, 0))));
    } else if system::blocked(exe.parent().ok_or("ICU-Pfad fehlt.")?)? {
        return Err(
            "ICU wird gerade aktualisiert. Bitte den Abschluss des Updates abwarten.".into(),
        );
    }
    Ok(())
}
pub fn restarting() -> bool {
    START_EVENT.get().is_some()
}
#[cfg(feature = "gui")]
pub fn healthy_frame(ctx: &egui::Context) {
    let Some(slot) = START_EVENT.get() else {
        return;
    };
    let mut slot = slot.lock().unwrap();
    if let Some((event, frames)) = slot.as_mut() {
        *frames += 1;
        if *frames >= 3 {
            let _ = event.set();
            *slot = None
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broker_and_worker_share_all_events_but_only_the_gui_acknowledges_start() {
        let nonce = system::nonce();
        let broker = Events::make(&nonce, true).unwrap();
        let worker = Events::make(&nonce, false).unwrap();
        assert!(!broker.started.ready());
        worker.staged.set().unwrap();
        assert!(broker.staged.ready());
        assert!(!broker.healthy.ready());
        Event::open(&nonce, "started").unwrap().set().unwrap();
        assert!(broker.started.ready());
        assert!(!worker.healthy.ready());
        broker.healthy.set().unwrap();
        assert!(worker.healthy.ready());
        assert!(Events::make(&nonce, true).is_err());
    }
}
