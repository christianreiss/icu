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
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session, .. } if *session == bob.session),
        )
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

#[tokio::test]
async fn admin_kick_closes_with_kicked_and_the_user_may_come_back() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let identity = server.enroll("bob").await;
    let mut bob = server.connect(&identity).await.expect("connect");
    bob.hello("bob").await;

    alice
        .send_msg(&ControlMsg::AdminKick {
            user_id: bob.user_id,
        })
        .await;
    let ok = alice
        .recv_matching(|m| matches!(m, ControlMsg::AdminKickOk { .. }))
        .await;
    assert!(
        matches!(ok, Some(ControlMsg::AdminKickOk { closed: 1, .. })),
        "{ok:?}"
    );
    assert_eq!(bob.closed_code().await, Some(ec::KICKED));

    // Nothing in the store changed: the same certificate gets back in.
    let mut again = server.connect(&identity).await.expect("reconnect");
    assert!(matches!(
        again.hello("bob").await,
        ControlMsg::Welcome { .. }
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn admin_kick_from_a_non_admin_is_rejected() {
    let server = TestServer::start().await;
    let alice = server.client("alice").await; // sole bootstrap admin
    let mut bob = server.client("bob").await;

    bob.send_msg(&ControlMsg::AdminKick {
        user_id: alice.user_id,
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
    server.shutdown().await;
}

#[tokio::test]
async fn admin_restriction_is_enforced_persisted_and_lifted() {
    use icu_proto::UserFlags;
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let identity = server.enroll("bob").await;
    let mut bob = server.connect(&identity).await.expect("connect");
    bob.hello("bob").await;
    let bob_id = bob.user_id;

    alice
        .send_msg(&ControlMsg::AdminSetRestriction {
            user_id: bob_id,
            muted: true,
            deafened: true,
        })
        .await;
    // The `UserState` broadcast goes out before the ack.
    let seen = alice
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session, .. } if *session == bob.session),
        )
        .await;
    let Some(ControlMsg::UserState { flags, .. }) = seen else {
        panic!("{seen:?}")
    };
    assert!(flags.muted() && flags.deafened());
    let ok = alice
        .recv_matching(|m| matches!(m, ControlMsg::AdminSetRestrictionOk { .. }))
        .await;
    assert!(ok.is_some(), "{ok:?}");
    let pushed = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserRestrictionChanged { .. }))
        .await;
    assert_eq!(
        pushed,
        Some(ControlMsg::UserRestrictionChanged {
            muted: true,
            deafened: true
        })
    );

    // The client cannot clear it.
    bob.send_msg(&ControlMsg::SetState {
        flags: UserFlags::NONE,
    })
    .await;
    alice.expect_silence().await;

    // Muted: nothing relayed to alice.
    bob.send_audio(1, 960, &[0xF8, 1, 2, 3]);
    assert!(alice
        .recv_audio(std::time::Duration::from_millis(300))
        .await
        .is_none());

    // Survives a reconnect, announced right after `Welcome`.
    bob.conn.close(0u32.into(), b"");
    drop(bob);
    let mut bob = server.connect(&identity).await.expect("reconnect");
    let ControlMsg::Welcome { you, .. } = bob.hello("bob").await else {
        panic!("no Welcome")
    };
    assert!(you.flags.muted() && you.flags.deafened());
    let pushed = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserRestrictionChanged { .. }))
        .await;
    assert!(pushed.is_some());

    // Listed for admins.
    alice.send_msg(&ControlMsg::UserListRequest).await;
    let Some(ControlMsg::UserListResponse { users }) = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserListResponse { .. }))
        .await
    else {
        panic!("no UserListResponse")
    };
    let row = users.iter().find(|u| u.user_id == bob_id).unwrap();
    assert!(row.muted && row.deafened);

    // Lifted: the client may unmute again.
    alice
        .send_msg(&ControlMsg::AdminSetRestriction {
            user_id: bob_id,
            muted: false,
            deafened: false,
        })
        .await;
    let pushed = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserRestrictionChanged { .. }))
        .await;
    assert_eq!(
        pushed,
        Some(ControlMsg::UserRestrictionChanged {
            muted: false,
            deafened: false
        })
    );
    bob.send_msg(&ControlMsg::SetState {
        flags: UserFlags::NONE,
    })
    .await;
    let state = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserState { .. }))
        .await;
    let Some(ControlMsg::UserState { flags, .. }) = state else {
        panic!("{state:?}")
    };
    assert!(!flags.muted() && !flags.deafened());
    server.shutdown().await;
}

