//! Audio relay (`Server/IMPLEMENTATION.md` §10: fan-out with the `speaker`
//! override, mute/deafen, the derived `speaking` bit, datagram rate limit).

mod common;

use std::time::Duration;

use common::{TestServer, BRIEFLY, SOON};
use icu_proto::{ControlMsg, UserFlags};
use icu_server::room::Counters;

/// §10: audio from A arrives at B and C with `speaker = A`, and not at A.
#[tokio::test]
async fn audio_reaches_every_other_client_with_the_speaker_set() {
    let server = TestServer::start().await;
    let alice = server.client("alice").await;
    let bob = server.client("bob").await;
    let carol = server.client("carol").await;
    assert_eq!((alice.session, bob.session, carol.session), (1, 2, 3));

    alice.send_audio(7, 960, &[0xAA; 60]);

    for client in [&bob, &carol] {
        let (header, payload) = client.recv_audio(SOON).await.expect("the datagram arrives");
        assert_eq!(header.speaker, alice.session, "the server sets `speaker`");
        assert_eq!(header.seq, 7, "seq is passed through unchanged");
        assert_eq!(header.ts, 960);
        assert_eq!(payload, vec![0xAA; 60]);
    }
    assert!(
        alice.recv_audio(BRIEFLY).await.is_none(),
        "a sender never hears itself"
    );
    server.shutdown().await;
}

/// §10: a deafened client receives nothing.
#[tokio::test]
async fn a_deafened_client_gets_no_audio() {
    let server = TestServer::start().await;
    let alice = server.client("alice").await;
    let mut bob = server.client("bob").await;
    let carol = server.client("carol").await;

    bob.send_msg(&ControlMsg::SetState {
        flags: UserFlags(UserFlags::DEAFENED),
    })
    .await;
    bob.recv_matching(|m| matches!(m, ControlMsg::UserState { session: 2, .. }))
        .await
        .expect("UserState");

    alice.send_audio(1, 0, &[1, 2, 3]);
    assert!(carol.recv_audio(SOON).await.is_some(), "carol still hears");
    assert!(bob.recv_audio(BRIEFLY).await.is_none(), "bob is deafened");
    server.shutdown().await;
}

/// §10: a muted sender is not relayed.
#[tokio::test]
async fn a_muted_sender_is_not_relayed() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = server.client("bob").await;

    alice
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::MUTED),
        })
        .await;
    alice
        .recv_matching(|m| matches!(m, ControlMsg::UserState { session: 1, .. }))
        .await
        .expect("UserState");

    alice.send_audio(1, 0, &[1, 2, 3]);
    assert!(bob.recv_audio(BRIEFLY).await.is_none());
    assert!(
        Counters::get(&server.handle.server().counters.dropped_muted) >= 1,
        "the drop is counted"
    );
    server.shutdown().await;
}

/// `PROTOCOL.md` §3: datagrams below 11 or above 410 bytes, and unknown
/// `kind` values, are dropped without touching the connection.
#[tokio::test]
async fn malformed_datagrams_are_dropped_silently() {
    let server = TestServer::start().await;
    let alice = server.client("alice").await;
    let bob = server.client("bob").await;

    let _ = alice.conn.send_datagram(vec![0x01; 10].into()); // too short
    let _ = alice.conn.send_datagram(vec![0x03; 50].into()); // unknown kind
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(bob.recv_audio(BRIEFLY).await.is_none());

    // The connection is unharmed: a valid datagram still goes through.
    alice.send_audio(1, 0, &[9; 20]);
    assert!(bob.recv_audio(SOON).await.is_some());
    assert!(Counters::get(&server.handle.server().counters.dropped_malformed) >= 2);
    server.shutdown().await;
}

/// `PROTOCOL.md` §2.1: the server derives `speaking` from the datagram flow.
#[tokio::test]
async fn speaking_is_derived_from_the_datagram_flow() {
    let server = TestServer::start_with(|config| {
        config.timing.speaking_silence_ms = 200;
    })
    .await;
    let alice = server.client("alice").await;
    let mut bob = server.client("bob").await;

    alice.send_audio(1, 0, &[1; 20]);
    let msg = bob
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session: 1, flags } if flags.speaking()),
        )
        .await;
    assert!(msg.is_some(), "speaking goes to 1 on the first datagram");

    // The END flag ends the spurt immediately.
    alice.send_audio_flags(2, 960, &[1; 20], icu_proto::header::AUDIO_FLAG_END);
    let msg = bob
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session: 1, flags } if !flags.speaking()),
        )
        .await;
    assert!(msg.is_some(), "END clears speaking");
    server.shutdown().await;
}

/// `PROTOCOL.md` §2.1: silence alone ends the talk spurt too.
#[tokio::test]
async fn speaking_expires_after_silence() {
    let server = TestServer::start_with(|config| {
        config.timing.speaking_silence_ms = 200;
    })
    .await;
    let alice = server.client("alice").await;
    let mut bob = server.client("bob").await;

    alice.send_audio(1, 0, &[1; 20]);
    bob.recv_matching(
        |m| matches!(m, ControlMsg::UserState { session: 1, flags } if flags.speaking()),
    )
    .await
    .expect("speaking on");

    let msg = bob
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session: 1, flags } if !flags.speaking()),
        )
        .await;
    assert!(msg.is_some(), "speaking clears without an END flag");
    server.shutdown().await;
}

/// `PROTOCOL.md` §3: the audio datagram rate limit drops datagrams and leaves
/// the connection alone.
#[tokio::test]
async fn audio_over_the_datagram_rate_limit_is_dropped() {
    let server = TestServer::start_with(|config| {
        config.limits.audio_datagrams_per_s = 10;
    })
    .await;
    let alice = server.client("alice").await;
    let bob = server.client("bob").await;

    for seq in 0..40u16 {
        alice.send_audio(seq, u32::from(seq) * 960, &[5; 40]);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut received = 0;
    while bob.recv_audio(BRIEFLY).await.is_some() {
        received += 1;
    }
    assert!((1..=15).contains(&received), "got {received}");
    assert!(Counters::get(&server.handle.server().counters.dropped_rate) > 0);
    server.shutdown().await;
}
