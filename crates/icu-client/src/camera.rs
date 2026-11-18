//! Camera capture (nokhwa) and H.264 encoding (openh264) on their own
//! thread (`docs/reference/client.md` §8). 320×240 capture at 15 fps; the
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
    OpenH264API, Timestamp,
};

use crate::{
    engine::{AppState, CameraLink, Command},
    problem::Problem,
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

/// Minimum spacing of the IDRs sent because a frame was lost: the same
/// once per second the receivers may ask for one (`docs/reference/protocol.md` §2.2
/// Nr. 13). An IDR is several P-frames' worth of bytes, and the loss was
/// most likely congestion.
const LOSS_IDR_INTERVAL: Duration = Duration::from_secs(1);

/// One RGB8 picture.
#[derive(Debug, Clone)]
pub struct RgbFrame {
    /// Width.
    pub width: usize,
    /// Height.
    pub height: usize,
    /// RGB8, row major.
    pub rgb: Vec<u8>,
    /// Monotonic acquisition time, before RGB decode and enhancement.
    pub captured_at: Option<Instant>,
    pub timing_accurate: bool,
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
            captured_at: None,
            timing_accurate: false,
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

/// Refuses an MJPEG frame whose headers libjpeg would not survive.
///
/// nokhwa decodes MJPEG with mozjpeg, and libjpeg reports a fatal error by
/// unwinding out of C. A release build aborts on that: one frame without a
/// start marker, cut off inside its headers or with a damaged table — what
/// a USB hiccup or a camera being unplugged produces — ended the whole
/// client. Damage *behind* the headers is harmless (libjpeg warns and pads
/// the picture), so only the headers are walked here, the way libjpeg's own
/// marker reader does: stray bytes between segments are skipped, and
/// whatever it would decode is let through.
fn jpeg_decodable(data: &[u8]) -> Result<(), &'static str> {
    const CUT: &str = "cut off inside its headers";
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err("no start marker");
    }
    let mut i = 2;
    // Component id and quantization table of the frame header's components.
    let mut components: Option<([(u8, u8); 4], usize)> = None;
    let mut quant = [false; 4];
    loop {
        while *data.get(i).ok_or(CUT)? != 0xFF {
            i += 1;
        }
        while *data.get(i).ok_or(CUT)? == 0xFF {
            i += 1;
        }
        let marker = data[i];
        i += 1;
        match marker {
            // A stuffed zero, TEM, RSTn: no segment follows.
            0x00 | 0x01 | 0xD0..=0xD7 => continue,
            0xD8 => return Err("a second start marker"),
            0xD9 => return Err("ends before its picture data"),
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([
            *data.get(i).ok_or(CUT)?,
            *data.get(i + 1).ok_or(CUT)?,
        ]));
        let segment = data.get(i + 2..i + len).ok_or(CUT)?;
        i += len;
        match marker {
            // SOF0–SOF2: baseline, extended and progressive Huffman.
            0xC0..=0xC2 => {
                if components.is_some() {
                    return Err("two frame headers");
                }
                let [precision, h0, h1, w0, w1, count, rest @ ..] = segment else {
                    return Err("short frame header");
                };
                let count = usize::from(*count);
                if *precision != 8
                    || u16::from_be_bytes([*h0, *h1]) == 0
                    || u16::from_be_bytes([*w0, *w1]) == 0
                    || !(1..=4).contains(&count)
                    || rest.len() != 3 * count
                {
                    return Err("unusable frame header");
                }
                let mut list = [(0, 0); 4];
                for (slot, component) in list.iter_mut().zip(rest.as_chunks::<3>().0) {
                    let (h, v, table) = (component[1] >> 4, component[1] & 15, component[2]);
                    if !(1..=4).contains(&h) || !(1..=4).contains(&v) || table > 3 {
                        return Err("unusable frame component");
                    }
                    *slot = (component[0], table);
                }
                components = Some((list, count));
            }
            0xC4 => huffman_tables_decodable(segment)?,
            0xDB => {
                let mut rest = segment;
                while let [head, tail @ ..] = rest {
                    let (wide, table) = (usize::from(head >> 4), usize::from(head & 15));
                    let size = 64 * (1 + wide);
                    if wide > 1 || table > 3 || tail.len() < size {
                        return Err("unusable quantization table");
                    }
                    quant[table] = true;
                    rest = &tail[size..];
                }
            }
            0xDD => {
                if segment.len() != 2 {
                    return Err("unusable restart interval");
                }
            }
            0xDA => {
                let Some((list, count)) = components else {
                    return Err("picture data before the frame header");
                };
                let [scan_count, rest @ ..] = segment else {
                    return Err("short scan header");
                };
                let scan_count = usize::from(*scan_count);
                if !(1..=4).contains(&scan_count) || rest.len() != 2 * scan_count + 3 {
                    return Err("unusable scan header");
                }
                for component in rest[..2 * scan_count].as_chunks::<2>().0 {
                    let Some((_, table)) = list[..count].iter().find(|(id, _)| *id == component[0])
                    else {
                        return Err("scan of an unknown component");
                    };
                    if !quant[usize::from(*table)] {
                        return Err("no quantization table");
                    }
                    if component[1] >> 4 > 3 || component[1] & 15 > 3 {
                        return Err("unusable scan component");
                    }
                }
                return Ok(());
            }
            // DNL, APPn, COM: skipped by libjpeg as well.
            0xDC | 0xE0..=0xEF | 0xFE => {}
            _ => return Err("unsupported marker"),
        }
    }
}

/// A DHT segment libjpeg accepts: each table's index and symbol count in
/// range, the code lengths forming a prefix code that leaves the all-ones
/// code free (`jpeg_make_d_derived_tbl`), DC symbols at most 15.
fn huffman_tables_decodable(mut rest: &[u8]) -> Result<(), &'static str> {
    const BAD: &str = "unusable Huffman table";
    while !rest.is_empty() {
        let (head, tail) = rest.split_first_chunk::<17>().ok_or(BAD)?;
        let (class, table, counts) = (head[0] >> 4, head[0] & 15, &head[1..]);
        let total: usize = counts.iter().map(|&n| usize::from(n)).sum();
        if class > 1 || table > 3 || total > 256 || tail.len() < total {
            return Err(BAD);
        }
        let used = counts
            .iter()
            .rposition(|&n| n != 0)
            .map_or(0, |last| last + 1);
        let mut code = 0u32;
        for (bits, &n) in (1u32..).zip(&counts[..used]) {
            code += u32::from(n);
            if code >= 1 << bits {
                return Err(BAD);
            }
            code <<= 1;
        }
        if class == 0 && tail[..total].iter().any(|&symbol| symbol > 15) {
            return Err(BAD);
        }
        rest = &tail[total..];
    }
    Ok(())
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
    time: crate::media::CameraTime,
}

