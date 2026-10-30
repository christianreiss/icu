//! One running client per configuration directory
//! (`docs/reference/client.md` §10).
//!
//! Starting ICU again — the natural way back after „Schließen“ hid the
//! window to the tray, or a double-click on an `.icu` file — must not
//! connect a second client with the same certificate: the server would
//! replace the running session (`SESSION_REPLACED`). The first windowed
//! instance holds a lock and listens on a Unix socket in `$XDG_RUNTIME_DIR`
//! (only this user can reach it; a runtime directory others can get into
//! is not used) — on Windows a named mutex and a named pipe only this user
//! can open (`windows`) — both named after a hash of its
//! configuration directory, so `--config-dir` test instances stay apart. A
//! later start hands its request over — „show“, plus the text of the invite
//! or passport it was given — and exits. `--headless` does none of this.
//!
//! The lock, not the socket, decides who is first: the kernel releases it
//! with the process, so a crashed instance never blocks a start, and the
//! socket file it left behind is simply replaced.
//!
//! Protocol, one connection per request: the new start writes the line
//! `icu-instance/2 show <version>`, then the invite or passport text if
//! any, shuts down its write side, and waits for the answer. `ok`: taken.
//! `version <v>`: the running ICU is another version and took nothing —
//! the start says so and ends, instead of handing an update's first start
//! to the old ICU in the tray. An ICU from before versions were sent
//! (`icu-instance/1 show`, still taken) closes without an answer on a
//! version-2 request: that too is an older ICU still running.

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

/// First line of a request from an ICU that sends no version (before
/// 2026-09-27); still taken.
const HELLO_V1: &str = "icu-instance/1 show";

/// First line of a request, followed by a space and the sender's version.
const HELLO_V2: &str = "icu-instance/2 show";

/// The version this build announces and compares.
const VERSION: &str = env!("CARGO_PKG_VERSION");

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
    // Whoever else can write there could listen in ICU's place and be
    // handed invites and passports.
    #[cfg(unix)]
    if let Err(why) = private_dir(&dir, effective_uid()) {
        tracing::warn!(dir = %dir.display(), %why, "XDG_RUNTIME_DIR is not private");
        return Ok(Claim::Unguarded(format!(
            "XDG_RUNTIME_DIR {}: {why}",
            dir.display()
        )));
    }
    #[cfg(not(windows))]
    claim_in(&dir, &key(paths), request, HAND_OVER)
}

/// Whether `dir` belongs to `euid` (when known) and nobody else may enter
/// it — what the XDG Base Directory Specification promises of
/// `$XDG_RUNTIME_DIR`, and a misconfigured one may not keep.
#[cfg(unix)]
fn private_dir(dir: &std::path::Path, euid: Option<u32>) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(dir).map_err(|e| e.to_string())?;
    if let Some(euid) = euid.filter(|euid| meta.uid() != *euid) {
        return Err(format!("owned by uid {}, not {euid}", meta.uid()));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(format!("mode {:o}, others may enter", meta.mode() & 0o7777));
    }
    Ok(())
}

/// This process's effective user id, where the platform bindings have it.
#[cfg(unix)]
fn effective_uid() -> Option<u32> {
    #[cfg(target_os = "linux")]
    // SAFETY: geteuid has no preconditions and cannot fail.
    return Some(unsafe { libc::geteuid() });
    #[cfg(not(target_os = "linux"))]
    None
}

/// The encoded request.
fn encode(request: &Request) -> Vec<u8> {
    format!(
        "{HELLO_V2} {VERSION}\n{}",
        request.bundle.as_deref().unwrap_or("")
    )
    .into_bytes()
}

/// Reads a request and the sender's version (`None` from an ICU that
/// sends none); `None` for anything else.
fn parse(data: &[u8]) -> Option<(Request, Option<String>)> {
    let text = std::str::from_utf8(data).ok()?;
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let first = first.trim_end();
    let version = if first == HELLO_V1 {
        None
    } else {
        let version = first.strip_prefix(HELLO_V2)?.strip_prefix(' ')?;
        if version.is_empty() || version.contains(char::is_whitespace) {
            return None;
        }
        Some(version.to_owned())
    };
    let request = Request {
        bundle: (!rest.trim().is_empty()).then(|| rest.to_owned()),
    };
    Some((request, version))
}

