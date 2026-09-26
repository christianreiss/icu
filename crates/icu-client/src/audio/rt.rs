//! Real-time scheduling for the engine's audio worker threads
//! (`Client/IMPLEMENTATION.md` §4).
//!
//! `icu-dsp` and `icu-mixer` sit between the real-time backend callbacks
//! and, respectively, the network and the playback ring. As ordinary
//! `SCHED_OTHER` threads they run whenever the scheduler gets round to them;
//! on a loaded desktop that can be later than the ~15 ms of playback lead
//! the mixer keeps, and the callback then finds the ring short — an audible
//! gap. PipeWire's client context loads `module-rt`, which promotes threads
//! through RTKit (or directly when `RLIMIT_RTPRIO` allows) and is what gives
//! the graph's own data loop in this process its priority; this module asks
//! it to do the same for the calling thread.
//!
//! RTKit enforces `RLIMIT_RTTIME` (200 ms of CPU without blocking kills the
//! process), so every loop on a promoted thread must sleep or block well
//! within that. Both threads do; the DSP thread's backlog cap
//! (`engine::MAX_CAPTURE_BACKLOG`) keeps its catch-up after a stall to a few
//! blocks for exactly this reason.
//!
//! On Windows the same threads join MMCSS („Pro Audio“) instead.
//!
//! Failure is not an error: without module-rt, RTKit or the rlimits the
//! threads simply stay best effort, as they were before.

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

    pub fn register_context(context: *mut c_void) {
        // SAFETY: `context` is a live `pw_context` (the caller's own, on the
        // PipeWire thread); the object registered under
        // `SPA_TYPE_INTERFACE_ThreadUtils` is a `spa_thread_utils` by
        // definition of that type, and module-rt keeps it alive as long as
        // the context, which outlives every engine thread.
        let utils = unsafe {
            pw_context_get_object(
                context,
                SPA_TYPE_INTERFACE_ThreadUtils.as_ptr() as *const c_char,
            )
        } as *mut spa_thread_utils;
        CONTEXT_THREAD_UTILS.store(utils, Ordering::Release);
    }

    pub fn promote(priority: c_int) -> Result<(), String> {
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
            // module-rt answers 0 as soon as RTKit accepted the request;
            // whether the kernel really switched the policy is a separate
            // question (measured 2026-09-24: "acquired" in module-rt's log,
            // thread still SCHED_OTHER, nothing in rtkit-daemon's journal).
            // Report what the scheduler says, not what was asked for.
            match libc::sched_getscheduler(0) {
                libc::SCHED_FIFO | libc::SCHED_RR => Ok(()),
                _ => Err(
                    "module-rt reported success, but the thread is still SCHED_OTHER \
                          (RTKit did not apply it)"
                        .into(),
                ),
            }
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

/// Remembers the PipeWire context whose module-rt should promote threads.
/// Called once by the PipeWire backend on its own thread.
#[cfg(target_os = "linux")]
pub fn register_context(context: *mut std::ffi::c_void) {
    imp::register_context(context);
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
            tracing::info!(thread = name, %error,
                "no real-time priority (module-rt/RTKit); running best effort");
            PriorityGuard::default()
        }
    }
}
