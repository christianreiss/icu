//! Load and network tests (`Server/IMPLEMENTATION.md` §10, rows "Last" and
//! "Netz").
//!
//! Both are `#[ignore]`d: they take tens of seconds and the netem one needs
//! root on a network namespace. Run them explicitly:
//!
//! ```text
//! # load test, 16 clients x 50 datagrams/s, 10 s by default
//! cargo test -p icu-server --test load -- --ignored --nocapture datagram_load
//! # longer run, as the spec asks for
//! ICU_LOAD_SECONDS=60 cargo test -p icu-server --test load -- --ignored --nocapture datagram_load
//!
//! # netem: 30 ms +- 10 ms, 2 % loss. Needs root and iproute2; the test skips
//! # itself when it cannot set the qdisc up.
//! sudo -E env "PATH=$PATH" cargo test -p icu-server --test load -- --ignored --nocapture netem
//! ```

mod common;

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use common::TestServer;
use icu_server::room::Counters;

/// Number of clients. `max_users` defaults to 16.
const CLIENTS: usize = 16;
/// Datagrams per second and client.
const RATE_PER_S: u64 = 50;

fn load_seconds() -> u64 {
    std::env::var("ICU_LOAD_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10)
}

/// CPU time of this process in seconds (utime + stime from `/proc/self/stat`).
fn cpu_seconds() -> f64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return 0.0;
    };
    // Field 14 and 15, after the (possibly parenthesised) comm field.
    let Some(rest) = stat.rsplit(") ").next() else {
        return 0.0;
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks: f64 = fields
        .get(11)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        + fields
            .get(12)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
    ticks / 100.0 // USER_HZ is 100 on Linux/x86-64
}

/// §10 "Last": 16 clients x 50 datagrams/s. No loss on loopback, CPU below
/// 10 % of a core, no latency growth.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load test, takes ICU_LOAD_SECONDS (default 10) seconds"]
async fn datagram_load() {
    let seconds = load_seconds();
    // 16 clients enrol and connect back to back from 127.0.0.1, which is well
    // over the 10 handshakes/s per IP of `PROTOCOL.md` §7. That limit is meant
    // for the internet, not for a loopback benchmark.
    let server = TestServer::start_with(|config| {
        config.limits.handshakes_per_ip_per_s = 10_000;
        config.limits.enroll_per_ip_per_min = 10_000;
        config.limits.enroll_global_per_min = 10_000;
    })
    .await;

    let mut clients = Vec::new();
    for i in 0..CLIENTS {
        clients.push(server.client(&format!("user{i:02}")).await);
    }
    // Drain the join notifications so the control streams stay quiet.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let start = Instant::now();
    let received = Arc::new(AtomicU64::new(0));
    let mut latencies: Arc<std::sync::Mutex<Vec<u64>>> =
        Arc::new(std::sync::Mutex::new(Vec::with_capacity(
            (seconds * RATE_PER_S * CLIENTS as u64 * (CLIENTS as u64 - 1)) as usize / 4,
        )));

    let mut readers = Vec::new();
    let mut senders = Vec::new();
    for client in clients {
        let conn = client.conn.clone();
        let received = received.clone();
        let latencies = latencies.clone();
        readers.push(tokio::spawn(async move {
            while let Ok(datagram) = conn.read_datagram().await {
                if datagram.len() >= 18 {
                    let mut nanos = [0u8; 8];
                    nanos.copy_from_slice(&datagram[10..18]);
                    let sent = u64::from_le_bytes(nanos);
                    let now = start.elapsed().as_nanos() as u64;
                    latencies
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(now.saturating_sub(sent));
                }
                received.fetch_add(1, Ordering::Relaxed);
            }
        }));
        senders.push(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_micros(1_000_000 / RATE_PER_S));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut seq: u16 = 0;
            let mut sent = 0u64;
            while start.elapsed() < Duration::from_secs(seconds) {
                ticker.tick().await;
                let mut payload = Vec::with_capacity(80);
                payload.extend_from_slice(&(start.elapsed().as_nanos() as u64).to_le_bytes());
                payload.resize(80, 0x5A);
                client.send_audio(seq, u32::from(seq) * 960, &payload);
                seq = seq.wrapping_add(1);
                sent += 1;
            }
            // Keep the connection alive until everybody is done reading.
            tokio::time::sleep(Duration::from_millis(500)).await;
            sent
        }));
    }

    let cpu_before = cpu_seconds();
    let mut sent_total = 0u64;
    for sender in senders {
        sent_total += sender.await.unwrap_or(0);
    }
    let cpu_used = cpu_seconds() - cpu_before;
    tokio::time::sleep(Duration::from_millis(300)).await;
    for reader in readers {
        reader.abort();
        let _ = reader.await;
    }

    let counters = &server.handle.server().counters;
    let datagrams_in = Counters::get(&counters.datagrams_in);
    let datagrams_out = Counters::get(&counters.datagrams_out);
    let dropped_rate = Counters::get(&counters.dropped_rate);
    let dropped_malformed = Counters::get(&counters.dropped_malformed);
    let send_errors = Counters::get(&counters.send_too_large)
        + Counters::get(&counters.send_unsupported)
        + Counters::get(&counters.send_disabled)
        + Counters::get(&counters.send_connection_lost);
    let got = received.load(Ordering::Relaxed);

    let mut values = std::mem::take(
        &mut *Arc::get_mut(&mut latencies)
            .expect("no other owner")
            .get_mut()
            .unwrap_or_else(|e| e.into_inner()),
    );
    values.sort_unstable();
    let percentile = |p: f64| -> f64 {
        if values.is_empty() {
            return 0.0;
        }
        let idx = ((values.len() as f64 - 1.0) * p).round() as usize;
        values[idx] as f64 / 1_000_000.0
    };

    println!("--- Spike 7: server load test ---");
    println!("clients           {CLIENTS}");
    println!("rate per client   {RATE_PER_S}/s");
    println!("duration          {seconds} s");
    println!("sent by clients   {sent_total}");
    println!("datagrams_in      {datagrams_in}");
    println!("datagrams_out     {datagrams_out}");
    println!("received total    {got}");
    println!("expected out      {}", datagrams_in * (CLIENTS as u64 - 1));
    println!("dropped rate      {dropped_rate}");
    println!("dropped malformed {dropped_malformed}");
    println!("send errors       {send_errors}");
    println!(
        "relay latency ms  p50 {:.3}  p99 {:.3}  max {:.3}  (n = {})",
        percentile(0.50),
        percentile(0.99),
        percentile(1.0),
        values.len()
    );
    println!(
        "cpu               {:.2} s over {} s = {:.1} % of one core",
        cpu_used,
        seconds,
        cpu_used / seconds as f64 * 100.0
    );

    assert_eq!(
        datagrams_in, sent_total,
        "every sent packet reaches the relay"
    );
    assert_eq!(
        got, datagrams_out,
        "every relayed packet reaches a receiver"
    );
    assert_eq!(dropped_rate, 0, "50/s is well below the 110/s limit");
    assert_eq!(dropped_malformed, 0);
    assert_eq!(send_errors, 0, "no send errors on loopback");
    assert_eq!(
        datagrams_out,
        datagrams_in * (CLIENTS as u64 - 1),
        "every datagram goes to every other client"
    );
    server.shutdown().await;
}

