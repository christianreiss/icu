//! The Checkmk local check `deploy/checkmk/icu_server` against a stand-in
//! for `icu-server status --json` (readiness review 2026-09-27, S8): values
//! for right now are graphed as they are, counters become rates against the
//! previous run, and a large share of video dropped for slow viewers turns
//! WARN only when it lasts.
//!
//! Since 2026-09-28 it also reads the ceilings under `limits`, the drops
//! (`com_dropped`, `relay.dropped.*`) and `enroll`, so a server running out
//! of connections or under a registration brute force is no longer a green
//! service with a nice graph.

use std::{path::Path, process::Command};

const SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../deploy/checkmk/icu_server"
);

/// Fields in the check's state file. One run without rates is the price of
/// a file from an older version; the check must not read it as a baseline.
const STATE_FIELDS: usize = 15;

/// A `status --json` answer with the given uptime and counters, with
/// everything else at a healthy zero.
fn base(
    uptime: u64,
    connections: u64,
    datagrams: u64,
    video_out: u64,
    slow: u64,
) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "uptime_s": uptime,
        "online": 3,
        "max_users": 16,
        "version": "0.1.0",
        "limits": {"max_connections": 256, "max_handshakes": 256},
        "connections": connections,
        "open_connections": 4,
        "handshakes": 1,
        "com_dropped": 0,
        "roomless_sessions": 0,
        "tls": {"server_cert_days_left": 200, "ca_days_left": 3000},
        "relay": {
            "datagrams_in": datagrams,
            "datagrams_out": 2 * datagrams,
            "video_frames_in": video_out,
            "video_frames_out": video_out,
            "control_in": 0,
            "dropped": {
                "malformed": 0,
                "rate": 0,
                "control_rate_limited": 0,
                "video_rate": 0,
                "video_slow_receiver": slow,
            },
        },
        "enroll": {"attempts": 0, "ok": 0, "failed": 0},
    })
}

fn status(uptime: u64, connections: u64, datagrams: u64, video_out: u64, slow: u64) -> String {
    base(uptime, connections, datagrams, video_out, slow).to_string()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A state file: the sample a minute ago, with the three streaks at the end.
fn previous(counters: [u64; 12], streaks: [u64; 3]) -> String {
    let mut fields: Vec<String> = counters.iter().map(u64::to_string).collect();
    fields.extend(streaks.iter().map(u64::to_string));
    assert_eq!(fields.len(), STATE_FIELDS);
    format!("{}\n", fields.join(" "))
}

/// The stand-in server and its temporary directory.
fn stand_in() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("icu-server");
    std::fs::write(&bin, "#!/bin/sh\ncat \"$ICU_TEST_STATUS\"\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Leaked on purpose: the check only tests that the socket file is there.
    let listener = std::os::unix::net::UnixListener::bind(dir.path().join("admin.sock")).unwrap();
    std::mem::forget(listener);
    dir
}

/// Runs the check with `json` as the server's answer; returns its lines.
fn check(dir: &Path, json: &str) -> Vec<String> {
    check_env(dir, json, &[])
}

fn check_env(dir: &Path, json: &str, env: &[(&str, &str)]) -> Vec<String> {
    std::fs::write(dir.join("status.json"), json).unwrap();
    let mut command = Command::new("sh");
    command
        .arg(SCRIPT)
        .env("ICU_SERVER_BIN", dir.join("icu-server"))
        .env("ICU_ADMIN_SOCKET", dir.join("admin.sock"))
        .env("ICU_CHECK_STATE", dir.join("state"))
        .env("ICU_TEST_STATUS", dir.join("status.json"));
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command.output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// One metric of a check line, as it stands in the third field: the value
/// and, where there is one, its threshold.
fn entry<'a>(line: &'a str, name: &str) -> &'a str {
    let metrics = line.split(' ').nth(2).unwrap_or("-");
    metrics
        .split('|')
        .find_map(|e| e.strip_prefix(name)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("no {name} in {line}"))
}

/// Asserts a rate against a sample a minute ago. Not to the last digit: the
/// check reads the clock itself, and on a loaded machine it measures 61 s
/// where the test meant 60, which moves every rate by a percent or two.
#[track_caller]
fn about(line: &str, name: &str, expected: f64) {
    let got: f64 = entry(line, name)
        .split(';')
        .next()
        .unwrap()
        .parse()
        .unwrap_or_else(|_| panic!("{name} is no number in {line}"));
    assert!(
        (got - expected).abs() <= expected.abs() * 0.05 + 0.01,
        "{name}: expected about {expected}, got {got} in {line}"
    );
}

