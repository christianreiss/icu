//! Protocol limits the server enforces (`PROTOCOL.md` §7).

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
/// Handshakes per second and source IP (`Server/IMPLEMENTATION.md` §6.6).
pub const HANDSHAKES_PER_IP_PER_S: u32 = 10;
/// Handshakes in flight above which the server sends a Retry.
pub const HANDSHAKES_IN_FLIGHT_RETRY: usize = 32;
/// `KeyframeRequest` forwards per second and speaker.
pub const KEYFRAME_REQUESTS_PER_S: u32 = 1;

/// Seconds a client has to send `Hello` after the handshake.
pub const HELLO_TIMEOUT_S: u64 = 5;
/// Idle timeout, both directions.
pub const MAX_IDLE_TIMEOUT_S: u64 = 60;
/// Client keepalive interval.
pub const KEEP_ALIVE_INTERVAL_S: u64 = 15;
/// Datagram send and receive buffer size.
pub const DATAGRAM_BUFFER_BYTES: usize = 64 * 1024;

/// Silence after which a speaker is considered to have stopped, and below
/// which a new datagram starts a talk spurt (`PROTOCOL.md` §2.1).
pub const SPEAKING_SILENCE_MS: u64 = 500;
/// Time after which a `LossReport` expires (`PROTOCOL.md` §2.2).
pub const LOSS_REPORT_TTL_S: u64 = 6;
/// Cap for `LossHint` in percent, as handed to `OPUS_SET_PACKET_LOSS_PERC`.
pub const LOSS_HINT_MAX_PERCENT: u16 = 20;

/// Window over which a sender's per-receiver video slot displacements are
/// aggregated into a `VideoCongestionHint` (`PROTOCOL.md` §2.2).
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
/// How long a registration certificate stays valid by default.
pub const INVITE_TTL_S: u64 = 24 * 60 * 60;
/// Organizational unit of a registration certificate (`PROTOCOL.md` §2.3).
/// Informational: it lets a client tell a registration bundle from a
/// passport before connecting; the server decides by its own records.
pub const REGISTRATION_OU: &str = "ICU registration";
/// Longest lifetime of a registration certificate; longer requests are
/// clamped to it (`PROTOCOL.md` §2.3).
pub const MAX_INVITE_TTL_S: u64 = 7 * 24 * 60 * 60;

/// Bytes reserved per `UserInfo` when validating that a full `Welcome` fits
/// into one control frame (`PROTOCOL.md` §7).
pub const WELCOME_BYTES_PER_USER: usize = 100;
/// Fixed overhead reserved for the rest of `Welcome`.
pub const WELCOME_RESERVE: usize = 1024;

/// `max_concurrent_uni_streams` per direction (`PROTOCOL.md` §1).
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
