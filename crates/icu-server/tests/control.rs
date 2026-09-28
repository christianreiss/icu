//! Control channel behaviour (`docs/reference/server.md` §10: Hello-Timeout,
//! `SERVER_FULL`, `Ping`/`Pong`, rename through `RenewRequest`, `Hello.name`,
//! rate limits).

mod common;

use std::time::Duration;

use common::TestServer;
use icu_proto::{error as ec, ControlMsg, UserFlags};

#[tokio::test]
async fn no_hello_within_the_timeout_closes_with_hello_timeout() {
    let server = TestServer::start_with(|config| {
        config.timing.hello_timeout_ms = 200;
    })
    .await;
    let identity = server.enroll("alice").await;
    let client = server.connect(&identity).await.expect("connect");
    // Say nothing at all.
    assert_eq!(client.closed_code().await, Some(ec::HELLO_TIMEOUT));
    server.shutdown().await;
}

#[tokio::test]
async fn a_full_room_closes_with_server_full() {
    let server = TestServer::start_with(|config| {
        config.server.max_users = 2;
    })
    .await;
    let _alice = server.client("alice").await;
    let _bob = server.client("bob").await;

    let carol = server.enroll("carol").await;
    let mut client = server.connect(&carol).await.expect("connect");
    assert!(
        client
            .hello_with("carol", icu_proto::MINOR, UserFlags::NONE)
            .await
            .is_none(),
        "no Welcome for a full room"
    );
    assert_eq!(client.closed_code().await, Some(ec::SERVER_FULL));
    server.shutdown().await;
}

