//! Real-time scheduling for the engine's audio worker threads
//! (`docs/reference/client.md` §4).
//!
//! `icu-dsp` and `icu-mixer` sit between the real-time backend callbacks
//! and, respectively, the network and the playback ring. As ordinary
//! `SCHED_OTHER` threads they run whenever the scheduler gets round to them;
//! on a loaded desktop that can be later than the ~15 ms of playback lead
//! the mixer keeps, and the callback then finds the ring short — an audible
//! gap. PipeWire's client context loads `module-rt`, which promotes threads
//! directly when `RLIMIT_RTPRIO` allows and otherwise sends a request to the
//! desktop portal or RTKit; this module asks it to do the same for the
//! calling thread. Where that request goes unanswered, a helper thread asks
//! RTKit itself, for the workers and for the callback thread
//! ([`watch_callback_thread`]).
//!
//! RTKit enforces `RLIMIT_RTTIME` (200 ms of CPU without blocking kills the
//! process), so every loop on a promoted thread must sleep or block well
//! within that. Both threads do; the DSP thread's backlog cap
//! (`engine::MAX_CAPTURE_BACKLOG`) keeps its catch-up after a stall to a few
//! blocks for exactly this reason.
//!
//! On Windows the same threads join MMCSS („Pro Audio“) instead.
//!
//! Failure is not an error: without RTKit and the rlimits the threads
//! simply stay best effort.

/// Requested priority: below PipeWire's own data loop (module-rt's default,
/// 20 through RTKit on a stock desktop), so the graph always wins over us.
pub const PRIORITY: i32 = 15;

/// Keeps the Windows MMCSS registration until the worker has finished.
/// Not Send: Windows requires release on the registering thread. On other
/// platforms scheduling already lasts for the thread lifetime.
#[derive(Default)]
#[must_use = "keep the guard alive for the audio worker's lifetime"]
pub struct PriorityGuard {
    #[cfg(target_os = "windows")]
    task: Option<windows::Win32::Foundation::HANDLE>,
    _same_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(target_os = "windows")]
