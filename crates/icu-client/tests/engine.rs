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

/// A problem found before the engine started (an unreadable settings.toml)
/// is the first banner the window shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_startup_problem_is_the_first_banner() {
    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let problem = icu_client::problem::Problem::plain("Die Einstellungen wurden zurückgesetzt.");
    let mut cfg = icu_client::engine::EngineConfig::new(identity, test_settings(), paths);
    cfg.server_addr = Some(server.addr());
    cfg.banner = Some(problem.clone());
    let engine = icu_client::engine::Engine::start(cfg, Box::new(FakeBackend::silent().0))
        .expect("engine starts");
    assert_eq!(engine.snapshot().banner, Some(problem.clone()));
    // Still there once connected: nothing on the way clears it.
    let state = wait_connected(&engine).await;
    assert_eq!(state.banner, Some(problem));
    engine.shutdown();
    server.handle.shutdown().await;
}

/// Goertzel power of `freq` Hz in `x` (48 kHz).
fn tone_power(x: &[f32], freq: f32) -> f32 {
    let coeff = 2.0 * (2.0 * std::f32::consts::PI * freq / 48_000.0).cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &v in x {
        let s0 = v + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// An engine alone on the server, sending the test tone (its gate held
/// open), with only the transmit cue among the cues switched as asked.
async fn lone_sender(
    server: &TestServer,
    name: &str,
    cue_tx: bool,
) -> (
    icu_client::engine::Engine,
    icu_client::audio::fake::FakeProbe,
    tempfile::TempDir,
) {
    let (identity, paths, dir) = server.enroll(name).await;
    let mut settings = test_settings();
    settings.audio.cue_tx = cue_tx;
    settings.audio.cue_mute = false;
    settings.audio.cue_join = false;
    let (backend, probe) = FakeBackend::silent();
    let mut cfg = icu_client::engine::EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    cfg.test_tone = true;
    let engine = icu_client::engine::Engine::start(cfg, Box::new(backend)).expect("engine starts");
    (engine, probe, dir)
}

/// „Senden beginnt oder endet“ (`audio.cue_tx`): a high tone when sending
/// starts, a lower one when it ends — and nothing when the cue is off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_transmit_cue_marks_start_and_end_of_sending() {
    let server = TestServer::start().await;

    let (engine, probe, _dir) = lone_sender(&server, "alice", true).await;
    wait_connected(&engine).await;
    wait_for(&engine, SOON, |s| s.sending).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let start = probe.played();
    assert!(peak(&start) > 0.05, "a start cue, peak {}", peak(&start));
    assert!(
        tone_power(&start, 1000.0) > 10.0 * tone_power(&start, 800.0),
        "the start cue is the high one"
    );
    probe.clear();
    engine.command(Command::SetMuted(true));
    wait_for(&engine, SOON, |s| !s.sending).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stop = probe.played();
    assert!(peak(&stop) > 0.05, "a stop cue, peak {}", peak(&stop));
    assert!(
        tone_power(&stop, 800.0) > 10.0 * tone_power(&stop, 1000.0),
        "the stop cue is the low one"
    );
    engine.shutdown();

    let (quiet, probe, _dir) = lone_sender(&server, "bob", false).await;
    wait_connected(&quiet).await;
    wait_for(&quiet, SOON, |s| s.sending).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(peak(&probe.played()) < 1e-3, "no cue when it is off");
    quiet.shutdown();
    server.handle.shutdown().await;
}

/// The speaking indicator follows the voice closely: it goes out within a
/// few frames of the speaker falling silent, and stays out — the server's
/// 500-ms flag and a 300-ms hold used to keep it lit, or light it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_speaking_indicator_goes_out_with_the_voice() {
    let server = TestServer::start().await;
    let (alice, _dir_a) = server
        .engine("alice", Box::new(FakeBackend::silent().0), true)
        .await;
    let (bob, _dir_b) = server
        .engine("bob", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&alice).await;
    wait_for(&bob, SOON, |s| s.peers.iter().any(|p| p.speaking)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let t0 = std::time::Instant::now();
    alice.command(Command::SetMuted(true));
    wait_for(&bob, SOON, |s| s.peers.iter().all(|p| !p.speaking)).await;
    let off = t0.elapsed();
    assert!(off < Duration::from_millis(250), "went out after {off:?}");
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            bob.snapshot().peers.iter().all(|p| !p.speaking),
            "lit again after going out"
        );
    }

    alice.shutdown();
    bob.shutdown();
    server.handle.shutdown().await;
}

/// Which stream was opened on which device, in order.
type Opened =
    std::sync::Arc<std::sync::Mutex<Vec<(icu_client::audio::StreamKind, Option<String>)>>>;

/// A backend with a device list the test controls, recording which device
/// each stream was opened on. It never calls back: no audio flows.
struct Devices {
    list: std::sync::Arc<std::sync::Mutex<Vec<icu_client::audio::DeviceInfo>>>,
    opened: Opened,
    events: Option<std::sync::mpsc::Receiver<icu_client::audio::BackendEvent>>,
}

