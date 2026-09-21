//! Shared wire types of the control channel (`PROTOCOL.md` §2.1).

use serde::{Deserialize, Serialize};

/// Server assigned connection id, `1..`, monotonic per server run, never
/// reused. `0` means "unknown/me".
pub type Session = u16;

/// Identity assigned at enrollment, `1..`, monotonic, never reused. Stable
/// across certificate renewals, reconnects and server restarts. `0` means
/// "unknown".
pub type UserId = u32;

/// Minimum display name length in bytes.
pub const NAME_MIN_LEN: usize = 1;
/// Maximum display name length in bytes.
pub const NAME_MAX_LEN: usize = 32;

/// Why a display name was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    /// Empty name.
    Empty,
    /// More than [`NAME_MAX_LEN`] bytes.
    TooLong,
    /// Contains a Unicode control character.
    ControlChar,
}

impl core::fmt::Display for NameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NameError::Empty => write!(f, "name is empty"),
            NameError::TooLong => write!(f, "name longer than {NAME_MAX_LEN} bytes"),
            NameError::ControlChar => write!(f, "name contains a control character"),
        }
    }
}

impl std::error::Error for NameError {}

/// Validates a display name: 1–32 bytes UTF-8, no control characters.
pub fn validate_name(name: &str) -> Result<(), NameError> {
    if name.len() < NAME_MIN_LEN {
        return Err(NameError::Empty);
    }
    if name.len() > NAME_MAX_LEN {
        return Err(NameError::TooLong);
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(NameError::ControlChar);
    }
    Ok(())
}

/// Case-insensitive name comparison, as used for server wide uniqueness.
pub fn name_eq_ci(a: &str, b: &str) -> bool {
    name_key(a) == name_key(b)
}

/// Normalized lookup key for the case-insensitive uniqueness check.
pub fn name_key(name: &str) -> String {
    name.to_lowercase()
}

/// Per-user state bits (`PROTOCOL.md` §2.1).
///
/// bit0 muted, bit1 deafened, bit2 speaking, bit3 camera_on, bit4–7 reserved.
/// Reserved bits are written as 0 by the sender and ignored by the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserFlags(pub u8);

impl UserFlags {
    /// bit0 — the user's microphone is muted.
    pub const MUTED: u8 = 1 << 0;
    /// bit1 — the user receives no audio.
    pub const DEAFENED: u8 = 1 << 1;
    /// bit2 — the user is currently speaking. Server authoritative.
    pub const SPEAKING: u8 = 1 << 2;
    /// bit3 — the user's camera is on.
    pub const CAMERA_ON: u8 = 1 << 3;
    /// bit4–7 — reserved, sender writes 0.
    pub const RESERVED: u8 = 0xF0;
    /// Bits a client may set through `SetState`: everything but `SPEAKING`
    /// and the reserved range.
    pub const CLIENT_SETTABLE: u8 = Self::MUTED | Self::DEAFENED | Self::CAMERA_ON;

    /// All bits cleared.
    pub const NONE: UserFlags = UserFlags(0);

    /// Whether the microphone is muted.
    pub fn muted(self) -> bool {
        self.0 & Self::MUTED != 0
    }
    /// Whether the user receives no audio.
    pub fn deafened(self) -> bool {
        self.0 & Self::DEAFENED != 0
    }
    /// Whether the server currently considers the user to be speaking.
    pub fn speaking(self) -> bool {
        self.0 & Self::SPEAKING != 0
    }
    /// Whether the camera is on.
    pub fn camera_on(self) -> bool {
        self.0 & Self::CAMERA_ON != 0
    }

    /// Returns a copy with `SPEAKING` set or cleared.
    pub fn with_speaking(self, on: bool) -> UserFlags {
        self.with_bit(Self::SPEAKING, on)
    }

    /// Returns a copy with `bit` set or cleared.
    pub fn with_bit(self, bit: u8, on: bool) -> UserFlags {
        if on {
            UserFlags(self.0 | bit)
        } else {
            UserFlags(self.0 & !bit)
        }
    }

    /// Applies a client requested state: takes the client settable bits from
    /// `requested`, keeps the server authoritative `SPEAKING` bit and drops
    /// everything reserved (`PROTOCOL.md` §2.1, `Server/IMPLEMENTATION.md`
    /// §6.4).
    pub fn apply_client_request(self, requested: UserFlags) -> UserFlags {
        UserFlags((requested.0 & Self::CLIENT_SETTABLE) | (self.0 & Self::SPEAKING))
    }

    /// Clears the reserved bits. Used on the sending side only; receivers must
    /// never reject a message because of them.
    pub fn sanitized(self) -> UserFlags {
        UserFlags(self.0 & !Self::RESERVED)
    }
}

/// One participant as seen on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    /// Stable identity, assigned at enrollment.
    pub user_id: UserId,
    /// Connection scoped id.
    pub session: Session,
    /// Display name, 1–32 bytes.
    pub name: String,
    /// State bits.
    pub flags: UserFlags,
}

