//! One running client per configuration directory
//! (`Client/IMPLEMENTATION.md` §10).
//!
//! Starting ICU again — the natural way back after „Schließen“ hid the
//! window to the tray, or a double-click on an `.icu` file — must not
//! connect a second client with the same certificate: the server would
//! replace the running session (`SESSION_REPLACED`). The first windowed
//! instance holds a lock and listens on a Unix socket in `$XDG_RUNTIME_DIR`
//! (only this user can reach it) — on Windows a named mutex and a named
//! pipe (`windows`) — both named after a hash of its
//! configuration directory, so `--config-dir` test instances stay apart. A
//! later start hands its request over — „show“, plus the text of the invite
//! or passport it was given — and exits. `--headless` does none of this.
//!
//! The lock, not the socket, decides who is first: the kernel releases it
//! with the process, so a crashed instance never blocks a start, and the
//! socket file it left behind is simply replaced.
//!
//! Protocol, one connection per request: the new start writes the line
//! `icu-instance/1 show`, then the invite or passport text if any, shuts
//! down its write side, and waits for `ok`.

use std::time::Duration;

use crate::config::Paths;

/// What a later start asks of the running instance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// The invite or passport text it was started with (the file's
    /// contents, not its path: the running instance may sit in another
    /// working directory).
    pub bundle: Option<String>,
}

/// The outcome of [`claim`].
pub enum Claim {
    /// This is the first instance; later starts arrive through the
    /// listener ([`install`] it for the windows).
    First(Listener),
    /// Another instance runs and took the request: exit.
    Handed,
    /// No guard here (no runtime directory, no Unix sockets, no locking):
    /// run as before, with the reason for the log.
    Unguarded(String),
}

/// First line of every request.
const HELLO: &str = "icu-instance/1 show";

/// The largest request taken: a passport is at most 8 KiB decoded, some
/// 11 KiB armored.
const MAX_REQUEST: usize = 64 * 1024;

/// How long a later start keeps trying to reach the instance that holds
/// the lock (it may be between taking the lock and listening).
const HAND_OVER: Duration = Duration::from_secs(3);

/// The socket's and the lock's name: `icu-` and 16 hex digits of the
/// SHA-256 of the canonical configuration directory.
pub fn key(paths: &Paths) -> String {
    use sha2::{Digest, Sha256};
    let dir = std::fs::canonicalize(&paths.config_dir).unwrap_or_else(|_| paths.config_dir.clone());
    let digest = Sha256::digest(dir.as_os_str().as_encoded_bytes());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("icu-{hex}")
}

/// Becomes the running instance for `paths`, or hands `request` to the one
/// that already is. `Err` when another instance holds the lock but does not
/// answer — starting a second client then would only knock the first one's
/// session off the server.
pub fn claim(paths: &Paths, request: &Request) -> Result<Claim, String> {
    #[cfg(windows)]
    return claim_in(std::path::Path::new(""), &key(paths), request, HAND_OVER);
    #[cfg(not(windows))]
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|d| d.is_absolute() && d.is_dir())
    else {
        return Ok(Claim::Unguarded("no XDG_RUNTIME_DIR".into()));
    };
    #[cfg(not(windows))]
    claim_in(&dir, &key(paths), request, HAND_OVER)
}

/// The encoded request.
fn encode(request: &Request) -> Vec<u8> {
    format!("{HELLO}\n{}", request.bundle.as_deref().unwrap_or("")).into_bytes()
}

/// Reads a request; `None` for anything else.
fn parse(data: &[u8]) -> Option<Request> {
    let text = std::str::from_utf8(data).ok()?;
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    if first.trim_end() != HELLO {
        return None;
    }
    Some(Request {
        bundle: (!rest.trim().is_empty()).then(|| rest.to_owned()),
    })
}

#[cfg(unix)]
pub use unix::{claim_in, Listener};

#[cfg(unix)]
mod unix {
    use std::{
        fs::{File, OpenOptions, TryLockError},
        io::{Read, Write},
        net::Shutdown,
        os::unix::net::{UnixListener, UnixStream},
        path::{Path, PathBuf},
        sync::{mpsc, Arc, Mutex},
        time::{Duration, Instant},
    };

    use super::{encode, parse, Claim, Request, MAX_REQUEST};

    /// What to call when a request arrived.
    type Waker = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;

