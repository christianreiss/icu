//! Platform neutral audio backend interface (`Client/IMPLEMENTATION.md` §3).
//!
//! A backend owns the device streams and calls back with mono f32 blocks at
//! 48 kHz. The callbacks run on the backend's own (usually real-time) thread
//! and must be lock free; the engine only pushes into ring buffers there.

use std::fmt;

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
/// Called to fill each playback block (mono f32, ±1.0); the slice is zeroed.
pub type PlaybackCallback = Box<dyn FnMut(&mut [f32]) + Send + 'static>;

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
/// the test double and — as `Unsupported` stubs — Windows.
pub trait AudioBackend: Send {
    /// Short backend name for the UI ("pipewire", "cpal", "fake").
    fn name(&self) -> &'static str;

    /// Current device list.
    fn devices(&self) -> Vec<DeviceInfo>;

    /// Opens the microphone stream. `device` is a [`DeviceInfo::id`] or
    /// `None` for the default.
    fn open_capture(
        &mut self,
        device: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError>;

    /// Opens the AEC reference stream on the active output sink's monitor.
    /// `None` follows the default sink.
    fn open_reference(
        &mut self,
        sink: Option<&str>,
        cb: CaptureCallback,
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
}
