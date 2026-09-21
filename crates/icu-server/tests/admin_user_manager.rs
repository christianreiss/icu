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

/// The live roster (`UserFlags::ADMIN`, not the admin-only `UserSummary`
/// list) shows the bootstrap admin's role to itself on `Welcome`, and to a
/// peer both in `Welcome.users` and live in `UserJoined` once they connect.
#[tokio::test]
async fn the_admin_flag_bit_rides_the_live_roster() {
    let server = TestServer::start().await;
    let alice_id = server.enroll("alice").await;
    let mut alice = server.connect(&alice_id).await.expect("connect");
    let welcome = alice.hello("alice").await; // sole bootstrap admin
    let ControlMsg::Welcome { you, .. } = welcome else {
        panic!("expected Welcome, got {welcome:?}");
    };
    assert!(you.flags.is_admin(), "the bootstrap admin sees its own bit");

    let bob_id = server.enroll("bob").await;
    let mut bob = server.connect(&bob_id).await.expect("connect");
    let bob_welcome = bob.hello("bob").await;
    let ControlMsg::Welcome { users, .. } = bob_welcome else {
        panic!("expected Welcome, got {bob_welcome:?}");
    };
    let alice_row = users.iter().find(|u| u.name == "alice").unwrap();
    assert!(
        alice_row.flags.is_admin(),
        "bob's own Welcome.users shows alice's badge"
    );

    let joined = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserJoined { .. }))
        .await;
    let Some(ControlMsg::UserJoined { user }) = joined else {
        panic!("expected UserJoined, got {joined:?}");
    };
    assert!(!user.flags.is_admin(), "bob is not an admin");
    server.shutdown().await;
}

/// `AdminSetRole` broadcasts a `UserState` for the target to every connected
/// peer (not just a unicast `UserAdminChanged` to the target itself), so the
/// live badge updates without a reconnect.
#[tokio::test]
async fn promoting_a_peer_broadcasts_the_admin_bit_to_everyone() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let mut bob = server.client("bob").await;
    let _ = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserJoined { .. }))
        .await;

    alice
        .send_msg(&ControlMsg::AdminSetRole {
            user_id: bob.user_id,
            is_admin: true,
        })
        .await;

    // Bob's own unicast notification...
    let pushed = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserAdminChanged { .. }))
        .await;
    assert!(matches!(
        pushed,
        Some(ControlMsg::UserAdminChanged { is_admin: true })
    ));

    // ...and alice, who is not the target, also sees Bob's UserState flip.
    let state = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserState { session, .. } if *session == bob.session))
        .await;
    let Some(ControlMsg::UserState { flags, .. }) = state else {
        panic!("expected UserState, got {state:?}");
    };
    assert!(flags.is_admin(), "alice's own roster sees bob's new badge");
    server.shutdown().await;
}

/// `UserRtt` (`PROTOCOL.md` §2.2) arrives within a few seconds once two
/// clients share the room, carrying a server-measured RTT for each — a
/// mandatory v1 variant, not gated behind any minor negotiation (client and
/// server ship in lockstep in this project).
#[tokio::test]
async fn user_rtt_is_broadcast_once_two_clients_share_the_room() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let mut bob = server.client("bob").await;

    let msg = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserRtt { .. }))
        .await;
    let Some(ControlMsg::UserRtt { rtts }) = msg else {
        panic!("expected UserRtt within the timeout, got {msg:?}");
    };
    assert_eq!(rtts.len(), 2, "one entry per online participant");
    assert!(
        rtts.iter().any(|(s, _)| *s == alice.session)
            && rtts.iter().any(|(s, _)| *s == bob.session),
        "{rtts:?}"
    );

    let _ = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserRtt { .. }))
        .await
        .expect("bob gets it too");
    server.shutdown().await;
}

/// A lone participant has nobody to show a ping to: `Room::rtt_snapshot`
/// stays empty and no `UserRtt` is ever broadcast for a room of one.
#[tokio::test]
async fn a_lone_participant_never_receives_user_rtt() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;

    // One tick is 2 s; wait past it without using the 5 s `SOON` a positive
    // assertion would need.
    let msg = alice
        .recv_timeout(std::time::Duration::from_millis(2500))
        .await;
    assert!(
        !matches!(msg, Some(ControlMsg::UserRtt { .. })),
        "a room of one must never see UserRtt, got {msg:?}"
    );
    server.shutdown().await;
}