    /// The running instance's end: requests from later starts.
    pub struct Listener {
        requests: Mutex<mpsc::Receiver<Request>>,
        waker: Waker,
        socket: PathBuf,
        /// Held for as long as this instance runs.
        _lock: File,
    }

    impl Listener {
        /// The next request, if one arrived.
        pub fn try_recv(&self) -> Option<Request> {
            self.requests.lock().unwrap().try_recv().ok()
        }

        /// Called after every request: a window hidden to the tray runs its
        /// logic only when woken (`egui::Context::request_repaint`).
        pub fn set_waker(&self, wake: impl Fn() + Send + 'static) {
            *self.waker.lock().unwrap() = Some(Box::new(wake));
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            // Still holding the lock: nobody else can have bound it since.
            let _ = std::fs::remove_file(&self.socket);
        }
    }

    /// [`super::claim`] in `dir`, for `key`, trying `hand_over` long to
    /// reach an instance that holds the lock.
    pub fn claim_in(
        dir: &Path,
        key: &str,
        request: &Request,
        hand_over: Duration,
    ) -> Result<Claim, String> {
        let socket = dir.join(format!("{key}.sock"));
        let lock = match OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{key}.lock")))
        {
            Ok(lock) => lock,
            Err(e) => return Ok(Claim::Unguarded(format!("lock file: {e}"))),
        };
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return hand(&socket, request, hand_over),
            Err(TryLockError::Error(e)) => return Ok(Claim::Unguarded(format!("lock: {e}"))),
        }
        // First. A socket file left by a crashed instance is in the way.
        let _ = std::fs::remove_file(&socket);
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(e) => return Ok(Claim::Unguarded(format!("socket: {e}"))),
        };
        let (tx, rx) = mpsc::channel();
        let waker = Waker::default();
        let wake = waker.clone();
        std::thread::Builder::new()
            .name("icu-instance".into())
            .spawn(move || {
                for conn in listener.incoming().flatten() {
                    if let Err(e) = serve(conn, &tx, &wake) {
                        tracing::debug!(%e, "instance request");
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Claim::First(Listener {
            requests: Mutex::new(rx),
            waker,
            socket,
            _lock: lock,
        }))
    }

    /// Takes one request: queued and the window woken before the answer, so
    /// a start that heard `ok` knows it arrived.
    fn serve(
        mut conn: UnixStream,
        tx: &mpsc::Sender<Request>,
        wake: &Waker,
    ) -> std::io::Result<()> {
        conn.set_read_timeout(Some(Duration::from_secs(2)))?;
        conn.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut data = Vec::new();
        (&mut conn)
            .take(MAX_REQUEST as u64 + 1)
            .read_to_end(&mut data)?;
        let request = (data.len() <= MAX_REQUEST)
            .then(|| parse(&data))
            .flatten()
            .ok_or_else(|| std::io::Error::other("not an instance request"))?;
        tracing::info!(
            bundle = request.bundle.is_some(),
            "another start handed over"
        );
        let _ = tx.send(request);
        if let Some(wake) = &*wake.lock().unwrap() {
            wake();
        }
        conn.write_all(b"ok\n")
    }

    /// Hands `request` to the instance behind `socket`, retrying while it
    /// may still be starting up.
    fn hand(socket: &Path, request: &Request, hand_over: Duration) -> Result<Claim, String> {
        let deadline = Instant::now() + hand_over;
        loop {
            match send(socket, request) {
                Ok(()) => return Ok(Claim::Handed),
                Err(e) if Instant::now() >= deadline => {
                    return Err(format!("ICU läuft bereits, antwortet aber nicht ({e})"))
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    fn send(socket: &Path, request: &Request) -> std::io::Result<()> {
        let mut conn = UnixStream::connect(socket)?;
        conn.set_read_timeout(Some(Duration::from_secs(3)))?;
        conn.set_write_timeout(Some(Duration::from_secs(3)))?;
        conn.write_all(&encode(request))?;
        conn.shutdown(Shutdown::Write)?;
        let mut answer = String::new();
        conn.take(16).read_to_string(&mut answer)?;
        if answer == "ok\n" {
            Ok(())
        } else {
            Err(std::io::Error::other("no answer"))
        }
    }
}

#[cfg(windows)]
pub use windows::{claim_in, Listener};

/// Windows: a named mutex in the session's namespace (`Local\\icu-…`)
/// decides who is first — Windows destroys it with the last handle, so a
/// crashed instance never blocks a start — and a named pipe
/// (`\\.\pipe\icu-…`, local clients only) takes the requests. Pipes have
/// no half-close: the pipe runs in message mode and a request is one
/// message.
#[cfg(windows)]
mod windows {
    use super::{encode, parse, Claim, Request, MAX_REQUEST};
    use ::windows::{
        core::HSTRING,
        Win32::{
            Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
            System::Threading::CreateMutexW,
        },
    };
    use std::{
        io,
        os::windows::io::{FromRawHandle, OwnedHandle},
        path::Path,
        sync::{mpsc, Arc, Mutex},
        time::{Duration, Instant},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::windows::named_pipe::{ClientOptions, NamedPipeServer, PipeMode, ServerOptions},
        sync::{oneshot, Semaphore},
    };

    type Waker = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
    const MAX_CLIENTS: usize = 8;

    pub struct Listener {
        requests: Mutex<mpsc::Receiver<Request>>,
        waker: Waker,
        // Dropping the listener cancels the accept loop and all pending I/O.
        _stop: oneshot::Sender<()>,
        _mutex: OwnedHandle,
    }

    impl Listener {
        pub fn try_recv(&self) -> Option<Request> {
            self.requests.lock().unwrap().try_recv().ok()
        }
        pub fn set_waker(&self, wake: impl Fn() + Send + 'static) {
            *self.waker.lock().unwrap() = Some(Box::new(wake));
        }
    }

    fn pipe_name(key: &str) -> String {
        format!(r"\\.\pipe\{key}")
    }

    pub fn claim_in(
        _dir: &Path,
        key: &str,
        request: &Request,
        hand_over: Duration,
    ) -> Result<Claim, String> {
        let name = HSTRING::from(format!(r"Local\{key}"));
        let handle =
            unsafe { CreateMutexW(None, false, &name) }.map_err(|e| format!("mutex: {e}"))?;
        let existing = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mutex = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        if existing {
            return hand(&pipe_name(key), request, hand_over);
        }
        let pipe = pipe_name(key);
        let (tx, rx) = mpsc::channel();
        let waker = Waker::default();
        let wake = waker.clone();
        let (stop, mut stopping) = oneshot::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("icu-instance".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let mut next = match create_pipe(&pipe, true) {
                        Ok(p) => p,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e.to_string()));
                            return;
                        }
                    };
                    let _ = ready_tx.send(Ok(()));
                    let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
                    loop {
                        tokio::select! {
                            _ = &mut stopping => return,
                            result = next.connect() => if let Err(e) = result {
                                tracing::warn!(%e, "instance pipe connect"); return;
                            }
                        }
                        // Keep a listening instance present before handling this client.
                        let fresh = match create_pipe(&pipe, false) {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!(%e, "instance pipe");
                                return;
                            }
                        };
                        let conn = std::mem::replace(&mut next, fresh);
                        let Ok(slot) = slots.clone().try_acquire_owned() else {
                            continue;
                        };
                        let (tx, wake) = (tx.clone(), wake.clone());
                        tokio::spawn(async move {
                            let _slot = slot;
                            if let Ok(Err(e)) =
                                tokio::time::timeout(REQUEST_TIMEOUT, serve(conn, tx, wake)).await
                            {
                                tracing::debug!(%e, "instance request");
                            }
                        });
                    }
                });
            })
            .map_err(|e| e.to_string())?;
        ready_rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|e| e.to_string())??;
        Ok(Claim::First(Listener {
            requests: Mutex::new(rx),
            waker,
            _stop: stop,
            _mutex: mutex,
        }))
    }

    fn create_pipe(name: &str, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .pipe_mode(PipeMode::Message)
            .reject_remote_clients(true)
            .in_buffer_size(MAX_REQUEST as u32 + 1)
            .create(name)
    }

    async fn serve(
        mut conn: NamedPipeServer,
        tx: mpsc::Sender<Request>,
        wake: Waker,
    ) -> io::Result<()> {
        let mut data = vec![0u8; MAX_REQUEST + 1];
        let n = conn.read(&mut data).await?;
        let request = (n <= MAX_REQUEST)
            .then(|| parse(&data[..n]))
            .flatten()
            .ok_or_else(|| io::Error::other("not an instance request"))?;
        tx.send(request)
            .map_err(|_| io::Error::other("instance stopped"))?;
        if let Some(wake) = &*wake.lock().unwrap() {
            wake();
        }
        tracing::info!("another start handed over");
        conn.write_all(b"ok\n").await?;
        // Wait for the client's receipt before closing. No FlushFileBuffers:
        // that blocks forever when a client connects but never reads.
        let mut receipt = [0u8; 1];
        let _ = conn.read(&mut receipt).await;
        Ok(())
    }

    fn hand(pipe: &str, request: &Request, hand_over: Duration) -> Result<Claim, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let deadline = Instant::now() + hand_over;
            loop {
                // Retry only opening: replay after a lost acknowledgement can
                // import the same passport multiple times.
                match ClientOptions::new().open(pipe) {
                    Ok(mut conn) => {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        return tokio::time::timeout(remaining, async {
                            conn.write_all(&encode(request)).await?;
                            let mut answer = [0u8; 3];
                            conn.read_exact(&mut answer).await?;
                            if &answer != b"ok\n" {
                                return Err(io::Error::other("no answer"));
                            }
                            let _ = conn.write_all(b"!").await;
                            Ok(Claim::Handed)
                        })
                        .await
                        .map_err(|_| {
                            "ICU läuft bereits, antwortet aber nicht (Timeout)".to_string()
                        })?
                        .map_err(|e: io::Error| {
                            format!("ICU läuft bereits, antwortet aber nicht ({e})")
                        });
                    }
                    Err(e) if Instant::now() >= deadline => {
                        return Err(format!("ICU läuft bereits, antwortet aber nicht ({e})"))
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        })
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        fn unique_key() -> String {
            format!(
                "icu-gate-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )
        }

        #[test]
        fn handoff_is_exactly_once_even_with_a_silent_client() {
            let key = unique_key();
            let listener = match claim_in(
                Path::new(""),
                &key,
                &Request::default(),
                Duration::from_secs(1),
            )
            .unwrap()
            {
                Claim::First(l) => l,
                _ => panic!("not first"),
            };
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _entered = rt.enter();
            let silent = ClientOptions::new().open(pipe_name(&key)).unwrap();
            drop(_entered);
            for _ in 0..10 {
                let request = Request {
                    bundle: Some("test passport".into()),
                };
                assert!(matches!(
                    claim_in(Path::new(""), &key, &request, Duration::from_secs(1)),
                    Ok(Claim::Handed)
                ));
                assert_eq!(listener.try_recv(), Some(request));
                assert_eq!(listener.try_recv(), None);
            }
            drop(silent);
        }

        #[test]
        fn an_unresponsive_pipe_has_a_deadline() {
            let key = unique_key();
            let mutex =
                unsafe { CreateMutexW(None, false, &HSTRING::from(format!(r"Local\{key}"))) }
                    .unwrap();
            let _mutex = unsafe { OwnedHandle::from_raw_handle(mutex.0) };
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let enter = rt.enter();
            let _pipe = create_pipe(&pipe_name(&key), true).unwrap();
            drop(enter);
            let start = Instant::now();
            assert!(claim_in(
                Path::new(""),
                &key,
                &Request::default(),
                Duration::from_millis(200)
            )
            .is_err());
            assert!(start.elapsed() < Duration::from_secs(1));
        }
    }
}

