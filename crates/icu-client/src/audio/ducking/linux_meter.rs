//! Passive per-application output monitor. The RT callback only accumulates
//! channel energy; raw audio is never retained, serialized or sent elsewhere.
use pipewire::{self as pw, properties::properties, spa};
use std::{
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct Shared {
    energy: AtomicU32,
    frames: AtomicU64,
}
struct Data {
    shared: Arc<Shared>,
    channels: usize,
}

pub(super) struct Meter {
    _listener: pw::stream::StreamListener<Data>,
    stream: pw::stream::StreamRc,
    shared: Arc<Shared>,
    seen: u64,
    at: Instant,
    settle_until: Option<Instant>,
    after_frames: u64,
}

impl Meter {
    pub fn new(core: pw::core::CoreRc, serial: u64) -> Result<Self, String> {
        let target = serial.to_string();
        let props = properties! {
            *pw::keys::APP_NAME => "ICU",
            *pw::keys::NODE_NAME => "icu-ducking-meter",
            *pw::keys::NODE_GROUP => "icu",
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::TARGET_OBJECT => target.as_str(),
            *pw::keys::STREAM_CAPTURE_SINK => "true",
            "node.passive" => "true",
            "node.dont-reconnect" => "true",
            "node.dont-fallback" => "true",
            "stream.dont-remix" => "true",
        };
        let stream = pw::stream::StreamRc::new(core, "icu-ducking-meter", props)
            .map_err(|e| e.to_string())?;
        let shared = Arc::new(Shared::default());
        let data = Data {
            shared: shared.clone(),
            channels: 0,
        };
        let listener = stream
            .add_local_listener_with_user_data(data)
            .param_changed(|_, data, id, pod| {
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                let Some(pod) = pod else {
                    data.channels = 0;
                    return;
                };
                let mut format = spa::param::audio::AudioInfoRaw::new();
                data.channels = if format.parse(pod).is_ok()
                    && format.rate() == 48000
                    && (1..=64).contains(&format.channels())
                {
                    format.channels() as usize
                } else {
                    0
                };
            })
            .process(|stream, data| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(plane) = buffer.datas_mut().first_mut() else {
                    return;
                };
                if data.channels == 0 {
                    return;
                }
                let offset = plane.chunk().offset() as usize;
                let size = plane.chunk().size() as usize;
                let Some(bytes) = plane.data() else {
                    return;
                };
                let start = offset.min(bytes.len());
                let size = size.min(bytes.len() - start).min(65536 * 4);
                let mut sum = 0.0f64;
                let mut count = 0u64;
                for bytes in bytes[start..start + size].as_chunks::<4>().0 {
                    let sample = f32::from_le_bytes(*bytes);
                    if !sample.is_finite() {
                        return;
                    }
                    sum += (sample as f64).powi(2);
                    count += 1;
                }
                if count > 0 {
                    data.shared.energy.store(
                        ((sum / count as f64) as f32).clamp(1e-16, 16.0).to_bits(),
                        Ordering::Relaxed,
                    );
                    data.shared
                        .frames
                        .fetch_add(count / data.channels as u64, Ordering::Release);
                }
            })
            .register()
            .map_err(|e| e.to_string())?;
        let mut format = spa::param::audio::AudioInfoRaw::new();
        format.set_format(spa::param::audio::AudioFormat::F32LE);
        format.set_rate(48000);
        let value = spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: format.into(),
        });
        let bytes =
            spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &value)
                .map_err(|e| e.to_string())?
                .0
                .into_inner();
        let mut params = [spa::pod::Pod::from_bytes(&bytes).ok_or("Ungültiges Monitorformat")?];
        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .map_err(|e| e.to_string())?;
        Ok(Self {
            _listener: listener,
            stream,
            shared,
            seen: 0,
            at: Instant::now(),
            settle_until: None,
            after_frames: 0,
        })
    }

    pub fn invalidate(&mut self, now: Instant) {
        self.settle_until = Some(now + Duration::from_millis(60));
    }

    pub fn energy(&mut self, now: Instant) -> Option<f32> {
        let frames = self.shared.frames.load(Ordering::Acquire);
        if let Some(until) = self.settle_until {
            if now < until {
                return None;
            }
            self.after_frames = frames;
            self.settle_until = None;
            return None;
        }
        if frames <= self.after_frames {
            return None;
        }
        if frames != self.seen {
            self.seen = frames;
            self.at = now;
        }
        (frames > 0 && now.saturating_duration_since(self.at) <= Duration::from_millis(250))
            .then(|| f32::from_bits(self.shared.energy.load(Ordering::Relaxed)))
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
    }
}
