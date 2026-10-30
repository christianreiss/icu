//! Protocol limits the server enforces (`docs/reference/protocol.md` §7).

/// ALPN of the main protocol. Major version gate.
pub const ALPN_MAIN: &[u8] = b"icu/1";

/// Default UDP port.
pub const DEFAULT_PORT: u16 = 4711;

/// Default `max_users`.
pub const DEFAULT_MAX_USERS: u16 = 16;
/// Hard upper bound for `max_users`.
pub const MAX_MAX_USERS: u16 = 64;

/// Control messages per second and client.
pub const CONTROL_MSGS_PER_S: u32 = 50;
/// `SetState` messages per second and client.
pub const SET_STATE_PER_S: u32 = 20;
/// Audio datagrams per second and client.
pub const AUDIO_DATAGRAMS_PER_S: u32 = 110;
/// Video frames per second and client.
pub const VIDEO_FRAMES_PER_S: u32 = 20;
/// Video bitrate ceiling per sender, sliding over 2 s.
pub const VIDEO_KBIT_S: u32 = 400;
/// Registration sessions per minute and source IP.
pub const ENROLL_PER_IP_PER_MIN: u32 = 5;
/// Registration sessions per minute, server wide.
pub const ENROLL_GLOBAL_PER_MIN: u32 = 20;
/// Handshakes per second and source IP (`docs/reference/server.md` §6.6).
pub const HANDSHAKES_PER_IP_PER_S: u32 = 10;
/// Handshakes per second, server wide. Caps what many source addresses
/// together can make the server spend on TLS.
pub const HANDSHAKES_GLOBAL_PER_S: u32 = 100;
/// Handshakes in flight from which on a source that has not proven its
/// address gets a Retry instead of a handshake.
pub const HANDSHAKES_IN_FLIGHT_RETRY: usize = 32;
/// Handshakes in flight, over all listen addresses, above which the server
/// refuses new ones outright.
pub const MAX_HANDSHAKES: u32 = 256;
/// Handshakes in flight per validated source (IPv4 address, IPv6 /64). A
/// source that answers the Retry and then stalls holds at most this many.
pub const HANDSHAKES_IN_FLIGHT_PER_IP: u32 = 4;
/// Handshakes in flight per IPv6 /48 over all its /64s: a single customer
/// usually holds a whole /48.
pub const HANDSHAKES_IN_FLIGHT_PER_48: u32 = 16;
/// Established connections above which a completed handshake is closed with
/// `SERVER_FULL`. A backstop: outsiders cannot keep a connection past the
/// handshake, and each certificate holds one at most.
pub const MAX_CONNECTIONS: u32 = 256;
/// Sessions outside the room — registration and renew-only — open at the
/// same time, server wide. Each holds a connection without joining the room,
/// so the room size does not bound them.
pub const MAX_ROOMLESS_SESSIONS: usize = 8;
/// Seconds a QUIC handshake may take before the server drops it. Without
/// this, a half-open handshake lives for the whole idle timeout.
pub const HANDSHAKE_TIMEOUT_S: u64 = 5;
/// IPv6 sources are rate limited per prefix of this length: a single host
/// usually holds a whole /64, so per-address buckets would not limit it.
pub const IPV6_LIMIT_PREFIX: u8 = 64;
/// `KeyframeRequest` forwards per second and speaker.
pub const KEYFRAME_REQUESTS_PER_S: u32 = 1;

/// Seconds a client has to send `Hello` after the handshake.
pub const HELLO_TIMEOUT_S: u64 = 5;
/// Idle timeout, both directions.
pub const MAX_IDLE_TIMEOUT_S: u64 = 60;
/// Client keepalive interval.
pub const KEEP_ALIVE_INTERVAL_S: u64 = 15;
/// Bytes a peer may send on one stream before the server has read them
/// (QUIC stream flow control). Flow control, not a size limit: a 512 KiB
/// video frame still flows, in windows of this size.
pub const STREAM_RECEIVE_WINDOW: u32 = 64 * 1024;
/// Bytes a peer may send over all streams of a connection before the server
/// has read them. Caps what a connection can pin in server memory, streams
/// the server never reads included (quinn's default is unbounded).
pub const CONNECTION_RECEIVE_WINDOW: u32 = 1024 * 1024;
/// Bytes the server holds for one connection before its writes wait:
/// unacknowledged video and control data towards one client.
pub const SEND_WINDOW: u64 = 1024 * 1024;
/// Video frames one sender may have in flight on the server at a time; a
/// further stream is stopped with `RATE_LIMITED` right away.
pub const VIDEO_STREAMS_PER_SENDER: usize = 4;
/// Video frames the server keeps unacknowledged per sender-receiver pair: a
/// further frame waits in the pair's slot (one deep) until the receiver has
/// acknowledged the oldest. Bounds how far a receiver with too narrow a
/// downlink falls behind to about this many frames, instead of whatever the
/// stream credit and send window hold. At 15 fps, 2 cover a round trip
/// (plus ACK delay) of up to about 130 ms without loss; beyond that the pair
/// drops frames, which `VideoCongestionHint` reports (`docs/reference/protocol.md` §4).
pub const VIDEO_UNACKED_PER_PAIR: usize = 2;
/// Seconds a video frame may take to arrive, header and payload, before the
/// server stops its stream with `RATE_LIMITED`.
pub const VIDEO_FRAME_TIMEOUT_S: u64 = 5;
/// Datagram receive buffer size on both sides, and the server's send buffer
/// (its send queue carries every speaker's audio to one receiver).
pub const DATAGRAM_BUFFER_BYTES: usize = 64 * 1024;
/// The client's datagram send buffer: only its own audio queues here, and
/// quinn drops the oldest datagram once it is full. 8 KiB is 20–50 packets
/// (0.4–1 s); the receiver's jitter buffer caps at 200 ms and drains any
/// post-stall backlog anyway, so queueing more only sends stale audio late
/// and bursts past the server's per-second rate limit (`docs/reference/protocol.md` §1).
pub const CLIENT_DATAGRAM_SEND_BUFFER_BYTES: usize = 8 * 1024;
/// Maximum residence of audio in each outgoing QUIC datagram queue.
/// Enforced by the vendored Quinn deadline at packet construction, independently
/// of payload size, speaker count, and whether more packets are enqueued.
pub const AUDIO_QUEUE_MAX_AGE_MS: u64 = 100;