impl NokhwaSource {
    /// Opens camera `index` (Spike 5: pick the capture node explicitly) at
    /// the closest format to 320×240 MJPEG 15 fps, or else the best one
    /// [`choose_format`] finds among what the camera offers.
    pub fn open(index: u32) -> Result<NokhwaSource, String> {
        use nokhwa::{
            pixel_format::RgbFormat,
            utils::{
                frame_formats, CameraFormat, CameraIndex, FrameFormat, RequestedFormat,
                RequestedFormatType, Resolution,
            },
        };
        let want = CameraFormat::new(Resolution::new(WIDTH, HEIGHT), FrameFormat::MJPEG, FPS);
        let req = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(want));
        let mut camera = match nokhwa::Camera::new(CameraIndex::Index(index), req) {
            Ok(camera) => camera,
            // nokhwa's `Closest` keeps only MJPEG and takes frame rates only
            // from an exact 320×240: a YUYV-only camera, or one without that
            // MJPEG mode, fails here. Open it with whatever it offers first,
            // then pick among its formats.
            Err(closest) => {
                let any = RequestedFormat::with_formats(RequestedFormatType::None, frame_formats());
                let mut camera = nokhwa::Camera::new(CameraIndex::Index(index), any)
                    .map_err(|_| closest.to_string())?;
                let offered = camera
                    .compatible_camera_formats()
                    .map_err(|e| e.to_string())?;
                let format = choose_format(&offered).ok_or_else(|| {
                    let mut fourccs: Vec<String> =
                        offered.iter().map(|f| f.format().to_string()).collect();
                    fourccs.dedup();
                    format!("{NO_USABLE_FORMAT}: {}", fourccs.join(", "))
                })?;
                camera
                    .set_camera_requset(RequestedFormat::new::<RgbFormat>(
                        RequestedFormatType::Exact(format),
                    ))
                    .map_err(|e| e.to_string())?;
                camera
            }
        };
        camera.open_stream().map_err(|e| e.to_string())?;
        tracing::info!(format = ?camera.camera_format(), "camera opened");
        Ok(NokhwaSource {
            camera,
            time: Default::default(),
        })
    }
}

/// How an error from [`NokhwaSource::open`] starts when the camera works but
/// offers no picture format ICU can decode; [`CameraHandle`] shows its own
/// text for it and does not retry.
pub const NO_USABLE_FORMAT: &str = "no usable camera format";

/// The capture format among `offered`: MJPEG first (small over USB), then
/// YUYV, then any other format the RGB decoder takes; within that format
/// the smallest resolution of at least 320×240 (else the largest below),
/// and for it the frame rate closest to 15 fps, the faster one on a tie.
/// `None` when nothing is decodable (a grey-only camera, say).
fn choose_format(offered: &[nokhwa::utils::CameraFormat]) -> Option<nokhwa::utils::CameraFormat> {
    use nokhwa::utils::{CameraFormat, FrameFormat};
    const PREFERENCE: [FrameFormat; 5] = [
        FrameFormat::MJPEG,
        FrameFormat::YUYV,
        FrameFormat::NV12,
        FrameFormat::RAWRGB,
        FrameFormat::RAWBGR,
    ];
    PREFERENCE.into_iter().find_map(|fourcc| {
        let same = || offered.iter().filter(move |f| f.format() == fourcc);
        let resolution = same().map(|f| f.resolution()).min_by_key(|r| {
            let area = u64::from(r.width()) * u64::from(r.height());
            if r.width() >= WIDTH && r.height() >= HEIGHT {
                (0, area)
            } else {
                (1, u64::MAX - area)
            }
        })?;
        let rate = same()
            .filter(|f| f.resolution() == resolution)
            .map(|f| f.frame_rate())
            .min_by_key(|&fps| (fps.abs_diff(FPS), std::cmp::Reverse(fps)))?;
        Some(CameraFormat::new(resolution, fourcc, rate))
    })
}

/// Brings a picture larger than 320×240 down to it: the centre cut to 4:3,
/// then each output pixel the average of the input pixels it covers. The
/// encoder's tiers, the receivers' limits (640×480) and the tiles all
/// assume 320×240; a camera without that mode delivers more. A picture
/// that fits already is returned as it is.
fn fit_capture(frame: RgbFrame) -> RgbFrame {
    let (w, h) = (frame.width, frame.height);
    if w <= WIDTH as usize && h <= HEIGHT as usize {
        return frame;
    }
    // The 4:3 centre.
    let (cw, ch) = if w * 3 > h * 4 {
        (h * 4 / 3, h)
    } else {
        (w, w * 3 / 4)
    };
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    // Even sides, as the encoder wants; never more than the cut, so every
    // output pixel covers at least one input pixel.
    let (tw, th) = (cw.min(WIDTH as usize) & !1, ch.min(HEIGHT as usize) & !1);
    let mut rgb = Vec::with_capacity(tw * th * 3);
    for ty in 0..th {
        let (ya, yb) = (y0 + ty * ch / th, y0 + (ty + 1) * ch / th);
        for tx in 0..tw {
            let (xa, xb) = (x0 + tx * cw / tw, x0 + (tx + 1) * cw / tw);
            let mut sum = [0u32; 3];
            for y in ya..yb {
                let row = &frame.rgb[(y * w + xa) * 3..(y * w + xb) * 3];
                for px in row.as_chunks::<3>().0 {
                    for c in 0..3 {
                        sum[c] += u32::from(px[c]);
                    }
                }
            }
            let n = ((yb - ya) * (xb - xa)) as u32;
            rgb.extend(sum.map(|v| ((v + n / 2) / n) as u8));
        }
    }
    RgbFrame {
        width: tw,
        height: th,
        rgb,
        captured_at: frame.captured_at,
        timing_accurate: frame.timing_accurate,
    }
}

impl FrameSource for NokhwaSource {
    fn next_frame(&mut self) -> Result<RgbFrame, String> {
        use nokhwa::{pixel_format::RgbFormat, utils::FrameFormat};
        let buffer = self.camera.frame().map_err(|e| e.to_string())?;
        let (captured_at, accurate) = self.time.convert(
            buffer.capture_timestamp(),
            Instant::now(),
            std::time::SystemTime::now(),
        );
        if buffer.source_frame_format() == FrameFormat::MJPEG {
            jpeg_decodable(buffer.buffer()).map_err(|e| format!("damaged MJPEG frame: {e}"))?;
        }
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
        let mut frame = RgbFrame::checked(width, height, rgb)?;
        frame.captured_at = Some(captured_at);
        frame.timing_accurate = accurate;
        Ok(fit_capture(frame))
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
            captured_at: None,
            timing_accurate: false,
            width: w,
            height: h,
            rgb,
        })
    }
}

