//! The server's CRL on the client (`PROTOCOL.md` §1, §2.2 `CrlUpdate`).
//!
//! The server sends its CRL after `Welcome` and whenever it changes. The
//! client keeps the newest one its own CA signed and checks the server leaf
//! against it on the next connect, so a server leaf replaced by
//! `rotate-server-cert` — say after its key leaked — is refused from then on.
//!
//! The file lives next to the identity, not inside it: the identity directory
//! is swapped as a whole on every renewal, and the CRL number must survive
//! that, or an older CRL could be slipped in again. A CRL that does not verify
//! against the identity's CA (after importing another server's passport) is
//! ignored and replaced by the next one that does.

use std::path::PathBuf;

use crate::config::{write_atomic, Paths};

/// `crl.der` in the configuration directory.
pub fn path(paths: &Paths) -> PathBuf {
    paths.config_dir.join("crl.der")
}

/// Checks `crl_der` against `ca_der` — issuer, signature, CRL number — and
/// returns its number.
pub fn verify(crl_der: &[u8], ca_der: &[u8]) -> Result<u64, String> {
    use x509_parser::prelude::FromDer;
    let (_, ca) = x509_parser::certificate::X509Certificate::from_der(ca_der)
        .map_err(|e| format!("CA unlesbar: {e}"))?;
    let (rest, crl) = x509_parser::revocation_list::CertificateRevocationList::from_der(crl_der)
        .map_err(|e| format!("CRL unlesbar: {e}"))?;
    if !rest.is_empty() {
        return Err("CRL mit überzähligen Bytes".into());
    }
    if crl.issuer() != ca.subject() {
        return Err("CRL stammt nicht von der eigenen CA".into());
    }
    crl.verify_signature(ca.public_key())
        .map_err(|e| format!("CRL-Signatur ungültig: {e}"))?;
    let number = crl.crl_number().ok_or("CRL ohne Nummer")?.to_bytes_be();
    if number.len() > 8 {
        return Err("CRL-Nummer zu groß".into());
    }
    Ok(number.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b)))
}

/// The stored CRL, if there is one and it verifies against `ca_der`.
pub fn load(paths: &Paths, ca_der: &[u8]) -> Option<Vec<u8>> {
    let crl = std::fs::read(path(paths)).ok()?;
    verify(&crl, ca_der).ok().map(|_| crl)
}

/// Stores `crl_der` if it verifies against `ca_der` and is newer than the
/// stored one. `Ok(false)`: valid, but not newer — an old CRL replayed or the
/// same one again.
pub fn accept(paths: &Paths, ca_der: &[u8], crl_der: &[u8]) -> Result<bool, String> {
    let number = verify(crl_der, ca_der)?;
    let stored = std::fs::read(path(paths))
        .ok()
        .and_then(|old| verify(&old, ca_der).ok());
    if stored.is_some_and(|old| old >= number) {
        return Ok(false);
    }
    write_atomic(&path(paths), crl_der, false).map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{
        BasicConstraints, CertificateParams, CertificateRevocationListParams, DnType, IsCa, Issuer,
        KeyIdMethod, KeyPair, KeyUsagePurpose, RevokedCertParams, SerialNumber,
    };

    fn ca(name: &str) -> (Vec<u8>, Issuer<'static, KeyPair>) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.distinguished_name.push(DnType::CommonName, name);
        let cert = params.self_signed(&key).unwrap();
        let der = cert.der().to_vec();
        (
            der.clone(),
            Issuer::from_ca_cert_der(&der.into(), key).unwrap(),
        )
    }

    fn crl(issuer: &Issuer<'_, KeyPair>, number: u8, serials: &[u8]) -> Vec<u8> {
        let now = time::OffsetDateTime::now_utc();
        CertificateRevocationListParams {
            this_update: now,
            next_update: now + time::Duration::days(1),
            crl_number: SerialNumber::from_slice(&[number]),
            issuing_distribution_point: None,
            revoked_certs: serials
                .iter()
                .map(|s| RevokedCertParams {
                    serial_number: SerialNumber::from_slice(&[*s]),
                    revocation_time: now,
                    reason_code: None,
                    invalidity_date: None,
                })
                .collect(),
            key_identifier_method: KeyIdMethod::Sha256,
        }
        .signed_by(issuer)
        .unwrap()
        .der()
        .to_vec()
    }

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        (dir, paths)
    }

    #[test]
    fn only_newer_crls_from_the_own_ca_are_kept() {
        let (_dir, paths) = paths();
        let (ca_der, issuer) = ca("ICU CA a");
        assert_eq!(accept(&paths, &ca_der, &crl(&issuer, 2, &[1])), Ok(true));
        assert_eq!(accept(&paths, &ca_der, &crl(&issuer, 2, &[1])), Ok(false));
        assert_eq!(
            accept(&paths, &ca_der, &crl(&issuer, 1, &[])),
            Ok(false),
            "an older CRL must not undo a revocation"
        );
        assert_eq!(
            load(&paths, &ca_der).map(|c| verify(&c, &ca_der)),
            Some(Ok(2))
        );
        assert_eq!(accept(&paths, &ca_der, &crl(&issuer, 3, &[1, 2])), Ok(true));
        assert_eq!(verify(&load(&paths, &ca_der).unwrap(), &ca_der), Ok(3));
    }

    #[test]
    fn a_crl_from_another_ca_is_refused() {
        let (_dir, paths) = paths();
        let (ca_der, _) = ca("ICU CA a");
        let (_, forger) = ca("ICU CA a"); // same name, other key
        assert!(accept(&paths, &ca_der, &crl(&forger, 9, &[])).is_err());
        let (_, other) = ca("ICU CA b");
        assert!(accept(&paths, &ca_der, &crl(&other, 9, &[])).is_err());
        assert!(accept(&paths, &ca_der, b"junk").is_err());
        assert_eq!(load(&paths, &ca_der), None);
    }

    #[test]
    fn a_stored_crl_of_another_ca_is_ignored() {
        let (_dir, paths) = paths();
        let (ca_a, issuer_a) = ca("ICU CA a");
        let (ca_b, issuer_b) = ca("ICU CA b");
        assert_eq!(accept(&paths, &ca_a, &crl(&issuer_a, 50, &[])), Ok(true));
        assert_eq!(load(&paths, &ca_b), None);
        // A lower number from the new CA still replaces it.
        assert_eq!(accept(&paths, &ca_b, &crl(&issuer_b, 1, &[])), Ok(true));
    }
}