/// An enforced deafen implies an enforced mute — who hears nobody must not
/// talk into the room — and cuts video to that user as well
/// (`PROTOCOL.md` §2.2 Nr. 33).
#[tokio::test]
async fn an_enforced_deafen_mutes_and_stops_video() {
    use icu_proto::UserFlags;
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let identity = server.enroll("bob").await;
    let mut bob = server.connect(&identity).await.expect("connect");
    bob.hello_with("bob", icu_proto::MINOR, UserFlags(UserFlags::VIEWING))
        .await
        .expect("Welcome");
    let mut carol = server.client("carol").await;

    alice
        .send_msg(&ControlMsg::AdminSetRestriction {
            user_id: bob.user_id,
            muted: false,
            deafened: true,
        })
        .await;
    let pushed = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserRestrictionChanged { .. }))
        .await;
    assert_eq!(
        pushed,
        Some(ControlMsg::UserRestrictionChanged {
            muted: true,
            deafened: true
        }),
        "deafened includes muted"
    );
    let bob_session = bob.session;
    let seen = alice
        .recv_matching(
            |m| matches!(m, ControlMsg::UserState { session, .. } if *session == bob_session),
        )
        .await;
    let Some(ControlMsg::UserState { flags, .. }) = seen else {
        panic!("{seen:?}")
    };
    assert!(flags.muted() && flags.deafened());

    // Bob's audio is not relayed.
    bob.send_audio(1, 960, &[0xF8, 1, 2, 3]);
    assert!(alice
        .recv_audio(std::time::Duration::from_millis(300))
        .await
        .is_none());

    // Carol's camera does not reach bob, although he is viewing.
    carol
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::CAMERA_ON),
        })
        .await;
    let carol_session = carol.session;
    carol
        .recv_matching(|m| matches!(m, ControlMsg::UserState { session, flags } if *session == carol_session && flags.camera_on()))
        .await
        .expect("camera on");
    carol.send_video(1, 0, true, &[0x42; 64]).await;
    assert!(bob
        .recv_video(std::time::Duration::from_millis(300))
        .await
        .is_none());

    // The restriction stored is what the admin set; the list says so.
    alice.send_msg(&ControlMsg::UserListRequest).await;
    let Some(ControlMsg::UserListResponse { users }) = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserListResponse { .. }))
        .await
    else {
        panic!("no UserListResponse")
    };
    let row = users.iter().find(|u| u.name == "bob").unwrap();
    assert!(!row.muted && row.deafened);

    // Lifted, video flows again.
    alice
        .send_msg(&ControlMsg::AdminSetRestriction {
            user_id: bob.user_id,
            muted: false,
            deafened: false,
        })
        .await;
    bob.recv_matching(|m| {
        matches!(
            m,
            ControlMsg::UserRestrictionChanged {
                deafened: false,
                ..
            }
        )
    })
    .await
    .expect("lifted");
    carol.send_video(2, 3200, true, &[0x43; 64]).await;
    assert!(bob.recv_video(common::SOON).await.is_some());
    server.shutdown().await;
}

/// A write that fails is the server's trouble, not a policy refusal: the
/// admin gets `SERVER_ERROR`, not `LAST_ADMIN` or `NAME_TAKEN`, nothing
/// changes, and the connection stays.
#[tokio::test]
async fn a_failed_write_is_a_server_error_not_a_refusal() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let bob = server.client("bob").await;

    // A directory where the atomic write puts its temp file.
    let blocker = server.dir.path().join(".users.toml.tmp");
    std::fs::create_dir(&blocker).expect("block the write");
    alice
        .send_msg(&ControlMsg::AdminSetRole {
            user_id: bob.user_id,
            is_admin: true,
        })
        .await;
    let answer = alice
        .recv_matching(|m| {
            matches!(
                m,
                ControlMsg::AdminActionError { .. } | ControlMsg::AdminSetRoleOk { .. }
            )
        })
        .await;
    assert!(
        matches!(answer, Some(ControlMsg::AdminActionError { code, .. }) if code == ec::SERVER_ERROR),
        "{answer:?}"
    );
    alice
        .send_msg(&ControlMsg::CreateInviteRequest {
            name: String::new(),
            ttl_s: 0,
        })
        .await;
    let answer = alice
        .recv_matching(|m| {
            matches!(
                m,
                ControlMsg::Error { .. } | ControlMsg::CreateInviteResponse { .. }
            )
        })
        .await;
    assert!(
        matches!(answer, Some(ControlMsg::Error { code, .. }) if code == ec::SERVER_ERROR),
        "{answer:?}"
    );
    assert!(!ec::is_fatal(ec::SERVER_ERROR));
    std::fs::remove_dir(&blocker).expect("unblock");

    alice
        .send_msg(&ControlMsg::AdminSetRole {
            user_id: bob.user_id,
            is_admin: true,
        })
        .await;
    assert!(alice
        .recv_matching(|m| matches!(m, ControlMsg::AdminSetRoleOk { .. }))
        .await
        .is_some());
    server.shutdown().await;
}

