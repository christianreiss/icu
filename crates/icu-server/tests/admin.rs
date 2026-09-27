//! The admin socket and the CLI path (`Server/IMPLEMENTATION.md` §5, §10 row
//! "Invite über den Admin-Socket wirkt ohne Serverneustart").

mod common;

use common::TestServer;
use icu_proto::ControlMsg;
use icu_server::{admin, AdminRequest, AdminResponse};

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

/// `status` once `open_connections` has become `n`.
async fn status_with_open(socket: &std::path::Path, n: u64) -> serde_json::Value {
    tokio::time::timeout(common::SOON, async {
        loop {
            let status = admin::request(socket, &AdminRequest::Status)
                .await
                .expect("status");
            if status["open_connections"] == serde_json::json!(n) {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("open_connections never became {n}"))
}

/// S8 (readiness review 2026-09-27): besides the counter `connections`,
/// `status` says what is open right now, so a monitor has a gauge to graph
/// and a counter to form a rate from.
#[tokio::test]
async fn status_reports_open_connections_beside_the_counter() {
    let server = TestServer::start().await;
    let socket = server.socket();
    let alice = server.client("alice").await;
    let _bob = server.client("bob").await;
    let two = status_with_open(&socket, 2).await;
    assert_eq!(two["online"], serde_json::json!(2));
    assert_eq!(two["handshakes"], serde_json::json!(0));
    assert_eq!(two["roomless_sessions"], serde_json::json!(0));
    let total = two["connections"].as_u64().expect("the counter stays");
    assert!(total >= 4, "two registrations and two logins: {total}");
    drop(alice);
    let one = status_with_open(&socket, 1).await;
    assert_eq!(
        one["connections"].as_u64(),
        Some(total),
        "a counter does not go down"
    );
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

/// `mute` and `deafen` each change one side of the pair and keep the other
/// as it is at the moment of the write.
#[tokio::test]
async fn mute_and_deafen_over_the_socket_keep_each_other() {
    let server = TestServer::start().await;
    server.enroll("alice").await;
    server.enroll("bob").await;
    let bob = || "bob".to_string();
    for request in [
        AdminRequest::Mute { user: bob() },
        AdminRequest::Deafen { user: bob() },
    ] {
        assert!(matches!(
            server.handle.admin(request).await,
            AdminResponse::User { user_id: 2, .. }
        ));
    }
    let restriction = |users: &[icu_server::admin::AdminUser]| {
        let row = users.iter().find(|u| u.name == "bob").unwrap();
        (row.muted, row.deafened)
    };
    let AdminResponse::Users(users) = server.handle.admin(AdminRequest::Users).await else {
        panic!("users")
    };
    assert_eq!(restriction(&users), (true, true));
    assert!(matches!(
        server
            .handle
            .admin(AdminRequest::Unmute { user: bob() })
            .await,
        AdminResponse::User { user_id: 2, .. }
    ));
    let AdminResponse::Users(users) = server.handle.admin(AdminRequest::Users).await else {
        panic!("users")
    };
    assert_eq!(restriction(&users), (false, true));
    server.shutdown().await;
}

/// `invite` says whether an admin has registered yet: until one has, not
/// the first bundle issued but the first one completed makes an admin, and
/// the CLI points that out (`PROTOCOL.md` §2.3).
#[tokio::test]
async fn invite_says_whether_an_admin_has_registered() {
    let server = TestServer::start().await;
    let invite = || AdminRequest::Invite {
        name: String::new(),
        ttl_s: None,
    };
    let first = server.handle.admin(invite()).await;
    assert!(
        matches!(
            first,
            AdminResponse::Invite {
                is_admin: true,
                admin_confirmed: false,
                ..
            }
        ),
        "{first:?}"
    );
    assert_eq!(first.to_json()["admin_confirmed"], serde_json::json!(false));
    server.enroll("alice").await;
    let later = server.handle.admin(invite()).await;
    assert!(
        matches!(
            later,
            AdminResponse::Invite {
                is_admin: false,
                admin_confirmed: true,
                ..
            }
        ),
        "{later:?}"
    );
    server.shutdown().await;
}

/// A name reserved for examples (`icu.example.org`) is refused only where it
/// would be baked in for the first time: a new CA, the first invitation. An
/// install whose CA already carries one, from before the check, keeps
/// starting and answering `status` and `users` — refusing it in the config,
/// as before, took such a server down with the upgrade, and every CLI command
/// with it.
#[tokio::test]
async fn reserved_example_names_are_refused_only_where_they_are_new() {
    let example = |dir: &std::path::Path| {
        let mut config = icu_server::Config::for_test(dir);
        config.server.server_name = "icu.example.org".into();
        config.server.public_host = "icu.example.org:4711".into();
        config
    };
    let invite = || AdminRequest::Invite {
        name: String::new(),
        ttl_s: None,
    };

    // A fresh install: neither the server nor the offline CLI makes a CA.
    let dir = tempfile::tempdir().unwrap();
    let config = example(dir.path());
    let started = icu_server::run(config.clone()).await.err();
    let offline = icu_server::Server::offline(config.clone()).err();
    for error in [started, offline] {
        let error = error.expect("refused").to_string();
        assert!(error.contains("server.server_name"), "{error}");
    }
    assert!(!dir.path().join("ca.crt").exists(), "no CA was created");

    // An install from before the check: its CA carries the example name.
    icu_server::ca::Ca::load_or_create(dir.path(), &config.tls, "icu.example.org").unwrap();
    let handle = icu_server::run(config)
        .await
        .expect("an existing CA starts");
    assert!(matches!(
        handle.admin(AdminRequest::Status).await,
        AdminResponse::Status(_)
    ));
    // Nobody holds a certificate yet, so the first invitation would be the
    // first to carry the example `public_host`.
    let first = handle.admin(invite()).await;
    assert!(
        matches!(&first, AdminResponse::Error { detail, .. } if detail.contains("server.public_host")),
        "{first:?}"
    );
    handle.shutdown().await;

    // With users on record, invitations go on: their certificates already
    // carry the name.
    let dir = tempfile::tempdir().unwrap();
    let config = example(dir.path());
    icu_server::ca::Ca::load_or_create(dir.path(), &config.tls, "icu.example.org").unwrap();
    std::fs::write(
        dir.path().join("users.toml"),
        format!(
            "schema = 1\nnext_user_id = 2\ncrl_number = 1790000000\n\n\
             [user.1]\nname = \"alice\"\ncreated = \"2026-01-01T00:00:00Z\"\nis_admin = true\n\n\
             [[user.1.cert]]\nfingerprint = \"{:064x}\"\nissued = \"2026-01-01T00:00:00Z\"\n\
             expires = \"2099-01-01T00:00:00Z\"\nrevoked = false\n",
            1
        ),
    )
    .unwrap();
    // A state in use never gets a new com key: every passport carries the
    // old one (`PROTOCOL.md` §1).
    let err = icu_server::run(config.clone())
        .await
        .err()
        .expect("no start without the com key");
    assert!(err.to_string().contains("no com key"), "{err}");
    std::fs::write(
        dir.path().join(icu_server::store::COM_KEY_FILE),
        format!(
            "{}\n",
            icu_proto::com::ComKey::generate().unwrap().to_base64()
        ),
    )
    .unwrap();
    let handle = icu_server::run(config).await.expect("starts");
    let later = handle.admin(invite()).await;
    assert!(matches!(later, AdminResponse::Invite { .. }), "{later:?}");
    let AdminResponse::Users(users) = handle.admin(AdminRequest::Users).await else {
        panic!("users")
    };
    assert_eq!(users.len(), 2);
    handle.shutdown().await;

    // Every CLI command loads the configuration file first.
    let file = dir.path().join("server.toml");
    std::fs::write(
        &file,
        "[server]\nserver_name = \"icu.example.org\"\npublic_host = \"icu.example.org:4711\"\n",
    )
    .unwrap();
    icu_server::Config::load(&file).expect("the configuration loads");
}

/// Whether this process runs as root, which reads through every mode bit
/// the tests below set up.
fn root() -> bool {
    let root = rustix::process::geteuid().is_root();
    if root {
        eprintln!("skipped: root is not held back by file modes");
    }
    root
}

/// Sets a file's mode and puts the old one back when dropped, so the
/// temporary directory goes away even after a failed assertion.
struct Mode(std::path::PathBuf, u32);

impl Mode {
    fn set(path: &std::path::Path, mode: u32) -> Mode {
        use std::os::unix::fs::PermissionsExt;
        let old = std::fs::metadata(path).unwrap().permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        Mode(path.to_path_buf(), old)
    }
}

impl Drop for Mode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1));
    }
}

