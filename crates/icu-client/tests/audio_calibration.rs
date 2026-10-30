#![cfg(unix)]
//! Ten minutes of sender-clock feedback with pauses, delayed reports and reversal.
use icu_client::dsp::feedback::{Measurement, PeerTuner};
use icu_proto::{AudioHeader, AudioLevel, AudioWindow, ControlMsg, CAP_AUDIO_FEEDBACK};
use icu_server::audio_feedback::Feedback;
use std::time::{Duration, Instant};
fn level(c: f32) -> AudioLevel {
    AudioLevel {
        level_cdb: (-2200.0 - c * 100.0) as i16,
        correction_cdb: (c * 100.0) as i16,
        peak_cdb: -1200,
        near_full_permille: 0,
        speech_ms: 1500,
    }
}
#[test]
fn changing_reporters_keeps_peer_hint_sequence_monotonic() {
    let start = Instant::now();
    let mut server = Feedback::default();
    for session in [1, 2, 3] {
        server.join(session, CAP_AUDIO_FEEDBACK);
    }
    let mut tuner = PeerTuner::default();
    tuner.set_epoch(1);
    let mut previous = None;
    for (i, (reporter, report_id)) in [(2, 100), (3, 0), (3, 1)].into_iter().enumerate() {
        let now = start + Duration::from_secs(i as u64 * 2);
        let window = AudioWindow {
            gain_epoch: 1,
            first_ts: i as u32 * 96_000,
            end_ts: (i as u32 + 1) * 96_000,
            report_id,
        };
        server.observe_audio(
            1,
            AudioHeader {
                gain_epoch: 1,
                ts: window.end_ts,
                ..Default::default()
            },
            now,
        );
        server.report(reporter, 1, level(4.0), window, now);
        let hint = server
            .tick(now, |_, _| true)
            .into_iter()
            .find_map(|(speaker, hint)| match hint {
                ControlMsg::AudioLevelHint {
                    window,
                    correction_cdb: Some(400),
                    reporters,
                    ..
                } if speaker == 1 => Some((window, reporters)),
                _ => None,
            })
            .expect("fresh agreeing report must produce a hint");
        if let Some(previous) = previous {
            assert!(
                hint.0.newer_than(previous),
                "receiver-local counters cannot order server hints"
            );
        }
        previous = Some(hint.0);
        tuner.hint_window(hint.0, Some(400), hint.1, now);
        tuner.tick(Some(level(4.0)), true, now);
    }
    assert_eq!(
        tuner.offset_db, 0.5,
        "three independent windows must corroborate"
    );
}

