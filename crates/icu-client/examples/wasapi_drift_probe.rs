//! Measures delivered microphone/loopback sample-rate drift without recording
//! microphone audio. Run on Windows: wasapi_drift_probe INPUT_MATCH OUTPUT_MATCH SECONDS.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows WASAPI endpoints.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    use icu_client::audio::{wasapi::WasapiBackend, AudioBackend, BackendEvent, DeviceKind};
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };
    tracing_subscriber::fmt()
        .with_env_filter("icu_client::audio=info")
        .init();
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 3, "INPUT_MATCH OUTPUT_MATCH SECONDS");
    let seconds: u64 = args[2].parse().expect("seconds");
    assert!(seconds >= 600, "measure at least ten minutes");
    let mut backend = WasapiBackend::new().expect("WASAPI");
    let devices = backend.devices();
    let find = |kind, needle: &str| {
        let matches: Vec<_> = devices
            .iter()
            .filter(|d| d.kind == kind && d.description.contains(needle))
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one {kind:?} matching {needle:?}: {devices:?}"
        );
        matches[0]
    };
    let input = find(DeviceKind::Input, &args[0]);
    let output = find(DeviceKind::Output, &args[1]);
    println!("input={} output={}", input.description, output.description);
    let capture = Arc::new(AtomicU64::new(0));
    let reference = Arc::new(AtomicU64::new(0));
    let c = capture.clone();
    backend
        .open_capture(
            Some(&input.id),
            Box::new(move |s| {
                c.fetch_add(s.len() as u64, Ordering::Relaxed);
            }),
        )
        .expect("capture");
    // Keep the render endpoint active, without playing a tone or capturing content.
    backend
        .open_playback(Some(&output.id), Box::new(|s| s.fill(0.0)))
        .expect("playback");
    let r = reference.clone();
    backend
        .open_reference(
            Some(&output.id),
            Box::new(move |s, _channels| {
                r.fetch_add(s.len() as u64, Ordering::Relaxed);
            }),
        )
        .expect("reference");
    let events = backend.take_events().unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let start = Instant::now();
    let c0 = capture.load(Ordering::Relaxed);
    let r0 = reference.load(Ordering::Relaxed);
    let (mut n, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let mut previous = (c0, r0);
    loop {
        for event in events.try_iter() {
            assert!(
                !matches!(
                    event,
                    BackendEvent::StreamError { .. }
                        | BackendEvent::StreamDiscontinuity { .. }
                        | BackendEvent::DefaultDeviceChanged
                ),
                "drift measurement lost its continuous device timeline: {event:?}"
            );
        }
        std::thread::sleep(Duration::from_secs(1));
        let t = start.elapsed().as_secs_f64();
        let c = capture.load(Ordering::Relaxed);
        let r = reference.load(Ordering::Relaxed);
        assert!(
            c > previous.0 && r > previous.1,
            "a stream stopped delivering at {t:.3}s: capture {c} (previous {}), reference {r} (previous {})", previous.0, previous.1
        );
        previous = (c, r);
        let delta = (c - c0) as f64 - (r - r0) as f64;
        n += 1.0;
        sx += t;
        sy += delta;
        sxx += t * t;
        sxy += t * delta;
        println!(
            "seconds={t:.3} capture={} reference={} delta={delta:.0}",
            c - c0,
            r - r0
        );
        if t >= seconds as f64 {
            break;
        }
    }
    let ppm = (n * sxy - sx * sy) / (n * sxx - sx * sx) / 48_000.0 * 1_000_000.0;
    drop(backend);
    println!("RESULT delivered_sample_drift_ppm={ppm:.2} (linear fit, capture minus reference)");
    assert!(
        ppm.is_finite() && ppm.abs() < 100.0,
        "drift exceeds the measured AEC acceptance range"
    );
}