/// Silence after which a speaker is considered to have stopped, and below
/// which a new datagram starts a talk spurt (`docs/reference/protocol.md` §2.1).
pub const SPEAKING_SILENCE_MS: u64 = 500;
/// Time after which a `LossReport` expires (`docs/reference/protocol.md` §2.2).
pub const LOSS_REPORT_TTL_S: u64 = 6;
/// Cap for `LossHint` in percent, as handed to `OPUS_SET_PACKET_LOSS_PERC`.
pub const LOSS_HINT_MAX_PERCENT: u16 = 20;

/// Window over which a sender's per-receiver video slot displacements are
/// aggregated into a `VideoCongestionHint` (`docs/reference/protocol.md` §2.2).
pub const VIDEO_CONGESTION_WINDOW_S: u64 = 2;
/// Minimum change, in per mille, before a new `VideoCongestionHint` is sent.
pub const VIDEO_CONGESTION_HINT_STEP_PERMILLE: u16 = 20;

/// Rate limit violations within [`RATE_VIOLATION_WINDOW_S`] that turn into a
/// fatal close.
pub const RATE_VIOLATIONS_FATAL: u32 = 3;
/// Window for counting rate limit violations.
pub const RATE_VIOLATION_WINDOW_S: u64 = 60;

/// Certificate renewals per day and `user_id`.
pub const RENEW_PER_DAY: u32 = 1;
/// Registration bundles one admin may mint per hour over the authenticated
/// connection (`CreateInviteRequest`). The host's CLI is not limited.
pub const INVITES_PER_ADMIN_PER_HOUR: u32 = 20;
/// How long a registration certificate stays valid by default.
pub const INVITE_TTL_S: u64 = 24 * 60 * 60;
/// Organizational unit of a registration certificate (`docs/reference/protocol.md` §2.3).
/// Informational: it lets a client tell a registration bundle from a
/// passport before connecting; the server decides by its own records.
pub const REGISTRATION_OU: &str = "ICU registration";
/// Longest lifetime of a registration certificate; longer requests are
/// clamped to it (`docs/reference/protocol.md` §2.3).
pub const MAX_INVITE_TTL_S: u64 = 7 * 24 * 60 * 60;

/// Bytes reserved per `UserInfo` when validating that a full `Welcome` fits
/// into one control frame (`docs/reference/protocol.md` §7).
pub const WELCOME_BYTES_PER_USER: usize = 100;
/// Fixed overhead reserved for the rest of `Welcome`.
pub const WELCOME_RESERVE: usize = 1024;

/// `max_concurrent_uni_streams` per direction (`docs/reference/protocol.md` §1).
pub fn max_uni_streams(max_users: u16) -> u32 {
    2 * u32::from(max_users) + 4
}

/// Whether a full `Welcome` for `max_users` participants still fits into one
/// control frame.
pub fn welcome_fits(max_users: u16) -> bool {
    usize::from(max_users) * WELCOME_BYTES_PER_USER + WELCOME_RESERVE
        < crate::framing::MAX_CONTROL_FRAME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uni_stream_formula() {
        assert_eq!(max_uni_streams(16), 36);
        assert_eq!(max_uni_streams(64), 132);
    }

    #[test]
    fn welcome_fits_up_to_the_hard_cap() {
        assert!(welcome_fits(DEFAULT_MAX_USERS));
        assert!(welcome_fits(MAX_MAX_USERS));
        assert!(!welcome_fits(1000));
    }
}
