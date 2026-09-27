//! Video relay and the feedback messages (`Server/IMPLEMENTATION.md` §10:
//! video relay with frame dropping, `LossReport` -> `LossHint`,
//! `KeyframeRequest` forwarding).

mod common;

use std::time::Duration;

use common::{TestServer, BRIEFLY, SOON};
use icu_proto::{
    error as ec,
    header::{VideoHeader, VIDEO_CODEC_H264, VIDEO_HEADER_LEN, VIDEO_MAX_FRAME},
    ControlMsg, UserFlags,
};
use icu_server::room::Counters;

/// §10: video relay with the `speaker` field set.
/// Turns the camera on and waits for the server to echo the flag back, which
/// is the point from which the relay accepts video from this sender.
async fn camera_on(client: &mut common::Client, session: icu_proto::Session) {
    client
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::CAMERA_ON),
        })
        .await;
    client
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session: s, flags } if *s == session && flags.camera_on()),
        )
        .await
        .expect("camera on");
}

/// A client that is looking at video (`UserFlags::VIEWING` from `Hello` on),
/// so the relay forwards frames to it.
async fn viewer(server: &TestServer, name: &str) -> common::Client {
    let identity = server.enroll(name).await;
    let mut client = server.connect(&identity).await.expect("connect");
    client
        .hello_with(name, icu_proto::MINOR, UserFlags(UserFlags::VIEWING))
        .await
        .expect("Welcome");
    client
}

#[tokio::test]
async fn video_reaches_the_other_clients_with_the_speaker_set() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = viewer(&server, "bob").await;

    camera_on(&mut alice, 1).await;

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
    let bob = viewer(&server, "bob").await;

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
    camera_on(&mut alice, 1).await;

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

/// §6.5: video goes only to peers that are viewing; a peer that starts
/// viewing gets the next frame.
#[tokio::test]
async fn video_reaches_only_viewers() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let mut bob = server.client("bob").await;
    let carol = viewer(&server, "carol").await;
    camera_on(&mut alice, 1).await;

    alice.send_video(1, 0, true, &[0x42; 64]).await;
    assert!(carol.recv_video(SOON).await.is_some(), "the viewer gets it");
    assert!(
        bob.recv_video(BRIEFLY).await.is_none(),
        "a non-viewer does not"
    );

    bob.send_msg(&ControlMsg::SetState {
        flags: UserFlags(UserFlags::VIEWING),
    })
    .await;
    let bob_session = bob.session;
    alice
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session, flags } if *session == bob_session && flags.viewing()),
        )
        .await
        .expect("bob is viewing");
    alice.send_video(2, 3200, false, &[0x43; 64]).await;
    let (header, _) = bob.recv_video(SOON).await.expect("now bob gets it");
    assert_eq!(header.seq, 2);
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
    let bob = viewer(&server, "bob").await;
    camera_on(&mut alice, 1).await;

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

