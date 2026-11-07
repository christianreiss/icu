//! Platform neutral audio backend interface (`docs/reference/client.md` §3).
//!
//! A backend owns device streams and exchanges 48 kHz f32 audio: selected
//! mono microphone input, mono voice playback, and independent native
//! channels for the echo reference. Callbacks run on the backend's own
//! (usually real-time) thread and must be lock free; the engine only pushes
//! into ring buffers there.

use std::{
    fmt,
    time::{Duration, Instant},
};

/// Sample rate every backend delivers and expects.
pub const SAMPLE_RATE: u32 = 48_000;
/// Block length the backends are asked for: 240 samples = 5 ms.
pub const BLOCK_SAMPLES: usize = 240;

/// Which side of the graph a device sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceKind {
    /// Microphone or other capture source.
    Input,
    /// Speaker, headset or other playback sink.
    Output,
}

/// One enumerated device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Backend specific stable id (PipeWire: `node.name`).
    pub id: String,
    /// Human readable description.
    pub description: String,
    /// Input or output.
    pub kind: DeviceKind,
}

/// Something the backend wants the engine to know.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    /// The device list changed (hot-plug). Carries the complete new list.
    DevicesChanged(Vec<DeviceInfo>),
    /// A default endpoint changed. Reopen all streams together so microphone,
    /// playback and echo reference start a new timing epoch.
    DefaultDeviceChanged,
    /// Automatic microphone selection changed on the running stream.
    /// Reset capture/DSP evidence, but keep the stream and selector alive.
    CaptureChannelChanged { channel: u16 },
    /// An established stream lost its capture clock. Reopen the audio group
    /// together instead of combining microphone and reference across the gap.
    StreamDiscontinuity {
        /// Which capture stream detected the gap.
        stream: StreamKind,
    },
    /// A stream ended with an error; the engine may reopen it.
    StreamError {
        /// Which stream.
        stream: StreamKind,
        /// What the backend reported.
        message: String,
    },
}

/// The three streams a backend can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// Microphone capture.
    Capture,
    /// AEC reference: the system mix as played on the active sink.
    Reference,
    /// Voice playback.
    Playback,
}

/// Called with each captured block (mono f32, ±1.0).
pub type CaptureCallback = Box<dyn FnMut(&[f32]) + Send + 'static>;
/// Independent render lanes. Ring occupancies count frames rather than
/// channel samples, preserving the microphone/reference clock relation.
pub const MAX_REFERENCE_CHANNELS: usize = 8;
pub type ReferenceFrame = [f32; MAX_REFERENCE_CHANNELS];
pub type ReferenceCallback = Box<dyn FnMut(&[ReferenceFrame], usize) + Send + 'static>;
/// Called to fill each playback block (mono f32, ±1.0); the slice is zeroed.
pub type PlaybackCallback = Box<dyn FnMut(&mut [f32]) + Send + 'static>;

/// Time of the first capture/presentation sample. `accurate` distinguishes
/// device timing from callback estimates; it does not imply physical AV proof.
#[derive(Clone, Copy, Debug)]
pub struct AudioTiming {
    pub at: Instant,
    pub accurate: bool,
}
impl AudioTiming {
    pub fn shifted(self, samples: usize) -> Self {
        Self {
            at: self.at + Duration::from_secs_f64(samples as f64 / 48_000.0),
            ..self
        }
    }
    #[cfg(target_os = "linux")]
    pub fn monotonic_ns(ns: i64) -> Option<Self> {
        let at = Instant::now();
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: valid writable timespec; CLOCK_MONOTONIC matches PipeWire.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
            return None;
        }
        let current = i128::from(ts.tv_sec) * 1_000_000_000 + i128::from(ts.tv_nsec);
        let delta = i128::from(ns) - current;
        if delta.abs() > 1_000_000_000 {
            return None;
        }
        let duration = Duration::from_nanos(delta.unsigned_abs() as u64);
        let at = if delta < 0 {
            at.checked_sub(duration)?
        } else {
            at.checked_add(duration)?
        };
        Some(Self { at, accurate: true })
    }
    pub fn estimated_capture(samples: usize) -> Self {
        Self {
            at: Instant::now() - Duration::from_secs_f64(samples as f64 / 48_000.0),
            accurate: false,
        }
    }
    pub fn estimated_playback() -> Self {
        Self {
            at: Instant::now(),
            accurate: false,
        }
    }
}
pub type TimedCaptureCallback = Box<dyn FnMut(&[f32], AudioTiming) + Send + 'static>;
pub type TimedPlaybackCallback = Box<dyn FnMut(&mut [f32], AudioTiming) + Send + 'static>;