#[test]
fn ten_minutes_hold_success_across_pauses_reject_delays_and_reverse_cleanly() {
    let start = Instant::now();
    let mut server = Feedback::default();
    for s in [1, 2, 3] {
        server.join(s, CAP_AUDIO_FEEDBACK);
    }
    let mut tuner = PeerTuner::default();
    let mut epoch = 1;
    tuner.set_epoch(epoch);
    let mut previous = 0.0;
    let mut held = None;
    let mut delayed = None;
    let mut stable_positive = vec![];
    let mut stable_negative = vec![];
    for id in 0..300u32 {
        let seconds = id * 2;
        let now = start + Duration::from_secs(seconds as u64);
        if previous != tuner.offset_db {
            epoch += 1;
            tuner.set_epoch(epoch);
            previous = tuner.offset_db;
        }
        let before = tuner.offset_db;
        let quiet = (120..300).contains(&seconds) || (480..520).contains(&seconds);
        let baseline = if seconds < 340 { 6.0 } else { -6.0 };
        let residual = baseline - tuner.offset_db;
        let window = AudioWindow {
            gain_epoch: epoch,
            first_ts: id * 96_000,
            end_ts: (id + 1) * 96_000,
            report_id: id,
        };
        let observation = level(residual);
        if !quiet {
            server.observe_audio(
                1,
                AudioHeader {
                    gain_epoch: epoch,
                    ts: window.end_ts,
                    ..Default::default()
                },
                now,
            );
            if let Some((old_level, old_window)) = delayed.take() {
                server.report(2, 1, old_level, old_window, now);
            }
            for receiver in [2, 3] {
                if id % 31 == 7
                    || (receiver == 2 && id % 13 == 5)
                    || (receiver == 3 && id % 17 == 3)
                {
                    continue;
                }
                let report = if (306..310).contains(&seconds) {
                    level(if receiver == 2 { 6.0 } else { -6.0 })
                } else {
                    observation
                };
                server.report(receiver, 1, report, window, now);
            }
            if id > 0 && id % 11 == 4 {
                server.observe_audio(
                    1,
                    AudioHeader {
                        gain_epoch: epoch - 1,
                        ts: window.first_ts,
                        ..Default::default()
                    },
                    now,
                );
            }
            // A new receiver repeating an old window is not a new vote.
            if id % 31 != 7 && id % 13 != 5 && !(306..310).contains(&seconds) {
                server.report(2, 1, observation, window, now);
            }
            delayed = Some((observation, window));
        }
        for (speaker, msg) in server.tick(now, |_, _| true) {
            if let (
                1,
                ControlMsg::AudioLevelHint {
                    window,
                    correction_cdb,
                    reporters,
                    ..
                },
            ) = (speaker, msg)
            {
                tuner.hint_window(window, correction_cdb, reporters, now);
            }
        }
        tuner.tick((!quiet).then_some(observation), true, now);
        assert!((tuner.offset_db - before).abs() <= 0.5);
        assert!(tuner.offset_db.abs() <= 6.0);
        if seconds == 120 {
            held = Some(tuner.offset_db);
        }
        if (120..300).contains(&seconds) {
            assert_eq!(
                Some(tuner.offset_db),
                held,
                "silence must not undo calibration"
            );
        }
        if (90..120).contains(&seconds) {
            stable_positive.push(tuner.offset_db);
        }
        if seconds >= 550 {
            stable_negative.push(tuner.offset_db);
        }
        if seconds == 88 || seconds == 478 || seconds == 598 {
            assert!(residual.abs() <= 2.0, "at {seconds}: error {residual}");
        }
    }
    for values in [stable_positive, stable_negative] {
        let min = values.iter().copied().fold(f32::INFINITY, f32::min);
        let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max - min <= 1.0,
            "stable correction must not pump: {min}..{max}"
        );
    }
}
#[test]
fn gain_generation_discards_partial_windows_and_old_audio() {
    let mut m = Measurement::default();
    let samples = [0.04; 480];
    for i in 0..150 {
        m.observe_at(&samples, Some(true), 4, i * 480);
    }
    for i in 0..50 {
        m.observe_at(&samples, Some(true), 5, 72_000 + i * 480);
    }
    let (short, window) = m
        .take_window()
        .expect("new generation alone has a valid short observation");
    assert_eq!(short.speech_ms, 480, "old and new gains cannot add up");
    assert_eq!(window.gain_epoch, 5);
    assert!(window.first_ts >= 72_960);
    for i in 0..200 {
        m.observe_at(&samples, Some(true), 5, 96_000 + i * 480);
    }
    // Delayed old-generation audio must not replace the new estimator.
    m.observe_at(&samples, Some(true), 4, 190_000);
    let (_, window) = m.take_window().unwrap();
    assert_eq!(window.gain_epoch, 5);
    assert!(window.first_ts >= 96_000);
}
#[test]
fn old_generation_hints_and_fast_timer_ticks_cannot_move_gain() {
    let start = Instant::now();
    let mut p = PeerTuner::default();
    p.set_epoch(8);
    for i in 0..12 {
        let window = AudioWindow {
            gain_epoch: 7,
            first_ts: i * 96_000,
            end_ts: (i + 1) * 96_000,
            report_id: i,
        };
        let now = start + Duration::from_secs(i as u64 * 2);
        p.hint_window(window, Some(400), 2, now);
        p.tick(Some(level(4.0)), true, now);
    }
    assert_eq!(p.offset_db, 0.0);
    for i in 0..6 {
        let window = AudioWindow {
            gain_epoch: 8,
            first_ts: i * 96_000,
            end_ts: (i + 1) * 96_000,
            report_id: i,
        };
        let now = start + Duration::from_secs(30 + i as u64 * 2);
        p.hint_window(window, Some(400), 2, now);
        p.tick(Some(level(4.0)), true, now);
    }
    let before = p.offset_db;
    let now = start + Duration::from_secs(41);
    for _ in 0..100 {
        p.tick(None, false, now);
    }
    assert_eq!(
        p.offset_db, before,
        "decay uses two-second wall time, not callback count"
    );
    p.tick(None, false, now + Duration::from_secs(1));
    assert_eq!(p.offset_db, before - 0.5);
}

mod common;

