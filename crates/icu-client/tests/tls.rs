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

/// `PROTOCOL.md` §2.2 `CrlUpdate`: a connected client stores the server's
/// CRL, verified against its CA.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connected_client_stores_the_servers_crl() {
    let server = TestServer::start().await;
    let (backend, _probe) = FakeBackend::silent();
    let (engine, paths, dir) = server
        .engine_with_paths("alice", Box::new(backend), false)
        .await;
    wait_for(&engine, SOON, |s| matches!(s.conn, ConnStatus::Connected)).await;
    let ca = server.handle.ca_der();
    let mut stored = None;
    for _ in 0..100 {
        stored = icu_client::crl::load(&paths, &ca);
        if stored.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let stored = stored.expect("the CRL from the server is stored");
    assert!(icu_client::crl::verify(&stored, &ca).is_ok());
    engine.shutdown();
    server.handle.shutdown().await;
    drop(dir);
}

/// A server leaf on the stored CRL — replaced by `rotate-server-cert`, its
/// key possibly leaked — fails the handshake. Not fatally: only the server
/// can fix it, so the client keeps retrying and refusing that leaf.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_leaf_on_the_crl_is_refused() {
    let server = TestServer::start().await;
    let (identity, _paths, dir) = server.enroll("alice").await;
    let crl = {
        let ca = server.handle.server().ca.read().await;
        let serial = icu_server::ca::leaf_serial(ca.server_der());
        ca.crl(1_000, &[serial]).expect("crl")
    };
    let (config, cert_failure) =
        icu_client::net::conn::client_config_with_crl(&identity, Some(&crl)).expect("config");
    let endpoint = icu_client::net::bind_endpoint(server.addr()).expect("endpoint");
    let err = icu_client::net::conn::connect(
        &endpoint,
        config,
        server.addr(),
        "localhost",
        "icu-test/0.1.0 linux",
        icu_proto::UserFlags::NONE,
        "alice",
        &cert_failure,
    )
    .await
    .err()
    .expect("the handshake fails");
    assert!(!err.is_fatal(), "{err}");
    assert!(
        err.to_string().to_lowercase().contains("revoked")
            || err.to_string().contains("widerrufen"),
        "{err}"
    );

    // Without the CRL the same identity connects: it is the CRL that refused.
    let (config, cert_failure) = icu_client::net::conn::client_config(&identity).expect("config");
    assert!(icu_client::net::conn::connect(
        &endpoint,
        config,
        server.addr(),
        "localhost",
        "icu-test/0.1.0 linux",
        icu_proto::UserFlags::NONE,
        "alice",
        &cert_failure,
    )
    .await
    .is_ok());
    server.handle.shutdown().await;
    drop(dir);
}
