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
    /// Contains an invisible, formatting, private-use, unassigned or
    /// separator character (bidi overrides, zero-width space, …).
    InvisibleChar,
    /// Leading, trailing or doubled whitespace, or whitespace other than
    /// U+0020.
    Whitespace,
    /// Not in Unicode Normalization Form C ([`normalize_name`] fixes it).
    NotNormalized,
    /// Starts with a combining mark.
    LeadingMark,
    /// Mixes scripts, e.g. Latin with a Cyrillic look-alike.
    MixedScript,
}

impl core::fmt::Display for NameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NameError::Empty => write!(f, "name is empty"),
            NameError::TooLong => write!(f, "name longer than {NAME_MAX_LEN} bytes"),
            NameError::ControlChar => write!(f, "name contains a control character"),
            NameError::InvisibleChar => write!(f, "name contains an invisible character"),
            NameError::Whitespace => write!(
                f,
                "name has leading, trailing, doubled or unusual whitespace"
            ),
            NameError::NotNormalized => write!(f, "name is not in Unicode NFC"),
            NameError::LeadingMark => write!(f, "name starts with a combining mark"),
            NameError::MixedScript => write!(f, "name mixes scripts"),
        }
    }
}

impl std::error::Error for NameError {}

/// The rules every name ever issued met: 1–32 bytes UTF-8, no control
/// characters. Enough for a name that is already stored and only carried
/// into a new certificate (plain renewal); a new name has to pass
/// [`validate_name`].
pub fn validate_stored_name(name: &str) -> Result<(), NameError> {
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

/// Validates a display name (`PROTOCOL.md` §2.1): 1–32 bytes UTF-8 in NFC,
/// no control, formatting, private-use, unassigned or separator characters,
/// single spaces (U+0020) only between words, no leading combining mark, and
/// a single script (UTS #39; digits and punctuation go with any).
pub fn validate_name(name: &str) -> Result<(), NameError> {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};
    use unicode_security::MixedScript;

    validate_stored_name(name)?;
    for c in name.chars() {
        match get_general_category(c) {
            G::Format
            | G::Surrogate
            | G::PrivateUse
            | G::Unassigned
            | G::LineSeparator
            | G::ParagraphSeparator => return Err(NameError::InvisibleChar),
            G::SpaceSeparator if c != ' ' => return Err(NameError::Whitespace),
            // Hangul fillers and friends render as nothing but are letters.
            _ if c.is_whitespace() && c != ' ' => return Err(NameError::Whitespace),
            _ if matches!(c, '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}') => {
                return Err(NameError::InvisibleChar)
            }
            _ => {}
        }
    }
    if name.starts_with(' ') || name.ends_with(' ') || name.contains("  ") {
        return Err(NameError::Whitespace);
    }
    let nfc = match is_nfc_quick(name.chars()) {
        IsNormalized::Yes => true,
        _ => name.chars().nfc().eq(name.chars()),
    };
    if !nfc {
        return Err(NameError::NotNormalized);
    }
    if name.chars().next().is_some_and(|c| {
        matches!(
            get_general_category(c),
            G::NonspacingMark | G::SpacingMark | G::EnclosingMark
        )
    }) {
        return Err(NameError::LeadingMark);
    }
    if !name.is_single_script() {
        return Err(NameError::MixedScript);
    }
    Ok(())
}

/// Brings typed input into the form [`validate_name`] expects: trimmed,
/// inner whitespace runs collapsed to one U+0020, NFC. It does not make an
/// invalid name valid, it only removes what a user cannot see.
pub fn normalize_name(input: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .nfc()
        .collect()
}

/// Lookup key for the uniqueness check: two names with the same key count as
/// the same name. Case-insensitive and confusable-insensitive: NFKC, lower
/// case, then the UTS #39 skeleton, so "Chris", "chris" and "Сhris" with a
/// Cyrillic "С" collide.
pub fn name_key(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let folded: String = name.nfkc().collect::<String>().to_lowercase();
    unicode_security::skeleton(&folded)
        .collect::<String>()
        .to_lowercase()
}