/// Session feedback must follow the running DSP's generation after reconnect,
/// even when no processing setting or peer trim changes. The tone supplies real
/// audio headers; the raw listener supplies a known report to isolate delivery
/// and acceptance from speech classification and gain convergence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_accepts_feedback_for_the_running_audio_generation() {
    use icu_client::{audio::fake::FakeBackend, engine::ConnStatus, stats::DebugStats};

    async fn report_window(
        listener: &common::RawClient,
        speaker: icu_proto::Session,
    ) -> AudioWindow {
        tokio::time::timeout(common::SOON, async {
            let mut first: Option<AudioHeader> = None;
            loop {
                let datagram = listener.session.conn.read_datagram().await.unwrap();
                let (header, _) = AudioHeader::decode(&datagram).unwrap();
                if header.speaker != speaker {
                    continue;
                }
                let start = first.get_or_insert(header);
                let span = header.ts.wrapping_sub(start.ts);
                if start.gain_epoch != header.gain_epoch || span > 105_600 {
                    *start = header;
                    continue;
                }
                if span >= 72_000 {
                    return AudioWindow {
                        gain_epoch: header.gain_epoch,
                        first_ts: start.ts,
                        end_ts: header.ts,
                        report_id: 0,
                    };
                }
            }
        })
        .await
        .expect("enough real audio in one gain generation")
    }

    let server = common::TestServer::start().await;
    let addr = server.addr();
    let (engine, _dir) = server
        .engine("speaker", Box::new(FakeBackend::silent().0), true)
        .await;
    let speaker = common::wait_connected(&engine).await.me.unwrap().session;
    let listener = server.raw_client("first-listener").await;
    let before = report_window(&listener, speaker).await;
    assert_ne!(before.gain_epoch, 0, "the DSP must already be running");
    drop(listener);

    let server = server.restart(addr).await;
    let stats = engine.stats();
    let state = common::wait_for(&engine, Duration::from_secs(20), |s| {
        s.conn == ConnStatus::Connected && DebugStats::get(&stats.reconnects) > 0
    })
    .await;
    let speaker = state.me.unwrap().session;
    let mut listener = server.raw_client("second-listener").await;
    let window = report_window(&listener, speaker).await;
    assert_ne!(window.gain_epoch, 0);
    listener
        .send(&ControlMsg::AudioLevelReport {
            speaker,
            level: level(4.0),
            window,
        })
        .await;
    let accepted = common::wait_until(common::SOON, || {
        let health = engine.snapshot().audio_health;
        health.remote_correction_db == Some(4.0) && health.reporters == 1
    })
    .await;
    let health = engine.snapshot().audio_health;
    drop(listener);
    engine.shutdown();
    server.handle.shutdown().await;
    assert!(
        accepted,
        "fresh feedback for generation {} must survive reconnect: {health:?}",
        window.gain_epoch
    );
}

/// Real engine commands must restore processing without changing transmission privacy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_restores_good_processing_and_forgets_only_current_microphone() {
    use icu_client::{
        audio::{
            calibration::{device_channel_key, ProcessingSettings, Profile, Store},
            fake::FakeBackend,
            level::CaptureLevel,
            AudioBackend, BackendError, CaptureCallback, DeviceInfo, PlaybackCallback,
            ReferenceCallback, StreamKind,
        },
        config::ModeSetting,
        engine::{Command, Engine, EngineConfig},
    };
    struct Backend(FakeBackend);
    impl AudioBackend for Backend {
        fn name(&self) -> &'static str {
            "fake"
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
            d: Option<&str>,
            cb: CaptureCallback,
        ) -> Result<(), BackendError> {
            self.0.open_capture(d, cb)
        }
        fn open_reference(
            &mut self,
            d: Option<&str>,
            cb: ReferenceCallback,
        ) -> Result<(), BackendError> {
            self.0.open_reference(d, cb)
        }
        fn open_playback(
            &mut self,
            d: Option<&str>,
            cb: PlaybackCallback,
        ) -> Result<(), BackendError> {
            self.0.open_playback(d, cb)
        }
        fn close(&mut self, s: StreamKind) {
            self.0.close(s);
        }
        fn take_events(
            &mut self,
        ) -> Option<std::sync::mpsc::Receiver<icu_client::audio::BackendEvent>> {
            self.0.take_events()
        }
        fn capture_level(&mut self) -> Result<Option<CaptureLevel>, BackendError> {
            Ok(Some(CaptureLevel {
                device: "calibration-mic".into(),
                revision: 0,
                db: -20.0,
                min_db: -60.0,
                max_db: 0.0,
                step_db: 1.0,
                muted: false,
                channels: vec![0.1],
                kind: "fake",
            }))
        }
    }
    let server = common::TestServer::start().await;
    let (identity, paths, _dir) = server.enroll("calibrated").await;
    let mut settings = common::test_settings();
    settings.audio.muted = true;
    settings.audio.mode = ModeSetting::Ptt;
    let mut good = settings.audio.clone();
    good.gain_db = 3.0;
    let key = device_channel_key("fake", "calibration-mic", Some(0));
    let other = device_channel_key("fake", "other-mic", Some(0));
    let now = icu_client::net::conn::unix_now().max(0) as u64;
    let mut store = Store::default();
    for device in [&key, &other] {
        store
            .save_profile(
                &paths,
                Profile {
                    device: device.clone(),
                    saved_at: now,
                    settings: ProcessingSettings::from(&good),
                    capture_db: -20.0,
                    mic_gain_db: 2.0,
                    peer_trim_db: 1.0,
                },
                now,
            )
            .unwrap();
    }
    let mut cfg = EngineConfig::new(identity, settings, paths.clone());
    cfg.server_addr = Some(server.addr());
    let engine = Engine::start(cfg, Box::new(Backend(FakeBackend::silent().0))).unwrap();
    common::wait_connected(&engine).await;
    common::wait_for(&engine, common::SOON, |s| s.can_restore_audio).await;
    engine.command(Command::RestoreGoodAudio);
    let state = common::wait_for(&engine, common::SOON, |s| s.settings.audio.gain_db == 3.0).await;
    assert!(state.muted);
    assert_eq!(state.settings.audio.mode, ModeSetting::Ptt);
    engine.command(Command::ForgetGoodAudio);
    common::wait_for(&engine, common::SOON, |s| !s.can_restore_audio).await;
    let store = Store::load(&paths);
    assert!(store.profile(&key, now).is_none());
    assert!(store.profile(&other, now).is_some());
    engine.shutdown();
    server.handle.shutdown().await;
}

