//! The handshake under attack (`docs/reference/protocol.md` §7, `docs/reference/server.md`
//! §6.2): whatever an outsider does before the handshake completes must not
//! keep a legitimate client out.

mod common;

use std::{
    io::{self, IoSliceMut},
    net::SocketAddr,
    pin::Pin,
    sync::{atomic::Ordering, Arc},
    task::{ready, Context, Poll},
    time::Duration,
};

use common::TestServer;
use icu_proto::{
    com::{ComKey, ComKeys, ComSocket, Side},
    ControlMsg,
};
use quinn::{
    udp::{RecvMeta, Transmit},
    AsyncUdpSocket, UdpPoller,
};

/// A client socket that receives nothing but Retry packets. Its owner proves
/// its address — it answers the server's Retry — and then never sees the
/// server's handshake flight, so every handshake it starts stalls on the
/// server until the handshake timeout.
#[derive(Debug)]
struct RetryOnly(Arc<dyn AsyncUdpSocket>);

impl AsyncUdpSocket for RetryOnly {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.0.clone().create_io_poller()
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        self.0.try_send(transmit)
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            let received = ready!(self.0.poll_recv(cx, bufs, meta))?;
            let mut kept = 0;
            for i in 0..received {
                // Long header, fixed bit, packet type Retry: 0b1111_xxxx.
                if bufs[i].first().is_some_and(|b| b & 0xf0 == 0xf0) {
                    if kept != i {
                        let len = meta[i].len;
                        let (head, tail) = bufs.split_at_mut(i);
                        head[kept][..len].copy_from_slice(&tail[0][..len]);
                        meta[kept] = meta[i];
                    }
                    kept += 1;
                }
            }
            if kept > 0 {
                return Poll::Ready(Ok(kept));
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.0.max_transmit_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.0.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.0.may_fragment()
    }
}

/// Waits until the server's count of handshakes in flight has not moved for
/// a while, and returns it.
async fn settled_handshakes(server: &TestServer) -> usize {
    let count = || server.handle.server().handshakes.load(Ordering::Relaxed);
    let mut last = count();
    let mut still = 0;
    for _ in 0..250 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let now = count();
        if now == last && now > 0 {
            still += 1;
            if still == 15 {
                break;
            }
        } else {
            still = 0;
        }
        last = now;
    }
    last
}

/// An attacker that answers the Retry from one address and then lets every
/// handshake stall holds a few handshakes at a time, not the whole ceiling:
/// a client from elsewhere still gets in, through a Retry of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_handshakes_from_one_source_do_not_lock_out_others() {
    let server = TestServer::start_with(|c| {
        // Only the in-flight limits are under test, not the rate buckets.
        c.limits.handshakes_per_ip_per_s = 10_000;
        c.limits.handshakes_global_per_s = 10_000;
        c.limits.max_handshakes = 40;
        c.timing.handshake_timeout_ms = 30_000;
    })
    .await;
    let alice = server.enroll("alice").await;

    // Any 127/8 address is loopback on Linux: the flood has a source of its
    // own, the legitimate client keeps 127.0.0.1. The attacker holds the com
    // key — a revoked member still does (`docs/reference/protocol.md` §1).
    let runtime = quinn::default_runtime().expect("tokio runtime");
    let socket = std::net::UdpSocket::bind("127.0.0.2:0").expect("bind 127.0.0.2");
    let socket = Arc::new(RetryOnly(Arc::new(ComSocket::new(
        runtime.wrap_udp_socket(socket).expect("wrap socket"),
        &server.com_key(),
        Side::Client,
        Arc::default(),
    ))));
    let mut attacker = quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        None,
        socket,
        runtime,
    )
    .expect("attacker endpoint");
    attacker.set_default_client_config(common::client_config(
        server.ca(),
        None,
        icu_proto::limits::ALPN_MAIN,
    ));
    // Dropping a `Connecting` would close it; they stay alive until the end.
    let flood: Vec<_> = (0..100)
        .map(|_| {
            attacker
                .connect(server.addr(), "localhost")
                .expect("connect")
        })
        .collect();

    let held = settled_handshakes(&server).await;
    assert!(
        held > icu_proto::limits::HANDSHAKES_IN_FLIGHT_RETRY,
        "the flood answered the Retry: {held}"
    );
    assert!(
        held <= icu_proto::limits::HANDSHAKES_IN_FLIGHT_RETRY
            + icu_proto::limits::HANDSHAKES_IN_FLIGHT_PER_IP as usize,
        "one source holds {held} handshakes"
    );

    let mut client = server
        .connect(&alice)
        .await
        .expect("a client from another address still connects");
    assert!(matches!(
        client.hello("alice").await,
        ControlMsg::Welcome { .. }
    ));

    attacker.close(0u32.into(), b"");
    drop(flood);
    server.shutdown().await;
}

