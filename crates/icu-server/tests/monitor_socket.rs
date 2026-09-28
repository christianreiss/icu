//! The read-only monitoring socket (`docs/reference/server.md` §5).
//!
//! Until 2026-09-28 a monitor had to be in the admin socket's group, which
//! may invite, disable and revoke. The second socket answers `status` and
//! refuses the rest, so a Checkmk agent needs none of that.

mod common;

use common::TestServer;
use icu_server::{admin, config::Config, AdminRequest};

/// The gid of the test's own directory: a group this process really is in,
/// so the `chgrp` of the socket succeeds instead of only warning.
fn own_group(dir: &std::path::Path) -> String {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir)
        .expect("stat tempdir")
        .gid()
        .to_string()
}

/// Starts a server whose monitoring socket belongs to a group we are in.
async fn with_monitor() -> TestServer {
    let probe = tempfile::tempdir().expect("tempdir");
    let group = own_group(probe.path());
    TestServer::start_with(move |config: &mut Config| {
        config.server.monitor_socket_group = Some(group);
    })
    .await
}

#[tokio::test]
async fn status_over_the_monitoring_socket_says_what_the_admin_socket_says() {
    let server = with_monitor().await;
    let monitor = server
        .handle
        .monitor_socket()
        .expect("the monitoring socket is up")
        .to_path_buf();
    assert!(monitor.ends_with("monitor.sock"), "{}", monitor.display());

    // 0660: the group is the gate, nothing else.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&monitor).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o660, "{mode:o}");

    let over_monitor = admin::request(&monitor, &AdminRequest::Status)
        .await
        .expect("status over the monitoring socket");
    let over_admin = admin::request(&server.socket(), &AdminRequest::Status)
        .await
        .expect("status over the admin socket");

    assert_eq!(over_monitor["ok"], serde_json::json!(true));
    for key in [
        "online",
        "max_users",
        "version",
        "limits",
        "relay",
        "enroll",
    ] {
        assert_eq!(
            over_monitor[key], over_admin[key],
            "{key} differs between the sockets"
        );
    }
    // The ceilings the check needs for its thresholds are really there.
    assert!(over_monitor["limits"]["max_connections"].as_u64().unwrap() > 0);
    assert!(over_monitor["limits"]["max_handshakes"].as_u64().unwrap() > 0);
    server.shutdown().await;
}

#[tokio::test]
async fn the_monitoring_socket_refuses_everything_but_status() {
    let server = with_monitor().await;
    let monitor = server.handle.monitor_socket().unwrap().to_path_buf();
    let alice = server.enroll("alice").await;
    let _ = alice;

    for request in [
        AdminRequest::Users,
        AdminRequest::Invite {
            name: "mallory".into(),
            ttl_s: Some(3600),
        },
        AdminRequest::Revoke {
            user: "alice".into(),
        },
        AdminRequest::Backup,
        AdminRequest::RenewCa,
    ] {
        let response = admin::request(&monitor, &request)
            .await
            .unwrap_or_else(|e| panic!("{request:?}: {e}"));
        assert_eq!(response["ok"], serde_json::json!(false), "{request:?}");
        assert_eq!(
            response["error"],
            serde_json::json!("FORBIDDEN"),
            "{request:?}"
        );
    }

    // Nothing of that happened: alice is still there, unrevoked, and no
    // second user was invited.
    let users = admin::request(&server.socket(), &AdminRequest::Users)
        .await
        .expect("users");
    let users = users["users"].as_array().expect("users array");
    assert_eq!(users.len(), 1, "{users:?}");
    assert_eq!(users[0]["name"], serde_json::json!("alice"));
    assert_eq!(users[0]["revoked"], serde_json::json!(false));
    server.shutdown().await;
}

#[tokio::test]
async fn without_a_group_there_is_no_monitoring_socket() {
    let server = TestServer::start().await;
    assert!(server.handle.monitor_socket().is_none());
    assert!(!server.handle.server().config.monitor_socket().exists());
    server.shutdown().await;
}