#[tokio::test]
async fn a_minor_below_the_minimum_is_version_unsupported() {
    let server = TestServer::start().await;
    let identity = server.enroll("alice").await;
    for minor in [0, icu_proto::MIN_PEER_MINOR - 1] {
        let mut client = server.connect(&identity).await.expect("connect");
        assert!(client
            .hello_with("alice", minor, UserFlags::NONE)
            .await
            .is_none());
        assert_eq!(
            client.closed_code().await,
            Some(ec::VERSION_UNSUPPORTED),
            "minor {minor}"
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn a_client_below_min_client_is_version_unsupported() {
    // The harness registers as `icu-test/0.1.0`, so the minimum is exactly that.
    let server = TestServer::start_with(|c| c.server.min_client = "0.1.0".into()).await;
    let identity = server.enroll("alice").await;
    for client_string in ["icu/0.0.9 linux", "icu linux", "icu/0.1 linux", ""] {
        let mut client = server.connect(&identity).await.expect("connect");
        assert!(
            client.hello_as("alice", client_string).await.is_none(),
            "{client_string:?} got a Welcome"
        );
        assert_eq!(
            client.closed_code().await,
            Some(ec::VERSION_UNSUPPORTED),
            "{client_string:?}"
        );
    }
    for client_string in [
        "icu/0.1.0 linux",
        "icu/0.1.0-rc.1 linux",
        "icu/1.0.0 windows",
    ] {
        let mut client = server.connect(&identity).await.expect("connect");
        assert!(
            matches!(
                client.hello_as("alice", client_string).await,
                Some(ControlMsg::Welcome { .. })
            ),
            "{client_string:?} got no Welcome"
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn a_higher_client_minor_is_negotiated_down() {
    let server = TestServer::start().await;
    let identity = server.enroll("alice").await;
    let mut client = server.connect(&identity).await.expect("connect");
    let welcome = client
        .hello_with("alice", 9, UserFlags::NONE)
        .await
        .expect("Welcome");
    match welcome {
        ControlMsg::Welcome {
            minor,
            server_minor,
            ..
        } => {
            assert_eq!(minor, icu_proto::MINOR);
            assert_eq!(server_minor, icu_proto::MINOR);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn ping_is_answered_with_a_plausible_pong() {
    let server = TestServer::start().await;
    let mut client = server.client("alice").await;
    let before = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    client.send_msg(&ControlMsg::Ping { nonce: 0xABCD }).await;
    let pong = client
        .recv_matching(|m| matches!(m, ControlMsg::Pong { .. }))
        .await
        .expect("Pong");
    match pong {
        ControlMsg::Pong {
            nonce,
            server_time_ms,
        } => {
            assert_eq!(nonce, 0xABCD, "the nonce comes back");
            let after = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            assert!(
                server_time_ms >= before && server_time_ms <= after + 1000,
                "server_time_ms {server_time_ms} not between {before} and {after}"
            );
        }
        other => panic!("expected Pong, got {other:?}"),
    }
    server.shutdown().await;
}

/// A rename is a renewal carrying a new CN: the name lives in the certificate
/// (`docs/reference/protocol.md` §2.2).
#[tokio::test]
async fn renewal_with_a_new_cn_renames_and_broadcasts_user_renamed() {
    let server = TestServer::start().await;
    let old = server.enroll("alice").await;
    let mut alice = server.connect(&old).await.expect("connect");
    alice.hello("alice").await;
    let mut bob = server.client("bob").await;

    let (_key, csr) = common::csr_with_cn("alicia");
    alice
        .send_msg(&ControlMsg::RenewRequest { csr_der: csr })
        .await;

    let response = alice
        .recv_matching(|m| matches!(m, ControlMsg::RenewResponse { .. }))
        .await
        .expect("RenewResponse");
    let ControlMsg::RenewResponse { leaf_der } = response else {
        unreachable!()
    };
    assert_eq!(
        icu_server::ca::leaf_common_name(&leaf_der),
        "alicia",
        "the new certificate carries the new name"
    );

    for client in [&mut alice, &mut bob] {
        let msg = client
            .recv_matching(|m| matches!(m, ControlMsg::UserRenamed { .. }))
            .await;
        assert!(
            matches!(&msg, Some(ControlMsg::UserRenamed { session: 1, name }) if name == "alicia"),
            "{msg:?}"
        );
    }

    // The pre-rename certificate stays valid until the renamed one first
    // logs in (`docs/reference/protocol.md` §2.2 Nr. 10), and sees the current name.
    let mut stale = server.connect(&old).await.expect("old cert still connects");
    match stale.hello("alice").await {
        ControlMsg::Welcome { you, .. } => assert_eq!(you.name, "alicia"),
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn renewal_onto_a_taken_name_is_name_taken_and_changes_nothing() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let mut bob = server.client("bob").await;

    let (_key, csr) = common::csr_with_cn("BOB");
    alice
        .send_msg(&ControlMsg::RenewRequest { csr_der: csr })
        .await;
    let msg = alice
        .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
        .await;
    assert!(
        matches!(&msg, Some(ControlMsg::Error { code, .. }) if *code == ec::NAME_TAKEN),
        "{msg:?}"
    );
    bob.expect_silence().await;

    // Non-fatal: the connection is still usable.
    alice.send_msg(&ControlMsg::Ping { nonce: 7 }).await;
    let pong = alice
        .recv_matching(|m| matches!(m, ControlMsg::Pong { .. }))
        .await;
    assert!(
        matches!(pong, Some(ControlMsg::Pong { nonce: 7, .. })),
        "{pong:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_renewal_cn_is_validated_like_any_other_name() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    for bad in ["", &"x".repeat(33), "a\nb"] {
        let (_key, csr) = common::csr_with_cn(bad);
        alice
            .send_msg(&ControlMsg::RenewRequest { csr_der: csr })
            .await;
        let msg = alice
            .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
            .await;
        assert!(
            matches!(&msg, Some(ControlMsg::Error { code, .. }) if *code == ec::RENEW_REJECTED),
            "{bad:?} should be rejected, got {msg:?}"
        );
    }
    server.shutdown().await;
}

/// A device that missed a rename connects with a stale CN, learns the current
/// name from `Welcome` and catches up — even though the rename already used
/// the day's renewal budget. A second catch-up on the same certificate is
/// refused, so a stale session cannot mint certificates in a loop.
#[tokio::test]
async fn a_stale_cn_connects_and_may_catch_up_the_same_day() {
    let server = TestServer::start().await;
    let old = server.enroll("alice").await;
    let mut alice = server.connect(&old).await.expect("connect");
    alice.hello("alice").await;

    let (_key, csr) = common::csr_with_cn("alicia");
    alice
        .send_msg(&ControlMsg::RenewRequest { csr_der: csr })
        .await;
    alice
        .recv_matching(|m| matches!(m, ControlMsg::RenewResponse { .. }))
        .await
        .expect("the rename lands");

    // Second device, still holding the pre-rename certificate.
    let mut stale = server.connect(&old).await.expect("stale connect");
    match stale.hello("alice").await {
        ControlMsg::Welcome { you, .. } => assert_eq!(
            you.name, "alicia",
            "the roster carries the current name, not the certificate's"
        ),
        other => panic!("expected Welcome, got {other:?}"),
    }

    let (_key, csr) = common::csr_with_cn("alicia");
    stale
        .send_msg(&ControlMsg::RenewRequest {
            csr_der: csr.clone(),
        })
        .await;
    let response = stale
        .recv_matching(|m| matches!(m, ControlMsg::RenewResponse { .. }))
        .await
        .expect("catch-up is exempt from the daily limit");
    let ControlMsg::RenewResponse { leaf_der } = response else {
        unreachable!()
    };
    assert_eq!(icu_server::ca::leaf_common_name(&leaf_der), "alicia");

    // The same certificate cannot catch up twice.
    stale
        .send_msg(&ControlMsg::RenewRequest { csr_der: csr })
        .await;
    let msg = stale
        .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
        .await;
    assert!(
        matches!(&msg, Some(ControlMsg::Error { code, .. }) if *code == ec::RENEW_REJECTED),
        "{msg:?}"
    );
    server.shutdown().await;
}

/// `SetName` is obsolete; the variant number stays reserved, so a server that
/// still receives it treats it as a wrong-direction message.
#[tokio::test]
async fn set_name_is_a_protocol_violation() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    alice
        .send_msg(&ControlMsg::SetName {
            name: "alicia".into(),
        })
        .await;
    assert_eq!(alice.closed_code().await, Some(ec::PROTOCOL_VIOLATION));
    server.shutdown().await;
}

#[tokio::test]
async fn hello_name_is_ignored_entirely() {
    let server = TestServer::start().await;
    let identity = server.enroll("alice").await;
    let mut client = server.connect(&identity).await.expect("connect");
    let welcome = client
        .hello_with("mallory", icu_proto::MINOR, UserFlags::NONE)
        .await
        .expect("Welcome");
    match welcome {
        ControlMsg::Welcome { you, .. } => assert_eq!(
            you.name, "alice",
            "the name comes from the certificate, never from Hello"
        ),
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn set_state_is_broadcast_including_to_the_originator() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let mut bob = server.client("bob").await;

    alice
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::MUTED | UserFlags::CAMERA_ON),
        })
        .await;
    for client in [&mut alice, &mut bob] {
        let msg = client
            .recv_matching(|m| matches!(m, ControlMsg::UserState { .. }))
            .await;
        match msg {
            Some(ControlMsg::UserState { session, flags }) => {
                assert_eq!(session, 1);
                assert!(flags.muted() && flags.camera_on());
            }
            other => panic!("expected UserState, got {other:?}"),
        }
    }
    server.shutdown().await;
}

#[tokio::test]
async fn a_client_cannot_set_the_speaking_bit() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    alice
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::SPEAKING | UserFlags::DEAFENED),
        })
        .await;
    let msg = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserState { .. }))
        .await;
    match msg {
        Some(ControlMsg::UserState { flags, .. }) => {
            assert!(flags.deafened());
            assert!(!flags.speaking(), "bit2 is server authoritative");
        }
        other => panic!("expected UserState, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn joining_and_leaving_is_announced() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    let bob = server.client("bob").await;

    let joined = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserJoined { .. }))
        .await;
    match joined {
        Some(ControlMsg::UserJoined { user }) => {
            assert_eq!(user.session, 2);
            assert_eq!(user.name, "bob");
            assert_eq!(user.user_id, 2);
        }
        other => panic!("expected UserJoined, got {other:?}"),
    }

    bob.conn
        .close(quinn::VarInt::from_u32(ec::NO_ERROR), b"bye");
    drop(bob);
    let left = alice
        .recv_matching(|m| matches!(m, ControlMsg::UserLeft { .. }))
        .await;
    assert!(
        matches!(left, Some(ControlMsg::UserLeft { session: 2, .. })),
        "{left:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_second_hello_is_a_protocol_violation() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    alice
        .send_msg(&ControlMsg::Hello {
            minor: 1,
            caps: 0,
            client: "again/1.0 linux".into(),
            initial: UserFlags::NONE,
            name: "alice".into(),
        })
        .await;
    assert_eq!(alice.closed_code().await, Some(ec::PROTOCOL_VIOLATION));
    server.shutdown().await;
}

#[tokio::test]
async fn a_server_to_client_message_from_a_client_is_a_protocol_violation() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    alice
        .send_msg(&ControlMsg::LossHint { loss_permille: 10 })
        .await;
    assert_eq!(alice.closed_code().await, Some(ec::PROTOCOL_VIOLATION));
    server.shutdown().await;
}

#[tokio::test]
async fn a_zero_length_frame_is_a_protocol_violation() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    alice.send_raw(&[0x00, 0x00]).await;
    assert_eq!(alice.closed_code().await, Some(ec::PROTOCOL_VIOLATION));
    server.shutdown().await;
}

