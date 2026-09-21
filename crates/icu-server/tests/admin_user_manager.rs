//! The in-band admin User Manager path (`PROTOCOL.md` §2.2, variants 19-28;
//! `Server/IMPLEMENTATION.md` §6.4 rows `AdminSetRole`/`AdminSetDisabled`/
//! `AdminRevokeUser`/`UserListRequest`). The socket/CLI path is covered by
//! `tests/admin.rs`; this file exercises the same store methods reached
//! directly over the authenticated `icu/1` connection.

mod common;

use common::TestServer;
use icu_proto::{error as ec, ControlMsg};

#[tokio::test]
async fn admin_set_role_over_the_wire_updates_and_pushes() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let mut bob = server.client("bob").await;

    alice
        .send_msg(&ControlMsg::AdminSetRole {
            user_id: bob.user_id,
            is_admin: true,
        })
        .await;
    let ok = alice
        .recv_matching(|m| matches!(m, ControlMsg::AdminSetRoleOk { .. }))
        .await;
    assert!(
        matches!(ok, Some(ControlMsg::AdminSetRoleOk { is_admin: true, .. })),
        "{ok:?}"
    );

    let pushed = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserAdminChanged { .. }))
        .await;
    assert!(
        matches!(
            pushed,
            Some(ControlMsg::UserAdminChanged { is_admin: true })
        ),
        "{pushed:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn admin_set_role_from_a_non_admin_is_rejected() {
    let server = TestServer::start().await;
    let _alice = server.client("alice").await; // sole bootstrap admin
    let mut bob = server.client("bob").await;

    bob.send_msg(&ControlMsg::AdminSetRole {
        user_id: bob.user_id,
        is_admin: true,
    })
    .await;
    let err = bob
        .recv_matching(|m| matches!(m, ControlMsg::AdminActionError { .. }))
        .await;
    assert!(
        matches!(
            err,
            Some(ControlMsg::AdminActionError {
                code: ec::NOT_ADMIN,
                ..
            })
        ),
        "{err:?}"
    );

    // Non fatal: the connection stays open.
    bob.send_msg(&ControlMsg::Ping { nonce: 7 }).await;
    let pong = bob.recv_msg().await;
    assert!(matches!(pong, ControlMsg::Pong { nonce: 7, .. }));
    server.shutdown().await;
}

#[tokio::test]
async fn admin_revoke_user_over_the_wire_kicks_the_target() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let bob = server.client("bob").await;

    alice
        .send_msg(&ControlMsg::AdminRevokeUser {
            user_id: bob.user_id,
        })
        .await;
    let ok = alice
        .recv_matching(|m| matches!(m, ControlMsg::AdminRevokeUserOk { .. }))
        .await;
    assert!(
        matches!(ok, Some(ControlMsg::AdminRevokeUserOk { closed: 1, .. })),
        "{ok:?}"
    );
    assert_eq!(bob.closed_code().await, Some(ec::KICKED));
    server.shutdown().await;
}

#[tokio::test]
async fn admin_set_disabled_over_the_wire_kicks_with_auth_disabled() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let bob = server.client("bob").await;

    alice
        .send_msg(&ControlMsg::AdminSetDisabled {
            user_id: bob.user_id,
            disabled: true,
        })
        .await;
    let ok = alice
        .recv_matching(|m| matches!(m, ControlMsg::AdminSetDisabledOk { .. }))
        .await;
    assert!(
        matches!(
            ok,
            Some(ControlMsg::AdminSetDisabledOk { disabled: true, .. })
        ),
        "{ok:?}"
    );
    assert_eq!(bob.closed_code().await, Some(ec::AUTH_DISABLED));
    server.shutdown().await;
}

#[tokio::test]
async fn user_list_request_returns_all_registered_users() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin, online
    let _bob = server.client("bob").await; // online
    server.enroll("carol").await; // never connects: offline

    alice.send_msg(&ControlMsg::UserListRequest).await;
    let response = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserListResponse { .. }))
        .await;
    let ControlMsg::UserListResponse { users } = response.expect("UserListResponse") else {
        unreachable!()
    };
    assert_eq!(users.len(), 3);

    let alice_row = users.iter().find(|u| u.name == "alice").unwrap();
    assert!(alice_row.is_admin);
    assert!(alice_row.online);
    assert!(!alice_row.disabled);
    assert!(!alice_row.revoked);

    let bob_row = users.iter().find(|u| u.name == "bob").unwrap();
    assert!(!bob_row.is_admin);
    assert!(bob_row.online);

    let carol_row = users.iter().find(|u| u.name == "carol").unwrap();
    assert!(!carol_row.online);
    server.shutdown().await;
}

#[tokio::test]
async fn last_admin_guard_applies_to_the_in_band_path() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin

    alice
        .send_msg(&ControlMsg::AdminSetRole {
            user_id: alice.user_id,
            is_admin: false,
        })
        .await;
    let err = alice
        .recv_matching(|m| matches!(m, ControlMsg::AdminActionError { .. }))
        .await;
    assert!(
        matches!(
            err,
            Some(ControlMsg::AdminActionError {
                code: ec::LAST_ADMIN,
                ..
            })
        ),
        "{err:?}"
    );
    server.shutdown().await;
}
