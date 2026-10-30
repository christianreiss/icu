//! Full-room media soak. Run explicitly; uses real mTLS/QUIC with opaque
//! codec-sized payloads. Real codec decoding is covered by client media_gate.
mod common;

use common::{Client, TestServer};
use icu_proto::{
    header::{AUDIO_MAX_PAYLOAD, VIDEO_CODEC_H264, VIDEO_FLAG_KEYFRAME, VIDEO_HEADER_LEN},
    AudioHeader, UserFlags, VideoHeader,
};
use icu_server::room::Counters;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

const VIDEO_BYTES: usize = 3000; // 360 kbit/s at 15 fps, below the 400 kbit/s ceiling.
const LATENCY_BUCKET_NS: u64 = 100_000;
const LATENCY_BUCKETS: usize = 10_001; // 0.1-ms buckets; last bucket includes overflow.

// The soak is shorter than one sequence wrap. Fixed bitmaps avoid synchronized
// multi-million-entry HashSet reallocations pausing the readers during a long run.
struct Seen {
    bits: Vec<u64>,
    count: usize,
}

impl Seen {
    fn new(peers: usize) -> Self {
        Self {
            bits: vec![0; peers * 1024],
            count: 0,
        }
    }

    fn insert(&mut self, peer: usize, seq: u16) -> bool {
        let word = &mut self.bits[peer * 1024 + usize::from(seq) / 64];
        let mask = 1u64 << (seq % 64);
        if *word & mask != 0 {
            return false;
        }
        *word |= mask;
        self.count += 1;
        true
    }
}

async fn participant(server: &TestServer, index: usize, blocked_video: bool) -> Client {
    let name = format!("media{index:02}");
    let identity = server.enroll(&name).await;
    let mut config =
        common::client_config(server.ca(), Some(identity), icu_proto::limits::ALPN_MAIN);
    if blocked_video {
        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_uni_streams(0u32.into());
        config.transport_config(Arc::new(transport));
    }
    let mut endpoint = common::bare_endpoint(&server.com_key());
    endpoint.set_default_client_config(config);
    let conn = endpoint
        .connect(server.addr(), "localhost")
        .unwrap()
        .await
        .unwrap();
    let (send, recv) = conn.open_bi().await.unwrap();
    let mut client = Client {
        endpoint,
        conn,
        send,
        recv,
        session: 0,
        user_id: 0,
        crl: None,
    };
    client
        .hello_with(
            &name,
            icu_proto::MINOR,
            UserFlags(UserFlags::VIEWING | UserFlags::CAMERA_ON),
        )
        .await
        .expect("Welcome");
    client
}