/// Per-user state bits (`PROTOCOL.md` §2.1).
///
/// bit0 muted, bit1 deafened, bit2 speaking, bit3 camera_on, bit4 admin,
/// bit5 viewing, bit6–7 reserved. Reserved bits are written as 0 by the
/// sender and ignored by the receiver.
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
    /// bit4 — the user holds the admin role. Server authoritative, mirrors
    /// `Store::is_admin`; rides the existing roster broadcasts
    /// (`Welcome`/`UserJoined`/`UserState`) instead of a dedicated message.
    pub const ADMIN: u8 = 1 << 4;
    /// bit5 — the user is looking at video right now (overlay on or main
    /// window visible). The server relays video only to viewers, and a
    /// sender pauses its camera while no peer has this bit set.
    pub const VIEWING: u8 = 1 << 5;
    /// bit6 — the user talks by Push-to-Talk; cleared means voice
    /// activation. Display only, so peers can show the mode.
    pub const PTT: u8 = 1 << 6;
    /// bit7 — reserved, sender writes 0.
    pub const RESERVED: u8 = 0x80;
    /// Bits a client may set through `SetState`: everything but `SPEAKING`,
    /// `ADMIN` and the reserved range.
    pub const CLIENT_SETTABLE: u8 =
        Self::MUTED | Self::DEAFENED | Self::CAMERA_ON | Self::VIEWING | Self::PTT;
    /// Bits the server keeps regardless of what a client requests.
    const SERVER_AUTHORITATIVE: u8 = Self::SPEAKING | Self::ADMIN;

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
    /// Whether the user holds the admin role.
    pub fn is_admin(self) -> bool {
        self.0 & Self::ADMIN != 0
    }
    /// Whether the user is looking at video right now.
    pub fn viewing(self) -> bool {
        self.0 & Self::VIEWING != 0
    }
    /// Whether the user talks by Push-to-Talk rather than voice activation.
    pub fn ptt(self) -> bool {
        self.0 & Self::PTT != 0
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
    /// `requested`, keeps the server authoritative `SPEAKING`/`ADMIN` bits and
    /// drops everything reserved (`PROTOCOL.md` §2.1, `Server/IMPLEMENTATION.md`
    /// §6.4).
    pub fn apply_client_request(self, requested: UserFlags) -> UserFlags {
        UserFlags((requested.0 & Self::CLIENT_SETTABLE) | (self.0 & Self::SERVER_AUTHORITATIVE))
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
    /// Invited, but the registration has not been confirmed yet
    /// (`PROTOCOL.md` §2.3).
    pub pending: bool,
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
    fn names_hide_nothing() {
        for ok in ["Chris", "Ärger", "neu-1a2b", "Anna Lena", "日本語", "Ωμέγα", "o'Neil 2"] {
            assert_eq!(validate_name(ok), Ok(()), "{ok:?}");
        }
        assert_eq!(validate_name("abc\u{202E}def"), Err(NameError::InvisibleChar));
        assert_eq!(validate_name("ad\u{200B}min"), Err(NameError::InvisibleChar));
        assert_eq!(validate_name("x\u{E000}"), Err(NameError::InvisibleChar));
        assert_eq!(validate_name("x\u{3164}"), Err(NameError::InvisibleChar));
        assert_eq!(validate_name("a\u{2028}b"), Err(NameError::InvisibleChar));
        assert_eq!(validate_name(" chris"), Err(NameError::Whitespace));
        assert_eq!(validate_name("chris "), Err(NameError::Whitespace));
        assert_eq!(validate_name("a  b"), Err(NameError::Whitespace));
        assert_eq!(validate_name("a\u{00A0}b"), Err(NameError::Whitespace));
        assert_eq!(validate_name("a\u{3000}b"), Err(NameError::Whitespace));
        assert_eq!(validate_name("\u{0301}a"), Err(NameError::LeadingMark));
        // Latin C, Cyrillic "hris": looks like "Chris".
        assert_eq!(validate_name("C\u{0445}ris"), Err(NameError::MixedScript));
        assert_eq!(validate_name("\u{0421}hris"), Err(NameError::MixedScript));
        assert_eq!(validate_name("Ωmega"), Err(NameError::MixedScript));
    }

    #[test]
    fn decomposed_names_are_normalized_first() {
        let nfd = "A\u{0308}rger";
        assert_eq!(validate_name(nfd), Err(NameError::NotNormalized));
        let fixed = normalize_name(nfd);
        assert_eq!(fixed, "Ärger");
        assert_eq!(validate_name(&fixed), Ok(()));
        assert_eq!(normalize_name("  Anna \t  Lena "), "Anna Lena");
    }

    #[test]
    fn look_alikes_share_a_key() {
        assert_eq!(name_key("Chris"), name_key("chris"));
        // Fully Cyrillic look-alike passes the script check, but is taken.
        assert_eq!(name_key("Chris"), name_key("\u{0421}\u{04BB}ris"));
        assert_eq!(name_key("admin"), name_key("adrnin"));
        assert_eq!(name_key("ｃｈｒｉｓ"), name_key("chris"));
        assert_ne!(name_key("chris"), name_key("chrissy"));
    }

    #[test]
    fn name_keys_are_case_insensitive() {
        assert_eq!(name_key("Alice"), name_key("alice"));
        assert_eq!(name_key("ÄRGER"), name_key("ärger"));
        assert_ne!(name_key("alice"), name_key("alicia"));
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
        assert!(out.muted() && out.deafened() && out.camera_on() && out.viewing() && out.ptt());

        // A client clearing everything does not clear SPEAKING.
        let out = current.apply_client_request(UserFlags::NONE);
        assert_eq!(out, UserFlags(UserFlags::SPEAKING));
    }

    #[test]
    fn client_request_cannot_set_or_clear_admin() {
        // An admin's own SetState, however crafted, can never strip ADMIN...
        let admin = UserFlags(UserFlags::ADMIN);
        let out = admin.apply_client_request(UserFlags::NONE);
        assert!(out.is_admin(), "server authoritative bit is kept");

        // ...and a non-admin cannot grant it to themselves either.
        let non_admin = UserFlags::NONE;
        let out = non_admin.apply_client_request(UserFlags(0xFF));
        assert!(!out.is_admin(), "a client cannot set ADMIN via SetState");
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
