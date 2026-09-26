//! Camera capture (nokhwa) and H.264 encoding (openh264) on their own
//! thread (`Client/IMPLEMENTATION.md` §8). 320×240 capture at 15 fps; the
//! encoded bitrate and effective frame rate follow the network-adaptive
//! [`crate::video::VideoTier`] (`crate::quality::QualityController`), IDR
//! every 2 s and on request.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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

use crate::{
    engine::{AppState, CameraLink},
    video::VideoTier,
};

/// Capture width. Fixed: re-opening the camera device per quality-tier
/// change would be far more disruptive than an encoder bitrate/fps change.
pub const WIDTH: u32 = 320;
/// Capture height.
pub const HEIGHT: u32 = 240;
/// Capture frame rate. The encoder thins this down per `VideoTier`.
pub const FPS: u32 = 15;
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

impl RgbFrame {
    /// A picture the encoder and the preview can take. `rgb` has to hold
    /// exactly `width × height` pixels; an odd side loses its last column
    /// or row, because openh264 encodes 4:2:0 and wants even sides. Anything
    /// else is an error, never a panic: openh264's `RgbSliceU8::new` and
    /// egui's `ColorImage::from_rgb` assert on the length, and a release
    /// build aborts on a panic — one odd frame from a camera (an MJPEG frame
    /// larger than negotiated, a YUYV row stride with padding, a virtual
    /// camera's odd size) would end the whole client.
    pub fn checked(width: usize, height: usize, mut rgb: Vec<u8>) -> Result<RgbFrame, String> {
        if width.checked_mul(height).and_then(|n| n.checked_mul(3)) != Some(rgb.len()) {
            return Err(format!(
                "camera frame of {} bytes does not hold {width}×{height} RGB pixels",
                rgb.len()
            ));
        }
        let (w, h) = (width & !1, height & !1);
        if w == 0 || h == 0 {
            return Err(format!("camera frame too small: {width}×{height}"));
        }
        if w != width {
            // Row by row towards the front: a row never moves past its old
            // start, so nothing is overwritten before it is copied.
            for y in 0..h {
                rgb.copy_within(y * width * 3..(y * width + w) * 3, y * w * 3);
            }
        }
        rgb.truncate(w * h * 3);
        Ok(RgbFrame {
            width: w,
            height: h,
            rgb,
        })
    }

    /// Whether the encoder and the preview can take this picture as it is:
    /// even, non-zero sides and exactly the bytes they promise. Checked once
    /// more right before the asserting constructors, because any
    /// [`FrameSource`] can hand in a picture.
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width.is_multiple_of(2)
            && self.height.is_multiple_of(2)
            && self
                .width
                .checked_mul(self.height)
                .and_then(|n| n.checked_mul(3))
                == Some(self.rgb.len())
    }
}

/// Width and height from a JPEG's frame header (SOF*n*), `None` without one.
/// nokhwa labels every decoded picture with the negotiated size, but decodes
/// an MJPEG frame at the size the frame itself says.
fn jpeg_size(data: &[u8]) -> Option<(usize, usize)> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    loop {
        if *data.get(i)? != 0xFF {
            return None;
        }
        let marker = *data.get(i + 1)?;
        match marker {
            // Fill byte before a marker.
            0xFF => {
                i += 1;
                continue;
            }
            // Markers without a length.
            0x01 | 0xD0..=0xD8 => {
                i += 2;
                continue;
            }
            // End of image, or the scan starts: no frame header before it.
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]));
        // SOF0–SOF15, except DHT (C4), JPG (C8) and DAC (CC): length,
        // precision, height, width.
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            let height = u16::from_be_bytes([*data.get(i + 5)?, *data.get(i + 6)?]);
            let width = u16::from_be_bytes([*data.get(i + 7)?, *data.get(i + 8)?]);
            return Some((usize::from(width), usize::from(height)));
        }
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
}

/// The capture thread's latest picture. `seq` counts frames, so a viewer
/// can tell a new picture from a repaint of the one it already uploaded —
/// the preview is drawn at the window's frame rate, the camera delivers at
/// [`FPS`], and without this two thirds of the texture uploads would be the
/// same bytes again.
#[derive(Default)]
pub struct PreviewSlot {
    /// The picture itself, `None` until the camera delivers one. Shared
    /// with the encoder's copy: publishing it costs no copy.
    pub frame: Mutex<Option<Arc<RgbFrame>>>,
    /// Incremented once per captured picture.
    pub seq: AtomicU64,
}

