//! Engine behaviour against an in-process server: what reaches the server of
//! the local state, and how mute, deafen and admin restrictions interact
//! (`docs/reference/client.md` §5, §10).
//!
//! Unix only: the in-process server (`common`) is.
#![cfg(unix)]

mod common;

use std::time::Duration;

use common::*;

/// A live selector switch must not reopen and reset selection to channel zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_channel_selection_keeps_the_native_stream_open() {
    use icu_client::audio::BackendEvent;
    use std::sync::{Arc, Mutex};
    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let opened = Arc::new(Mutex::new(Vec::new()));
    let (events, rx) = std::sync::mpsc::channel();
    let backend = Devices {
        list: Arc::new(Mutex::new(vec![])),
        opened: opened.clone(),
        events: Some(rx),
    };
    let mut settings = test_settings();
    settings.audio.aec = true;
    settings.audio.muted = true;
    let mut cfg = icu_client::engine::EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    let engine = icu_client::engine::Engine::start(cfg, Box::new(backend)).unwrap();
    wait_connected(&engine).await;
    opened.lock().unwrap().clear();
    events
        .send(BackendEvent::CaptureChannelChanged { channel: 1 })
        .unwrap();
    wait_for(&engine, SOON, |s| s.audio_health.capture_channel == Some(1)).await;
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert!(
        opened.lock().unwrap().is_empty(),
        "channel selection reopened audio"
    );
    assert!(engine.snapshot().muted);
    engine.shutdown();
    server.handle.shutdown().await;
}
use icu_client::{audio::fake::FakeBackend, engine::Command, stats::DebugStats};
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
/// (`docs/reference/features.md`); unmuting while deafened undeafens too; an
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
        ReferenceCallback, StreamKind,
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
        fn set_capture_channel(&mut self, channel: Option<u16>) -> Result<(), BackendError> {
            self.0.set_capture_channel(channel)
        }
        fn capture_channel_index(&self) -> Option<u16> {
            self.0.capture_channel_index()
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
            cb: ReferenceCallback,
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

/// An engine sending the test tone (its gate held open), with only the
/// transmit cue among the cues switched as asked. It sends only while
/// someone else is in the room.
async fn tone_sender(
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

/// Setup is a separate transmission guard: global mute/deafen actions may
/// change the user's preference, but cannot make setup audio leave the client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_blocks_network_audio_and_preserves_user_mute_and_deafen() {
    async fn hear_audio(bob: &RawClient) {
        tokio::time::timeout(SOON, async {
            loop {
                let datagram = bob.session.conn.read_datagram().await.expect("connected");
                let (header, _) = icu_proto::AudioHeader::decode(&datagram).expect("audio packet");
                if !header.end() {
                    return;
                }
            }
        })
        .await
        .expect("audio resumes when the guard and user mute allow it");
    }

    async fn settle_audio(bob: &RawClient) {
        // Drain packets already in flight and the silent END. A continuing
        // stream must fail, rather than allowing an unbounded drain to hang.
        tokio::time::timeout(SOON, async {
            loop {
                match tokio::time::timeout(
                    Duration::from_millis(60),
                    bob.session.conn.read_datagram(),
                )
                .await
                {
                    Ok(packet) => {
                        packet.expect("connected");
                    }
                    Err(_) => return,
                }
            }
        })
        .await
        .expect("audio stops after setup opens");
    }

    async fn hear_nothing(bob: &RawClient) {
        assert!(
            tokio::time::timeout(Duration::from_millis(200), bob.session.conn.read_datagram(),)
                .await
                .is_err(),
            "no audio datagram may escape while transmission is inhibited"
        );
    }

    let server = TestServer::start().await;
    let bob = server.raw_client("bob").await;
    let (engine, _probe, _dir) = tone_sender(&server, "alice", false).await;
    wait_connected(&engine).await;
    hear_audio(&bob).await;

    engine.command(Command::SetSetupActive(true));
    wait_for(&engine, SOON, |s| s.setup_active && !s.sending).await;
    settle_audio(&bob).await;
    hear_nothing(&bob).await;

    engine.command(Command::SetMuted(true));
    wait_for(&engine, SOON, |s| s.muted).await;
    hear_nothing(&bob).await;
    engine.command(Command::SetMuted(false));
    wait_for(&engine, SOON, |s| !s.muted).await;
    hear_nothing(&bob).await;
    engine.command(Command::SetDeafened(true));
    wait_for(&engine, SOON, |s| s.deafened && s.muted).await;
    hear_nothing(&bob).await;
    engine.command(Command::SetDeafened(false));
    wait_for(&engine, SOON, |s| !s.deafened && !s.muted).await;
    hear_nothing(&bob).await;

    // Finishing setup preserves an explicit mute. Only the user's later
    // unmute permits audio to resume.
    engine.command(Command::SetMuted(true));
    engine.command(Command::SetSetupActive(false));
    let state = wait_for(&engine, SOON, |s| !s.setup_active && s.muted).await;
    assert!(!state.deafened);
    hear_nothing(&bob).await;
    engine.command(Command::SetMuted(false));
    wait_for(&engine, SOON, |s| !s.muted).await;
    hear_audio(&bob).await;

    // The same applies to deafen; closing setup must not undeafen or lose
    // the remembered unmuted state from before deafen.
    engine.command(Command::SetSetupActive(true));
    wait_for(&engine, SOON, |s| s.setup_active && !s.sending).await;
    settle_audio(&bob).await;
    engine.command(Command::SetDeafened(true));
    engine.command(Command::SetSetupActive(false));
    wait_for(&engine, SOON, |s| !s.setup_active && s.deafened && s.muted).await;
    hear_nothing(&bob).await;
    engine.command(Command::SetDeafened(false));
    wait_for(&engine, SOON, |s| !s.deafened && !s.muted).await;
    hear_audio(&bob).await;

    engine.shutdown();
    server.handle.shutdown().await;
}

/// The processed meter can be quiet after manual gain or AGC even though
/// the input is clipped. Setup must report that damage while it blocks
/// transmission, including when RNNoise is disabled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_reports_raw_input_clipping_even_when_the_processed_level_is_low() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let clipping = Arc::new(AtomicBool::new(false));
    let signal = clipping.clone();
    let (backend, _) = FakeBackend::new(Box::new(move |_, out| {
        out.fill(if signal.load(Ordering::Relaxed) {
            1.0
        } else {
            0.2
        });
    }));
    let mut settings = test_settings();
    settings.audio.muted = true;
    settings.audio.gain_db = -20.0;
    settings.audio.denoise = false;
    settings.audio.aec = false;
    let mut cfg = icu_client::engine::EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    let engine = icu_client::engine::Engine::start(cfg, Box::new(backend)).expect("engine starts");
    engine.command(Command::SetSetupActive(true));
    wait_connected(&engine).await;
    wait_for(&engine, SOON, |s| s.setup_active && s.level.peak_db > -90.0).await;
    assert!(!engine.snapshot().input_clipping);

    clipping.store(true, Ordering::Relaxed);
    let state = wait_for(&engine, SOON, |s| s.input_clipping).await;
    assert!(state.setup_active && state.muted && !state.sending);
    assert!(
        state.level.peak_db < -10.0,
        "processing conceals the raw full-scale peak"
    );
    assert!(!state.settings.audio.denoise);

    // A short overload remains visible through a UI refresh, then clears
    // when the input becomes safe; mute/setup must not freeze the warning.
    clipping.store(false, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(engine.snapshot().input_clipping);
    let state = wait_for(&engine, SOON, |s| !s.input_clipping).await;
    assert!(state.setup_active && state.muted && !state.sending);

    engine.shutdown();
    server.handle.shutdown().await;
}

/// „Senden beginnt oder endet“ (`audio.cue_tx`): a high tone when sending
/// starts, a lower one when it ends — and nothing when the cue is off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_transmit_cue_marks_start_and_end_of_sending() {
    let server = TestServer::start().await;
    // Someone to talk to, for alice and then bob: alone, nothing is sent.
    let _carol = server.raw_client("carol").await;

    let (engine, probe, _dir) = tone_sender(&server, "alice", true).await;
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

    let (quiet, probe, _dir) = tone_sender(&server, "bob", false).await;
    wait_connected(&quiet).await;
    wait_for(&quiet, SOON, |s| s.sending).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(peak(&probe.played()) < 1e-3, "no cue when it is off");
    quiet.shutdown();
    server.handle.shutdown().await;
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Muting ends the talk spurt with an `END` packet whose frame was
/// collected up to the mute and partly after it: that packet carries
/// silence, so nothing captured after the key press leaves the machine.
/// Usually the server learns of the mute first and drops the packet; here
/// a burst of camera toggles has used up the `SetState` budget, so the mute
/// reaches the server a few frames late and the `END` packet reaches bob,
/// who decodes alice's stream as it arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_end_packet_of_a_mute_carries_no_audio() {
    let server = TestServer::start().await;
    let bob = server.raw_client("bob").await;
    let (engine, _probe, _dir) = tone_sender(&server, "alice", false).await;
    wait_connected(&engine).await;
    wait_for(&engine, SOON, |s| s.sending).await;

    let mut decoder = icu_client::dsp::opus::Decoder::new().unwrap();
    let mut pcm = vec![0.0f32; 5760];
    let mut talking = Vec::new();
    let mut end = None;
    let deadline = tokio::time::Instant::now() + SOON;
    let mut muted = false;
    while end.is_none() && tokio::time::Instant::now() < deadline {
        let Ok(Ok(datagram)) =
            tokio::time::timeout(Duration::from_secs(1), bob.session.conn.read_datagram()).await
        else {
            continue;
        };
        let (header, payload) = icu_proto::AudioHeader::decode(&datagram).unwrap();
        let n = decoder.decode(payload, &mut pcm).unwrap();
        if header.end() {
            end = Some(pcm[..n].to_vec());
        } else {
            talking.push(rms(&pcm[..n]));
        }
        if !muted && talking.len() >= 20 {
            for _ in 0..30 {
                engine.command(Command::ToggleCamera);
            }
            engine.command(Command::SetMuted(true));
            muted = true;
        }
    }
    let end = end.expect("an END packet after the mute");
    // The harmonic test voice contains 200-ms syllable gaps. A fixed
    // packet index can land in one after a capture/startup boundary.
    let voice = talking.iter().copied().fold(0.0f32, f32::max);
    assert!(voice > 0.02, "the tone went out, rms {voice}");
    // Opus delays decoded samples: the beginning of this packet still
    // represents input from BEFORE mute. Check silence after the queried
    // codec delay, without confusing the preceding syllable with a leak.
    let delay = icu_client::dsp::opus::Encoder::new()
        .unwrap()
        .lookahead_samples()
        .unwrap();
    assert!(delay < end.len());
    let end = rms(&end[delay..]);
    assert!(
        end < 0.01,
        "the END packet carries audio: rms {end} (voice {voice})"
    );
    engine.shutdown();
    server.handle.shutdown().await;
}

