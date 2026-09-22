//! ASCII-armored identity bundle for moving a client to another device
//! (`PROTOCOL.md` §2.3.2).
//!
//! Everything `Identity::load` would read off disk — private key, leaf, CA and
//! the server coordinates — in one block a user can copy into a second
//! installation. It lives here rather than in the client because it is an
//! interchange format between two client installs of possibly different
//! versions, and because the armor conventions and the postcard/base64
//! dependencies already live in this crate next to `invite_blob`.
//!
//! **No server code path constructs, accepts or parses a passport**, and none
//! should: it is a file the user is responsible for, never a transport format.
//!
//! The body is **not encrypted**. A passport is a bearer credential for the
//! account until the certificate expires or an admin revokes it — whoever reads
//! the text can sign in as that user. The client warns accordingly and writes
//! the file with mode 0600.

use serde::{Deserialize, Serialize};

/// Armor header line.
pub const ARMOR_BEGIN: &str = "-----BEGIN ICU PASSPORT-----";
/// Armor footer line.
pub const ARMOR_END: &str = "-----END ICU PASSPORT-----";

/// Format version, the first byte of the decoded body.
///
/// It sits *outside* the postcard body on purpose: postcard is not
/// self-describing, so a future v2 with a changed field would otherwise decode
/// as a plausible-looking v1 instead of being rejected.
pub const PASSPORT_VERSION: u8 = 1;

/// Upper bound on the decoded body. A real bundle is 1–1.5 KiB; the headroom
/// covers a longer key or CA without inviting a decode bomb.
pub const MAX_PASSPORT_BYTES: usize = 8192;

/// Base64 line width inside the armor.
const ARMOR_COLUMNS: usize = 64;

/// A whole ICU identity, portable to another device.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassportV1 {
    /// Host to dial.
    pub host: String,
    /// Port to dial.
    pub port: u16,
    /// SNI for the main connection.
    pub server_name: String,
    /// CN of `leaf_der` at export time. Informational, for the import preview;
    /// the certificate stays the authority on the name.
    pub name: String,
    /// Private key, PKCS#8 DER.
    pub key_der: Vec<u8>,
    /// Client leaf certificate, DER.
    pub leaf_der: Vec<u8>,
    /// The server's CA certificate, DER.
    pub ca_der: Vec<u8>,
}

/// `key_der` is redacted: `Command` and friends derive `Debug`, and a passport
/// must never end up in a log line.
impl std::fmt::Debug for PassportV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassportV1")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("server_name", &self.server_name)
            .field("name", &self.name)
            .field("key_der", &format_args!("<redacted, {} bytes>", self.key_der.len()))
            .field("leaf_der", &format_args!("{} bytes", self.leaf_der.len()))
            .field("ca_der", &format_args!("{} bytes", self.ca_der.len()))
            .finish()
    }
}

/// Why a passport could not be decoded.
#[derive(Debug)]
pub enum PassportError {
    /// No [`ARMOR_BEGIN`]/[`ARMOR_END`] pair, or they are out of order.
    BadArmor,
    /// The armored body is not valid base64.
    BadBase64,
    /// The decoded body exceeds [`MAX_PASSPORT_BYTES`].
    TooLarge,
    /// The body carries a version this build does not know.
    UnsupportedVersion(u8),
    /// The decoded body is empty or not a valid `PassportV1`.
    Malformed,
}

impl std::fmt::Display for PassportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PassportError::BadArmor => write!(f, "missing {ARMOR_BEGIN} / {ARMOR_END}"),
            PassportError::BadBase64 => write!(f, "not valid base64"),
            PassportError::TooLarge => write!(f, "passport exceeds {MAX_PASSPORT_BYTES} bytes"),
            PassportError::UnsupportedVersion(v) => write!(f, "unsupported passport version {v}"),
            PassportError::Malformed => write!(f, "not a valid passport"),
        }
    }
}

impl std::error::Error for PassportError {}

impl PassportV1 {
    /// The full armored block, ready to copy or write to a file. Ends with a
    /// newline so concatenating one into a file needs no fixup.
    pub fn encode(&self) -> String {
        use base64::Engine;
        let mut body = Vec::with_capacity(1 + 1024);
        body.push(PASSPORT_VERSION);
        body.extend_from_slice(&postcard::to_stdvec(self).unwrap_or_default());
        let b64 = base64::engine::general_purpose::STANDARD.encode(body);

        let mut out = String::with_capacity(b64.len() + b64.len() / ARMOR_COLUMNS + 128);
        out.push_str(ARMOR_BEGIN);
        out.push('\n');
        for chunk in b64.as_bytes().chunks(ARMOR_COLUMNS) {
            // `chunks` on a base64 string always splits on char boundaries.
            out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
            out.push('\n');
        }
        out.push_str(ARMOR_END);
        out.push('\n');
        out
    }

    /// Decodes a full armored block.
    ///
    /// Tolerant about whitespace: a pasted passport may arrive re-wrapped, with
    /// CRLF line endings, or indented by a chat client, and all of that decodes
    /// to the same value.
    pub fn decode(armored: &str) -> Result<PassportV1, PassportError> {
        use base64::Engine;
        let start = armored.find(ARMOR_BEGIN).ok_or(PassportError::BadArmor)?;
        let after = start + ARMOR_BEGIN.len();
        let end = armored[after..]
            .find(ARMOR_END)
            .ok_or(PassportError::BadArmor)?
            + after;
        let b64: String = armored[after..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let body = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|_| PassportError::BadBase64)?;
        if body.len() > MAX_PASSPORT_BYTES {
            return Err(PassportError::TooLarge);
        }
        let (version, rest) = body.split_first().ok_or(PassportError::Malformed)?;
        if *version != PASSPORT_VERSION {
            return Err(PassportError::UnsupportedVersion(*version));
        }
        postcard::from_bytes(rest).map_err(|_| PassportError::Malformed)
    }

