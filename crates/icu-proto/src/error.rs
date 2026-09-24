//! Error codes of the ICU wire protocol (`PROTOCOL.md` §6).
//!
//! The same numeric space is used in three places: the QUIC `CONNECTION_CLOSE`
//! code (varint), [`crate::ControlMsg::Error::code`] (u32) and
//! [`crate::ControlMsg::UserLeft::reason`] (u8). All defined codes fit in a u8.

/// Normal close, no error.
pub const NO_ERROR: u32 = 0x00;
/// Framing/decode error, wrong first message, limit exceeded. Fatal.
pub const PROTOCOL_VIOLATION: u32 = 0x01;
/// `Hello.minor` cannot be served. Fatal.
pub const VERSION_UNSUPPORTED: u32 = 0x02;
/// Certificate valid against the CA, but the fingerprint is unknown. Fatal.
pub const AUTH_UNKNOWN_CERT: u32 = 0x03;
/// Fingerprint revoked. Fatal.
pub const AUTH_REVOKED: u32 = 0x04;
/// `max_users` reached. Not fatal, the client reconnects with backoff.
pub const SERVER_FULL: u32 = 0x05;
/// Admin action (revoke with immediate close). Fatal.
pub const KICKED: u32 = 0x06;
/// No `Hello` within 5 s. Not fatal.
pub const HELLO_TIMEOUT: u32 = 0x07;
/// Server is shutting down. Not fatal.
pub const SHUTDOWN: u32 = 0x08;
/// Rate exceeded; sent as `Error`, the connection stays open. Not fatal.
pub const RATE_LIMITED: u32 = 0x09;
/// ALPN `icu/1` without a client certificate. Fatal.
pub const AUTH_REQUIRED: u32 = 0x0A;
/// Third `RATE_LIMITED` violation within 60 s; the server closes. Fatal.
pub const RATE_LIMITED_FATAL: u32 = 0x0B;
/// Idle timeout or transport abort. Not fatal.
pub const TIMEOUT: u32 = 0x0C;
/// A requested name — the subject CN of a renewal or registration CSR — is
/// already taken server-wide (case-insensitive). Not fatal: nothing changed and
/// the client may retry with another name. Arrives as `Error`.
pub const NAME_TAKEN: u32 = 0x0D;
/// Reserved: enrollment token unknown, used or expired. Only the removed
/// `icu-enroll/1` sent it. Fatal.
pub const ENROLL_BAD_TOKEN: u32 = 0x10;
/// CSR not parsable, signature invalid, or its subject does not carry exactly
/// one CN that passes `validate_name`. Fatal. A renewal reports these as
/// `RENEW_REJECTED` instead.
pub const ENROLL_BAD_CSR: u32 = 0x11;
/// Registration rate limit (5/min per IP, 20/min global): the server closes a
/// registration session with it. Fatal.
pub const ENROLL_RATE_LIMITED: u32 = 0x12;
/// Renewal CSR rejected. Not fatal.
pub const RENEW_REJECTED: u32 = 0x13;
/// Reserved: bootstrap enrollment (empty token) while the server already had
/// users. Only the removed `icu-enroll/1` sent it. Fatal.
pub const ENROLL_BOOTSTRAP_CLOSED: u32 = 0x14;
/// `CreateInviteRequest` from a non-admin user. Only ever sent as `Error`. Not
/// fatal.
pub const NOT_ADMIN: u32 = 0x15;
/// An admin action would demote, disable or revoke the last remaining
/// functional admin. Only ever sent as `AdminActionError`. Not fatal.
pub const LAST_ADMIN: u32 = 0x16;
/// The account is disabled (`UserRecord.disabled`). Checked at handshake,
/// same shape as `AUTH_REVOKED`; also the close code used to kick an online
/// user the moment they are disabled. Fatal.
pub const AUTH_DISABLED: u32 = 0x17;
/// An admin action named a `user_id` that does not exist. Only ever sent as
/// `AdminActionError`. Not fatal.
pub const NO_SUCH_USER: u32 = 0x18;
/// The same certificate connected again; the newer connection wins and the
/// older one is closed with this code (`PROTOCOL.md` §7). Fatal, so two
/// devices sharing a passport do not take turns kicking each other.
pub const SESSION_REPLACED: u32 = 0x19;
/// The receipt that ends a registration session: the client closes with this
/// code once it has stored its new identity, and only then does the server
/// revoke the bundle (`PROTOCOL.md` §2.3). Deliberately not `NO_ERROR`: quinn
/// closes a dropped connection implicitly with code 0, which must not count.
/// Only ever a client close code. Not fatal.
pub const REGISTRATION_STORED: u32 = 0x1A;
/// The presented certificate has expired: the session is renew-only and never
/// joins the room. Sent right after `Welcome` and in answer to anything but
/// `RenewRequest`/`Ping`; the client renews with its existing key and
/// reconnects with the new leaf (`PROTOCOL.md` §2.2). Not fatal.
pub const RENEW_REQUIRED: u32 = 0x1B;