/// Backend failure.
#[derive(Debug, Clone)]
pub struct BackendError(pub String);

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BackendError {}

impl From<String> for BackendError {
    fn from(s: String) -> Self {
        BackendError(s)
    }
}

impl From<&str> for BackendError {
    fn from(s: &str) -> Self {
        BackendError(s.to_string())
    }
}

/// The backend contract. Implemented by PipeWire (Linux), the cpal fallback,
/// the test double and WASAPI (Windows).
pub trait AudioBackend: Send {
    /// Short backend name for the UI ("pipewire", "cpal", "fake").
    fn name(&self) -> &'static str;

    /// Current device list.
    fn devices(&self) -> Vec<DeviceInfo>;

    /// Set before opening capture; explicit indices are zero based.
    fn set_capture_channel(&mut self, channel: Option<u16>) -> Result<(), BackendError> {
        if channel.is_some_and(|channel| channel != 0) {
            return Err(BackendError(
                "Mikrofonkanal wird von diesem Backend nicht unterstützt".into(),
            ));
        }
        Ok(())
    }
    /// Actual selected channel, when known; part of calibration identity.
    fn capture_channel_index(&self) -> Option<u16> {
        None
    }

    /// Opens the microphone stream. `device` is a [`DeviceInfo::id`] or
    /// `None` for the default.
    fn open_capture(
        &mut self,
        device: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError>;

    /// Timing-aware capture; portable backends explicitly report an estimate.
    fn open_capture_timed(
        &mut self,
        device: Option<&str>,
        mut cb: TimedCaptureCallback,
    ) -> Result<(), BackendError> {
        self.open_capture(
            device,
            Box::new(move |samples| cb(samples, AudioTiming::estimated_capture(samples.len()))),
        )
    }
    /// Timing-aware playback; native implementations include device buffering.
    fn open_playback_timed(
        &mut self,
        device: Option<&str>,
        mut cb: TimedPlaybackCallback,
    ) -> Result<(), BackendError> {
        self.open_playback(
            device,
            Box::new(move |samples| cb(samples, AudioTiming::estimated_playback())),
        )
    }

    /// Opens the AEC reference stream on the active output sink's monitor.
    /// `None` follows the default sink.
    fn open_reference(
        &mut self,
        sink: Option<&str>,
        cb: ReferenceCallback,
    ) -> Result<(), BackendError>;

    /// Opens the playback stream.
    fn open_playback(
        &mut self,
        device: Option<&str>,
        cb: PlaybackCallback,
    ) -> Result<(), BackendError>;

    /// Closes one stream. Closing a stream that is not open is a no-op.
    fn close(&mut self, stream: StreamKind);

    /// Takes the event receiver. Only the first call returns `Some`.
    fn take_events(&mut self) -> Option<std::sync::mpsc::Receiver<BackendEvent>>;

    /// Snapshot of the actual open capture device; unsupported on fallback hosts.
    fn capture_level(&mut self) -> Result<Option<super::level::CaptureLevel>, BackendError> {
        Ok(None)
    }
    /// Compare the current native value before writing; acknowledge with readback.
    fn set_capture_level(
        &mut self,
        _expected: &super::level::CaptureLevel,
        _db: f32,
    ) -> Result<super::level::CaptureLevel, BackendError> {
        Err(super::level::unsupported())
    }
    /// Restore the original native channel values if ICU still owns the level.
    fn restore_capture_level(
        &mut self,
        expected: &super::level::CaptureLevel,
        original: &super::level::CaptureLevel,
    ) -> Result<(), BackendError> {
        self.set_capture_level(expected, original.db).map(|_| ())
    }

    /// How late the AEC reference may arrive behind the microphone before
    /// the engine counts it as missing. Zero where both run in one graph
    /// cycle (PipeWire); a backend with free-running streams names its
    /// scheduling jitter.
    fn reference_slack(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}