fn line<'a>(lines: &'a [String], item: &str) -> &'a str {
    lines
        .iter()
        .find(|l| l.split(' ').nth(1) == Some(item))
        .unwrap_or_else(|| panic!("no {item} in {lines:?}"))
}

#[test]
fn gauges_rates_and_a_lasting_slow_viewer() {
    let dir = stand_in();
    let dir = dir.path();
    let state = dir.join("state");

    // First run: the values for now, no rates yet, a baseline written.
    let lines = check(dir, &status(100, 10, 1000, 500, 0));
    let server = line(&lines, "ICU_Server");
    assert!(server.starts_with("0 "), "{server}");
    assert!(server.contains("online=3;;;0;16|uptime=100|"), "{server}");
    assert!(
        server.contains(
            "|open_connections=4;204;243;0;256|handshakes=1;204;243;0;256|roomless_sessions=0 "
        ),
        "{server}"
    );
    assert!(server.contains("version 0.1.0"), "{server}");
    assert!(
        !server.contains("connections=10"),
        "a counter is no gauge: {server}"
    );
    assert!(!server.contains("_per_s"), "{server}");
    for item in ["ICU_Video", "ICU_Relay", "ICU_Enroll"] {
        let l = line(&lines, item);
        assert!(l.starts_with(&format!("0 {item} - ")), "{l}");
    }
    assert!(line(&lines, "ICU_Server_Certificate").starts_with("0 "));
    let written = std::fs::read_to_string(&state).unwrap();
    assert_eq!(
        written.split_whitespace().count(),
        STATE_FIELDS,
        "{written}"
    );

    // A minute later: rates. 100 of 1000 video frames dropped for slow
    // viewers is 10 %, at the threshold, but only once.
    std::fs::write(
        &state,
        previous(
            [now() - 60, 100, 10, 1000, 2000, 500, 0, 0, 0, 0, 0, 0],
            [0, 0, 0],
        ),
    )
    .unwrap();
    let lines = check(dir, &status(160, 16, 4000, 1400, 100));
    let server = line(&lines, "ICU_Server");
    about(server, "connects_per_s", 0.10);
    about(server, "datagrams_in_per_s", 50.0);
    about(server, "datagrams_out_per_s", 100.0);
    let video = line(&lines, "ICU_Video");
    assert!(video.starts_with("0 ICU_Video "), "{video}");
    about(video, "video_out_per_s", 15.0);
    about(video, "video_slow_drops_per_s", 1.67);
    // A share, not a rate: the clock does not move it.
    assert_eq!(entry(video, "video_slow_pct"), "10;10", "{video}");
    assert!(std::fs::read_to_string(&state)
        .unwrap()
        .ends_with(" 1 0 0\n"));

    // The third run in a row with that share: WARN.
    std::fs::write(
        &state,
        previous(
            [now() - 60, 100, 10, 1000, 2000, 500, 0, 0, 0, 0, 0, 0],
            [2, 0, 0],
        ),
    )
    .unwrap();
    let lines = check(dir, &status(160, 16, 4000, 1400, 100));
    assert!(
        line(&lines, "ICU_Video").starts_with("1 ICU_Video "),
        "{lines:?}"
    );

    // A share below the threshold ends the streak.
    std::fs::write(
        &state,
        previous(
            [now() - 60, 100, 10, 1000, 2000, 500, 0, 0, 0, 0, 0, 0],
            [2, 0, 0],
        ),
    )
    .unwrap();
    let lines = check(dir, &status(160, 16, 4000, 1400, 50));
    assert!(
        line(&lines, "ICU_Video").starts_with("0 ICU_Video "),
        "{lines:?}"
    );
    assert!(std::fs::read_to_string(&state)
        .unwrap()
        .ends_with(" 0 0 0\n"));

    // A restart resets the counters: no rates from that run.
    std::fs::write(
        &state,
        previous(
            [now() - 60, 100, 10, 1000, 2000, 500, 0, 0, 0, 0, 0, 0],
            [2, 0, 0],
        ),
    )
    .unwrap();
    let lines = check(dir, &status(5, 1, 10, 0, 0));
    assert!(!line(&lines, "ICU_Server").contains("_per_s"), "{lines:?}");
    assert!(line(&lines, "ICU_Video").starts_with("0 ICU_Video - "));

    // A garbled state file is no baseline either, and neither is one from
    // the older, eight-field version of this check.
    for stale in [
        "garbage\n".to_string(),
        format!("{} 100 10 1000 2000 500 0 0\n", now() - 60),
    ] {
        std::fs::write(&state, stale).unwrap();
        let lines = check(dir, &status(160, 16, 4000, 1400, 100));
        assert!(!line(&lines, "ICU_Server").contains("_per_s"), "{lines:?}");
    }
}

