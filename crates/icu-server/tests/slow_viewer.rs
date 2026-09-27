//! A viewer whose downlink is too narrow for the video it asks for
//! (`PROTOCOL.md` §4; readiness review 2026-09-27, S3).
//!
//! The server keeps at most `VIDEO_UNACKED_PER_PAIR` frames per pair
//! unacknowledged. Such a viewer therefore drops frames and stays close to
//! live, instead of collecting seconds of video in the network and the
//! server's send queue; its sender hears about it through
//! `VideoCongestionHint`, and a healthy viewer of the same sender notices
//! nothing. A viewer that acknowledges nothing at all has its frames reset
//! at their deadline and when their forwarder ends (external review
//! 2026-09-27).

mod common;

use std::{
    collections::HashSet,
    net::SocketAddr,
    time::{Duration, Instant},
};

use common::{Client, TestServer, SOON};
use icu_proto::{
    header::{VideoHeader, VIDEO_HEADER_LEN},
    ControlMsg, UserFlags,
};
use icu_server::room::Counters;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, watch},
    task::JoinHandle,
};

/// Bytes per second the narrow downlink carries: 128 kbit/s.
const DOWNLINK: f64 = 16_000.0;
/// Payload per frame. At 15 fps that is 360 kbit/s, almost three times what
/// the downlink carries.
const PAYLOAD: usize = 3_000;
/// Four seconds of video at 15 fps.
const FRAMES: u16 = 60;
/// A keyframe every third of a second, so that a viewer that dropped a frame
/// gets going again soon (the real sender sends one every 2 s and on
/// `KeyframeRequest`).
const KEYFRAME_EVERY: u16 = 5;

/// A UDP relay in front of the server whose server-to-client direction is a
/// link of [`DOWNLINK`] bytes per second behind an unbounded FIFO: a narrow
/// downlink with a deep buffer. Nothing is lost, so only acknowledgements
/// tell the server how far behind the viewer is. Returns the address to
/// connect to.
async fn narrow_downlink(server: SocketAddr) -> SocketAddr {
    let front = std::sync::Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let back = std::sync::Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    back.connect(server).await.unwrap();
    let addr = front.local_addr().unwrap();
    let (client_tx, client_rx) = watch::channel(None::<SocketAddr>);

    // Client to server: as it comes.
    let (up_front, up_back) = (front.clone(), back.clone());
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65_536];
        while let Ok((n, from)) = up_front.recv_from(&mut buf).await {
            client_tx.send_replace(Some(from));
            let _ = up_back.send(&buf[..n]).await;
        }
    });

    // Server to client: queued, then each packet once the link has carried
    // the ones before it.
    let (queue_tx, mut queue_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65_536];
        while let Ok(n) = back.recv(&mut buf).await {
            if queue_tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });
    tokio::spawn(async move {
        let mut free = tokio::time::Instant::now();
        while let Some(packet) = queue_rx.recv().await {
            let serialise = Duration::from_secs_f64(packet.len() as f64 / DOWNLINK);
            free = free.max(tokio::time::Instant::now()) + serialise;
            tokio::time::sleep_until(free).await;
            let client = *client_rx.borrow();
            if let Some(client) = client {
                let _ = front.send_to(&packet, client).await;
            }
        }
    });
    addr
}

/// A viewer connected to the server through `addr`.
async fn viewer_via(server: &TestServer, addr: SocketAddr, name: &str) -> Client {
    let identity = server.enroll(name).await;
    let mut client = common::connect_with(addr, server.ca(), &server.com_key(), Some(identity))
        .await
        .expect("connect");
    client
        .hello_with(name, icu_proto::MINOR, UserFlags(UserFlags::VIEWING))
        .await
        .expect("Welcome");
    client
}

/// One frame as a viewer got it.
#[derive(Debug, Clone, Copy)]
struct Got {
    seq: u16,
    keyframe: bool,
    /// From the sender writing it to the viewer having all of it.
    lag: Duration,
}

