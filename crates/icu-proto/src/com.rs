//! The com key: an HMAC tag on every UDP datagram (`docs/reference/protocol.md` §1), like
//! OpenVPN's `tls-auth`.
//!
//! Each server has one random 32-byte key; it travels to the clients inside
//! the passport (§2.3.1). Both sides append `HMAC-SHA256(k_dir, datagram)`,
//! cut to [`TAG_LEN`] bytes, to every datagram they send, and drop every
//! datagram whose tag does not verify before quinn sees it. Without the key
//! the port stays silent: no Version Negotiation, no Stateless Reset, no
//! Retry, no handshake.
//!
//! The key is a group secret, not an admission: everyone holding a passport of
//! the server passes, revoked users included. Admission stays with mutual TLS.
//! There is no replay protection; a replayed Initial costs at most one
//! handshake slot, which the server's handshake limits bound (§7).
//!
//! [`ComSocket`] is the only part of this crate that does I/O: a filter
//! around quinn's own UDP socket.

use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use ring::hmac;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

/// Length of the com key in bytes.
pub const COM_KEY_LEN: usize = 32;

/// Length of the tag at the end of every datagram.
pub const TAG_LEN: usize = 16;

/// Upper bound for quinn's MTU discovery: quinn's default of 1452 (1500 minus
/// the IPv6 and UDP headers) less the tag, so a probe still fits into a
/// 1500-byte packet once the tag is on.
pub const MTU_DISCOVERY_UPPER_BOUND: u16 = 1452 - TAG_LEN as u16;

/// How many batches of rejected datagrams [`ComSocket::poll_recv`] reads
/// before it yields, so that a flood of garbage cannot hold the endpoint task.
const REJECTED_BATCHES_BEFORE_YIELD: usize = 8;

/// The server's com key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComKey([u8; COM_KEY_LEN]);

impl ComKey {
    /// A fresh random key.
    pub fn generate() -> Result<ComKey, ring::error::Unspecified> {
        use ring::rand::SecureRandom;
        let mut key = [0u8; COM_KEY_LEN];
        ring::rand::SystemRandom::new().fill(&mut key)?;
        Ok(ComKey(key))
    }

    /// The key from its raw bytes.
    pub fn from_bytes(bytes: [u8; COM_KEY_LEN]) -> ComKey {
        ComKey(bytes)
    }

    /// The raw bytes.
    pub fn as_bytes(&self) -> &[u8; COM_KEY_LEN] {
        &self.0
    }

    /// Standard base64 with padding, the form of the key files.
    pub fn to_base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(self.0)
    }

    /// Reads [`ComKey::to_base64`] back; surrounding whitespace is ignored.
    /// `None` unless it is valid base64 of exactly [`COM_KEY_LEN`] bytes.
    pub fn from_base64(text: &str) -> Option<ComKey> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .ok()?;
        Some(ComKey(bytes.try_into().ok()?))
    }
}

/// The key never ends up in a log line.
impl std::fmt::Debug for ComKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComKey(<redacted>)")
    }
}

/// Which end of the connection a socket is: it signs with its own direction's
/// key and verifies with the other one, so a datagram mirrored back to its
/// sender does not verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Sends client→server, receives server→client.
    Client,
    /// Sends server→client, receives client→server.
    Server,
}

/// The two direction keys derived from a [`ComKey`] for one side.
pub struct ComKeys {
    send: hmac::Key,
    recv: hmac::Key,
}

/// Result of [`ComKeys::open_segments`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opened {
    /// Bytes of verified payload at the start of the buffer.
    pub len: usize,
    /// Distance between the payloads, i.e. the old stride less the tag.
    pub stride: usize,
    /// Segments dropped because their tag did not verify.
    pub dropped: u64,
}