/// A camera-independent privacy picture. The label is burned into the pixels,
/// so older clients and every video surface see the same unmistakable state.
pub struct PrivacySource {
    tick: u32,
    start: Instant,
}

impl Default for PrivacySource {
    fn default() -> Self {
        Self {
            tick: 0,
            start: Instant::now(),
        }
    }
}

impl PrivacySource {
    fn picture(tick: u32) -> RgbFrame {
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let mut rgb = vec![0; w * h * 3];
        // Coarse, low-contrast noise compresses well at the existing video
        // bitrates. Scanlines and a slow rolling band suggest an analogue TV
        // without full-screen flashes or making the steady label flicker.
        for y in 0..h {
            for x in 0..w {
                let mut hash = ((x / 4) as u32).wrapping_mul(374_761_393)
                    ^ ((y / 4) as u32).wrapping_mul(668_265_263)
                    ^ tick.wrapping_mul(2_246_822_519);
                hash = (hash ^ (hash >> 13)).wrapping_mul(1_274_126_177);
                let mut v = 64 + ((hash >> 24) % 112) as u8;
                if y % 4 == 0 {
                    v /= 2;
                }
                if (y + tick as usize * 2) % h < 10 {
                    v = v.saturating_sub(24);
                }
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(v);
            }
        }
        let label = "PRIVACY MODE ON";
        let scale = 3;
        let left = (w - (label.len() * 6 - 1) * scale) / 2;
        let top = (h - 7 * scale) / 2;
        for y in top - 10..top + 7 * scale + 10 {
            rgb[y * w * 3..(y + 1) * w * 3].fill(18);
        }
        for (i, c) in label.chars().enumerate() {
            let rows = match c {
                'P' => [30, 17, 17, 30, 16, 16, 16],
                'R' => [30, 17, 17, 30, 20, 18, 17],
                'I' => [31, 4, 4, 4, 4, 4, 31],
                'V' => [17, 17, 17, 17, 17, 10, 4],
                'A' => [14, 17, 17, 31, 17, 17, 17],
                'C' => [14, 17, 16, 16, 16, 17, 14],
                'Y' => [17, 17, 10, 4, 4, 4, 4],
                'M' => [17, 27, 21, 21, 17, 17, 17],
                'O' => [14, 17, 17, 17, 17, 17, 14],
                'D' => [30, 17, 17, 17, 17, 17, 30],
                'E' => [31, 16, 16, 30, 16, 16, 31],
                'N' => [17, 25, 25, 21, 19, 19, 17],
                _ => [0; 7],
            };
            for (y, row) in rows.iter().enumerate() {
                for x in 0..5 {
                    if row & (1 << (4 - x)) != 0 {
                        for dy in 0..scale {
                            let offset =
                                ((top + y * scale + dy) * w + left + (i * 6 + x) * scale) * 3;
                            rgb[offset..offset + scale * 3].fill(240);
                        }
                    }
                }
            }
        }
        RgbFrame {
            captured_at: None,
            timing_accurate: false,
            width: w,
            height: h,
            rgb,
        }
    }
}

impl FrameSource for PrivacySource {
    fn next_frame(&mut self) -> Result<RgbFrame, String> {
        let deadline = self.start + Duration::from_millis(1000 / FPS as u64) * self.tick;
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let frame = Self::picture(self.tick);
        self.tick = self.tick.wrapping_add(1);
        Ok(frame)
    }
}

/// Frames per second `tier` encodes: the capture rate thinned by its
/// [`VideoTier::frame_divisor`]; 0 at `T4`.
pub fn frame_rate(tier: VideoTier) -> f32 {
    match tier.frame_divisor() {
        0 => 0.0,
        n => FPS as f32 / n as f32,
    }
}

/// The encoder configuration for one [`VideoTier`]. Never called for `T4`
/// (paused): `run_sender` simply stops encoding instead of building an
/// encoder with a zero bitrate.
///
/// The frame rate is the tier's own, not the capture rate: openh264's rate
/// control budgets every frame as bitrate ÷ configured frame rate, so an
/// encoder told 15 fps and fed a third of that spent a third of the tier's
/// bitrate (T1 ≈ 50 instead of 100 kbit/s, T3 ≈ 8 instead of 40).
pub fn encoder(tier: VideoTier) -> Result<Encoder, String> {
    let cfg = EncoderConfig::new()
        .bitrate(BitRate::from_bps(tier.bitrate()))
        .max_frame_rate(FrameRate::from_hz(frame_rate(tier)))
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate);
    Encoder::with_api_config(OpenH264API::from_source(), cfg).map_err(|e| e.to_string())
}

/// Thins the capture to a tier's frame rate by capture time, not by count.
/// Counting assumed a 15 fps camera; one that runs faster — a mode offered
/// only at 30 fps — multiplied the bitrate, since openh264 budgets every
/// frame for the configured rate and its frame skipping does not catch up
/// (measured: 30 fps into T0 gave 298 instead of 150 kbit/s). A slower one
/// (low light) has every picture encoded.
#[derive(Debug, Default)]
struct Pacer {
    /// When the next picture is due; `None` before the first.
    due: Option<Duration>,
    /// The previous picture's capture time.
    last: Option<Duration>,
    /// Smoothed spacing of the pictures; half of it is the tolerance a
    /// picture may come early and still count as due.
    spacing: Duration,
}

impl Pacer {
    /// Whether the picture captured at `at` is encoded at `rate` fps.
    fn take(&mut self, at: Duration, rate: f32) -> bool {
        if let Some(last) = self.last {
            let d = at.saturating_sub(last);
            self.spacing = if self.spacing.is_zero() {
                d
            } else {
                (self.spacing * 7 + d) / 8
            };
        }
        self.last = Some(at);
        if self.due.is_some_and(|due| at + self.spacing / 2 < due) {
            return false;
        }
        // One interval on from when it was due — the rate stays exact
        // whatever the jitter — but never behind the picture itself, or a
        // slow stretch would be caught up in a burst.
        let interval = Duration::from_secs_f32(1.0 / rate);
        self.due = Some((self.due.unwrap_or(at) + interval).max(at));
        true
    }
}