/// A user list longer than one control frame is shortened, not dropped:
/// online users and admins first, and a last row with `user_id` 0 saying
/// how many are left out (`PROTOCOL.md` §2.2 Nr. 27).
#[tokio::test]
async fn a_user_list_too_long_for_a_frame_is_shortened() {
    let server = TestServer::start().await;
    let alice = server.enroll("alice").await; // sole bootstrap admin
    let bob = server.enroll("bob").await;
    let common::TestServer { dir, handle } = server;
    handle.shutdown().await;
    // 600 accounts with long names: years of invitations, most removed.
    let users = dir.path().join("users.toml");
    let mut text = std::fs::read_to_string(&users).unwrap();
    const EXTRA: u32 = 600;
    for i in 0..EXTRA {
        text.push_str(&format!(
            "\n[user.{id}]\nname = \"old-account-with-a-long-name-{i:03}\"\n\
             created = \"2026-01-01T00:00:00Z\"\n\n[[user.{id}.cert]]\n\
             fingerprint = \"{i:064x}\"\nissued = \"2026-01-01T00:00:00Z\"\n\
             expires = \"2099-01-01T00:00:00Z\"\nrevoked = {revoked}\n",
            id = 10_000 + i,
            revoked = i % 3 != 0,
        ));
    }
    std::fs::write(&users, text).unwrap();
    let server = TestServer {
        handle: icu_server::run(icu_server::config::Config::for_test(dir.path()))
            .await
            .expect("restarts"),
        dir,
    };
    let mut alice = server.connect(&alice).await.expect("connect");
    alice.hello("alice").await;
    let mut bob = server.connect(&bob).await.expect("connect");
    bob.hello("bob").await;

    alice.send_msg(&ControlMsg::UserListRequest).await;
    let Some(ControlMsg::UserListResponse { users }) = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserListResponse { .. }))
        .await
    else {
        panic!("no UserListResponse")
    };
    let rest = users.last().expect("rows");
    assert_eq!(rest.user_id, 0, "the last row stands for the rest");
    let left_out: usize = rest
        .name
        .split_whitespace()
        .find_map(|w| w.parse().ok())
        .expect("a count in the name");
    assert_eq!(
        users.len() - 1 + left_out,
        EXTRA as usize + 2,
        "{}",
        rest.name
    );
    assert!(
        users.len() > 100,
        "a frame still carries a lot: {}",
        users.len()
    );
    assert!(users[..2].iter().all(|u| u.online), "online users first");
    assert!(
        users[..users.len() - 1]
            .windows(2)
            .all(|w| w[0].revoked <= w[1].revoked),
        "removed accounts last"
    );
    server.shutdown().await;
}

/// An admin action queued on the store lock is judged by the store as it
/// is when the action gets the lock, not as it was when the message was
/// read: here alice asks to re-enable herself while another admin's
/// disable holds the lock. Without the check under the lock her request
/// would undo the disable that kicks her.
#[tokio::test]
async fn a_queued_self_enable_fails_once_the_disable_commits() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let bob = server.client("bob").await;
    let store = &server.handle.server().store;
    store.lock().await.set_admin(bob.user_id, true).unwrap();

    let mut guard = store.lock().await;
    alice
        .send_msg(&ControlMsg::AdminSetDisabled {
            user_id: alice.user_id,
            disabled: false,
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    guard.set_disabled(alice.user_id, true).unwrap();
    drop(guard);

    let err = alice
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
    assert!(store.lock().await.user(alice.user_id).unwrap().disabled);
    server.shutdown().await;
}

/// The same for a revoked actor: a role grant queued behind the revoke
/// does not go through.
#[tokio::test]
async fn a_queued_role_grant_fails_once_the_actor_is_revoked() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await; // sole bootstrap admin
    let bob = server.client("bob").await;
    let carol = server.client("carol").await;
    let store = &server.handle.server().store;
    store.lock().await.set_admin(bob.user_id, true).unwrap();

    let mut guard = store.lock().await;
    alice
        .send_msg(&ControlMsg::AdminSetRole {
            user_id: carol.user_id,
            is_admin: true,
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    guard.revoke(alice.user_id).unwrap();
    drop(guard);

    let err = alice
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
    assert!(!store.lock().await.is_admin(carol.user_id));
    server.shutdown().await;
}

/// `registration::issue` checks its actor under its own lock: the in-band
/// caller's earlier check ran under another one, and a demotion in between
/// must not leave the invitation authorized.
#[tokio::test]
async fn an_invitation_is_not_issued_for_a_demoted_actor() {
    let server = TestServer::start().await;
    let alice = server.client("alice").await; // sole bootstrap admin
    let bob = server.client("bob").await;
    let srv = server.handle.server();
    let actor = {
        let mut store = srv.store.lock().await;
        store.set_admin(bob.user_id, true).unwrap();
        let user = store.user(alice.user_id).unwrap();
        // The session's leaf; the registration certificate is revoked.
        let fp = user.certs.iter().find(|c| !c.revoked).unwrap().fingerprint.clone();
        icu_server::admin::Actor {
            user_id: alice.user_id,
            cert_fp: fp,
        }
    };
    let issued = icu_server::registration::issue(srv, Some(&actor), "", 0).await;
    assert!(issued.is_ok(), "{:?}", issued.err());

    srv.store
        .lock()
        .await
        .set_admin(alice.user_id, false)
        .unwrap();
    let refused = icu_server::registration::issue(srv, Some(&actor), "", 0).await;
    assert_eq!(refused.err().and_then(|e| e.code()), Some(ec::NOT_ADMIN));
    assert!(icu_server::registration::issue(srv, None, "", 0)
        .await
        .is_ok(), "the socket has no actor");
    server.shutdown().await;
}
