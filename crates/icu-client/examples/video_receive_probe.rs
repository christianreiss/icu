//! Live video acceptance probe. Uses an existing test identity, records only
//! frame counts/geometry, and never writes camera images to disk.
//! Usage: cargo run -p icu-client --example video_receive_probe -- CONFIG_DIR SECONDS

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use icu_client::{
    audio::fake::FakeBackend,
    config::{Identity, Paths, Settings},
    engine::{Engine, EngineConfig},
};

fn main() {
    let mut args = std::env::args().skip(1);
    let paths = Paths::new(Some(args.next().expect("CONFIG_DIR").into()));
    let seconds: u64 = args.next().expect("SECONDS").parse().expect("seconds");
    assert!(seconds >= 10, "observe at least ten seconds");
    let identity = Identity::load(&paths)
        .expect("load identity")
        .expect("registered identity");
    let mut settings = Settings::load(&paths).expect("settings");
    settings.camera.enabled = false;
    let engine = Engine::start(
        EngineConfig::new(identity, settings, paths),
        Box::new(FakeBackend::silent().0),
    )
    .expect("engine");
    let frames = engine.video_frames_source();
    let start = Instant::now();
    let mut seen = HashMap::new();
    let mut decoded = 0;
    let mut nonuniform = 0;
    let mut report = Instant::now();
    let mut last_frame = None;
    while start.elapsed() < Duration::from_secs(seconds) {
        for (session, seq) in frames.seqs() {
            if seen.get(&session) == Some(&seq) {
                continue;
            }
            let Some((actual_seq, frame)) = frames.picture(session) else {
                continue;
            };
            assert!(frame.width > 0 && frame.height > 0);
            assert_eq!(frame.rgba.len(), frame.width * frame.height * 4);
            if frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel != &frame.rgba[..4])
            {
                nonuniform += 1;
            }
            seen.insert(session, actual_seq);
            decoded += 1;
            last_frame = Some(Instant::now());
            if decoded == 1 {
                println!("first decoded frame: {}x{}", frame.width, frame.height);
            }
        }
        if report.elapsed() >= Duration::from_secs(2) {
            println!("elapsed={:.1}s decoded={decoded} nonuniform={nonuniform} fresh_streams={} connection={:?}", start.elapsed().as_secs_f64(), frames.seqs().len(), engine.snapshot().conn);
            report = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let fresh = last_frame.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
    engine.shutdown();
    assert!(decoded >= 30 && nonuniform >= 30 && fresh,
        "insufficient live decode evidence: decoded={decoded} nonuniform={nonuniform} fresh={fresh}");
    println!("PASS: decoded={decoded} nonuniform={nonuniform} fresh={fresh}");
}
