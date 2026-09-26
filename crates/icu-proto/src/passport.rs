//! ASCII-armored identity bundle (`PROTOCOL.md` §2.3, §2.3.1).
//!
//! Private key, client leaf and CA in one block a user can copy into another
//! installation. The same format carries a **registration bundle**: an admin
//! issues a one-time key and a short-lived registration certificate, the new
//! client connects with it and swaps it for a certificate on its own key.
//!
//! Version 2 carries only the three DER blobs. Where to connect comes from the
//! certificates themselves: the client leaf has a URI SAN `icu://host:port`,
//! the CA's CN is `ICU CA <server_name>`. Version 1 still decodes; it carried
//! the server coordinates as separate fields.
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

/// Format version this build writes, the first byte of the decoded body.
///
/// It sits *outside* the postcard body on purpose: postcard is not
/// self-describing, so a version with changed fields would otherwise decode as
/// a plausible-looking older one instead of being rejected.
pub const PASSPORT_VERSION: u8 = 2;

/// The first format, with the server coordinates as separate fields. Still
/// accepted on import.
pub const PASSPORT_VERSION_V1: u8 = 1;

/// Upper bound on the decoded body. A real bundle is 1–1.5 KiB; the headroom
/// covers a longer key or CA without inviting a decode bomb.
pub const MAX_PASSPORT_BYTES: usize = 8192;

/// Base64 line width inside the armor.
const ARMOR_COLUMNS: usize = 64;

/// A whole ICU identity, portable to another device. Current format.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassportV2 {
    /// Private key, PKCS#8 DER.
    pub key_der: Vec<u8>,
    /// Client leaf certificate, DER. Carries the server address as URI SAN.
    pub leaf_der: Vec<u8>,
    /// The server's CA certificate, DER.
    pub ca_der: Vec<u8>,
}

/// `key_der` is redacted: a passport must never end up in a log line.
impl std::fmt::Debug for PassportV2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassportV2")
            .field(
                "key_der",
                &format_args!("<redacted, {} bytes>", self.key_der.len()),
            )
            .field("leaf_der", &format_args!("{} bytes", self.leaf_der.len()))
            .field("ca_der", &format_args!("{} bytes", self.ca_der.len()))
            .finish()
    }
}

/// Any passport this build can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Passport {
    /// Old format with explicit server coordinates.
    V1(PassportV1),
    /// Current format.
    V2(PassportV2),
}

impl Passport {
    /// Decodes a full armored block of either version.
    ///
    /// Tolerant about whitespace: a pasted passport may arrive re-wrapped, with
    /// CRLF line endings, or indented by a chat client, and all of that decodes
    /// to the same value.
    pub fn decode(armored: &str) -> Result<Passport, PassportError> {
        let body = dearmor(armored)?;
        let (version, rest) = body.split_first().ok_or(PassportError::Malformed)?;
        match *version {
            PASSPORT_VERSION_V1 => postcard::from_bytes(rest)
                .map(Passport::V1)
                .map_err(|_| PassportError::Malformed),
            PASSPORT_VERSION => postcard::from_bytes(rest)
                .map(Passport::V2)
                .map_err(|_| PassportError::Malformed),
            other => Err(PassportError::UnsupportedVersion(other)),
        }
    }

    /// Cheap sniff so a paste field can tell a passport from anything else
    /// before trying to parse it.
    pub fn looks_like(text: &str) -> bool {
        text.trim_start().starts_with(ARMOR_BEGIN)
    }

    /// The key, leaf and CA, whatever the version.
    pub fn into_parts(self) -> PassportV2 {
        match self {
            Passport::V1(p) => PassportV2 {
                key_der: p.key_der,
                leaf_der: p.leaf_der,
                ca_der: p.ca_der,
            },
            Passport::V2(p) => p,
        }
    }
}

impl PassportV2 {
    /// The full armored block, ready to copy or write to a file. Ends with a
    /// newline so concatenating one into a file needs no fixup.
    pub fn encode(&self) -> String {
        armor(
            PASSPORT_VERSION,
            &postcard::to_stdvec(self).unwrap_or_default(),
        )
    }
}

