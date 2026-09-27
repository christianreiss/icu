//! Server release gate: real client codecs exchange audio and video together.
#![cfg(all(unix, feature = "camera"))]

mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::*;
use icu_client::{
    audio::fake::FakeBackend,
    camera::{CameraHandle, SyntheticSource},
    engine::{Command, ConnStatus},
    stats::DebugStats,
};
use icu_server::room::Counters;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_audio_and_video_survive_concurrent_transmission() {
    let server = TestServer::start().await;
    let (audio_a, probe_a) = FakeBackend::silent();
    let (audio_b, probe_b) = FakeBackend::silent();
    let (alice, _dir_a) = server.engine("alice", Box::new(audio_a), true).await;
    let (bob, _dir_b) = server.engine("bob", Box::new(audio_b), true).await;
    let a = wait_connected(&alice).await.me.unwrap().session;
    let b = wait_connected(&bob).await.me.unwrap().session;

    alice.command(Command::SetCamera(true));
    bob.command(Command::SetCamera(true));
    wait_for(&alice, SOON, |s| s.camera_on && s.peers.len() == 1).await;
    wait_for(&bob, SOON, |s| s.camera_on && s.peers.len() == 1).await;
    let mut camera_a = CameraHandle::new(alice.camera_link());
    let mut camera_b = CameraHandle::new(bob.camera_link());
    camera_a.start_with(Box::new(SyntheticSource::new()));
    camera_b.start_with(Box::new(SyntheticSource::new()));
    assert!(
        wait_until(SOON, || {
            alice.video_frames().iter().any(|(s, _)| *s == b)
                && bob.video_frames().iter().any(|(s, _)| *s == a)
        })
        .await,
        "both clients decode their peer's H.264"
    );

    let stats_a = alice.stats();
    let stats_b = bob.stats();
    let before_a = DebugStats::get(&stats_a.datagrams_received);
    let before_b = DebugStats::get(&stats_b.datagrams_received);
    let video_a = DebugStats::get(&stats_a.video_received);
    let video_b = DebugStats::get(&stats_b.video_received);
    probe_a.clear();
    probe_b.clear();
    tokio::time::sleep(Duration::from_secs(5)).await;
    for (name, engine, stats, audio_before, video_before, probe) in [
        ("alice", &alice, &stats_a, before_a, video_a, &probe_a),
        ("bob", &bob, &stats_b, before_b, video_b, &probe_b),
    ] {
        let audio = DebugStats::get(&stats.datagrams_received) - audio_before;
        let video = DebugStats::get(&stats.video_received) - video_before;
        println!("{name}: {audio} audio packets and {video} video frames in 5 s");
        assert_eq!(engine.snapshot().conn, ConnStatus::Connected);
        assert_eq!(
            stats.video_tier.load(Ordering::Relaxed),
            0,
            "{name}: highest supported video tier"
        );
        assert!(
            audio >= 200,
            "{name}: audio continues at roughly 50 packets/s"
        );
        assert!(video >= 60, "{name}: video continues while audio flows");
        assert!(
            probe.played().iter().any(|sample| sample.abs() > 0.01),
            "{name}: decoded audio reaches playback"
        );
        let frames = engine.video_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!((frames[0].1.width, frames[0].1.height), (320, 240));
        assert_eq!(frames[0].1.rgba.len(), 320 * 240 * 4);
    }
    let counters = &server.handle.server().counters;
    assert_eq!(Counters::get(&counters.dropped_rate), 0);
    assert_eq!(Counters::get(&counters.dropped_malformed), 0);
    assert_eq!(Counters::get(&counters.video_dropped_rate), 0);
    assert_eq!(Counters::get(&counters.video_dropped_slow_receiver), 0);
    camera_a.stop();
    camera_b.stop();
    alice.shutdown();
    bob.shutdown();
    server.handle.shutdown().await;
}