/// Alone in the room nothing goes to the server: the test tone holds the
/// gate open, yet not one datagram leaves until someone joins, and none
/// once they have left again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lone_engine_sends_nothing_until_someone_joins() {
    let server = TestServer::start().await;
    let (engine, _probe, _dir) = tone_sender(&server, "alice", false).await;
    wait_connected(&engine).await;
    let stats = engine.stats();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !engine.snapshot().sending,
        "alone, the engine is not sending"
    );
    assert_eq!(DebugStats::get(&stats.datagrams_sent), 0, "no audio alone");

    let bob = server.raw_client("bob").await;
    wait_for(&engine, SOON, |s| s.sending).await;
    assert!(
        wait_until(SOON, || DebugStats::get(&stats.datagrams_sent) > 0).await,
        "audio goes out once bob is in"
    );

    bob.session.conn.close(0u32.into(), b"bye");
    // `sending` is pushed by the DSP thread after the frame it decided on,
    // so once it reads false, no later frame goes out while alone.
    wait_for(&engine, SOON, |s| s.peers.is_empty() && !s.sending).await;
    let sent = DebugStats::get(&stats.datagrams_sent);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        DebugStats::get(&stats.datagrams_sent),
        sent,
        "nothing once alone again"
    );
    engine.shutdown();
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
        _cb: icu_client::audio::ReferenceCallback,
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

