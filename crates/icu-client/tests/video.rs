//! Video: a synthetic camera sends H.264 over the relay, a `KeyframeRequest`
//! forces an IDR (`docs/reference/client.md` §14).
//!
//! Unix only: the in-process server (`common`) is.
#![cfg(unix)]
#![cfg(feature = "camera")]

mod common;

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use common::*;
use icu_client::{
    audio::fake::FakeBackend,
    camera::{CameraHandle, FrameSource, Opener, SyntheticSource},
    engine::{AppState, Command},
    problem::Problem,
    stats::DebugStats,
};
use icu_proto::ControlMsg;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keyframe_request_produces_an_idr() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::silent();
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    let state = wait_connected(&engine).await;
    let alice = state.me.unwrap().session;

    // Camera on: SetState(camera_on) so the server relays our frames.
    engine.command(Command::SetCamera(true));
    wait_for(&engine, SOON, |s| s.camera_on).await;
    let mut camera = CameraHandle::new(engine.camera_link());
    camera.start_with(Box::new(SyntheticSource::new()));
    // Alone, the camera captures for the self-view but sends nothing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        DebugStats::get(&engine.stats().video_sent),
        0,
        "no video while alone"
    );

    let bob = server.raw_client("bob").await;
    // Bob joined after the camera started. Until then nobody was viewing and
    // the camera was paused, so his first frame comes from a fresh encoder.
    let (first, _) = bob.recv_video(SOON).await.expect("a video frame arrives");
    assert_eq!(first.speaker, alice);
    assert_eq!(first.codec, icu_proto::header::VIDEO_CODEC_H264);
    assert!(
        first.media_epoch > 0 && first.media_time > 0,
        "capture timing survives relay"
    );

    let forced_before = DebugStats::get(&engine.stats().keyframes_forced);
    let mut bob = bob;
    bob.send(&ControlMsg::KeyframeRequest { speaker: alice })
        .await;
    // Within a few frames an IDR must arrive.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut got_idr = false;
    while tokio::time::Instant::now() < deadline {
        let Some((header, data)) = bob.recv_video(Duration::from_secs(1)).await else {
            continue;
        };
        if header.keyframe() {
            assert!(
                data.windows(5).any(|w| w == [0, 0, 0, 1, 0x67]),
                "IDR carries an SPS (Annex-B)"
            );
            got_idr = true;
            break;
        }
    }
    assert!(got_idr, "no IDR after KeyframeRequest");
    assert!(
        wait_until(SOON, || DebugStats::get(&engine.stats().keyframes_forced)
            > forced_before)
        .await,
        "the encoder was asked for an IDR"
    );
    assert!(DebugStats::get(&engine.stats().video_sent) >= 2);

    camera.stop();
    engine.shutdown();
    server.handle.shutdown().await;
}

/// The camera switched off in the engine — a hotkey while the window is in
/// the tray — sends no further frame, although the window, whose
/// `CameraHandle::sync` stops the thread's sending, never ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_the_camera_off_stops_sending_without_the_window() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::silent();
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    wait_connected(&engine).await;
    engine.command(Command::SetCamera(true));
    wait_for(&engine, SOON, |s| s.camera_on).await;
    let mut camera = CameraHandle::new(engine.camera_link());
    camera.start_with(Box::new(SyntheticSource::new()));
    let bob = server.raw_client("bob").await;
    bob.recv_video(SOON).await.expect("frames flow");

    engine.command(Command::ToggleCamera);
    wait_for(&engine, SOON, |s| !s.camera_on).await;
    let stats = engine.stats();
    let sent = DebugStats::get(&stats.video_sent);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        DebugStats::get(&stats.video_sent),
        sent,
        "frames after the camera went off"
    );

    camera.stop();
    engine.shutdown();
    server.handle.shutdown().await;
}