/// S1 (readiness review 2026-09-27): with the packaged configuration —
/// `server_name` unset — `status` and `users` need nothing but the admin
/// socket. Working the name out reads `ca.crt` in the state directory,
/// which under `DynamicUser` only root can; done up front, it refused every
/// command to members of the socket's group, Checkmk included.
#[tokio::test]
async fn status_and_users_need_only_the_socket() {
    if root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (state, run) = (dir.path().join("state"), dir.path().join("run"));
    let mut config = icu_server::Config::for_test(&state);
    config.paths.runtime_dir = run.clone();
    let handle = icu_server::run(config).await.expect("server");
    let file = dir.path().join("server.toml");
    std::fs::write(
        &file,
        format!(
            "[paths]\nstate_dir = {:?}\nruntime_dir = {:?}\n",
            state.to_str().unwrap(),
            run.to_str().unwrap()
        ),
    )
    .unwrap();
    // `/var/lib/private` as a member of the socket's group sees it.
    let hidden = Mode::set(&state, 0o000);
    for command in ["status", "users"] {
        let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_icu-server"))
            .arg("--config")
            .arg(&file)
            .args([command, "--json"])
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["ok"], serde_json::json!(true), "{command}");
    }
    drop(hidden);
    handle.shutdown().await;
}

/// The CLI does the work itself only when no server runs: socket missing or
/// refusing. A socket it may not use is reported as such, with the fix —
/// falling back hid it behind the state lock's "is a server running?".
#[tokio::test]
async fn a_socket_the_cli_may_not_use_is_reported_not_bypassed() {
    if root() {
        return;
    }
    let server = TestServer::start().await;
    let socket = server.socket();
    let locked = Mode::set(&socket, 0o000);
    let config = icu_server::Config::for_test(server.dir.path());
    let error = icu_server::cli::dispatch(&config, AdminRequest::Status)
        .await
        .expect_err("refused")
        .to_string();
    assert!(
        error.contains(&socket.display().to_string()) && error.contains("admin_socket_group"),
        "{error}"
    );
    assert!(!error.contains("cannot lock"), "no fallback: {error}");
    drop(locked);
    server.shutdown().await;
}