/// A client presents its leaf and nothing else. Extra certificates would
/// only be issuer candidates for the path search, signature checks an
/// unauthenticated peer could have the server run in bulk, so a longer
/// chain fails the handshake before any of them.
#[tokio::test]
async fn a_client_chain_with_intermediates_is_refused() {
    let server = TestServer::start().await;
    let alice = server.enroll("alice").await;
    let mut endpoint = common::bare_endpoint(&server.com_key());
    let chain = vec![alice.cert.clone(), alice.ca.clone(), alice.ca.clone()];
    endpoint.set_default_client_config(common::client_config_with_chain(
        server.ca(),
        Some((chain, alice.key())),
        icu_proto::limits::ALPN_MAIN,
    ));
    let code = match endpoint
        .connect(server.addr(), "localhost")
        .expect("connect")
        .await
    {
        Ok(conn) => {
            let _ = conn.open_bi().await;
            let reason = tokio::time::timeout(common::SOON, conn.closed())
                .await
                .expect("closed");
            common::code_of(&reason)
        }
        Err(e) => common::code_of(&e),
    };
    assert!(
        code.is_some_and(|c| c >= 0x100),
        "a TLS alert, not a session: {code:?}"
    );

    // The leaf alone is fine.
    let mut client = server.connect(&alice).await.expect("connect");
    assert!(matches!(
        client.hello("alice").await,
        ControlMsg::Welcome { .. }
    ));
    server.shutdown().await;
}

/// Waits up to [`common::BRIEFLY`] × 3 for any datagram on `socket`.
async fn answer(socket: &tokio::net::UdpSocket) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 2048];
    let n = tokio::time::timeout(common::BRIEFLY * 3, socket.recv(&mut buf))
        .await
        .ok()?
        .ok()?;
    buf.truncate(n);
    Some(buf)
}

/// Without the com tag the port stays silent (`docs/reference/protocol.md` §1): a genuine
/// QUIC Initial starts no handshake, a packet of an unknown version gets no
/// Version Negotiation, a short header no Stateless Reset. The same Initial
/// with the tag is answered, so the silence is the tag's doing.
#[tokio::test]
async fn a_datagram_without_the_com_tag_gets_no_answer() {
    let server = TestServer::start().await;
    let alice = server.enroll("alice").await;
    let dropped = || server.handle.server().com_dropped.load(Ordering::Relaxed);
    let before = dropped();

    // A real Initial, caught from a quinn client without the com key.
    let catcher = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut plain = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    plain.set_default_client_config(common::client_config(
        server.ca(),
        Some(alice),
        icu_proto::limits::ALPN_MAIN,
    ));
    let _connecting = plain
        .connect(catcher.local_addr().unwrap(), "localhost")
        .unwrap();
    let initial = answer(&catcher)
        .await
        .expect("the client sends its Initial");
    assert!(initial.len() >= 1200, "a padded Initial: {}", initial.len());

    // Long header of an unsupported version, padded to 1200 bytes.
    let mut unknown_version = vec![0xC0, 0x1A, 0x2A, 0x3A, 0x4A, 8];
    unknown_version.extend([7u8; 8]);
    unknown_version.push(8);
    unknown_version.extend([9u8; 8]);
    unknown_version.resize(1200, 0);
    // Short header with a connection ID nobody issued.
    let mut short = vec![0x40];
    short.extend([0x5Au8; 100]);

    let probe = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for datagram in [&initial, &unknown_version, &short] {
        probe.send_to(datagram, server.addr()).await.unwrap();
        assert_eq!(answer(&probe).await, None, "no answer without the tag");
    }
    assert_eq!(server.handle.server().handshakes.load(Ordering::Relaxed), 0);
    assert_eq!(dropped() - before, 3);

    let sealed = ComKeys::new(&server.com_key(), Side::Client).seal(&initial, None);
    probe.send_to(&sealed, server.addr()).await.unwrap();
    assert!(
        answer(&probe).await.is_some(),
        "the tagged Initial is answered"
    );
    plain.close(0u32.into(), b"");
    server.shutdown().await;
}

/// A client whose passport carries another com key — another server's, or
/// one from before a restore — hears nothing: its handshake runs into the
/// timeout like against a server that is down.
#[tokio::test]
async fn a_client_with_another_com_key_hears_nothing() {
    let server = TestServer::start().await;
    let alice = server.enroll("alice").await;
    let before = server.handle.server().com_dropped.load(Ordering::Relaxed);

    let endpoint = common::client_endpoint(
        server.ca(),
        &ComKey::generate().unwrap(),
        Some(alice.clone()),
        icu_proto::limits::ALPN_MAIN,
    );
    let connecting = endpoint.connect(server.addr(), "localhost").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), connecting)
            .await
            .is_err(),
        "no handshake with the wrong key"
    );
    assert_eq!(server.handle.server().handshakes.load(Ordering::Relaxed), 0);
    assert!(server.handle.server().com_dropped.load(Ordering::Relaxed) > before);

    // With the server's key the same identity gets in.
    let mut client = server.connect(&alice).await.expect("connect");
    assert!(matches!(
        client.hello("alice").await,
        ControlMsg::Welcome { .. }
    ));
    server.shutdown().await;
}
