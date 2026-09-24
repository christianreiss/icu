//! Certificate lifetimes. An expired client certificate renews itself on a
//! renew-only session (`PROTOCOL.md` §2.3.3), however long ago it expired,
//! without anyone trusting anything new: the key is proven in the handshake,
//! the record in the store admits it, the CA still vouches for the server.
//! And no re-issue outside the renewal window extends a certificate.

mod common;

use std::time::Duration;

use common::{renewal_csr, Identity, TestServer};
use icu_proto::{error as ec, ControlMsg};
use icu_server::{AdminRequest, AdminResponse};
use rustls::pki_types::CertificateDer;

/// Enrolls `name` on a server that issues certificates expiring the moment
/// they are made, waits until it has, and brings the server back with the
/// normal lifetime. The expired identity and the running server.
async fn expired_user(name: &str) -> (TestServer, Identity) {
    let server = TestServer::start_with(|c| c.tls.client_validity_days = 0).await;
    // The first user becomes admin; the one under test must not be the last.
    let _admin = server.enroll("admin").await;
    let identity = server.enroll(name).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let server = server.restart_with(|_| {}).await;
    (server, identity)
}

fn days_left(der: &[u8]) -> i64 {
    let (_, cert) = x509_parser::parse_x509_certificate(der).expect("leaf parses");
    (cert.validity().not_after.timestamp() - time_now()) / 86_400
}