/// Reads every video frame `conn` receives until `until`, in stream order.
/// The first 8 payload bytes are when the frame was sent, as nanoseconds
/// since `start`.
fn record(
    conn: quinn::Connection,
    start: Instant,
    until: tokio::time::Instant,
) -> JoinHandle<Vec<Got>> {
    tokio::spawn(async move {
        let mut got = Vec::new();
        while let Ok(Ok(mut recv)) = tokio::time::timeout_at(until, conn.accept_uni()).await {
            let read = recv.read_to_end(VIDEO_HEADER_LEN + PAYLOAD);
            let Ok(Ok(frame)) = tokio::time::timeout_at(until, read).await else {
                continue;
            };
            let header = VideoHeader::decode(&frame).expect("video header");
            let sent = &frame[VIDEO_HEADER_LEN..VIDEO_HEADER_LEN + 8];
            let sent = Duration::from_nanos(u64::from_le_bytes(sent.try_into().unwrap()));
            got.push(Got {
                seq: header.seq,
                keyframe: header.keyframe(),
                lag: start.elapsed().saturating_sub(sent),
            });
        }
        got
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_with_a_narrow_downlink_drops_frames_instead_of_lagging() {
    let server = TestServer::start_with(|c| c.limits.video_kbit_s = 5_000).await;
    let mut alice = server.client("alice").await;
    let bob = viewer_via(&server, server.addr(), "bob").await;
    let narrow = narrow_downlink(server.addr()).await;
    let carol = viewer_via(&server, narrow, "carol").await;
    let session = alice.session;
    alice
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::CAMERA_ON),
        })
        .await;
    alice
        .recv_matching(|m| matches!(m, ControlMsg::UserState { session: s, flags } if *s == session && flags.camera_on()))
        .await
        .expect("camera on");

    let start = Instant::now();
    let until = tokio::time::Instant::now()
        + Duration::from_secs_f64(f64::from(FRAMES) / 15.0)
        + Duration::from_secs(3);
    let healthy = record(bob.conn.clone(), start, until);
    let narrow = record(carol.conn.clone(), start, until);

    let mut tick = tokio::time::interval(Duration::from_nanos(1_000_000_000 / 15));
    for seq in 0..FRAMES {
        tick.tick().await;
        let mut payload = vec![seq as u8; PAYLOAD];
        payload[..8].copy_from_slice(&(start.elapsed().as_nanos() as u64).to_le_bytes());
        alice
            .send_video(
                seq,
                u32::from(seq) * 6000,
                seq % KEYFRAME_EVERY == 0,
                &payload,
            )
            .await;
    }
    let healthy = healthy.await.unwrap();
    let narrow = narrow.await.unwrap();
    let worst = |got: &[Got]| got.iter().map(|g| g.lag).max().unwrap_or_default();
    println!(
        "healthy: {} frames, worst lag {:?}; narrow: {} frames {:?}, worst lag {:?}",
        healthy.len(),
        worst(&healthy),
        narrow.len(),
        narrow.iter().map(|g| g.seq).collect::<Vec<_>>(),
        worst(&narrow),
    );

    // The healthy viewer of the same sender gets everything, promptly.
    assert_eq!(
        healthy.iter().map(|g| g.seq).collect::<Vec<_>>(),
        (0..FRAMES).collect::<Vec<_>>(),
        "the healthy viewer misses nothing"
    );
    assert!(worst(&healthy) < Duration::from_millis(500));

    // The narrow one gets what fits, near live, up to the last frames: at
    // 3 KB a frame the link carries about 5 of 15 a second, and before the
    // fix it queued the rest, seconds deep.
    assert!(narrow.len() >= 5, "the narrow viewer still gets video");
    assert!(
        worst(&narrow) < Duration::from_millis(1500),
        "the narrow viewer stays near live: worst lag {:?}",
        worst(&narrow)
    );
    let last = narrow.last().expect("frames").seq;
    assert!(
        last >= FRAMES - 2 * KEYFRAME_EVERY,
        "the narrow viewer keeps up to the end, got up to {last}"
    );
    // Every delta it gets can be decoded: the frame before it came too.
    let seqs: HashSet<u16> = narrow.iter().map(|g| g.seq).collect();
    for g in &narrow {
        assert!(
            g.keyframe || seqs.contains(&(g.seq - 1)),
            "delta {} arrived without frame {}",
            g.seq,
            g.seq - 1
        );
    }

    // The drops are counted and reach the sender.
    let counters = &server.handle.server().counters;
    assert!(Counters::get(&counters.video_dropped_slow_receiver) > 0);
    let hint = tokio::time::timeout(
        SOON,
        alice.recv_matching(|m| {
            matches!(m, ControlMsg::VideoCongestionHint { drop_permille } if *drop_permille > 0)
        }),
    )
    .await;
    assert!(
        matches!(hint, Ok(Some(_))),
        "the sender gets a VideoCongestionHint"
    );
    assert!(carol.conn.close_reason().is_none(), "no disconnect");
    server.shutdown().await;
}

