//! Video: a synthetic camera sends H.264 over the relay, a `KeyframeRequest`
//! forces an IDR (`Client/IMPLEMENTATION.md` §14).

#![cfg(feature = "camera")]

mod common;

use std::time::Duration;

use common::*;
use icu_client::{
    audio::fake::FakeBackend,
    camera::{CameraHandle, SyntheticSource},
    engine::Command,
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

    let bob = server.raw_client("bob").await;
    // Bob joined after the camera started. Until then nobody was viewing and
    // the camera was paused, so his first frame comes from a fresh encoder.
    let (first, _) = bob.recv_video(SOON).await.expect("a video frame arrives");
    assert_eq!(first.speaker, alice);
    assert_eq!(first.codec, icu_proto::header::VIDEO_CODEC_H264);

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