impl PreviewSlot {
    /// Publishes a picture to whoever is watching.
    pub fn put(&self, frame: Arc<RgbFrame>) {
        *self.frame.lock().unwrap() = Some(frame);
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// Forgets the last picture, so a stopped camera doesn't leave a stale
    /// still frame on screen.
    pub fn clear(&self) {
        *self.frame.lock().unwrap() = None;
        self.seq.fetch_add(1, Ordering::Release);
    }
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
        use nokhwa::{pixel_format::RgbFormat, utils::FrameFormat};
        let buffer = self.camera.frame().map_err(|e| e.to_string())?;
        let img = buffer
            .decode_image::<RgbFormat>()
            .map_err(|e| e.to_string())?;
        let (mut width, mut height) = (img.width() as usize, img.height() as usize);
        // `into_raw` hands back the whole decoded buffer, which is longer than
        // the image's (negotiated) size when an MJPEG frame is larger: take
        // the frame's own size when it explains the byte count.
        let rgb = img.into_raw();
        if buffer.source_frame_format() == FrameFormat::MJPEG
            && width.saturating_mul(height).saturating_mul(3) != rgb.len()
        {
            if let Some((w, h)) = jpeg_size(buffer.buffer()) {
                if w.saturating_mul(h).saturating_mul(3) == rgb.len() {
                    (width, height) = (w, h);
                }
            }
        }
        RgbFrame::checked(width, height, rgb)
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

/// The encoder configuration for one [`VideoTier`]. Never called for `T4`
/// (paused): `run_sender` simply stops encoding instead of building an
/// encoder with a zero bitrate.
pub fn encoder(tier: VideoTier) -> Result<Encoder, String> {
    let cfg = EncoderConfig::new()
        .bitrate(BitRate::from_bps(tier.bitrate()))
        .max_frame_rate(FrameRate::from_hz(FPS as f32))
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate);
    Encoder::with_api_config(OpenH264API::from_source(), cfg).map_err(|e| e.to_string())
}

/// Encoder thread loop: capture → encode → send. `preview` receives every
/// captured picture for the settings preview, regardless of tier, so the
/// settings preview keeps working even while video is paused for everyone
/// else.
///
/// The encoder is rebuilt whenever `link.send_tier()` changes: openh264's
/// safe Rust binding exposes no runtime bitrate mutator, so a tier change
/// means a fresh `Encoder`. That is cheap relative to how rarely
/// `QualityController` actually steps (bounded by its own hysteresis), and a
/// freshly built encoder's first frame is an IDR regardless of
/// `force_intra_frame` (`encoder_emits_idr_on_request` below), so SPS/PPS
/// ride along for free — no separate flush logic is needed.
pub fn run_sender(
    mut source: Box<dyn FrameSource>,
    link: CameraLink,
    stop: Arc<AtomicBool>,
    preview: Arc<PreviewSlot>,
) {
    // No encoder while paused: `run_sender` stops encoding rather than
    // building one with a zero bitrate.
    let build = |tier: VideoTier| -> Result<Option<Encoder>, String> {
        if tier.paused() {
            Ok(None)
        } else {
            encoder(tier).map(Some)
        }
    };
    let mut tier = link.send_tier();
    let mut enc = match build(tier) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(%e, "openh264 encoder");
            return;
        }
    };
    let start = Instant::now();
    let mut last_idr = Instant::now() - IDR_INTERVAL;
    let mut yuv: Option<YUVBuffer> = None;
    let mut frame_index: u32 = 0;
    // Failed frames in a row: the first one is a warning, the rest of the
    // streak only debug lines — a camera that sends nothing usable would
    // otherwise log five warnings a second.
    let mut failed: u32 = 0;
    while !stop.load(Ordering::Relaxed) {
        let frame = match source.next_frame() {
            Ok(f) if f.is_valid() => Arc::new(f),
            result => {
                let e = result.map_or_else(
                    |e| e,
                    |f| {
                        format!(
                            "unusable camera frame {}×{}, {} bytes",
                            f.width,
                            f.height,
                            f.rgb.len()
                        )
                    },
                );
                failed += 1;
                if failed == 1 {
                    tracing::warn!(%e, "camera frame");
                } else {
                    tracing::debug!(%e, failed, "camera frame");
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        if failed > 0 {
            tracing::info!(failed, "camera frames again");
            failed = 0;
        }
        preview.put(frame.clone());

        let new_tier = link.send_tier();
        if new_tier != tier {
            tier = new_tier;
            enc = build(tier).unwrap_or_else(|e| {
                tracing::error!(%e, "openh264 encoder rebuild");
                None
            });
            last_idr = Instant::now() - IDR_INTERVAL;
            frame_index = 0;
        }

        let Some(enc) = enc.as_mut() else {
            // Paused: still drain keyframe requests so none is lost across
            // the pause/resume boundary — the next encoder's first frame is
            // an IDR anyway once video resumes.
            let _ = link.take_keyframe_request();
            continue;
        };

        frame_index += 1;
        if !frame_index.is_multiple_of(tier.frame_divisor()) {
            continue; // thin the fixed-FPS capture down to this tier's rate
        }

        // Asserts length and even sides: `frame.is_valid()` above.
        let src = RgbSliceU8::new(&frame.rgb, (frame.width, frame.height));
        match yuv.as_mut() {
            Some(buf) if dimensions_match(buf, frame.width, frame.height) => buf.read_rgb(src),
            _ => yuv = Some(YUVBuffer::from_rgb_source(src)),
        }
        let Some(buf) = yuv.as_ref() else { continue };
        let want_idr = link.take_keyframe_request() || last_idr.elapsed() >= IDR_INTERVAL;
        if want_idr {
            enc.force_intra_frame();
        }
        let ts =
            (start.elapsed().as_secs_f64() * f64::from(crate::audio::SAMPLE_RATE)) as u64 as u32;
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

/// Whether `buf` is already the right size to be overwritten in place.
fn dimensions_match(buf: &YUVBuffer, w: usize, h: usize) -> bool {
    use openh264::formats::YUVSource;
    buf.dimensions() == (w, h)
}

/// Owns the camera thread; started/stopped from the UI according to
/// `AppState::camera_on`.
pub struct CameraHandle {
    link: CameraLink,
    thread: Option<(JoinHandle<()>, Arc<AtomicBool>)>,
    preview: Arc<PreviewSlot>,
    device: Option<String>,
}

impl CameraHandle {
    /// New, camera off.
    pub fn new(link: CameraLink) -> CameraHandle {
        CameraHandle {
            link,
            thread: None,
            preview: Arc::new(PreviewSlot::default()),
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
        self.preview.clear();
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

    /// The slot the capture thread writes every picture into, shared so a
    /// [`CameraPreview`] can show it without copying a frame per repaint.
    pub fn preview_source(&self) -> Arc<PreviewSlot> {
        self.preview.clone()
    }
}

impl Drop for CameraHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Draws the camera's own picture, for the settings window: "is this the
/// right camera, and is it pointing at me?". It owns the GPU texture, so it
/// belongs to whoever draws rather than to [`CameraHandle`], which belongs
/// to the capture thread and outlives any one window.
#[cfg(feature = "gui")]
pub struct CameraPreview {
    slot: Arc<PreviewSlot>,
    texture: Option<egui::TextureHandle>,
    /// The `PreviewSlot::seq` already uploaded to [`Self::texture`].
    shown: u64,
}

#[cfg(feature = "gui")]
impl CameraPreview {
    /// Reads from [`CameraHandle::preview_source`].
    pub fn new(slot: Arc<PreviewSlot>) -> CameraPreview {
        CameraPreview {
            slot,
            texture: None,
            shown: u64::MAX,
        }
    }

    /// Uploads the latest picture if the capture thread produced a new one,
    /// and hands back what to draw. `None` while the camera is off or still
    /// opening (a USB webcam needs a few seconds for auto-exposure,
    /// `Research/camera-video.md`).
    pub fn texture(&mut self, ctx: &egui::Context) -> Option<egui::load::SizedTexture> {
        let seq = self.slot.seq.load(Ordering::Acquire);
        if seq != self.shown {
            self.shown = seq;
            // The capture thread only waits for the pointer copy, not for
            // the conversion below.
            let frame = self.slot.frame.lock().unwrap().clone();
            match frame {
                // `ColorImage::from_rgb` asserts on the length; a picture
                // that does not fit keeps the last one on screen.
                Some(frame) if !frame.is_valid() => {}
                Some(frame) => {
                    let image = egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb);
                    match &mut self.texture {
                        Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                        None => {
                            self.texture = Some(ctx.load_texture(
                                "camera-preview",
                                image,
                                egui::TextureOptions::LINEAR,
                            ))
                        }
                    }
                }
                None => self.texture = None,
            }
        }
        self.texture
            .as_ref()
            .map(egui::load::SizedTexture::from_handle)
    }

    /// Draws the latest picture into a `width`-wide 4:3 box. Returns `false`
    /// when there is nothing to show yet.
    pub fn ui(&mut self, ui: &mut egui::Ui, width: f32) -> bool {
        let Some(tex) = self.texture(ui.ctx()) else {
            return false;
        };
        ui.add(
            egui::Image::new((tex.id, egui::vec2(width, width * 3.0 / 4.0)))
                .corner_radius(egui::CornerRadius::same(crate::ui::theme::R_CHIP)),
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder produces an IDR on `force_intra_frame` and SPS/PPS come
    /// with it.
    #[test]
    fn encoder_emits_idr_on_request() {
        let mut enc = encoder(VideoTier::T0).unwrap();
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

    /// A `w × h` picture whose every pixel says where it is: R = x, G = y.
    fn pattern(w: usize, h: usize) -> Vec<u8> {
        (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [x as u8, y as u8, 7]))
            .collect()
    }

    #[test]
    fn checked_keeps_an_exact_frame() {
        let f = RgbFrame::checked(4, 2, pattern(4, 2)).unwrap();
        assert_eq!((f.width, f.height), (4, 2));
        assert_eq!(f.rgb, pattern(4, 2));
        assert!(f.is_valid());
    }

    /// A virtual camera's 5×3 becomes 4×2: the last column and row go, every
    /// other pixel stays where it was.
    #[test]
    fn checked_crops_odd_sides() {
        let f = RgbFrame::checked(5, 3, pattern(5, 3)).unwrap();
        assert_eq!((f.width, f.height), (4, 2));
        assert_eq!(f.rgb, pattern(4, 2));
        assert!(f.is_valid());
        let f = RgbFrame::checked(4, 3, pattern(4, 3)).unwrap();
        assert_eq!((f.width, f.height, f.rgb), (4, 2, pattern(4, 2)));
        let f = RgbFrame::checked(5, 2, pattern(5, 2)).unwrap();
        assert_eq!((f.width, f.height, f.rgb), (4, 2, pattern(4, 2)));
    }

    /// A buffer that does not hold what the size says — the oversized MJPEG
    /// frame, a padded YUYV stride — is rejected instead of reaching an
    /// assert.
    #[test]
    fn checked_rejects_mismatched_buffers() {
        let mut long = pattern(320, 240);
        long.extend_from_slice(&[0; 320 * 3]); // one padded row too many
        assert!(RgbFrame::checked(320, 240, long).is_err());
        assert!(RgbFrame::checked(320, 240, pattern(320, 239)).is_err());
        assert!(RgbFrame::checked(640, 480, pattern(320, 240)).is_err());
        assert!(RgbFrame::checked(0, 0, Vec::new()).is_err());
        assert!(
            RgbFrame::checked(1, 1, pattern(1, 1)).is_err(),
            "nothing left"
        );
        assert!(
            RgbFrame::checked(usize::MAX, 3, Vec::new()).is_err(),
            "overflow"
        );
        // And a hand-made frame is caught by the last check.
        let bad = RgbFrame {
            width: 5,
            height: 3,
            rgb: pattern(5, 3),
        };
        assert!(!bad.is_valid(), "odd sides");
        let bad = RgbFrame {
            width: 4,
            height: 2,
            rgb: pattern(4, 3),
        };
        assert!(!bad.is_valid(), "length");
    }

    /// What survives `checked` goes through the encoder's asserting
    /// constructors without a panic.
    #[test]
    fn a_cropped_frame_encodes() {
        let f = RgbFrame::checked(321, 241, pattern(321, 241)).unwrap();
        let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&f.rgb, (f.width, f.height)));
        let mut enc = encoder(VideoTier::T0).unwrap();
        let bs = enc.encode(&yuv).unwrap();
        assert!(matches!(bs.frame_type(), FrameType::IDR));
    }

    #[test]
    fn jpeg_size_reads_the_frame_header() {
        // SOI, APP0 (JFIF, 16 bytes), DQT (a stub), then SOF0 640×480.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        jpeg.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        jpeg.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x04, 0x00, 0x00]);
        let sof = [
            0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80, 0x03, 0x01, 0x22, 0x00, 0x02,
            0x11, 0x01, 0x03, 0x11, 0x01,
        ];
        jpeg.extend_from_slice(&sof);
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08]);
        assert_eq!(jpeg_size(&jpeg), Some((640, 480)));
        // Progressive (SOF2), with a fill byte before the marker.
        let mut progressive = vec![0xFF, 0xD8, 0xFF];
        progressive.extend_from_slice(&sof);
        progressive[4] = 0xC2;
        assert_eq!(jpeg_size(&progressive), Some((640, 480)));
        // A DHT (C4) is not a frame header.
        let dht = [0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x03, 0x00, 0xFF, 0xD9];
        assert_eq!(jpeg_size(&dht), None);
        assert_eq!(jpeg_size(b"not a jpeg"), None);
        assert_eq!(jpeg_size(&jpeg[..jpeg.len() - 20]), None, "cut short");
        assert_eq!(jpeg_size(&[0xFF, 0xD8, 0xFF, 0xDA]), None, "scan first");
    }
}
