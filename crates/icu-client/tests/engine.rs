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

/// A settings change right before quitting reaches the disk, although the
/// file is only written once changes have been quiet for a while.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_right_before_quitting_is_saved() {
    let server = TestServer::start().await;
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;
    let mut settings = engine.snapshot().settings;
    settings.audio.output_volume = 42.0;
    engine.command(Command::UpdateSettings(Box::new(settings)));
    engine.shutdown();
    let stored = icu_client::config::Settings::load(&paths).unwrap();
    assert_eq!(stored.audio.output_volume, 42.0);
    server.handle.shutdown().await;
}

/// The engine lets go of its audio backend when it shuts down. The thread
/// that forwards backend events used to hold it, and with it (PipeWire) the
/// thread owning the event sender: every logout/login leaked a PipeWire
/// connection and two threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutting_down_releases_the_audio_backend() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use icu_client::audio::{
        AudioBackend, BackendError, BackendEvent, CaptureCallback, DeviceInfo, PlaybackCallback,
        StreamKind,
    };

    /// The fake backend, telling when it is dropped.
    struct Watched(FakeBackend, Arc<AtomicBool>);

    impl Drop for Watched {
        fn drop(&mut self) {
            self.1.store(true, Ordering::SeqCst);
        }
    }

    impl AudioBackend for Watched {
        fn name(&self) -> &'static str {
            self.0.name()
        }
        fn devices(&self) -> Vec<DeviceInfo> {
            self.0.devices()
        }
        fn open_capture(
            &mut self,
            device: Option<&str>,
            cb: CaptureCallback,
        ) -> Result<(), BackendError> {
            self.0.open_capture(device, cb)
        }
        fn open_reference(
            &mut self,
            sink: Option<&str>,
            cb: CaptureCallback,
        ) -> Result<(), BackendError> {
            self.0.open_reference(sink, cb)
        }
        fn open_playback(
            &mut self,
            device: Option<&str>,
            cb: PlaybackCallback,
        ) -> Result<(), BackendError> {
            self.0.open_playback(device, cb)
        }
        fn close(&mut self, stream: StreamKind) {
            self.0.close(stream)
        }
        fn take_events(&mut self) -> Option<std::sync::mpsc::Receiver<BackendEvent>> {
            self.0.take_events()
        }
    }

    let server = TestServer::start().await;
    let dropped = Arc::new(AtomicBool::new(false));
    let backend = Watched(FakeBackend::silent().0, dropped.clone());
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    wait_connected(&engine).await;
    engine.shutdown();
    assert!(
        wait_until(SOON, || dropped.load(Ordering::SeqCst)).await,
        "the backend outlived its engine"
    );
    server.handle.shutdown().await;
}

/// Logging out deletes `identity/` right after the engine is shut down. A
/// renewal answered in that moment must not bring it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logging_out_during_a_rename_leaves_no_identity_behind() {
    use icu_client::config::Identity;

    let server = TestServer::start().await;
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;
    engine.command(Command::RequestRename("alicia".into()));
    engine.shutdown();
    Identity::remove(&paths).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(Identity::load(&paths).unwrap().is_none());
    assert!(!paths.identity_dir().exists());
    server.handle.shutdown().await;
}
