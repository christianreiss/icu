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
//!
//! Verifying is not enough to be usable: rustls (webpki) refuses CRLs that
//! x509-parser reads fine — an unknown critical extension, a delta, indirect
//! or reason-partitioned CRL — and a stored one it refuses would fail every
//! future connect. A new CRL is therefore tried in the verifier before it is
//! stored, and a stored one that fails the same trial is moved aside
//! (`crl.der.rejected-<unix time>`) and connecting goes on without one.

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

/// Whether the server-certificate verifier takes `crl_der` — the build the
/// connect makes (`net::conn::server_verifier`), tried out.
pub fn usable(crl_der: &[u8], ca_der: &[u8]) -> Result<(), String> {
    crate::net::conn::server_verifier(
        ca_der,
        Some(crl_der),
        std::sync::Arc::new(rustls::crypto::ring::default_provider()),
    )
    .map(|_| ())
    .map_err(|e| format!("CRL für die Prüfung unbrauchbar: {e}"))
}

/// The stored CRL, if there is one, it verifies against `ca_der` and the
/// verifier takes it. One the verifier refuses is moved aside, so it cannot
/// fail every connect; the next `CrlUpdate` brings a usable one.
pub fn load(paths: &Paths, ca_der: &[u8]) -> Option<Vec<u8>> {
    let crl = std::fs::read(path(paths)).ok()?;
    verify(&crl, ca_der).ok()?;
    if let Err(e) = usable(&crl, ca_der) {
        let aside = paths
            .config_dir
            .join(format!("crl.der.rejected-{}", crate::net::conn::unix_now()));
        match std::fs::rename(path(paths), &aside) {
            Ok(()) => tracing::warn!(
                error = %e,
                moved_to = %aside.display(),
                "stored CRL unusable, connecting without one"
            ),
            Err(mv) => tracing::warn!(
                error = %e,
                rename = %mv,
                "stored CRL unusable and cannot be moved aside, connecting without one"
            ),
        }
        return None;
    }
    Some(crl)
}

/// Stores `crl_der` if it verifies against `ca_der` and is newer than the
/// stored one. `Ok(false)`: valid, but not newer — an old CRL replayed or the
/// same one again.
pub fn accept(paths: &Paths, ca_der: &[u8], crl_der: &[u8]) -> Result<bool, String> {
    let number = verify(crl_der, ca_der)?;
    usable(crl_der, ca_der)?;
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

    /// DER tag-length-value.
    fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let n = body.len();
        let mut out = vec![tag];
        match n {
            0..0x80 => out.push(n as u8),
            0x80..0x100 => out.extend([0x81, n as u8]),
            _ => out.extend([0x82, (n >> 8) as u8, n as u8]),
        }
        out.extend_from_slice(body);
        out
    }

    /// A CRL signed by `issuer` that x509-parser reads and verifies, but
    /// that webpki refuses: it carries an unknown *critical* extension.
    fn crl_with_unknown_critical_extension(ca_der: &[u8], issuer: &Issuer<'_, KeyPair>) -> Vec<u8> {
        use rcgen::SigningKey;
        use x509_parser::prelude::FromDer;
        let (_, ca) = x509_parser::certificate::X509Certificate::from_der(ca_der).unwrap();
        // ecdsa-with-SHA256
        let alg = tlv(
            0x30,
            &tlv(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02]),
        );
        let crl_number = tlv(
            0x30,
            &[
                tlv(0x06, &[0x55, 0x1d, 0x14]),
                tlv(0x04, &tlv(0x02, &[0x07])),
            ]
            .concat(),
        );
        // 1.3.6.1.4.1.55555.1, critical, value NULL
        let unknown = tlv(
            0x30,
            &[
                tlv(
                    0x06,
                    &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x83, 0xb2, 0x03, 0x01],
                ),
                tlv(0x01, &[0xff]),
                tlv(0x04, &tlv(0x05, &[])),
            ]
            .concat(),
        );
        let tbs = tlv(
            0x30,
            &[
                tlv(0x02, &[0x01]),
                alg.clone(),
                ca.subject().as_raw().to_vec(),
                tlv(0x17, b"260901000000Z"),
                tlv(0x17, b"261001000000Z"),
                tlv(0xa0, &tlv(0x30, &[crl_number, unknown].concat())),
            ]
            .concat(),
        );
        let signature = issuer.key().sign(&tbs).unwrap();
        tlv(
            0x30,
            &[tbs, alg, tlv(0x03, &[&[0u8][..], &signature].concat())].concat(),
        )
    }

    /// A CRL that verifies but that rustls would refuse is never stored:
    /// stored, it would fail every future connect.
    #[test]
    fn a_crl_the_verifier_refuses_is_not_stored() {
        let (_dir, paths) = paths();
        let (ca_der, issuer) = ca("ICU CA a");
        let odd = crl_with_unknown_critical_extension(&ca_der, &issuer);
        assert_eq!(verify(&odd, &ca_der), Ok(7), "x509-parser takes it");
        assert!(usable(&odd, &ca_der).is_err(), "webpki does not");
        assert!(accept(&paths, &ca_der, &odd).is_err());
        assert!(!path(&paths).exists());
        assert!(usable(&crl(&issuer, 1, &[3]), &ca_der).is_ok());
    }

    /// One stored by an older client is moved aside, and connecting goes on
    /// without a CRL until the next usable one arrives.
    #[test]
    fn a_stored_crl_the_verifier_refuses_is_moved_aside() {
        let (dir, paths) = paths();
        let (ca_der, issuer) = ca("ICU CA a");
        let odd = crl_with_unknown_critical_extension(&ca_der, &issuer);
        std::fs::write(path(&paths), &odd).unwrap();
        assert_eq!(load(&paths, &ca_der), None);
        assert!(!path(&paths).exists());
        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("crl.der.rejected-")
            })
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read(aside[0].path()).unwrap(), odd);
        // The next usable CRL takes its place.
        assert_eq!(accept(&paths, &ca_der, &crl(&issuer, 1, &[])), Ok(true));
        assert!(load(&paths, &ca_der).is_some());
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
