//! Admin-issued registration bundles and the first admin
//! (`PROTOCOL.md` §2.3, `Server/IMPLEMENTATION.md` §6.3, §6.4
//! `CreateInviteRequest`).

mod common;

use common::{TestServer, SOON};
use icu_proto::{error as ec, ControlMsg};
use x509_parser::prelude::FromDer;

fn common_name(der: &[u8]) -> String {
    x509_parser::certificate::X509Certificate::from_der(der)
        .unwrap()
        .1
        .subject()
        .iter_common_name()
        .next()
        .and_then(|a| a.as_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// Registers and connects the first user, who is the admin — the starting
/// point of most tests in this file.
async fn admin_client(server: &TestServer) -> common::Client {
    server.client("admin").await
}

async fn mint(client: &mut common::Client, name: &str, ttl_s: u64) -> (String, String) {
    client
        .send_msg(&ControlMsg::CreateInviteRequest {
            name: name.to_string(),
            ttl_s,
        })
        .await;
    match client
        .recv_matching(|m| matches!(m, ControlMsg::CreateInviteResponse { .. }))
        .await
        .expect("CreateInviteResponse")
    {
        ControlMsg::CreateInviteResponse { bundle, expires } => (bundle, expires),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn the_first_registration_is_the_admin_and_the_second_is_not() {
    let server = TestServer::start().await;
    let mut first = server.connect(&server.enroll("first").await).await.unwrap();
    let mut second = server
        .connect(&server.enroll("second").await)
        .await
        .unwrap();
    assert!(matches!(
        first.hello("first").await,
        ControlMsg::Welcome { is_admin: true, .. }
    ));
    assert!(matches!(
        second.hello("second").await,
        ControlMsg::Welcome {
            is_admin: false,
            ..
        }
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn a_bundle_from_an_admin_registers_a_client_with_its_own_name() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    // The label is only the placeholder name; the name in the certificate is
    // the one the invited person picks (`PROTOCOL.md` §2.3).
    let (bundle, _expires) = mint(&mut admin, "invited-person", 0).await;
    let registration = common::identity_from_bundle(&bundle);
    assert_eq!(common_name(&registration.cert), "invited-person");

    let identity = server
        .register(&bundle, "whatever-the-client-picks")
        .await
        .expect("bundle registers");
    assert_eq!(
        common_name(&identity.cert),
        "whatever-the-client-picks",
        "the CN comes from the CSR, the label binds nothing"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_bundle_without_a_label_gets_a_placeholder() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (bundle, _expires) = mint(&mut admin, "", 0).await;
    let placeholder = common_name(&common::identity_from_bundle(&bundle).cert);
    assert!(placeholder.starts_with("neu-"), "{placeholder}");

    let identity = server.register(&bundle, "carol").await.expect("registers");
    assert_eq!(common_name(&identity.cert), "carol");
    server.shutdown().await;
}

#[tokio::test]
async fn a_taken_label_is_refused_non_fatally() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;
    admin
        .send_msg(&ControlMsg::CreateInviteRequest {
            name: "ADMIN".into(),
            ttl_s: 0,
        })
        .await;
    match admin
        .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
        .await
    {
        Some(ControlMsg::Error { code, .. }) => assert_eq!(code, ec::NAME_TAKEN),
        other => panic!("expected NAME_TAKEN, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn a_non_admin_cannot_mint_a_bundle_and_the_connection_stays_open() {
    let server = TestServer::start().await;
    // The first registration becomes admin; register a second, non-admin user.
    server.enroll("admin").await;
    let mut member = server.client("member").await;

    member
        .send_msg(&ControlMsg::CreateInviteRequest {
            name: "someone".into(),
            ttl_s: 0,
        })
        .await;
    match member.recv_msg().await {
        ControlMsg::Error { code, .. } => assert_eq!(code, ec::NOT_ADMIN),
        other => panic!("expected Error(NOT_ADMIN), got {other:?}"),
    }

    // Non-fatal: the connection must still answer a Ping.
    member.send_msg(&ControlMsg::Ping { nonce: 42 }).await;
    match member.recv_timeout(SOON).await {
        Some(ControlMsg::Pong { nonce, .. }) => assert_eq!(nonce, 42),
        other => panic!("expected Pong after NOT_ADMIN, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn a_requested_ttl_bounds_the_bundle() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (bundle, _expires) = mint(&mut admin, "short", 3600).await;
    let cert = common::identity_from_bundle(&bundle).cert;
    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(&cert).unwrap();
    let left =
        cert.validity().not_after.timestamp() - time::OffsetDateTime::now_utc().unix_timestamp();
    assert!((3500..=3700).contains(&left), "{left}");
    server.shutdown().await;
}

async fn user_list(client: &mut common::Client) -> Vec<icu_proto::UserSummary> {
    client.send_msg(&ControlMsg::UserListRequest).await;
    match client
        .recv_matching(|m| matches!(m, ControlMsg::UserListResponse { .. }))
        .await
    {
        Some(ControlMsg::UserListResponse { users }) => users,
        other => panic!("expected UserListResponse, got {other:?}"),
    }
}

#[tokio::test]
async fn an_invited_user_is_pending_until_registered() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (bundle, _expires) = mint(&mut admin, "guest", 0).await;
    let pending = |users: &[icu_proto::UserSummary], name: &str| {
        users.iter().find(|u| u.name == name).map(|u| u.pending)
    };
    let users = user_list(&mut admin).await;
    assert_eq!(pending(&users, "guest"), Some(true));
    assert_eq!(pending(&users, "admin"), Some(false));

    server.register(&bundle, "guest").await.expect("registers");
    let users = user_list(&mut admin).await;
    assert_eq!(pending(&users, "guest"), Some(false));
    server.shutdown().await;
}
