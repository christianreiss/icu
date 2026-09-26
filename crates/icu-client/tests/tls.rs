//! TLS hostname/certificate verification (main session), an in-process
//! server and the fake audio backend (`Client/IMPLEMENTATION.md` §14).

mod common;

use common::*;
use icu_client::{audio::fake::FakeBackend, engine::ConnStatus};

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
    let engine = server
        .engine_from(identity, paths, Box::new(backend), false)
        .expect("engine starts");

    let state = wait_for(&engine, SOON, |s| matches!(&s.conn, ConnStatus::Fatal(_))).await;
    let ConnStatus::Fatal(reason) = &state.conn else {
        unreachable!("predicate guarantees Fatal");
    };
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

/// A client whose credentials come from another CA — the server was set up
/// anew since — is told in plain words to ask for a new invitation, with
/// the raw TLS error kept only as technical detail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credentials_from_another_ca_read_as_a_plain_request_for_an_invite() {
    let old_server = TestServer::start().await;
    let (identity, paths, dir) = old_server.enroll("alice").await;
    let new_server = TestServer::start().await;

    let (backend, _probe) = FakeBackend::silent();
    let engine = new_server
        .engine_from(identity, paths, Box::new(backend), false)
        .expect("engine starts");

    let state = wait_for(&engine, SOON, |s| matches!(&s.conn, ConnStatus::Fatal(_))).await;
    let ConnStatus::Fatal(problem) = &state.conn else {
        unreachable!("predicate guarantees Fatal");
    };
    assert!(
        problem.text.contains("neue Einladung"),
        "user text: {}",
        problem.text
    );
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

/// Connects `identity` once to `addr` and returns why the handshake failed.
async fn handshake_error(
    identity: &icu_client::config::Identity,
    addr: std::net::SocketAddr,
) -> icu_client::net::conn::ConnectError {
    let (config, cert_failure) = icu_client::net::conn::client_config(identity).expect("config");
    let endpoint = icu_client::net::bind_endpoint(addr).expect("endpoint");
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
    let other = quinn::Endpoint::server(quic, "127.0.0.1:0".parse().unwrap()).expect("bind");
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
