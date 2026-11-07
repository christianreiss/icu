//! Privacy guards the actual media paths, independently of saved user choices.
#![cfg(unix)]
mod common;

use common::*;
use icu_client::{audio::fake::FakeBackend, engine::Command, stats::DebugStats};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn privacy_guards_audio_and_survives_hotkeys_settings_and_reconnect() {
    let server = TestServer::start().await;
    let (alice, _a) = server
        .engine("alice", Box::new(FakeBackend::silent().0), true)
        .await;
    let (bob, _b) = server
        .engine("bob", Box::new(FakeBackend::silent().0), true)
        .await;
    wait_connected(&alice).await;
    wait_connected(&bob).await;
    assert!(wait_until(SOON, || DebugStats::get(&alice.stats().datagrams_sent) > 5).await);

    alice.command(Command::SetPrivacy(true));
    let state = wait_for(&alice, SOON, |s| s.privacy_active).await;
    assert!(!state.muted, "privacy does not overwrite the user's mute");
    assert!(!state.settings.audio.muted);
    assert!(!state.camera_on, "a disabled camera stays disabled");
    wait_for(&bob, SOON, |s| s.peers.iter().any(|p| p.info.flags.muted())).await;
    let before = DebugStats::get(&alice.stats().datagrams_sent);
    let received = DebugStats::get(&alice.stats().datagrams_received);
    alice.command(Command::ToggleMute);
    alice.command(Command::SetMuted(false));
    alice.command(Command::ToggleCamera);
    alice.command(Command::SetCamera(true));
    alice.command(Command::UpdateSettings(Box::new(state.settings)));
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(DebugStats::get(&alice.stats().datagrams_sent), before);
    assert!(
        DebugStats::get(&alice.stats().datagrams_received) > received,
        "incoming audio continues"
    );
    assert!(!alice.snapshot().camera_on);

    let session = alice.snapshot().me.unwrap().session;
    server
        .handle
        .admin(icu_server::AdminRequest::Kick {
            user: "alice".into(),
        })
        .await;
    wait_for(&alice, SOON, |s| {
        matches!(s.conn, icu_client::engine::ConnStatus::Fatal(_))
    })
    .await;
    alice.command(Command::Reconnect);
    wait_for(&alice, SOON, |s| {
        s.me.as_ref().is_some_and(|u| u.session != session)
    })
    .await;
    let state = wait_connected(&alice).await;
    assert!(state.privacy_active);
    assert!(
        state.me.unwrap().flags.muted(),
        "Hello advertises effective privacy mute"
    );
    let before = DebugStats::get(&alice.stats().datagrams_sent);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(DebugStats::get(&alice.stats().datagrams_sent), before);

    alice.command(Command::SetSetupActive(true));
    alice.command(Command::SetPrivacy(false));
    wait_for(&alice, SOON, |s| !s.privacy_active && s.setup_active).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        DebugStats::get(&alice.stats().datagrams_sent),
        before,
        "setup guard survives leaving privacy"
    );
    alice.command(Command::SetSetupActive(false));
    assert!(
        wait_until(SOON, || DebugStats::get(&alice.stats().datagrams_sent)
            > before + 5)
        .await
    );
    assert!(!alice.snapshot().settings.audio.muted);
    alice.shutdown();
    bob.shutdown();
    server.handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn privacy_preserves_previous_mute_deafen_and_new_admin_restrictions() {
    let server = TestServer::start().await;
    let (alice, _a) = server
        .engine("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&alice).await;
    for muted in [false, true] {
        alice.command(Command::SetMuted(muted));
        alice.command(Command::SetPrivacy(true));
        wait_for(&alice, SOON, |s| s.privacy_active).await;
        alice.command(Command::SetPrivacy(false));
        let state = wait_for(&alice, SOON, |s| !s.privacy_active).await;
        assert_eq!(state.muted, muted);
        assert_eq!(state.settings.audio.muted, muted);
    }
    alice.command(Command::SetMuted(false));
    alice.command(Command::SetPrivacy(true));
    alice.command(Command::SetDeafened(true));
    alice.command(Command::SetPrivacy(false));
    let state = wait_for(&alice, SOON, |s| !s.privacy_active && s.deafened).await;
    assert!(state.muted);
    alice.command(Command::SetDeafened(false));
    wait_for(&alice, SOON, |s| !s.muted).await;
    alice.command(Command::SetPrivacy(true));
    wait_for(&alice, SOON, |s| s.privacy_active).await;
    server
        .handle
        .admin(icu_server::AdminRequest::Mute {
            user: "alice".into(),
        })
        .await;
    wait_for(&alice, SOON, |s| s.restricted_muted).await;
    alice.command(Command::SetPrivacy(false));
    let state = wait_for(&alice, SOON, |s| !s.privacy_active).await;
    assert!(state.muted && state.restricted_muted);
    alice.shutdown();
    server.handle.shutdown().await;
}

#[cfg(feature = "camera")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn privacy_replaces_sent_and_self_video_and_releases_the_camera() {
    use icu_client::camera::{CameraHandle, FrameSource, RgbFrame};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct Red(Arc<AtomicUsize>);
    impl FrameSource for Red {
        fn next_frame(&mut self) -> Result<RgbFrame, String> {
            std::thread::sleep(Duration::from_millis(66));
            Ok(RgbFrame {
                captured_at: None,
                timing_accurate: false,
                width: 320,
                height: 240,
                rgb: [220, 20, 20].repeat(320 * 240),
            })
        }
    }
    impl Drop for Red {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let server = TestServer::start().await;
    let (alice, _a) = server
        .engine("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    let (bob, _b) = server
        .engine("bob", Box::new(FakeBackend::silent().0), false)
        .await;
    let session = wait_connected(&alice).await.me.unwrap().session;
    wait_connected(&bob).await;
    let opened = Arc::new(AtomicUsize::new(0));
    let count = opened.clone();
    let mut camera = CameraHandle::with_opener(
        alice.camera_link(),
        Box::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Red(count.clone())))
        }),
    );
    alice.command(Command::SetCamera(true));
    let state = wait_for(&alice, SOON, |s| s.camera_on).await;
    camera.sync(&state);
    assert!(
        wait_until(SOON, || bob
            .video_frames()
            .iter()
            .any(|(s, f)| *s == session && f.rgba[0] > f.rgba[1] + 80))
        .await
    );
    let old_source = alice.camera_link();
    alice.command(Command::SetPrivacy(true));
    let state = wait_for(&alice, SOON, |s| s.privacy_active).await;
    assert!(
        old_source.send_frame(true, 0, &[0; 4]).is_err(),
        "unbound real sources cannot send in privacy"
    );
    camera.sync(&state);
    assert_eq!(opened.load(Ordering::SeqCst), 0, "physical source released");
    let preview = camera.preview_source();
    assert!(
        wait_until(SOON, || preview.frame.lock().unwrap().as_ref().is_some_and(
            |f| f
                .rgb
                .as_chunks::<3>()
                .0
                .iter()
                .all(|p| p[0] == p[1] && p[1] == p[2])
        ))
        .await
    );
    let first = preview.frame.lock().unwrap().clone().unwrap();
    assert!(
        wait_until(SOON, || bob
            .video_frames()
            .iter()
            .any(|(s, f)| *s == session
                && f.rgba[0].abs_diff(f.rgba[1]) < 8
                && f.rgba[0].abs_diff(f.rgba[2]) < 8))
        .await,
        "peer decodes the same grayscale privacy source"
    );
    assert!(
        wait_until(SOON, || preview
            .frame
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|f| f.rgb != first.rgb))
        .await,
        "static animates"
    );
    // The central label is steady, with both bright letters and dark backing.
    let band = &first.rgb[109 * 320 * 3..130 * 320 * 3];
    assert!(band.contains(&240) && band.contains(&18));
    if let Ok(dir) = std::env::var("ICU_PRIVACY_SHOTS") {
        image::save_buffer(
            std::path::Path::new(&dir).join("privacy-source.png"),
            &first.rgb,
            320,
            240,
            image::ColorType::Rgb8,
        )
        .unwrap();
        let frames = bob.video_frames();
        let (_, frame) = frames.iter().find(|(s, _)| *s == session).unwrap();
        image::save_buffer(
            std::path::Path::new(&dir).join("privacy-received.png"),
            frame.rgba.as_ref(),
            320,
            240,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
    alice.command(Command::SetPrivacy(false));
    let state = wait_for(&alice, SOON, |s| !s.privacy_active).await;
    camera.sync(&state);
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    assert!(state.camera_on && state.settings.camera.enabled);
    assert!(
        wait_until(SOON, || bob
            .video_frames()
            .iter()
            .any(|(s, f)| *s == session && f.rgba[0] > f.rgba[1] + 80))
        .await
    );
    camera.stop();
    // Preview-only also uses static: privacy must never reopen the physical camera.
    alice.command(Command::SetCamera(false));
    alice.command(Command::SetPrivacy(true));
    let state = wait_for(&alice, SOON, |s| s.privacy_active && !s.camera_on).await;
    camera.set_preview(true);
    camera.sync(&state);
    assert_eq!(opened.load(Ordering::SeqCst), 0);
    camera.stop();
    alice.shutdown();
    bob.shutdown();
    server.handle.shutdown().await;
}