/// A desktop logging out ends ICU with SIGTERM: the process says goodbye,
/// so the others see it leave at once instead of after the server's idle
/// timeout (60 s).
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_ends_the_session_with_a_goodbye() {
    let server = TestServer::start().await;
    let (_identity, _paths, dir) = server.enroll("alice").await;
    let mut watcher = server.raw_client("bob").await;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_icu"))
        // Without a terminal ICU also logs to a file: keep it in the test's
        // directory, not in the user's `~/.local/state/icu`.
        .env("XDG_STATE_HOME", dir.path())
        .arg("--config-dir")
        .arg(dir.path())
        .args([
            "--headless",
            "--no-hotkeys",
            "--no-uri-handler",
            "--fake-audio",
            "--server-addr",
        ])
        .arg(server.addr().to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("icu starts");
    let joined = watcher
        .recv_matching(SOON, |m| matches!(m, ControlMsg::UserJoined { .. }))
        .await;
    assert!(joined.is_some(), "the client joined");

    // SAFETY: a plain kill(2) on our own child.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let left = watcher
        .recv_matching(Duration::from_secs(3), |m| {
            matches!(m, ControlMsg::UserLeft { .. })
        })
        .await;
    let exited = wait_until(Duration::from_secs(6), || {
        matches!(child.try_wait(), Ok(Some(_)))
    })
    .await;
    if !exited {
        let _ = child.kill();
    }
    assert!(left.is_some(), "the others see the client leave at once");
    assert!(exited, "the client ends after SIGTERM");
}

/// Windows default-endpoint notifications must reset all three streams,
/// even though the configured device stays `None`. A burst gets one reopen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_default_endpoint_change_reopens_the_audio_group_once() {
    use icu_client::audio::{BackendEvent, StreamKind};
    use std::sync::{Arc, Mutex};
    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let opened = Arc::new(Mutex::new(Vec::new()));
    let (events, rx) = std::sync::mpsc::channel();
    let backend = Devices {
        list: Arc::new(Mutex::new(vec![])),
        opened: opened.clone(),
        events: Some(rx),
    };
    let mut settings = test_settings();
    settings.audio.aec = true;
    let mut cfg = icu_client::engine::EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    let engine = icu_client::engine::Engine::start(cfg, Box::new(backend)).unwrap();
    wait_connected(&engine).await;
    opened.lock().unwrap().clear();
    for _ in 0..3 {
        events.send(BackendEvent::DefaultDeviceChanged).unwrap();
    }
    tokio::time::timeout(SOON, async {
        while opened.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        *opened.lock().unwrap(),
        vec![
            (StreamKind::Capture, None),
            (StreamKind::Reference, None),
            (StreamKind::Playback, None)
        ]
    );
    events
        .send(BackendEvent::StreamError {
            stream: StreamKind::Playback,
            message: "audio service stopped".into(),
        })
        .unwrap();
    wait_for(&engine, SOON, |s| s.banner.is_some()).await;
    wait_for(&engine, SOON, |s| s.banner.is_none()).await;
    drop(engine);
}

/// Native capture discontinuities use the coordinated reopen path without
/// becoming a persistent device-error banner or changing the user's mute.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_discontinuities_restart_the_audio_group_and_preserve_mute() {
    use icu_client::audio::{BackendEvent, StreamKind};
    use std::sync::{Arc, Mutex};
    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let opened = Arc::new(Mutex::new(Vec::new()));
    let (events, rx) = std::sync::mpsc::channel();
    let backend = Devices {
        list: Arc::new(Mutex::new(vec![])),
        opened: opened.clone(),
        events: Some(rx),
    };
    let mut settings = test_settings();
    settings.audio.aec = true;
    settings.audio.muted = true;
    let mut cfg = icu_client::engine::EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    let engine = icu_client::engine::Engine::start(cfg, Box::new(backend)).unwrap();
    wait_connected(&engine).await;
    for stream in [StreamKind::Capture, StreamKind::Reference] {
        opened.lock().unwrap().clear();
        events
            .send(BackendEvent::StreamDiscontinuity { stream })
            .unwrap();
        tokio::time::timeout(SOON, async {
            while opened.lock().unwrap().len() < 3 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            *opened.lock().unwrap(),
            vec![
                (StreamKind::Capture, None),
                (StreamKind::Reference, None),
                (StreamKind::Playback, None),
            ],
        );
        let state = engine.snapshot();
        assert!(state.muted);
        assert!(state.banner.is_none());
    }
    engine.shutdown();
    server.handle.shutdown().await;
}

/// A backend whose microphone does not open while `mic` is off — a
/// desktop without one — recording every open like [`Devices`]. No audio
/// flows.
struct NoMic {
    mic: std::sync::Arc<std::sync::atomic::AtomicBool>,
    opened: Opened,
    events: Option<std::sync::mpsc::Receiver<icu_client::audio::BackendEvent>>,
}

impl icu_client::audio::AudioBackend for NoMic {
    fn name(&self) -> &'static str {
        "test"
    }
    fn devices(&self) -> Vec<icu_client::audio::DeviceInfo> {
        Vec::new()
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
        if self.mic.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(())
        } else {
            Err("kein Standardgerät".into())
        }
    }
    fn open_reference(
        &mut self,
        sink: Option<&str>,
        _cb: icu_client::audio::ReferenceCallback,
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

/// A microphone that does not open is tried again on its own, and the
/// streams that run are left alone: before, every attempt reopened all
/// three — a gap in the playback and an AEC restart every 30 s on a desktop
/// without a microphone. Once one is there, it opens and the banner goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_microphone_that_does_not_open_is_retried_alone() {
    use icu_client::audio::StreamKind;
    use std::sync::{atomic::AtomicBool, Arc, Mutex};

    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let mic = Arc::new(AtomicBool::new(false));
    let opened: Opened = Arc::new(Mutex::new(Vec::new()));
    let (_events, events_rx) = std::sync::mpsc::channel();
    let backend = NoMic {
        mic: mic.clone(),
        opened: opened.clone(),
        events: Some(events_rx),
    };
    let mut settings = test_settings();
    settings.audio.aec = true;
    let mut cfg = icu_client::engine::EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    let engine = icu_client::engine::Engine::start(cfg, Box::new(backend)).expect("engine starts");
    let count = |kind| {
        opened
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == kind)
            .count()
    };
    let banner = engine.snapshot().banner.expect("a banner");
    assert!(banner.text.contains("Mikrofon"), "{}", banner.text);

    assert!(
        wait_until(SOON, || count(StreamKind::Capture) >= 2).await,
        "the microphone is tried again"
    );
    assert_eq!(count(StreamKind::Playback), 1, "the playback runs on");
    assert_eq!(count(StreamKind::Reference), 1, "so does the reference");

    mic.store(true, std::sync::atomic::Ordering::SeqCst);
    wait_for(&engine, SOON, |s| s.banner.is_none()).await;
    let tries = count(StreamKind::Capture);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(count(StreamKind::Capture), tries, "open now: no more tries");
    assert_eq!(count(StreamKind::Playback), 1);
    assert_eq!(count(StreamKind::Reference), 1);

    engine.shutdown();
    server.handle.shutdown().await;
}

