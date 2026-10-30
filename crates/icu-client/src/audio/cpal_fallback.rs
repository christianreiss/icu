//! Host through cpal: the emergency path on Linux (ALSA under PipeWire,
//! feature `cpal-fallback`) and on Windows when the WASAPI backend cannot
//! start (cpal's WASAPI host, shared mode). No AEC reference;
//! the block size is whatever the host delivers (Spike 3: `BufferSize::Fixed`
//! is ignored, set `PIPEWIRE_LATENCY=240/48000` in the environment).
//! Native integer/float samples are converted through bounded scratch;
//! non-48 kHz devices use a streaming windowed-sinc resampler. Input selects
//! one native microphone channel, while playback maps mono voice to front
//! speakers without changing endpoint volume or defaults.

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU16, Ordering},
        mpsc, Arc,
    },
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::backend::{
    AudioBackend, BackendError, BackendEvent, CaptureCallback, DeviceInfo, DeviceKind,
    PlaybackCallback, ReferenceCallback, StreamKind, SAMPLE_RATE,
};

/// cpal backend.
pub struct CpalBackend {
    host: cpal::Host,
    capture: Option<cpal::Stream>,
    playback: Option<cpal::Stream>,
    events: Option<mpsc::Receiver<BackendEvent>>,
    events_tx: mpsc::Sender<BackendEvent>,
    capture_channel: Option<u16>,
    selected_channel: Arc<AtomicU16>,
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
            capture_channel: None,
            selected_channel: Arc::new(AtomicU16::new(u16::MAX)),
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

// Format conversion follows CPAL's actual supported sample format instead
// of passing an f32 callback to integer-only devices.
macro_rules! typed_stream {
    ($format:expr, $build:ident, $($arg:expr),+ $(,)?) => {
        match $format {
            cpal::SampleFormat::F32 => $build::<f32>($($arg),+),
            cpal::SampleFormat::F64 => $build::<f64>($($arg),+),
            cpal::SampleFormat::I8 => $build::<i8>($($arg),+),
            cpal::SampleFormat::I16 => $build::<i16>($($arg),+),
            cpal::SampleFormat::I24 => $build::<cpal::I24>($($arg),+),
            cpal::SampleFormat::I32 => $build::<i32>($($arg),+),
            cpal::SampleFormat::I64 => $build::<i64>($($arg),+),
            cpal::SampleFormat::U8 => $build::<u8>($($arg),+),
            cpal::SampleFormat::U16 => $build::<u16>($($arg),+),
            cpal::SampleFormat::U24 => $build::<cpal::U24>($($arg),+),
            cpal::SampleFormat::U32 => $build::<u32>($($arg),+),
            cpal::SampleFormat::U64 => $build::<u64>($($arg),+),
            other => Err(BackendError(format!("Unsupported PCM format: {other:?}"))),
        }
    };
}

fn choose_config(
    device: &cpal::Device,
    input: bool,
) -> Result<cpal::SupportedStreamConfig, BackendError> {
    let default = if input {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .map_err(|e| BackendError(e.to_string()))?;
    let supported = if input {
        device
            .supported_input_configs()
            .map(|c| c.collect::<Vec<_>>())
    } else {
        device
            .supported_output_configs()
            .map(|c| c.collect::<Vec<_>>())
    };
    let chosen = supported
        .ok()
        .and_then(|configs| {
            configs
                .into_iter()
                .filter(|c| {
                    c.channels() == default.channels()
                        && c.min_sample_rate() <= SAMPLE_RATE
                        && c.max_sample_rate() >= SAMPLE_RATE
                })
                .min_by_key(|c| match c.sample_format() {
                    cpal::SampleFormat::F32 => 0,
                    cpal::SampleFormat::I16 => 1,
                    cpal::SampleFormat::U16 => 2,
                    _ => 3,
                })
                .map(|c| c.with_sample_rate(SAMPLE_RATE))
        })
        .unwrap_or(default);
    if !(4_000..=384_000).contains(&chosen.sample_rate()) {
        return Err(BackendError(format!(
            "Unsupported sample rate: {}",
            chosen.sample_rate()
        )));
    }
    Ok(chosen)
}

fn capture_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut callback: CaptureCallback,
    mut selector: super::channels::CaptureSelector,
    selected: Arc<AtomicU16>,
    events: mpsc::Sender<BackendEvent>,
) -> Result<cpal::Stream, BackendError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels as usize;
    let mut converter = super::resample::Resampler::new(config.sample_rate, SAMPLE_RATE);
    let mut interleaved = vec![0.0; 1024 * channels];
    let mut output = vec![0.0; 8192];
    let errors = events.clone();
    device
        .build_input_stream(
            config,
            move |samples: &[T], _| {
                let mut count = 0;
                for block in samples.chunks(1024 * channels) {
                    let len = block.len() / channels * channels;
                    for (dest, source) in interleaved[..len].iter_mut().zip(block) {
                        *dest = source.to_sample::<f32>();
                    }
                    if selector.update(&interleaved[..len], &selected, &events) {
                        // Samples from different microphones must not share
                        // the resampler's history or a pending callback.
                        converter.reset_history();
                        count = 0;
                    }
                    for frame in interleaved[..len].chunks_exact(channels) {
                        converter.push(frame[selector.channel()], |sample| {
                            output[count] = sample;
                            count += 1;
                            if count == output.len() {
                                callback(&output);
                                count = 0;
                            }
                        });
                    }
                }
                if count > 0 {
                    callback(&output[..count]);
                }
            },
            move |error| {
                let _ = errors.send(BackendEvent::StreamError {
                    stream: StreamKind::Capture,
                    message: error.to_string(),
                });
            },
            None,
        )
        .map_err(|e| BackendError(e.to_string()))
}