/// The ceilings from `limits`: WARN at 80 %, CRIT at 95 % of the connections
/// or handshakes the server will hold. A server from before `limits` reports
/// none and simply gets no threshold.
#[test]
fn connections_close_to_the_limit_warn() {
    let dir = stand_in();
    let dir = dir.path();

    let with = |open: u64, handshakes: u64| {
        let mut value = base(100, 10, 1000, 500, 0);
        value["open_connections"] = open.into();
        value["handshakes"] = handshakes.into();
        value.to_string()
    };

    let lines = check(dir, &with(200, 1));
    assert!(line(&lines, "ICU_Server").starts_with("0 "), "{lines:?}");

    let lines = check(dir, &with(210, 1));
    let server = line(&lines, "ICU_Server");
    assert!(
        server.starts_with("1 ICU_Server ") && server.contains("210 of 256 connections in use"),
        "{server}"
    );

    let lines = check(dir, &with(4, 250));
    let server = line(&lines, "ICU_Server");
    assert!(
        server.starts_with("2 ICU_Server ") && server.contains("250 of 256 handshakes in use"),
        "{server}"
    );

    // Thresholds are configurable.
    std::fs::write(dir.join("status.json"), with(210, 1)).unwrap();
    let lines = check_env(dir, &with(210, 1), &[("ICU_CONN_WARN_PCT", "90")]);
    assert!(line(&lines, "ICU_Server").starts_with("0 "), "{lines:?}");

    // An older server without `limits`: graphed, never judged.
    let mut old = base(100, 10, 1000, 500, 0);
    old.as_object_mut().unwrap().remove("limits");
    old["open_connections"] = 250.into();
    let lines = check(dir, &old.to_string());
    let server = line(&lines, "ICU_Server");
    assert!(server.starts_with("0 "), "{server}");
    assert!(
        server.contains("|open_connections=250|"),
        "no ceiling, no thresholds: {server}"
    );
}

/// Drops: datagrams without a com tag are port noise and only graphed, the
/// ones a rate limit caused turn WARN when they last.
#[test]
fn abuse_drops_warn_only_when_they_last() {
    let dir = stand_in();
    let dir = dir.path();
    let state = dir.join("state");

    let with = |com: u64, rate: u64| {
        let mut value = base(160, 16, 4000, 1400, 0);
        value["com_dropped"] = com.into();
        value["relay"]["dropped"]["rate"] = rate.into();
        value.to_string()
    };

    // 60 datagrams in a minute is 1/s, far below the 20/s threshold; the
    // 600 com drops (10/s) are never judged at all.
    std::fs::write(
        &state,
        previous(
            [now() - 60, 100, 10, 1000, 2000, 500, 0, 0, 0, 0, 0, 0],
            [0, 2, 0],
        ),
    )
    .unwrap();
    let lines = check(dir, &with(600, 60));
    let relay = line(&lines, "ICU_Relay");
    assert!(relay.starts_with("0 ICU_Relay "), "{relay}");
    about(relay, "com_dropped_per_s", 10.0);
    about(relay, "abuse_drops_per_s", 1.0);
    assert!(
        entry(relay, "abuse_drops_per_s").ends_with(";20"),
        "{relay}"
    );
    assert!(std::fs::read_to_string(&state)
        .unwrap()
        .ends_with(" 0 0 0\n"));

    // 1800 in a minute is 30/s, and it is the third run in a row.
    std::fs::write(
        &state,
        previous(
            [now() - 60, 100, 10, 1000, 2000, 500, 0, 0, 0, 0, 0, 0],
            [0, 2, 0],
        ),
    )
    .unwrap();
    let lines = check(dir, &with(0, 1800));
    let relay = line(&lines, "ICU_Relay");
    assert!(
        relay.starts_with("1 ICU_Relay ") && relay.contains("for 3 runs in a row"),
        "{relay}"
    );

    // The same rate on the first of those runs is not yet a WARN.
    std::fs::write(
        &state,
        previous(
            [now() - 60, 100, 10, 1000, 2000, 500, 0, 0, 0, 0, 0, 0],
            [0, 0, 0],
        ),
    )
    .unwrap();
    assert!(
        line(&check(dir, &with(0, 1800)), "ICU_Relay").starts_with("0 "),
        "one run is no streak"
    );
}

