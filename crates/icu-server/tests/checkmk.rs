//! The Checkmk local check `deploy/checkmk/icu_server` against a stand-in
//! for `icu-server status --json` (readiness review 2026-09-27, S8): values
//! for right now are graphed as they are, counters become rates against the
//! previous run, and a large share of video dropped for slow viewers turns
//! WARN only when it lasts.

use std::{path::Path, process::Command};

const SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../deploy/checkmk/icu_server"
);

/// A `status --json` answer with the given uptime and counters.
fn status(uptime: u64, connections: u64, datagrams: u64, video_out: u64, slow: u64) -> String {
    serde_json::json!({
        "ok": true,
        "uptime_s": uptime,
        "online": 3,
        "max_users": 16,
        "connections": connections,
        "open_connections": 4,
        "handshakes": 1,
        "roomless_sessions": 0,
        "tls": {"server_cert_days_left": 200, "ca_days_left": 3000},
        "relay": {
            "datagrams_in": datagrams,
            "datagrams_out": 2 * datagrams,
            "video_frames_in": video_out,
            "video_frames_out": video_out,
            "control_in": 0,
            "dropped": {"video_slow_receiver": slow, "rate": 0},
        },
    })
    .to_string()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Runs the check with `json` as the server's answer; returns its lines.
fn check(dir: &Path, json: &str) -> Vec<String> {
    std::fs::write(dir.join("status.json"), json).unwrap();
    let out = Command::new("sh")
        .arg(SCRIPT)
        .env("ICU_SERVER_BIN", dir.join("icu-server"))
        .env("ICU_ADMIN_SOCKET", dir.join("admin.sock"))
        .env("ICU_CHECK_STATE", dir.join("state"))
        .env("ICU_TEST_STATUS", dir.join("status.json"))
        .output()
        .unwrap();
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

fn line<'a>(lines: &'a [String], item: &str) -> &'a str {
    lines
        .iter()
        .find(|l| l.split(' ').nth(1) == Some(item))
        .unwrap_or_else(|| panic!("no {item} in {lines:?}"))
}

#[test]
fn gauges_rates_and_a_lasting_slow_viewer() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("icu-server");
    std::fs::write(&bin, "#!/bin/sh\ncat \"$ICU_TEST_STATUS\"\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(dir.path().join("admin.sock")).unwrap();
    let state = dir.path().join("state");

    // First run: the values for now, no rates yet, a baseline written.
    let lines = check(dir.path(), &status(100, 10, 1000, 500, 0));
    let server = line(&lines, "ICU_Server");
    assert!(server.starts_with("0 "), "{server}");
    assert!(
        server.contains("online=3;;;0;16|uptime=100|open_connections=4|handshakes=1 "),
        "{server}"
    );
    assert!(
        !server.contains("connections=10"),
        "a counter is no gauge: {server}"
    );
    assert!(!server.contains("_per_s"), "{server}");
    assert!(line(&lines, "ICU_Video").starts_with("0 ICU_Video - "));
    assert!(line(&lines, "ICU_Server_Certificate").starts_with("0 "));
    let base = std::fs::read_to_string(&state).unwrap();
    assert_eq!(base.split_whitespace().count(), 8, "{base}");

    // A minute later: rates. 100 of 1000 video frames dropped for slow
    // viewers is 10 %, at the threshold, but only once.
    let t = now() - 60;
    std::fs::write(&state, format!("{t} 100 10 1000 2000 500 0 0\n")).unwrap();
    let lines = check(dir.path(), &status(160, 16, 4000, 1400, 100));
    let server = line(&lines, "ICU_Server");
    assert!(
        server
            .contains("|connects_per_s=0.10|datagrams_in_per_s=50.00|datagrams_out_per_s=100.00 "),
        "{server}"
    );
    let video = line(&lines, "ICU_Video");
    assert!(
        video.starts_with(
            "0 ICU_Video video_out_per_s=15.00|video_slow_drops_per_s=1.67|video_slow_pct=10;10 "
        ),
        "{video}"
    );
    assert!(std::fs::read_to_string(&state).unwrap().ends_with(" 1\n"));

    // The third run in a row with that share: WARN.
    std::fs::write(&state, format!("{t} 100 10 1000 2000 500 0 2\n")).unwrap();
    let lines = check(dir.path(), &status(160, 16, 4000, 1400, 100));
    assert!(
        line(&lines, "ICU_Video").starts_with("1 ICU_Video "),
        "{lines:?}"
    );

    // A share below the threshold ends the streak.
    std::fs::write(&state, format!("{t} 100 10 1000 2000 500 0 2\n")).unwrap();
    let lines = check(dir.path(), &status(160, 16, 4000, 1400, 50));
    assert!(
        line(&lines, "ICU_Video").starts_with("0 ICU_Video "),
        "{lines:?}"
    );
    assert!(std::fs::read_to_string(&state).unwrap().ends_with(" 0\n"));

    // A restart resets the counters: no rates from that run.
    std::fs::write(&state, format!("{t} 100 10 1000 2000 500 0 2\n")).unwrap();
    let lines = check(dir.path(), &status(5, 1, 10, 0, 0));
    assert!(!line(&lines, "ICU_Server").contains("_per_s"), "{lines:?}");
    assert!(line(&lines, "ICU_Video").starts_with("0 ICU_Video - "));

    // A garbled state file is no baseline either.
    std::fs::write(&state, "garbage\n").unwrap();
    let lines = check(dir.path(), &status(160, 16, 4000, 1400, 100));
    assert!(!line(&lines, "ICU_Server").contains("_per_s"), "{lines:?}");
}
