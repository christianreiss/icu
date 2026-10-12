//! ICU wire protocol v1.
//!
//! This crate is the single place where the wire format of
//! [`docs/reference/protocol.md`](docs/reference/protocol.md) is defined; server and client compile
//! the same crate. Nothing here does I/O except [`com::ComSocket`], a filter
//! around quinn's UDP socket.
//!
//! Layout:
//!
//! * [`types`] — `UserFlags`, `UserInfo`, `AudioParams`, name rules, SemVer
//! * [`control`] — `ControlMsg`
//! * [`framing`] — u16 LE length prefix, 16 KiB / 8 KiB limits
//! * [`header`] — audio and video headers plus the wrap arithmetic helpers
//! * [`error`] — error codes and the fatality table
//! * [`limits`] — the numbers from `docs/reference/protocol.md` §7 as constants
//! * [`passport`] — the armored identity bundle, also used for registration
//!   bundles (§2.3)
//! * [`com`] — the com key: an HMAC tag on every UDP datagram (§1)

#![forbid(unsafe_code)]

pub mod com;
pub mod control;
pub mod error;
pub mod framing;
pub mod header;
pub mod limits;
pub mod passport;
pub mod types;

pub mod audio_level;
pub use audio_level::AudioLevel;
pub use control::{ControlMsg, CAPS, CAP_AUDIO_FEEDBACK, MINOR, MIN_PEER_MINOR};
pub use framing::MAX_CONTROL_FRAME;
pub use header::{seq_diff, set_speaker_in_place, ts_diff, AudioHeader, VideoHeader};
pub use types::{
    client_version, is_plain_text, name_key, normalize_name, validate_name, validate_stored_name,
    AudioParams, NameError, Rgb, Session, UserFlags, UserId, UserInfo, UserSummary, Version,
    VersionError,
};

/// Everything that can go wrong while decoding or encoding wire data.
///
/// Every variant maps onto an error code from `docs/reference/protocol.md` §6 via
/// [`ProtoError::error_code`].
#[derive(Debug)]
pub enum ProtoError {
    /// A frame declared `len = 0`.
    EmptyFrame,
    /// A frame exceeded its limit.
    FrameTooLarge {
        /// Declared length.
        len: usize,
        /// Applicable limit.
        max: usize,
    },
    /// The enum discriminant is not known in this version (Spike 2b: postcard
    /// reports this as `SerdeDeCustom`).
    UnknownVariant,
    /// Any other deserialization failure.
    Decode(postcard::Error),
    /// Serialization failure.
    Encode(postcard::Error),
    /// A header was shorter than its fixed size.
    ShortHeader(usize),
    /// Unknown `kind` byte, i.e. a header version this build does not speak.
    BadKind(u8),
    /// Datagram or payload length outside the allowed window.
    BadPayloadLen(usize),
}

impl ProtoError {
    /// The error code this failure closes the connection with.
    ///
    /// All decode failures are `PROTOCOL_VIOLATION`; the caller decides
    /// whether a malformed *datagram* is dropped silently instead (it is,
    /// `docs/reference/protocol.md` §3).
    pub fn error_code(&self) -> u32 {
        error::PROTOCOL_VIOLATION
    }
}

impl core::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProtoError::EmptyFrame => write!(f, "frame length 0"),
            ProtoError::FrameTooLarge { len, max } => {
                write!(f, "frame length {len} exceeds limit {max}")
            }
            ProtoError::UnknownVariant => write!(f, "unknown message variant"),
            ProtoError::Decode(e) => write!(f, "decode error: {e}"),
            ProtoError::Encode(e) => write!(f, "encode error: {e}"),
            ProtoError::ShortHeader(n) => write!(f, "header too short: {n} bytes"),
            ProtoError::BadKind(k) => write!(f, "unknown header kind 0x{k:02x}"),
            ProtoError::BadPayloadLen(n) => write!(f, "payload length {n} out of range"),
        }
    }
}

impl std::error::Error for ProtoError {}
