//! Admin-minted, self-contained invite blobs (`PROTOCOL.md` §2.3.1,
//! `Server/IMPLEMENTATION.md` §6.4 `CreateInviteRequest`).

mod common;

use std::time::Duration;

use common::{TestServer, SOON};
use icu_proto::{error as ec, invite_blob::InviteBlob, ControlMsg};
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

/// Mints an invite blob as the given already-connected admin client.
/// Enrolls and connects the bootstrap admin, the starting point of every test
/// in this file.
async fn admin_client(server: &TestServer) -> common::Client {
    let identity = server.enroll_with_token("", "admin").await.unwrap();
    let mut admin = server.connect(&identity).await.expect("connect");
    admin.hello("admin").await;
    admin
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
        ControlMsg::CreateInviteResponse { blob, expires } => (blob, expires),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn a_blob_from_an_admin_enrolls_a_second_client_with_its_own_name() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    // The label on the invite is a prefill for the invited person's dialog;
    // the name in the certificate is the one they pick (`PROTOCOL.md` §2.3).
    let (blob, _expires) = mint(&mut admin, "invited-person", 0).await;
    let body = blob
        .strip_prefix(icu_proto::invite_blob::ARMOR_PREFIX)
        .expect("armor prefix");

    let identity = server
        .enroll_with_token(body, "whatever-the-client-picks")
        .await
        .expect("blob enrolls");
    assert_eq!(
        common_name(&identity.cert),
        "whatever-the-client-picks",
        "the CN comes from the CSR, the invite label binds nothing"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn an_invite_without_a_label_still_enrolls() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (blob, _expires) = mint(&mut admin, "", 0).await;
    let body = blob
        .strip_prefix(icu_proto::invite_blob::ARMOR_PREFIX)
        .expect("armor prefix");

    let identity = server
        .enroll_with_token(body, "carol")
        .await
        .expect("blob enrolls");
    assert_eq!(common_name(&identity.cert), "carol");
    server.shutdown().await;
}

#[tokio::test]
async fn tampering_with_the_sealed_token_is_rejected() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (blob, _) = mint(&mut admin, "invited-person", 0).await;
    let mut parsed = InviteBlob::decode(&blob).expect("decode");
    parsed.sealed[0] ^= 0xFF;
    let body = parsed.body_base64();

    let err = server
        .enroll_with_token(&body, "mallory")
        .await
        .expect_err("a tampered seal must not open");
    assert_eq!(err.code(), Some(ec::ENROLL_BAD_TOKEN));
    server.shutdown().await;
}

#[tokio::test]
async fn tampering_with_the_nonce_is_rejected() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (blob, _) = mint(&mut admin, "invited-person", 0).await;
    let mut parsed = InviteBlob::decode(&blob).expect("decode");
    parsed.nonce[0] ^= 0xFF;
    let body = parsed.body_base64();

    let err = server
        .enroll_with_token(&body, "mallory")
        .await
        .expect_err("a tampered nonce must not open");
    assert_eq!(err.code(), Some(ec::ENROLL_BAD_TOKEN));
    server.shutdown().await;
}

#[tokio::test]
async fn an_expired_blob_is_rejected() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (blob, _) = mint(&mut admin, "invited-person", 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let body = blob
        .strip_prefix(icu_proto::invite_blob::ARMOR_PREFIX)
        .unwrap();

    let err = server
        .enroll_with_token(body, "too-late")
        .await
        .expect_err("an expired invite must be rejected");
    assert_eq!(err.code(), Some(ec::ENROLL_BAD_TOKEN));
    server.shutdown().await;
}

#[tokio::test]
async fn a_non_admin_cannot_mint_an_invite_and_the_connection_stays_open() {
    let server = TestServer::start().await;
    // The first enrollment becomes admin; enroll a second, non-admin user.
    server.enroll_with_token("", "admin").await.unwrap();
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