/// The AEC reference failing is not the microphone failing: no banner
/// saying so (the settings show the missing reference), and the streams
/// are reopened as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reference_error_blames_no_microphone() {
    use icu_client::audio::{BackendEvent, StreamKind};
    use std::sync::{Arc, Mutex};

    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let opened = Arc::new(Mutex::new(Vec::new()));
    let (events, rx) = std::sync::mpsc::channel();
    let backend = Devices {
        list: Arc::new(Mutex::new(vec![])),
        opened: opened.clone(),
        events: Some(rx),
    };
    let mut settings = test_settings();
    settings.audio.aec = true;
    let mut cfg = icu_client::engine::EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    let engine = icu_client::engine::Engine::start(cfg, Box::new(backend)).unwrap();
    wait_connected(&engine).await;
    opened.lock().unwrap().clear();
    events
        .send(BackendEvent::StreamError {
            stream: StreamKind::Reference,
            message: "loopback gone".into(),
        })
        .unwrap();
    wait_for(&engine, SOON, |s| !s.aec_reference).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(engine.snapshot().banner, None);
    assert!(
        wait_until(SOON, || opened.lock().unwrap().len() >= 3).await,
        "reopened after the backoff"
    );
    drop(engine);
}

/// A graph quantum of 8192 samples (forced, pro audio) arrives in one piece
/// every 170 ms. The DSP thread takes all of it; held to the fixed 100-ms
/// backlog cap it kept only the newest two blocks of every cycle and threw
/// the rest of the microphone away.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_graph_quantum_is_not_trimmed() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::with_block(
        Box::new(|start, out| {
            for (i, s) in out.iter_mut().enumerate() {
                let t = (start + i as u64) as f32 / 48_000.0;
                *s = 0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            }
        }),
        false,
        8192,
    );
    let (engine, _dir) = server.engine("alice", Box::new(backend), false).await;
    wait_connected(&engine).await;
    let stats = engine.stats();
    let before = DebugStats::get(&stats.dsp_blocks);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let blocks = DebugStats::get(&stats.dsp_blocks) - before;
    assert_eq!(DebugStats::get(&stats.capture_backlog_dropped), 0);
    assert!(blocks > 120, "{blocks} blocks in 1.5 s");
    engine.shutdown();
    server.handle.shutdown().await;
}

