//! Camera capture (nokhwa) and H.264 encoding (openh264) on their own
//! thread (`Client/IMPLEMENTATION.md` §8). 320×240 @ 15 fps, 150 kbit/s,
//! IDR every 2 s and on request.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use openh264::{
    encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, RateControlMode, UsageType},
    formats::{RgbSliceU8, YUVBuffer},
    OpenH264API,
};

use crate::engine::{AppState, CameraLink};

/// Target width.
pub const WIDTH: u32 = 320;
/// Target height.
pub const HEIGHT: u32 = 240;
/// Target frame rate.
pub const FPS: u32 = 15;
/// Target bitrate.
pub const BITRATE: u32 = 150_000;
/// IDR interval.
pub const IDR_INTERVAL: Duration = Duration::from_secs(2);

/// One RGB8 picture.
#[derive(Debug, Clone)]
pub struct RgbFrame {
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
    /// RGB8, row major.
    pub rgb: Vec<u8>,
}

/// A source of pictures at roughly [`FPS`].
pub trait FrameSource: Send {
    /// Blocks until the next picture.
    fn next_frame(&mut self) -> Result<RgbFrame, String>;
}

/// Lists cameras as `(index, name)`.
pub fn list() -> Vec<(u32, String)> {
    nokhwa::query(nokhwa::utils::ApiBackend::Auto)
        .map(|cams| {
            cams.iter()
                .filter_map(|c| match c.index() {
                    nokhwa::utils::CameraIndex::Index(i) => Some((*i, c.human_name())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// nokhwa V4L2/MSMF source.
pub struct NokhwaSource {
    camera: nokhwa::Camera,
}

impl NokhwaSource {
    /// Opens camera `index` (Spike 5: pick the capture node explicitly) at
    /// the closest format to 320×240 MJPEG 15 fps.
    pub fn open(index: u32) -> Result<NokhwaSource, String> {
        use nokhwa::{
            pixel_format::RgbFormat,
            utils::{
                CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
                Resolution,
            },
        };
        let want = CameraFormat::new(Resolution::new(WIDTH, HEIGHT), FrameFormat::MJPEG, FPS);
        let req = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(want));
        let mut camera =
            nokhwa::Camera::new(CameraIndex::Index(index), req).map_err(|e| e.to_string())?;
        camera.open_stream().map_err(|e| e.to_string())?;
        tracing::info!(format = ?camera.camera_format(), "camera opened");
        Ok(NokhwaSource { camera })
    }
}

impl FrameSource for NokhwaSource {
    fn next_frame(&mut self) -> Result<RgbFrame, String> {
        use nokhwa::pixel_format::RgbFormat;
        let buffer = self.camera.frame().map_err(|e| e.to_string())?;
        let res = buffer.resolution();
        let img = buffer
            .decode_image::<RgbFormat>()
            .map_err(|e| e.to_string())?;
        Ok(RgbFrame {
            width: res.width() as usize,
            height: res.height() as usize,
            rgb: img.into_raw(),
        })
    }
}

/// A moving test pattern for tests and `--fake-camera`.
pub struct SyntheticSource {
    tick: u32,
    start: Instant,
}

impl Default for SyntheticSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntheticSource {
    /// New pattern generator.
    pub fn new() -> SyntheticSource {
        SyntheticSource {
            tick: 0,
            start: Instant::now(),
        }
    }
}

impl FrameSource for SyntheticSource {
    fn next_frame(&mut self) -> Result<RgbFrame, String> {
        self.tick += 1;
        let deadline = self.start + Duration::from_millis(1000 / FPS as u64) * self.tick;
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let mut rgb = vec![0u8; w * h * 3];
        let phase = self.tick as usize % w;
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                rgb[i] = ((x + phase) % 256) as u8;
                rgb[i + 1] = (y % 256) as u8;
                rgb[i + 2] = (((x + phase) * y) / 64 % 256) as u8;
            }
        }
        Ok(RgbFrame {
            width: w,
            height: h,
            rgb,
        })
    }
}

/// The v1 encoder configuration.
pub fn encoder() -> Result<Encoder, String> {
    let cfg = EncoderConfig::new()
        .bitrate(BitRate::from_bps(BITRATE))
        .max_frame_rate(FrameRate::from_hz(FPS as f32))
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate);
    Encoder::with_api_config(OpenH264API::from_source(), cfg).map_err(|e| e.to_string())
}

/// Encoder thread loop: capture → encode → send. `preview` receives every
/// captured picture for the settings preview.
pub fn run_sender(
    mut source: Box<dyn FrameSource>,
    link: CameraLink,
    stop: Arc<AtomicBool>,
    preview: Arc<Mutex<Option<RgbFrame>>>,
) {
    let mut enc = match encoder() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(%e, "openh264 encoder");
            return;
        }
    };
    let start = Instant::now();
    let mut last_idr = Instant::now() - IDR_INTERVAL;
    let mut yuv: Option<YUVBuffer> = None;
    while !stop.load(Ordering::Relaxed) {
        let frame = match source.next_frame() {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(%e, "camera frame");
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        *preview.lock().unwrap() = Some(frame.clone());
        let src = RgbSliceU8::new(&frame.rgb, (frame.width, frame.height));
        match yuv.as_mut() {
            Some(buf) if buf.dimensions_match(frame.width, frame.height) => buf.read_rgb(src),
            _ => yuv = Some(YUVBuffer::from_rgb_source(src)),
        }
        let Some(buf) = yuv.as_ref() else { continue };
        let want_idr = link.take_keyframe_request() || last_idr.elapsed() >= IDR_INTERVAL;
        if want_idr {
            enc.force_intra_frame();
        }
        let ts = (start.elapsed().as_secs_f64() * 48_000.0) as u64 as u32;
        match enc.encode(buf) {
            Ok(bs) => {
                let keyframe = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
                if keyframe {
                    last_idr = Instant::now();
                }
                let data = bs.to_vec();
                if data.is_empty() {
                    continue;
                }
                if let Err(e) = link.send_frame(keyframe, ts, &data) {
                    tracing::debug!(%e, "video frame not sent");
                }
            }
            Err(e) => tracing::warn!(%e, "h264 encode"),
        }
    }
}

trait DimsMatch {
    fn dimensions_match(&self, w: usize, h: usize) -> bool;
}

impl DimsMatch for YUVBuffer {
    fn dimensions_match(&self, w: usize, h: usize) -> bool {
        use openh264::formats::YUVSource;
        self.dimensions() == (w, h)
    }
}

/// Owns the camera thread; started/stopped from the UI according to
/// `AppState::camera_on`.
pub struct CameraHandle {
    link: CameraLink,
    thread: Option<(JoinHandle<()>, Arc<AtomicBool>)>,
    preview: Arc<Mutex<Option<RgbFrame>>>,
    #[cfg(feature = "gui")]
    texture: Option<egui::TextureHandle>,
    device: Option<String>,
}

impl CameraHandle {
    /// New, camera off.
    pub fn new(link: CameraLink) -> CameraHandle {
        CameraHandle {
            link,
            thread: None,
            preview: Arc::new(Mutex::new(None)),
            #[cfg(feature = "gui")]
            texture: None,
            device: None,
        }
    }

    /// Starts with an explicit source (tests).
    pub fn start_with(&mut self, source: Box<dyn FrameSource>) {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        let link = self.link.clone();
        let preview = self.preview.clone();
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("icu-camera".into())
            .spawn(move || run_sender(source, link, stop_t, preview))
            .expect("camera thread");
        self.thread = Some((thread, stop));
    }

    /// Starts the real camera; `device` is an index (default 0) or
    /// `synthetic`.
    pub fn start(&mut self, device: Option<&str>) -> Result<(), String> {
        let source: Box<dyn FrameSource> = match device {
            Some("synthetic") => Box::new(SyntheticSource::new()),
            Some(idx) => Box::new(NokhwaSource::open(
                idx.parse()
                    .map_err(|_| "camera device must be an index".to_string())?,
            )?),
            None => Box::new(NokhwaSource::open(0)?),
        };
        self.start_with(source);
        Ok(())
    }

    /// Stops the thread.
    pub fn stop(&mut self) {
        if let Some((thread, stop)) = self.thread.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
        }
        *self.preview.lock().unwrap() = None;
    }

    /// Whether the thread runs.
    pub fn running(&self) -> bool {
        self.thread.is_some()
    }

    /// Follows the UI state.
    pub fn sync(&mut self, state: &AppState) {
        let device = state.settings.camera.device.clone();
        if state.camera_on && (!self.running() || device != self.device) {
            self.device = device.clone();
            if let Err(e) = self.start(device.as_deref()) {
                tracing::warn!(%e, "camera");
            }
        } else if !state.camera_on && self.running() {
            self.stop();
        }
    }

    /// Latest captured picture.
    pub fn latest(&self) -> Option<RgbFrame> {
        self.preview.lock().unwrap().clone()
    }

    /// Draws the preview.
    #[cfg(feature = "gui")]
    pub fn preview(&mut self, ui: &mut egui::Ui) {
        if let Some(frame) = self.latest() {
            let image = egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb);
            let tex = self.texture.get_or_insert_with(|| {
                ui.ctx().load_texture(
                    "camera-preview",
                    image.clone(),
                    egui::TextureOptions::LINEAR,
                )
            });
            tex.set(image, egui::TextureOptions::LINEAR);
            ui.add(egui::Image::new((tex.id(), egui::vec2(160.0, 120.0))).corner_radius(4.0));
        }
    }
}

impl Drop for CameraHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder produces an IDR on `force_intra_frame` and SPS/PPS come
    /// with it.
    #[test]
    fn encoder_emits_idr_on_request() {
        let mut enc = encoder().unwrap();
        let mut src = SyntheticSource::new();
        let mut types = Vec::new();
        for i in 0..8 {
            let f = src.next_frame().unwrap();
            let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&f.rgb, (f.width, f.height)));
            if i == 5 {
                enc.force_intra_frame();
            }
            let bs = enc.encode(&yuv).unwrap();
            types.push((bs.frame_type(), bs.to_vec().len()));
        }
        assert!(
            matches!(types[0].0, FrameType::IDR),
            "first frame is an IDR"
        );
        assert!(
            matches!(types[5].0, FrameType::IDR),
            "forced IDR at 5: {:?}",
            types
        );
        assert!(matches!(types[1].0, FrameType::P));
        assert!(types.iter().all(|(_, n)| *n > 0));
    }
}
