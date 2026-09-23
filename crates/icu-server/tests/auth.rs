//! Enrollment and authentication (`Server/IMPLEMENTATION.md` §10, rows
//! "Enrollment → Connect → Hello/Welcome", "Connect ohne Cert",
//! "unbekanntes Cert", "Revoke während Verbindung", "RenewRequest").

mod common;

use common::{EnrollFail, TestServer, SOON};
use icu_proto::{error as ec, ControlMsg};
use icu_server::{AdminRequest, AdminResponse};

#[tokio::test]
async fn enrollment_then_connect_then_hello_welcome() {
    let server = TestServer::start().await;
    let identity = server.enroll("alice").await;
    assert_eq!(identity.ca, server.ca(), "the CA travels with the response");

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
    let (leaf, _) = ca.sign_client_csr(&csr, 365).unwrap();

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
    let client = server.connect(&identity).await.expect("connect");
    assert_eq!(client.closed_code().await, Some(ec::AUTH_REVOKED));
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
async fn an_unknown_token_is_enroll_bad_token() {
    let server = TestServer::start().await;
    let result = server
        .enroll_with_token("AAAAAAAAAAAAAAAAAAAAAA", "mallory")
        .await;
    match result {
        Err(fail) => assert_eq!(fail.code(), Some(ec::ENROLL_BAD_TOKEN), "{fail:?}"),
        Ok(_) => panic!("an unknown token must not enroll"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn a_token_works_exactly_once() {
    let server = TestServer::start().await;
    let token = server.invite("bob").await;
    assert!(server.enroll_with_token(&token, "bob").await.is_ok());
    match server.enroll_with_token(&token, "bob").await {
        Err(fail) => assert_eq!(fail.code(), Some(ec::ENROLL_BAD_TOKEN), "{fail:?}"),
        Ok(_) => panic!("a token must not enroll twice"),
    }
    server.shutdown().await;
}

/// §10: a taken name is refused outright — no silent `-2` suffix any more —
/// and the invite survives the attempt, so the user can pick another name.
#[tokio::test]
async fn a_taken_name_at_enrollment_leaves_the_token_reusable() {
    let server = TestServer::start().await;
    server.enroll("alice").await;

    let token = server.invite("").await;
    let err = server
        .enroll_with_token(&token, "ALICE")
        .await
        .expect_err("a taken name is refused");
    assert_eq!(err.code(), Some(ec::NAME_TAKEN), "{err:?}");

    // Same token, another name: this is what makes the refusal recoverable.
    let carol = server
        .enroll_with_token(&token, "carol")
        .await
        .expect("the token is still unused");
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

#[tokio::test]
async fn renewal_issues_a_second_certificate_and_both_connect() {
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

    // The new certificate connects...
    let mut with_new = server.connect(&renewed).await.expect("connect new");
    let welcome = with_new.hello("alice").await;
    match &welcome {
        ControlMsg::Welcome { you, .. } => {
            assert_eq!(you.user_id, 1, "the user_id survives the renewal");
            assert_eq!(you.name, "alice");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    // ...and the old one still does, until it expires.
    let mut with_old = server.connect(&old).await.expect("connect old");
    let welcome = with_old.hello("alice").await;
    assert!(matches!(welcome, ControlMsg::Welcome { .. }));

    // A second renewal on the same day is rejected.
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
async fn the_enrollment_error_path_sends_a_message_before_closing() {
    // `PROTOCOL.md` §2.3: Error first, then finish(), then close with the same
    // code — otherwise the dialog only shows a number.
    let server = TestServer::start().await;
    let fail = server
        .enroll_with_token("ZZZZZZZZZZZZZZZZZZZZZZ", "mallory")
        .await
        .expect_err("must fail");
    match fail {
        EnrollFail::Rejected { code, detail } => {
            assert_eq!(code, ec::ENROLL_BAD_TOKEN);
            assert!(!detail.is_empty(), "the detail text carries the reason");
        }
        other => panic!("expected a message, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn enrollment_is_rate_limited_per_ip() {
    let server = TestServer::start_with(|config| {
        config.limits.enroll_per_ip_per_min = 2;
    })
    .await;
    let mut codes = Vec::new();
    for i in 0..4 {
        let token = server.invite(&format!("user{i}")).await;
        codes.push(
            server
                .enroll_with_token(&token, &format!("user{i}"))
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
        "third enrollment from the same IP within a minute"
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
