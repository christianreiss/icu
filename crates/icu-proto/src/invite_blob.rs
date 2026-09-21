//! Self-contained, ASCII-armored invite (`PROTOCOL.md` §2.3.1).
//!
//! Bundles everything a fresh client needs to enroll — the `host:port`
//! authority, the full CA certificate, and a sealed bearer token — into one
//! blob the admin's client can display and the joining client can paste, no
//! separate `ca=` pin fetch and no CLI on either side.
//!
//! `host_port`/`server_name`/`ca_der`/`expires` are necessarily plaintext:
//! the client has to read them before any secret exchange happens, to know
//! where to dial and what to pin against (the same trust-on-first-use model
//! as the `icu://…?ca=` link, just carrying full CA bytes instead of a
//! hash). Only `token` needs real confidentiality, so only it is sealed,
//! under a key only the server holds (`icu-server::invite_key`); the AEAD
//! tag binds it to every plaintext field via `aad()`, so tampering with any
//! of them invalidates the seal.

use serde::{Deserialize, Serialize};

/// Prefix that marks a string as an invite blob rather than an `icu://` link.
pub const ARMOR_PREFIX: &str = "ICU1-";

/// Upper bound on the encoded blob, generous for a `CertificateDer` plus a
/// few short strings.
pub const MAX_BLOB_BYTES: usize = 4096;

/// A self-contained invite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteBlob {
    /// `host:port` (or `[ipv6]:port`) to dial, exactly as the server's own
    /// `public_host()` renders it — the same string an `icu://` link's
    /// authority carries, so the receiving client's existing authority
    /// parser applies unchanged.
    pub host_port: String,
    /// SNI to use for the main connection.
    pub server_name: String,
    /// The server's CA certificate, DER.
    pub ca_der: Vec<u8>,
    /// When the underlying invite expires, RFC 3339 UTC. Advisory only —
    /// the server is the authority on expiry.
    pub expires: String,
    /// AEAD nonce.
    pub nonce: [u8; 12],
    /// `ChaCha20-Poly1305(token)` with its 16 byte tag appended.
    pub sealed: Vec<u8>,
}

/// Why an invite blob could not be decoded.
#[derive(Debug)]
pub enum InviteBlobError {
    /// The string did not start with [`ARMOR_PREFIX`].
    BadPrefix,
    /// The armored body is not valid base64url.
    BadBase64,
    /// The decoded body exceeds [`MAX_BLOB_BYTES`].
    TooLarge,
    /// The decoded body is not a valid `InviteBlob`.
    Malformed,
}

impl std::fmt::Display for InviteBlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InviteBlobError::BadPrefix => write!(f, "missing {ARMOR_PREFIX} prefix"),
            InviteBlobError::BadBase64 => write!(f, "not valid base64url"),
            InviteBlobError::TooLarge => write!(f, "blob exceeds {MAX_BLOB_BYTES} bytes"),
            InviteBlobError::Malformed => write!(f, "not a valid invite blob"),
        }
    }
}

impl std::error::Error for InviteBlobError {}

impl InviteBlob {
    /// Bytes the AEAD seal authenticates: every field except `nonce` and
    /// `sealed` themselves. Tampering with any of them makes `open()` fail.
    pub fn aad(&self) -> Vec<u8> {
        postcard::to_stdvec(&(
            &self.host_port,
            &self.server_name,
            &self.ca_der,
            &self.expires,
        ))
        .unwrap_or_default()
    }

    /// The base64url body without the [`ARMOR_PREFIX`] — what a client
    /// relays back, opaquely, as `EnrollRequest.token`.
    pub fn body_base64(&self) -> String {
        use base64::Engine;
        let body = postcard::to_stdvec(self).unwrap_or_default();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body)
    }

    /// The full `ICU1-…` string, ready to display or paste.
    pub fn encode(&self) -> String {
        format!("{ARMOR_PREFIX}{}", self.body_base64())
    }

    /// Decodes a full `ICU1-…` string.
    pub fn decode(armored: &str) -> Result<InviteBlob, InviteBlobError> {
        let body = armored
            .strip_prefix(ARMOR_PREFIX)
            .ok_or(InviteBlobError::BadPrefix)?;
        Self::decode_body_base64(body)
    }

    /// Decodes the bare base64url body (no `ARMOR_PREFIX`), the shape a
    /// server receives inside `EnrollRequest.token`.
    pub fn decode_body_base64(body: &str) -> Result<InviteBlob, InviteBlobError> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| InviteBlobError::BadBase64)?;
        if bytes.len() > MAX_BLOB_BYTES {
            return Err(InviteBlobError::TooLarge);
        }
        postcard::from_bytes(&bytes).map_err(|_| InviteBlobError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> InviteBlob {
        InviteBlob {
            host_port: "icu.example.org:4711".into(),
            server_name: "icu.example.org".into(),
            ca_der: vec![0x30, 0x82, 0x01, 0x02],
            expires: "2026-09-22T00:00:00Z".into(),
            nonce: [7u8; 12],
            sealed: vec![1, 2, 3, 4],
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let blob = sample();
        let armored = blob.encode();
        assert!(armored.starts_with(ARMOR_PREFIX));
        assert_eq!(InviteBlob::decode(&armored).unwrap(), blob);
    }

    #[test]
    fn body_base64_roundtrips_via_decode_body_base64() {
        let blob = sample();
        let body = blob.body_base64();
        assert_eq!(InviteBlob::decode_body_base64(&body).unwrap(), blob);
    }

    #[test]
    fn missing_prefix_is_rejected() {
        let blob = sample();
        let body = blob.body_base64();
        assert!(matches!(
            InviteBlob::decode(&body),
            Err(InviteBlobError::BadPrefix)
        ));
    }

    #[test]
    fn malformed_base64_is_rejected() {
        assert!(matches!(
            InviteBlob::decode("ICU1-not base64!!"),
            Err(InviteBlobError::BadBase64)
        ));
    }

    #[test]
    fn oversized_body_is_rejected() {
        use base64::Engine;
        let huge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(vec![0u8; MAX_BLOB_BYTES + 1]);
        assert!(matches!(
            InviteBlob::decode_body_base64(&huge),
            Err(InviteBlobError::TooLarge)
        ));
    }

    #[test]
    fn aad_is_deterministic_and_field_sensitive() {
        let a = sample();
        let mut b = sample();
        assert_eq!(a.aad(), b.aad());
        b.host_port = "other.example.org:4711".into();
        assert_ne!(a.aad(), b.aad());
        // nonce/sealed must NOT affect aad — the server recomputes it from
        // the plaintext fields alone before it has opened anything.
        let mut c = sample();
        c.nonce = [9u8; 12];
        c.sealed = vec![9, 9, 9];
        assert_eq!(a.aad(), c.aad());
    }
}
