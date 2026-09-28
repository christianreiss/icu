//! TLS hostname/certificate verification (main session), an in-process
//! server and the fake audio backend (`docs/reference/client.md` §14).
//!
//! Unix only: the in-process server (`common`) is.
#![cfg(unix)]

mod common;

use common::*;
use icu_client::{audio::fake::FakeBackend, engine::ConnStatus};

/// Waits for the first failed attempt, then checks the engine keeps
/// retrying on the slow schedule (`conn::slow_backoff`, 15 s first) instead
/// of ending the reconnects: three seconds later it is still waiting after
/// attempt 1, where the normal backoff (1, 2 s) would be at attempt 3.
async fn retried_slowly(engine: &icu_client::engine::Engine) -> icu_client::problem::Problem {
    let state = wait_for(engine, SOON, |s| {
        matches!(
            &s.conn,
            ConnStatus::Disconnected { .. } | ConnStatus::Fatal(_)
        )
    })
    .await;
    let ConnStatus::Disconnected { reason, attempt } = state.conn else {
        panic!("fatal: {:?}", state.conn);
    };
    assert_eq!(attempt, 1);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    match engine.snapshot().conn {
        ConnStatus::Disconnected { attempt: 1, .. } => {}
        other => panic!("not waiting on the slow schedule: {other:?}"),
    }
    reason
}

/// A stored identity whose `server_name` no longer matches the leaf cert
/// (e.g. hand-edited, or the server moved to a different FQDN) fails the
/// handshake with a precise error naming that FQDN. The server proved
/// nothing before our check refused its certificate, so this is retried —
/// slowly — and not fatal (review 2026-09-27, C-1; until then it ended the
/// reconnects for good).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_server_name_is_retried_slowly_not_fatal() {
    let server = TestServer::start().await;
    let (identity, paths, dir) = server.enroll("alice").await;

    let mut identity = identity;
    identity.server.server_name = "wrong.invalid".to_string();
    identity.save(&paths).expect("identity re-saved");

    let (backend, _probe) = FakeBackend::silent();
    let engine = server
        .engine_from(identity, paths, Box::new(backend), false)
        .expect("engine starts");

    let reason = retried_slowly(&engine).await;
    let technical = reason.technical.as_deref().unwrap_or_default();
    assert!(
        technical.contains("wrong.invalid"),
        "the technical detail should name the mismatched FQDN, got: {technical}"
    );
    assert!(
        reason.text.contains("Serveradresse passt nicht"),
        "user text: {}",
        reason.text
    );

    engine.shutdown();
    server.handle.shutdown().await;
    drop(dir);
}

/// A server whose certificate comes from another CA — set up anew, or
/// somebody on the path who holds the com key — is not taken at its word:
/// the client says it cannot confirm the server, points to the admin on
/// another channel instead of to a new invitation (which is what an
/// attacker would offer next), and keeps retrying slowly (review
/// 2026-09-27, C-1). The raw TLS error stays technical detail. The new
/// server keeps the old com key: with a new one the client would hear
/// nothing at all (`docs/reference/protocol.md` §1, §5).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_from_another_ca_is_retried_slowly_not_fatal() {
    let old_server = TestServer::start().await;
    let (identity, paths, dir) = old_server.enroll("alice").await;
    let new_server = TestServer::start_with_com_key(Some(&identity.com_key)).await;

    let (backend, _probe) = FakeBackend::silent();
    let engine = new_server
        .engine_from(identity, paths, Box::new(backend), false)
        .expect("engine starts");

    let problem = retried_slowly(&engine).await;
    assert_eq!(problem.text, icu_client::problem::SERVER_UNVERIFIED);
    assert!(!problem.text.contains("neue Einladung"), "{}", problem.text);
    for jargon in [
        "UnknownIssuer",
        "BadSignature",
        "TLS",
        "Zertifikatsprüfung",
        "tls:",
    ] {
        assert!(
            !problem.text.contains(jargon),
            "{jargon} in {}",
            problem.text
        );
    }
    assert!(
        // `UnknownIssuer` in the field; `BadSignature` here, where both test
        // CAs carry the same subject.
        problem
            .technical
            .as_deref()
            .unwrap_or_default()
            .contains("Zertifikatsprüfung fehlgeschlagen"),
        "technical: {:?}",
        problem.technical
    );

    engine.shutdown();
    old_server.handle.shutdown().await;
    new_server.handle.shutdown().await;
    drop(dir);
}

/// `docs/reference/protocol.md` §2.2 `CrlUpdate`: a connected client stores the server's
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
    let endpoint =
        icu_client::net::bind_endpoint(server.addr(), &identity.com_key).expect("endpoint");
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

/// Connects `identity` once to `addr` and returns why the handshake failed.
async fn handshake_error(
    identity: &icu_client::config::Identity,
    addr: std::net::SocketAddr,
) -> icu_client::net::conn::ConnectError {
    let (config, cert_failure) = icu_client::net::conn::client_config(identity).expect("config");
    let endpoint = icu_client::net::bind_endpoint(addr, &identity.com_key).expect("endpoint");
    icu_client::net::conn::connect(
        &endpoint,
        config,
        addr,
        "localhost",
        "icu-test/0.1.0 linux",
        icu_proto::UserFlags::NONE,
        "alice",
        &cert_failure,
    )
    .await
    .err()
    .expect("the handshake fails")
}

/// A revoked client learns it from the server's `certificate_revoked`,
/// sent after the server proved its identity: that stays fatal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_client_is_told_so_after_the_server_proved_itself() {
    let server = TestServer::start().await;
    let (_admin, _paths, _admin_dir) = server.enroll("alice").await;
    let (identity, _paths, dir) = server.enroll("bob").await;
    server
        .handle
        .admin(icu_server::AdminRequest::Revoke { user: "bob".into() })
        .await;
    let err = handshake_error(&identity, server.addr()).await;
    assert!(
        matches!(
            err,
            icu_client::net::conn::ConnectError::Certificate {
                issue: icu_client::net::conn::CertIssue::RevokedByServer,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.is_fatal());
    server.handle.shutdown().await;
    drop(dir);
}

/// An alert in reply to our first packet — here a real one, from a QUIC
/// server with another ALPN — comes before the server proved anything.
/// Anyone on the path could send it, so it is retried, not fatal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_alert_before_server_authentication_is_retried() {
    let server = TestServer::start().await;
    let (identity, _paths, dir) = server.enroll("alice").await;

    let key = rcgen::KeyPair::generate().expect("key");
    let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
        .expect("params")
        .self_signed(&key)
        .expect("cert");
    let mut tls = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("tls13")
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
    )
    .expect("server tls");
    tls.alpn_protocols = vec![b"not-icu".to_vec()];
    let quic = quinn::ServerConfig::with_crypto(std::sync::Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic"),
    ));
    // It speaks with the client's com key: otherwise the client would hear
    // nothing at all, and the alert under test would never arrive.
    let other = icu_proto::com::endpoint(
        std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"),
        &identity.com_key,
        icu_proto::com::Side::Server,
        Some(quic),
        Default::default(),
    )
    .expect("bind");
    let addr = other.local_addr().unwrap();
    let accept = tokio::spawn(async move {
        if let Some(incoming) = other.accept().await {
            let _ = incoming.await;
        }
        other
    });

    let err = handshake_error(&identity, addr).await;
    assert!(
        matches!(err, icu_client::net::conn::ConnectError::VersionHint(_)),
        "{err:?}"
    );
    assert!(!err.is_fatal());
    drop(accept.await);
    server.handle.shutdown().await;
    drop(dir);
}