/// Failed registrations that keep coming: a brute force against bundles, or
/// a client rollout that cannot register.
#[test]
fn lasting_enrollment_failures_warn() {
    let dir = stand_in();
    let dir = dir.path();
    let state = dir.join("state");

    let with = |attempts: u64, failed: u64| {
        let mut value = base(160, 16, 4000, 1400, 0);
        value["enroll"] = serde_json::json!({"attempts": attempts, "ok": 1, "failed": failed});
        value.to_string()
    };

    // 120 failures in a minute is 2/s, over the threshold, third run.
    std::fs::write(
        &state,
        previous(
            [now() - 60, 100, 10, 1000, 2000, 500, 0, 0, 0, 0, 0, 0],
            [0, 0, 2],
        ),
    )
    .unwrap();
    let lines = check(dir, &with(130, 120));
    let enroll = line(&lines, "ICU_Enroll");
    assert!(enroll.starts_with("1 ICU_Enroll "), "{enroll}");
    about(enroll, "enroll_per_s", 2.17);
    about(enroll, "enroll_failed_per_s", 2.0);
    assert!(
        entry(enroll, "enroll_failed_per_s").ends_with(";1"),
        "{enroll}"
    );
    // `"ok"` must come from `enroll`, not from the `"ok":true` of the answer.
    assert!(
        enroll.contains("(1 ok, 120 failed since start)"),
        "{enroll}"
    );

    // A quiet minute ends the streak.
    std::fs::write(
        &state,
        previous(
            [now() - 60, 100, 10, 1000, 2000, 500, 0, 0, 0, 0, 0, 0],
            [0, 0, 2],
        ),
    )
    .unwrap();
    let lines = check(dir, &with(1, 0));
    assert!(line(&lines, "ICU_Enroll").starts_with("0 "), "{lines:?}");
    assert!(std::fs::read_to_string(&state)
        .unwrap()
        .ends_with(" 0 0 0\n"));
}

/// A failed state backup is a WARN with the reason; backups turned off are
/// said so; one that never succeeded is a WARN of its own; a server from
/// before the backups gets no line.
#[test]
fn a_failed_backup_warns() {
    let dir = stand_in();
    let dir = dir.path();
    let with_backup = |backup: serde_json::Value| {
        let mut value = base(100, 10, 1000, 500, 0);
        value["backup"] = backup;
        value.to_string()
    };

    let lines = check(dir, &status(100, 10, 1000, 500, 0));
    assert!(!lines.iter().any(|l| l.contains("ICU_Backup")), "{lines:?}");

    let ok = serde_json::json!({"keep": 30, "failed": 0, "last_ok_unix": now(), "error": ""});
    let lines = check(dir, &with_backup(ok));
    assert!(line(&lines, "ICU_Backup").starts_with("0 "), "{lines:?}");

    let failed = serde_json::json!({
        "keep": 30,
        "failed": 1,
        "last_ok_unix": 0,
        "error": "cannot create /srv/icu-backups: Permission denied",
    });
    let lines = check(dir, &with_backup(failed));
    let backup = line(&lines, "ICU_Backup");
    assert!(
        backup.starts_with("1 ICU_Backup - ") && backup.contains("Permission denied"),
        "{backup}"
    );

    let off = serde_json::json!({"keep": 0, "failed": 0, "last_ok_unix": 0, "error": ""});
    let lines = check(dir, &with_backup(off));
    assert!(line(&lines, "ICU_Backup").contains("off"), "{lines:?}");

    // Never confirmed, and the server has been up long enough for the one
    // at the start to have happened.
    let mut never = base(1000, 10, 1000, 500, 0);
    never["backup"] = serde_json::json!({"keep": 30, "failed": 0, "last_ok_unix": 0, "error": ""});
    let lines = check(dir, &never.to_string());
    let backup = line(&lines, "ICU_Backup");
    assert!(
        backup.starts_with("1 ICU_Backup - no state backup has ever been confirmed"),
        "{backup}"
    );

    // Right after the start it is not a finding yet.
    let mut fresh = base(3, 10, 1000, 500, 0);
    fresh["backup"] = serde_json::json!({"keep": 30, "failed": 0, "last_ok_unix": 0, "error": ""});
    let lines = check(dir, &fresh.to_string());
    assert!(line(&lines, "ICU_Backup").starts_with("0 "), "{lines:?}");
}

/// S-6 (external review 2026-09-27): a server that does not answer turns
/// the check CRIT after `ICU_CHECK_TIMEOUT` seconds instead of hanging the
/// agent with it.
#[test]
fn a_hanging_server_is_crit_after_the_timeout() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("icu-server");
    std::fs::write(&bin, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(dir.path().join("admin.sock")).unwrap();
    let started = std::time::Instant::now();
    let out = Command::new("sh")
        .arg(SCRIPT)
        .env("ICU_SERVER_BIN", &bin)
        .env("ICU_ADMIN_SOCKET", dir.path().join("admin.sock"))
        .env("ICU_CHECK_STATE", dir.path().join("state"))
        .env("ICU_CHECK_TIMEOUT", "1")
        .output()
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.starts_with("2 ICU_Server - icu-server status did not answer within 1s"),
        "{text}"
    );
}