impl ComKeys {
    /// `k_c2s = HMAC-SHA256(key, "icu com c2s")`, `k_s2c` likewise.
    pub fn new(key: &ComKey, side: Side) -> ComKeys {
        let root = hmac::Key::new(hmac::HMAC_SHA256, &key.0);
        let derive =
            |label: &[u8]| hmac::Key::new(hmac::HMAC_SHA256, hmac::sign(&root, label).as_ref());
        let c2s = derive(b"icu com c2s");
        let s2c = derive(b"icu com s2c");
        match side {
            Side::Client => ComKeys {
                send: c2s,
                recv: s2c,
            },
            Side::Server => ComKeys {
                send: s2c,
                recv: c2s,
            },
        }
    }

    /// `datagrams` with a tag after every `segment_size` bytes (after the
    /// whole of it without one), ready to send.
    pub fn seal(&self, datagrams: &[u8], segment_size: Option<usize>) -> Vec<u8> {
        let size = segment_size
            .filter(|&s| s > 0 && s < datagrams.len())
            .unwrap_or(datagrams.len());
        let count = if size == 0 {
            1
        } else {
            datagrams.len().div_ceil(size)
        };
        let mut out = Vec::with_capacity(datagrams.len() + count * TAG_LEN);
        let mut push = |datagram: &[u8]| {
            out.extend_from_slice(datagram);
            out.extend_from_slice(&hmac::sign(&self.send, datagram).as_ref()[..TAG_LEN]);
        };
        if size == 0 {
            push(datagrams);
        } else {
            datagrams.chunks(size).for_each(push);
        }
        out
    }

    /// Length of the payload in front of the tag, if `sealed` carries a tag
    /// that verifies and at least one byte before it.
    fn verify(&self, sealed: &[u8]) -> Option<usize> {
        let payload_len = sealed.len().checked_sub(TAG_LEN).filter(|&n| n > 0)?;
        let (payload, tag) = sealed.split_at(payload_len);
        let expected = hmac::sign(&self.recv, payload);
        bool::from(expected.as_ref()[..TAG_LEN].ct_eq(tag)).then_some(payload_len)
    }

    /// Checks one received buffer of `len` bytes that holds datagrams every
    /// `stride` bytes, the last one possibly shorter (GRO). Verified payloads
    /// are moved together at the start of `buf`, every `stride - TAG_LEN`
    /// bytes; segments that fail are left out. Only the original last segment
    /// can be short, so the result is again such a layout.
    pub fn open_segments(&self, buf: &mut [u8], len: usize, stride: usize) -> Opened {
        let len = len.min(buf.len());
        let stride = if stride == 0 { len } else { stride };
        let mut opened = Opened {
            len: 0,
            stride: stride.saturating_sub(TAG_LEN),
            dropped: 0,
        };
        let mut read = 0;
        while read < len {
            let end = (read + stride).min(len);
            match self.verify(&buf[read..end]) {
                Some(payload_len) => {
                    buf.copy_within(read..read + payload_len, opened.len);
                    opened.len += payload_len;
                }
                None => opened.dropped += 1,
            }
            read = end;
        }
        opened
    }
}

impl std::fmt::Debug for ComKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComKeys(<redacted>)")
    }
}

/// quinn's UDP socket with the com tag: sealed on the way out, checked and
/// stripped on the way in.
#[derive(Debug)]
pub struct ComSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    keys: ComKeys,
    dropped: Arc<AtomicU64>,
}

impl ComSocket {
    /// Wraps `inner`; every datagram dropped for its tag is counted in
    /// `dropped`.
    pub fn new(
        inner: Arc<dyn AsyncUdpSocket>,
        key: &ComKey,
        side: Side,
        dropped: Arc<AtomicU64>,
    ) -> ComSocket {
        ComSocket {
            inner,
            keys: ComKeys::new(key, side),
            dropped,
        }
    }
}