fn time_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test]
async fn an_expired_certificate_renews_and_the_new_one_logs_in() {
    let (server, alice) = expired_user("alice").await;
    assert!(days_left(&alice.cert) < 0 || days_left(&alice.cert) == 0);

    let mut client = server.connect(&alice).await.expect("handshake passes");
    let welcome = client.hello("alice").await;
    let ControlMsg::Welcome { users, you, .. } = welcome else {
        panic!("expected Welcome, got {welcome:?}");
    };
    assert!(users.is_empty(), "a renew-only session sees nobody");
    assert_eq!(you.name, "alice");
    assert!(matches!(
        client.recv_msg().await,
        ControlMsg::Error { code: ec::RENEW_REQUIRED, .. }
    ));

    // Nothing but renewing.
    client
        .send_msg(&ControlMsg::SetState {
            flags: icu_proto::UserFlags::NONE,
        })
        .await;
    assert!(matches!(
        client.recv_msg().await,
        ControlMsg::Error { code: ec::RENEW_REQUIRED, .. }
    ));

    client
        .send_msg(&ControlMsg::RenewRequest {
            csr_der: renewal_csr(&alice, "alice"),
        })
        .await;
    let ControlMsg::RenewResponse { leaf_der } = client.recv_msg().await else {
        panic!("expected RenewResponse");
    };
    assert!(days_left(&leaf_der) > 360, "a full lifetime again");
    client.conn.close(quinn::VarInt::from_u32(0), b"");

    let renewed = Identity {
        cert: CertificateDer::from(leaf_der),
        ..alice
    };
    let mut client = server.connect(&renewed).await.expect("connect");
    let welcome = client.hello("alice").await;
    assert!(matches!(welcome, ControlMsg::Welcome { .. }));
    assert!(
        client
            .recv_timeout(Duration::from_millis(300))
            .await
            .is_none_or(|m| !matches!(m, ControlMsg::Error { .. })),
        "a normal session"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn an_expired_certificate_cannot_rename_or_renew_twice_in_an_hour() {
    let (server, alice) = expired_user("alice").await;
    let mut client = server.connect(&alice).await.expect("connect");
    client.hello("alice").await;
    client.recv_msg().await; // RENEW_REQUIRED

    client
        .send_msg(&ControlMsg::RenewRequest {
            csr_der: renewal_csr(&alice, "mallory"),
        })
        .await;
    assert!(matches!(
        client.recv_msg().await,
        ControlMsg::Error { code: ec::RENEW_REJECTED, .. }
    ));
    client
        .send_msg(&ControlMsg::RenewRequest {
            csr_der: renewal_csr(&alice, "alice"),
        })
        .await;
    assert!(matches!(client.recv_msg().await, ControlMsg::RenewResponse { .. }));
    client.conn.close(quinn::VarInt::from_u32(0), b"");

    let mut again = server.connect(&alice).await.expect("connect");
    again.hello("alice").await;
    again.recv_msg().await; // RENEW_REQUIRED
    again
        .send_msg(&ControlMsg::RenewRequest {
            csr_der: renewal_csr(&alice, "alice"),
        })
        .await;
    assert!(matches!(
        again.recv_msg().await,
        ControlMsg::Error { code: ec::RENEW_REJECTED, .. }
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn a_revoked_expired_certificate_stays_out() {
    let (server, alice) = expired_user("alice").await;
    assert!(matches!(
        server
            .handle
            .admin(AdminRequest::Revoke {
                user: "alice".into()
            })
            .await,
        AdminResponse::Revoke { .. }
    ));
    let code = server.rejection(&alice).await;
    assert!(
        code == Some(common::TLS_CERTIFICATE_REVOKED) || code == Some(ec::AUTH_REVOKED),
        "got {code:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_disabled_user_cannot_renew_an_expired_certificate() {
    let (server, alice) = expired_user("alice").await;
    assert!(matches!(
        server
            .handle
            .admin(AdminRequest::Disable {
                user: "alice".into()
            })
            .await,
        AdminResponse::Disable { .. }
    ));
    assert_eq!(server.rejection(&alice).await, Some(ec::AUTH_DISABLED));
    server.shutdown().await;
}

#[tokio::test]
async fn an_expired_certificate_from_another_ca_fails_the_handshake() {
    let (other, stranger) = expired_user("alice").await;
    other.shutdown().await;
    let server = TestServer::start().await;
    let _alice = server.enroll("alice").await;
    let code = server.rejection(&stranger).await;
    assert!(
        code.is_some_and(|c| c >= 0x100),
        "a TLS alert, not a session: {code:?}"
    );
    server.shutdown().await;
}

/// Renames `identity` to `name` on a fresh session and returns the new leaf.
async fn rename(server: &TestServer, identity: &Identity, old: &str, name: &str) -> Vec<u8> {
    let mut client = server.connect(identity).await.expect("connect");
    client.hello(old).await;
    client
        .send_msg(&ControlMsg::RenewRequest {
            csr_der: renewal_csr(identity, name),
        })
        .await;
    let answer = client
        .recv_matching(|m| matches!(m, ControlMsg::RenewResponse { .. } | ControlMsg::Error { .. }))
        .await;
    let Some(ControlMsg::RenewResponse { leaf_der }) = answer else {
        panic!("rename must work, got {answer:?}");
    };
    leaf_der
}

fn validity(der: &[u8]) -> (i64, i64) {
    let (_, cert) = x509_parser::parse_x509_certificate(der).expect("leaf parses");
    (
        cert.validity().not_before.timestamp(),
        cert.validity().not_after.timestamp(),
    )
}

/// A re-issue outside the renewal window never extends a certificate: a
/// rename gets exactly the old validity (`PROTOCOL.md` §2.2 Nr. 9).
#[tokio::test]
async fn a_rename_outside_the_window_keeps_the_validity() {
    let server = TestServer::start().await;
    let _admin = server.enroll("admin").await;
    let alice = server.enroll("alice").await;
    let leaf = rename(&server, &alice, "alice", "alicia").await;
    assert_eq!(validity(&leaf), validity(&alice.cert));
    server.shutdown().await;
}

/// Inside the window a rename is a renewal as well: a fresh lifetime.
#[tokio::test]
async fn a_rename_inside_the_window_gets_a_fresh_lifetime() {
    let server = TestServer::start_with(|c| c.tls.client_validity_days = 20).await;
    let _admin = server.enroll("admin").await;
    let alice = server.enroll("alice").await;
    let server = server.restart_with(|_| {}).await;
    let leaf = rename(&server, &alice, "alice", "alicia").await;
    assert!(days_left(&leaf) > 360, "fresh lifetime, got {}", days_left(&leaf));
    server.shutdown().await;
}