/// A mute survives a restart, as in Mumble: the next engine starts muted —
/// in the window from its first state, at the server from `Hello` on. A
/// deafen does not: a restart undeafens, back to the mute from before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mute_survives_a_restart() {
    use icu_client::config::Settings;
    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let start = |settings: Settings| {
        let mut cfg =
            icu_client::engine::EngineConfig::new(identity.clone(), settings, paths.clone());
        cfg.server_addr = Some(server.addr());
        icu_client::engine::Engine::start(cfg, Box::new(FakeBackend::silent().0)).unwrap()
    };

    let engine = start(test_settings());
    assert!(!engine.snapshot().muted);
    wait_connected(&engine).await;
    engine.command(Command::SetMuted(true));
    wait_for(&engine, SOON, |s| s.muted).await;
    engine.shutdown();
    let saved = Settings::load(&paths).unwrap();
    assert!(saved.audio.muted, "the mute is kept");

    let engine = start(saved);
    assert!(engine.snapshot().muted, "muted from the first state");
    let me = wait_connected(&engine).await.me.unwrap();
    assert!(me.flags.muted(), "the server knew it from Hello");
    engine.command(Command::SetMuted(false));
    wait_for(&engine, SOON, |s| !s.muted).await;
    engine.command(Command::SetDeafened(true));
    wait_for(&engine, SOON, |s| s.deafened && s.muted).await;
    engine.shutdown();
    assert!(
        !Settings::load(&paths).unwrap().audio.muted,
        "deafened from unmuted: the restart undeafens to unmuted"
    );
    server.handle.shutdown().await;
}

