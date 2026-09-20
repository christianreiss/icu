//! The server's own certificate authority (`Server/IMPLEMENTATION.md` §6.1,
//! §6.3).
//!
//! The server is its own CA: it issues the server leaf and every client
//! certificate, and it is the only thing clients trust. Key facts verified in
//! Spike 2b: the server overrides subject, SAN, EKU and validity, so nothing
//! from a CSR ends up in a certificate except the public key.

use std::{
    net::IpAddr,
    path::{Path, PathBuf},
};

use rand::RngCore;
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DistinguishedName,
    DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType, SerialNumber,
};
use rustls::pki_types::{
    pem::PemObject, CertificateDer, CertificateSigningRequestDer, PrivateKeyDer, PrivatePkcs8KeyDer,
};
use time::OffsetDateTime;

use crate::{
    config::TlsSection,
    error::{Context, Error, Result},
    store,
};

/// Common name of the CA certificate.
pub const CA_COMMON_NAME: &str = "ICU CA";

/// File names under the state directory.
const CA_CRT: &str = "ca.crt";
const CA_KEY: &str = "ca.key";
const SERVER_CRT: &str = "server.crt";
const SERVER_KEY: &str = "server.key";

/// CA and server certificate material.
pub struct Ca {
    dir: PathBuf,
    ca_der: CertificateDer<'static>,
    ca_key_pem: String,
    server_der: CertificateDer<'static>,
    server_key_pem: String,
}

impl std::fmt::Debug for Ca {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ca")
            .field("dir", &self.dir)
            .field("ca_fingerprint", &self.ca_fingerprint())
            .finish_non_exhaustive()
    }
}

impl Ca {
    /// Loads CA and server certificate from `dir`, creating whatever is
    /// missing (§6.1.2, §6.1.3).
    pub fn load_or_create(dir: &Path, tls: &TlsSection, server_name: &str) -> Result<Ca> {
        std::fs::create_dir_all(dir)?;
        let (ca_der, ca_key_pem) = match (
            std::fs::read(dir.join(CA_CRT)),
            std::fs::read_to_string(dir.join(CA_KEY)),
        ) {
            (Ok(crt), Ok(key)) => (read_cert_pem(&crt)?, key),
            _ => create_ca(dir, tls)?,
        };

        let mut ca = Ca {
            dir: dir.to_path_buf(),
            ca_der,
            ca_key_pem,
            server_der: CertificateDer::from(Vec::new()),
            server_key_pem: String::new(),
        };

        match (
            std::fs::read(dir.join(SERVER_CRT)),
            std::fs::read_to_string(dir.join(SERVER_KEY)),
        ) {
            (Ok(crt), Ok(key)) => {
                ca.server_der = read_cert_pem(&crt)?;
                ca.server_key_pem = key;
            }
            _ => ca.rotate_server_cert(tls, server_name)?,
        }
        Ok(ca)
    }

    /// Issues a fresh server leaf from the same CA. Clients need no change
    /// (`Server/IMPLEMENTATION.md` §5, `rotate-server-cert`).
    pub fn rotate_server_cert(&mut self, tls: &TlsSection, server_name: &str) -> Result<()> {
        let issuer = self.issuer()?;
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, server_name);
        params.subject_alt_names = vec![match server_name.parse::<IpAddr>() {
            Ok(ip) => SanType::IpAddress(ip),
            Err(_) => SanType::DnsName(
                server_name
                    .to_string()
                    .try_into()
                    .map_err(|e| Error::msg(format!("bad server_name {server_name:?}: {e}")))?,
            ),
        }];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        set_validity(&mut params, tls.server_validity_days);
        params.serial_number = Some(random_serial());

