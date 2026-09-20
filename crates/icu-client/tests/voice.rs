//! Integration tests with an in-process server and the fake audio backend
//! (`Client/IMPLEMENTATION.md` §14).

mod common;

use std::time::Duration;

use common::*;
use icu_client::{
    audio::fake::FakeBackend,
    config::Identity,
    dsp::testsignal::{best_lag, harmonic_voice, pearson},
    engine::{Command, ConnStatus},
    stats::DebugStats,
};
use icu_proto::ControlMsg;

/// Enrollment → connect → Hello/Welcome; the identity is persisted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enroll_connect_welcome() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::silent();
    let (engine, dir) = server.engine("alice", Box::new(backend), false).await;
    let state = wait_connected(&engine).await;
    let me = state.me.expect("own entry");
    assert_eq!(me.name, "alice");
    assert!(me.user_id >= 1);
    assert!(me.session >= 1);
    assert!(state.peers.is_empty());
    assert!(state.update_required.is_none());

    let paths = icu_client::config::Paths::new(Some(dir.path().to_path_buf()));
    let identity = Identity::load(&paths).unwrap().expect("identity stored");
    assert_eq!(identity.server.name, "alice");
    assert_eq!(identity.server.server_name, "localhost");

    engine.shutdown();
    server.handle.shutdown().await;
}

/// Two clients: A sends the test tone, B decodes it; the playback output
/// correlates with the source signal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audio_from_a_arrives_decoded_at_b() {
    let server = TestServer::start().await;
    let (backend_a, _probe_a) = FakeBackend::silent();
    let (engine_a, _dir_a) = server.engine("alice", Box::new(backend_a), true).await;
    let (backend_b, probe_b) = FakeBackend::silent();
    let (engine_b, _dir_b) = server.engine("bob", Box::new(backend_b), false).await;
    wait_connected(&engine_a).await;
    let state_b = wait_for(&engine_b, SOON, |s| {
        s.conn == ConnStatus::Connected && s.peers.len() == 1
    })
    .await;
    assert_eq!(state_b.peers[0].info.name, "alice");

    // Let audio flow for a while.
    probe_b.clear();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let played = probe_b.played();
    let stats_a = engine_a.stats();
    let stats_b = engine_b.stats();
    assert!(
        DebugStats::get(&stats_a.datagrams_sent) > 50,
        "A sent {}",
        DebugStats::get(&stats_a.datagrams_sent)
    );
    assert!(
        DebugStats::get(&stats_b.datagrams_received) > 50,
        "B received {}",
        DebugStats::get(&stats_b.datagrams_received)
    );
    assert!(
        DebugStats::get(&stats_b.frames_decoded) > 50,
        "B decoded {}",
        DebugStats::get(&stats_b.frames_decoded)
    );
    assert!(played.len() > 48_000, "B played {} samples", played.len());

    // B speaks-indicator from the local datagram path.
    let state_b = wait_for(&engine_b, SOON, |s| s.peers.iter().any(|p| p.speaking)).await;
    assert!(state_b.peers[0].speaking);

    // Correlate one second of B's output against the 4 s source loop.
    let source = harmonic_voice(48_000 * 4, 0.15);
    let window = &played[24_000..48_000];
    let coarse = best_lag(window, &source, source.len() - window.len(), 16);
    let lo = coarse.saturating_sub(16);
    let fine = lo + best_lag(window, &source[lo..], 32, 1);
    let c = pearson(window, &source[fine..fine + window.len()]);
    assert!(c > 0.7, "correlation {c:.3} at lag {fine}");

    engine_a.shutdown();
    engine_b.shutdown();
    server.handle.shutdown().await;
}