/// §2.2: with every frame delivered promptly (no slot displacement), the
/// sender never receives a `VideoCongestionHint`, even across more than one
/// aggregation window.
#[tokio::test]
async fn no_congestion_sends_no_video_congestion_hint() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = viewer(&server, "bob").await;

    camera_on(&mut alice, 1).await;

    // Every frame is read promptly, so nothing is ever displaced; span more
    // than one 2 s aggregation window (`icu_proto::limits::
    // VIDEO_CONGESTION_WINDOW_S`).
    for seq in 0..6u16 {
        alice
            .send_video(seq, u32::from(seq) * 3200, seq == 0, &[7; 64])
            .await;
        assert!(bob.recv_video(SOON).await.is_some());
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    alice.expect_silence().await;
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

/// Opens a video stream, writes the first `bytes` of a frame and stops
/// there, without a FIN: a sender that stalls mid-frame.
async fn stalled_frame(client: &common::Client, bytes: usize) -> quinn::SendStream {
    let header = VideoHeader {
        flags: 0,
        speaker: 0,
        seq: 1,
        ts: 0,
        codec: VIDEO_CODEC_H264,
    };
    let mut frame = header.encode().to_vec();
    frame.extend_from_slice(&[7; 64]);
    let mut send = client.conn.open_uni().await.expect("open_uni");
    send.write_all(&frame[..bytes]).await.expect("write");
    send
}

/// How the server ended a stream: the code it stopped it with.
async fn stop_code(send: &mut quinn::SendStream, within: Duration) -> Option<u32> {
    let stopped = tokio::time::timeout(within, send.stopped())
        .await
        .expect("the server ends the stream in time")
        .expect("stopped, not lost");
    stopped.map(|code| u64::from(code) as u32)
}

/// A frame whose sender stops writing, in the header or in the payload, is
/// dropped after `timing.video_frame_timeout_ms` instead of holding a relay
/// task for as long as the connection lives (`PROTOCOL.md` §4).
#[tokio::test]
async fn a_stalled_video_frame_is_dropped() {
    let server = TestServer::start_with(|c| c.timing.video_frame_timeout_ms = 300).await;
    let mut alice = server.client("alice").await;
    let bob = viewer(&server, "bob").await;
    camera_on(&mut alice, 1).await;

    for bytes in [5, VIDEO_HEADER_LEN + 10] {
        let mut send = stalled_frame(&alice, bytes).await;
        assert_eq!(stop_code(&mut send, SOON).await, Some(ec::RATE_LIMITED));
    }
    let counters = &server.handle.server().counters;
    assert_eq!(Counters::get(&counters.video_dropped_stalled), 2);

    // Nothing else is affected: the next frame goes through.
    alice.send_video(2, 3200, true, &[0x42; 64]).await;
    assert!(bob.recv_video(SOON).await.is_some());
    server.shutdown().await;
}

/// A sender has at most `VIDEO_STREAMS_PER_SENDER` frames in flight on the
/// server: one more stream is refused at once, and the slots come back once
/// the stalled frames time out.
#[tokio::test]
async fn more_frames_in_flight_than_a_camera_sends_are_refused() {
    let server = TestServer::start_with(|c| c.timing.video_frame_timeout_ms = 1500).await;
    let mut alice = server.client("alice").await;
    let bob = viewer(&server, "bob").await;
    camera_on(&mut alice, 1).await;

    let mut stalled = Vec::new();
    for _ in 0..icu_proto::limits::VIDEO_STREAMS_PER_SENDER {
        stalled.push(stalled_frame(&alice, 5).await);
    }
    let mut one_more = stalled_frame(&alice, 5).await;
    assert_eq!(
        stop_code(&mut one_more, Duration::from_millis(1000)).await,
        Some(ec::RATE_LIMITED),
        "refused before any slot frees up"
    );
    for send in &mut stalled {
        assert_eq!(stop_code(send, SOON).await, Some(ec::RATE_LIMITED));
    }
    alice.send_video(2, 3200, true, &[0x42; 64]).await;
    assert!(
        bob.recv_video(SOON).await.is_some(),
        "the slots are free again"
    );
    server.shutdown().await;
}

/// The flow control windows are no size limit: a frame of the largest size
/// the protocol allows still flows, window by window (`PROTOCOL.md` §1, §4).
#[tokio::test]
async fn a_video_frame_of_the_maximum_size_still_flows() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = viewer(&server, "bob").await;
    camera_on(&mut alice, 1).await;

    let payload: Vec<u8> = (0..VIDEO_MAX_FRAME).map(|i| i as u8).collect();
    alice.send_video(1, 0, true, &payload).await;
    let (header, got) = bob.recv_video(SOON).await.expect("the frame arrives");
    assert_eq!(header.speaker, alice.session);
    assert!(got == payload, "all {} bytes, in order", payload.len());
    server.shutdown().await;
}

/// Sends one video frame and returns its stream, to see whether the server
/// stopped it. Unfinished, a stream can only settle by being stopped: a
/// finished small one may be acknowledged before the server reads it.
async fn send_frame(
    client: &common::Client,
    seq: u16,
    len: usize,
    finish: bool,
) -> quinn::SendStream {
    let header = VideoHeader {
        flags: 0,
        speaker: 0,
        seq,
        ts: 0,
        codec: VIDEO_CODEC_H264,
    };
    let mut frame = header.encode().to_vec();
    frame.resize(VIDEO_HEADER_LEN + len, 0x42);
    let mut send = client.conn.open_uni().await.expect("open_uni");
    let _ = send.write_all(&frame).await;
    if finish {
        let _ = send.finish();
    }
    send
}