impl icu_client::audio::AudioBackend for Devices {
    fn name(&self) -> &'static str {
        "test"
    }
    fn devices(&self) -> Vec<icu_client::audio::DeviceInfo> {
        self.list.lock().unwrap().clone()
    }
    fn open_capture(
        &mut self,
        device: Option<&str>,
        _cb: icu_client::audio::CaptureCallback,
    ) -> Result<(), icu_client::audio::BackendError> {
        let kind = icu_client::audio::StreamKind::Capture;
        self.opened
            .lock()
            .unwrap()
            .push((kind, device.map(str::to_string)));
        Ok(())
    }
    fn open_reference(
        &mut self,
        sink: Option<&str>,
        _cb: icu_client::audio::CaptureCallback,
    ) -> Result<(), icu_client::audio::BackendError> {
        let kind = icu_client::audio::StreamKind::Reference;
        self.opened
            .lock()
            .unwrap()
            .push((kind, sink.map(str::to_string)));
        Ok(())
    }
    fn open_playback(
        &mut self,
        device: Option<&str>,
        _cb: icu_client::audio::PlaybackCallback,
    ) -> Result<(), icu_client::audio::BackendError> {
        let kind = icu_client::audio::StreamKind::Playback;
        self.opened
            .lock()
            .unwrap()
            .push((kind, device.map(str::to_string)));
        Ok(())
    }
    fn close(&mut self, _stream: icu_client::audio::StreamKind) {}
    fn take_events(
        &mut self,
    ) -> Option<std::sync::mpsc::Receiver<icu_client::audio::BackendEvent>> {
        self.events.take()
    }
}

/// A saved microphone that is not plugged in: the default is opened and the
/// choice kept; plugged in, it is used again; pulled, the default again. An
/// unrelated device coming and going reopens nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_device_falls_back_to_the_default_and_returns() {
    use icu_client::audio::{BackendEvent, DeviceInfo, DeviceKind, StreamKind};
    use std::sync::{Arc, Mutex};

    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let device = |id: &str, kind| DeviceInfo {
        id: id.into(),
        description: id.into(),
        kind,
    };
    let list = Arc::new(Mutex::new(vec![
        device("builtin-mic", DeviceKind::Input),
        device("speaker", DeviceKind::Output),
    ]));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let (events, events_rx) = std::sync::mpsc::channel();
    let backend = Devices {
        list: list.clone(),
        opened: opened.clone(),
        events: Some(events_rx),
    };
    let mut settings = test_settings();
    settings.audio.input_device = Some("usb-mic".into());
    let mut cfg = icu_client::engine::EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    let engine = icu_client::engine::Engine::start(cfg, Box::new(backend)).expect("engine starts");
    let last_capture = || {
        opened
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(kind, _)| *kind == StreamKind::Capture)
            .map(|(_, d)| d.clone())
    };
    let changed = |list: &Arc<Mutex<Vec<DeviceInfo>>>| {
        events
            .send(BackendEvent::DevicesChanged(list.lock().unwrap().clone()))
            .unwrap()
    };

    assert_eq!(
        last_capture(),
        Some(None),
        "the default, not a missing device"
    );
    let state = engine.snapshot();
    assert_eq!(state.active_input, None);
    assert_eq!(
        state.settings.audio.input_device.as_deref(),
        Some("usb-mic"),
        "the choice is kept"
    );

    list.lock()
        .unwrap()
        .push(device("usb-mic", DeviceKind::Input));
    changed(&list);
    wait_for(&engine, SOON, |s| {
        s.active_input.as_deref() == Some("usb-mic")
    })
    .await;
    assert_eq!(last_capture(), Some(Some("usb-mic".into())));

    list.lock().unwrap().retain(|d| d.id != "usb-mic");
    changed(&list);
    wait_for(&engine, SOON, |s| s.active_input.is_none()).await;
    assert_eq!(last_capture(), Some(None));

    let opens = opened.lock().unwrap().len();
    list.lock()
        .unwrap()
        .push(device("headset", DeviceKind::Output));
    changed(&list);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(opened.lock().unwrap().len(), opens, "nothing to follow");

    engine.shutdown();
    server.handle.shutdown().await;
}

/// „Mikrofontest“: while it is on, the microphone after the send chain plays
/// locally; off, nothing does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_mic_test_plays_the_own_voice_while_it_is_on() {
    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let (backend, probe) = FakeBackend::new(Box::new(|start, out| {
        for (i, s) in out.iter_mut().enumerate() {
            let t = (start + i as u64) as f32 / 48_000.0;
            *s = 0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
        }
    }));
    let mut cfg = icu_client::engine::EngineConfig::new(identity, test_settings(), paths);
    cfg.server_addr = Some(server.addr());
    let engine = icu_client::engine::Engine::start(cfg, Box::new(backend)).expect("engine starts");
    wait_connected(&engine).await;
    assert!(!engine.snapshot().mic_test, "off by default");

    let listen = || async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        probe.clear();
        tokio::time::sleep(Duration::from_millis(500)).await;
        probe.played()
    };
    assert!(peak(&listen().await) < 1e-3, "silent while off");

    engine.command(Command::SetMicTest(true));
    wait_for(&engine, SOON, |s| s.mic_test).await;
    let heard = listen().await;
    assert!(peak(&heard) > 0.02, "the own voice, peak {}", peak(&heard));
    assert!(
        tone_power(&heard, 440.0) > 100.0 * tone_power(&heard, 1000.0),
        "the microphone's tone"
    );
    let longest_gap = heard
        .split(|v| v.abs() > 1e-6)
        .map(<[f32]>::len)
        .max()
        .unwrap_or(0);
    assert!(longest_gap < 48, "a dropout of {longest_gap} samples");

    engine.command(Command::SetMicTest(false));
    wait_for(&engine, SOON, |s| !s.mic_test).await;
    assert!(peak(&listen().await) < 1e-3, "silent again");

    engine.shutdown();
    server.handle.shutdown().await;
}
