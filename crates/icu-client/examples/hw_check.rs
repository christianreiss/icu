//! Unattended hardware checks: PipeWire devices and streams, evdev devices,
//! camera frame + H.264 encode. Prints one line per check.

use std::time::Duration;
#[cfg(feature = "camera")]
use std::time::Instant;

use icu_client::audio::{AudioBackend, StreamKind};

fn main() {
    // PipeWire
    match icu_client::audio::pipewire::PipeWireBackend::new() {
        Ok(mut b) => {
            let devices = b.devices();
            println!("pipewire: {} devices", devices.len());
            for d in &devices {
                println!("  {:?} {} [{}]", d.kind, d.description, d.id);
            }
            let cap = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let c = cap.clone();
            let r = b.open_capture(
                None,
                Box::new(move |blk| {
                    c.fetch_add(blk.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }),
            );
            println!("pipewire capture open: {r:?}");
            let r = b.open_playback(None, Box::new(|out| out.fill(0.0)));
            println!("pipewire playback open: {r:?}");
            std::thread::sleep(Duration::from_millis(500));
            println!(
                "pipewire captured {} samples in 0.5 s",
                cap.load(std::sync::atomic::Ordering::Relaxed)
            );
            b.close(StreamKind::Capture);
            b.close(StreamKind::Playback);
        }
        Err(e) => println!("pipewire: {e}"),
    }

    // evdev
    let devs = icu_client::hotkey::evdev::list_devices();
    println!(
        "evdev: {} devices, {} keyboards, {} mice",
        devs.len(),
        devs.iter().filter(|d| d.keyboard).count(),
        devs.iter().filter(|d| d.mouse).count()
    );
    for d in devs.iter().filter(|d| d.keyboard || d.mouse) {
        println!(
            "  {} {} kbd={} mouse={}",
            d.path.display(),
            d.name,
            d.keyboard,
            d.mouse
        );
    }

    // Camera
    #[cfg(feature = "camera")]
    {
        use icu_client::camera::{FrameSource, NokhwaSource};
        let cams = icu_client::camera::list();
        println!("cameras: {cams:?}");
        match NokhwaSource::open(0) {
            Ok(mut src) => {
                let t = Instant::now();
                match src.next_frame() {
                    Ok(f) => {
                        println!(
                            "camera frame {}x{} after {:?}",
                            f.width,
                            f.height,
                            t.elapsed()
                        );
                        let mut enc = icu_client::camera::encoder(icu_client::video::VideoTier::T0)
                            .expect("encoder");
                        let yuv = openh264::formats::YUVBuffer::from_rgb_source(
                            openh264::formats::RgbSliceU8::new(&f.rgb, (f.width, f.height)),
                        );
                        let t = Instant::now();
                        let bs = enc.encode(&yuv).expect("encode");
                        println!(
                            "h264 encode: {:?} {} bytes in {:?}",
                            bs.frame_type(),
                            bs.to_vec().len(),
                            t.elapsed()
                        );
                    }
                    Err(e) => println!("camera frame: {e}"),
                }
            }
            Err(e) => println!("camera: {e}"),
        }
    }
}
