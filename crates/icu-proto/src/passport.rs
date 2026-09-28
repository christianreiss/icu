//! ASCII-armored identity bundle (`docs/reference/protocol.md` §2.3, §2.3.1).
//!
//! Private key, client leaf, CA and the server's com key in one block a user
//! can copy into another installation, optionally with the exporting client's
//! settings. The same format carries a **registration bundle**: an admin
//! issues a one-time key and a short-lived registration certificate, the new
//! client connects with it and swaps it for a certificate on its own key.
//!
//! Where to connect comes from the certificates themselves: the client leaf
//! has a URI SAN `icu://host:port`, the CA's CN is `ICU CA <server_name>`.
//! There is one format, version 5 (2026-09-28); earlier versions lack fields
//! this crate depends on and are refused outright, with no migration path —
//! client and server in this project ship in lockstep.
//!
//! The body is **not encrypted**. A passport is a bearer credential for the
//! account until the certificate expires or an admin revokes it — whoever reads
//! the text can sign in as that user. The client warns accordingly and writes
//! the file with mode 0600.

use serde::{Deserialize, Serialize};

use crate::com::ComKey;

/// Armor header line.
pub const ARMOR_BEGIN: &str = "-----BEGIN ICU PASSPORT-----";
/// Armor footer line.
pub const ARMOR_END: &str = "-----END ICU PASSPORT-----";

/// The format version, the first byte of the decoded body.
///
/// It sits *outside* the postcard body on purpose: postcard is not
/// self-describing, so a version with changed fields would otherwise decode as
/// a plausible-looking older one instead of being rejected.
pub const PASSPORT_VERSION: u8 = 5;

/// Upper bound on the decoded body. A real bundle is 1–1.5 KiB, with settings
/// some 3 KiB; the headroom covers a longer key or CA without inviting a
/// decode bomb.
pub const MAX_PASSPORT_BYTES: usize = 8192;

/// Base64 line width inside the armor.
const ARMOR_COLUMNS: usize = 64;

/// A whole ICU identity, portable to another device: what the server writes
/// for a registration bundle (without settings), and what a client exports.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Passport {
    /// Private key, PKCS#8 DER.
    pub key_der: Vec<u8>,
    /// Client leaf certificate, DER. Carries the server address as URI SAN.
    pub leaf_der: Vec<u8>,
    /// The server's CA certificate, DER.
    pub ca_der: Vec<u8>,
    /// The server's com key (§1): without it no datagram reaches the server.
    pub com_key: ComKey,
    /// The settings that may move to another device, as TOML text; empty in a
    /// registration bundle. Opaque here: TOML stays readable when a newer
    /// client adds a setting or an older one lacks it, which postcard would
    /// not.
    pub settings: String,
    /// DER ECDSA-P256 signature by the server's CA key over
    /// [`Passport::signable_bytes`], present (non-empty) only on a
    /// registration bundle: the one case where the signing key never travels
    /// inside the bundle itself, so the signature actually proves nothing
    /// was altered since the server wrote it. Empty on a personal export —
    /// a client signing its own export would only be proving the bundle
    /// matches a key that travels right there in the same bundle.
    pub signature: Vec<u8>,
}

/// `key_der` and `com_key` are redacted: a passport must never end up in a
/// log line.
impl std::fmt::Debug for Passport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Passport")
            .field(
                "key_der",
                &format_args!("<redacted, {} bytes>", self.key_der.len()),
            )
            .field("leaf_der", &format_args!("{} bytes", self.leaf_der.len()))
            .field("ca_der", &format_args!("{} bytes", self.ca_der.len()))
            .field("com_key", &self.com_key)
            .field("settings", &format_args!("{} bytes", self.settings.len()))
            .field("signature", &format_args!("{} bytes", self.signature.len()))
            .finish()
    }
}

impl Passport {
    /// Decodes a full armored block.
    ///
    /// Tolerant about whitespace: a pasted passport may arrive re-wrapped, with
    /// CRLF line endings, or indented by a chat client, and all of that decodes
    /// to the same value.
    pub fn decode(armored: &str) -> Result<Passport, PassportError> {
        let body = dearmor(armored)?;
        let (version, rest) = body.split_first().ok_or(PassportError::Malformed)?;
        match *version {
            PASSPORT_VERSION => postcard::from_bytes(rest).map_err(|_| PassportError::Malformed),
            other => Err(PassportError::UnsupportedVersion(other)),
        }
    }