impl AsyncUdpSocket for ComSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.inner.clone().create_io_poller()
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        let sealed = self.keys.seal(transmit.contents, transmit.segment_size);
        self.inner.try_send(&Transmit {
            destination: transmit.destination,
            ecn: transmit.ecn,
            contents: &sealed,
            segment_size: transmit.segment_size.map(|s| s + TAG_LEN),
            src_ip: transmit.src_ip,
        })
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        for _ in 0..REJECTED_BATCHES_BEFORE_YIELD {
            let received = match self.inner.poll_recv(cx, bufs, meta) {
                Poll::Ready(Ok(n)) => n,
                other => return other,
            };
            let mut kept = 0;
            let mut dropped = 0;
            for i in 0..received {
                let opened = self
                    .keys
                    .open_segments(&mut bufs[i], meta[i].len, meta[i].stride);
                dropped += opened.dropped;
                if opened.len == 0 {
                    continue;
                }
                if kept != i {
                    // quinn cuts all buffers from one allocation in equal
                    // chunks, so an earlier one is as large as this one.
                    let (head, tail) = bufs.split_at_mut(i);
                    head[kept][..opened.len].copy_from_slice(&tail[0][..opened.len]);
                }
                meta[kept] = RecvMeta {
                    len: opened.len,
                    stride: opened.stride,
                    ..meta[i]
                };
                kept += 1;
            }
            if dropped > 0 {
                self.dropped.fetch_add(dropped, Ordering::Relaxed);
            }
            if kept > 0 {
                return Poll::Ready(Ok(kept));
            }
        }
        // Everything read so far was rejected and the socket may still hold
        // more: come back after the other tasks had their turn.
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    /// One datagram per send: GSO buys nothing at ICU's packet rates, and
    /// quinn then never hands over more than one.
    fn max_transmit_segments(&self) -> usize {
        1
    }

    /// quinn-udp turns GRO on by itself; quinn sizes its receive buffers by
    /// this, so it must be the inner socket's value.
    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }

    /// Decides whether quinn runs MTU discovery at all.
    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

/// A quinn endpoint on `socket` that speaks only with the com tag of `key`.
/// Datagrams dropped for their tag are added to `dropped`.
pub fn endpoint(
    socket: std::net::UdpSocket,
    key: &ComKey,
    side: Side,
    server_config: Option<quinn::ServerConfig>,
    dropped: Arc<AtomicU64>,
) -> io::Result<quinn::Endpoint> {
    let runtime = quinn::default_runtime()
        .ok_or_else(|| io::Error::other("no async runtime found for quinn"))?;
    let inner = runtime.wrap_udp_socket(socket)?;
    let socket = Arc::new(ComSocket::new(inner, key, side, dropped));
    quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        server_config,
        socket,
        runtime,
    )
}