/// What the running instance of version `own` answers a start of version
/// `sender`: `None` when it takes the request (`ok`) — same version, or a
/// start that sends none — else its own version, and nothing is taken.
fn refusal(sender: Option<&str>, own: &str) -> Option<String> {
    sender
        .filter(|sender| *sender != own)
        .map(|_| format!("version {own}\n"))
}

/// The running instance's answer, as the start reads it.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    /// Taken.
    Ok,
    /// Not taken: it runs this other version.
    Version(String),
    /// Closed without a word: an ICU from before versions were sent, which
    /// knows only `icu-instance/1`.
    Silent,
    /// Something else.
    Garbled,
}

fn read_answer(data: &[u8]) -> Answer {
    if data.is_empty() {
        return Answer::Silent;
    }
    match std::str::from_utf8(data).map(str::trim_end) {
        Ok("ok") => Answer::Ok,
        Ok(text) => match text.strip_prefix("version ") {
            Some(version) if !version.is_empty() => Answer::Version(version.to_owned()),
            _ => Answer::Garbled,
        },
        Err(_) => Answer::Garbled,
    }
}

/// What a start tells the user when the running ICU is another version
/// (`None`: one from before versions were sent): it has to be quit first,
/// or this start would only bring the old one to the front.
fn other_version(running: Option<&str>) -> String {
    let running = match running {
        Some(version) => format!("ICU {version}"),
        None => "Eine ältere ICU-Version".into(),
    };
    format!(
        "{running} läuft bereits, diese ist {VERSION}. Bitte das laufende ICU beenden \
         (Infobereich: Rechtsklick → „Beenden“) und diese Version erneut starten."
    )
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

    use super::{
        encode, other_version, parse, read_answer, refusal, Answer, Claim, Request, MAX_REQUEST,
        VERSION,
    };

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
    /// a start that heard `ok` knows it arrived. A start of another version
    /// gets this one's version instead, and nothing is taken.
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
        let (request, version) = (data.len() <= MAX_REQUEST)
            .then(|| parse(&data))
            .flatten()
            .ok_or_else(|| std::io::Error::other("not an instance request"))?;
        if let Some(refused) = refusal(version.as_deref(), VERSION) {
            tracing::info!(?version, "a start of another version was turned away");
            return conn.write_all(refused.as_bytes());
        }
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
    /// may still be starting up. A running ICU of another version is an
    /// error for the user, not something to wait out.
    fn hand(socket: &Path, request: &Request, hand_over: Duration) -> Result<Claim, String> {
        let deadline = Instant::now() + hand_over;
        loop {
            let e = match send(socket, request) {
                Ok(Answer::Ok) => return Ok(Claim::Handed),
                Ok(Answer::Version(version)) => return Err(other_version(Some(&version))),
                Ok(Answer::Silent) => return Err(other_version(None)),
                Ok(Answer::Garbled) => std::io::Error::other("no answer"),
                Err(e) => e,
            };
            if Instant::now() >= deadline {
                return Err(format!("ICU läuft bereits, antwortet aber nicht ({e})"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn send(socket: &Path, request: &Request) -> std::io::Result<Answer> {
        let mut conn = UnixStream::connect(socket)?;
        conn.set_read_timeout(Some(Duration::from_secs(3)))?;
        conn.set_write_timeout(Some(Duration::from_secs(3)))?;
        conn.write_all(&encode(request))?;
        conn.shutdown(Shutdown::Write)?;
        let mut answer = Vec::new();
        conn.take(64).read_to_end(&mut answer)?;
        Ok(read_answer(&answer))
    }
}

#[cfg(windows)]
pub use windows::{claim_in, Listener};

/// Windows: a named mutex in the session's namespace (`Local\\icu-…`)
/// decides who is first — Windows destroys it with the last handle, so a
/// crashed instance never blocks a start — and a named pipe
/// (`\\.\pipe\icu-…-<user SID>`, local clients only) takes the requests.
/// Pipe names are machine-wide: the pipe is owned by this user and only
/// this user may open it, and a start hands its request only to a pipe
/// this user owns — another account can create the name first (SIDs are no
/// secret), but not with this user as its owner. Pipes have no half-close:
/// the pipe runs in message mode and a request is one message.
#[cfg(windows)]
pub(crate) mod windows {
    use super::{
        encode, other_version, parse, read_answer, refusal, Answer, Claim, Request, MAX_REQUEST,
        VERSION,
    };
    use ::windows::{
        core::{HSTRING, PWSTR},
        Win32::{
            Foundation::{GetLastError, LocalFree, ERROR_ALREADY_EXISTS, HANDLE, HLOCAL},
            Security::{
                Authorization::{
                    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                    GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
                },
                GetTokenInformation, TokenUser, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
                PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
            },
            System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken},
        },
    };
    use std::{
        io,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        path::Path,
        sync::{mpsc, Arc, Mutex},
        time::{Duration, Instant},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::windows::named_pipe::{
            ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
        },
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

    fn pipe_name(key: &str, sid: &str) -> String {
        format!(r"\\.\pipe\{key}-{sid}")
    }

    /// A SID as text (`S-1-5-21-…`).
    fn sid_string(sid: PSID) -> io::Result<String> {
        let mut text = PWSTR::null();
        // SAFETY: `sid` is valid for the call; the string is LocalAlloc'ed
        // and freed once copied.
        unsafe {
            ConvertSidToStringSidW(sid, &mut text)?;
            let sid = text.to_string();
            LocalFree(Some(HLOCAL(text.0.cast())));
            sid.map_err(io::Error::other)
        }
    }

    /// The user this process runs as, as a SID string.
    pub(crate) fn user_sid() -> io::Result<String> {
        // SAFETY: the token handle is owned (closed on drop); TOKEN_USER is
        // read from a buffer of the size Windows asked for, aligned for it.
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
            let token = OwnedHandle::from_raw_handle(token.0);
            let token = HANDLE(token.as_raw_handle());
            let mut len = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
            let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                len,
                &mut len,
            )?;
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);
            sid_string(user.User.Sid)
        }
    }

    /// A security descriptor owned by `sid` that lets only `sid` in.
    struct Descriptor(PSECURITY_DESCRIPTOR);

    impl Descriptor {
        fn only_for(sid: &str) -> io::Result<Descriptor> {
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            // SAFETY: the SDDL string outlives the call; the descriptor is
            // LocalAlloc'ed and freed in `drop`.
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    &HSTRING::from(format!("O:{sid}D:P(A;;GA;;;{sid})")),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    None,
                )?;
            }
            Ok(Descriptor(descriptor))
        }
    }

    impl Drop for Descriptor {
        fn drop(&mut self) {
            // SAFETY: allocated by ConvertStringSecurityDescriptor…, freed once.
            unsafe { LocalFree(Some(HLOCAL(self.0 .0))) };
        }
    }

    /// Whether the pipe behind `conn` is owned by `sid`: made by an ICU of
    /// this user, not by another account that took the name first.
    fn owned_by(conn: &NamedPipeClient, sid: &str) -> bool {
        let mut owner = PSID::default();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `owner` points into `descriptor`, which is LocalAlloc'ed
        // and freed after the owner was copied out.
        unsafe {
            let status = GetSecurityInfo(
                HANDLE(conn.as_raw_handle()),
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                Some(&mut owner),
                None,
                None,
                None,
                Some(&mut descriptor),
            );
            if status.is_err() {
                return false;
            }
            let ours = sid_string(owner).is_ok_and(|owner| owner == sid);
            LocalFree(Some(HLOCAL(descriptor.0)));
            ours
        }
    }

    pub fn claim_in(
        _dir: &Path,
        key: &str,
        request: &Request,
        hand_over: Duration,
    ) -> Result<Claim, String> {
        let sid = match user_sid() {
            Ok(sid) => sid,
            Err(e) => return Ok(Claim::Unguarded(format!("user SID: {e}"))),
        };
        let name = HSTRING::from(format!(r"Local\{key}"));
        let handle =
            unsafe { CreateMutexW(None, false, &name) }.map_err(|e| format!("mutex: {e}"))?;
        let existing = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let mutex = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        if existing {
            return hand(&pipe_name(key, &sid), &sid, request, hand_over);
        }
        let pipe = pipe_name(key, &sid);
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
                    let mut next = match create_pipe(&pipe, &sid, true) {
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
                        let fresh = match create_pipe(&pipe, &sid, false) {
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
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Claim::First(Listener {
                requests: Mutex::new(rx),
                waker,
                _stop: stop,
                _mutex: mutex,
            })),
            // The name is taken — by another account that got there
            // first. Run all the same; the mutex stays held, so a later
            // start of this user refuses that pipe instead of becoming a
            // second client.
            Ok(Err(e)) => {
                std::mem::forget(mutex);
                Ok(Claim::Unguarded(format!("instance pipe: {e}")))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn create_pipe(name: &str, sid: &str, first: bool) -> io::Result<NamedPipeServer> {
        let descriptor = Descriptor::only_for(sid)?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0 .0,
            bInheritHandle: false.into(),
        };
        // SAFETY: `attributes` and the descriptor it points to outlive the
        // call, which copies them into the pipe.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .pipe_mode(PipeMode::Message)
                .reject_remote_clients(true)
                .in_buffer_size(MAX_REQUEST as u32 + 1)
                .create_with_security_attributes_raw(
                    name,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )
        }
    }

    async fn serve(
        mut conn: NamedPipeServer,
        tx: mpsc::Sender<Request>,
        wake: Waker,
    ) -> io::Result<()> {
        let mut data = vec![0u8; MAX_REQUEST + 1];
        let n = conn.read(&mut data).await?;
        let (request, version) = (n <= MAX_REQUEST)
            .then(|| parse(&data[..n]))
            .flatten()
            .ok_or_else(|| io::Error::other("not an instance request"))?;
        if let Some(refused) = refusal(version.as_deref(), VERSION) {
            tracing::info!(?version, "a start of another version was turned away");
            conn.write_all(refused.as_bytes()).await?;
        } else {
            tx.send(request)
                .map_err(|_| io::Error::other("instance stopped"))?;
            if let Some(wake) = &*wake.lock().unwrap() {
                wake();
            }
            tracing::info!("another start handed over");
            conn.write_all(b"ok\n").await?;
        }
        // Wait for the client's receipt before closing. No FlushFileBuffers:
        // that blocks forever when a client connects but never reads.
        let mut receipt = [0u8; 1];
        let _ = conn.read(&mut receipt).await;
        Ok(())
    }

    fn hand(
        pipe: &str,
        sid: &str,
        request: &Request,
        hand_over: Duration,
    ) -> Result<Claim, String> {
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
                    Ok(conn) if !owned_by(&conn, sid) => {
                        return Err(
                            "ICU läuft bereits, antwortet aber nicht (die Pipe gehört einem \
                             anderen Konto)"
                                .to_string(),
                        );
                    }
                    Ok(mut conn) => {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        return tokio::time::timeout(remaining, async {
                            conn.write_all(&encode(request)).await?;
                            let mut answer = [0u8; 64];
                            // An ICU without versions drops the pipe without a word.
                            let n = match conn.read(&mut answer).await {
                                Ok(n) => n,
                                Err(e)
                                    if matches!(
                                        e.kind(),
                                        io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof
                                    ) =>
                                {
                                    0
                                }
                                Err(e) => return Err(e),
                            };
                            match read_answer(&answer[..n]) {
                                Answer::Ok => {
                                    let _ = conn.write_all(b"!").await;
                                    Ok(Ok(Claim::Handed))
                                }
                                Answer::Version(version) => Ok(Err(other_version(Some(&version)))),
                                Answer::Silent => Ok(Err(other_version(None))),
                                Answer::Garbled => Err(io::Error::other("no answer")),
                            }
                        })
                        .await
                        .map_err(|_| {
                            "ICU läuft bereits, antwortet aber nicht (Timeout)".to_string()
                        })?
                        .map_err(|e: io::Error| {
                            format!("ICU läuft bereits, antwortet aber nicht ({e})")
                        })?;
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
            let silent = ClientOptions::new()
                .open(pipe_name(&key, &user_sid().unwrap()))
                .unwrap();
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
            let sid = user_sid().unwrap();
            let _pipe = create_pipe(&pipe_name(&key, &sid), &sid, true).unwrap();
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

        /// The pipe carries this user's SID in its name and is owned by it
        /// (what a start checks before handing anything over); a pipe with
        /// Windows' default descriptor, as another account would create it,
        /// is refused unless its owner happens to be this user.
        #[test]
        fn the_pipe_is_this_users() {
            let key = unique_key();
            let sid = user_sid().unwrap();
            assert!(sid.starts_with("S-1-"), "{sid}");
            let name = pipe_name(&key, &sid);
            assert!(name.ends_with(&format!("-{sid}")), "{name}");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _enter = rt.enter();
            let _pipe = create_pipe(&name, &sid, true).unwrap();
            let conn = ClientOptions::new().open(&name).unwrap();
            assert!(owned_by(&conn, &sid));
            assert!(!owned_by(&conn, "S-1-5-18"), "not SYSTEM's");
        }

        /// A running ICU from before versions were sent takes the request
        /// line `icu-instance/2` for garbage and closes: the start says an
        /// older ICU has to be quit, at once rather than at the deadline.
        #[test]
        fn an_icu_without_versions_is_named_as_older() {
            let key = unique_key();
            let sid = user_sid().unwrap();
            let mutex =
                unsafe { CreateMutexW(None, false, &HSTRING::from(format!(r"Local\{key}"))) }
                    .unwrap();
            let _mutex = unsafe { OwnedHandle::from_raw_handle(mutex.0) };
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap();
            let name = pipe_name(&key, &sid);
            let pipe = {
                let _enter = rt.enter();
                create_pipe(&name, &sid, true).unwrap()
            };
            // The old server: reads one message, finds no `icu-instance/1`,
            // drops the pipe.
            rt.spawn(async move {
                let mut pipe = pipe;
                pipe.connect().await.unwrap();
                let mut data = vec![0u8; MAX_REQUEST + 1];
                let _ = pipe.read(&mut data).await;
            });
            let start = Instant::now();
            let refused = claim_in(
                Path::new(""),
                &key,
                &Request::default(),
                Duration::from_secs(3),
            );
            assert!(
                matches!(&refused, Err(e) if e.contains("ältere ICU-Version")),
                "{:?}",
                refused.as_ref().err()
            );
            assert!(start.elapsed() < Duration::from_secs(2));
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

    /// A private runtime directory is used; one of another user, or one
    /// others may enter, is not (`$XDG_RUNTIME_DIR` misconfigured).
    #[test]
    fn only_a_private_runtime_directory_is_used() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let uid = std::fs::metadata(dir.path()).unwrap().uid();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(private_dir(dir.path(), Some(uid)), Ok(()));
        assert_eq!(private_dir(dir.path(), None), Ok(()));
        assert_eq!(effective_uid().unwrap_or(uid), uid, "tempdir is ours");
        let foreign = private_dir(dir.path(), Some(uid + 1)).unwrap_err();
        assert!(foreign.contains("owned by uid"), "{foreign}");
        for mode in [0o755, 0o770, 0o701] {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            let open = private_dir(dir.path(), Some(uid)).unwrap_err();
            assert!(open.contains("others may enter"), "{mode:o}: {open}");
        }
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn only_instance_requests_are_taken() {
        assert_eq!(
            parse(&encode(&Request::default())),
            Some((Request::default(), Some(VERSION.to_string())))
        );
        let request = Request {
            bundle: Some(PASSPORT.into()),
        };
        assert_eq!(
            parse(&encode(&request)),
            Some((request.clone(), Some(VERSION.to_string())))
        );
        // What an ICU without versions sends is still taken.
        assert_eq!(
            parse(format!("icu-instance/1 show\n{PASSPORT}").as_bytes()),
            Some((request, None))
        );
        assert_eq!(parse(b"GET / HTTP/1.1\n"), None);
        assert_eq!(parse(b"icu-instance/2 show\n"), None);
        assert_eq!(parse(b"icu-instance/2 show \n"), None);
        assert_eq!(parse(b"icu-instance/2 show 1.0 2\n"), None);
        assert_eq!(parse(b"icu-instance/3 show 1.0\n"), None);
        assert_eq!(parse(&[0xff, 0xfe]), None);
    }

    /// The same version, or one that sends none, is taken; another version
    /// is told the running one's, and a start reads each answer.
    #[test]
    fn another_version_is_turned_away_and_told() {
        assert_eq!(refusal(None, "0.2.0"), None);
        assert_eq!(refusal(Some("0.2.0"), "0.2.0"), None);
        assert_eq!(
            refusal(Some("0.3.0"), "0.2.0").as_deref(),
            Some("version 0.2.0\n")
        );
        assert_eq!(read_answer(b"ok\n"), Answer::Ok);
        assert_eq!(
            read_answer(b"version 0.2.0\n"),
            Answer::Version("0.2.0".into())
        );
        assert_eq!(read_answer(b""), Answer::Silent);
        assert_eq!(read_answer(b"version \n"), Answer::Garbled);
        assert_eq!(read_answer(b"nope\n"), Answer::Garbled);
        assert!(other_version(Some("0.2.0")).starts_with("ICU 0.2.0 läuft bereits"));
        assert!(other_version(None).contains("ältere ICU-Version"));
    }

    /// A start from before versions were sent still hands over to this one.
    #[test]
    fn a_start_without_a_version_is_still_taken() {
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let listener = first(dir.path(), "icu-test");
        let mut conn =
            std::os::unix::net::UnixStream::connect(dir.path().join("icu-test.sock")).unwrap();
        conn.write_all(format!("icu-instance/1 show\n{PASSPORT}").as_bytes())
            .unwrap();
        conn.shutdown(std::net::Shutdown::Write).unwrap();
        let mut answer = String::new();
        conn.read_to_string(&mut answer).unwrap();
        assert_eq!(answer, "ok\n");
        assert_eq!(
            listener.try_recv(),
            Some(Request {
                bundle: Some(PASSPORT.into())
            })
        );
    }

    /// Stands in for a running ICU that holds the lock and answers every
    /// request with `answer` (empty: closes without a word).
    fn running(dir: &Path, answer: &'static [u8]) -> std::fs::File {
        use std::io::{Read, Write};
        let lock = std::fs::File::create(dir.join("icu-test.lock")).unwrap();
        lock.try_lock().unwrap();
        let socket = std::os::unix::net::UnixListener::bind(dir.join("icu-test.sock")).unwrap();
        std::thread::spawn(move || {
            for mut conn in socket.incoming().flatten() {
                let mut request = Vec::new();
                let _ = conn.read_to_end(&mut request);
                let _ = conn.write_all(answer);
            }
        });
        lock
    }

    /// A running ICU of another version, or one from before versions were
    /// sent, takes nothing: the start ends with a message to quit it, at
    /// once rather than after the hand-over deadline.
    #[test]
    fn a_running_icu_of_another_version_is_named() {
        for (answer, expected) in [
            (&b"version 0.0.1\n"[..], "ICU 0.0.1 läuft bereits"),
            (&b""[..], "Eine ältere ICU-Version läuft bereits"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let _lock = running(dir.path(), answer);
            let start = std::time::Instant::now();
            let refused = claim_in(
                dir.path(),
                "icu-test",
                &Request::default(),
                Duration::from_secs(3),
            );
            assert!(
                matches!(&refused, Err(e) if e.starts_with(expected) && e.contains("beenden")),
                "{:?}",
                refused.as_ref().err()
            );
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }
}