/// Frames from a peer are decoded per speaker and exposed to the overlay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn received_video_is_decoded_per_speaker() {
    let server = TestServer::start().await;
    let (backend_a, _pa) = FakeBackend::silent();
    let (engine_a, _da) = server.engine("alice", Box::new(backend_a), false).await;
    let (backend_b, _pb) = FakeBackend::silent();
    let (engine_b, _db) = server.engine("bob", Box::new(backend_b), false).await;
    let state_a = wait_connected(&engine_a).await;
    wait_connected(&engine_b).await;
    let alice = state_a.me.unwrap().session;

    engine_a.command(Command::SetCamera(true));
    wait_for(&engine_a, SOON, |s| s.camera_on).await;
    let mut camera = CameraHandle::new(engine_a.camera_link());
    camera.start_with(Box::new(SyntheticSource::new()));

    assert!(
        wait_until(SOON, || engine_b
            .video_frames()
            .iter()
            .any(|(s, f)| *s == alice && f.width == 320 && f.height == 240))
        .await,
        "bob decodes alice's frames: {:?}",
        engine_b.video_frames().len()
    );
    let frames = engine_b.video_frames();
    let (_, f) = frames.iter().find(|(s, _)| *s == alice).unwrap();
    assert_eq!(f.rgba.len(), 320 * 240 * 4);
    assert!(
        f.stamp.micros > 0 && f.stamp.epoch > 0,
        "decode retains original capture time"
    );
    assert!(f.rgba.iter().any(|b| *b != 0));
    assert!(DebugStats::get(&engine_b.stats().video_received) >= 1);

    camera.stop();
    engine_a.shutdown();
    engine_b.shutdown();
    server.handle.shutdown().await;
}

/// The `seq` the participant list uses to skip redundant texture uploads
/// advances with every decoded picture, and a speaker that stops drops out
/// of `seqs` so no stale picture sticks in a row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn video_seq_advances_and_expires() {
    let server = TestServer::start().await;
    let (backend_a, _pa) = FakeBackend::silent();
    let (engine_a, _da) = server.engine("alice", Box::new(backend_a), false).await;
    let (backend_b, _pb) = FakeBackend::silent();
    let (engine_b, _db) = server.engine("bob", Box::new(backend_b), false).await;
    let state_a = wait_connected(&engine_a).await;
    wait_connected(&engine_b).await;
    let alice = state_a.me.unwrap().session;

    engine_a.command(Command::SetCamera(true));
    wait_for(&engine_a, SOON, |s| s.camera_on).await;
    let mut camera = CameraHandle::new(engine_a.camera_link());
    camera.start_with(Box::new(SyntheticSource::new()));

    let frames = engine_b.video_frames_source();
    assert!(
        wait_until(SOON, || frames.seqs().iter().any(|(s, _)| *s == alice)).await,
        "alice shows up with a picture"
    );
    let (seq, picture) = frames.picture(alice).expect("a picture");
    assert_eq!(picture.rgba.len(), 320 * 240 * 4);
    // `>=`, not `==`: the two calls take the lock separately, so a decode
    // may land between them.
    assert!(
        frames.seqs().iter().any(|(s, q)| *s == alice && *q >= seq),
        "`seqs` and `picture` report the same counter"
    );
    assert!(
        wait_until(SOON, || frames
            .seqs()
            .iter()
            .any(|(s, q)| *s == alice && *q > seq))
        .await,
        "the next decoded picture advances the seq"
    );

    // Nothing arrives any more: after the freshness window the row has to
    // fall back to the initials instead of freezing on the last picture.
    camera.stop();
    assert!(
        wait_until(Duration::from_secs(6), || frames.seqs().is_empty()).await,
        "a stopped speaker leaves `seqs`"
    );
    assert!(frames.picture(alice).is_none());

    engine_a.shutdown();
    engine_b.shutdown();
    server.handle.shutdown().await;
}

/// Opens the test pattern for any device and counts how often it was asked.
fn synthetic_opener(opened: Arc<AtomicUsize>) -> Opener {
    Box::new(
        move |_: Option<&str>| -> Result<Box<dyn FrameSource>, String> {
            opened.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(SyntheticSource::new()))
        },
    )
}

