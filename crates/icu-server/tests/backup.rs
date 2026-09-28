//! Automatic state snapshots and restoring them (`docs/reference/server.md`
//! §4, §5 `backup`/`restore`; runbook "Backup und Restore" in
//! `deploy/README.md`).

mod common;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use common::TestServer;
use icu_proto::ControlMsg;
use icu_server::{backup, config::Config, AdminRequest, AdminResponse};

/// Snapshots on, settling quickly.
fn on(config: &mut Config) {
    config.backup.keep = 10;
    config.backup.settle_ms = 50;
}

fn snapshots(server: &TestServer) -> Vec<PathBuf> {
    backup::list(&server.dir.path().join("backups")).unwrap()
}

/// Waits until `f` holds, for at most five seconds.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Long enough for a change to have been picked up, had there been one.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

fn manifest(path: &Path) -> backup::Manifest {
    backup::read_manifest(path).unwrap()
}

/// One at the start, one per change, none for a restart that changes
/// nothing but the CRL number; a rotation and a CA renewal are changes too,
/// each in one snapshot.
#[tokio::test]
async fn the_server_backs_up_at_start_and_after_every_change() {
    let server = TestServer::start_with(on).await;
    until("the start snapshot", || snapshots(&server).len() == 1).await;
    assert_eq!(manifest(&snapshots(&server)[0]).reason, "start");

    server.enroll("alice").await;
    until("a snapshot with alice", || {
        snapshots(&server)
            .last()
            .is_some_and(|s| manifest(s).users == 1 && manifest(s).reason == "change")
    })
    .await;
    settle().await;
    let before = snapshots(&server).len();

    let server = server.restart_with(on).await;
    settle().await;
    assert_eq!(snapshots(&server).len(), before, "a restart is no change");

    assert_eq!(
        server.handle.admin(AdminRequest::RotateServerCert).await,
        AdminResponse::Ok
    );
    until("the rotation's snapshot", || {
        snapshots(&server).len() == before + 1
    })
    .await;
    settle().await;
    assert_eq!(
        snapshots(&server).len(),
        before + 1,
        "one rotation, one snapshot"
    );
    let newest = snapshots(&server).pop().unwrap();
    assert_eq!(
        std::fs::read(newest.join("server.crt")).unwrap(),
        std::fs::read(server.dir.path().join("server.crt")).unwrap()
    );

    assert_eq!(
        server.handle.admin(AdminRequest::RenewCa).await,
        AdminResponse::Ok
    );
    until("the renewal's snapshot", || {
        snapshots(&server).len() == before + 2
    })
    .await;
    let newest = snapshots(&server).pop().unwrap();
    assert_eq!(
        std::fs::read(newest.join("ca.crt")).unwrap(),
        std::fs::read(server.dir.path().join("ca.crt")).unwrap()
    );
    let status = server.handle.admin(AdminRequest::Status).await.to_json();
    assert_eq!(status["backup"]["failed"], serde_json::json!(0));
    assert!(status["backup"]["last_ok_unix"].as_u64().unwrap() > 0);
    server.shutdown().await;
}

/// `backup.keep` bounds the snapshots; the newest stay.
#[tokio::test]
async fn keep_bounds_the_snapshots() {
    let server = TestServer::start_with(|c| {
        on(c);
        c.backup.keep = 3;
    })
    .await;
    for n in 1..=5 {
        server.invite("").await;
        until("the invite's snapshot", || {
            snapshots(&server)
                .last()
                .is_some_and(|s| manifest(s).users == n)
        })
        .await;
    }
    let kept = snapshots(&server);
    assert_eq!(kept.len(), 3);
    assert_eq!(manifest(&kept[0]).users, 3);
    server.shutdown().await;
}

/// A change still settling when the server stops is backed up on the way
/// out.
#[tokio::test]
async fn a_pending_change_is_backed_up_at_shutdown() {
    let server = TestServer::start_with(|c| {
        on(c);
        c.backup.settle_ms = 60_000;
        c.timing.shutdown_grace_ms = 1000;
    })
    .await;
    until("the start snapshot", || snapshots(&server).len() == 1).await;
    server.invite("").await;
    let backups = server.dir.path().join("backups");
    let dir = server.dir.path().to_path_buf();
    server.handle.shutdown().await;
    let kept = backup::list(&backups).unwrap();
    assert_eq!(kept.len(), 2, "{dir:?}");
    assert_eq!(manifest(&kept[1]).reason, "shutdown");
    assert_eq!(manifest(&kept[1]).users, 1);
}

/// A backup that cannot be written is reported in `status`; the server
/// serves on.
#[tokio::test]
async fn a_failing_backup_is_reported_and_the_server_serves_on() {
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("not-a-directory");
    std::fs::write(&blocked, "").unwrap();
    let server = TestServer::start_with(|c| {
        on(c);
        c.backup.dir = Some(blocked.join("backups"));
    })
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let status = server.handle.admin(AdminRequest::Status).await.to_json();
        if status["backup"]["failed"] == serde_json::json!(1) {
            assert!(!status["backup"]["error"].as_str().unwrap().is_empty());
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no failure in status"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let alice = server.enroll("alice").await;
    let mut client = server.connect(&alice).await.expect("connect");
    assert!(matches!(
        client.hello("alice").await,
        ControlMsg::Welcome { .. }
    ));
    assert!(matches!(
        server.handle.admin(AdminRequest::Backup).await,
        AdminResponse::Error { .. }
    ));
    server.shutdown().await;
}

/// The offline CLI backs up the change it made itself: no server runs to
/// notice it.
#[tokio::test]
async fn the_offline_cli_backs_up_its_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::for_test(dir.path());
    on(&mut config);
    let value = icu_server::cli::dispatch(
        &config,
        AdminRequest::Invite {
            name: String::new(),
            ttl_s: Some(600),
        },
    )
    .await
    .unwrap();
    assert_eq!(value["ok"], serde_json::json!(true));
    let kept = backup::list(&config.backup_dir()).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(manifest(&kept[0]).users, 1);

    let value = icu_server::cli::dispatch(&config, AdminRequest::Backup)
        .await
        .unwrap();
    let path = PathBuf::from(value["path"].as_str().unwrap());
    assert_eq!(manifest(&path).reason, "manual");
    assert_eq!(backup::list(&config.backup_dir()).unwrap().len(), 2);
}

