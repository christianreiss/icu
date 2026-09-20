//! Video relay and the feedback messages (`Server/IMPLEMENTATION.md` §10:
//! video relay with frame dropping, `LossReport` -> `LossHint`,
//! `KeyframeRequest` forwarding).

mod common;

use std::time::Duration;

use common::{TestServer, BRIEFLY, SOON};
use icu_proto::{ControlMsg, UserFlags};
use icu_server::room::Counters;

/// §10: video relay with the `speaker` field set.
#[tokio::test]
async fn video_reaches_the_other_clients_with_the_speaker_set() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = server.client("bob").await;

    alice
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::CAMERA_ON),
        })
        .await;
    alice
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session: 1, flags } if flags.camera_on()),
        )
        .await
        .expect("camera on");

    alice.send_video(3, 1920, true, &[0x42; 200]).await;
    let (header, payload) = bob.recv_video(SOON).await.expect("the frame arrives");
    assert_eq!(header.speaker, alice.session);
    assert_eq!(header.seq, 3);
    assert!(header.keyframe());
    assert_eq!(header.codec, icu_proto::header::VIDEO_CODEC_H264);
    assert_eq!(payload, vec![0x42; 200]);
    server.shutdown().await;
}

/// §6.5: without `camera_on` a frame is not relayed.
#[tokio::test]
async fn video_without_camera_on_is_dropped() {
    let server = TestServer::start().await;
    let alice = server.client("alice").await;
    let bob = server.client("bob").await;

    alice.send_video(1, 0, true, &[0x42; 200]).await;
    assert!(bob.recv_video(BRIEFLY).await.is_none());
    assert!(Counters::get(&server.handle.server().counters.video_dropped_not_allowed) >= 1);
    server.shutdown().await;
}

/// §10 / §6.5: with nobody to forward to, the stream is still drained so the
/// sender's flow control keeps moving.
#[tokio::test]
async fn a_video_frame_without_receivers_is_drained() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    alice
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::CAMERA_ON),
        })
        .await;
    alice
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session: 1, flags } if flags.camera_on()),
        )
        .await
        .expect("camera on");

    // More frames than `max_concurrent_uni_streams` would allow if they were
    // left open: this only completes because every stream is drained.
    for seq in 0..40u16 {
        alice
            .send_video(seq, u32::from(seq) * 3200, seq == 0, &[7; 64])
            .await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let counters = &server.handle.server().counters;
    assert!(
        Counters::get(&counters.video_dropped_no_receiver) > 0,
        "frames without a receiver are counted"
    );
    server.shutdown().await;
}

/// §10: the video frame rate limit drops frames instead of the connection.
#[tokio::test]
async fn video_over_the_frame_rate_limit_is_dropped() {
    let server = TestServer::start_with(|config| {
        config.limits.video_frames_per_s = 3;
    })
    .await;
    let mut alice = server.client("alice").await;
    let bob = server.client("bob").await;
    alice
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::CAMERA_ON),
        })
        .await;
    alice
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session: 1, flags } if flags.camera_on()),
        )
        .await
        .expect("camera on");

    for seq in 0..12u16 {
        alice
            .send_video(seq, u32::from(seq) * 3200, seq == 0, &[7; 64])
            .await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;

    let mut received = 0;
    while bob.recv_video(BRIEFLY).await.is_some() {
        received += 1;
    }
    assert!(
        received <= 4,
        "at most the burst of 3 (plus refill) passes, got {received}"
    );
    assert!(Counters::get(&server.handle.server().counters.video_dropped_rate) > 0);
    // The sender is still connected.
    assert!(alice.recv_timeout(BRIEFLY).await.is_none());
    server.shutdown().await;
}

/// §10: `LossReport` becomes a `LossHint` for the speaker.
#[tokio::test]
async fn a_loss_report_becomes_a_loss_hint_for_the_speaker() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let mut bob = server.client("bob").await;
    let mut carol = server.client("carol").await;

    bob.send_msg(&ControlMsg::LossReport {
        speaker: alice.session,
        loss_permille: 30,
    })
    .await;
    let hint = alice
        .recv_matching(|m| matches!(m, ControlMsg::LossHint { .. }))
        .await;
    assert!(
        matches!(hint, Some(ControlMsg::LossHint { loss_permille: 30 })),
        "{hint:?}"
    );

    // The maximum over all receivers wins.
    carol
        .send_msg(&ControlMsg::LossReport {
            speaker: alice.session,
            loss_permille: 90,
        })
        .await;
    let hint = alice
        .recv_matching(|m| matches!(m, ControlMsg::LossHint { .. }))
        .await;
    assert!(
        matches!(hint, Some(ControlMsg::LossHint { loss_permille: 90 })),
        "{hint:?}"
    );
    server.shutdown().await;
}

/// §10: once every report has expired the speaker gets `LossHint { 0 }`.
#[tokio::test]
async fn expired_loss_reports_send_a_zero_hint() {
    let server = TestServer::start_with(|config| {
        config.timing.loss_report_ttl_ms = 300;
    })
    .await;
    let mut alice = server.client("alice").await;
    let mut bob = server.client("bob").await;

    bob.send_msg(&ControlMsg::LossReport {
        speaker: alice.session,
        loss_permille: 50,
    })
    .await;
    let hint = alice
        .recv_matching(|m| matches!(m, ControlMsg::LossHint { .. }))
        .await;
    assert!(matches!(
        hint,
        Some(ControlMsg::LossHint { loss_permille: 50 })
    ));

    let hint = alice
        .recv_matching(|m| matches!(m, ControlMsg::LossHint { loss_permille: 0 }))
        .await;
    assert!(
        hint.is_some(),
        "the expiry of every report is itself a change"
    );
    server.shutdown().await;
}

/// §10: `KeyframeRequest` is forwarded to the speaker with `speaker = 0`.
#[tokio::test]
async fn a_keyframe_request_is_forwarded_to_the_speaker() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let mut bob = server.client("bob").await;

    bob.send_msg(&ControlMsg::KeyframeRequest {
        speaker: alice.session,
    })
    .await;
    let request = alice
        .recv_matching(|m| matches!(m, ControlMsg::KeyframeRequest { .. }))
        .await;
    assert!(
        matches!(request, Some(ControlMsg::KeyframeRequest { speaker: 0 })),
        "the speaker does not learn who asked: {request:?}"
    );

    // At most one per second per speaker.
    bob.send_msg(&ControlMsg::KeyframeRequest {
        speaker: alice.session,
    })
    .await;
    alice.expect_silence().await;
    server.shutdown().await;
}