    /// Cheap sniff so a paste field can tell a passport from anything else
    /// before trying to parse it.
    pub fn looks_like(text: &str) -> bool {
        text.trim_start().starts_with(ARMOR_BEGIN)
    }

    /// The full armored block, ready to copy or write to a file. Ends with a
    /// newline so concatenating one into a file needs no fixup.
    pub fn encode(&self) -> String {
        armor(
            PASSPORT_VERSION,
            &postcard::to_stdvec(self).unwrap_or_default(),
        )
    }

    /// Whether the body stays within [`MAX_PASSPORT_BYTES`], so that
    /// [`Passport::decode`] takes it. A writer checks before adding settings:
    /// a very long volume list could outgrow it.
    pub fn fits(&self) -> bool {
        // The version byte comes on top of the postcard body.
        postcard::to_stdvec(self).is_ok_and(|body| body.len() < MAX_PASSPORT_BYTES)
    }

    /// Bytes [`Passport::signature`] covers: the version byte plus every
    /// other field, postcard-encoded. The version byte is included so a
    /// signature from one format version can't be replayed against another.
    /// Both the signer and the verifier must hash exactly this.
    pub fn signable_bytes(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Signable<'a> {
            key_der: &'a [u8],
            leaf_der: &'a [u8],
            ca_der: &'a [u8],
            com_key: &'a ComKey,
            settings: &'a str,
        }
        let mut out = vec![PASSPORT_VERSION];
        out.extend(
            postcard::to_stdvec(&Signable {
                key_der: &self.key_der,
                leaf_der: &self.leaf_der,
                ca_der: &self.ca_der,
                com_key: &self.com_key,
                settings: &self.settings,
            })
            .unwrap_or_default(),
        );
        out
    }

    /// Whether [`Passport::signature`] verifies against `ec_point` (an
    /// uncompressed P-256 public key point, `0x04 || x || y` — e.g. the raw
    /// bit-string contents of a certificate's SPKI, not the SPKI DER
    /// itself) for the fields this passport carries. `false` on an empty
    /// signature or anything else that doesn't check out; never panics.
    pub fn verify_signature(&self, ec_point: &[u8]) -> bool {
        use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
        if self.signature.is_empty() {
            return false;
        }
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, ec_point)
            .verify(&self.signable_bytes(), &self.signature)
            .is_ok()
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

    fn sample() -> Passport {
        Passport {
            key_der: (0..200u32).map(|i| i as u8).collect(),
            leaf_der: (0..318u32).map(|i| (i % 251) as u8).collect(),
            ca_der: (0..366u32).map(|i| (i % 253) as u8).collect(),
            com_key: ComKey::from_bytes([0x5A; crate::com::COM_KEY_LEN]),
            settings: String::new(),
            signature: Vec::new(),
        }
    }

    fn with_settings() -> Passport {
        Passport {
            settings: "[audio]\nmode = \"ptt\"\n".into(),
            ..sample()
        }
    }

    fn armored_body(version: u8, body: &[u8]) -> String {
        use base64::Engine;
        let mut raw = vec![version];
        raw.extend_from_slice(body);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        format!("{ARMOR_BEGIN}\n{b64}\n{ARMOR_END}\n")
    }

    #[test]
    fn encode_decode_roundtrip() {
        for p in [sample(), with_settings()] {
            let armored = p.encode();
            assert!(armored.starts_with(ARMOR_BEGIN));
            assert!(armored.trim_end().ends_with(ARMOR_END));
            for line in armored.lines() {
                assert!(line.len() <= ARMOR_COLUMNS.max(ARMOR_BEGIN.len()));
            }
            assert_eq!(Passport::decode(&armored).unwrap(), p);
            assert!(Passport::looks_like(&armored));
            assert!(Passport::looks_like(&format!("\n  {armored}")));
        }
        assert!(!Passport::looks_like("icu://host:4711/enroll?t=x"));
    }

    #[test]
    fn whitespace_is_tolerated() {
        let p = sample();
        let armored = p.encode();
        let crlf = armored.replace('\n', "\r\n");
        assert_eq!(Passport::decode(&crlf).unwrap(), p);

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
        assert_eq!(Passport::decode(&odd).unwrap(), p);
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
        let armored = armored_body(PASSPORT_VERSION, &vec![0u8; MAX_PASSPORT_BYTES]);
        assert!(matches!(
            Passport::decode(&armored),
            Err(PassportError::TooLarge)
        ));
    }

    /// Versions 1–3 had no com key and version 4 no signature field: none of
    /// them can be read by this build, and anything newer is refused too.
    #[test]
    fn other_versions_are_rejected() {
        let body = postcard::to_stdvec(&sample()).unwrap();
        for version in [1, 2, 3, 4, 6] {
            assert!(matches!(
                Passport::decode(&armored_body(version, &body)),
                Err(PassportError::UnsupportedVersion(v)) if v == version
            ));
        }
    }

    #[test]
    fn truncated_or_empty_body_is_malformed() {
        let mut body = postcard::to_stdvec(&sample()).unwrap();
        body.truncate(body.len() / 2);
        assert!(matches!(
            Passport::decode(&armored_body(PASSPORT_VERSION, &body)),
            Err(PassportError::Malformed)
        ));

        let empty = format!("{ARMOR_BEGIN}\n\n{ARMOR_END}\n");
        assert!(matches!(
            Passport::decode(&empty),
            Err(PassportError::Malformed)
        ));
    }

    #[test]
    fn debug_redacts_the_keys() {
        let shown = format!("{:?}", with_settings());
        assert!(shown.contains("<redacted, 200 bytes>"));
        assert!(shown.contains("ComKey(<redacted>)"));
        assert!(shown.contains("signature: 0 bytes"));
        assert!(!shown.contains("197, 198, 199"));
        assert!(!shown.contains("90, 90"));
    }

    /// A real ECDSA-P256 keypair (`ring`, not `rcgen`: icu-proto doesn't
    /// depend on it), standing in for the server's CA key.
    fn test_keypair() -> (ring::signature::EcdsaKeyPair, Vec<u8>) {
        use ring::rand::SystemRandom;
        use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
                .unwrap();
        let ec_point = key_pair.public_key().as_ref().to_vec();
        (key_pair, ec_point)
    }

    fn sign_with(p: &Passport, key_pair: &ring::signature::EcdsaKeyPair) -> Vec<u8> {
        let rng = ring::rand::SystemRandom::new();
        key_pair
            .sign(&rng, &p.signable_bytes())
            .unwrap()
            .as_ref()
            .to_vec()
    }

    /// The concrete guarantee this feature buys: a valid signature accepts
    /// the exact bundle it was made for, and rejects it the moment any
    /// signed field — including `com_key`, which nothing else here checks —
    /// changes afterwards.
    #[test]
    fn signature_roundtrip_and_tamper_detection() {
        let (key_pair, ec_point) = test_keypair();
        let mut p = with_settings();
        p.signature = sign_with(&p, &key_pair);
        assert!(p.verify_signature(&ec_point));

        // Round-trips through the armor unchanged.
        let armored = p.encode();
        assert_eq!(Passport::decode(&armored).unwrap(), p);

        let mut tampered_settings = p.clone();
        tampered_settings.settings.push('x');
        assert!(!tampered_settings.verify_signature(&ec_point));

        let mut tampered_com_key = p.clone();
        tampered_com_key.com_key = ComKey::from_bytes([0x11; crate::com::COM_KEY_LEN]);
        assert!(!tampered_com_key.verify_signature(&ec_point));

        let mut flipped_signature = p.clone();
        let last = flipped_signature.signature.len() - 1;
        flipped_signature.signature[last] ^= 1;
        assert!(!flipped_signature.verify_signature(&ec_point));

        // No signature at all, and a signature checked against the wrong key.
        assert!(!sample().verify_signature(&ec_point));
        let (_, other_point) = test_keypair();
        assert!(!p.verify_signature(&other_point));
    }

    /// Settings that would push the body past the cap are caught before
    /// writing: a reader would refuse the whole passport.
    #[test]
    fn oversized_settings_do_not_fit() {
        assert!(with_settings().fits());
        let mut p = with_settings();
        p.settings = "x".repeat(MAX_PASSPORT_BYTES);
        assert!(!p.fits());
        assert!(matches!(
            Passport::decode(&p.encode()),
            Err(PassportError::TooLarge)
        ));
    }
}