/// Failed frames in a row after which the capture thread gives up (about
/// 5 s at one attempt per 200 ms): the device stopped delivering — unplugged,
/// or taken by another program — and is opened anew by
/// [`CameraHandle::sync`], with backoff.
const MAX_FAILED_FRAMES: u32 = 25;

/// Failed starts in a row after which [`CameraHandle::sync`] stops trying
/// and switches the camera off (`Command::CameraFailed`).
const MAX_FAILURES: u32 = 5;

/// The wait before the next start after `failures` failed ones: 1, 2, 4 …
/// up to 30 s.
fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1u64 << failures.saturating_sub(1).min(5)).min(Duration::from_secs(30))
}

/// Latest-only acquisition worker: camera buffering cannot grow behind a send.
type CaptureSlot = (Mutex<Option<Result<RgbFrame, String>>>, std::sync::Condvar);
struct FreshSource {
    slot: Arc<CaptureSlot>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl FreshSource {
    fn start(mut source: Box<dyn FrameSource>, stop: Arc<AtomicBool>) -> Result<Self, String> {
        let slot = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
        let target = slot.clone();
        let halted = stop.clone();
        let worker = std::thread::Builder::new()
            .name("icu-camera-capture".into())
            .spawn(move || {
                while !halted.load(Ordering::Relaxed) {
                    let result = source.next_frame().map(|mut frame| {
                        if frame.captured_at.is_none() {
                            frame.captured_at = Some(Instant::now());
                        }
                        frame
                    });
                    let failed = result.is_err();
                    *target.0.lock().unwrap() = Some(result);
                    target.1.notify_one();
                    if failed {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
                target.1.notify_one();
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            slot,
            stop,
            worker: Some(worker),
        })
    }
}
impl FrameSource for FreshSource {
    fn next_frame(&mut self) -> Result<RgbFrame, String> {
        let mut slot = self.slot.0.lock().unwrap();
        loop {
            if let Some(frame) = slot.take() {
                return frame;
            }
            if self.stop.load(Ordering::Relaxed) {
                return Err("capture stopped".into());
            }
            slot = self
                .slot
                .1
                .wait_timeout(slot, Duration::from_millis(50))
                .unwrap()
                .0;
            if self.worker.as_ref().is_some_and(|w| w.is_finished()) && slot.is_none() {
                return Err("capture worker ended".into());
            }
        }
    }
}
impl Drop for FreshSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The worker owns the source and is usually inside one capture: give
        // it a few frame intervals to come back, so the device is released
        // when the camera thread ends — the next open of the same camera
        // (privacy off, a restart) otherwise finds it busy. A driver that
        // stays blocked keeps its source until it returns; privacy
        // revocation is enforced independently by CameraLink.
        if let Some(worker) = self.worker.take() {
            let deadline = Instant::now() + Duration::from_millis(300);
            while !worker.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

/// Encoder thread loop: capture → encode → send. `preview` receives every
/// captured picture for the settings preview, regardless of tier, so the
/// settings preview keeps working even while video is paused for everyone
/// else. While `live` is off the thread only captures for that preview —
/// nothing is encoded or sent (`CameraHandle::set_preview`).
///
/// The encoder is rebuilt whenever the tier changes (`link.send_tier()`,
/// or paused while not `live`): openh264's safe Rust binding exposes no
/// runtime bitrate mutator, so a tier change means a fresh `Encoder`. That
/// is cheap relative to how rarely `QualityController` actually steps
/// (bounded by its own hysteresis), and a freshly built encoder's first
/// frame is an IDR regardless of `force_intra_frame`
/// (`encoder_emits_idr_on_request` below), so SPS/PPS ride along for free —
/// no separate flush logic is needed.
///
/// Returns once `stop` is set, or with the last error after
/// [`MAX_FAILED_FRAMES`] failed frames in a row.
pub fn run_sender(
    source: Box<dyn FrameSource>,
    link: CameraLink,
    stop: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
    preview: Arc<PreviewSlot>,
) -> Result<(), String> {
    let mut source = FreshSource::start(source, stop.clone())?;
    let mut enhancer = crate::video::enhance::Enhancer::default();
    // No encoder while paused: `run_sender` stops encoding rather than
    // building one with a zero bitrate.
    let build = |tier: VideoTier| -> Result<Option<Encoder>, String> {
        if tier.paused() {
            Ok(None)
        } else {
            encoder(tier).map(Some)
        }
    };
    // Built with the first frame that is to go out.
    let mut tier = VideoTier::T4;
    let mut enc: Option<Encoder> = None;
    let start = Instant::now();
    let mut last_idr = Instant::now() - IDR_INTERVAL;
    let mut yuv: Option<YUVBuffer> = None;
    let mut pacer = Pacer::default();
    // Failed frames in a row: the first one is a warning, the rest of the
    // streak only debug lines — a camera that sends nothing usable would
    // otherwise log five warnings a second.
    let mut failed: u32 = 0;
    // A frame did not go out on a live connection: the next ones refer to
    // it, so the receivers see a broken picture until the next IDR.
    let mut lost = false;
    while !stop.load(Ordering::Relaxed) {
        let mut frame = match source.next_frame() {
            Ok(f) if f.is_valid() => f,
            // Stopping: the source ending is not a failed frame to warn
            // about and sleep on — `stop` joins this thread from the UI.
            Err(_) if stop.load(Ordering::Relaxed) => break,
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
                if failed >= MAX_FAILED_FRAMES {
                    return Err(e);
                }
                if failed == 1 {
                    tracing::warn!(%e, "camera frame");
                } else {
                    tracing::debug!(%e, failed, "camera frame");
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        // When the picture arrived: the wire timestamp, and what openh264's
        // rate control measures the real frame spacing with.
        let acquired = frame.captured_at.unwrap_or_else(Instant::now);
        if acquired.elapsed() > Duration::from_millis(100) {
            continue;
        }
        let captured = acquired.saturating_duration_since(start);
        let stamp = link.media_stamp(acquired);
        if failed > 0 {
            tracing::info!(failed, "camera frames again");
            failed = 0;
        }
        // A revoked source may have been blocked inside next_frame. It must
        // publish neither a stale preview nor another encoded camera frame.
        if !link.source_current() {
            return Ok(());
        }
        let report = enhancer.process(&mut frame, Instant::now());
        link.report_processing(report, frame.timing_accurate);
        let frame = Arc::new(frame);
        preview.put(frame.clone());

        let new_tier = if live.load(Ordering::Relaxed) {
            link.send_tier()
        } else {
            VideoTier::T4
        };
        if new_tier != tier {
            tier = new_tier;
            enc = build(tier).unwrap_or_else(|e| {
                tracing::error!(%e, "openh264 encoder");
                None
            });
            last_idr = Instant::now() - IDR_INTERVAL;
            pacer = Pacer::default();
        }

        let Some(enc) = enc.as_mut() else {
            // Paused or preview only: still drain keyframe requests so none
            // is lost across the pause/resume boundary — the next encoder's
            // first frame is an IDR anyway once video resumes.
            let _ = link.take_keyframe_request();
            continue;
        };

        if !pacer.take(captured, frame_rate(tier)) {
            continue; // thin the capture down to this tier's rate
        }

        // Asserts length and even sides: `frame.is_valid()` above.
        let src = RgbSliceU8::new(&frame.rgb, (frame.width, frame.height));
        match yuv.as_mut() {
            Some(buf) if dimensions_match(buf, frame.width, frame.height) => buf.read_rgb(src),
            _ => yuv = Some(YUVBuffer::from_rgb_source(src)),
        }
        let Some(buf) = yuv.as_ref() else { continue };
        let want_idr = link.take_keyframe_request()
            || last_idr.elapsed() >= IDR_INTERVAL
            || (lost && last_idr.elapsed() >= LOSS_IDR_INTERVAL);
        if want_idr {
            enc.force_intra_frame();
        }
        let ts = (captured.as_secs_f64() * f64::from(crate::audio::SAMPLE_RATE)) as u64 as u32;
        match enc.encode_at(buf, Timestamp::from_millis(captured.as_millis() as u64)) {
            Ok(bs) => {
                let keyframe = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
                if keyframe {
                    last_idr = Instant::now();
                    lost = false;
                }
                let data = bs.to_vec();
                if data.is_empty() {
                    continue;
                }
                if let Err(e) = link.send_captured_frame(
                    keyframe,
                    ts,
                    &data,
                    (stamp, acquired, frame.timing_accurate),
                ) {
                    tracing::debug!(%e, "video frame not sent");
                    // Not while offline: there is nobody to repair for.
                    lost |= link.connected();
                }
            }
            Err(e) => tracing::warn!(%e, "h264 encode"),
        }
    }
    Ok(())
}

/// Whether `buf` is already the right size to be overwritten in place.
fn dimensions_match(buf: &YUVBuffer, w: usize, h: usize) -> bool {
    use openh264::formats::YUVSource;
    buf.dimensions() == (w, h)
}

/// Opens the capture source for a device setting: an index (default 0), or
/// `synthetic` for the test pattern.
pub type Opener = Box<dyn FnMut(Option<&str>) -> Result<Box<dyn FrameSource>, String> + Send>;

/// The real devices: nokhwa by index, or the test pattern.
fn open_device(device: Option<&str>) -> Result<Box<dyn FrameSource>, String> {
    Ok(match device {
        Some("synthetic") => Box::new(SyntheticSource::new()),
        Some(idx) => Box::new(NokhwaSource::open(
            idx.parse()
                .map_err(|_| "camera device must be an index".to_string())?,
        )?),
        None => Box::new(NokhwaSource::open(0)?),
    })
}

/// A running capture thread.
struct Capture {
    thread: JoinHandle<Result<(), String>>,
    stop: Arc<AtomicBool>,
    /// Encode and send (`camera_on`); off captures for the preview only.
    live: Arc<AtomicBool>,
    /// The device setting it was opened for.
    device: Option<String>,
    epoch: usize,
    /// `PreviewSlot::seq` when it started: if the thread fails after
    /// that moved, the camera did work, and the failure streak starts over.
    seq_at_start: u64,
}

/// Failed starts in a row and when the next one may run.
#[derive(Debug, Default)]
struct Retry {
    failures: u32,
    next: Option<Instant>,
}

impl Retry {
    /// Whether a start may be tried at `now`.
    fn may_try(&self, now: Instant) -> bool {
        self.failures < MAX_FAILURES && self.next.is_none_or(|next| now >= next)
    }

    /// Books a failed start; `false` once it is time to give up.
    fn failed(&mut self, now: Instant) -> bool {
        self.failures += 1;
        self.next = Some(now + backoff(self.failures));
        self.failures < MAX_FAILURES
    }

    fn reset(&mut self) {
        *self = Retry::default();
    }
}

/// Owns the camera thread, following `AppState::camera_on` and the local
/// preview through [`CameraHandle::sync`], which the window calls on every
/// frame (and `--headless` on every state change).
///
/// A camera that will not start is retried with backoff (1, 2, 4 … 30 s),
/// not on every call; after five failed starts in a row it is switched off
/// and the reason shown (`Command::CameraFailed`). A device change, the
/// camera or the preview switched on anew start over.
pub struct CameraHandle {
    link: CameraLink,
    thread: Option<Capture>,
    preview: Arc<PreviewSlot>,
    opener: Opener,
    /// Capture for the local preview while the camera is off.
    preview_wanted: bool,
    retry: Retry,
    /// `(camera_on, preview_wanted, device)` as of the last `sync`.
    last: (bool, bool, Option<String>),
}

impl CameraHandle {
    /// New, camera off.
    pub fn new(link: CameraLink) -> CameraHandle {
        CameraHandle::with_opener(link, Box::new(open_device))
    }

    /// New, with another way to open a device (tests).
    pub fn with_opener(link: CameraLink, opener: Opener) -> CameraHandle {
        CameraHandle {
            link,
            thread: None,
            preview: Arc::new(PreviewSlot::default()),
            opener,
            preview_wanted: false,
            retry: Retry::default(),
            last: (false, false, None),
        }
    }

    /// Starts with an explicit source, sending (tests).
    pub fn start_with(&mut self, source: Box<dyn FrameSource>) {
        let epoch = self.link.shared_epoch();
        let source = if epoch % 2 == 1 {
            Box::new(PrivacySource::default()) as Box<dyn FrameSource>
        } else {
            source
        };
        self.spawn(source, None, true, epoch);
    }

    fn spawn(
        &mut self,
        source: Box<dyn FrameSource>,
        device: Option<String>,
        live: bool,
        epoch: usize,
    ) {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicBool::new(live));
        let link = self.link.for_epoch(epoch);
        let preview = self.preview.clone();
        let (stop_t, live_t) = (stop.clone(), live.clone());
        let thread = std::thread::Builder::new()
            .name("icu-camera".into())
            .spawn(move || run_sender(source, link, stop_t, live_t, preview))
            .expect("camera thread");
        self.thread = Some(Capture {
            thread,
            stop,
            live,
            device,
            epoch,
            seq_at_start: self.preview.seq.load(Ordering::Acquire),
        });
    }

    /// Opens `device` (an index, default 0, or `synthetic`) and starts
    /// sending from it.
    pub fn start(&mut self, device: Option<&str>) -> Result<(), String> {
        let epoch = self.link.shared_epoch();
        let source = if epoch % 2 == 1 {
            Box::new(PrivacySource::default()) as Box<dyn FrameSource>
        } else {
            (self.opener)(device)?
        };
        self.spawn(source, device.map(str::to_owned), true, epoch);
        Ok(())
    }

    /// Stops the thread.
    pub fn stop(&mut self) {
        if let Some(capture) = self.thread.take() {
            capture.stop.store(true, Ordering::Relaxed);
            let _ = capture.thread.join();
        }
        self.preview.clear();
    }

    /// Whether the thread runs.
    pub fn running(&self) -> bool {
        self.thread.is_some()
    }

    /// Failed starts in a row; reset by a successful picture, a device
    /// change or switching the camera on anew.
    pub fn failures(&self) -> u32 {
        self.retry.failures
    }

    /// Captures for the local preview even while the camera is off — „is
    /// this the right camera, and is it pointing at me?“ before anyone sees
    /// it. Nothing is encoded or sent and `CAMERA_ON` stays as it is; the
    /// pictures go to [`CameraHandle::preview_source`]. The window turns it
    /// on while Settings → Kamera is open. Takes effect with the next
    /// [`CameraHandle::sync`].
    pub fn set_preview(&mut self, on: bool) {
        self.preview_wanted = on;
    }

    /// Follows the state: captures while the camera is on or the preview is
    /// wanted, sends only while the camera is on.
    pub fn sync(&mut self, state: &AppState) {
        let camera_on = state.camera_on;
        let device = state.settings.camera.device.clone();
        let wanted = camera_on || self.preview_wanted;
        let (was_on, was_preview, was_device) = &self.last;
        // Switched on anew, another device, or nothing wanted: a clean slate.
        if (camera_on && !was_on)
            || (self.preview_wanted && !was_preview)
            || device != *was_device
            || !wanted
        {
            self.retry.reset();
        }
        self.last = (camera_on, self.preview_wanted, device.clone());

        if self
            .thread
            .as_ref()
            .is_some_and(|c| c.epoch != state.privacy_epoch)
        {
            self.stop();
            self.retry.reset();
        }

        // A thread that ended by itself: the camera stopped delivering.
        if self.thread.as_ref().is_some_and(|c| c.thread.is_finished()) {
            if let Some(capture) = self.thread.take() {
                let delivered = self.preview.seq.load(Ordering::Acquire) != capture.seq_at_start;
                self.preview.clear();
                match capture.thread.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        if delivered {
                            self.retry.reset();
                        }
                        self.failed(e);
                    }
                    Err(_) => self.failed("camera thread panicked".into()),
                }
            }
        }

        if !wanted {
            if self.running() {
                self.stop();
            }
            return;
        }
        if self
            .thread
            .as_ref()
            .is_some_and(|c| c.device != device || c.epoch != state.privacy_epoch)
        {
            self.stop();
        }
        if !self.running() {
            if !self.retry.may_try(Instant::now()) {
                return;
            }
            let source = if state.privacy_active {
                Ok(Box::new(PrivacySource::default()) as Box<dyn FrameSource>)
            } else {
                (self.opener)(device.as_deref())
            };
            match source {
                Ok(source) => self.spawn(source, device, camera_on, state.privacy_epoch),
                Err(e) => {
                    self.failed(e);
                    return;
                }
            }
        }
        if let Some(capture) = &self.thread {
            capture.live.store(camera_on, Ordering::Relaxed);
            // Pictures arrive: whatever failed before is over.
            if self.retry.failures > 0
                && self.preview.seq.load(Ordering::Acquire) != capture.seq_at_start
            {
                self.retry.reset();
            }
        }
    }

    /// Books a failed start: retry later, or give up — the engine switches
    /// the camera off (if it is on) and shows why.
    fn failed(&mut self, e: String) {
        // A camera without a usable format will not grow one: no retries.
        let unusable = e.starts_with(NO_USABLE_FORMAT);
        if unusable {
            self.retry.failures = MAX_FAILURES;
        } else if self.retry.failed(Instant::now()) {
            tracing::warn!(%e, retry_in = ?backoff(self.retry.failures), "camera");
            return;
        }
        tracing::warn!(%e, failures = self.retry.failures, "camera given up");
        let text = if unusable {
            "Die Kamera liefert kein Bildformat, das ICU lesen kann (MJPEG, YUYV, NV12 oder \
             RGB). Eine andere Kamera wählen."
        } else {
            "Die Kamera lässt sich nicht verwenden. Ist sie angeschlossen und nicht gerade von \
             einem anderen Programm belegt?"
        };
        self.link
            .command(Command::CameraFailed(Problem::new(text, e)));
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
    /// `docs/research/camera-video.md`).
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

    /// A picture that keeps the rate control busy: a moving gradient under
    /// fresh noise every frame, so no tier can sit far below its budget.
    fn busy_frame(i: u32, seed: &mut u32) -> YUVBuffer {
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 17;
                *seed ^= *seed << 5;
                let noise = (*seed % 48) as i32 - 24;
                let base = ((x + y + i as usize * 3) % 200) as i32 + 28;
                let i = (y * w + x) * 3;
                for c in 0..3 {
                    rgb[i + c] = (base + noise + c as i32 * 10).clamp(0, 255) as u8;
                }
            }
        }
        YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (w, h)))
    }