async fn soak(blocked_video: bool) {
    let clients_count: usize = std::env::var("ICU_MEDIA_CLIENTS")
        .ok()
        .map(|s| s.parse().expect("clients"))
        .unwrap_or(icu_proto::limits::DEFAULT_MAX_USERS as usize);
    assert!((2..=icu_proto::limits::MAX_MAX_USERS as usize).contains(&clients_count));
    let seconds: u64 = std::env::var("ICU_MEDIA_SECONDS")
        .ok()
        .map(|s| s.parse().expect("seconds"))
        .unwrap_or(30);
    assert!((10..=600).contains(&seconds));
    let server = TestServer::start_with(|c| {
        c.server.max_users = clients_count as u16;
        // Only registration/handshake setup rates differ from production.
        c.limits.handshakes_per_ip_per_s = 10_000;
        c.limits.enroll_per_ip_per_min = 10_000;
        c.limits.enroll_global_per_min = 10_000;
    })
    .await;
    let mut clients = Vec::new();
    for i in 0..clients_count {
        clients.push(participant(&server, i, blocked_video && i == clients_count - 1).await);
    }
    let sessions: Arc<HashMap<_, _>> = Arc::new(
        clients
            .iter()
            .enumerate()
            .map(|(i, c)| (c.session, i))
            .collect(),
    );
    let (ready_tx, ready_rx) = tokio::sync::watch::channel(None::<Instant>);
    let mut endpoints = Vec::new();
    let mut controls = Vec::new();
    let mut control_readers = Vec::new();
    let mut readers = Vec::new();
    let mut senders = Vec::new();
    for (index, client) in clients.into_iter().enumerate() {
        let Client {
            endpoint,
            conn,
            send,
            mut recv,
            session,
            ..
        } = client;
        endpoints.push(endpoint);
        controls.push(send);
        control_readers.push(tokio::spawn(async move {
            while common::read_frame(&mut recv, icu_proto::MAX_CONTROL_FRAME)
                .await
                .is_ok()
            {}
        }));
        let peer = conn.clone();
        let known = sessions.clone();
        let mut ready = ready_rx.clone();
        let mut seen = Seen::new(clients_count);
        let mut latencies = vec![0u64; LATENCY_BUCKETS];
        readers.push(tokio::spawn(async move {
            let start = ready.wait_for(Option::is_some).await.unwrap().unwrap();
            while let Ok(packet) = peer.read_datagram().await {
                let (h, payload) = AudioHeader::decode(&packet).unwrap();
                assert!(h.speaker != session && known.contains_key(&h.speaker));
                assert_eq!(payload.len(), AUDIO_MAX_PAYLOAD);
                assert!(payload[8..].iter().all(|b| *b == h.seq as u8));
                assert!(seen.insert(known[&h.speaker], h.seq), "duplicate audio");
                let sent = u64::from_le_bytes(payload[..8].try_into().unwrap());
                let elapsed = (start.elapsed().as_nanos() as u64).saturating_sub(sent);
                let bucket = (elapsed / LATENCY_BUCKET_NS).min((LATENCY_BUCKETS - 1) as u64);
                latencies[bucket as usize] += 1;
            }
            (false, seen.count, latencies)
        }));
        if !(blocked_video && index == clients_count - 1) {
            let peer = conn.clone();
            let known = sessions.clone();
            let mut seen = Seen::new(clients_count);
            readers.push(tokio::spawn(async move {
                while let Ok(mut recv) = peer.accept_uni().await {
                    let frame = recv
                        .read_to_end(VIDEO_HEADER_LEN + VIDEO_BYTES)
                        .await
                        .expect("whole video frame");
                    let h = VideoHeader::decode(&frame[..VIDEO_HEADER_LEN]).unwrap();
                    assert!(h.speaker != session && known.contains_key(&h.speaker));
                    assert_eq!(frame.len(), VIDEO_HEADER_LEN + VIDEO_BYTES);
                    assert!(frame[VIDEO_HEADER_LEN..].iter().all(|b| *b == h.seq as u8));
                    assert!(seen.insert(known[&h.speaker], h.seq), "duplicate video");
                }
                (true, seen.count, Vec::new())
            }));
        }
        let mut ready = ready_rx.clone();
        senders.push(tokio::spawn(async move {
            let start = ready.wait_for(Option::is_some).await.unwrap().unwrap();
            let deadline = tokio::time::Instant::from_std(start + Duration::from_secs(seconds));
            let mut audio = tokio::time::interval(Duration::from_millis(20));
            let mut video = tokio::time::interval(Duration::from_nanos(1_000_000_000 / 15));
            audio.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            video.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let (mut a, mut v) = (0u16, 0u16);
            loop {
                tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(deadline) => break,
                    _ = audio.tick() => {
                        let mut payload = [a as u8; AUDIO_MAX_PAYLOAD];
                        payload[..8].copy_from_slice(&(start.elapsed().as_nanos() as u64).to_le_bytes());
                        let header = AudioHeader { seq: a, ts: u32::from(a)*960, ..Default::default() };
                        conn.send_datagram(header.datagram(&payload).unwrap().into()).unwrap();
                        a += 1;
                    }
                    _ = video.tick() => {
                        // Model an actual encoder's periodic IDR so a pair can
                        // recover after pressure instead of losing video forever.
                        let flags = if v % 30 == 0 { VIDEO_FLAG_KEYFRAME } else { 0 };
                        let header = VideoHeader { flags, speaker: 0, seq: v, ts: u32::from(v)*3200, codec: VIDEO_CODEC_H264 };
                        let mut frame = header.encode().to_vec();
                        frame.resize(VIDEO_HEADER_LEN + VIDEO_BYTES, v as u8);
                        tokio::time::timeout(Duration::from_secs(1), async {
                            let mut stream = conn.open_uni().await.unwrap();
                            stream.write_all(&frame).await.unwrap();
                            stream.finish().unwrap();
                        }).await.expect("sender must not stall");
                        v += 1;
                    }
                }
            }
            assert!(u64::from(a) >= seconds*45, "audio pacing");
            assert!(u64::from(v) >= seconds*14, "video pacing");
            (index, u64::from(a), u64::from(v), conn)
        }));
    }
    // Allocate every reader before starting the common measurement clock.
    let start = Instant::now();
    let deadline = tokio::time::Instant::from_std(start + Duration::from_secs(seconds));
    ready_tx.send(Some(start)).unwrap();
    // ICU_MEDIA_JUNK_PPS: datagrams without the com key's tag at the
    // server's port for the whole soak, rounded up to whole packets per
    // millisecond. The filter in front of quinn drops them; the room must not
    // notice.
    let junk_pps: u64 = std::env::var("ICU_MEDIA_JUNK_PPS")
        .ok()
        .map(|s| s.parse().expect("packets per second"))
        .unwrap_or(0);
    let junk = (junk_pps > 0).then(|| {
        let target = server.addr();
        tokio::spawn(async move {
            let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let mut tick = tokio::time::interval(Duration::from_millis(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // A QUIC long header, then a counter: never the same datagram twice.
            // Only datagrams the socket took count as sent.
            let mut packet = [0u8; 1200];
            packet[0] = 0xc0;
            let (mut seq, mut sent) = (0u64, 0u64);
            while tokio::time::Instant::now() < deadline {
                tick.tick().await;
                for _ in 0..junk_pps.div_ceil(1000) {
                    packet[1..9].copy_from_slice(&seq.to_le_bytes());
                    seq += 1;
                    if socket.send_to(&packet, target).await.is_ok() {
                        sent += 1;
                    }
                }
            }
            sent
        })
    });
    let (mut sent_audio, mut expected_video) = (0, 0);
    let mut connections = Vec::new();
    for task in senders {
        let (index, audio, video, conn) = task.await.expect("sender task");
        sent_audio += audio;
        let receivers = if blocked_video && index != clients_count - 1 {
            clients_count - 2
        } else {
            clients_count - 1
        };
        expected_video += video * receivers as u64;
        connections.push(conn);
    }
    let junk_sent = match junk {
        Some(task) => task.await.expect("junk task"),
        None => 0,
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(connections.iter().all(|c| c.close_reason().is_none()));
    let com_dropped = server
        .handle
        .server()
        .com_dropped
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        junk_sent == 0 || com_dropped > 0,
        "the junk reached the filter"
    );
    // Every client tags its datagrams, so the filter drops only junk.
    assert!(
        com_dropped <= junk_sent,
        "the filter dropped tagged traffic"
    );
    let c = &server.handle.server().counters;
    assert_eq!(Counters::get(&c.datagrams_in), sent_audio);
    assert_eq!(
        Counters::get(&c.datagrams_out),
        sent_audio * (clients_count as u64 - 1)
    );
    assert_eq!(Counters::get(&c.dropped_rate), 0);
    assert_eq!(Counters::get(&c.video_dropped_rate), 0);
    assert_eq!(Counters::get(&c.video_dropped_stalled), 0);
    if blocked_video {
        assert!(
            Counters::get(&c.video_dropped_slow_receiver) > 0,
            "the hostile viewer was isolated"
        );
    } else {
        assert_eq!(Counters::get(&c.video_dropped_slow_receiver), 0);
    }
    for conn in connections {
        conn.close(icu_proto::error::SHUTDOWN.into(), b"");
    }
    let (mut audio, mut video) = (0, 0);
    let mut latencies = vec![0u64; LATENCY_BUCKETS];
    for reader in readers {
        let (is_video, count, samples) = reader.await.expect("reader task");
        if is_video {
            video += count as u64;
        } else {
            audio += count as u64;
            for (bucket, count) in latencies.iter_mut().zip(samples) {
                *bucket += count;
            }
        }
    }
    for task in control_readers {
        task.await.unwrap();
    }
    assert_eq!(latencies.iter().sum::<u64>(), audio);
    let mut cumulative = 0;
    let p99_bucket = latencies
        .iter()
        .position(|count| {
            cumulative += count;
            cumulative > audio * 99 / 100
        })
        .expect("audio latency samples");
    // Conservative upper edge, never round a slow sample down across the gate.
    let p99_ms = (p99_bucket + 1) as f64 * LATENCY_BUCKET_NS as f64 / 1_000_000.0;
    // Junk the socket took, and what the filter in front of quinn saw of
    // it: the rate the room was actually under.
    let (junk_rate, dropped_rate) = (junk_sent / seconds, com_dropped / seconds);
    println!("{seconds}s, {clients_count} clients, blocked_video={blocked_video}: audio {audio}, video {video}, expected_video {expected_video}, audio p99 {p99_ms:.3}ms, junk sent {junk_sent} ({junk_rate}/s), com_dropped {com_dropped} ({dropped_rate}/s)");
    assert_eq!(
        audio,
        sent_audio * (clients_count as u64 - 1),
        "all audio delivered with concurrent video"
    );
    assert_eq!(
        video, expected_video,
        "all video delivered to healthy viewers"
    );
    assert!(
        p99_ms < 100.0,
        "audio remains real time under full video load"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "full-room A/V soak, 30 s by default"]
async fn full_room_audio_and_video() {
    soak(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "full-room A/V soak with a hostile viewer, 30 s by default"]
async fn blocked_video_receiver_does_not_disrupt_healthy_peers() {
    soak(true).await;
}
