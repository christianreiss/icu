//! The handshake under attack (`PROTOCOL.md` §7, `Server/IMPLEMENTATION.md`
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
use icu_proto::ControlMsg;
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
    // own, the legitimate client keeps 127.0.0.1.
    let runtime = quinn::default_runtime().expect("tokio runtime");
    let socket = std::net::UdpSocket::bind("127.0.0.2:0").expect("bind 127.0.0.2");
    let socket = Arc::new(RetryOnly(
        runtime.wrap_udp_socket(socket).expect("wrap socket"),
    ));
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