    /// kbit/s `enc` spends on 20 s of busy pictures at `fps`, stamped with
    /// their capture time.
    fn spent_kbps(enc: &mut Encoder, fps: f32) -> f64 {
        const SECONDS: f32 = 20.0;
        let mut seed = 0x9e37_79b9;
        let mut bytes = 0usize;
        let frames = (SECONDS * fps) as u32;
        for i in 0..frames {
            let yuv = busy_frame(i, &mut seed);
            let at = Timestamp::from_millis(((i as f32 * 1000.0) / fps) as u64);
            bytes += enc.encode_at(&yuv, at).unwrap().to_vec().len();
        }
        bytes as f64 * 8.0 / SECONDS as f64 / 1000.0
    }

    /// Every tier spends its own bitrate at its own frame rate. With the
    /// capture rate configured for all of them, openh264 budgeted each frame
    /// for 15 fps and the thinned tiers got ½ to ⅕ of theirs (measured with
    /// this test: T1 50, T2 20, T3 8 kbit/s).
    #[test]
    fn each_tier_spends_its_bitrate() {
        for tier in [VideoTier::T0, VideoTier::T1, VideoTier::T2, VideoTier::T3] {
            let fps = frame_rate(tier);
            assert_eq!(fps * tier.frame_divisor() as f32, FPS as f32);
            let kbps = spent_kbps(&mut encoder(tier).unwrap(), fps);
            let target = f64::from(tier.bitrate()) / 1000.0;
            assert!(
                (0.8 * target..=1.2 * target).contains(&kbps),
                "{tier:?}: {kbps:.1} kbit/s for a target of {target}"
            );
        }
        assert_eq!(frame_rate(VideoTier::T4), 0.0);
    }