/// The client reconnects after a server restart with backoff and gets a new
/// session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_after_server_restart() {
    let server = TestServer::start().await;
    let addr = server.addr();
    let (backend, _probe) = FakeBackend::silent();
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    let first = wait_connected(&engine).await;
    let session1 = first.me.as_ref().unwrap().session;

    let server = server.restart(addr).await;
    let disconnected = wait_for(&engine, SOON, |s| {
        matches!(
            s.conn,
            ConnStatus::Disconnected { .. } | ConnStatus::Connecting
        )
    })
    .await;
    if let ConnStatus::Disconnected { reason, .. } = &disconnected.conn {
        assert!(reason.contains("SHUTDOWN"), "reason {reason}");
    }
    let again = wait_for(&engine, Duration::from_secs(20), |s| {
        s.conn == ConnStatus::Connected
    })
    .await;
    let me = again.me.unwrap();
    assert_eq!(me.session, 1, "session numbering restarts with the server");
    assert_eq!(session1, 1);
    assert_eq!(me.name, "alice", "name survives in users.toml");
    assert!(DebugStats::get(&engine.stats().reconnects) >= 1);

    engine.shutdown();
    server.handle.shutdown().await;
}

/// A `LossReport` from a listener becomes a `LossHint` that sets the
/// encoder's `PACKET_LOSS_PERC` (rounded up, capped at 20).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loss_hint_sets_packet_loss_perc() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::silent();
    let (engine, _dir) = server.engine("alice", Box::new(backend), true).await;
    let state = wait_connected(&engine).await;
    let speaker = state.me.unwrap().session;
    assert_eq!(engine.packet_loss_perc(), 0);

    let mut bob = server.raw_client("bob").await;
    bob.send(&ControlMsg::LossReport {
        speaker,
        loss_permille: 123,
    })
    .await;
    assert!(
        wait_until(SOON, || engine.packet_loss_perc() == 13).await,
        "loss perc {}",
        engine.packet_loss_perc()
    );

    bob.send(&ControlMsg::LossReport {
        speaker,
        loss_permille: 950,
    })
    .await;
    assert!(wait_until(SOON, || engine.packet_loss_perc() == 20).await);

    engine.shutdown();
    server.handle.shutdown().await;
}

/// Mute/deafen go out as `SetState` and come back as `UserState`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mute_and_deafen_roundtrip_through_the_server() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::silent();
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    let state = wait_connected(&engine).await;
    let alice = state.me.unwrap().session;
    let mut bob = server.raw_client("bob").await;

    engine.command(Command::SetMuted(true));
    let msg = bob
        .recv_matching(
            SOON,
            |m| matches!(m, ControlMsg::UserState { session, .. } if *session == alice),
        )
        .await
        .expect("UserState");
    match msg {
        ControlMsg::UserState { flags, .. } => assert!(flags.muted() && !flags.deafened()),
        _ => unreachable!(),
    }
    let state = wait_for(&engine, SOON, |s| {
        s.me.as_ref().map(|m| m.flags.muted()) == Some(true)
    })
    .await;
    assert!(state.muted);

    engine.command(Command::SetDeafened(true));
    let msg = bob
        .recv_matching(SOON, |m| matches!(m, ControlMsg::UserState { session, flags } if *session == alice && flags.deafened()))
        .await
        .expect("UserState deafened");
    assert!(matches!(msg, ControlMsg::UserState { .. }));

    engine.command(Command::SetName("alicia".into()));
    let msg = bob
        .recv_matching(SOON, |m| matches!(m, ControlMsg::UserRenamed { .. }))
        .await
        .expect("UserRenamed");
    assert_eq!(
        msg,
        ControlMsg::UserRenamed {
            session: alice,
            name: "alicia".into()
        }
    );
    let state = wait_for(&engine, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("alicia")
    })
    .await;
    assert_eq!(state.settings.ui.name.as_deref(), Some("alicia"));

    // Taken name → NAME_TAKEN error shown, name unchanged.
    engine.command(Command::SetName("bob".into()));
    let state = wait_for(&engine, SOON, |s| s.name_error.is_some()).await;
    assert_eq!(state.me.unwrap().name, "alicia");

    engine.shutdown();
    server.handle.shutdown().await;
}