/// All codes defined by v1, in ascending order.
pub const ALL_CODES: &[u32] = &[
    NO_ERROR,
    PROTOCOL_VIOLATION,
    VERSION_UNSUPPORTED,
    AUTH_UNKNOWN_CERT,
    AUTH_REVOKED,
    SERVER_FULL,
    KICKED,
    HELLO_TIMEOUT,
    SHUTDOWN,
    RATE_LIMITED,
    AUTH_REQUIRED,
    RATE_LIMITED_FATAL,
    TIMEOUT,
    NAME_TAKEN,
    ENROLL_BAD_TOKEN,
    ENROLL_BAD_CSR,
    ENROLL_RATE_LIMITED,
    RENEW_REJECTED,
    ENROLL_BOOTSTRAP_CLOSED,
    NOT_ADMIN,
    LAST_ADMIN,
    AUTH_DISABLED,
    NO_SUCH_USER,
    SESSION_REPLACED,
    REGISTRATION_STORED,
    RENEW_REQUIRED,
];

/// Fatal codes (`PROTOCOL.md` §5/§6): the client must not reconnect.
///
/// `NO_ERROR` is neither fatal nor a reconnect reason — it is listed as `–` in
/// the table and handled by the caller (a clean close is a clean close).
pub fn is_fatal(code: u32) -> bool {
    matches!(
        code,
        PROTOCOL_VIOLATION
            | VERSION_UNSUPPORTED
            | AUTH_UNKNOWN_CERT
            | AUTH_REVOKED
            | KICKED
            | RATE_LIMITED_FATAL
            | ENROLL_BAD_TOKEN
            | ENROLL_BAD_CSR
            | ENROLL_RATE_LIMITED
            | ENROLL_BOOTSTRAP_CLOSED
            | AUTH_DISABLED
            | SESSION_REPLACED
    )
}

/// Short, stable name of a code, for logs and the admin socket.
pub fn name(code: u32) -> &'static str {
    match code {
        NO_ERROR => "NO_ERROR",
        PROTOCOL_VIOLATION => "PROTOCOL_VIOLATION",
        VERSION_UNSUPPORTED => "VERSION_UNSUPPORTED",
        AUTH_UNKNOWN_CERT => "AUTH_UNKNOWN_CERT",
        AUTH_REVOKED => "AUTH_REVOKED",
        SERVER_FULL => "SERVER_FULL",
        KICKED => "KICKED",
        HELLO_TIMEOUT => "HELLO_TIMEOUT",
        SHUTDOWN => "SHUTDOWN",
        RATE_LIMITED => "RATE_LIMITED",
        AUTH_REQUIRED => "AUTH_REQUIRED",
        RATE_LIMITED_FATAL => "RATE_LIMITED_FATAL",
        TIMEOUT => "TIMEOUT",
        NAME_TAKEN => "NAME_TAKEN",
        ENROLL_BAD_TOKEN => "ENROLL_BAD_TOKEN",
        ENROLL_BAD_CSR => "ENROLL_BAD_CSR",
        ENROLL_RATE_LIMITED => "ENROLL_RATE_LIMITED",
        RENEW_REJECTED => "RENEW_REJECTED",
        ENROLL_BOOTSTRAP_CLOSED => "ENROLL_BOOTSTRAP_CLOSED",
        NOT_ADMIN => "NOT_ADMIN",
        LAST_ADMIN => "LAST_ADMIN",
        AUTH_DISABLED => "AUTH_DISABLED",
        NO_SUCH_USER => "NO_SUCH_USER",
        SESSION_REPLACED => "SESSION_REPLACED",
        REGISTRATION_STORED => "REGISTRATION_STORED",
        RENEW_REQUIRED => "RENEW_REQUIRED",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PROTOCOL.md` §6: exactly these codes are fatal.
    #[test]
    fn fatal_table_matches_protocol() {
        let expected_fatal: &[u32] = &[
            PROTOCOL_VIOLATION,
            VERSION_UNSUPPORTED,
            AUTH_UNKNOWN_CERT,
            AUTH_REVOKED,
            KICKED,
            RATE_LIMITED_FATAL,
            ENROLL_BAD_TOKEN,
            ENROLL_BAD_CSR,
            ENROLL_RATE_LIMITED,
            ENROLL_BOOTSTRAP_CLOSED,
            AUTH_DISABLED,
            SESSION_REPLACED,
        ];
        for code in ALL_CODES {
            assert_eq!(
                is_fatal(*code),
                expected_fatal.contains(code),
                "fatality mismatch for {}",
                name(*code)
            );
        }
    }

    #[test]
    fn all_codes_fit_in_u8_and_are_named() {
        for code in ALL_CODES {
            assert!(*code < 256, "{} does not fit in UserLeft.reason", code);
            assert_ne!(name(*code), "UNKNOWN");
        }
        assert_eq!(name(0xFF), "UNKNOWN");
        assert!(!is_fatal(0xFF));
    }
}