    /// MJPEG at the size and rate closest to 320×240 @ 15 fps, where
    /// nokhwa's own `Closest` found nothing (no exact 320×240 MJPEG); then
    /// YUYV, then anything decodable; a grey-only camera gets nothing.
    #[test]
    fn the_capture_format_falls_back_where_nokhwa_gives_up() {
        use nokhwa::utils::{CameraFormat, FrameFormat};
        let f = |w, h, fourcc, fps| CameraFormat::new_from(w, h, fourcc, fps);
        let (mjpeg, yuyv) = (FrameFormat::MJPEG, FrameFormat::YUYV);

        let exact = [
            f(640, 480, mjpeg, 30),
            f(320, 240, mjpeg, 30),
            f(320, 240, mjpeg, 15),
        ];
        assert_eq!(choose_format(&exact), Some(f(320, 240, mjpeg, 15)));

        // No 320×240: the smallest above it, at the rate closest to 15.
        let bigger = [
            f(1280, 720, mjpeg, 30),
            f(640, 480, mjpeg, 30),
            f(640, 480, mjpeg, 10),
            f(160, 120, mjpeg, 15),
        ];
        assert_eq!(choose_format(&bigger), Some(f(640, 480, mjpeg, 10)));
        // Only smaller ones: the largest of them; 20 beats 10 on the tie.
        let smaller = [
            f(160, 120, yuyv, 15),
            f(176, 144, yuyv, 20),
            f(176, 144, yuyv, 10),
        ];
        assert_eq!(choose_format(&smaller), Some(f(176, 144, yuyv, 20)));

        let yuyv_only = [f(640, 480, yuyv, 30), f(320, 240, yuyv, 30)];
        assert_eq!(choose_format(&yuyv_only), Some(f(320, 240, yuyv, 30)));
        let both = [f(320, 240, yuyv, 15), f(1280, 720, mjpeg, 30)];
        assert_eq!(
            choose_format(&both),
            Some(f(1280, 720, mjpeg, 30)),
            "MJPEG first"
        );

        let nv12 = [
            f(640, 480, FrameFormat::GRAY, 15),
            f(640, 480, FrameFormat::NV12, 15),
        ];
        assert_eq!(
            choose_format(&nv12),
            Some(f(640, 480, FrameFormat::NV12, 15))
        );
        assert_eq!(choose_format(&[f(320, 240, FrameFormat::GRAY, 15)]), None);
        assert_eq!(choose_format(&[]), None);
    }

