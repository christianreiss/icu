//! The admin socket and the CLI path (`Server/IMPLEMENTATION.md` §5, §10 row
//! "Invite über den Admin-Socket wirkt ohne Serverneustart").

mod common;

use common::TestServer;
use icu_proto::ControlMsg;
use icu_server::{admin, AdminRequest};

/// §10: a registration bundle issued through the socket works right away,
/// without a restart.
#[tokio::test]
async fn an_invite_over_the_socket_works_without_a_restart() {
    let server = TestServer::start().await;
    let socket = server.socket();

    let response = admin::request(
        &socket,
        &AdminRequest::Invite {
            name: "bob".into(),
            ttl_s: Some(3600),
        },
    )
    .await
    .expect("admin request");
    assert_eq!(response["ok"], serde_json::json!(true));
    let bundle = response["bundle"].as_str().expect("bundle").to_string();
    assert!(
        icu_proto::passport::Passport::looks_like(&bundle),
        "{bundle}"
    );
    assert_eq!(response["name"], serde_json::json!("bob"));
    assert_eq!(response["user_id"], serde_json::json!(1));
    assert_eq!(response["is_admin"], serde_json::json!(true), "first user");

    // The very same running server accepts it.
    let identity = server
        .register(&bundle, "bob")
        .await
        .expect("registration with the fresh bundle");
    let mut client = server.connect(&identity).await.expect("connect");
    let welcome = client.hello("bob").await;
    assert!(matches!(welcome, ControlMsg::Welcome { .. }));
    server.shutdown().await;
}

#[tokio::test]
async fn users_lists_online_state_and_the_newest_certificate() {
    let server = TestServer::start().await;
    let socket = server.socket();
    let _alice = server.client("alice").await;
    server.enroll("bob").await;

    let response = admin::request(&socket, &AdminRequest::Users)
        .await
        .expect("users");
    let users = response["users"].as_array().expect("users array");
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["user_id"], serde_json::json!(1));
    assert_eq!(users[0]["name"], serde_json::json!("alice"));
    assert_eq!(users[0]["online"], serde_json::json!(true));
    assert_eq!(users[0]["revoked"], serde_json::json!(false));
    assert_eq!(
        users[0]["fingerprint"].as_str().unwrap().len(),
        12,
        "the table shows a shortened fingerprint"
    );
    assert_eq!(users[1]["online"], serde_json::json!(false));
    server.shutdown().await;
}

#[tokio::test]
async fn status_reports_uptime_online_and_relay_counters() {
    let server = TestServer::start().await;
    let socket = server.socket();
    let alice = server.client("alice").await;
    let bob = server.client("bob").await;
    alice.send_audio(1, 0, &[1; 30]);
    bob.recv_audio(common::SOON).await.expect("relayed");

    let response = admin::request(&socket, &AdminRequest::Status)
        .await
        .expect("status");
    assert_eq!(response["ok"], serde_json::json!(true));
    assert_eq!(response["online"], serde_json::json!(2));
    assert!(response["uptime_s"].is_u64());
    assert_eq!(response["relay"]["datagrams_in"], serde_json::json!(1));
    assert_eq!(response["relay"]["datagrams_out"], serde_json::json!(1));
    assert!(response["relay"]["dropped"].is_object());
    assert_eq!(response["enroll"]["ok"], serde_json::json!(2));
    server.shutdown().await;
}

/// §10: promote/demote over the socket push `UserAdminChanged` live to the
/// target's own open connection, without a reconnect — mirrors
/// `rename_over_the_socket_broadcasts_to_the_room`'s live-propagation shape.
#[tokio::test]
async fn promote_and_demote_over_the_socket_push_to_the_online_target() {
    let server = TestServer::start().await;
    let socket = server.socket();
    let _alice = server.client("alice").await; // sole bootstrap admin
    let mut bob = server.client("bob").await;

    let response = admin::request(&socket, &AdminRequest::Promote { user: "bob".into() })
        .await
        .expect("promote");
    assert_eq!(response["ok"], serde_json::json!(true));
    let msg = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserAdminChanged { .. }))
        .await;
    assert!(
        matches!(msg, Some(ControlMsg::UserAdminChanged { is_admin: true })),
        "{msg:?}"
    );

    let response = admin::request(&socket, &AdminRequest::Demote { user: "bob".into() })
        .await
        .expect("demote");
    assert_eq!(response["ok"], serde_json::json!(true));
    let msg = bob
        .recv_matching(|m| matches!(m, ControlMsg::UserAdminChanged { .. }))
        .await;
    assert!(
        matches!(msg, Some(ControlMsg::UserAdminChanged { is_admin: false })),
        "{msg:?}"
    );
    server.shutdown().await;
}