/// A revocation made by mistake is undone by restoring the snapshot from
/// before it; the state it replaced is kept, and the server leaf is new.
#[tokio::test]
async fn restore_brings_back_the_state_before_a_mistake() {
    let server = TestServer::start_with(on).await;
    server.enroll("alice").await;
    let bob = server.enroll("bob").await;
    until("a snapshot with bob", || {
        snapshots(&server)
            .last()
            .is_some_and(|s| manifest(s).users == 2)
    })
    .await;
    settle().await;
    let good = snapshots(&server).pop().unwrap();
    let revoke = server
        .handle
        .admin(AdminRequest::Revoke { user: "bob".into() })
        .await;
    assert!(matches!(revoke, AdminResponse::Revoke { .. }), "{revoke:?}");
    until("the revocation's snapshot", || {
        snapshots(&server).last() != Some(&good)
    })
    .await;
    let leaf = std::fs::read(server.dir.path().join("server.crt")).unwrap();

    let mut config = Config::for_test(server.dir.path());
    on(&mut config);
    let err = backup::restore(&config, good.to_str().unwrap())
        .await
        .expect_err("the server runs");
    assert!(err.to_string().contains("stop the server first"), "{err}");

    let TestServer { dir, handle } = server;
    handle.shutdown().await;
    let restored = backup::restore(&config, good.to_str().unwrap())
        .await
        .expect("restore");
    assert_eq!(restored.users, 2);
    let pre = restored.pre_restore.expect("the replaced state is kept");
    assert_eq!(manifest(&pre).reason, "pre-restore");
    assert_ne!(
        std::fs::read(dir.path().join("server.crt")).unwrap(),
        leaf,
        "a new server leaf after every restore"
    );
    assert_eq!(
        manifest(&backup::latest(&dir.path().join("backups")).unwrap()).reason,
        "restore"
    );

    let handle = icu_server::run(config).await.expect("server starts");
    let server = TestServer { dir, handle };
    let mut client = server.connect(&bob).await.expect("bob is back");
    assert!(matches!(
        client.hello("bob").await,
        ControlMsg::Welcome { .. }
    ));
    server.shutdown().await;
}

/// Disaster recovery: a snapshot copied off the host brings the server back
/// on a new one with an empty state directory — same CA, same com key, the
/// same users.
#[tokio::test]
async fn a_snapshot_restores_onto_a_new_host() {
    let old = TestServer::start_with(on).await;
    let alice = old.enroll("alice").await;
    until("a snapshot with alice", || {
        snapshots(&old)
            .last()
            .is_some_and(|s| manifest(s).users == 1)
    })
    .await;
    settle().await;
    let offsite = tempfile::tempdir().unwrap();
    let copy = offsite.path().join("snapshot");
    std::fs::create_dir_all(&copy).unwrap();
    for entry in std::fs::read_dir(snapshots(&old).pop().unwrap()).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
    }
    old.shutdown().await;

    let new = tempfile::tempdir().unwrap();
    let state = new.path().join("state");
    let mut config = Config::for_test(&state);
    on(&mut config);
    let restored = backup::restore(&config, copy.to_str().unwrap())
        .await
        .expect("restore on a new host");
    assert!(restored.pre_restore.is_none());
    let handle = icu_server::run(config).await.expect("server starts");
    let server = TestServer { dir: new, handle };
    let mut client = server.connect(&alice).await.expect("alice connects");
    assert!(matches!(
        client.hello("alice").await,
        ControlMsg::Welcome { .. }
    ));
    server.shutdown().await;
}

/// A snapshot that does not load the way the server would load it replaces
/// nothing.
#[tokio::test]
async fn a_broken_snapshot_replaces_nothing() {
    let server = TestServer::start_with(on).await;
    until("the start snapshot", || snapshots(&server).len() == 1).await;
    let snapshot = snapshots(&server).pop().unwrap();
    let TestServer { dir, handle } = server;
    handle.shutdown().await;
    let mut config = Config::for_test(dir.path());
    on(&mut config);
    let users = std::fs::read(dir.path().join("users.toml")).unwrap();

    // A copy by hand, without a manifest, whose CA pair does not match.
    let copy = dir.path().join("copy");
    std::fs::create_dir_all(&copy).unwrap();
    for name in ["ca.crt", "com.key", "users.toml"] {
        std::fs::copy(snapshot.join(name), copy.join(name)).unwrap();
    }
    std::fs::write(
        copy.join("ca.key"),
        rcgen::KeyPair::generate().unwrap().serialize_pem(),
    )
    .unwrap();
    let err = backup::restore(&config, copy.to_str().unwrap())
        .await
        .expect_err("not a pair");
    assert!(err.to_string().contains("does not belong"), "{err}");
    assert_eq!(std::fs::read(dir.path().join("users.toml")).unwrap(), users);
    assert_eq!(
        backup::list(&config.backup_dir()).unwrap().len(),
        1,
        "not even a pre-restore snapshot"
    );
}