/// A settings copy taken before a mute (the settings window, a passport)
/// does not undo it when it is applied, neither in the engine nor on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_settings_copy_keeps_the_mute() {
    let server = TestServer::start().await;
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;
    let mut stale = engine.snapshot().settings;
    assert!(!stale.audio.muted);
    engine.command(Command::SetMuted(true));
    wait_for(&engine, SOON, |s| s.muted && s.settings.audio.muted).await;
    stale.audio.output_volume = 42.0;
    engine.command(Command::UpdateSettings(Box::new(stale)));
    let s = wait_for(&engine, SOON, |s| s.settings.audio.output_volume == 42.0).await;
    assert!(s.muted && s.settings.audio.muted);
    engine.shutdown();
    let saved = icu_client::config::Settings::load(&paths).unwrap();
    assert_eq!(saved.audio.output_volume, 42.0);
    assert!(saved.audio.muted, "the stale copy did not unmute the file");
    server.handle.shutdown().await;
}

/// A normal member can inspect the actual public leaf of an online peer,
/// including its locally checked CA signature. Cache lifetime is the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_certificate_matches_the_authenticated_session_and_expires_with_it() {
    use icu_client::{config::Identity, conninfo::CertInfo};
    let server = TestServer::start().await;
    let (bob, paths, _bob_dir) = server
        .engine_with_paths("bob", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&bob).await;
    let (alice, _alice_dir) = server
        .engine("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    let bob_session = wait_connected(&bob).await.me.unwrap().session;
    wait_for(&alice, SOON, |s| {
        s.peers.iter().any(|p| p.info.session == bob_session)
    })
    .await;
    alice.command(Command::RequestPeerCertificate(bob_session));
    let s = wait_for(&alice, SOON, |s| {
        s.peers
            .iter()
            .any(|p| p.info.session == bob_session && matches!(p.certificate, Some(Ok(_))))
    })
    .await;
    let received = s
        .peers
        .iter()
        .find(|p| p.info.session == bob_session)
        .unwrap()
        .certificate
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap();
    let expected = CertInfo::from_der(&Identity::load(&paths).unwrap().unwrap().leaf_der);
    assert_eq!(received.fingerprint, expected.fingerprint);
    assert_eq!(received.der, expected.der);
    assert_eq!(received.signature_valid, Some(true));
    assert!(!s.is_admin, "lookup is available to ordinary room members");
    bob.shutdown();
    wait_for(&alice, SOON, |s| s.peers.is_empty()).await;
    // A subsequent lookup cannot recreate departed peer state.
    alice.command(Command::RequestPeerCertificate(bob_session));
    assert!(alice.snapshot().peers.is_empty());
    let (carol, _carol_dir) = server
        .engine("carol", Box::new(FakeBackend::silent().0), false)
        .await;
    let new_session = wait_connected(&carol).await.me.unwrap().session;
    let s = wait_for(&alice, SOON, |s| {
        s.peers.iter().any(|p| p.info.session == new_session)
    })
    .await;
    assert!(s
        .peers
        .iter()
        .find(|p| p.info.session == new_session)
        .unwrap()
        .certificate
        .is_none());
    let mut inspector = server.raw_client("inspector").await;
    inspector
        .send(&ControlMsg::PeerCertificateRequest {
            session: bob_session,
        })
        .await;
    let answer = inspector
        .recv_matching(SOON, |m| {
            matches!(m, ControlMsg::PeerCertificateResponse { .. })
        })
        .await;
    assert_eq!(
        answer,
        Some(ControlMsg::PeerCertificateResponse {
            session: bob_session,
            certificate_der: None
        })
    );
    alice.shutdown();
    carol.shutdown();
    server.handle.shutdown().await;
}