        // rcgen 0.14: the direct form takes the public key AND the issuer
        // (A10); only the CSR form takes the issuer alone.
        let cert = params.signed_by(&key, &issuer)?;
        self.server_der = cert.der().clone();
        self.server_key_pem = key.serialize_pem();
        store::write_atomic(&self.dir.join(SERVER_CRT), &cert.pem())?;
        store::write_private(&self.dir.join(SERVER_KEY), &self.server_key_pem)?;
        Ok(())
    }

    /// Signs a client CSR. Subject CN, EKU, validity and serial come from the
    /// server; only the public key comes from the CSR (Spike 2b).
    pub fn sign_client_csr(
        &self,
        csr_der: &[u8],
        common_name: &str,
        validity_days: u32,
    ) -> Result<CertificateDer<'static>> {
        if csr_der.len() > icu_proto::ControlMsg::MAX_CSR_DER {
            return Err(Error::coded(
                icu_proto::error::ENROLL_BAD_CSR,
                format!("csr is {} bytes, limit is 2 KiB", csr_der.len()),
            ));
        }
        let csr_der = CertificateSigningRequestDer::from(csr_der.to_vec());
        let mut csr = CertificateSigningRequestParams::from_der(&csr_der).map_err(|e| {
            Error::coded(
                icu_proto::error::ENROLL_BAD_CSR,
                format!("csr not parsable: {e}"),
            )
        })?;

        // Only P-256 and P-384 keys (§6.3.3).
        let alg = csr.public_key.algorithm();
        if alg != &rcgen::PKCS_ECDSA_P256_SHA256 && alg != &rcgen::PKCS_ECDSA_P384_SHA384 {
            return Err(Error::coded(
                icu_proto::error::ENROLL_BAD_CSR,
                "csr key is neither P-256 nor P-384",
            ));
        }

        csr.params.distinguished_name = DistinguishedName::new();
        csr.params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        csr.params.subject_alt_names = Vec::new();
        csr.params.is_ca = IsCa::NoCa;
        csr.params.key_usages = Vec::new();
        csr.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        csr.params.use_authority_key_identifier_extension = true;
        set_validity(&mut csr.params, validity_days);
        csr.params.serial_number = Some(random_serial());

        let issuer = self.issuer()?;
        let cert = csr.signed_by(&issuer).map_err(|e| {
            Error::coded(
                icu_proto::error::ENROLL_BAD_CSR,
                format!("cannot sign csr: {e}"),
            )
        })?;
        Ok(cert.der().clone())
    }

    /// The CA certificate in DER form.
    pub fn ca_der(&self) -> &CertificateDer<'static> {
        &self.ca_der
    }

    /// The server leaf in DER form.
    pub fn server_der(&self) -> &CertificateDer<'static> {
        &self.server_der
    }

    /// The chain the server presents: leaf + CA (`PROTOCOL.md` §2.3 requires
    /// exactly this for the enrollment pinning rule).
    pub fn server_chain(&self) -> Vec<CertificateDer<'static>> {
        vec![self.server_der.clone(), self.ca_der.clone()]
    }

    /// The server's private key.
    pub fn server_key(&self) -> Result<PrivateKeyDer<'static>> {
        let key = KeyPair::from_pem(&self.server_key_pem)?;
        Ok(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            key.serialize_der(),
        )))
    }

    /// SHA-256 of the CA DER, lowercase hex.
    pub fn ca_fingerprint(&self) -> String {
        store::fingerprint(&self.ca_der)
    }

    /// The `ca=` parameter of an invite link: base64url **without padding**
    /// over SHA-256 of the CA DER (`PROTOCOL.md` §2.3).
    pub fn ca_pin(&self) -> String {
        use base64::Engine;
        use sha2::Digest;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(&self.ca_der))
    }

    fn issuer(&self) -> Result<Issuer<'static, KeyPair>> {
        let key = KeyPair::from_pem(&self.ca_key_pem).context("cannot load ca.key")?;
        Issuer::from_ca_cert_der(&self.ca_der, key).context("cannot load ca.crt as issuer")
    }
}

fn create_ca(dir: &Path, tls: &TlsSection) -> Result<(CertificateDer<'static>, String)> {
    let key = KeyPair::generate()?; // P-256 by default (Spike 2b)
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, CA_COMMON_NAME);
    set_validity(&mut params, tls.ca_validity_days);
    params.serial_number = Some(random_serial());
    let cert = params.self_signed(&key)?;
    let key_pem = key.serialize_pem();
    store::write_atomic(&dir.join(CA_CRT), &cert.pem())?;
    store::write_private(&dir.join(CA_KEY), &key_pem)?;
    Ok((cert.der().clone(), key_pem))
}

fn set_validity(params: &mut CertificateParams, days: u32) {
    let now = OffsetDateTime::from(std::time::SystemTime::now());
    params.not_before = now - time::Duration::hours(1);
    params.not_after = now + time::Duration::days(i64::from(days));
}

fn random_serial() -> SerialNumber {
    let mut raw = [0u8; 16];
    rand::rng().fill_bytes(&mut raw);
    // A leading zero bit keeps the DER INTEGER positive without a pad byte.
    raw[0] &= 0x7F;
    SerialNumber::from_slice(&raw)
}