    /// A larger picture comes down to 320×240 from its 4:3 centre, each
    /// output pixel the mean of what it covers; a smaller one stays.
    #[test]
    fn a_larger_capture_is_cut_to_4_3_and_scaled_down() {
        let solid = |w: usize, h: usize, v: u8| RgbFrame {
            captured_at: None,
            timing_accurate: false,
            width: w,
            height: h,
            rgb: vec![v; w * h * 3],
        };
        let small = fit_capture(solid(160, 120, 9));
        assert_eq!((small.width, small.height), (160, 120));

        // 640×480, left half 0, right half 200: halved, the edge stays sharp.
        let mut half = solid(640, 480, 0);
        for y in 0..480 {
            for x in 320..640 {
                half.rgb[(y * 640 + x) * 3..][..3].fill(200);
            }
        }
        let out = fit_capture(half);
        assert_eq!((out.width, out.height), (320, 240));
        assert!(out.is_valid());
        let px = |f: &RgbFrame, x: usize, y: usize| f.rgb[(y * f.width + x) * 3];
        assert_eq!(px(&out, 159, 100), 0);
        assert_eq!(px(&out, 160, 100), 200);

        // 16:9: the bars left and right go, the middle 4:3 is kept.
        let mut wide = solid(1280, 720, 50);
        for y in 0..720 {
            for x in (0..160).chain(1120..1280) {
                wide.rgb[(y * 1280 + x) * 3..][..3].fill(255);
            }
        }
        let out = fit_capture(wide);
        assert_eq!((out.width, out.height), (320, 240));
        assert!(out.rgb.iter().all(|&v| v == 50), "no bar survives the cut");

        // CIF (11:9) loses rows, not columns.
        let cif = fit_capture(solid(352, 288, 77));
        assert_eq!((cif.width, cif.height), (320, 240));
        assert!(cif.is_valid());
    }

    /// How many of `seconds` of pictures at `fps` (± `jitter_ms`) the pacer
    /// encodes at `rate`.
    fn paced(fps: f32, jitter_ms: f32, rate: f32, seconds: f32) -> usize {
        let mut pacer = Pacer::default();
        (0..(seconds * fps) as u32)
            .filter(|&i| {
                let wobble = if i % 2 == 0 { jitter_ms } else { -jitter_ms };
                let ms = (i as f32 * 1000.0 / fps + wobble).max(0.0);
                pacer.take(Duration::from_secs_f32(ms / 1000.0), rate)
            })
            .count()
    }

    /// Each tier gets its own rate from a 15 fps camera, jitter or not; a
    /// faster camera does not raise it, a slower one has every picture sent.
    #[test]
    fn the_pacer_holds_each_tier_to_its_rate() {
        for tier in [VideoTier::T0, VideoTier::T1, VideoTier::T2, VideoTier::T3] {
            let rate = frame_rate(tier);
            let want = (rate * 20.0) as usize;
            for (fps, jitter) in [(15.0, 0.0), (15.0, 12.0), (30.0, 0.0), (30.0, 6.0)] {
                let got = paced(fps, jitter, rate, 20.0);
                assert!(
                    got.abs_diff(want) <= 1,
                    "{tier:?} from {fps} fps ±{jitter} ms: {got} frames, want {want}"
                );
            }
        }
        assert_eq!(paced(5.0, 0.0, frame_rate(VideoTier::T0), 20.0), 100);
        // 30 fps into T0 now spends T0's bitrate, not twice it.
        let mut enc = encoder(VideoTier::T0).unwrap();
        let mut pacer = Pacer::default();
        let mut seed = 0x9e37_79b9;
        let mut bytes = 0usize;
        for i in 0..600u32 {
            let at = Duration::from_millis(u64::from(i) * 1000 / 30);
            let yuv = busy_frame(i, &mut seed);
            if pacer.take(at, frame_rate(VideoTier::T0)) {
                let ts = Timestamp::from_millis(at.as_millis() as u64);
                bytes += enc.encode_at(&yuv, ts).unwrap().to_vec().len();
            }
        }
        let kbps = bytes as f64 * 8.0 / 20.0 / 1000.0;
        assert!((120.0..=180.0).contains(&kbps), "{kbps:.1} kbit/s");
    }