/// Buttons, hotkeys and the settings panel share the camera switch; both
/// media switches and their detailed settings survive quitting and restarting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn camera_and_overlay_survive_a_restart() {
    use icu_client::config::{Corner, Expand, Settings};
    let server = TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("alice").await;
    let start = |settings: Settings| {
        let mut cfg =
            icu_client::engine::EngineConfig::new(identity.clone(), settings, paths.clone());
        cfg.server_addr = Some(server.addr());
        icu_client::engine::Engine::start(cfg, Box::new(FakeBackend::silent().0)).unwrap()
    };

    let engine = start(test_settings());
    wait_connected(&engine).await;
    let mut settings = engine.snapshot().settings;
    settings.camera.device = Some("2".into());
    settings.camera.enabled = true;
    settings.overlay.corner = Corner::BottomLeft;
    settings.overlay.offset = [72, 96];
    settings.overlay.size = 300;
    settings.overlay.expand = Expand::Horizontal;
    settings.overlay.voice_plates = false;
    settings.overlay.speaking_frame = false;
    settings.overlay.dim_non_speaking = false;
    settings.overlay.frame_color = [12, 34, 56];
    engine.command(Command::UpdateSettings(Box::new(settings)));
    wait_for(&engine, SOON, |s| s.camera_on).await;
    engine.command(Command::SetOverlay(true));
    let expected = wait_for(&engine, SOON, |s| s.settings.overlay.enabled)
        .await
        .settings;
    engine.shutdown();

    let saved = Settings::load(&paths).unwrap();
    assert_eq!(saved.camera, expected.camera);
    assert_eq!(saved.overlay, expected.overlay);
    let engine = start(saved);
    assert!(engine.snapshot().camera_on, "on from the first state");
    assert!(wait_connected(&engine).await.me.unwrap().flags.camera_on());
    engine.command(Command::ToggleCamera);
    engine.command(Command::SetOverlay(false));
    wait_for(&engine, SOON, |s| {
        !s.camera_on && !s.settings.overlay.enabled
    })
    .await;
    engine.shutdown();

    let saved = Settings::load(&paths).unwrap();
    assert!(!saved.camera.enabled);
    assert!(!saved.overlay.enabled);
    assert_eq!(saved.camera.device, expected.camera.device);
    let engine = start(saved);
    assert!(!engine.snapshot().camera_on);
    assert!(!wait_connected(&engine).await.me.unwrap().flags.camera_on());
    engine.command(Command::SetCamera(true));
    wait_for(&engine, SOON, |s| s.camera_on && s.settings.camera.enabled).await;
    engine.shutdown();
    assert!(Settings::load(&paths).unwrap().camera.enabled);
    server.handle.shutdown().await;
}