    /// Cheap sniff so a paste field can tell a passport from an invite before
    /// trying to parse either.
    pub fn looks_like(text: &str) -> bool {
        text.trim_start().starts_with(ARMOR_BEGIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PassportV1 {
        PassportV1 {
            host: "icu.example.org".into(),
            port: 4711,
            server_name: "icu.example.org".into(),
            name: "alice".into(),
            key_der: (0..200u32).map(|i| i as u8).collect(),
            leaf_der: (0..318u32).map(|i| (i % 251) as u8).collect(),
            ca_der: (0..366u32).map(|i| (i % 253) as u8).collect(),
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let p = sample();
        let armored = p.encode();
        assert!(armored.starts_with(ARMOR_BEGIN));
        assert!(armored.trim_end().ends_with(ARMOR_END));
        for line in armored.lines() {
            assert!(line.len() <= ARMOR_COLUMNS.max(ARMOR_BEGIN.len()));
        }
        assert_eq!(PassportV1::decode(&armored).unwrap(), p);
        assert!(PassportV1::looks_like(&armored));
        assert!(PassportV1::looks_like(&format!("\n  {armored}")));
        assert!(!PassportV1::looks_like("icu://host:4711/enroll?t=x"));
    }

    #[test]
    fn whitespace_is_tolerated() {
        let p = sample();
        let armored = p.encode();
        let crlf = armored.replace('\n', "\r\n");
        assert_eq!(PassportV1::decode(&crlf).unwrap(), p);

        // Re-wrapped at a different width and indented, the way a chat client
        // might hand it back.
        let body: String = armored
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect::<String>();
        let rewrapped: String = body
            .as_bytes()
            .chunks(37)
            .map(|c| format!("   {}\n", std::str::from_utf8(c).unwrap()))
            .collect();
        let odd = format!("Hier ist er:\n{ARMOR_BEGIN}\n{rewrapped}{ARMOR_END}\nGruß\n");
        assert_eq!(PassportV1::decode(&odd).unwrap(), p);
    }

    #[test]
    fn missing_armor_is_rejected() {
        let armored = sample().encode();
        let bare: String = armored
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        assert!(matches!(
            PassportV1::decode(&bare),
            Err(PassportError::BadArmor)
        ));
        let no_end = armored.replace(ARMOR_END, "");
        assert!(matches!(
            PassportV1::decode(&no_end),
            Err(PassportError::BadArmor)
        ));
        // Footer before header is not a block.
        let swapped = format!("{ARMOR_END}\nAAAA\n{ARMOR_BEGIN}\n");
        assert!(matches!(
            PassportV1::decode(&swapped),
            Err(PassportError::BadArmor)
        ));
    }

    #[test]
    fn bad_base64_is_rejected() {
        let armored = format!("{ARMOR_BEGIN}\n!!!!not base64!!!!\n{ARMOR_END}\n");
        assert!(matches!(
            PassportV1::decode(&armored),
            Err(PassportError::BadBase64)
        ));
    }

    #[test]
    fn oversized_is_rejected() {
        use base64::Engine;
        let mut body = vec![PASSPORT_VERSION];
        body.extend(std::iter::repeat_n(0u8, MAX_PASSPORT_BYTES));
        let b64 = base64::engine::general_purpose::STANDARD.encode(&body);
        let armored = format!("{ARMOR_BEGIN}\n{b64}\n{ARMOR_END}\n");
        assert!(matches!(
            PassportV1::decode(&armored),
            Err(PassportError::TooLarge)
        ));
    }

    #[test]
    fn wrong_version_is_rejected() {
        use base64::Engine;
        let mut body = vec![PASSPORT_VERSION];
        body.extend_from_slice(&postcard::to_stdvec(&sample()).unwrap());
        body[0] = 2;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&body);
        let armored = format!("{ARMOR_BEGIN}\n{b64}\n{ARMOR_END}\n");
        assert!(matches!(
            PassportV1::decode(&armored),
            Err(PassportError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn truncated_or_empty_body_is_malformed() {
        use base64::Engine;
        let mut body = vec![PASSPORT_VERSION];
        body.extend_from_slice(&postcard::to_stdvec(&sample()).unwrap());
        body.truncate(body.len() / 2);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&body);
        let armored = format!("{ARMOR_BEGIN}\n{b64}\n{ARMOR_END}\n");
        assert!(matches!(
            PassportV1::decode(&armored),
            Err(PassportError::Malformed)
        ));

        let empty = format!("{ARMOR_BEGIN}\n\n{ARMOR_END}\n");
        assert!(matches!(
            PassportV1::decode(&empty),
            Err(PassportError::Malformed)
        ));
    }

    #[test]
    fn debug_redacts_the_key() {
        let shown = format!("{:?}", sample());
        assert!(shown.contains("<redacted, 200 bytes>"));
        assert!(!shown.contains("197, 198, 199"));
    }
}
