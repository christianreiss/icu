//! TLS hostname/certificate verification (main session), an in-process
//! server and the fake audio backend (`Client/IMPLEMENTATION.md` §14).

mod common;

use common::*;
use icu_client::{
    audio::fake::FakeBackend,
    config::Settings,
    engine::{ConnStatus, Engine, EngineConfig},
};

/// A stored identity whose `server_name` no longer matches the leaf cert
/// (e.g. hand-edited, or the server rotated to a different FQDN) must fail
/// the handshake with a precise, non-retryable error naming that FQDN — not
/// cycle through reconnect attempts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_server_name_is_a_fatal_certificate_error() {
    let server = TestServer::start().await;
    let (identity, paths, dir) = server.enroll("alice").await;

    let mut identity = identity;
    identity.server.server_name = "wrong.invalid".to_string();
    identity.save(&paths).expect("identity re-saved");

    let (backend, _probe) = FakeBackend::silent();
    let mut settings = Settings::default();
    settings.audio.aec = false;
    settings.audio.denoise = false;
    settings.ui.register_uri_handler = false;
    let mut cfg = EngineConfig::new(identity, settings, paths);
    cfg.server_addr = Some(server.addr());
    let engine = Engine::start(cfg, Box::new(backend)).expect("engine starts");

    let state = wait_for(&engine, SOON, |s| matches!(&s.conn, ConnStatus::Fatal(_))).await;
    let ConnStatus::Fatal(reason) = &state.conn else {
        unreachable!("predicate guarantees Fatal");
    };
    assert!(
        reason.contains("wrong.invalid"),
        "reason should name the mismatched FQDN, got: {reason}"
    );

    engine.shutdown();
    server.handle.shutdown().await;
    drop(dir);
}