/// One row of the admin User Manager list (`ControlMsg::UserListResponse`,
/// `PROTOCOL.md` §2.2). Lighter than the admin-socket's `AdminUser`: no
/// fingerprint/issued/expires, the manager only needs role and status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserSummary {
    /// Stable identity, assigned at enrollment.
    pub user_id: UserId,
    /// Display name.
    pub name: String,
    /// When the user was created, RFC 3339 UTC.
    pub created: String,
    /// Whether the user may send admin-gated requests.
    pub is_admin: bool,
    /// Reversible login block, independent of `revoked`.
    pub disabled: bool,
    /// Whether every certificate of this user is revoked.
    pub revoked: bool,
    /// Whether the user is connected right now.
    pub online: bool,
}

/// Audio parameters announced in `Welcome` (`PROTOCOL.md` §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioParams {
    /// Sample rate in Hz. v1: 48000.
    pub sample_rate: u32,
    /// Frame length in milliseconds. v1 default: 20.
    pub frame_ms: u8,
    /// Upper bitrate bound in bit/s. v1: 64000.
    pub max_bitrate: u32,
}

impl Default for AudioParams {
    fn default() -> Self {
        AudioParams {
            sample_rate: 48_000,
            frame_ms: 20,
            max_bitrate: 64_000,
        }
    }
}

/// A minimal SemVer triple, enough for `Welcome.min_client`.
///
/// Deliberately not the `semver` crate: the server dependency list in
/// `Server/IMPLEMENTATION.md` §9 does not contain it, and only
/// `major.minor.patch` ordering is needed. Pre-release and build metadata are
/// parsed off and ignored for ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    /// Major version.
    pub major: u64,
    /// Minor version.
    pub minor: u64,
    /// Patch version.
    pub patch: u64,
}

impl Version {
    /// Parses `major.minor.patch` with an optional `-pre` / `+build` suffix.
    pub fn parse(s: &str) -> Result<Version, VersionError> {
        let core = s.split(['-', '+']).next().unwrap_or("");
        let mut parts = core.split('.');
        let mut next = || -> Result<u64, VersionError> {
            parts
                .next()
                .ok_or(VersionError)?
                .parse::<u64>()
                .map_err(|_| VersionError)
        };
        let major = next()?;
        let minor = next()?;
        let patch = next()?;
        if parts.next().is_some() {
            return Err(VersionError);
        }
        Ok(Version {
            major,
            minor,
            patch,
        })
    }
}

impl core::fmt::Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A string that is not a `major.minor.patch` SemVer version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionError;

impl core::fmt::Display for VersionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "not a semver version")
    }
}

impl std::error::Error for VersionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        assert!(validate_name("alice").is_ok());
        assert_eq!(validate_name(""), Err(NameError::Empty));
        assert_eq!(validate_name(&"x".repeat(33)), Err(NameError::TooLong));
        assert!(validate_name(&"x".repeat(32)).is_ok());
        assert_eq!(validate_name("a\nb"), Err(NameError::ControlChar));
        assert_eq!(validate_name("a\u{7f}b"), Err(NameError::ControlChar));
        // 32 bytes, not 32 chars.
        assert_eq!(validate_name(&"ä".repeat(17)), Err(NameError::TooLong));
        assert!(validate_name(&"ä".repeat(16)).is_ok());
    }

    #[test]
    fn name_comparison_is_case_insensitive() {
        assert!(name_eq_ci("Alice", "alice"));
        assert!(name_eq_ci("ÄRGER", "ärger"));
        assert!(!name_eq_ci("alice", "alicia"));
    }

    #[test]
    fn user_flags_bits() {
        let f = UserFlags(UserFlags::MUTED | UserFlags::CAMERA_ON);
        assert!(f.muted() && f.camera_on() && !f.deafened() && !f.speaking());
        assert!(f.with_speaking(true).speaking());
        assert!(!f.with_speaking(true).with_speaking(false).speaking());
    }

    #[test]
    fn client_request_cannot_set_speaking_or_reserved() {
        let current = UserFlags(UserFlags::SPEAKING);
        let requested = UserFlags(0xFF);
        let out = current.apply_client_request(requested);
        assert_eq!(out.0 & UserFlags::RESERVED, 0);
        assert!(out.speaking(), "server authoritative bit is kept");
        assert!(out.muted() && out.deafened() && out.camera_on());

        // A client clearing everything does not clear SPEAKING.
        let out = current.apply_client_request(UserFlags::NONE);
        assert_eq!(out, UserFlags(UserFlags::SPEAKING));
    }

    #[test]
    fn version_parse_and_order() {
        assert_eq!(
            Version::parse("0.1.0").unwrap(),
            Version {
                major: 0,
                minor: 1,
                patch: 0
            }
        );
        assert!(Version::parse("0.2.0").unwrap() > Version::parse("0.1.9").unwrap());
        assert!(Version::parse("1.0.0").unwrap() > Version::parse("0.99.99").unwrap());
        assert_eq!(Version::parse("1.2.3-rc.1").unwrap().patch, 3);
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1.2.3.4").is_err());
        assert!(Version::parse("x.y.z").is_err());
        assert_eq!(Version::parse("1.2.3").unwrap().to_string(), "1.2.3");
    }
}