/// A stale socket file, as a crash leaves it, refuses connections: no
/// server, so the CLI does the work itself.
#[tokio::test]
async fn a_stale_socket_file_means_no_server() {
    let dir = tempfile::tempdir().unwrap();
    let config = icu_server::Config::for_test(dir.path());
    drop(std::os::unix::net::UnixListener::bind(config.admin_socket()).unwrap());
    assert!(config.admin_socket().exists(), "the file stays behind");
    let value = icu_server::cli::dispatch(&config, AdminRequest::Status)
        .await
        .expect("offline status");
    assert_eq!(value["ok"], serde_json::json!(true));
}

/// S2 (readiness review 2026-09-27): clients take the server name from the
/// CA's CN. A `server_name` other than that is refused — by `run` and the
/// offline CLI alike, before any server certificate is issued for it: the
/// next one would have locked every client out.
#[tokio::test]
async fn a_server_name_other_than_the_cas_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = icu_server::Config::for_test(dir.path());
    icu_server::ca::Ca::load_or_create(dir.path(), &config.tls, "icu.home.arpa").unwrap();
    let leaf = std::fs::read(dir.path().join("server.crt")).unwrap();
    // A leaf that needs repair would be the first thing issued.
    std::fs::remove_file(dir.path().join("server.key")).unwrap();
    config.server.server_name = "voice.home.arpa".into();
    let started = icu_server::run(config.clone()).await.err();
    let offline = icu_server::Server::offline(config.clone()).err();
    for error in [started, offline] {
        let error = error.expect("refused").to_string();
        assert!(
            error.contains("\"voice.home.arpa\"") && error.contains("\"icu.home.arpa\""),
            "{error}"
        );
    }
    assert_eq!(
        std::fs::read(dir.path().join("server.crt")).unwrap(),
        leaf,
        "nothing was issued"
    );
    // DNS names compare without regard to case, as clients check them.
    config.server.server_name = "ICU.Home.Arpa".into();
    let handle = icu_server::run(config)
        .await
        .expect("the CA's own name starts");
    handle.shutdown().await;
}