/// Exercise the actual measurement/consensus/attributed tuner path for short
/// conversational turns. Acceptance is measured from emitted sample energy,
/// independently of the controller's correction field and receive AGC.
#[test]
fn short_turns_converge_with_bounded_suitable_speech_and_one_acceptance_band() {
    let start = Instant::now();
    let mut converged_levels = Vec::new();
    for speech_ms in [200u32, 400, 800] {
        for input_db in [-28.0f32, -16.0] {
            let mut local = Measurement::default();
            let mut receive = Measurement::default();
            let mut server = Feedback::default();
            for id in [1, 2] {
                server.join(id, CAP_AUDIO_FEEDBACK);
            }
            let mut tuner = PeerTuner::default();
            let mut epoch = 1;
            tuner.set_epoch(epoch);
            let mut first = None;
            let mut suitable_ms = 0;
            let mut converged = None;
            for turn in 0..60u32 {
                let now = start + Duration::from_secs((turn as u64 + 1) * 2);
                let amplitude = 10.0f32.powf((input_db + tuner.offset_db) / 20.0);
                let mut speech_energy = 0.0f64;
                let mut speech_samples = 0;
                for block in 0..200u32 {
                    let speaking = block * 10 < speech_ms;
                    let samples = [if speaking { amplitude } else { 0.0 }; 480];
                    let ts = turn * 96_000 + block * 480;
                    local.observe_at(&samples, Some(speaking), epoch, ts);
                    receive.observe_at(&samples, Some(speaking), epoch, ts);
                    if speaking {
                        speech_energy += samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>();
                        speech_samples += samples.len();
                    }
                }
                suitable_ms += speech_ms;
                let rms_db = 10.0 * (speech_energy / speech_samples as f64).log10();
                if (rms_db + 22.0).abs() <= 1.501 {
                    converged = Some((turn + 1, suitable_ms, rms_db));
                    break;
                }
                server.observe_audio(
                    1,
                    AudioHeader {
                        gain_epoch: epoch,
                        ts: (turn + 1) * 96_000,
                        ..Default::default()
                    },
                    now,
                );
                let (level, window) = receive
                    .take_window()
                    .expect("every short answer independently reports");
                first.get_or_insert((turn + 1, suitable_ms));
                server.report(2, 1, level, window, now);
                for (speaker, hint) in server.tick(now, |_, _| true) {
                    if let (
                        1,
                        ControlMsg::AudioLevelHint {
                            window,
                            correction_cdb,
                            reporters,
                            agreeing,
                            speech_ms,
                        },
                    ) = (speaker, hint)
                    {
                        tuner.hint_observation(
                            window,
                            correction_cdb,
                            reporters,
                            agreeing,
                            speech_ms,
                            now,
                        );
                    }
                }
                let old = tuner.offset_db;
                let matched = tuner
                    .window()
                    .and_then(|w| local.measure_window(w).map(|l| (l, w)));
                tuner.tick_matched(matched, true, now);
                if old != tuner.offset_db {
                    epoch += 1;
                    tuner.set_epoch(epoch);
                }
            }
            let (turns, suitable, db) =
                converged.expect("bounded independent short speech must converge");
            assert_eq!(first, Some((1, speech_ms)));
            assert!(
                suitable <= 15_000,
                "{speech_ms} ms: {suitable} ms suitable speech"
            );
            eprintln!("short speech={speech_ms}ms input={input_db}dB: first=2s/{speech_ms}ms speech, converged={}s/{suitable}ms speech, active RMS={db:.2}dBFS", turns * 2);
            converged_levels.push(db);
        }
    }
    let min = converged_levels
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let max = converged_levels
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        max - min <= 3.001,
        "one intended acceptance band: {min}..{max}"
    );
}