#[tokio::test]
async fn an_oversized_frame_is_a_protocol_violation() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    // 16385 = 0x4001 little endian.
    alice.send_raw(&[0x01, 0x40]).await;
    assert_eq!(alice.closed_code().await, Some(ec::PROTOCOL_VIOLATION));
    server.shutdown().await;
}

#[tokio::test]
async fn an_unknown_variant_is_a_protocol_violation() {
    let server = TestServer::start().await;
    let mut alice = server.client("alice").await;
    // One byte body with discriminant 99: no such variant in v1.
    alice.send_raw(&[0x01, 0x00, 99]).await;
    assert_eq!(alice.closed_code().await, Some(ec::PROTOCOL_VIOLATION));
    server.shutdown().await;
}

#[tokio::test]
async fn the_first_message_must_be_hello() {
    let server = TestServer::start().await;
    let identity = server.enroll("alice").await;
    let mut client = server.connect(&identity).await.expect("connect");
    client.send_msg(&ControlMsg::Ping { nonce: 1 }).await;
    assert_eq!(client.closed_code().await, Some(ec::PROTOCOL_VIOLATION));
    server.shutdown().await;
}

/// §10: the third `RATE_LIMITED` violation within the window closes with
/// `RATE_LIMITED_FATAL`.
///
/// One violation per burst: the bucket allows 5 messages per second with a
/// one second burst, so the sixth message of a burst is exactly one violation.
#[tokio::test]
async fn three_rate_limit_violations_close_fatally() {
    let server = TestServer::start_with(|config| {
        config.limits.control_msgs_per_s = 5;
        config.limits.set_state_per_s = 100;
    })
    .await;
    let mut alice = server.client("alice").await;

    let mut errors = 0;
    for burst in 0..3u64 {
        for i in 0..6u64 {
            alice
                .send_msg(&ControlMsg::Ping {
                    nonce: burst * 10 + i,
                })
                .await;
        }
        if let Some(ControlMsg::Error { code, .. }) = alice
            .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
            .await
        {
            assert_eq!(code, ec::RATE_LIMITED);
            errors += 1;
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
    }

    assert!(
        errors >= 2,
        "the first two violations are reported as Error, got {errors}"
    );
    assert_eq!(
        alice.closed_code().await,
        Some(ec::RATE_LIMITED_FATAL),
        "the third violation is fatal"
    );
    server.shutdown().await;
}

/// A single violation is not fatal: the connection carries on.
#[tokio::test]
async fn one_rate_limit_violation_keeps_the_connection() {
    let server = TestServer::start_with(|config| {
        config.limits.control_msgs_per_s = 2;
        config.limits.set_state_per_s = 100;
    })
    .await;
    let mut alice = server.client("alice").await;
    for nonce in 0..3u64 {
        alice.send_msg(&ControlMsg::Ping { nonce }).await;
    }
    let error = alice
        .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
        .await;
    assert!(
        matches!(&error, Some(ControlMsg::Error { code, .. }) if *code == ec::RATE_LIMITED),
        "{error:?}"
    );

    // One second later there are tokens again and the connection still works.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    alice.send_msg(&ControlMsg::Ping { nonce: 99 }).await;
    let pong = alice
        .recv_matching(|m| matches!(m, ControlMsg::Pong { .. }))
        .await;
    assert!(
        matches!(pong, Some(ControlMsg::Pong { nonce: 99, .. })),
        "{pong:?}"
    );
    server.shutdown().await;
}
