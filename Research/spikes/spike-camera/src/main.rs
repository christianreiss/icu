//! Spike 5: Kamera-Capture (nokhwa/V4L2) + H.264 (openh264, Source-Build) bei 320x240@15.
use anyhow::Result;
use nokhwa::{pixel_format::RgbFormat, utils::{CameraIndex, RequestedFormat, RequestedFormatType, Resolution, CameraFormat, FrameFormat}, Camera};
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let cams = nokhwa::query(nokhwa::utils::ApiBackend::Auto)?;
    for c in &cams { println!("camera: idx={} name={} desc={}", c.index(), c.human_name(), c.description()); }
    let idx = CameraIndex::Index(0);
    let want = CameraFormat::new(Resolution::new(320, 240), FrameFormat::MJPEG, 15);
    let req = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(want));
    let mut cam = Camera::new(idx, req)?;
    println!("negotiated: {:?}", cam.camera_format());
    if let Ok(fmts) = cam.compatible_camera_formats() { for f in fmts.iter().filter(|f| f.width() <= 640).take(12) { println!("  supports {:?}", f); } }
    cam.open_stream()?;
    let w = cam.resolution().width(); let h = cam.resolution().height();

    let cfg = openh264::encoder::EncoderConfig::new().bitrate(openh264::encoder::BitRate::from_bps(150_000)).max_frame_rate(openh264::encoder::FrameRate::from_hz(15.0)).usage_type(openh264::encoder::UsageType::CameraVideoRealTime);
    let mut enc = openh264::encoder::Encoder::with_api_config(openh264::OpenH264API::from_source(), cfg)?;
    let mut n = 0; let mut bytes = 0usize; let mut mjpeg_bytes = 0usize; let mut cap_t = Duration::ZERO; let mut enc_t = Duration::ZERO; let mut dec_t = Duration::ZERO; let mut key = 0;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(4) {
        let a = Instant::now();
        let frame = cam.frame()?;
        cap_t += a.elapsed();
        mjpeg_bytes += frame.buffer().len();
        let d = Instant::now();
        let rgb = frame.decode_image::<RgbFormat>()?;
        dec_t += d.elapsed();
        let b = Instant::now();
        let yuv = openh264::formats::YUVBuffer::from_rgb_source(openh264::formats::RgbSliceU8::new(rgb.as_raw(), (w as usize, h as usize)));
        let bs = enc.encode(&yuv)?;
        enc_t += b.elapsed();
        let v = bs.to_vec(); bytes += v.len();
        if matches!(bs.frame_type(), openh264::encoder::FrameType::IDR | openh264::encoder::FrameType::I) { key += 1; }
        n += 1;
    }
    let secs = t0.elapsed().as_secs_f64();
    println!("{}x{}: {} frames in {:.1}s = {:.1} fps | raw-from-cam (MJPEG) {:.0} kbps | h264 {:.0} kbps ({} keyframes) | cam.frame() wait avg {:?} | mjpeg decode avg {:?} | rgb->yuv+encode avg {:?}",
        w, h, n, secs, n as f64 / secs, mjpeg_bytes as f64 * 8.0 / secs / 1000.0, bytes as f64 * 8.0 / secs / 1000.0, key, cap_t / n.max(1), dec_t / n.max(1), enc_t / n.max(1));
    Ok(())
}
