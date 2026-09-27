//! Full-room media soak. Run explicitly; uses real mTLS/QUIC with opaque
//! codec-sized payloads. Real codec decoding is covered by client media_gate.
mod common;

use common::{Client, TestServer};
use icu_proto::{
    header::{AUDIO_MAX_PAYLOAD, VIDEO_CODEC_H264, VIDEO_HEADER_LEN},
    AudioHeader, UserFlags, VideoHeader,
};
use icu_server::room::Counters;
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

const CLIENTS: usize = 16;
const VIDEO_BYTES: usize = 3000; // 360 kbit/s at 15 fps, below the 400 kbit/s ceiling.

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
    let seconds: u64 = std::env::var("ICU_MEDIA_SECONDS")
        .ok()
        .map(|s| s.parse().expect("seconds"))
        .unwrap_or(30);
    assert!((10..=600).contains(&seconds));
    let server = TestServer::start_with(|c| {
        // Only registration/handshake setup bursts differ from production.
        c.limits.handshakes_per_ip_per_s = 10_000;
        c.limits.enroll_per_ip_per_min = 10_000;
        c.limits.enroll_global_per_min = 10_000;
    })
    .await;
    let mut clients = Vec::new();
    for i in 0..CLIENTS {
        clients.push(participant(&server, i, blocked_video && i == CLIENTS - 1).await);
    }
    let sessions: Arc<HashSet<_>> = Arc::new(clients.iter().map(|c| c.session).collect());
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
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
        readers.push(tokio::spawn(async move {
            let mut seen = HashSet::new();
            let mut latencies = Vec::new();
            while let Ok(packet) = peer.read_datagram().await {
                let (h, payload) = AudioHeader::decode(&packet).unwrap();
                assert!(h.speaker != session && known.contains(&h.speaker));
                assert_eq!(payload.len(), AUDIO_MAX_PAYLOAD);
                assert!(payload[8..].iter().all(|b| *b == h.seq as u8));
                assert!(seen.insert((h.speaker, h.seq)), "duplicate audio");
                let sent = u64::from_le_bytes(payload[..8].try_into().unwrap());
                latencies.push((start.elapsed().as_nanos() as u64).saturating_sub(sent));
            }
            (false, seen.len(), latencies)
        }));
        if !(blocked_video && index == CLIENTS - 1) {
            let peer = conn.clone();
            let known = sessions.clone();
            readers.push(tokio::spawn(async move {
                let mut seen = HashSet::new();
                while let Ok(mut recv) = peer.accept_uni().await {
                    let frame = recv
                        .read_to_end(VIDEO_HEADER_LEN + VIDEO_BYTES)
                        .await
                        .expect("whole video frame");
                    let h = VideoHeader::decode(&frame[..VIDEO_HEADER_LEN]).unwrap();
                    assert!(h.speaker != session && known.contains(&h.speaker));
                    assert_eq!(frame.len(), VIDEO_HEADER_LEN + VIDEO_BYTES);
                    assert!(frame[VIDEO_HEADER_LEN..].iter().all(|b| *b == h.seq as u8));
                    assert!(seen.insert((h.speaker, h.seq)), "duplicate video");
                }
                (true, seen.len(), Vec::new())
            }));
        }
        senders.push(tokio::spawn(async move {
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
                        let header = VideoHeader { flags: 0, speaker: 0, seq: v, ts: u32::from(v)*3200, codec: VIDEO_CODEC_H264 };
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
    let (mut sent_audio, mut expected_video) = (0, 0);
    let mut connections = Vec::new();
    for task in senders {
        let (index, audio, video, conn) = task.await.expect("sender task");
        sent_audio += audio;
        let receivers = if blocked_video && index != CLIENTS - 1 {
            CLIENTS - 2
        } else {
            CLIENTS - 1
        };
        expected_video += video * receivers as u64;
        connections.push(conn);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(connections.iter().all(|c| c.close_reason().is_none()));
    let c = &server.handle.server().counters;
    assert_eq!(Counters::get(&c.datagrams_in), sent_audio);
    assert_eq!(
        Counters::get(&c.datagrams_out),
        sent_audio * (CLIENTS as u64 - 1)
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
    let (mut audio, mut video, mut latencies) = (0, 0, Vec::new());
    for reader in readers {
        let (is_video, count, samples) = reader.await.expect("reader task");
        if is_video {
            video += count as u64;
        } else {
            audio += count as u64;
            latencies.extend(samples);
        }
    }
    for task in control_readers {
        task.await.unwrap();
    }
    latencies.sort_unstable();
    let p99_ms = latencies[latencies.len() * 99 / 100] as f64 / 1_000_000.0;
    println!("{seconds}s, {CLIENTS} clients, blocked_video={blocked_video}: audio {audio}, video {video}, expected_video {expected_video}, audio p99 {p99_ms:.3}ms");
    assert_eq!(
        audio,
        sent_audio * (CLIENTS as u64 - 1),
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
