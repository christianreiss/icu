//! Host through cpal: the emergency path on Linux (ALSA under PipeWire,
//! feature `cpal-fallback`) and on Windows when the WASAPI backend cannot
//! start (cpal's WASAPI host, shared mode). No AEC reference;
//! the block size is whatever the host delivers (Spike 3: `BufferSize::Fixed`
//! is ignored, set `PIPEWIRE_LATENCY=240/48000` in the environment).

use std::sync::mpsc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::backend::{
    AudioBackend, BackendError, BackendEvent, CaptureCallback, DeviceInfo, DeviceKind,
    PlaybackCallback, StreamKind, SAMPLE_RATE,
};

/// cpal backend.
pub struct CpalBackend {
    host: cpal::Host,
    capture: Option<cpal::Stream>,
    playback: Option<cpal::Stream>,
    events: Option<mpsc::Receiver<BackendEvent>>,
    events_tx: mpsc::Sender<BackendEvent>,
}

impl CpalBackend {
    /// Opens the default host.
    pub fn new() -> Result<CpalBackend, BackendError> {
        #[cfg(target_os = "linux")]
        if std::env::var_os("PIPEWIRE_LATENCY").is_none() {
            // Safety: set before any audio thread exists.
            std::env::set_var("PIPEWIRE_LATENCY", format!("240/{SAMPLE_RATE}"));
        }
        let (tx, rx) = mpsc::channel();
        Ok(CpalBackend {
            host: cpal::default_host(),
            capture: None,
            playback: None,
            events: Some(rx),
            events_tx: tx,
        })
    }

    fn find(&self, id: Option<&str>, kind: DeviceKind) -> Option<cpal::Device> {
        let devices = match kind {
            DeviceKind::Input => self.host.input_devices().ok()?,
            DeviceKind::Output => self.host.output_devices().ok()?,
        };
        match id {
            Some(id) => devices.into_iter().find(|d| d.to_string() == id),
            None => match kind {
                DeviceKind::Input => self.host.default_input_device(),
                DeviceKind::Output => self.host.default_output_device(),
            },
        }
    }
}

impl AudioBackend for CpalBackend {
    fn name(&self) -> &'static str {
        "cpal"
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        let mut out = Vec::new();
        if let Ok(devs) = self.host.input_devices() {
            for d in devs {
                let name = d.to_string();
                out.push(DeviceInfo {
                    id: name.clone(),
                    description: name,
                    kind: DeviceKind::Input,
                });
            }
        }
        if let Ok(devs) = self.host.output_devices() {
            for d in devs {
                let name = d.to_string();
                out.push(DeviceInfo {
                    id: name.clone(),
                    description: name,
                    kind: DeviceKind::Output,
                });
            }
        }
        out
    }

    fn open_capture(
        &mut self,
        device: Option<&str>,
        mut cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        let dev = self
            .find(device, DeviceKind::Input)
            .ok_or("no input device")?;
        let default = dev
            .default_input_config()
            .map_err(|e| BackendError(e.to_string()))?;
        let channels = default.channels() as usize;
        let config = cpal::StreamConfig {
            channels: default.channels(),
            sample_rate: SAMPLE_RATE,
            buffer_size: cpal::BufferSize::Default,
        };
        let mut mono = Vec::new();
        let events = self.events_tx.clone();
        let stream = dev
            .build_input_stream(
                config,
                move |data: &[f32], _| {
                    mono.clear();
                    mono.extend(data.chunks(channels).map(|c| c[0]));
                    cb(&mono);
                },
                move |e| {
                    let _ = events.send(BackendEvent::StreamError {
                        stream: StreamKind::Capture,
                        message: e.to_string(),
                    });
                },
                None,
            )
            .map_err(|e| BackendError(e.to_string()))?;
        stream.play().map_err(|e| BackendError(e.to_string()))?;
        self.capture = Some(stream);
        Ok(())
    }

    fn open_reference(&mut self, _: Option<&str>, _: CaptureCallback) -> Result<(), BackendError> {
        Err(BackendError(
            "Unsupported: cpal has no monitor capture".into(),
        ))
    }

    fn open_playback(
        &mut self,
        device: Option<&str>,
        mut cb: PlaybackCallback,
    ) -> Result<(), BackendError> {
        let dev = self
            .find(device, DeviceKind::Output)
            .ok_or("no output device")?;
        let default = dev
            .default_output_config()
            .map_err(|e| BackendError(e.to_string()))?;
        let channels = default.channels() as usize;
        let config = cpal::StreamConfig {
            channels: default.channels(),
            sample_rate: SAMPLE_RATE,
            buffer_size: cpal::BufferSize::Default,
        };
        let mut mono = Vec::new();
        let voice = super::channels::voice_channels(channels, None);
        let events = self.events_tx.clone();
        let stream = dev
            .build_output_stream(
                config,
                move |data: &mut [f32], _| {
                    let frames = data.len() / channels;
                    mono.clear();
                    mono.resize(frames, 0.0);
                    super::channels::render_voice(&mut mono, data, &voice, &mut cb);
                },
                move |e| {
                    let _ = events.send(BackendEvent::StreamError {
                        stream: StreamKind::Playback,
                        message: e.to_string(),
                    });
                },
                None,
            )
            .map_err(|e| BackendError(e.to_string()))?;
        stream.play().map_err(|e| BackendError(e.to_string()))?;
        self.playback = Some(stream);
        Ok(())
    }

    fn close(&mut self, stream: StreamKind) {
        match stream {
            StreamKind::Capture => self.capture = None,
            StreamKind::Playback => self.playback = None,
            StreamKind::Reference => {}
        }
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<BackendEvent>> {
        self.events.take()
    }
}