/// §10 "Netz": `tc netem` 30 ms ± 10 ms, 2 % loss. The relay must not lose
/// anything on top of the link, and the keepalive must hold the connection
/// over 10 minutes of silence.
///
/// This needs `CAP_NET_ADMIN` on the loopback interface, so the test skips
/// itself unless it can set the qdisc up. Set it by hand with
///
/// ```text
/// sudo tc qdisc add dev lo root netem delay 30ms 10ms loss 2%
/// sudo tc qdisc del dev lo root
/// ```
///
/// and then run this test with `ICU_NETEM=assume` to skip the setup.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs root for `tc qdisc` on lo"]
async fn netem() {
    let assume = std::env::var("ICU_NETEM").as_deref() == Ok("assume");
    if !assume {
        let status = std::process::Command::new("tc")
            .args([
                "qdisc", "add", "dev", "lo", "root", "netem", "delay", "30ms", "10ms", "loss", "2%",
            ])
            .status();
        match status {
            Ok(status) if status.success() => {}
            other => {
                println!("skipping: cannot set the netem qdisc up ({other:?})");
                return;
            }
        }
    }
    let cleanup = !assume;

    let server = TestServer::start().await;
    let alice = server.client("alice").await;
    let bob = server.client("bob").await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let count = 500u16;
    for seq in 0..count {
        alice.send_audio(seq, u32::from(seq) * 960, &[0x33; 80]);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    let mut got = 0u32;
    while bob.recv_audio(Duration::from_millis(300)).await.is_some() {
        got += 1;
    }
    let counters = &server.handle.server().counters;
    let datagrams_in = Counters::get(&counters.datagrams_in);
    let datagrams_out = Counters::get(&counters.datagrams_out);
    println!("--- netem 30 ms +- 10 ms, 2 % loss ---");
    println!("sent {count}, server in {datagrams_in}, out {datagrams_out}, bob got {got}");
    assert_eq!(
        datagrams_in, datagrams_out,
        "the relay itself loses nothing; only the link does"
    );
    assert!(
        got > u32::from(count) * 8 / 10,
        "clients still receive media under 2% link loss"
    );

    if cleanup {
        let _ = std::process::Command::new("tc")
            .args(["qdisc", "del", "dev", "lo", "root"])
            .status();
    }
    server.shutdown().await;
}

/// §10 "Netz", second half: the connection survives 10 minutes of silence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs for 10 minutes"]
async fn keepalive_holds_over_ten_minutes_of_silence() {
    let server = TestServer::start().await;
    let alice = server.client("alice").await;
    tokio::time::sleep(Duration::from_secs(10 * 60)).await;
    assert!(
        alice.conn.close_reason().is_none(),
        "the connection must still be open after 10 minutes of silence"
    );
    alice.send_audio(1, 0, &[1; 20]);
    server.shutdown().await;
}