fn read_cert_pem(bytes: &[u8]) -> Result<CertificateDer<'static>> {
    CertificateDer::from_pem_slice(bytes)
        .map(|c| c.into_owned())
        .map_err(|e| Error::msg(format!("cannot parse certificate pem: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use x509_parser::prelude::FromDer;

    fn tls() -> TlsSection {
        TlsSection::default()
    }

    fn parse(der: &[u8]) -> x509_parser::certificate::X509Certificate<'_> {
        x509_parser::certificate::X509Certificate::from_der(der)
            .unwrap()
            .1
    }

    fn common_name(der: &[u8]) -> String {
        parse(der)
            .subject()
            .iter_common_name()
            .next()
            .and_then(|a| a.as_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    /// §10: CA creation.
    #[test]
    fn ca_is_created_once_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "icu.example.org").unwrap();
        let fp = ca.ca_fingerprint();
        assert_eq!(fp.len(), 64);
        assert_eq!(common_name(ca.ca_der()), CA_COMMON_NAME);
        assert_eq!(common_name(ca.server_der()), "icu.example.org");
        assert_eq!(ca.server_chain().len(), 2, "leaf + ca");

        let x = parse(ca.ca_der());
        assert!(x.basic_constraints().unwrap().unwrap().value.ca);
        assert_eq!(
            x.basic_constraints()
                .unwrap()
                .unwrap()
                .value
                .path_len_constraint,
            Some(0)
        );

        drop(ca);
        let again = Ca::load_or_create(dir.path(), &tls(), "icu.example.org").unwrap();
        assert_eq!(again.ca_fingerprint(), fp, "same CA after reload");
        assert!(again.server_key().is_ok());
    }

    #[test]
    fn ca_key_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        for name in [CA_KEY, SERVER_KEY] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name} must be 0600");
        }
    }

    /// §10: the CSR override from Spike 2b — a cheeky CN does not survive.
    #[test]
    fn csr_subject_and_eku_are_overridden() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();

        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, "admin-impersonator");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let csr = params.serialize_request(&key).unwrap();

        let leaf = ca.sign_client_csr(csr.der(), "alice", 365).unwrap();
        assert_eq!(common_name(&leaf), "alice");
        let x = parse(&leaf);
        assert!(
            x.basic_constraints().unwrap().is_none()
                || !x.basic_constraints().unwrap().unwrap().value.ca,
            "the CSR must not be able to request a CA certificate"
        );
        let eku = x.extended_key_usage().unwrap().unwrap().value;
        assert!(eku.client_auth && !eku.server_auth);
        assert_eq!(
            x.issuer()
                .iter_common_name()
                .next()
                .unwrap()
                .as_str()
                .unwrap(),
            CA_COMMON_NAME
        );
    }

    /// §10: fingerprint stability — the same DER always hashes the same, two
    /// certificates never collide.
    #[test]
    fn fingerprints_are_stable_and_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let key = KeyPair::generate().unwrap();
        let csr = CertificateParams::default()
            .serialize_request(&key)
            .unwrap();

        let a = ca.sign_client_csr(csr.der(), "alice", 365).unwrap();
        let b = ca.sign_client_csr(csr.der(), "alice", 365).unwrap();
        assert_eq!(store::fingerprint(&a), store::fingerprint(&a));
        assert_ne!(
            store::fingerprint(&a),
            store::fingerprint(&b),
            "a fresh serial makes every issued certificate distinct"
        );
    }

    #[test]
    fn bad_csrs_are_rejected_with_enroll_bad_csr() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();

        let err = ca.sign_client_csr(b"not a csr", "alice", 365).unwrap_err();
        assert_eq!(err.code(), Some(icu_proto::error::ENROLL_BAD_CSR));

        let err = ca
            .sign_client_csr(&vec![0u8; 3000], "alice", 365)
            .unwrap_err();
        assert_eq!(err.code(), Some(icu_proto::error::ENROLL_BAD_CSR));

        // RSA keys are refused: §6.3.3 allows P-256 and P-384 only.
        let rsa = KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256);
        if let Ok(rsa) = rsa {
            let csr = CertificateParams::default()
                .serialize_request(&rsa)
                .unwrap();
            let err = ca.sign_client_csr(csr.der(), "alice", 365).unwrap_err();
            assert_eq!(err.code(), Some(icu_proto::error::ENROLL_BAD_CSR));
        }
    }

    #[test]
    fn rotating_the_server_cert_keeps_the_ca() {
        let dir = tempfile::tempdir().unwrap();
        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let ca_fp = ca.ca_fingerprint();
        let old = store::fingerprint(ca.server_der());
        ca.rotate_server_cert(&tls(), "localhost").unwrap();
        assert_eq!(ca.ca_fingerprint(), ca_fp);
        assert_ne!(store::fingerprint(ca.server_der()), old);
        assert!(ca.server_key().is_ok());
    }

    #[test]
    fn ca_pin_is_base64url_without_padding() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let pin = ca.ca_pin();
        assert_eq!(pin.len(), 43, "32 bytes base64url unpadded");
        assert!(!pin.contains('='));
        assert!(!pin.contains('+') && !pin.contains('/'));
    }

    #[test]
    fn an_ip_server_name_becomes_an_ip_san() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "192.0.2.10").unwrap();
        let x = parse(ca.server_der());
        let san = x.subject_alternative_name().unwrap().unwrap();
        assert!(san
            .value
            .general_names
            .iter()
            .any(|n| matches!(n, x509_parser::extensions::GeneralName::IPAddress(_))));
    }
}