/// Neither Unix nor Windows: no guard.
#[cfg(not(any(unix, windows)))]
pub struct Listener;

#[cfg(not(any(unix, windows)))]
impl Listener {
    /// Never anything.
    pub fn try_recv(&self) -> Option<Request> {
        None
    }

    /// Nothing to wake for.
    pub fn set_waker(&self, _wake: impl Fn() + Send + 'static) {}
}

/// See the Unix version; here there is no guard.
#[cfg(not(any(unix, windows)))]
pub fn claim_in(
    _dir: &std::path::Path,
    _key: &str,
    _request: &Request,
    _hand_over: Duration,
) -> Result<Claim, String> {
    Ok(Claim::Unguarded(
        "no single-instance guard on this system".into(),
    ))
}

/// The running instance's listener, for the windows.
static LISTENER: std::sync::OnceLock<Listener> = std::sync::OnceLock::new();

/// Makes `listener` the one [`poll`] and [`set_waker`] talk to (`main`).
pub fn install(listener: Listener) {
    let _ = LISTENER.set(listener);
}

/// The next request a later start handed over, if any. The window comes to
/// the front on it (like the tray's „Fenster anzeigen“).
pub fn poll() -> Option<Request> {
    LISTENER.get()?.try_recv()
}

/// What to call when a request arrives: the window's `request_repaint`.
pub fn set_waker(wake: impl Fn() + Send + 'static) {
    if let Some(listener) = LISTENER.get() {
        listener.set_waker(wake);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        path::Path,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use super::*;

    const PASSPORT: &str = "-----BEGIN ICU PASSPORT-----\nAAAA\n-----END ICU PASSPORT-----\n";

    fn first(dir: &Path, key: &str) -> Listener {
        match claim_in(dir, key, &Request::default(), Duration::from_millis(200)) {
            Ok(Claim::First(listener)) => listener,
            Ok(Claim::Handed) => panic!("handed over instead of first"),
            Ok(Claim::Unguarded(why)) => panic!("unguarded: {why}"),
            Err(e) => panic!("{e}"),
        }
    }

    /// A second start hands „show“ and its passport to the first and is
    /// told to exit; the first finds the request queued and was woken.
    #[test]
    fn a_second_start_hands_over_to_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let listener = first(dir.path(), "icu-test");
        let woken = Arc::new(AtomicUsize::new(0));
        let counter = woken.clone();
        listener.set_waker(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        assert!(listener.try_recv().is_none());

        let request = Request {
            bundle: Some(PASSPORT.into()),
        };
        let second = claim_in(dir.path(), "icu-test", &request, Duration::from_secs(2));
        assert!(matches!(second, Ok(Claim::Handed)));
        assert_eq!(listener.try_recv(), Some(request), "queued before `ok`");
        assert_eq!(woken.load(Ordering::Relaxed), 1);

        // A plain start: just „show“.
        let third = claim_in(
            dir.path(),
            "icu-test",
            &Request::default(),
            Duration::from_secs(2),
        );
        assert!(matches!(third, Ok(Claim::Handed)));
        assert_eq!(listener.try_recv(), Some(Request::default()));
    }

    /// A crash leaves the socket file behind, but not the lock: the next
    /// start becomes the first and replaces it.
    #[test]
    fn a_stale_socket_does_not_block_a_start() {
        let dir = tempfile::tempdir().unwrap();
        let stale =
            std::os::unix::net::UnixListener::bind(dir.path().join("icu-test.sock")).unwrap();
        drop(stale);
        assert!(dir.path().join("icu-test.sock").exists(), "left behind");
        let listener = first(dir.path(), "icu-test");
        let second = claim_in(
            dir.path(),
            "icu-test",
            &Request::default(),
            Duration::from_secs(2),
        );
        assert!(matches!(second, Ok(Claim::Handed)));
        assert!(listener.try_recv().is_some());
    }

    /// The lock is held but nobody answers: no second client — an error.
    #[test]
    fn an_instance_that_does_not_answer_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let lock = std::fs::File::create(dir.path().join("icu-test.lock")).unwrap();
        lock.try_lock().unwrap();
        let second = claim_in(
            dir.path(),
            "icu-test",
            &Request::default(),
            Duration::from_millis(300),
        );
        assert!(matches!(second, Err(e) if e.contains("antwortet aber nicht")));
    }

    /// Each configuration directory has its own instance (`--config-dir`
    /// for a second identity on the same machine); spellings of the same
    /// directory are one.
    #[test]
    fn config_dirs_have_their_own_instance() {
        let base = tempfile::tempdir().unwrap();
        let a = base.path().join("a");
        let b = base.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let key_a = key(&Paths::new(Some(a.clone())));
        assert_ne!(key_a, key(&Paths::new(Some(b))));
        assert_eq!(key_a, key(&Paths::new(Some(a.join(".")))));
        assert!(key_a.starts_with("icu-") && key_a.len() == 4 + 16);

        let runtime = tempfile::tempdir().unwrap();
        let _first_a = first(runtime.path(), "icu-a");
        let _first_b = first(runtime.path(), "icu-b");
    }

    #[test]
    fn only_instance_requests_are_taken() {
        assert_eq!(
            parse(&encode(&Request::default())),
            Some(Request::default())
        );
        let request = Request {
            bundle: Some(PASSPORT.into()),
        };
        assert_eq!(parse(&encode(&request)), Some(request));
        assert_eq!(parse(b"GET / HTTP/1.1\n"), None);
        assert_eq!(parse(b"icu-instance/2 show\n"), None);
        assert_eq!(parse(&[0xff, 0xfe]), None);
    }
}