/// Connects `identity` as `name`, not viewing.
async fn connect_as(
    server: &TestServer,
    identity: &common::Identity,
    name: &str,
) -> common::Client {
    let mut client = server.connect(identity).await.expect("connect");
    client
        .hello_with(name, icu_proto::MINOR, UserFlags::NONE)
        .await
        .expect("Welcome");
    client
}

/// §7: a sender in debt on its byte budget is stopped before the server
/// reads the frame, not after — the budget limits what crosses the wire,
/// not only what is forwarded. A frame larger than the burst still passes
/// while the budget is not spent.
#[tokio::test]
async fn a_sender_in_debt_is_stopped_before_its_frame_is_read() {
    // 1000 bytes/s, 2000 bytes of burst.
    let server = TestServer::start_with(|config| config.limits.video_kbit_s = 8).await;
    let identity = server.enroll("alice").await;
    let mut alice = connect_as(&server, &identity, "alice").await;
    let bob = viewer(&server, "bob").await;
    let session = alice.session;
    camera_on(&mut alice, session).await;

    assert_eq!(
        stop_code(&mut send_frame(&alice, 0, 10_000, true).await, SOON).await,
        None
    );
    assert!(bob.recv_video(SOON).await.is_some(), "the big frame passes");

    let mut refused = send_frame(&alice, 1, 100, false).await;
    // At once, not after the 5 s stall timeout of a frame read in full.
    assert_eq!(
        stop_code(&mut refused, Duration::from_secs(1)).await,
        Some(ec::RATE_LIMITED)
    );
    assert!(bob.recv_video(BRIEFLY).await.is_none());
    server.shutdown().await;
}

/// §7: the budget belongs to the user, not the connection. Reconnecting
/// does not wipe the debt of the connection before.
#[tokio::test]
async fn a_reconnect_does_not_refill_the_video_budget() {
    let server = TestServer::start_with(|config| config.limits.video_kbit_s = 8).await;
    let identity = server.enroll("alice").await;
    let mut alice = connect_as(&server, &identity, "alice").await;
    let bob = viewer(&server, "bob").await;
    let session = alice.session;
    camera_on(&mut alice, session).await;
    assert_eq!(
        stop_code(&mut send_frame(&alice, 0, 10_000, true).await, SOON).await,
        None
    );
    assert!(bob.recv_video(SOON).await.is_some());

    let mut again = connect_as(&server, &identity, "alice").await;
    let session = again.session;
    camera_on(&mut again, session).await;
    let mut refused = send_frame(&again, 0, 100, false).await;
    // At once, not after the 5 s stall timeout of a frame read in full.
    assert_eq!(
        stop_code(&mut refused, Duration::from_secs(1)).await,
        Some(ec::RATE_LIMITED)
    );
    assert!(bob.recv_video(BRIEFLY).await.is_none());
    server.shutdown().await;
}

/// A viewer can withhold all stream credit while keeping control/audio alive.
async fn blocked_viewer(server: &TestServer) -> common::Client {
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_uni_streams(0u32.into());
    viewer_with_transport(server, transport).await
}

async fn viewer_with_transport(
    server: &TestServer,
    transport: quinn::TransportConfig,
) -> common::Client {
    let identity = server.enroll("blocked").await;
    let mut config =
        common::client_config(server.ca(), Some(identity), icu_proto::limits::ALPN_MAIN);
    config.transport_config(std::sync::Arc::new(transport));
    let mut endpoint = common::bare_endpoint(&server.com_key());
    endpoint.set_default_client_config(config);
    let conn = endpoint
        .connect(server.addr(), "localhost")
        .unwrap()
        .await
        .unwrap();
    let (send, recv) = conn.open_bi().await.unwrap();
    let mut client = common::Client {
        endpoint,
        conn,
        send,
        recv,
        session: 0,
        user_id: 0,
        crl: None,
    };
    client
        .hello_with("blocked", icu_proto::MINOR, UserFlags(UserFlags::VIEWING))
        .await
        .expect("Welcome");
    client
}