/// §10: the last-admin guard applies identically over the socket.
#[tokio::test]
async fn last_admin_guard_applies_over_the_socket() {
    let server = TestServer::start().await;
    let socket = server.socket();
    server.enroll("alice").await; // sole bootstrap admin

    let response = admin::request(
        &socket,
        &AdminRequest::Demote {
            user: "alice".into(),
        },
    )
    .await
    .expect("demote");
    assert_eq!(response["ok"], serde_json::json!(false));
    assert_eq!(response["error"], serde_json::json!("LAST_ADMIN"));
    server.shutdown().await;
}

/// §10: `disable` over the socket reports the live-kick count, like `revoke`.
#[tokio::test]
async fn disable_over_the_socket_reports_closed_count() {
    let server = TestServer::start().await;
    let socket = server.socket();
    let _alice = server.client("alice").await;
    let bob = server.client("bob").await;

    let response = admin::request(&socket, &AdminRequest::Disable { user: "bob".into() })
        .await
        .expect("disable");
    assert_eq!(response["ok"], serde_json::json!(true));
    assert_eq!(response["user_id"], serde_json::json!(2));
    assert_eq!(response["closed"], serde_json::json!(1));
    assert_eq!(
        bob.closed_code().await,
        Some(icu_proto::error::AUTH_DISABLED)
    );
    server.shutdown().await;
}

/// §10: the `users` table reports the new `disabled` column.
#[tokio::test]
async fn users_command_reports_disabled() {
    let server = TestServer::start().await;
    let socket = server.socket();
    let _alice = server.client("alice").await;
    server.enroll("bob").await;
    admin::request(&socket, &AdminRequest::Disable { user: "bob".into() })
        .await
        .expect("disable");

    let response = admin::request(&socket, &AdminRequest::Users)
        .await
        .expect("users");
    let users = response["users"].as_array().expect("users array");
    assert_eq!(users[0]["disabled"], serde_json::json!(false));
    assert_eq!(users[1]["disabled"], serde_json::json!(true));
    server.shutdown().await;
}

#[tokio::test]
async fn an_unknown_user_is_reported_as_an_error() {
    let server = TestServer::start().await;
    let socket = server.socket();
    let response = admin::request(
        &socket,
        &AdminRequest::Revoke {
            user: "nobody".into(),
        },
    )
    .await
    .expect("revoke");
    assert_eq!(response["ok"], serde_json::json!(false));
    assert_eq!(response["error"], serde_json::json!("NO_SUCH_USER"));
    assert!(response["detail"].as_str().unwrap().contains("nobody"));
    server.shutdown().await;
}

#[tokio::test]
async fn a_malformed_request_line_is_answered_not_dropped() {
    let server = TestServer::start().await;
    let socket = server.socket();
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let (read, mut write) = stream.into_split();
    write.write_all(b"{\"cmd\":\"nonsense\"}\n").await.unwrap();
    let mut lines = BufReader::new(read).lines();
    let line = lines.next_line().await.unwrap().expect("a response line");
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["ok"], serde_json::json!(false));
    assert_eq!(value["error"], serde_json::json!("BAD_REQUEST"));
    server.shutdown().await;
}

/// §5: with no server running, the CLI does the work itself under
/// `flock(state.lock)`.
#[tokio::test]
async fn the_cli_falls_back_to_the_state_file_when_no_server_runs() {
    let dir = tempfile::tempdir().unwrap();
    let config = icu_server::Config::for_test(dir.path());

    let value = icu_server::cli::dispatch(
        &config,
        AdminRequest::Invite {
            name: "offline".into(),
            ttl_s: Some(600),
        },
    )
    .await
    .expect("offline invite");
    assert_eq!(value["ok"], serde_json::json!(true));
    assert_eq!(
        value["is_admin"],
        serde_json::json!(true),
        "the first registration on a fresh server is the admin's"
    );
    let bundle = value["bundle"].as_str().unwrap().to_string();

    // A server started afterwards honours the bundle.
    let handle = icu_server::run(config.clone()).await.expect("server");
    let server = TestServer { dir, handle };
    let identity = server
        .register(&bundle, "offline")
        .await
        .expect("register with the offline bundle");
    let mut client = server.connect(&identity).await.expect("connect");
    assert!(matches!(
        client.hello("offline").await,
        ControlMsg::Welcome { .. }
    ));
    server.shutdown().await;
}

/// §5: a running server holds `flock(state.lock)`, so the offline path is not
/// available while it runs — the CLI has to use the socket.
#[tokio::test]
async fn the_offline_path_is_blocked_while_a_server_runs() {
    let server = TestServer::start().await;
    let config = icu_server::Config::for_test(server.dir.path());
    let error = icu_server::Server::offline(config).expect_err("the lock is held");
    assert!(error.to_string().contains("cannot lock"), "{error}");
    server.shutdown().await;
}
