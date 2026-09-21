//! ICU wire protocol v1.
//!
//! This crate is the single place where the wire format of
//! [`PROTOCOL.md`](../../../PROTOCOL.md) is defined; server and client compile
//! the same crate. Nothing here does I/O.
//!
//! Layout:
//!
//! * [`types`] — `UserFlags`, `UserInfo`, `AudioParams`, name rules, SemVer
//! * [`control`] — `ControlMsg`, `EnrollMsg`
//! * [`framing`] — u16 LE length prefix, 16 KiB / 8 KiB limits
//! * [`header`] — audio and video headers plus the wrap arithmetic helpers
//! * [`error`] — error codes and the fatality table
//! * [`limits`] — the numbers from `PROTOCOL.md` §7 as constants
//! * [`invite_blob`] — the self-contained `ICU1-…` invite format (§2.3.1)

#![forbid(unsafe_code)]

pub mod control;
pub mod error;
pub mod framing;
pub mod header;
pub mod invite_blob;
pub mod limits;
pub mod types;

pub use control::{ControlMsg, EnrollMsg, CAPS, MINOR};
pub use framing::{MAX_CONTROL_FRAME, MAX_ENROLL_FRAME};
pub use header::{seq_diff, set_speaker_in_place, ts_diff, AudioHeader, VideoHeader};
pub use types::{
    name_key, validate_name, AudioParams, NameError, Session, UserFlags, UserId, UserInfo,
    UserSummary, Version, VersionError,
};

/// Everything that can go wrong while decoding or encoding wire data.
///
/// Every variant maps onto an error code from `PROTOCOL.md` §6 via
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
    /// `PROTOCOL.md` §3).
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
