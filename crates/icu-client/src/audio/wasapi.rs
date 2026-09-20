//! Windows backend stub. The real WASAPI backend is milestone M3; until then
//! every call reports `Unsupported` so the trait stays honest on Windows.

use super::backend::{
    AudioBackend, BackendError, BackendEvent, CaptureCallback, DeviceInfo, PlaybackCallback,
    StreamKind,
};

/// Placeholder for `IAudioClient3`.
pub struct WasapiBackend;

impl WasapiBackend {
    /// Always fails: not implemented before M3.
    pub fn new() -> Result<WasapiBackend, BackendError> {
        Err(BackendError(
            "Unsupported: WASAPI backend not implemented (M3)".into(),
        ))
    }
}

impl AudioBackend for WasapiBackend {
    fn name(&self) -> &'static str {
        "wasapi"
    }
    fn devices(&self) -> Vec<DeviceInfo> {
        Vec::new()
    }
    fn open_capture(&mut self, _: Option<&str>, _: CaptureCallback) -> Result<(), BackendError> {
        Err(BackendError("Unsupported".into()))
    }
    fn open_reference(&mut self, _: Option<&str>, _: CaptureCallback) -> Result<(), BackendError> {
        Err(BackendError("Unsupported".into()))
    }
    fn open_playback(&mut self, _: Option<&str>, _: PlaybackCallback) -> Result<(), BackendError> {
        Err(BackendError("Unsupported".into()))
    }
    fn close(&mut self, _: StreamKind) {}
    fn take_events(&mut self) -> Option<std::sync::mpsc::Receiver<BackendEvent>> {
        None
    }
}
