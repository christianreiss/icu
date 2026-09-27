//! Tests that start child processes: the `icu-server` binary for `status`
//! and `users` through the socket alone and for `backup` and `restore` on a
//! stopped server (`Server/IMPLEMENTATION.md` §5), and `getent` for the
//! admin socket's group.
//!
//! In a test binary of their own, and one at a time, on purpose. `flock`
//! belongs to the open file description, and a child forked while another
//! test holds a state lock shares that description until its `exec` closes
//! it. A test that released its lock and took it again straight away — the
//! offline CLI, then `run` on the same state — found it still held by a
//! stranger's child now and then: "cannot lock state.lock (is a server
//! running?)" in `tests/admin.rs`, 7 of 40 runs with the spawning test
//! beside it, none without.

mod common;

use common::TestServer;

/// Held by every test here, so none forks while another holds a lock.
static ALONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// S1 (readiness review 2026-09-27): with the packaged configuration —
/// `server_name` unset — `status` and `users` need nothing but the admin
/// socket. Working the name out reads `ca.crt` in the state directory,
/// which under `DynamicUser` only root can; done up front, it refused every
/// command to members of the socket's group, Checkmk included.
#[tokio::test]
async fn status_and_users_need_only_the_socket() {
    let _alone = ALONE.lock().await;
    if common::root() {
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
    let hidden = common::Mode::set(&state, 0o000);
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

/// The binary: `backup` offline, `backup --list`, `restore latest`.
#[tokio::test]
async fn the_cli_backs_up_lists_and_restores() {
    let _alone = ALONE.lock().await;
    let server = TestServer::start_with(|c| {
        c.backup.keep = 10;
        c.backup.settle_ms = 50;
    })
    .await;
    server.enroll("alice").await;
    let TestServer { dir, handle } = server;
    handle.shutdown().await;
    let file = dir.path().join("server.toml");
    std::fs::write(
        &file,
        format!(
            "[paths]\nstate_dir = {:?}\nruntime_dir = {:?}\n\n[backup]\nkeep = 10\n",
            dir.path().to_str().unwrap(),
            dir.path().join("run").to_str().unwrap()
        ),
    )
    .unwrap();
    let cli = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_icu-server"))
            .arg("--config")
            .arg(&file)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    assert!(cli(&["backup"]).starts_with("state backed up to "));
    let list = cli(&["backup", "--list"]);
    assert!(list.contains("manual"), "{list}");
    let out = cli(&["restore", "latest"]);
    assert!(out.contains("1 user(s) on record"), "{out}");
    assert!(
        out.contains("pre-restore") || out.contains("replaced"),
        "{out}"
    );
}

/// External review 2026-09-27: the admin socket's group is looked up in
/// `/etc/group` and then through the host's NSS (`getent`), so a group only
/// FreeIPA/SSSD knows works too; it used to be ignored with a warning.
#[tokio::test]
async fn the_socket_group_is_asked_of_the_hosts_nss() {
    let _alone = ALONE.lock().await;
    let err = icu_server::admin::resolve_group("no-such-group-icu-test")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no-such-group-icu-test"), "{err}");
    match icu_server::admin::getent_gid("root") {
        Ok(gid) => assert_eq!(gid, 0),
        Err(e) if e.to_string().contains("no getent") => eprintln!("skipped: {e}"),
        Err(e) => panic!("{e}"),
    }
}