/// The preview in Settings → Kamera captures without telling anyone:
/// nothing is sent and `camera_on` stays off. Switching the camera on sends
/// from the same capture; switching it off again goes back to preview only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_preview_captures_without_sending() {
    let server = TestServer::start().await;
    let (backend_a, _pa) = FakeBackend::silent();
    let (engine_a, _da) = server.engine("alice", Box::new(backend_a), false).await;
    let (backend_b, _pb) = FakeBackend::silent();
    let (engine_b, _db) = server.engine("bob", Box::new(backend_b), false).await;
    wait_connected(&engine_a).await;
    wait_connected(&engine_b).await;
    // Bob watches (no window counts as watching): sending is not paused
    // for want of an audience.
    wait_for(&engine_a, SOON, |s| {
        s.peers.iter().any(|p| p.info.flags.viewing())
    })
    .await;

    let opened = Arc::new(AtomicUsize::new(0));
    let mut camera =
        CameraHandle::with_opener(engine_a.camera_link(), synthetic_opener(opened.clone()));
    let preview = camera.preview_source();
    camera.set_preview(true);
    camera.sync(&engine_a.snapshot());
    assert!(
        wait_until(SOON, || preview.seq.load(Ordering::Acquire) >= 5).await,
        "the preview shows pictures"
    );
    assert_eq!(
        DebugStats::get(&engine_a.stats().video_sent),
        0,
        "nothing sent"
    );
    assert!(!engine_a.snapshot().camera_on, "and nobody was told");

    engine_a.command(Command::SetCamera(true));
    let on = wait_for(&engine_a, SOON, |s| s.camera_on).await;
    camera.sync(&on);
    assert!(
        wait_until(SOON, || DebugStats::get(&engine_a.stats().video_sent) > 0).await,
        "sends once the camera is on"
    );
    assert_eq!(opened.load(Ordering::Relaxed), 1, "from the same capture");

    engine_a.command(Command::SetCamera(false));
    let off = wait_for(&engine_a, SOON, |s| !s.camera_on).await;
    camera.sync(&off);
    assert!(camera.running(), "the preview goes on");
    let sent = DebugStats::get(&engine_a.stats().video_sent);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        DebugStats::get(&engine_a.stats().video_sent) <= sent + 1,
        "at most the frame in flight"
    );
    camera.set_preview(false);
    camera.sync(&off);
    assert!(!camera.running(), "preview closed, camera off: stopped");

    engine_a.shutdown();
    engine_b.shutdown();
    server.handle.shutdown().await;
}

/// A camera that will not open is retried with backoff — not on every call
/// (the window calls `sync` about 30 times a second).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_camera_that_will_not_open_is_retried_with_backoff() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::silent();
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    wait_connected(&engine).await;
    engine.command(Command::SetCamera(true));
    let on = wait_for(&engine, SOON, |s| s.camera_on).await;

    let tries = Arc::new(AtomicUsize::new(0));
    let counted = tries.clone();
    let mut camera = CameraHandle::with_opener(
        engine.camera_link(),
        Box::new(
            move |_: Option<&str>| -> Result<Box<dyn FrameSource>, String> {
                counted.fetch_add(1, Ordering::Relaxed);
                Err("no such camera".into())
            },
        ),
    );
    for _ in 0..30 {
        camera.sync(&on);
    }
    assert_eq!(tries.load(Ordering::Relaxed), 1, "once, then back off");
    assert_eq!(camera.failures(), 1);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    for _ in 0..30 {
        camera.sync(&on);
    }
    assert_eq!(tries.load(Ordering::Relaxed), 2, "again after a second");
    assert_eq!(camera.failures(), 2);

    // Switched off and on again: a fresh start, no waiting.
    let off = AppState {
        camera_on: false,
        ..on.clone()
    };
    camera.sync(&off);
    camera.sync(&on);
    assert_eq!(tries.load(Ordering::Relaxed), 3);
    assert_eq!(camera.failures(), 1);

    engine.shutdown();
    server.handle.shutdown().await;
}

/// A camera that offers no format ICU can decode is not retried: it says so
/// at once, in its own words, and the camera goes off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_camera_without_a_usable_format_says_so_at_once() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::silent();
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    wait_connected(&engine).await;
    engine.command(Command::SetCamera(true));
    let on = wait_for(&engine, SOON, |s| s.camera_on).await;

    let tries = Arc::new(AtomicUsize::new(0));
    let counted = tries.clone();
    let mut camera = CameraHandle::with_opener(
        engine.camera_link(),
        Box::new(
            move |_: Option<&str>| -> Result<Box<dyn FrameSource>, String> {
                counted.fetch_add(1, Ordering::Relaxed);
                Err(format!("{}: GRAY", icu_client::camera::NO_USABLE_FORMAT))
            },
        ),
    );
    camera.sync(&on);
    let off = wait_for(&engine, SOON, |s| !s.camera_on).await;
    let banner = off.banner.expect("banner");
    assert!(banner.text.contains("kein Bildformat"), "{}", banner.text);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    camera.sync(&on);
    assert_eq!(tries.load(Ordering::Relaxed), 1, "not retried");

    engine.shutdown();
    server.handle.shutdown().await;
}