/// A UDP relay in front of the server whose server-to-client direction can
/// be cut: while the returned flag is set, whatever the server sends is lost,
/// so the viewer acknowledges nothing. Lost, not held back — packets let
/// through later would still deliver the frame they carry. Returns the
/// address to connect to.
async fn cuttable_downlink(server: SocketAddr) -> (SocketAddr, watch::Sender<bool>) {
    let front = std::sync::Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let back = std::sync::Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    back.connect(server).await.unwrap();
    let addr = front.local_addr().unwrap();
    let (client_tx, client_rx) = watch::channel(None::<SocketAddr>);
    let (cut_tx, cut_rx) = watch::channel(false);

    let (up_front, up_back) = (front.clone(), back.clone());
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65_536];
        while let Ok((n, from)) = up_front.recv_from(&mut buf).await {
            client_tx.send_replace(Some(from));
            let _ = up_back.send(&buf[..n]).await;
        }
    });
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65_536];
        while let Ok(n) = back.recv(&mut buf).await {
            let (cut, client) = (*cut_rx.borrow(), *client_rx.borrow());
            if let (false, Some(client)) = (cut, client) {
                let _ = front.send_to(&buf[..n], client).await;
            }
        }
    });
    (addr, cut_tx)
}

/// Turns the camera on and waits for the server to echo it.
async fn camera_on(client: &mut Client) {
    let session = client.session;
    client
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::CAMERA_ON),
        })
        .await;
    client
        .recv_matching(|m| matches!(m, ControlMsg::UserState { session: s, flags } if *s == session && flags.camera_on()))
        .await
        .expect("camera on");
}

/// The last frame before a pause, to a viewer that acknowledges nothing, is
/// given up at its deadline like any other: reset, so quinn stops
/// retransmitting it, and counted (external review 2026-09-27). Before, the
/// deadline was only watched with `VIDEO_UNACKED_PER_PAIR` frames in flight;
/// a lone frame was retransmitted for as long as the connection lasted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_frame_before_a_pause_is_given_up_at_its_deadline() {
    let server = TestServer::start_with(|c| c.timing.video_frame_timeout_ms = 300).await;
    let mut alice = server.client("alice").await;
    let (link, cut) = cuttable_downlink(server.addr()).await;
    let bob = viewer_via(&server, link, "bob").await;
    camera_on(&mut alice).await;

    cut.send_replace(true);
    alice.send_video(1, 0, true, &[0x41; 1_000]).await;
    let counters = &server.handle.server().counters;
    tokio::time::timeout(SOON, async {
        while Counters::get(&counters.video_dropped_slow_receiver) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the unacknowledged frame is given up at its deadline");

    // Frame 1 was reset, so the first frame bob gets in full is the next
    // keyframe. A reset stream may still be accepted, and reads as nothing.
    cut.send_replace(false);
    alice.send_video(2, 6000, true, &[0x42; 1_000]).await;
    let seq = tokio::time::timeout(SOON, async {
        loop {
            if let Some((header, _)) = bob.recv_video(SOON).await {
                return header.seq;
            }
        }
    })
    .await
    .expect("the pair gets going again with the next keyframe");
    assert_eq!(seq, 2, "frame 1 arrived after it was given up");
    server.shutdown().await;
}

/// Frames still in flight when their forwarder ends are reset as well: a
/// dropped quinn `SendStream` is finished, not abandoned, and would go on
/// being retransmitted to a viewer long after the sender left (external
/// review 2026-09-27).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frames_in_flight_are_reset_when_the_forwarder_ends() {
    // A deadline far beyond the test: only the forwarder's end can reset.
    let server = TestServer::start_with(|c| c.timing.video_frame_timeout_ms = 60_000).await;
    let mut alice = server.client("alice").await;
    let (link, cut) = cuttable_downlink(server.addr()).await;
    let mut bob = viewer_via(&server, link, "bob").await;
    let session = alice.session;
    camera_on(&mut alice).await;

    cut.send_replace(true);
    alice.send_video(1, 0, true, &[0x41; 1_000]).await;
    let srv = server.handle.server();
    tokio::time::timeout(SOON, async {
        while Counters::get(&srv.counters.video_frames_out) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the frame is handed to quinn");

    // The sender leaves: its slots close and the forwarder ends.
    alice.conn.close(icu_proto::error::SHUTDOWN.into(), b"");
    tokio::time::timeout(SOON, async {
        while srv.room.flags(session).is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the server sees alice leave");
    tokio::time::sleep(Duration::from_millis(100)).await;

    cut.send_replace(false);
    bob.recv_matching(|m| matches!(m, ControlMsg::UserLeft { session: s, .. } if *s == session))
        .await
        .expect("the downlink carries again");
    assert!(
        bob.recv_video(Duration::from_secs(2)).await.is_none(),
        "a frame of a sender that left was still delivered"
    );
    server.shutdown().await;
}
