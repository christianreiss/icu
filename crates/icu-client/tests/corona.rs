//! Each user's own corona color (protocol minor 5): picked in the
//! settings, stored on the server, shown for that user on every client;
//! a color picked before it was shared carries over, and a device that
//! connects later takes over the stored one.
//!
//! Unix only: the in-process server (`common`) is.
#![cfg(unix)]

mod common;

use common::{test_settings, wait_connected, wait_for, TestServer, SOON};
use icu_client::{
    audio::fake::FakeBackend,
    config::{Identity, DEFAULT_FRAME_COLOR},
    engine::{AppState, Command, Engine, EngineConfig},
};

const PINK: [u8; 3] = [255, 105, 180];

fn peer_color(s: &AppState, name: &str) -> Option<Option<[u8; 3]>> {
    s.peers
        .iter()
        .find(|p| p.info.name == name)
        .map(|p| p.info.corona_color)
}

#[tokio::test]
async fn own_corona_color_reaches_everybody_and_follows_the_user() {
    let server = TestServer::start().await;
    let (alice, alice_paths, _alice_dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    let (bob, _bob_dir) = server
        .engine("bob", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&alice).await;
    wait_connected(&bob).await;
    wait_for(&bob, SOON, |s| peer_color(s, "alice") == Some(None)).await;

    // Alice picks pink: bob sees it, and so does alice's own `me`.
    let mut settings = alice.state().borrow().settings.clone();
    settings.overlay.frame_color = PINK;
    alice.command(Command::UpdateSettings(Box::new(settings.clone())));
    wait_for(&bob, SOON, |s| peer_color(s, "alice") == Some(Some(PINK))).await;
    wait_for(&alice, SOON, |s| {
        s.me.as_ref().and_then(|m| m.corona_color) == Some(PINK)
    })
    .await;

    // A second start of alice's identity with default local settings takes
    // the stored color over instead of resetting it.
    alice.shutdown();
    wait_for(&bob, SOON, |s| peer_color(s, "alice").is_none()).await;
    let identity = Identity::load(&alice_paths).unwrap().unwrap();
    let alice = server
        .engine_from(
            identity,
            alice_paths.clone(),
            Box::new(FakeBackend::silent().0),
            false,
        )
        .expect("engine starts");
    wait_for(&alice, SOON, |s| s.settings.overlay.frame_color == PINK).await;
    wait_for(&bob, SOON, |s| peer_color(s, "alice") == Some(Some(PINK))).await;

    // Back to the default: the server forgets it.
    let mut settings = alice.state().borrow().settings.clone();
    settings.overlay.frame_color = DEFAULT_FRAME_COLOR;
    alice.command(Command::UpdateSettings(Box::new(settings)));
    wait_for(&bob, SOON, |s| peer_color(s, "alice") == Some(None)).await;

    // A color picked while it was still only local goes up on connect.
    let (identity, paths, _carol_dir) = server.enroll("carol").await;
    let mut settings = test_settings();
    settings.overlay.frame_color = [0, 200, 0];
    let mut cfg = EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    let carol = Engine::start(cfg, Box::new(FakeBackend::silent().0)).expect("engine starts");
    wait_for(&bob, SOON, |s| {
        peer_color(s, "carol") == Some(Some([0, 200, 0]))
    })
    .await;

    for engine in [alice, bob, carol] {
        engine.shutdown();
    }
    server.handle.shutdown().await;
}