impl Drop for PriorityGuard {
    fn drop(&mut self) {
        if let Some(task) = self.task {
            // SAFETY: the guard cannot move to another thread and owns this
            // registration. Drop runs before the worker exits, outside TLS
            // teardown and the Windows loader lock.
            if let Err(error) =
                unsafe { windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(task) }
            {
                tracing::warn!(%error, "could not release MMCSS registration");
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{
        ffi::{c_char, c_int, c_void},
        sync::atomic::{AtomicPtr, Ordering},
    };

    use pipewire::spa::sys::{
        spa_thread, spa_thread_utils, spa_thread_utils_methods, SPA_TYPE_INTERFACE_ThreadUtils,
    };

    #[link(name = "pipewire-0.3")]
    extern "C" {
        /// libpipewire's global thread utils. Since 1.x this is only the
        /// plain pthread implementation, whose `acquire_rt` is `ENOTSUP`;
        /// module-rt registers its own per context (`pipewire/thread.h`,
        /// `pipewire/context.h`; neither is bound by pipewire-sys).
        fn pw_thread_utils_get() -> *mut spa_thread_utils;
        fn pw_context_get_object(context: *mut c_void, type_: *const c_char) -> *mut c_void;
    }

    /// module-rt's thread utils, taken from the backend's context
    /// (`register_context`); null until the PipeWire thread has one.
    static CONTEXT_THREAD_UTILS: AtomicPtr<spa_thread_utils> = AtomicPtr::new(std::ptr::null_mut());

    /// Registers `context`'s thread utils; returns what was registered, for
    /// [`unregister`].
    pub fn register_context(context: *mut c_void) -> *mut c_void {
        // SAFETY: `context` is a live, connected `pw_context` (the caller's
        // own, on the PipeWire thread); the object registered under
        // `SPA_TYPE_INTERFACE_ThreadUtils` is a `spa_thread_utils` by
        // definition of that type, and module-rt keeps it alive as long as
        // the context. The caller unregisters it before the context goes.
        let utils = unsafe {
            pw_context_get_object(
                context,
                SPA_TYPE_INTERFACE_ThreadUtils.as_ptr() as *const c_char,
            )
        } as *mut spa_thread_utils;
        store(utils)
    }

    pub fn store(utils: *mut spa_thread_utils) -> *mut c_void {
        CONTEXT_THREAD_UTILS.store(utils, Ordering::Release);
        utils.cast()
    }

    /// Forgets `utils` again, unless a newer registration replaced it.
    pub fn unregister(utils: *mut c_void) {
        let _ = CONTEXT_THREAD_UTILS.compare_exchange(
            utils.cast(),
            std::ptr::null_mut(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    #[cfg(test)]
    pub fn registered() -> *mut c_void {
        CONTEXT_THREAD_UTILS.load(Ordering::Acquire).cast()
    }

    /// Asks module-rt (see [`promote`]); `Ok` only means it took the request.
    fn module_rt(priority: c_int) -> Result<(), String> {
        // SAFETY: the context's thread utils (see `register_context`) or
        // libpipewire's static default implementation, never null; the
        // interface contract makes `cb.funcs` a `spa_thread_utils_methods`
        // table and `cb.data` its object. module-rt (like libpipewire
        // itself, `pw_data_loop_start`) takes a `pthread_t` cast to
        // `spa_thread *` and resolves the calling thread's tid from it.
        unsafe {
            let mut utils = CONTEXT_THREAD_UTILS.load(Ordering::Acquire);
            if utils.is_null() {
                utils = pw_thread_utils_get();
            }
            if utils.is_null() {
                return Err("no thread utils".into());
            }
            let iface = &(*utils).iface;
            let methods = iface.cb.funcs as *const spa_thread_utils_methods;
            if methods.is_null() {
                return Err("thread utils without methods".into());
            }
            let Some(acquire_rt) = (*methods).acquire_rt else {
                return Err("thread utils without acquire_rt".into());
            };
            let thread = libc::pthread_self() as *mut c_void as *mut spa_thread;
            let r = acquire_rt(iface.cb.data, thread, priority);
            if r < 0 {
                return Err(std::io::Error::from_raw_os_error(-r).to_string());
            }
            Ok(())
        }
    }

    /// Whether the calling thread runs under a real-time policy. RTKit and
    /// module-rt set `SCHED_RESET_ON_FORK` with it, and the kernel reports
    /// that flag in the same value.
    pub fn is_realtime() -> bool {
        thread_is_realtime(0)
    }

    /// [`is_realtime`] for kernel thread `thread`; 0 is the caller.
    pub fn thread_is_realtime(thread: u32) -> bool {
        // SAFETY: a plain syscall; an unknown thread id is an error value.
        let policy = unsafe { libc::sched_getscheduler(thread as libc::pid_t) };
        policy >= 0
            && matches!(
                policy & !libc::SCHED_RESET_ON_FORK,
                libc::SCHED_FIFO | libc::SCHED_RR
            )
    }

    /// Asks module-rt for the calling thread and reports what the scheduler
    /// says, not what was asked for.
    ///
    /// With the rlimits (`@pipewire` group) module-rt switches the policy
    /// itself, at once. Without them it only *sends* a request and answers
    /// 0 — to the desktop portal's `Realtime` interface when the session
    /// bus has one, else to RTKit — and never reads the reply. Measured
    /// 2026-11-18 (Fedora 43, xdg-desktop-portal 1.20.4): the portal
    /// refuses every such request ("Could not get pidns"), so the thread
    /// stayed `SCHED_OTHER` on every start and rtkit-daemon's journal never
    /// saw ICU, while RTKit itself granted the same request at once. A
    /// thread that is not real-time after this is therefore handed to the
    /// helper, which asks RTKit directly (`super::watch_callback_thread`).
    pub fn promote(priority: c_int) -> Result<(), String> {
        let asked = module_rt(priority);
        if is_realtime() {
            return Ok(());
        }
        Err(match asked {
            Ok(()) => "module-rt took the request, the thread is still SCHED_OTHER".into(),
            Err(error) => format!("module-rt: {error}"),
        })
    }
}

/// RTKit on the system bus (`org.freedesktop.RealtimeKit1`), the service
/// behind module-rt's fallback: it switches a thread of an unprivileged
/// process to `SCHED_RR` on request.
#[cfg(target_os = "linux")]
mod rtkit {
    use std::time::Duration;

    const SERVICE: &str = "org.freedesktop.RealtimeKit1";
    const PATH: &str = "/org/freedesktop/RealtimeKit1";
    /// The whole exchange. It runs on the helper thread, never on an audio
    /// thread; a hung bus only delays the promotion.
    const TIMEOUT: Duration = Duration::from_secs(2);

    /// The kernel thread id of the calling thread.
    pub fn current_thread() -> u32 {
        // SAFETY: a plain syscall without arguments.
        (unsafe { libc::syscall(libc::SYS_gettid) }) as u32
    }

    /// Asks RTKit to make thread `thread` of this process real-time at
    /// `priority` (clamped to what RTKit allows) and waits for its answer.
    /// RTKit takes the thread id, so any thread may ask for another.
    pub fn make_thread_realtime(thread: u32, priority: i32) -> Result<(), String> {
        let thread = u64::from(thread);
        let process = u64::from(std::process::id());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime
            .block_on(async {
                tokio::time::timeout(TIMEOUT, async {
                    let conn = zbus::Connection::system().await?;
                    let max = property(&conn, "MaxRealtimePriority").await?;
                    let rttime = property(&conn, "RTTimeUSecMax").await?;
                    limit_rttime(rttime);
                    let priority = clamp_priority(priority, max);
                    conn.call_method(
                        Some(SERVICE),
                        PATH,
                        Some(SERVICE),
                        "MakeThreadRealtimeWithPID",
                        &(process, thread, priority),
                    )
                    .await?;
                    Ok::<(), zbus::Error>(())
                })
                .await
            })
            .map_err(|_| "no answer from the system bus".to_string())?
            .map_err(|e| e.to_string())
    }

    /// One integer property. RTKit serves `Get` only, not `GetAll`, which a
    /// caching `zbus::Proxy` would ask for.
    async fn property(conn: &zbus::Connection, name: &str) -> zbus::Result<i64> {
        let reply = conn
            .call_method(
                Some(SERVICE),
                PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(SERVICE, name),
            )
            .await?;
        let value: zbus::zvariant::OwnedValue = reply.body().deserialize()?;
        // `MaxRealtimePriority` is an int32, `RTTimeUSecMax` an int64.
        i64::try_from(&value)
            .or_else(|_| i32::try_from(&value).map(i64::from))
            .map_err(zbus::Error::from)
    }

    /// The priority to ask for: RTKit refuses anything above its maximum
    /// and anything below 1.
    pub fn clamp_priority(wanted: i32, max: i64) -> u32 {
        i64::from(wanted).clamp(1, max.max(1)) as u32
    }

    /// The `RLIMIT_RTTIME` to run under, if it must change: RTKit refuses a
    /// process whose limit is above `max_usec` (or unlimited).
    pub fn rttime_limit(current: libc::rlimit, max_usec: i64) -> Option<libc::rlimit> {
        let max = libc::rlim_t::try_from(max_usec).ok().filter(|&m| m > 0)?;
        (current.rlim_cur > max || current.rlim_max > max).then_some(libc::rlimit {
            rlim_cur: current.rlim_cur.min(max),
            rlim_max: current.rlim_max.min(max),
        })
    }

    /// Lowers the process's `RLIMIT_RTTIME` to RTKit's maximum, as module-rt
    /// does for its own requests. It only ever binds real-time threads.
    fn limit_rttime(max_usec: i64) {
        let mut current = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `current` is a valid, writable `rlimit`.
        if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut current) } != 0 {
            return;
        }
        if let Some(limit) = rttime_limit(current, max_usec) {
            // SAFETY: `limit` is a valid `rlimit`, only lowering both values.
            unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &limit) };
        }
    }
}

/// Windows: the Multimedia Class Scheduler (MMCSS) task „Pro Audio“, the
/// class WASAPI's own audio engine threads use; without it (the service
/// disabled) a time-critical thread priority. The MMCSS registration lasts
/// as long as the thread.
#[cfg(target_os = "windows")]
mod imp {
    use super::PriorityGuard;
    use windows::{
        core::w,
        Win32::System::Threading::{
            AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority, GetCurrentThread,
            SetThreadPriority, AVRT_PRIORITY_HIGH, THREAD_PRIORITY_TIME_CRITICAL,
        },
    };

    pub fn promote(_priority: i32) -> Result<PriorityGuard, String> {
        let mut index = 0u32;
        // SAFETY: plain Win32 calls on the calling thread.
        unsafe {
            match AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) {
                Ok(task) => {
                    let _ = AvSetMmThreadPriority(task, AVRT_PRIORITY_HIGH);
                    Ok(PriorityGuard {
                        task: Some(task),
                        ..PriorityGuard::default()
                    })
                }
                Err(mmcss) => SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL)
                    .map(|()| PriorityGuard::default())
                    .map_err(|e| format!("MMCSS: {mmcss}; SetThreadPriority: {e}")),
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod imp {
    pub fn promote(_priority: i32) -> Result<(), String> {
        Err("not implemented on this platform".into())
    }
}

/// The PipeWire context whose module-rt promotes threads, registered by
/// [`register_context`]. Dropping it forgets the context again; it must go
/// before the context does, or a thread started later would call into
/// freed memory.
#[cfg(target_os = "linux")]
#[must_use = "the context is forgotten when this is dropped"]
pub struct ContextRegistration(*mut std::ffi::c_void);

#[cfg(target_os = "linux")]
impl Drop for ContextRegistration {
    fn drop(&mut self) {
        imp::unregister(self.0);
    }
}

/// The thread utils registered right now; null when none (tests).
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn registered_context() -> *mut std::ffi::c_void {
    imp::registered()
}

/// Remembers the PipeWire context whose module-rt should promote threads,
/// until the returned registration is dropped. Called by the PipeWire
/// backend on its own thread, once the context is connected: a context
/// that failed to connect is freed right away, and its pointer, left
/// behind, was what the silent fallback's threads then dereferenced.
#[cfg(target_os = "linux")]
pub fn register_context(context: *mut std::ffi::c_void) -> ContextRegistration {
    ContextRegistration(imp::register_context(context))
}

/// Priority for the backend's callback thread (PipeWire's data loop): what
/// module-rt asks RTKit for on a stock desktop, above the workers'
/// [`PRIORITY`].
#[cfg(target_os = "linux")]
pub const CALLBACK_PRIORITY: i32 = 20;

/// The callback thread that announced itself last and has not been looked
/// at yet ([`note_callback_thread`]); 0 when none.
#[cfg(target_os = "linux")]
static CALLBACK_THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Worker threads module-rt left best effort, waiting for the helper: thread
/// id and name.
#[cfg(target_os = "linux")]
static WORKERS: std::sync::Mutex<Vec<(u32, String)>> = std::sync::Mutex::new(Vec::new());

/// Helpers running ([`watch_callback_thread`]). Without one nobody asks
/// RTKit: tests and runs on the fake backend stay best effort, as before.
#[cfg(target_os = "linux")]
static HELPERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Called at the top of the backend's real-time callback. The first call
/// on a thread hands its id to the helper started by
/// [`watch_callback_thread`]; every later one is a thread-local read.
/// Real-time safe: no lock, no allocation, one system call per thread.
#[cfg(target_os = "linux")]
#[inline]
pub fn note_callback_thread() {
    thread_local! {
        static NOTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !NOTED.with(|noted| noted.replace(true)) {
        CALLBACK_THREAD.store(
            rtkit::current_thread(),
            std::sync::atomic::Ordering::Release,
        );
    }
}

/// Stops the helper of [`watch_callback_thread`] when dropped.
#[cfg(target_os = "linux")]
#[must_use = "the helper stops when this is dropped"]
pub struct CallbackWatch(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(target_os = "linux")]
impl Drop for CallbackWatch {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Starts the helper thread that asks RTKit for the audio threads module-rt
/// left best effort, for as long as the returned guard lives.
///
/// Two kinds of threads reach it. `icu-dsp` and `icu-mixer` hand themselves
/// over in [`promote_current_thread`]. The data loop that runs the stream
/// callbacks gets its priority from module-rt like any thread it creates —
/// through the same unanswered request — and cannot ask for itself (a bus
/// round trip on the real-time path), so it only announces its thread id
/// ([`note_callback_thread`]). RTKit takes a thread id, so the helper asks
/// for all of them and no audio thread ever waits for the bus.
///
/// Started by the PipeWire backend only: without a real audio graph there
/// is nothing to be on time for, and a test run starting dozens of engines
/// would spend RTKit's per-user request allowance.
#[cfg(target_os = "linux")]
pub fn watch_callback_thread() -> CallbackWatch {
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    HELPERS.fetch_add(1, Ordering::AcqRel);
    let spawned = std::thread::Builder::new()
        .name("icu-rt-helper".into())
        .spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let workers = std::mem::take(&mut *WORKERS.lock().unwrap());
                for (thread, name) in workers {
                    promote_from_outside(thread, &name, PRIORITY);
                }
                let callback = CALLBACK_THREAD.swap(0, Ordering::AcqRel);
                if callback != 0 {
                    promote_from_outside(callback, "audio callback", CALLBACK_PRIORITY);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if HELPERS.fetch_sub(1, Ordering::AcqRel) == 1 {
                WORKERS.lock().unwrap().clear();
            }
        });
    if let Err(error) = spawned {
        HELPERS.fetch_sub(1, Ordering::AcqRel);
        tracing::warn!(%error, "cannot start the real-time helper thread");
    }
    CallbackWatch(stop)
}

/// Asks RTKit for thread `thread` unless it is real-time already, and logs
/// what the scheduler says afterwards.
#[cfg(target_os = "linux")]
fn promote_from_outside(thread: u32, name: &str, priority: i32) {
    let asked = if imp::thread_is_realtime(thread) {
        Ok(())
    } else {
        rtkit::make_thread_realtime(thread, priority)
    };
    if imp::thread_is_realtime(thread) {
        tracing::info!(thread = name, priority, "real-time priority acquired");
    } else {
        let error = asked
            .err()
            .unwrap_or_else(|| "accepted, not applied".into());
        tracing::info!(thread = name, %error, "no real-time priority (RTKit); running best effort");
    }
}

/// Asks for real-time priority for the calling thread and logs the outcome.
/// Never fails the caller.
pub fn promote_current_thread(name: &str) -> PriorityGuard {
    #[cfg(target_os = "windows")]
    let result = imp::promote(PRIORITY);
    #[cfg(not(target_os = "windows"))]
    let result = imp::promote(PRIORITY).map(|()| PriorityGuard::default());
    match result {
        Ok(guard) => {
            tracing::info!(
                thread = name,
                priority = PRIORITY,
                "real-time priority acquired"
            );
            guard
        }
        Err(error) => {
            // The helper asks RTKit and logs the outcome; without one the
            // thread stays best effort.
            #[cfg(target_os = "linux")]
            if HELPERS.load(std::sync::atomic::Ordering::Acquire) > 0 {
                tracing::debug!(thread = name, %error, "asking RTKit for real-time priority");
                WORKERS
                    .lock()
                    .unwrap()
                    .push((rtkit::current_thread(), name.to_string()));
                return PriorityGuard::default();
            }
            tracing::info!(thread = name, %error, "no real-time priority; running best effort");
            PriorityGuard::default()
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A registration lasts as long as its guard, and an older guard going
    /// away does not take a newer registration with it.
    #[test]
    fn a_registration_ends_with_its_guard() {
        // Zeroed utils: a thread promoted meanwhile (another test) finds no
        // methods and stays best effort instead of calling into garbage.
        // SAFETY: a plain C struct of pointers and integers; all-zero is
        // a valid value of it.
        let mut first_utils: pipewire::spa::sys::spa_thread_utils = unsafe { std::mem::zeroed() };
        let mut second_utils: pipewire::spa::sys::spa_thread_utils = unsafe { std::mem::zeroed() };
        let first = ContextRegistration(imp::store(&mut first_utils));
        assert_eq!(imp::registered(), first.0);
        let second = ContextRegistration(imp::store(&mut second_utils));
        drop(first);
        assert_eq!(imp::registered(), second.0, "still the newer one");
        drop(second);
        assert!(imp::registered().is_null(), "nothing left to dereference");
    }

    #[test]
    fn rtkit_priority_stays_inside_its_range() {
        assert_eq!(rtkit::clamp_priority(15, 20), 15);
        assert_eq!(rtkit::clamp_priority(15, 10), 10);
        assert_eq!(rtkit::clamp_priority(0, 20), 1);
        assert_eq!(rtkit::clamp_priority(15, 0), 1, "a broken maximum");
    }

    #[test]
    fn rttime_is_only_ever_lowered() {
        let limit = |cur, max| libc::rlimit {
            rlim_cur: cur,
            rlim_max: max,
        };
        let lowered = rtkit::rttime_limit(limit(libc::RLIM_INFINITY, libc::RLIM_INFINITY), 200_000)
            .expect("unlimited is above RTKit's maximum");
        assert_eq!((lowered.rlim_cur, lowered.rlim_max), (200_000, 200_000));
        let lowered = rtkit::rttime_limit(limit(150_000, libc::RLIM_INFINITY), 200_000)
            .expect("the hard limit is above it");
        assert_eq!((lowered.rlim_cur, lowered.rlim_max), (150_000, 200_000));
        assert!(rtkit::rttime_limit(limit(150_000, 200_000), 200_000).is_none());
        assert!(rtkit::rttime_limit(limit(libc::RLIM_INFINITY, libc::RLIM_INFINITY), 0).is_none());
    }

    /// On a desktop with RTKit (`ICU_TEST_REQUIRE_RT=1`; skipped otherwise,
    /// so an ordinary test run asks RTKit for nothing): with the helper
    /// running, a worker that hands itself over and a callback thread that
    /// announces itself both end up real-time, without either of them
    /// touching the bus.
    #[test]
    fn the_helper_promotes_workers_and_the_callback_thread() {
        if std::env::var_os("ICU_TEST_REQUIRE_RT").is_none() {
            return;
        }
        let watch = watch_callback_thread();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let done_rx = std::sync::Arc::new(std::sync::Mutex::new(done_rx));
        let wait = |rx: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !imp::is_realtime() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let _ = rx.lock().unwrap().recv();
            imp::is_realtime()
        };
        let rx = done_rx.clone();
        let worker = std::thread::spawn(move || {
            assert!(!imp::is_realtime(), "threads start best effort");
            let _priority = promote_current_thread("icu-test-worker");
            wait(rx)
        });
        let rx = done_rx.clone();
        let callback = std::thread::spawn(move || {
            note_callback_thread();
            note_callback_thread();
            wait(rx)
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(done_tx);
        assert!(worker.join().unwrap(), "the worker was promoted");
        assert!(callback.join().unwrap(), "the callback thread was promoted");
        drop(watch);
    }

    /// Without a helper nobody is asked and the thread stays what it was.
    #[test]
    fn without_a_helper_a_thread_stays_best_effort() {
        if std::env::var_os("ICU_TEST_REQUIRE_RT").is_some() {
            return;
        }
        let realtime = std::thread::spawn(|| {
            let _priority = promote_current_thread("icu-test-worker");
            std::thread::sleep(std::time::Duration::from_millis(150));
            imp::is_realtime()
        })
        .join()
        .unwrap();
        assert!(!realtime);
        assert!(
            WORKERS.lock().unwrap().is_empty(),
            "nothing queued for nobody"
        );
    }
}