/// The first passport format. Only decoded now; [`PassportV1::encode`] exists
/// so tests can produce one.
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
            .field(
                "key_der",
                &format_args!("<redacted, {} bytes>", self.key_der.len()),
            )
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
    /// The decoded body is empty or not a valid passport of its version.
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
    /// The armored block in the old format.
    pub fn encode(&self) -> String {
        armor(
            PASSPORT_VERSION_V1,
            &postcard::to_stdvec(self).unwrap_or_default(),
        )
    }
}

fn armor(version: u8, postcard_body: &[u8]) -> String {
    use base64::Engine;
    let mut body = Vec::with_capacity(1 + postcard_body.len());
    body.push(version);
    body.extend_from_slice(postcard_body);
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

/// The decoded body between the armor lines, version byte included.
fn dearmor(armored: &str) -> Result<Vec<u8>, PassportError> {
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
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PassportV2 {
        PassportV2 {
            key_der: (0..200u32).map(|i| i as u8).collect(),
            leaf_der: (0..318u32).map(|i| (i % 251) as u8).collect(),
            ca_der: (0..366u32).map(|i| (i % 253) as u8).collect(),
        }
    }

    fn sample_v1() -> PassportV1 {
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
        assert_eq!(Passport::decode(&armored).unwrap(), Passport::V2(p.clone()));
        assert!(Passport::looks_like(&armored));
        assert!(Passport::looks_like(&format!("\n  {armored}")));
        assert!(!Passport::looks_like("icu://host:4711/enroll?t=x"));
    }

    #[test]
    fn whitespace_is_tolerated() {
        let p = sample();
        let armored = p.encode();
        let crlf = armored.replace('\n', "\r\n");
        assert_eq!(Passport::decode(&crlf).unwrap(), Passport::V2(p.clone()));

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
        assert_eq!(Passport::decode(&odd).unwrap(), Passport::V2(p));
    }

    #[test]
    fn missing_armor_is_rejected() {
        let armored = sample().encode();
        let bare: String = armored
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        assert!(matches!(
            Passport::decode(&bare),
            Err(PassportError::BadArmor)
        ));
        let no_end = armored.replace(ARMOR_END, "");
        assert!(matches!(
            Passport::decode(&no_end),
            Err(PassportError::BadArmor)
        ));
        // Footer before header is not a block.
        let swapped = format!("{ARMOR_END}\nAAAA\n{ARMOR_BEGIN}\n");
        assert!(matches!(
            Passport::decode(&swapped),
            Err(PassportError::BadArmor)
        ));
    }

    #[test]
    fn bad_base64_is_rejected() {
        let armored = format!("{ARMOR_BEGIN}\n!!!!not base64!!!!\n{ARMOR_END}\n");
        assert!(matches!(
            Passport::decode(&armored),
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
            Passport::decode(&armored),
            Err(PassportError::TooLarge)
        ));
    }

    #[test]
    fn wrong_version_is_rejected() {
        use base64::Engine;
        let mut body = vec![PASSPORT_VERSION];
        body.extend_from_slice(&postcard::to_stdvec(&sample()).unwrap());
        body[0] = 3;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&body);
        let armored = format!("{ARMOR_BEGIN}\n{b64}\n{ARMOR_END}\n");
        assert!(matches!(
            Passport::decode(&armored),
            Err(PassportError::UnsupportedVersion(3))
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
            Passport::decode(&armored),
            Err(PassportError::Malformed)
        ));

        let empty = format!("{ARMOR_BEGIN}\n\n{ARMOR_END}\n");
        assert!(matches!(
            Passport::decode(&empty),
            Err(PassportError::Malformed)
        ));
    }

    #[test]
    fn debug_redacts_the_key() {
        let shown = format!("{:?}", sample());
        assert!(shown.contains("<redacted, 200 bytes>"));
        assert!(!shown.contains("197, 198, 199"));
        let shown = format!("{:?}", sample_v1());
        assert!(shown.contains("<redacted, 200 bytes>"));
    }

    /// A passport exported by an older client still imports, and yields the
    /// same key material.
    #[test]
    fn v1_still_decodes() {
        let old = sample_v1();
        let back = Passport::decode(&old.encode()).unwrap();
        assert_eq!(back, Passport::V1(old.clone()));
        let parts = back.into_parts();
        assert_eq!(parts.key_der, old.key_der);
        assert_eq!(parts.leaf_der, old.leaf_der);
        assert_eq!(parts.ca_der, old.ca_der);
    }
}