/// quinn's MTU discovery with [`MTU_DISCOVERY_UPPER_BOUND`]. Both sides set
/// it: quinn only lowers its own bound to the peer's `max_udp_payload_size`.
pub fn mtu_discovery() -> quinn::MtuDiscoveryConfig {
    let mut config = quinn::MtuDiscoveryConfig::default();
    config.upper_bound(MTU_DISCOVERY_UPPER_BOUND);
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> ComKey {
        ComKey::from_bytes([byte; COM_KEY_LEN])
    }

    #[test]
    fn a_sealed_datagram_opens_on_the_other_side_only() {
        let client = ComKeys::new(&key(7), Side::Client);
        let server = ComKeys::new(&key(7), Side::Server);
        let datagram = b"quic initial".to_vec();

        let mut sealed = client.seal(&datagram, None);
        assert_eq!(sealed.len(), datagram.len() + TAG_LEN);
        let len = sealed.len();
        let opened = server.open_segments(&mut sealed, len, len);
        assert_eq!(opened.dropped, 0);
        assert_eq!(&sealed[..opened.len], &datagram[..]);

        // Mirrored back to its sender, it does not verify.
        let mut mirrored = client.seal(&datagram, None);
        let opened = client.open_segments(&mut mirrored, len, len);
        assert_eq!((opened.len, opened.dropped), (0, 1));
    }

    #[test]
    fn a_wrong_key_or_a_damaged_datagram_is_dropped() {
        let server = ComKeys::new(&key(7), Side::Server);
        let stranger = ComKeys::new(&key(8), Side::Client);
        let client = ComKeys::new(&key(7), Side::Client);

        let mut foreign = stranger.seal(b"hello", None);
        let len = foreign.len();
        assert_eq!(server.open_segments(&mut foreign, len, len).len, 0);

        let mut flipped = client.seal(b"hello", None);
        flipped[2] ^= 1;
        assert_eq!(server.open_segments(&mut flipped, len, len).len, 0);

        let mut cut = client.seal(b"hello", None);
        cut.pop();
        let cut_len = cut.len();
        assert_eq!(server.open_segments(&mut cut, cut_len, cut_len).len, 0);

        // No tag at all, and a tag with nothing in front of it.
        let mut plain = b"a plain quic packet".to_vec();
        let plain_len = plain.len();
        assert_eq!(
            server.open_segments(&mut plain, plain_len, plain_len).len,
            0
        );
        let mut empty = client.seal(b"", None);
        assert_eq!(empty.len(), TAG_LEN);
        assert_eq!(server.open_segments(&mut empty, TAG_LEN, TAG_LEN).len, 0);
    }

    /// Loopback never coalesces, so this is the only test of the GRO path:
    /// three segments, the last one short, the middle one forged.
    #[test]
    fn a_coalesced_buffer_keeps_its_good_segments_in_layout() {
        let client = ComKeys::new(&key(7), Side::Client);
        let stranger = ComKeys::new(&key(9), Side::Client);
        let server = ComKeys::new(&key(7), Side::Server);
        let a = vec![0xAA; 100];
        let b = vec![0xBB; 100];
        let c = vec![0xCC; 40];

        let mut buf = client.seal(&a, None);
        buf.extend(stranger.seal(&b, None));
        buf.extend(client.seal(&c, None));
        let stride = 100 + TAG_LEN;
        let len = buf.len();
        buf.resize(len + 64, 0);

        let opened = server.open_segments(&mut buf, len, stride);
        assert_eq!(opened.dropped, 1);
        assert_eq!(opened.stride, 100);
        assert_eq!(opened.len, 140);
        assert_eq!(&buf[..100], &a[..]);
        assert_eq!(&buf[100..140], &c[..]);

        // All three genuine: nothing dropped, same layout less the tags.
        let mut buf = client.seal(&[a.clone(), b.clone(), c.clone()].concat(), Some(100));
        let len = buf.len();
        let opened = server.open_segments(&mut buf, len, stride);
        assert_eq!((opened.len, opened.stride, opened.dropped), (240, 100, 0));
        assert_eq!(&buf[..240], &[a, b, c].concat()[..]);
    }

    #[test]
    fn a_segmented_send_tags_every_segment() {
        let client = ComKeys::new(&key(7), Side::Client);
        let sealed = client.seal(&[1u8; 250], Some(100));
        assert_eq!(sealed.len(), 250 + 3 * TAG_LEN);
        assert_eq!(&sealed[..100], &[1u8; 100][..]);
        assert_eq!(&sealed[100 + TAG_LEN..200 + TAG_LEN], &[1u8; 100][..]);
    }

    #[test]
    fn the_key_file_form_roundtrips_and_stays_out_of_logs() {
        let k = ComKey::generate().unwrap();
        assert_ne!(k, ComKey::generate().unwrap());
        assert_eq!(
            ComKey::from_base64(&format!("  {}\n", k.to_base64())),
            Some(k.clone())
        );
        assert_eq!(ComKey::from_base64("c2hvcnQ="), None);
        assert_eq!(ComKey::from_base64("not base64!"), None);
        assert_eq!(format!("{k:?}"), "ComKey(<redacted>)");
    }
}
