//! Symmetric key that seals and opens the bearer token inside an invite blob
//! (`Server/IMPLEMENTATION.md` §6.1, `PROTOCOL.md` §2.3.1).
//!
//! Independent from the CA key (`ca.rs`) — a different purpose gets a
//! different key, so compromising one does not compromise the other. 32
//! random bytes, generated once, mode 0600, never rotates in v1.

use std::path::Path;

use icu_proto::error as ec;
use rand::RngCore;
use ring::aead;

use crate::{
    error::{Error, Result},
    store,
};

const INVITE_KEY_FILE: &str = "invite.key";
const KEY_LEN: usize = 32;

/// The AEAD key that seals/opens the token carried inside an `InviteBlob`.
pub struct InviteKey {
    key: aead::LessSafeKey,
}

impl std::fmt::Debug for InviteKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InviteKey").finish_non_exhaustive()
    }
}

impl InviteKey {
    /// Loads the key from `dir`, generating and persisting one if none
    /// exists yet.
    pub fn load_or_create(dir: &Path) -> Result<InviteKey> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(INVITE_KEY_FILE);
        let raw = match std::fs::read_to_string(&path) {
            Ok(hex) => decode_hex(hex.trim())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut raw = [0u8; KEY_LEN];
                rand::rng().fill_bytes(&mut raw);
                store::write_private(&path, &encode_hex(&raw))?;
                raw.to_vec()
            }
            Err(e) => return Err(Error::from(e).context(format!("cannot read {}", path.display()))),
        };
        if raw.len() != KEY_LEN {
            return Err(Error::msg(format!(
                "{} has {} bytes, expected {KEY_LEN}",
                path.display(),
                raw.len()
            )));
        }
        let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &raw)
            .map_err(|_| Error::msg("invite key material rejected"))?;
        Ok(InviteKey {
            key: aead::LessSafeKey::new(unbound),
        })
    }

    /// Seals `token` under `aad`. Returns a fresh nonce and the
    /// ciphertext with its 16 byte tag appended.
    pub fn seal(&self, token: &str, aad: &[u8]) -> ([u8; 12], Vec<u8>) {
        let mut nonce_bytes = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        let mut buf = token.as_bytes().to_vec();
        self.key
            .seal_in_place_append_tag(nonce, aead::Aad::from(aad), &mut buf)
            .expect("seal with a freshly generated nonce never fails");
        (nonce_bytes, buf)
    }

    /// Opens a previously sealed token. `sealed` is overwritten in place;
    /// tampering with either `sealed` or `aad` since sealing makes this
    /// fail with `ENROLL_BAD_TOKEN`.
    pub fn open(&self, nonce: [u8; 12], sealed: &mut [u8], aad: &[u8]) -> Result<String> {
        let nonce = aead::Nonce::assume_unique_for_key(nonce);
        let opened = self
            .key
            .open_in_place(nonce, aead::Aad::from(aad), sealed)
            .map_err(|_| Error::coded(ec::ENROLL_BAD_TOKEN, "invite blob does not open"))?;
        String::from_utf8(opened.to_vec())
            .map_err(|_| Error::coded(ec::ENROLL_BAD_TOKEN, "invite blob payload is not utf8"))
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn decode_hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(Error::msg("invite.key has an odd number of hex digits"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| Error::msg(format!("invite.key is not hex: {e}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let key = InviteKey::load_or_create(dir.path()).unwrap();
        let (nonce, mut sealed) = key.seal("k3nP8sQ2vL7xR1wY4tZ6bA", b"aad");
        let token = key.open(nonce, &mut sealed, b"aad").unwrap();
        assert_eq!(token, "k3nP8sQ2vL7xR1wY4tZ6bA");
    }

    #[test]
    fn tampered_aad_does_not_open() {
        let dir = tempfile::tempdir().unwrap();
        let key = InviteKey::load_or_create(dir.path()).unwrap();
        let (nonce, mut sealed) = key.seal("k3nP8sQ2vL7xR1wY4tZ6bA", b"aad-a");
        let err = key.open(nonce, &mut sealed, b"aad-b").unwrap_err();
        assert_eq!(err.code(), Some(ec::ENROLL_BAD_TOKEN));
    }

    #[test]
    fn tampered_ciphertext_does_not_open() {
        let dir = tempfile::tempdir().unwrap();
        let key = InviteKey::load_or_create(dir.path()).unwrap();
        let (nonce, mut sealed) = key.seal("k3nP8sQ2vL7xR1wY4tZ6bA", b"aad");
        sealed[0] ^= 0xFF;
        assert!(key.open(nonce, &mut sealed, b"aad").is_err());
    }

    #[test]
    fn wrong_nonce_does_not_open() {
        let dir = tempfile::tempdir().unwrap();
        let key = InviteKey::load_or_create(dir.path()).unwrap();
        let (mut nonce, mut sealed) = key.seal("k3nP8sQ2vL7xR1wY4tZ6bA", b"aad");
        nonce[0] ^= 0xFF;
        assert!(key.open(nonce, &mut sealed, b"aad").is_err());
    }

    #[test]
    fn key_is_created_once_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let a = InviteKey::load_or_create(dir.path()).unwrap();
        let (nonce, mut sealed) = a.seal("k3nP8sQ2vL7xR1wY4tZ6bA", b"aad");
        let b = InviteKey::load_or_create(dir.path()).unwrap();
        // The reloaded key opens what the first instance sealed only if the
        // bytes on disk were reused rather than regenerated.
        assert_eq!(
            b.open(nonce, &mut sealed, b"aad").unwrap(),
            "k3nP8sQ2vL7xR1wY4tZ6bA"
        );
    }

    #[test]
    fn key_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        InviteKey::load_or_create(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join(INVITE_KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
