//! Registration and authentication (`Server/IMPLEMENTATION.md` §10, rows
//! "Registrierung → Connect → Hello/Welcome", "Connect ohne Cert",
//! "unbekanntes Cert", "Revoke während Verbindung", "RenewRequest").

mod common;

use common::{EnrollFail, TestServer, SOON};
use icu_proto::{error as ec, ControlMsg};
use icu_server::{AdminRequest, AdminResponse};

#[tokio::test]
async fn enrollment_then_connect_then_hello_welcome() {
    let server = TestServer::start().await;
    let identity = server.enroll("alice").await;
    assert_eq!(identity.ca, server.ca(), "the CA travels in the bundle");

    let mut client = server.connect(&identity).await.expect("connect");
    let welcome = client.hello("alice").await;
    match welcome {
        ControlMsg::Welcome {
            minor,
            you,
            users,
            min_client,
            audio,
            ..
        } => {
            assert_eq!(minor, icu_proto::MINOR);
            assert_eq!(you.user_id, 1, "first enrolled user");
            assert_eq!(you.session, 1, "session ids start at 1");
            assert_eq!(you.name, "alice");
            assert!(users.is_empty(), "Welcome.users is without oneself");
            assert_eq!(min_client, "0.1.0");
            assert_eq!(audio.sample_rate, 48_000);
            assert_eq!(audio.frame_ms, 20);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn connect_without_a_certificate_is_auth_required() {
    let server = TestServer::start().await;
    // Spike 2: `connect()` succeeds on the client side even when the server
    // rejects right after, so the test waits for the close.
    match server.connect_anonymous().await {
        Ok(client) => {
            assert_eq!(client.closed_code().await, Some(ec::AUTH_REQUIRED));
        }
        Err(e) => panic!("connect itself should succeed, got {e}"),
    }
    server.shutdown().await;
}

/// `PROTOCOL.md` §7: past `max_connections` established connections,
/// `Hello` or not, a completed handshake is closed with the non-fatal
/// `SERVER_FULL` instead of accumulating state.
#[tokio::test]
async fn connections_past_the_limit_are_closed_with_server_full() {
    let server = TestServer::start_with(|config| {
        config.server.max_users = 1;
        config.limits.max_connections = 2;
    })
    .await;
    let alice = server.enroll("alice").await;
    // The registration task outlives the client's close by a store write;
    // wait until it has let go of its slot.
    let open = &server.handle.server().open_connections;
    for _ in 0..200 {
        if open.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let bob = server.enroll("bob").await;
    let carol = server.enroll("carol").await;
    for _ in 0..200 {
        if open.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(open.load(std::sync::atomic::Ordering::Relaxed), 0);
    // Connected but silent: each holds a slot until the `Hello` timeout.
    // Three certificates, since a second connection with the same one would
    // replace the first instead of adding to it.
    let _a = server.connect(&alice).await.expect("first connection");
    let _b = server.connect(&bob).await.expect("second connection");
    let third = server
        .connect(&carol)
        .await
        .expect("the handshake itself completes");
    assert_eq!(third.closed_code().await, Some(ec::SERVER_FULL));
    assert!(
        !ec::is_fatal(ec::SERVER_FULL),
        "the client tries again later"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn an_unknown_certificate_from_our_own_ca_is_auth_unknown_cert() {
    let server = TestServer::start().await;
    // A leaf signed by the server's CA but never written to users.toml: only
    // then does the connection reach the session code at all.
    let ca = icu_server::ca::Ca::load_or_create(
        server.dir.path(),
        &icu_server::config::TlsSection::default(),
        "localhost",
    )
    .expect("load ca");
    let (key, csr) = common::csr_with_cn("mallory");
    let (leaf, _) = ca.sign_client_csr(&csr, 365, "localhost:4711").unwrap();

    let identity = common::Identity {
        cert: leaf,
        key_pem: key.serialize_pem(),
        key_der: key.serialize_der(),
        ca: server.ca(),
    };
    let client = server.connect(&identity).await.expect("connect");
    assert_eq!(client.closed_code().await, Some(ec::AUTH_UNKNOWN_CERT));
    server.shutdown().await;
}

#[tokio::test]
async fn revoke_during_a_connection_kicks_with_kicked() {
    // alice is the sole bootstrap admin, and the last-admin guard forbids
    // revoking her (`store::guard_last_admin`); bob, the second, non-admin
    // enrollee, exercises the same revoke+kick mechanics without tripping it.
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = server.client("bob").await;
    assert_eq!(bob.session, 2, "bob joined after alice");

    let response = server
        .handle
        .admin(AdminRequest::Revoke { user: "bob".into() })
        .await;
    match response {
        AdminResponse::Revoke { user_id, closed } => {
            assert_eq!(user_id, 2);
            assert_eq!(closed, 1, "the live connection is closed immediately");
        }
        other => panic!("revoke failed: {other:?}"),
    }

    assert_eq!(bob.closed_code().await, Some(ec::KICKED));

    let left = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserLeft { .. }))
        .await;
    assert!(matches!(
        left,
        Some(ControlMsg::UserLeft { session: 2, .. })
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn a_revoked_certificate_cannot_reconnect() {
    let server = TestServer::start().await;
    server.enroll("alice").await; // sole bootstrap admin, kept untouched
    let identity = server.enroll("bob").await;
    server
        .handle
        .admin(AdminRequest::Revoke { user: "bob".into() })
        .await;
    // On the CRL: refused in the handshake already, no restart needed.
    assert_eq!(
        server.rejection(&identity).await,
        Some(common::TLS_CERTIFICATE_REVOKED)
    );
    server.shutdown().await;
}

#[tokio::test]
async fn revoking_the_sole_admin_is_rejected_with_last_admin() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin

    let response = server
        .handle
        .admin(AdminRequest::Revoke {
            user: "alice".into(),
        })
        .await;
    match response {
        AdminResponse::Error { error, .. } => assert_eq!(error, "LAST_ADMIN"),
        other => panic!("expected LAST_ADMIN, got {other:?}"),
    }

    // The connection was never touched by the rejected admin action.
    alice.send_msg(&ControlMsg::Ping { nonce: 42 }).await;
    let pong = alice.recv_msg().await;
    assert!(matches!(pong, ControlMsg::Pong { nonce: 42, .. }));
    server.shutdown().await;
}

#[tokio::test]
async fn a_disabled_user_cannot_reconnect_with_auth_disabled() {
    let server = TestServer::start().await;
    server.enroll("alice").await; // sole bootstrap admin, kept untouched
    let identity = server.enroll("bob").await;

    let response = server
        .handle
        .admin(AdminRequest::Disable { user: "bob".into() })
        .await;
    assert!(matches!(response, AdminResponse::Disable { closed: 0, .. }));

    let client = server.connect(&identity).await.expect("connect");
    assert_eq!(client.closed_code().await, Some(ec::AUTH_DISABLED));
    server.shutdown().await;
}

#[tokio::test]
async fn disabling_an_online_user_kicks_immediately() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = server.client("bob").await;

    let response = server
        .handle
        .admin(AdminRequest::Disable { user: "bob".into() })
        .await;
    match response {
        AdminResponse::Disable { user_id, closed } => {
            assert_eq!(user_id, 2);
            assert_eq!(closed, 1, "the live connection is closed immediately");
        }
        other => panic!("disable failed: {other:?}"),
    }

    assert_eq!(bob.closed_code().await, Some(ec::AUTH_DISABLED));

    let left = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserLeft { .. }))
        .await;
    assert!(matches!(
        left,
        Some(ControlMsg::UserLeft { session: 2, .. })
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn enabling_restores_login_without_reenrollment() {
    let server = TestServer::start().await;
    server.enroll("alice").await; // sole bootstrap admin, kept untouched
    let identity = server.enroll("bob").await;

    server
        .handle
        .admin(AdminRequest::Disable { user: "bob".into() })
        .await;
    server
        .handle
        .admin(AdminRequest::Enable { user: "bob".into() })
        .await;

    let mut client = server.connect(&identity).await.expect("connect");
    let welcome = client.hello("bob").await;
    assert!(matches!(welcome, ControlMsg::Welcome { .. }));
    server.shutdown().await;
}

#[tokio::test]
async fn a_bundle_works_exactly_once() {
    let server = TestServer::start().await;
    let bundle = server.invite("").await;
    let bob = server.register(&bundle, "bob").await.expect("registers");

    // The certificate on the client's own key is a normal one. Logging in
    // with it proves the client stored it and consumes the bundle — the
    // clean close after `RenewResponse` does the same, but asynchronously.
    let mut client = server.connect(&bob).await.expect("connect");
    match client.hello("bob").await {
        ControlMsg::Welcome {
            you, registration, ..
        } => {
            assert!(!registration);
            assert_eq!(you.name, "bob");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    let reused = common::identity_from_bundle(&bundle);
    assert_eq!(
        server.rejection(&reused).await,
        Some(common::TLS_CERTIFICATE_REVOKED)
    );
    server.shutdown().await;
}

/// `REGISTRATION_STORED` after `RenewResponse` is the client's receipt: the bundle
/// is dead from then on, even before the first login.
#[tokio::test]
async fn a_confirmed_registration_consumes_the_bundle() {
    let server = TestServer::start().await;
    let bundle = server.invite("").await;
    server.register(&bundle, "bob").await.expect("registers");

    // The server sees the close asynchronously; one pause instead of a poll,
    // since every early attempt would spend the per-IP registration budget.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let reused = common::identity_from_bundle(&bundle);
    assert_eq!(
        server.rejection(&reused).await,
        Some(common::TLS_CERTIFICATE_REVOKED),
        "the clean close must consume the bundle"
    );
    server.shutdown().await;
}

/// A client that got no answer, or could not store it, closes without
/// `REGISTRATION_STORED`: the bundle stays valid, and redeeming it again replaces the
/// certificate nobody holds (`PROTOCOL.md` §2.3).
#[tokio::test]
async fn an_unconfirmed_registration_can_be_repeated() {
    let server = TestServer::start().await;
    let bundle = server.invite("").await;
    let lost = server
        .register_unconfirmed(&bundle, "bob")
        .await
        .expect("registers");
    let bob = server
        .register(&bundle, "bob")
        .await
        .expect("the unconfirmed bundle redeems again");

    assert_eq!(
        server.rejection(&lost).await,
        Some(common::TLS_CERTIFICATE_REVOKED),
        "the lost certificate is replaced"
    );
    let mut client = server.connect(&bob).await.expect("connect");
    match client.hello("bob").await {
        ControlMsg::Welcome { you, .. } => {
            assert_eq!(you.name, "bob");
            assert_eq!(you.user_id, 1, "same user, not a second one");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}

/// `NO_ERROR` is no receipt: quinn closes a dropped connection implicitly with
/// code 0, so a client that quit between `RenewResponse` and saving would
/// otherwise burn its bundle (`PROTOCOL.md` §2.3).
#[tokio::test]
async fn a_plain_close_is_no_receipt() {
    let server = TestServer::start().await;
    let bundle = server.invite("").await;
    let (key, csr) = common::csr_with_cn("bob");
    server
        .register_closing(&bundle, key, csr, ec::NO_ERROR)
        .await
        .expect("registers");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let bob = server
        .register(&bundle, "bob")
        .await
        .expect("the bundle is still valid");
    let mut client = server.connect(&bob).await.expect("connect");
    assert!(matches!(
        client.hello("bob").await,
        ControlMsg::Welcome { .. }
    ));
    server.shutdown().await;
}

/// A placeholder whose bundle expired unused frees its name for a
/// registration right away, not only after the next invite or a restart.
#[tokio::test]
async fn an_expired_placeholder_frees_its_name() {
    let server = TestServer::start().await;
    let bundle = server.invite("").await;
    match server
        .handle
        .admin(AdminRequest::Invite {
            name: "dora".into(),
            ttl_s: Some(1),
        })
        .await
    {
        AdminResponse::Invite { .. } => {}
        other => panic!("invite failed: {other:?}"),
    }
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    server
        .register(&bundle, "dora")
        .await
        .expect("the expired placeholder no longer holds the name");
    server.shutdown().await;
}

/// A registration refused by the rate limit must not first replace a
/// session of the same bundle that is still in progress.
#[tokio::test]
async fn a_rate_limited_attempt_leaves_the_live_registration_alone() {
    let server = TestServer::start_with(|config| {
        config.limits.enroll_per_ip_per_min = 1;
    })
    .await;
    let bundle = server.invite("").await;
    let registration = common::identity_from_bundle(&bundle);
    let mut live = server.connect(&registration).await.expect("connect");
    assert!(matches!(
        live.hello("").await,
        ControlMsg::Welcome {
            registration: true,
            ..
        }
    ));
    assert_eq!(
        server.rejection(&registration).await,
        Some(ec::ENROLL_RATE_LIMITED)
    );
    let (_key, csr) = common::csr_with_cn("erin");
    live.send_msg(&ControlMsg::RenewRequest { csr_der: csr })
        .await;
    let answer = live
        .recv_matching(|m| !matches!(m, ControlMsg::Pong { .. }))
        .await;
    assert!(
        matches!(answer, Some(ControlMsg::RenewResponse { .. })),
        "{answer:?}"
    );
    server.shutdown().await;
}

/// The key in the bundle has been seen by whoever carried the bundle, so the
/// server insists on a new one.
#[tokio::test]
async fn registering_with_the_bundle_key_is_rejected() {
    let server = TestServer::start().await;
    let bundle = server.invite("").await;
    let parts = icu_proto::passport::Passport::decode(&bundle)
        .unwrap()
        .into_parts();
    let key = rcgen::KeyPair::try_from(parts.key_der.as_slice()).unwrap();
    let mut params = rcgen::CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "mallory");
    let csr = params.serialize_request(&key).unwrap().der().to_vec();

    let err = server
        .register_with_csr(&bundle, key, csr)
        .await
        .expect_err("the bundle key must not be kept");
    assert_eq!(err.code(), Some(ec::RENEW_REJECTED), "{err:?}");
    assert!(
        server.register(&bundle, "carol").await.is_ok(),
        "the bundle is still unused"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn an_expired_bundle_is_refused() {
    let server = TestServer::start().await;
    let bundle = match server
        .handle
        .admin(AdminRequest::Invite {
            name: String::new(),
            ttl_s: Some(1),
        })
        .await
    {
        AdminResponse::Invite { bundle, .. } => bundle,
        other => panic!("invite failed: {other:?}"),
    };
    // The certificate carries an hour of clock slack before `not_before`,
    // but `not_after` is exact.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let err = server
        .register(&bundle, "late")
        .await
        .expect_err("an expired registration certificate must not connect");
    assert!(matches!(err, EnrollFail::Connection(_)), "{err:?}");
    server.shutdown().await;
}

/// A registration session is not a member of the room: anything but a
/// renewal or a ping is a protocol violation.
#[tokio::test]
async fn a_registration_session_may_only_renew() {
    let server = TestServer::start().await;
    let _alice = server.client("alice").await;
    let bundle = server.invite("").await;
    let mut client = server
        .connect(&common::identity_from_bundle(&bundle))
        .await
        .expect("connect");
    match client.hello("").await {
        ControlMsg::Welcome {
            registration,
            users,
            you,
            ..
        } => {
            assert!(registration);
            assert!(users.is_empty(), "no roster before registering");
            assert_eq!(you.session, 0, "not in the room");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    client.send_msg(&ControlMsg::Ping { nonce: 7 }).await;
    assert!(matches!(
        client.recv_msg().await,
        ControlMsg::Pong { nonce: 7, .. }
    ));
    client.send_audio(1, 0, &[1; 30]);
    client
        .send_msg(&ControlMsg::SetState {
            flags: icu_proto::UserFlags::NONE,
        })
        .await;
    assert_eq!(client.closed_code().await, Some(ec::PROTOCOL_VIOLATION));
    server.shutdown().await;
}

/// §10: a taken name is refused outright — no silent `-2` suffix — and the
/// bundle survives the attempt, so the user can pick another name.
#[tokio::test]
async fn a_taken_name_at_registration_leaves_the_bundle_reusable() {
    let server = TestServer::start().await;
    server.enroll("alice").await;

    let bundle = server.invite("").await;
    let err = server
        .register(&bundle, "ALICE")
        .await
        .expect_err("a taken name is refused");
    assert_eq!(err.code(), Some(ec::NAME_TAKEN), "{err:?}");

    // Same bundle, another name: this is what makes the refusal recoverable.
    let carol = server
        .register(&bundle, "carol")
        .await
        .expect("the bundle is still unused");
    let mut client = server.connect(&carol).await.expect("connect");
    match client.hello("ignored").await {
        ControlMsg::Welcome { you, .. } => {
            assert_eq!(you.name, "carol");
            assert_eq!(you.user_id, 2);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}

/// Every client certificate says where the server is, so a passport needs no
/// separate address (`PROTOCOL.md` §2.3).
#[tokio::test]
async fn certificates_carry_the_server_address() {
    use x509_parser::prelude::FromDer;
    let server = TestServer::start().await;
    let bundle = server.invite("").await;
    let registration = common::identity_from_bundle(&bundle);
    let alice = server.register(&bundle, "alice").await.unwrap();
    for der in [&registration.cert, &alice.cert] {
        let (_, cert) = x509_parser::certificate::X509Certificate::from_der(der).unwrap();
        let uris: Vec<String> = cert
            .subject_alternative_name()
            .unwrap()
            .unwrap()
            .value
            .general_names
            .iter()
            .filter_map(|n| match n {
                x509_parser::extensions::GeneralName::URI(u) => Some(u.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(uris, vec!["icu://localhost:4711".to_string()]);
    }
    let (_, ca) = x509_parser::certificate::X509Certificate::from_der(&alice.ca).unwrap();
    let cn = ca.subject().iter_common_name().next().unwrap();
    assert_eq!(cn.as_str().unwrap(), "ICU CA localhost");
    server.shutdown().await;
}

#[tokio::test]
async fn renewal_revokes_the_old_certificate_once_the_new_one_logs_in() {
    // Short-lived certificates, so the fresh one is already inside the
    // 30-day renewal window (`PROTOCOL.md` §2.2 Nr. 9).
    let server = TestServer::start_with(|c| c.tls.client_validity_days = 20).await;
    let old = server.enroll("alice").await;
    let mut client = server.connect(&old).await.expect("connect");
    client.hello("alice").await;

    let (new_key, csr) = common::csr_with_cn("alice");
    client
        .send_msg(&ControlMsg::RenewRequest {
            csr_der: csr.clone(),
        })
        .await;
    let response = client
        .recv_matching(|m| matches!(m, ControlMsg::RenewResponse { .. }))
        .await
        .expect("RenewResponse");
    let ControlMsg::RenewResponse { leaf_der } = response else {
        unreachable!()
    };
    let renewed = common::Identity {
        cert: rustls::pki_types::CertificateDer::from(leaf_der),
        key_pem: new_key.serialize_pem(),
        key_der: new_key.serialize_der(),
        ca: server.ca(),
    };

    // Until the new certificate logs in, the old one still does: the
    // response may have been lost. Connecting it again replaces the first
    // connection that used it (one per certificate).
    let mut with_old = server.connect(&old).await.expect("connect old");
    assert!(matches!(
        with_old.hello("alice").await,
        ControlMsg::Welcome { .. }
    ));
    assert_eq!(client.closed_code().await, Some(ec::SESSION_REPLACED));

    // The new certificate connects...
    let mut with_new = server.connect(&renewed).await.expect("connect new");
    match &with_new.hello("alice").await {
        ControlMsg::Welcome { you, .. } => {
            assert_eq!(you.user_id, 1, "the user_id survives the renewal");
            assert_eq!(you.name, "alice");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    // ...which revokes the old one: its live session is closed, and it
    // cannot come back.
    assert_eq!(with_old.closed_code().await, Some(ec::AUTH_REVOKED));
    let code = server.rejection(&old).await;
    assert!(
        code == Some(common::TLS_CERTIFICATE_REVOKED) || code == Some(ec::AUTH_REVOKED),
        "got {code:?}"
    );

    // The new session is untouched, and the user is not revoked.
    with_new.send_msg(&ControlMsg::Ping { nonce: 7 }).await;
    assert!(with_new
        .recv_matching(|m| matches!(m, ControlMsg::Pong { .. }))
        .await
        .is_some());
    server.shutdown().await;
}

#[tokio::test]
async fn an_invalid_renewal_csr_is_rejected_without_closing() {
    let server = TestServer::start().await;
    let mut client = server.client("alice").await;
    client
        .send_msg(&ControlMsg::RenewRequest {
            csr_der: b"not a csr".to_vec(),
        })
        .await;
    let answer = client
        .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
        .await;
    assert!(
        matches!(answer, Some(ControlMsg::Error { code, .. }) if code == ec::RENEW_REJECTED),
        "{answer:?}"
    );
    // The connection is still usable.
    client.send_msg(&ControlMsg::Ping { nonce: 1 }).await;
    assert!(client
        .recv_matching(|m| matches!(m, ControlMsg::Pong { .. }))
        .await
        .is_some());
    server.shutdown().await;
}

#[tokio::test]
async fn registration_is_rate_limited_per_ip() {
    let server = TestServer::start_with(|config| {
        config.limits.enroll_per_ip_per_min = 2;
    })
    .await;
    let mut codes = Vec::new();
    for i in 0..4 {
        let bundle = server.invite("").await;
        codes.push(
            server
                .register(&bundle, &format!("user{i}"))
                .await
                .err()
                .and_then(|e| e.code()),
        );
    }
    assert_eq!(codes[0], None);
    assert_eq!(codes[1], None);
    assert_eq!(
        codes[2],
        Some(ec::ENROLL_RATE_LIMITED),
        "third registration from the same IP within a minute"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_shut_down_server_closes_with_shutdown() {
    let server = TestServer::start().await;
    let client = server.client("alice").await;
    let conn = client.conn.clone();
    server.shutdown().await;
    let reason = tokio::time::timeout(SOON, conn.closed())
        .await
        .expect("closed");
    assert_eq!(common::code_of(&reason), Some(ec::SHUTDOWN));
}

/// A plain renewal is refused while the presented certificate still has
/// more than 30 days to run (`PROTOCOL.md` §2.2 Nr. 9).
#[tokio::test]
async fn plain_renewal_is_refused_outside_the_window() {
    let server = TestServer::start().await;
    let alice = server.enroll("alice").await;
    let mut client = server.connect(&alice).await.expect("connect");
    client.hello("alice").await;

    let (_, csr) = common::csr_with_cn("alice");
    client
        .send_msg(&ControlMsg::RenewRequest { csr_der: csr })
        .await;
    let answer = client
        .recv_matching(|m| {
            matches!(
                m,
                ControlMsg::Error { .. } | ControlMsg::RenewResponse { .. }
            )
        })
        .await;
    assert!(
        matches!(answer, Some(ControlMsg::Error { code, .. }) if code == ec::RENEW_REJECTED),
        "{answer:?}"
    );
    server.shutdown().await;
}

/// Serials a CRL lists, lowercase hex, after checking its CA signature.
fn crl_serials(crl_der: &[u8], ca: &[u8]) -> Vec<String> {
    use x509_parser::prelude::FromDer;
    let (_, ca) = x509_parser::certificate::X509Certificate::from_der(ca).unwrap();
    let (_, crl) =
        x509_parser::revocation_list::CertificateRevocationList::from_der(crl_der).unwrap();
    crl.verify_signature(ca.public_key())
        .expect("the CRL is signed by the CA");
    crl.iter_revoked_certificates()
        .map(|r| icu_server::store::encode_hex(r.raw_serial()))
        .collect()
}

/// The serial of a leaf, lowercase hex.
fn serial_of(der: &[u8]) -> String {
    icu_server::ca::leaf_serial(der)
}

/// `PROTOCOL.md` §7: one live connection per certificate, the newest wins
/// and the older one ends with the fatal `SESSION_REPLACED`.
#[tokio::test]
async fn a_second_connection_with_the_same_certificate_replaces_the_first() {
    let server = TestServer::start().await;
    let mut watcher = server.client("alice").await;
    let bob = server.enroll("bob").await;
    let mut first = server.connect(&bob).await.expect("connect");
    first.hello("bob").await;
    let first_session = first.session;
    watcher
        .recv_matching(|m| matches!(m, ControlMsg::UserJoined { .. }))
        .await
        .expect("UserJoined");

    let mut second = server.connect(&bob).await.expect("connect again");
    second.hello("bob").await;
    assert_eq!(first.closed_code().await, Some(ec::SESSION_REPLACED));
    assert!(ec::is_fatal(ec::SESSION_REPLACED), "no reconnect ping-pong");
    let left = watcher
        .recv_matching(|m| matches!(m, ControlMsg::UserLeft { .. }))
        .await;
    assert!(
        matches!(left, Some(ControlMsg::UserLeft { session, reason })
            if session == first_session && u32::from(reason) == ec::SESSION_REPLACED),
        "{left:?}"
    );

    // The replacement works normally.
    second.send_msg(&ControlMsg::Ping { nonce: 7 }).await;
    assert!(matches!(
        second
            .recv_matching(|m| matches!(m, ControlMsg::Pong { .. }))
            .await,
        Some(ControlMsg::Pong { nonce: 7, .. })
    ));
    server.shutdown().await;
}

/// The same rule holds for registration sessions: two holders of one bundle
/// cannot run in parallel.
#[tokio::test]
async fn a_registration_session_is_replaced_by_a_newer_one() {
    let server = TestServer::start().await;
    let bundle = server.invite("").await;
    let identity = common::identity_from_bundle(&bundle);
    let mut first = server.connect(&identity).await.expect("connect");
    first.hello("x").await;
    let mut second = server.connect(&identity).await.expect("connect again");
    second.hello("x").await;
    assert_eq!(first.closed_code().await, Some(ec::SESSION_REPLACED));
    server.shutdown().await;
}

/// `PROTOCOL.md` §7: sessions outside the room — registration and
/// renew-only — are capped server wide, since the room size does not bound
/// them. A room session is not affected, and a freed slot is taken again.
#[tokio::test]
async fn sessions_outside_the_room_are_capped() {
    use icu_proto::limits::MAX_ROOMLESS_SESSIONS;
    let server = TestServer::start_with(|c| {
        // Every session comes from 127.0.0.1 within a second.
        c.limits.handshakes_per_ip_per_s = 1000;
        c.limits.enroll_per_ip_per_min = 1000;
        c.limits.enroll_global_per_min = 1000;
    })
    .await;
    let admin = server.enroll("admin").await;
    let registration = |bundle: &str| common::identity_from_bundle(bundle);

    let mut open = Vec::new();
    for _ in 0..MAX_ROOMLESS_SESSIONS {
        let bundle = server.invite("").await;
        let mut client = server
            .connect(&registration(&bundle))
            .await
            .expect("connect");
        assert!(matches!(
            client.hello("").await,
            ControlMsg::Welcome {
                registration: true,
                ..
            }
        ));
        open.push(client);
    }
    let bundle = server.invite("").await;
    let refused = server
        .connect(&registration(&bundle))
        .await
        .expect("the handshake completes");
    assert_eq!(refused.closed_code().await, Some(ec::SERVER_FULL));
    assert!(
        !ec::is_fatal(ec::SERVER_FULL),
        "the client tries again later"
    );

    let mut member = server.connect(&admin).await.expect("connect");
    assert!(
        matches!(member.hello("admin").await, ControlMsg::Welcome { .. }),
        "the room is not affected"
    );

    open.pop().expect("a session").conn.close(0u32.into(), b"");
    let roomless = &server.handle.server().roomless;
    for _ in 0..200 {
        if roomless.lock().unwrap().len() < MAX_ROOMLESS_SESSIONS {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let mut again = server
        .connect(&registration(&bundle))
        .await
        .expect("connect");
    assert!(matches!(
        again.hello("").await,
        ControlMsg::Welcome {
            registration: true,
            ..
        }
    ));
    server.shutdown().await;
}

/// `PROTOCOL.md` §1/§2.2: the CRL follows `Welcome`, and a revoke puts the
/// certificate on a new one that reaches everybody online at once.
#[tokio::test]
async fn a_revoke_updates_the_crl_live() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    alice.expect_silence().await;
    let first = alice.crl.clone().expect("CrlUpdate after Welcome");
    let ca = server.ca();
    crl_serials(&first, &ca); // signed by the CA

    let bob = server.enroll("bob").await;
    server
        .handle
        .admin(AdminRequest::Revoke { user: "bob".into() })
        .await;
    alice.expect_silence().await;
    let updated = alice.crl.clone().expect("a new CRL was broadcast");
    assert_ne!(updated, first);
    assert!(crl_serials(&updated, &ca).contains(&serial_of(&bob.cert)));

    // Refused in the handshake, without a restart.
    assert_eq!(
        server.rejection(&bob).await,
        Some(common::TLS_CERTIFICATE_REVOKED)
    );
    server.shutdown().await;
}

/// `rotate-server-cert` takes effect without a restart, and the old leaf is
/// on the CRL clients get.
#[tokio::test]
async fn rotating_the_server_certificate_revokes_the_old_one_live() {
    let server = TestServer::start().await;
    let alice = server.enroll("alice").await;
    let before = server.connect(&alice).await.expect("connect");
    let old_leaf = server_leaf(&before.conn);
    drop(before);

    let response = server.handle.admin(AdminRequest::RotateServerCert).await;
    assert!(matches!(response, AdminResponse::Ok), "{response:?}");

    let mut after = server
        .connect(&alice)
        .await
        .expect("connect after rotation");
    let new_leaf = server_leaf(&after.conn);
    assert_ne!(
        new_leaf, old_leaf,
        "the new leaf is served without a restart"
    );
    after.hello("alice").await;
    after.expect_silence().await;
    let crl = after.crl.clone().expect("CrlUpdate after Welcome");
    let serials = crl_serials(&crl, &server.ca());
    assert!(serials.contains(&serial_of(&old_leaf)));
    assert!(!serials.contains(&serial_of(&new_leaf)));
    server.shutdown().await;
}

/// A rotation that fails — here the new certificate cannot be written —
/// leaves the live leaf in use and off the CRL. Revoking it first, as
/// before, locked every client out until a later rotation succeeded.
#[tokio::test]
async fn a_failed_rotation_leaves_the_live_server_certificate_alone() {
    let server = TestServer::start().await;
    let alice = server.enroll("alice").await;
    let live = server_leaf(&server.connect(&alice).await.expect("connect").conn);

    // A directory where the atomic writes put their temp files: whichever
    // of the pair is written first, the write fails.
    let dir = server.dir.path();
    for tmp in [".server.crt.tmp", ".server.key.tmp"] {
        std::fs::create_dir(dir.join(tmp)).expect("block the write");
    }
    let response = server.handle.admin(AdminRequest::RotateServerCert).await;
    for tmp in [".server.crt.tmp", ".server.key.tmp"] {
        std::fs::remove_dir(dir.join(tmp)).expect("unblock");
    }
    assert!(
        matches!(response, AdminResponse::Error { .. }),
        "{response:?}"
    );
    // Whatever rebuilds the CRL next — a revoke, a registration, a restart.
    icu_server::endpoint::reload_tls(server.handle.server())
        .await
        .expect("reload");

    let mut client = server.connect(&alice).await.expect("connect");
    assert_eq!(server_leaf(&client.conn), live, "still the same leaf");
    client.hello("alice").await;
    client.expect_silence().await;
    let serials = crl_serials(client.crl.as_ref().expect("CrlUpdate"), &server.ca());
    assert!(
        !serials.contains(&serial_of(&live)),
        "the live leaf must not be on the CRL"
    );

    // Once writing works again, the rotation goes through.
    let response = server.handle.admin(AdminRequest::RotateServerCert).await;
    assert!(matches!(response, AdminResponse::Ok), "{response:?}");
    server.shutdown().await;
}

/// A crash between the two writes of a rotation leaves a server key that
/// does not match the certificate. The next start issues a new leaf instead
/// of failing forever, and the one it found is on the CRL.
#[tokio::test]
async fn a_server_key_that_does_not_match_is_repaired_at_startup() {
    let server = TestServer::start().await;
    let alice = server.enroll("alice").await;
    let old = server_leaf(&server.connect(&alice).await.expect("connect").conn);
    let dir = server.dir.path().to_path_buf();
    let TestServer { dir: tmp, handle } = server;
    handle.shutdown().await;
    icu_server::store::write_private(
        &dir.join("server.key"),
        &rcgen::KeyPair::generate().unwrap().serialize_pem(),
    )
    .unwrap();
    let server = TestServer {
        handle: icu_server::run(icu_server::config::Config::for_test(&dir))
            .await
            .expect("the server starts"),
        dir: tmp,
    };

    let mut client = server.connect(&alice).await.expect("connect");
    let new = server_leaf(&client.conn);
    assert_ne!(new, old);
    client.hello("alice").await;
    client.expect_silence().await;
    let serials = crl_serials(client.crl.as_ref().expect("CrlUpdate"), &server.ca());
    assert!(
        serials.contains(&serial_of(&old)),
        "the replaced leaf is revoked"
    );
    assert!(!serials.contains(&serial_of(&new)));
    server.shutdown().await;
}

/// A server leaf close to expiry is rotated by the server itself, right at
/// startup: served live, the old one on the CRL, and `status` reports it.
#[tokio::test]
async fn an_expiring_server_certificate_is_rotated_automatically() {
    // One day of validity is below the rotation threshold from the start.
    let server = TestServer::start_with(|c| c.tls.server_validity_days = 1).await;
    let alice = server.enroll("alice").await;
    let mut rotated = None;
    for _ in 0..100 {
        let mut client = server.connect(&alice).await.expect("connect");
        let leaf = server_leaf(&client.conn);
        client.hello("alice").await;
        client.expect_silence().await;
        let serials = crl_serials(client.crl.as_ref().expect("CrlUpdate"), &server.ca());
        if !serials.is_empty() && !serials.contains(&serial_of(&leaf)) {
            rotated = Some(leaf);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        rotated.is_some(),
        "the old leaf is on the CRL, the new one served"
    );

    let AdminResponse::Status(status) = server.handle.admin(AdminRequest::Status).await else {
        panic!("status");
    };
    assert!(status["tls"]["server_cert_days_left"].as_i64().unwrap() <= 1);
    assert!(status["tls"]["ca_days_left"].as_i64().unwrap() > 3000);
    server.shutdown().await;
}

/// The server leaf a test connection saw.
fn server_leaf(conn: &quinn::Connection) -> Vec<u8> {
    conn.peer_identity()
        .and_then(|any| {
            any.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
                .ok()
        })
        .and_then(|certs| certs.first().map(|c| c.to_vec()))
        .expect("server certificate")
}

/// A name stored before the strict rules (`PROTOCOL.md` §2.1, 2026-09-24)
/// is carried into new certificates as it is: catch-up and plain renewal
/// both work, or its owner would be locked out once the certificate expires.
#[tokio::test]
async fn a_name_from_before_the_strict_rules_still_renews() {
    let server = TestServer::start_with(|c| c.tls.client_validity_days = 20).await;
    let alice = server.enroll("alice").await;
    let TestServer { dir, handle } = server;
    handle.shutdown().await;

    // Mixed script: valid then, invalid now.
    let legacy = "Ωmega";
    assert!(icu_proto::validate_name(legacy).is_err());
    let users = dir.path().join("users.toml");
    let text = std::fs::read_to_string(&users).unwrap();
    std::fs::write(
        &users,
        text.replace("name = \"alice\"", &format!("name = \"{legacy}\"")),
    )
    .unwrap();
    let mut config = icu_server::config::Config::for_test(dir.path());
    config.tls.client_validity_days = 20;
    let server = TestServer {
        handle: icu_server::run(config).await.expect("restarts"),
        dir,
    };

    let renew = || {
        let (key, csr) = common::csr_with_cn(legacy);
        (key, ControlMsg::RenewRequest { csr_der: csr })
    };
    let mut client = server.connect(&alice).await.expect("connect");
    let welcome = client.hello("alice").await;
    assert!(matches!(&welcome, ControlMsg::Welcome { you, .. } if you.name == legacy));

    // Catch-up: the certificate still says "alice".
    let (key, request) = renew();
    client.send_msg(&request).await;
    let answer = client
        .recv_matching(|m| {
            matches!(
                m,
                ControlMsg::RenewResponse { .. } | ControlMsg::Error { .. }
            )
        })
        .await;
    let Some(ControlMsg::RenewResponse { leaf_der }) = answer else {
        panic!("catch-up with the stored name must work, got {answer:?}");
    };
    let caught_up = common::Identity {
        cert: rustls::pki_types::CertificateDer::from(leaf_der),
        key_pem: key.serialize_pem(),
        key_der: key.serialize_der(),
        ca: server.ca(),
    };

    // Plain renewal with a certificate that already carries the name.
    let mut client = server.connect(&caught_up).await.expect("connect");
    client.hello(legacy).await;
    let (_, request) = renew();
    client.send_msg(&request).await;
    let answer = client
        .recv_matching(|m| {
            matches!(
                m,
                ControlMsg::RenewResponse { .. } | ControlMsg::Error { .. }
            )
        })
        .await;
    assert!(
        matches!(answer, Some(ControlMsg::RenewResponse { .. })),
        "plain renewal with the stored name must work, got {answer:?}"
    );

    // A rename to a name that breaks today's rules is still refused.
    let (_, csr) = common::csr_with_cn("Chri\u{0455}");
    client
        .send_msg(&ControlMsg::RenewRequest { csr_der: csr })
        .await;
    let answer = client
        .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
        .await;
    assert!(
        matches!(answer, Some(ControlMsg::Error { code, .. }) if code == ec::RENEW_REJECTED),
        "{answer:?}"
    );
    server.shutdown().await;
}

/// Consuming the bundle at the first real login is a write; when it fails,
/// the login still goes through instead of ending in a fatal code, and the
/// bundle stays valid until a later login consumes it.
#[tokio::test]
async fn a_failed_store_write_does_not_lock_out_the_first_login() {
    use std::os::unix::fs::PermissionsExt;

    let server = TestServer::start().await;
    let _alice = server.client("alice").await;
    let bundle = server.invite("").await;
    let bob = server
        .register_unconfirmed(&bundle, "bob")
        .await
        .expect("registers");

    let dir = server.dir.path().to_path_buf();
    let set_mode = |mode| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode));
    set_mode(0o500).expect("chmod");
    if std::fs::write(dir.join("probe"), b"").is_ok() {
        // Root ignores the mode; nothing to test then.
        set_mode(0o700).expect("chmod back");
        server.shutdown().await;
        return;
    }
    let mut client = server.connect(&bob).await.expect("connect");
    let welcome = client.hello("bob").await;
    set_mode(0o700).expect("chmod back");
    match welcome {
        ControlMsg::Welcome {
            you, registration, ..
        } => {
            assert_eq!(you.name, "bob");
            assert!(!registration);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}

/// A registration session answers `Ping`, but not at any rate.
#[tokio::test]
async fn registration_pings_are_rate_limited() {
    let server = TestServer::start().await;
    let _alice = server.client("alice").await;
    let bundle = server.invite("").await;
    let mut client = server
        .connect(&common::identity_from_bundle(&bundle))
        .await
        .expect("connect");
    assert!(matches!(
        client.hello("").await,
        ControlMsg::Welcome {
            registration: true,
            ..
        }
    ));
    const SENT: u64 = 200;
    for nonce in 0..SENT {
        client.send_msg(&ControlMsg::Ping { nonce }).await;
    }
    let mut pongs = 0;
    while let Some(msg) = client
        .recv_timeout(std::time::Duration::from_millis(300))
        .await
    {
        assert!(matches!(msg, ControlMsg::Pong { .. }), "{msg:?}");
        pongs += 1;
    }
    assert!(pongs > 0, "some Pings are answered");
    assert!(pongs < SENT, "{pongs} of {SENT} Pings answered");
    server.shutdown().await;
}
