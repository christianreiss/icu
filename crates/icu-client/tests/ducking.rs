//! Actual engine → QUIC relay → decoder → fake device callback evidence.
//! Native session gains are covered separately by opt-in platform tests.
#![cfg(unix)]
mod common;
use common::{wait_connected, wait_for, TestServer, SOON};
use icu_client::{
    audio::fake::FakeBackend,
    engine::{Command, Engine},
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

async fn speech(engine: &Engine) {
    let until = Instant::now() + SOON;
    while engine.ducking_input().speech_db.is_none() {
        assert!(
            Instant::now() < until,
            "No played speech: {:?}",
            engine.ducking_input()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
async fn no_speech(engine: &Engine) {
    tokio::time::sleep(Duration::from_millis(200)).await;
    for _ in 0..10 {
        assert!(
            engine.ducking_input().speech_db.is_none(),
            "{:?}",
            engine.ducking_input()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_sent_and_audible_speech_triggers_ducking_in_the_real_engine() {
    let server = TestServer::start().await;
    let active = Arc::new(AtomicBool::new(false));
    let enabled = active.clone();
    let voice = icu_client::dsp::testsignal::voice(48_000 * 4, 7, 0.08).samples;
    let (backend, _) = FakeBackend::new(Box::new(move |offset, out| {
        for (i, s) in out.iter_mut().enumerate() {
            *s = if enabled.load(Ordering::Relaxed) {
                voice[(offset as usize + i) % voice.len()]
            } else {
                0.0
            };
        }
    }));
    let (sender, _sender_dir) = server.engine("speaker", Box::new(backend), false).await;
    let (receiver, _receiver_dir) = server
        .engine("listener", Box::new(FakeBackend::silent().0), false)
        .await;
    let user = wait_connected(&sender).await.me.unwrap().user_id;
    wait_connected(&receiver).await;
    sender.command(Command::Ptt(true));
    no_speech(&receiver).await;
    assert!(
        !sender.ducking_input().transmitting_voice,
        "PTT silence must not duck"
    );
    active.store(true, Ordering::Relaxed);
    speech(&receiver).await;
    let until = Instant::now() + SOON;
    while !sender.ducking_input().transmitting_voice {
        assert!(Instant::now() < until, "sent speech was never observed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    receiver.command(Command::SetVolume(user, 0.0));
    no_speech(&receiver).await;
    receiver.command(Command::SetVolume(user, 100.0));
    speech(&receiver).await;
    receiver.command(Command::SetDeafened(true));
    wait_for(&receiver, SOON, |s| s.deafened).await;
    no_speech(&receiver).await;
    receiver.command(Command::SetDeafened(false));
    speech(&receiver).await;
    let mut settings = receiver.snapshot().settings;
    settings.audio.output_volume = 0.0;
    receiver.command(Command::UpdateSettings(Box::new(settings)));
    no_speech(&receiver).await;
    active.store(false, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !sender.ducking_input().transmitting_voice,
        "open PTT silence must expire"
    );
    sender.command(Command::SetMicTest(true));
    active.store(true, Ordering::Relaxed);
    sender.command(Command::SetMuted(true));
    wait_for(&sender, SOON, |s| s.muted).await;
    no_speech(&sender).await;
    assert!(
        !sender.ducking_input().transmitting_voice,
        "muted mic test must not duck"
    );
    sender.shutdown();
    receiver.shutdown();
    server.handle.shutdown().await;
}