    #[test]
    fn backoff_doubles_up_to_30_s() {
        let secs: Vec<u64> = (1..=8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(backoff(u32::MAX), Duration::from_secs(30));
    }

    /// Once per backoff, not once per window frame; given up after five.
    #[test]
    fn a_failing_start_waits_and_gives_up() {
        let mut retry = Retry::default();
        let t0 = Instant::now();
        assert!(retry.may_try(t0));
        assert!(retry.failed(t0));
        assert!(!retry.may_try(t0 + Duration::from_millis(999)), "backoff");
        assert!(retry.may_try(t0 + Duration::from_secs(1)));
        let mut now = t0;
        for n in 2..MAX_FAILURES {
            now += backoff(n - 1);
            assert!(retry.may_try(now));
            assert!(retry.failed(now), "failure {n} is retried");
        }
        now += backoff(MAX_FAILURES - 1);
        assert!(retry.may_try(now));
        assert!(!retry.failed(now), "the fifth gives up");
        assert!(
            !retry.may_try(now + Duration::from_secs(3600)),
            "until reset"
        );
        retry.reset();
        assert!(retry.may_try(now));
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
            captured_at: None,
            timing_accurate: false,
            width: 5,
            height: 3,
            rgb: pattern(5, 3),
        };
        assert!(!bad.is_valid(), "odd sides");
        let bad = RgbFrame {
            captured_at: None,
            timing_accurate: false,
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

    /// The headers of a one-component baseline JPEG, up to and including
    /// the scan header.
    fn jpeg_headers() -> Vec<u8> {
        let mut jpeg = vec![0xFF, 0xD8];
        // DQT: table 0, 8 bit.
        jpeg.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
        jpeg.extend_from_slice(&[16; 64]);
        // SOF0: 8 bit, 240×320, one component (id 1, 1×1, table 0).
        jpeg.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0xF0, 0x01, 0x40, 0x01, 0x01, 0x11, 0x00,
        ]);
        // DHT: DC table 0 with one 1-bit code, AC table 0 with one 2-bit code.
        jpeg.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x26]);
        jpeg.extend_from_slice(&[0x00, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05]);
        jpeg.extend_from_slice(&[0x10, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xF0]);
        // SOS: one component.
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        jpeg
    }

    /// Position of the first `marker` segment's payload.
    fn payload(jpeg: &[u8], marker: u8) -> usize {
        jpeg.windows(2).position(|w| w == [0xFF, marker]).unwrap() + 4
    }

    /// What libjpeg ends the process over is refused before it gets there;
    /// what it decodes passes, damaged picture data included.
    #[test]
    fn a_damaged_mjpeg_frame_is_refused_before_libjpeg_sees_it() {
        let good = jpeg_headers();
        assert_eq!(jpeg_decodable(&good), Ok(()));
        let mut with_data = good.clone();
        with_data.extend_from_slice(&[0x12, 0x34, 0xFF, 0x00, 0x56]);
        assert_eq!(
            jpeg_decodable(&with_data),
            Ok(()),
            "no end marker: a warning"
        );
        // Stray bytes and fill bytes between segments, an APP0, a comment.
        let mut padded = vec![
            0xFF, 0xD8, 0x00, 0x12, 0xFF, 0xFF, 0xE0, 0x00, 0x04, b'A', b'B',
        ];
        padded.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x03, b'x']);
        padded.extend_from_slice(&good[2..]);
        assert_eq!(jpeg_decodable(&padded), Ok(()));

        assert!(jpeg_decodable(&[]).is_err(), "empty");
        assert!(jpeg_decodable(b"not a jpeg").is_err());
        assert!(jpeg_decodable(&good[2..]).is_err(), "the start is missing");
        for cut in 2..good.len() {
            assert!(jpeg_decodable(&good[..cut]).is_err(), "cut at {cut}");
        }
        let broken = |change: &dyn Fn(&mut Vec<u8>)| {
            let mut jpeg = good.clone();
            change(&mut jpeg);
            jpeg_decodable(&jpeg)
        };
        let (dqt, sof, dht, sos) = (
            payload(&good, 0xDB),
            payload(&good, 0xC0),
            payload(&good, 0xC4),
            payload(&good, 0xDA),
        );
        assert!(
            broken(&|j| j.splice(2..2, [0xFF, 0xD9]).for_each(drop)).is_err(),
            "EOI first"
        );
        assert!(
            broken(&|j| j.splice(2..2, [0xFF, 0xD8]).for_each(drop)).is_err(),
            "SOI twice"
        );
        assert!(broken(&|j| j[dqt] = 0x04).is_err(), "quantization table 4");
        assert!(broken(&|j| j[sof] = 12).is_err(), "12-bit samples");
        assert!(
            broken(&|j| j[sof + 1..sof + 3].fill(0)).is_err(),
            "no height"
        );
        assert!(broken(&|j| j[sof + 7] = 0x51).is_err(), "sampling factor 5");
        assert!(
            broken(&|j| j[sof + 8] = 1).is_err(),
            "table without a definition"
        );
        assert!(broken(&|j| j[sof - 3] = 0xC9).is_err(), "arithmetic coding");
        assert!(broken(&|j| j[dht + 1] = 2).is_err(), "two 1-bit codes");
        assert!(broken(&|j| j[dht + 17] = 16).is_err(), "DC symbol 16");
        assert!(broken(&|j| j[dht] = 0x04).is_err(), "Huffman table 4");
        assert!(broken(&|j| j[sos + 1] = 9).is_err(), "scan of component 9");
        assert!(broken(&|j| j[sos] = 2).is_err(), "scan length");
        // The scan before the frame header.
        let mut early = vec![0xFF, 0xD8];
        early.extend_from_slice(&good[sos - 4..]);
        assert!(jpeg_decodable(&early).is_err());
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
