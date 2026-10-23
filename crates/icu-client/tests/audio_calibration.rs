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
    assert!(
        m.take_window().is_none(),
        "old and new gains cannot add up to one valid window"
    );
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
/// Real engine commands must restore processing without changing transmission privacy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_restores_good_processing_and_forgets_only_current_microphone() {
    use icu_client::{
        audio::{
            calibration::{device_key, ProcessingSettings, Profile, Store},
            fake::FakeBackend,
            level::CaptureLevel,
            AudioBackend, BackendError, CaptureCallback, DeviceInfo, PlaybackCallback, StreamKind,
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
            cb: CaptureCallback,
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
    let key = device_key("fake", "calibration-mic");
    let other = device_key("fake", "other-mic");
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