#[tokio::test]
async fn a_departed_sender_cancels_a_forwarder_waiting_for_stream_credit() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let mut bob = blocked_viewer(&server).await;
    let session = alice.session;
    camera_on(&mut alice, session).await;
    alice.send_video(1, 0, true, &[0x42; 200]).await;
    tokio::time::sleep(BRIEFLY).await;
    alice.conn.close(ec::SHUTDOWN.into(), b"");
    bob.recv_matching(|m| matches!(m, ControlMsg::UserLeft { session: s, .. } if *s == session))
        .await
        .expect("sender left");
    tokio::time::sleep(BRIEFLY).await;
    bob.conn.set_max_concurrent_uni_streams(1u32.into());
    assert!(
        bob.recv_video(BRIEFLY).await.is_none(),
        "no stale frame after the sender left"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_receiver_without_stream_credit_has_a_bounded_frame_wait() {
    let server = TestServer::start_with(|c| c.timing.video_frame_timeout_ms = 100).await;
    let mut alice = server.client("alice").await;
    let bob = blocked_viewer(&server).await;
    let session = alice.session;
    camera_on(&mut alice, session).await;
    alice.send_video(1, 0, true, &[0x42; 200]).await;
    tokio::time::timeout(SOON, async {
        while Counters::get(&server.handle.server().counters.video_dropped_slow_receiver) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("blocked forwarding expires");
    bob.conn.set_max_concurrent_uni_streams(1u32.into());
    assert!(
        bob.recv_video(BRIEFLY).await.is_none(),
        "expired frame is discarded"
    );
    // Bob never got frame 1, so a delta is useless to him until the next
    // keyframe (`PROTOCOL.md` §4): it is dropped, the keyframe passes.
    alice.send_video(2, 3200, false, &[0x43; 200]).await;
    assert!(
        bob.recv_video(BRIEFLY).await.is_none(),
        "a delta after the lost frame is dropped"
    );
    alice.send_video(3, 6400, true, &[0x44; 200]).await;
    assert_eq!(
        bob.recv_video(SOON).await.expect("viewer recovers").0.seq,
        3
    );
    server.shutdown().await;
}

#[tokio::test]
async fn camera_off_during_an_incoming_frame_prevents_forwarding() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = viewer(&server, "bob").await;
    let session = alice.session;
    camera_on(&mut alice, session).await;
    let mut frame = send_frame(&alice, 1, 200, false).await;
    tokio::time::sleep(BRIEFLY).await;
    alice
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags::NONE,
        })
        .await;
    alice.recv_matching(|m| matches!(m, ControlMsg::UserState { session: s, flags } if *s == session && !flags.camera_on()))
        .await.expect("camera off");
    frame.finish().unwrap();
    assert!(
        bob.recv_video(BRIEFLY).await.is_none(),
        "camera permission is checked after reading too"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_receiver_that_does_not_read_has_a_bounded_write_wait() {
    let server = TestServer::start_with(|c| {
        c.timing.video_frame_timeout_ms = 500;
        c.limits.video_kbit_s = 100_000;
    })
    .await;
    let mut alice = server.client("alice").await;
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(64u32.into());
    transport.receive_window(128u32.into());
    let _bob = viewer_with_transport(&server, transport).await;
    let session = alice.session;
    camera_on(&mut alice, session).await;
    // More data than the server's 1 MiB send window. Bob acknowledges only
    // 128 bytes and never reads, so a forwarder eventually blocks in write.
    for seq in 0..3 {
        alice
            .send_video(seq, 0, true, &vec![0x42; VIDEO_MAX_FRAME])
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::timeout(SOON, async {
        while Counters::get(&server.handle.server().counters.video_dropped_slow_receiver) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("blocked write expires without disconnecting its receiver");
    assert!(_bob.conn.close_reason().is_none());
    server.shutdown().await;
}

#[tokio::test]
async fn the_last_viewer_stopping_retires_its_blocked_slot() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let mut bob = blocked_viewer(&server).await;
    let session = alice.session;
    camera_on(&mut alice, session).await;
    alice.send_video(1, 0, true, &[0x42; 200]).await;
    tokio::time::sleep(BRIEFLY).await;
    let viewer_session = bob.session;
    bob.send_msg(&ControlMsg::SetState {
        flags: UserFlags::NONE,
    })
    .await;
    bob.recv_matching(|m| matches!(m, ControlMsg::UserState { session, flags } if *session == viewer_session && !flags.viewing()))
        .await.expect("viewing off");
    alice.send_video(2, 3200, false, &[0x43; 200]).await;
    tokio::time::timeout(SOON, async {
        while Counters::get(&server.handle.server().counters.video_dropped_no_receiver) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the frame without viewers was processed");
    bob.conn.set_max_concurrent_uni_streams(1u32.into());
    assert!(
        bob.recv_video(BRIEFLY).await.is_none(),
        "no queued frame after the last viewer stopped"
    );
    bob.send_msg(&ControlMsg::SetState {
        flags: UserFlags(UserFlags::VIEWING),
    })
    .await;
    bob.recv_matching(|m| matches!(m, ControlMsg::UserState { session, flags } if *session == viewer_session && flags.viewing()))
        .await.expect("viewing on");
    alice.send_video(3, 6400, false, &[0x44; 200]).await;
    assert_eq!(
        bob.recv_video(SOON)
            .await
            .expect("a fresh slot forwards again")
            .0
            .seq,
        3
    );
    server.shutdown().await;
}

/// Waits until the server has taken in `n` video frames in all, and a moment
/// longer for the last one's fan-out: frames on separate streams are relayed
/// by separate tasks, so only this fixes the order they reach the slot in.
async fn settle(server: &TestServer, n: u64) {
    let counters = &server.handle.server().counters;
    tokio::time::timeout(SOON, async {
        while Counters::get(&counters.video_frames_in) < n {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the server takes the frame in");
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// S4 (readiness review 2026-09-27): a waiting keyframe never gives way to
/// a delta, and once a frame of a pair is dropped only a keyframe passes
/// until one is taken in — a delta behind a lost frame is useless to the
/// receiver's decoder. Bob holds back stream credit, so the forwarder sits
/// on frame 1 and every later frame meets the one-deep slot.
#[tokio::test]
async fn after_a_dropped_frame_only_a_keyframe_passes() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = blocked_viewer(&server).await;
    let session = alice.session;
    camera_on(&mut alice, session).await;
    let frames = [
        (1, true),  // taken by the forwarder, waits for credit
        (2, false), // waits in the slot
        (3, false), // dropped: 2 waits (the old rule dropped 2 instead)
        (4, true),  // takes 2's place
        (5, false), // dropped: never replaces a waiting keyframe
        (6, false), // dropped: the pair waits for a keyframe again
    ];
    for (n, (seq, keyframe)) in frames.into_iter().enumerate() {
        alice
            .send_video(seq, u32::from(seq) * 3200, keyframe, &[seq as u8; 200])
            .await;
        settle(&server, n as u64 + 1).await;
    }
    bob.conn.set_max_concurrent_uni_streams(8u32.into());
    let mut got = Vec::new();
    while let Some((header, _)) = bob.recv_video(BRIEFLY).await {
        got.push((header.seq, header.keyframe()));
    }
    assert_eq!(got, vec![(1, true), (4, true)]);

    alice.send_video(7, 7 * 3200, false, &[7; 200]).await;
    assert!(
        bob.recv_video(BRIEFLY).await.is_none(),
        "still waiting for a keyframe"
    );
    alice.send_video(8, 8 * 3200, true, &[8; 200]).await;
    assert_eq!(bob.recv_video(SOON).await.expect("keyframe").0.seq, 8);
    alice.send_video(9, 9 * 3200, false, &[9; 200]).await;
    assert_eq!(
        bob.recv_video(SOON).await.expect("deltas pass again").0.seq,
        9
    );
    server.shutdown().await;
}