/// S5 (readiness review 2026-09-27): a name may be all digits. `revoke 1`
/// with a user called "1" around is refused instead of hitting user 1 —
/// often the owner; `revoke` is final. The explicit forms reach each of
/// them, and every answer names whom the selector hit.
#[tokio::test]
async fn an_ambiguous_selector_is_refused_and_answers_name_the_user() {
    let server = TestServer::start().await;
    server.enroll("owner").await;
    server.enroll("1").await;
    let revoke = |user: &str| AdminRequest::Revoke { user: user.into() };

    let refused = server.handle.admin(revoke("1")).await;
    let AdminResponse::Error { error, detail } = &refused else {
        panic!("{refused:?}")
    };
    assert_eq!(error, "AMBIGUOUS_USER");
    assert!(
        detail.contains("user_id 1 (\"owner\")") && detail.contains("user_id 2 (\"1\")"),
        "{detail}"
    );
    let AdminResponse::Users(users) = server.handle.admin(AdminRequest::Users).await else {
        panic!("users")
    };
    assert!(users.iter().all(|u| !u.revoked), "nobody was touched");

    let revoked = server.handle.admin(revoke("name:1")).await;
    assert!(
        matches!(&revoked, AdminResponse::Revoke { user_id: 2, name, .. } if name == "1"),
        "{revoked:?}"
    );
    let promoted = admin::request(
        &server.socket(),
        &AdminRequest::Promote {
            user: "id:1".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        promoted,
        serde_json::json!({"ok": true, "user_id": 1, "name": "owner"})
    );
    server.shutdown().await;
}

/// S6 (readiness review 2026-09-27): a `promote` and a `demote` at once
/// leave room and store agreeing, and the last `UserAdminChanged` the target
/// got says what the store holds. The pushes used to go out after the store
/// lock, so the loser of the race could overwrite the room's view. A stress
/// test: the race window is short, so a run without the fix only fails
/// sometimes; with it, never.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_role_changes_leave_room_and_store_agreeing() {
    let server = TestServer::start().await;
    let _alice = server.client("alice").await;
    let mut bob = server.client("bob").await;
    let shared = server.handle.server().clone();
    for _ in 0..100 {
        let tasks = [true, false].map(|promote| {
            let shared = shared.clone();
            tokio::spawn(async move {
                let user = "bob".to_string();
                let request = if promote {
                    AdminRequest::Promote { user }
                } else {
                    AdminRequest::Demote { user }
                };
                admin::handle(&shared, request).await
            })
        });
        for task in tasks {
            task.await.unwrap();
        }
        let stored = shared.store.lock().await.is_admin(bob.user_id);
        let shown = shared.room.flags(bob.session).expect("bob").is_admin();
        assert_eq!(stored, shown, "room and store disagree");
    }
    let stored = shared.store.lock().await.is_admin(bob.user_id);
    let mut last = None;
    while let Some(msg) = bob.recv_timeout(common::BRIEFLY).await {
        if let ControlMsg::UserAdminChanged { is_admin } = msg {
            last = Some(is_admin);
        }
    }
    assert_eq!(last, Some(stored), "bob's last word is the store's");
    server.shutdown().await;
}
