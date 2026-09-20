//! Audio backends and the capture/playback glue.

pub mod backend;
#[cfg(feature = "cpal-fallback")]
pub mod cpal_fallback;
pub mod fake;
#[cfg(target_os = "linux")]
pub mod pipewire;
#[cfg(target_os = "windows")]
pub mod wasapi;

pub use backend::{
    AudioBackend, BackendError, BackendEvent, CaptureCallback, DeviceInfo, DeviceKind,
    PlaybackCallback, StreamKind, BLOCK_SAMPLES, SAMPLE_RATE,
};

/// Opens the platform's native backend.
pub fn native_backend() -> Result<Box<dyn AudioBackend>, BackendError> {
    #[cfg(target_os = "linux")]
    {
        match pipewire::PipeWireBackend::new() {
            Ok(b) => Ok(Box::new(b)),
            Err(e) => {
                #[cfg(feature = "cpal-fallback")]
                {
                    tracing::warn!(error = %e, "pipewire unavailable, using cpal");
                    cpal_fallback::CpalBackend::new().map(|b| Box::new(b) as _)
                }
                #[cfg(not(feature = "cpal-fallback"))]
                Err(e)
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        wasapi::WasapiBackend::new().map(|b| Box::new(b) as _)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(BackendError("no audio backend for this platform".into()))
    }
}
