//! First-enrollment-wins-admin bootstrap (`PROTOCOL.md` §2.3,
//! `Server/IMPLEMENTATION.md` §6.3).

mod common;

use common::TestServer;
use icu_proto::error as ec;

#[tokio::test]
async fn empty_token_on_a_fresh_store_enrolls_as_admin() {
    let server = TestServer::start().await;
    let identity = server
        .enroll_with_token("", "admin")
        .await
        .expect("bootstrap enrolls");
    let mut client = server.connect(&identity).await.expect("connect");
    let welcome = client.hello("admin").await;
    match welcome {
        icu_proto::ControlMsg::Welcome { is_admin, .. } => {
            assert!(is_admin, "the first enrollment must be granted admin");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn a_second_empty_token_after_bootstrap_is_closed() {
    let server = TestServer::start().await;
    server
        .enroll_with_token("", "admin")
        .await
        .expect("first bootstrap enrolls");
    let err = server
        .enroll_with_token("", "someone-else")
        .await
        .expect_err("a second bootstrap must be rejected");
    assert_eq!(err.code(), Some(ec::ENROLL_BOOTSTRAP_CLOSED));
    server.shutdown().await;
}

#[tokio::test]
async fn concurrent_bootstrap_attempts_only_one_wins() {
    let server = TestServer::start().await;
    let (a, b) = tokio::join!(
        server.enroll_with_token("", "alice"),
        server.enroll_with_token("", "bob"),
    );
    let successes = [&a, &b].iter().filter(|r| r.is_ok()).count();
    let closed = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Err(e) if e.code() == Some(ec::ENROLL_BOOTSTRAP_CLOSED)))
        .count();
    assert_eq!(successes, 1, "exactly one concurrent bootstrap must win");
    assert_eq!(closed, 1, "the other must see the store already non-empty");
    server.shutdown().await;
}

#[tokio::test]
async fn a_named_invite_can_still_win_bootstrap_admin() {
    // Clarified requirement: admin is granted to whichever enrollment is
    // first against an empty store, not only the no-token path.
    let server = TestServer::start().await;
    let identity = server.enroll("alice").await;
    let mut client = server.connect(&identity).await.expect("connect");
    let welcome = client.hello("alice").await;
    match welcome {
        icu_proto::ControlMsg::Welcome { is_admin, .. } => {
            assert!(
                is_admin,
                "a CLI-issued invite redeemed first must also grant admin"
            );
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}
