//! The guardian owns native volume interfaces. EOF/heartbeat loss restores them
//! even after the GUI process is killed. All OS and file operations stay here.
use super::{Envelope, Input, Status};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use super::linux::Native;
#[cfg(target_os = "windows")]
use super::windows::Native;

pub struct Service {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Service {
    pub fn start(
        input: impl Fn() -> Input + Send + 'static,
        status: impl Fn(Status) + Send + Sync + 'static,
    ) -> Result<Self, std::io::Error> {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let status = Arc::new(status);
        let thread = std::thread::Builder::new()
            .name("icu-ducking-control".into())
            .spawn(move || {
                let run = || -> Result<(), String> {
                    let mut command =
                        Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
                    command
                        .arg("--ducking-worker")
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null());
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        command.creation_flags(0x08000000);
                    }
                    let mut child = command.spawn().map_err(|e| e.to_string())?;
                    let mut pipe = child.stdin.take().ok_or("Ducking-Steuerkanal fehlt")?;
                    let output = child.stdout.take().ok_or("Ducking-Statuskanal fehlt")?;
                    let report = status.clone();
                    let reader = std::thread::spawn(move || {
                        let mut reader = BufReader::new(output);
                        while let Some(line) = bounded_line(&mut reader) {
                            if let Ok(value) = serde_json::from_slice(&line) {
                                report(value);
                            }
                        }
                    });
                    while !done.load(Ordering::Acquire) {
                        let line = serde_json::to_vec(&input()).map_err(|e| e.to_string())?;
                        if pipe
                            .write_all(&line)
                            .and_then(|_| pipe.write_all(b"\n"))
                            .is_err()
                        {
                            break;
                        }
                        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(40));
                    }
                    // Closing the pipe, rather than killing the helper, requests restore.
                    drop(pipe);
                    let until = Instant::now() + Duration::from_secs(3);
                    while Instant::now() < until && child.try_wait().ok().flatten().is_none() {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    // A recovery helper may outlive us while an endpoint is unavailable.
                    // Never kill it just to shorten GUI shutdown.
                    if reader.is_finished() {
                        let _ = reader.join();
                    }
                    if done.load(Ordering::Acquire) {
                        Ok(())
                    } else {
                        Err("Lautstärkewächter beendet; erneuter Verbindungsversuch".into())
                    }
                };
                while !done.load(Ordering::Acquire) {
                    if let Err(error) = run() {
                        status(Status {
                            detail: format!("Absenkung nicht verfügbar: {error}"),
                            ..Status::default()
                        });
                    }
                    for _ in 0..50 {
                        if done.load(Ordering::Acquire) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(40));
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn bounded_line(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut line = Vec::with_capacity(512);
    loop {
        let available = reader.fill_buf().ok()?;
        if available.is_empty() {
            return None;
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        if line.len() + count > 8192 {
            return None;
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if line.last() == Some(&b'\n') {
            return Some(line);
        }
    }
}

pub(crate) fn state_dir() -> Result<std::path::PathBuf, String> {
    let path = directories::ProjectDirs::from("", "", "icu")
        .ok_or("Kein lokales Datenverzeichnis")?
        .data_local_dir()
        .join("ducking");
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(path)
}

/// Hidden same-binary entrypoint, deliberately before GUI/instance setup.
pub fn run_worker() -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options
        .open(state_dir()?.join("owner.lock"))
        .map_err(|e| e.to_string())?;
    if lock.try_lock().is_err() {
        let s = Status {
            detail: "Eine andere ICU-Instanz steuert die Absenkung".into(),
            ..Status::default()
        };
        println!("{}", serde_json::to_string(&s).unwrap());
        return Ok(());
    }
    let latest = Arc::new(Mutex::new(None::<(Input, Instant)>));
    let alive = Arc::new(AtomicBool::new(true));
    let inbox = latest.clone();
    let connected = alive.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        while let Some(line) = bounded_line(&mut reader) {
            let Ok(input) = serde_json::from_slice::<Input>(&line) else {
                break;
            };
            if input.route.as_ref().is_some_and(|r| r.len() > 2048) {
                break;
            }
            *crate::sync::lock(&inbox) = Some((input, Instant::now()));
        }
        connected.store(false, Ordering::Release);
    });
    let mut native = None;
    let mut retry_at = Instant::now();
    let mut last_error = String::new();
    let mut envelope = Envelope::new(Instant::now());
    let mut route = None;
    let mut report_at = Instant::now() - Duration::from_secs(1);
    let mut end_at = None;
    loop {
        let now = Instant::now();
        let snapshot = crate::sync::lock(&latest).clone();
        let fresh = snapshot
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) < Duration::from_millis(750));
        let mut input = snapshot.map(|(i, _)| i).unwrap_or_default();
        if !fresh || !alive.load(Ordering::Acquire) {
            input = Input::default();
        }
        if route != input.route {
            if let Some(controller) = native.as_mut() {
                Native::restore(controller);
            }
            envelope.reset(now);
            route = input.route.clone();
        }
        let mut status = Status {
            transmitting: input.transmitting_voice,
            receiving: input.speech_db.is_some(),
            route: input.route.clone(),
            ..Status::default()
        };
        if native.is_none() && now >= retry_at {
            match Native::new() {
                Ok(controller) => {
                    native = Some(controller);
                    last_error.clear();
                }
                Err(error) => {
                    last_error = error;
                    retry_at = now + Duration::from_secs(2);
                }
            }
        }
        let measured = native
            .as_mut()
            .map(|controller| controller.measure(input.route.as_deref()));
        match measured {
            Some(Ok(background)) => {
                let (db, reason) = envelope.update(&input, background, now);
                status.attenuation_db = db;
                status.detail = reason.into();
                let controller = native.as_mut().unwrap();
                status.route = controller.active_route();
                match controller.apply(db) {
                    Ok(count) => {
                        status.sessions = count;
                        if count == 0 {
                            status.attenuation_db = 0.0;
                        }
                    }
                    Err(error) => {
                        controller.restore();
                        status.attenuation_db = 0.0;
                        status.detail = error;
                    }
                }
            }
            Some(Err(error)) => {
                if let Some(mut controller) = native.take() {
                    controller.restore();
                }
                // Durable native journals retain any unresolved conditional
                // restore. Reconnect also discovers a restarted audio service.
                retry_at = now + Duration::from_secs(2);
                envelope.reset(now);
                last_error = error;
                status.detail = last_error.clone();
            }
            None => status.detail = last_error.clone(),
        }
        if now.duration_since(report_at) >= Duration::from_millis(200) {
            report_at = now;
            if let Ok(line) = serde_json::to_string(&status) {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
            }
        }
        if !alive.load(Ordering::Acquire) {
            let deadline = *end_at.get_or_insert(now + Duration::from_secs(30));
            if native.as_mut().is_some_and(Native::restore) || now >= deadline {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipc_is_bounded_and_requires_complete_lines() {
        assert_eq!(bounded_line(&mut &b"{}\n"[..]), Some(b"{}\n".to_vec()));
        assert!(bounded_line(&mut &b"{}"[..]).is_none());
        assert!(bounded_line(&mut vec![b'a'; 8193].as_slice()).is_none());
    }
}
