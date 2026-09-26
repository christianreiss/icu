//! Spike 8: pipewire-rs 0.10 against PipeWire 1.4.11.
//!
//! Opens capture, playback and the AEC reference stream through the real
//! backend, plays a −20 dBFS 440 Hz tone for a few seconds and reports what
//! the graph delivered: block sizes, callback intervals, whether the monitor
//! stream sees the tone. Run `pw-top -b -n 3` alongside to read the quantum.
//!
//! Linux only (PipeWire, evdev); elsewhere it says so and ends.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("pw_spike: nur unter Linux");
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use icu_client::audio::{
        pipewire::PipeWireBackend, AudioBackend, DeviceKind, StreamKind, SAMPLE_RATE,
    };

    #[derive(Default)]
    struct Stats {
        intervals_us: Vec<f64>,
        sizes: Vec<usize>,
        rms_sum: f64,
        blocks: u64,
        last: Option<Instant>,
    }

    impl Stats {
        fn block(&mut self, data: &[f32]) {
            let now = Instant::now();
            if let Some(last) = self.last {
                self.intervals_us.push((now - last).as_secs_f64() * 1e6);
            }
            self.last = Some(now);
            self.sizes.push(data.len());
            let e: f64 = data.iter().map(|s| (*s as f64) * (*s as f64)).sum();
            self.rms_sum += (e / data.len().max(1) as f64).sqrt();
            self.blocks += 1;
        }

        fn report(&self, name: &str) {
            let mut iv = self.intervals_us.clone();
            iv.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mut sizes = self.sizes.clone();
            sizes.sort();
            sizes.dedup();
            let p = |q: f64| {
                if iv.is_empty() {
                    0.0
                } else {
                    iv[((iv.len() - 1) as f64 * q) as usize] / 1000.0
                }
            };
            println!(
                "{name:14} blocks={} sizes={:?} interval ms p50={:.2} p99={:.2} max={:.2} mean rms={:.4}",
                self.blocks,
                sizes,
                p(0.5),
                p(0.99),
                p(1.0),
                self.rms_sum / self.blocks.max(1) as f64
            );
        }
    }

    pub fn main() {
        let secs: u64 = std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
        let mut backend = PipeWireBackend::new().expect("pipewire backend");
        println!("devices:");
        for d in backend.devices() {
            println!(
                "  {:6} {}  [{}]",
                match d.kind {
                    DeviceKind::Input => "input",
                    DeviceKind::Output => "output",
                },
                d.description,
                d.id
            );
        }

        let cap = Arc::new(Mutex::new(Stats::default()));
        let refr = Arc::new(Mutex::new(Stats::default()));
        let play = Arc::new(Mutex::new(Stats::default()));

        let capture_device = std::env::args().nth(2);
        let c = cap.clone();
        backend
            .open_capture(
                capture_device.as_deref(),
                Box::new(move |data| c.lock().unwrap().block(data)),
            )
            .expect("open capture");
        let r = refr.clone();
        backend
            .open_reference(None, Box::new(move |data| r.lock().unwrap().block(data)))
            .expect("open reference");
        let p = play.clone();
        let mut phase = 0.0f32;
        backend
            .open_playback(
                None,
                Box::new(move |out| {
                    for s in out.iter_mut() {
                        *s = (phase * std::f32::consts::TAU).sin() * 0.1;
                        phase += 440.0 / SAMPLE_RATE as f32;
                        if phase >= 1.0 {
                            phase -= 1.0;
                        }
                    }
                    p.lock().unwrap().block(out);
                }),
            )
            .expect("open playback");

        println!("running {secs} s with a 440 Hz tone on the default sink…");
        std::thread::sleep(Duration::from_secs(secs));
        backend.close(StreamKind::Playback);
        backend.close(StreamKind::Reference);
        backend.close(StreamKind::Capture);
        std::thread::sleep(Duration::from_millis(100));

        cap.lock().unwrap().report("capture");
        refr.lock().unwrap().report("reference");
        play.lock().unwrap().report("playback");
        let ref_rms = {
            let r = refr.lock().unwrap();
            r.rms_sum / r.blocks.max(1) as f64
        };
        println!(
            "reference sees playback tone (A4): {}",
            if ref_rms > 0.01 { "YES" } else { "NO" }
        );
    }
}