fn playback_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut callback: PlaybackCallback,
    events: mpsc::Sender<BackendEvent>,
) -> Result<cpal::Stream, BackendError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let voice = super::channels::voice_channels(channels, None);
    let mut converter = super::resample::Resampler::new(SAMPLE_RATE, rate);
    let mut pending = VecDeque::with_capacity(128);
    let mut mono = vec![0.0f32; 8192];
    let (mut read, mut count) = (0, 0);
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                let frames = out.len() / channels;
                for (index, frame) in out.chunks_exact_mut(channels).enumerate() {
                    while pending.is_empty() {
                        if read == count {
                            count = ((frames - index) as u64 * u64::from(SAMPLE_RATE))
                                .div_ceil(u64::from(rate))
                                .clamp(1, mono.len() as u64)
                                as usize;
                            mono[..count].fill(0.0);
                            callback(&mut mono[..count]);
                            read = 0;
                        }
                        converter.push(mono[read], |sample| pending.push_back(sample));
                        read += 1;
                    }
                    let sample = pending.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
                    for (dest, enabled) in frame.iter_mut().zip(&voice) {
                        *dest = T::from_sample(if *enabled { sample } else { 0.0 });
                    }
                }
            },
            move |error| {
                let _ = events.send(BackendEvent::StreamError {
                    stream: StreamKind::Playback,
                    message: error.to_string(),
                });
            },
            None,
        )
        .map_err(|e| e.to_string().into())
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

    fn set_capture_channel(&mut self, channel: Option<u16>) -> Result<(), BackendError> {
        self.capture_channel = channel;
        Ok(())
    }
    fn capture_channel_index(&self) -> Option<u16> {
        let channel = self.selected_channel.load(Ordering::Relaxed);
        (channel != u16::MAX).then_some(channel)
    }
    fn open_capture(
        &mut self,
        device: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        let device = self
            .find(device, DeviceKind::Input)
            .ok_or("no input device")?;
        let chosen = choose_config(&device, true)?;
        let format = chosen.sample_format();
        let config = chosen.config();
        let selector =
            super::channels::CaptureSelector::new(self.capture_channel, config.channels as usize)?;
        self.selected_channel.store(u16::MAX, Ordering::Relaxed);
        let stream = typed_stream!(
            format,
            capture_stream,
            &device,
            config,
            cb,
            selector,
            self.selected_channel.clone(),
            self.events_tx.clone()
        )?;
        stream.play().map_err(|e| BackendError(e.to_string()))?;
        self.capture = Some(stream);
        Ok(())
    }
    fn open_reference(
        &mut self,
        _: Option<&str>,
        _: ReferenceCallback,
    ) -> Result<(), BackendError> {
        Err(BackendError(
            "Unsupported: cpal has no monitor capture".into(),
        ))
    }
    fn open_playback(
        &mut self,
        device: Option<&str>,
        cb: PlaybackCallback,
    ) -> Result<(), BackendError> {
        let device = self
            .find(device, DeviceKind::Output)
            .ok_or("no output device")?;
        let chosen = choose_config(&device, false)?;
        let format = chosen.sample_format();
        let config = chosen.config();
        if config.channels == 0 || config.channels > 64 {
            return Err(BackendError("Unsupported output channel count".into()));
        }
        let stream = typed_stream!(
            format,
            playback_stream,
            &device,
            config,
            cb,
            self.events_tx.clone()
        )?;
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
