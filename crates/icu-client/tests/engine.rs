//! Engine behaviour against an in-process server: what reaches the server of
//! the local state, and how mute, deafen and admin restrictions interact
//! (`Client/IMPLEMENTATION.md` §5, §10).

mod common;

use std::time::Duration;

use common::*;
use icu_client::{audio::fake::FakeBackend, engine::Command};
use icu_proto::{ControlMsg, Session};

/// Every `UserState` bob receives for `session` until the stream has been
/// quiet for `quiet`; the last one is what the server holds.
async fn user_states(bob: &mut RawClient, session: Session, quiet: Duration) -> Vec<(bool, bool)> {
    let mut seen = Vec::new();
    while let Some(msg) = bob
        .recv_matching(
            quiet,
            |m| matches!(m, ControlMsg::UserState { session: s, .. } if *s == session),
        )
        .await
    {
        if let ControlMsg::UserState { flags, .. } = msg {
            seen.push((flags.muted(), flags.deafened()));
        }
    }
    seen
}

/// A bouncing mute key produces far more changes than the 20 `SetState` per
/// second the server accepts. The client coalesces them: the server ends up
/// with the last state — here an unmute, which, dropped, would leave the
/// server discarding the user's audio — and never answers `RATE_LIMITED`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_of_mute_changes_ends_on_the_last_one() {
    let server = TestServer::start().await;
    let (engine, _dir) = server
        .engine("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    let alice = wait_connected(&engine).await.me.unwrap().session;
    let mut bob = server.raw_client("bob").await;

    // 41 commands at once, ending unmuted. The budget sends the first
    // changes — the last of them a mute — and the final state as soon as the
    // next token is there; a fixed 20-per-second window sent the first 20
    // (again ending on a mute) and dropped the rest.
    for i in 0..41 {
        engine.command(Command::SetMuted(i % 2 == 1));
    }
    let seen = user_states(&mut bob, alice, Duration::from_millis(1500)).await;
    assert!(seen.len() > 1, "{seen:?}");
    assert_eq!(seen.last(), Some(&(false, false)), "{seen:?}");
    let state = engine.snapshot();
    assert!(!state.muted);
    assert!(
        state.banner.is_none(),
        "the server never rate limited: {:?}",
        state.banner
    );

    engine.shutdown();
    server.handle.shutdown().await;
}

/// An admin enforces deafen only. Deafen implies mute: the client mutes, the
/// server learns it, and unmuting stays refused until the restriction is
/// lifted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_enforced_deafen_keeps_the_microphone_off() {
    let server = TestServer::start().await;
    let (engine, _dir) = server
        .engine("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    let alice = wait_connected(&engine).await.me.unwrap().session;
    let mut bob = server.raw_client("bob").await;

    server
        .handle
        .admin(icu_server::AdminRequest::Deafen {
            user: "alice".into(),
        })
        .await;
    wait_for(&engine, SOON, |s| {
        s.restricted_deafened && s.deafened && s.muted
    })
    .await;
    bob.recv_matching(SOON, |m| {
        matches!(m, ControlMsg::UserState { session, flags }
            if *session == alice && flags.muted() && flags.deafened())
    })
    .await
    .expect("the server holds the mute as well");

    engine.command(Command::SetMuted(false));
    engine.command(Command::ToggleMute);
    engine.command(Command::SetDeafened(false));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let state = engine.snapshot();
    assert!(state.muted && state.deafened, "still enforced");

    engine.shutdown();
    server.handle.shutdown().await;
}

/// Undeafening restores the mute from before deafening
/// (`Research/feature-freeze.md`); unmuting while deafened undeafens too; an
/// enforced mute survives undeafening.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undeafen_restores_the_previous_mute() {
    let server = TestServer::start().await;
    let (engine, _dir) = server
        .engine("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;

    // Unmuted before: unmuted after.
    engine.command(Command::SetDeafened(true));
    wait_for(&engine, SOON, |s| s.deafened && s.muted).await;
    engine.command(Command::SetDeafened(false));
    let s = wait_for(&engine, SOON, |s| !s.deafened).await;
    assert!(!s.muted, "unmuted as before deafening");

    // Muted before: muted after.
    engine.command(Command::SetMuted(true));
    engine.command(Command::SetDeafened(true));
    wait_for(&engine, SOON, |s| s.deafened).await;
    engine.command(Command::SetDeafened(false));
    let s = wait_for(&engine, SOON, |s| !s.deafened).await;
    assert!(s.muted, "muted as before deafening");

    // Unmuting while deafened undeafens: deafen implies mute.
    engine.command(Command::SetDeafened(true));
    wait_for(&engine, SOON, |s| s.deafened).await;
    engine.command(Command::SetMuted(false));
    let s = wait_for(&engine, SOON, |s| !s.muted).await;
    assert!(!s.deafened);

    // An enforced mute is not lifted by undeafening.
    server
        .handle
        .admin(icu_server::AdminRequest::Mute {
            user: "alice".into(),
        })
        .await;
    wait_for(&engine, SOON, |s| s.restricted_muted && s.muted).await;
    engine.command(Command::SetDeafened(true));
    wait_for(&engine, SOON, |s| s.deafened).await;
    engine.command(Command::SetDeafened(false));
    let s = wait_for(&engine, SOON, |s| !s.deafened).await;
    assert!(s.muted, "the admin's mute holds");

    engine.shutdown();
    server.handle.shutdown().await;
}