/// Given up: the camera goes off and the banner says why. The camera hotkey
/// toggles it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_camera_is_switched_off_with_a_reason() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::silent();
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    wait_connected(&engine).await;

    engine.command(Command::ToggleCamera);
    wait_for(&engine, SOON, |s| s.camera_on).await;
    engine.command(Command::CameraFailed(Problem::plain(
        "Die Kamera lässt sich nicht verwenden.",
    )));
    let off = wait_for(&engine, SOON, |s| !s.camera_on).await;
    assert_eq!(
        off.banner.map(|b| b.text).as_deref(),
        Some("Die Kamera lässt sich nicht verwenden.")
    );
    engine.command(Command::ToggleCamera);
    wait_for(&engine, SOON, |s| s.camera_on).await;

    engine.shutdown();
    server.handle.shutdown().await;
}

/// Camera startup is independent of audio startup: transmitted capture time
/// still belongs to the engine clock, not a newly zeroed camera clock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_camera_start_keeps_the_audio_engine_clock() {
    let server = TestServer::start().await;
    let (backend, _) = FakeBackend::silent();
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    wait_connected(&engine).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    engine.command(Command::SetCamera(true));
    wait_for(&engine, SOON, |s| s.camera_on).await;
    let mut camera = CameraHandle::new(engine.camera_link());
    camera.start_with(Box::new(SyntheticSource::new()));
    let bob = server.raw_client("bob").await;
    let (first, _) = bob.recv_video(SOON).await.expect("camera sends");
    assert!(
        first.media_time >= 500_000,
        "camera must not reset capture time: {first:?}"
    );
    let epoch = first.media_epoch;
    camera.stop();
    camera.start_with(Box::new(SyntheticSource::new()));
    let (again, _) = bob.recv_video(SOON).await.expect("camera resumes");
    assert_eq!(again.media_epoch, epoch);
    assert!(again.media_time >= first.media_time);
    camera.stop();
    engine.shutdown();
    server.handle.shutdown().await;
}

/// Opt-in local hardware evidence: camera images remain in memory and travel
/// only through the loopback test server. Audio is synthetic, never a microphone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ICU_VIDEO_TEST_CAMERA; local camera, no image recording"]
async fn native_camera_processing_and_timing_remain_live() {
    let index: u32 = std::env::var("ICU_VIDEO_TEST_CAMERA")
        .expect("camera index")
        .parse()
        .unwrap();
    let server = TestServer::start().await;
    let (backend, _) = FakeBackend::silent();
    let (a, _ad) = server
        .engine("native-camera-source", Box::new(backend), true)
        .await;
    let (backend, _) = FakeBackend::silent();
    let (b, _bd) = server
        .engine("local-only-viewer", Box::new(backend), false)
        .await;
    let state = wait_connected(&a).await;
    wait_connected(&b).await;
    let session = state.me.unwrap().session;
    tokio::time::sleep(Duration::from_secs(1)).await;
    a.command(Command::SetCamera(true));
    wait_for(&a, SOON, |s| s.camera_on).await;
    let mut camera = CameraHandle::new(a.camera_link());
    camera.start_with(Box::new(
        icu_client::camera::NokhwaSource::open(index).expect("open native camera"),
    ));
    let mut count = 0;
    let mut last = 0;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        if let Some((seq, picture)) = b.video_frames_source().picture(session) {
            if seq != last {
                assert!(picture.stamp.micros > 500_000);
                assert!(picture.stamp.epoch > 0);
                count += 1;
                last = seq;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let stats = a.stats();
    let receive = b.stats();
    eprintln!("native camera decoded={count} processing={} budget_bypasses={} camera_timing_estimated={} decode_offset_us={} queue={}",stats.video_processing.lock().unwrap().summary(),DebugStats::get(&stats.video_processing_bypassed),DebugStats::get(&stats.video_timing_estimated),receive.av_offset_us.load(Ordering::Relaxed),receive.video_queue_age.lock().unwrap().summary());
    camera.stop();
    a.shutdown();
    b.shutdown();
    server.handle.shutdown().await;
    assert!(
        count >= 100,
        "native camera pipeline did not remain live: {count}"
    );
}
