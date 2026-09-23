//! The server's own certificate authority (`Server/IMPLEMENTATION.md` §6.1,
//! §6.3).
//!
//! The server is its own CA: it issues the server leaf and every client
//! certificate, and it is the only thing clients trust. SAN, EKU, basic
//! constraints, key usage, validity and serial are server-set; from a CSR come
//! the public key **and the validated subject CN**, which is the user's display
//! name (`PROTOCOL.md` §2.1, revised 2026-09-22).
//!
//! The certificates also say where the server is, so a passport needs nothing
//! besides key, leaf and CA (`PROTOCOL.md` §2.3): every client certificate
//! carries a URI SAN `icu://<public_host>`, and the CA's CN is
//! `ICU CA <server_name>`.

use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
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

/// Common name of the CA certificate, or its prefix: a CA created since the
/// cert-only registration carries `ICU CA <server_name>`, which is where a
/// client takes its SNI from.
pub const CA_COMMON_NAME: &str = "ICU CA";

/// Organizational unit of a registration certificate; the server decides by
/// `CertRecord.registration`, never by this.
pub use icu_proto::limits::REGISTRATION_OU;

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
            _ => create_ca(dir, tls, server_name)?,
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

    /// Signs a client CSR. The public key and the validated subject CN come
    /// from the CSR; SAN, EKU, basic constraints, key usage, validity and
    /// serial are server-set. The only SAN is `icu://<public_host>`. Returns
    /// the leaf and the CN it carries.
    pub fn sign_client_csr(
        &self,
        csr_der: &[u8],
        validity_days: u32,
        public_host: &str,
    ) -> Result<(CertificateDer<'static>, String)> {
        let common_name = csr_common_name(csr_der)?;
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

        // Rebuilt from scratch even though the CN is adopted: this canonicalises
        // the encoding to a UTF8String and drops every other DN attribute.
        csr.params.distinguished_name = DistinguishedName::new();
        csr.params
            .distinguished_name
            .push(DnType::CommonName, common_name.clone());
        csr.params.subject_alt_names = vec![server_uri(public_host)?];
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
        Ok((cert.der().clone(), common_name))
    }

    /// Issues a registration certificate on a freshly generated key
    /// (`PROTOCOL.md` §2.3): subject `CN=<name>, OU=ICU registration`, the
    /// server URI SAN, EKU ClientAuth, valid for `ttl`. Returns the leaf and
    /// the key as PKCS#8 DER — the key leaves the server inside the bundle and
    /// is not kept.
    pub fn issue_registration(
        &self,
        name: &str,
        ttl: Duration,
        public_host: &str,
    ) -> Result<(CertificateDer<'static>, Vec<u8>)> {
        icu_proto::validate_name(name)?;
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, name);
        params
            .distinguished_name
            .push(DnType::OrganizationalUnitName, REGISTRATION_OU);
        params.subject_alt_names = vec![server_uri(public_host)?];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params.use_authority_key_identifier_extension = true;
        let now = OffsetDateTime::from(std::time::SystemTime::now());
        params.not_before = now - time::Duration::hours(1);
        params.not_after = now + ttl;
        params.serial_number = Some(random_serial());
        let cert = params.signed_by(&key, &self.issuer()?)?;
        Ok((cert.der().clone(), key.serialize_der()))
    }

    /// The CA certificate in DER form.
    pub fn ca_der(&self) -> &CertificateDer<'static> {
        &self.ca_der
    }

    /// The server leaf in DER form.
    pub fn server_der(&self) -> &CertificateDer<'static> {
        &self.server_der
    }

    /// The chain the server presents: leaf + CA.
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

    fn issuer(&self) -> Result<Issuer<'static, KeyPair>> {
        let key = KeyPair::from_pem(&self.ca_key_pem).context("cannot load ca.key")?;
        Issuer::from_ca_cert_der(&self.ca_der, key).context("cannot load ca.crt as issuer")
    }
}

/// The display name a CSR asks for: exactly one subject CN, readable as UTF-8,
/// passing `validate_name` (`PROTOCOL.md` §2.3).
///
/// Parsed with x509-parser on the raw DER rather than through rcgen's
/// `DistinguishedName`: that one is a map keyed by `DnType`, so a CSR carrying
/// two CN attributes would silently collapse to one and "exactly one CN" would
/// not be checkable at all.
pub fn csr_common_name(csr_der: &[u8]) -> Result<String> {
    use x509_parser::prelude::FromDer;

    if csr_der.len() > icu_proto::ControlMsg::MAX_CSR_DER {
        return Err(Error::coded(
            icu_proto::error::ENROLL_BAD_CSR,
            format!("csr is {} bytes, limit is 2 KiB", csr_der.len()),
        ));
    }
    let (_, csr) =
        x509_parser::certification_request::X509CertificationRequest::from_der(csr_der).map_err(
            |e| {
                Error::coded(
                    icu_proto::error::ENROLL_BAD_CSR,
                    format!("csr not parsable: {e}"),
                )
            },
        )?;
    let mut cns = csr.certification_request_info.subject.iter_common_name();
    let Some(cn) = cns.next() else {
        return Err(Error::coded(
            icu_proto::error::ENROLL_BAD_CSR,
            "csr subject has no common name",
        ));
    };
    if cns.next().is_some() {
        return Err(Error::coded(
            icu_proto::error::ENROLL_BAD_CSR,
            "csr subject has more than one common name",
        ));
    }
    let cn = cn.as_str().map_err(|_| {
        Error::coded(
            icu_proto::error::ENROLL_BAD_CSR,
            "csr common name is not readable as utf-8",
        )
    })?;
    icu_proto::validate_name(cn)
        .map_err(|e| Error::coded(icu_proto::error::ENROLL_BAD_CSR, format!("bad name: {e}")))?;
    Ok(cn.to_string())
}

/// The raw SubjectPublicKeyInfo of a CSR. A registration renewal compares it
/// with the registration certificate's to insist on a fresh key.
pub fn csr_public_key(csr_der: &[u8]) -> Result<Vec<u8>> {
    use x509_parser::prelude::FromDer;
    let (_, csr) = x509_parser::certification_request::X509CertificationRequest::from_der(csr_der)
        .map_err(|e| {
            Error::coded(
                icu_proto::error::RENEW_REJECTED,
                format!("csr not parsable: {e}"),
            )
        })?;
    Ok(csr.certification_request_info.subject_pki.raw.to_vec())
}

/// The raw SubjectPublicKeyInfo of a leaf, empty when it does not parse.
pub fn leaf_public_key(der: &[u8]) -> Vec<u8> {
    use x509_parser::prelude::FromDer;
    x509_parser::certificate::X509Certificate::from_der(der)
        .map(|(_, cert)| cert.public_key().raw.to_vec())
        .unwrap_or_default()
}

/// `icu://<public_host>` as a URI SAN.
fn server_uri(public_host: &str) -> Result<SanType> {
    let uri = format!("icu://{public_host}");
    Ok(SanType::URI(uri.clone().try_into().map_err(|e| {
        Error::msg(format!("bad public_host {public_host:?}: {e}"))
    })?))
}

/// The subject CN of an already verified leaf, or `""` when it has none or does
/// not parse. Never fails: an unreadable CN must not cost a client its
/// connection, it only means the client has to catch up (`PROTOCOL.md` §2.2).
pub fn leaf_common_name(der: &[u8]) -> String {
    use x509_parser::prelude::FromDer;

    x509_parser::certificate::X509Certificate::from_der(der)
        .ok()
        .and_then(|(_, cert)| {
            cert.subject()
                .iter_common_name()
                .next()
                .and_then(|a| a.as_str().ok())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn create_ca(
    dir: &Path,
    tls: &TlsSection,
    server_name: &str,
) -> Result<(CertificateDer<'static>, String)> {
    let key = KeyPair::generate()?; // P-256 by default (Spike 2b)
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, format!("{CA_COMMON_NAME} {server_name}"));
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

    const HOST: &str = "icu.example.org:4711";

    fn tls() -> TlsSection {
        TlsSection::default()
    }

    /// The URI SANs of a leaf.
    fn uris(der: &[u8]) -> Vec<String> {
        parse(der)
            .subject_alternative_name()
            .unwrap()
            .map(|san| {
                san.value
                    .general_names
                    .iter()
                    .filter_map(|n| match n {
                        x509_parser::extensions::GeneralName::URI(u) => Some(u.to_string()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
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
        assert_eq!(common_name(ca.ca_der()), "ICU CA icu.example.org");
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

    /// A CSR for `cn`, plus its key so the caller can keep it.
    fn csr_with_cn(cn: &str) -> (KeyPair, Vec<u8>) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, cn);
        let csr = params.serialize_request(&key).unwrap().der().to_vec();
        (key, csr)
    }

    /// §10: the CN is adopted from the CSR (it *is* the name the user picked),
    /// everything else in the subject and the extensions is not.
    #[test]
    fn csr_subject_is_adopted_but_validated() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();

        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        // A cheeky name is just a name: uniqueness and impersonation are the
        // store's business, not the CA's.
        params
            .distinguished_name
            .push(DnType::CommonName, "admin-impersonator");
        params.distinguished_name.push(DnType::OrganizationName, "evil corp");
        params.subject_alt_names = vec![SanType::DnsName("icu.example.org".try_into().unwrap())];
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let csr = params.serialize_request(&key).unwrap();

        let (leaf, cn) = ca.sign_client_csr(csr.der(), 365, HOST).unwrap();
        assert_eq!(cn, "admin-impersonator");
        assert_eq!(common_name(&leaf), "admin-impersonator");
        assert_eq!(leaf_common_name(&leaf), "admin-impersonator");
        let x = parse(&leaf);
        assert_eq!(
            uris(&leaf),
            vec!["icu://icu.example.org:4711".to_string()],
            "the only SAN is the server-set URI; the CSR's DNS SAN is dropped"
        );
        assert_eq!(
            x.subject_alternative_name()
                .unwrap()
                .unwrap()
                .value
                .general_names
                .len(),
            1
        );
        assert_eq!(
            x.subject().iter().count(),
            1,
            "every DN attribute besides the CN is dropped"
        );
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
            "ICU CA localhost"
        );
    }

    /// A registration certificate: fresh key, placeholder CN, the marker OU,
    /// the server URI and the requested lifetime.
    #[test]
    fn registration_certificates_carry_key_uri_and_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let (leaf, key_der) = ca
            .issue_registration("neu-1a2b", Duration::from_secs(3600), HOST)
            .unwrap();
        assert_eq!(common_name(&leaf), "neu-1a2b");
        let x = parse(&leaf);
        let ou: Vec<_> = x
            .subject()
            .iter_organizational_unit()
            .filter_map(|a| a.as_str().ok())
            .collect();
        assert_eq!(ou, vec![REGISTRATION_OU]);
        assert_eq!(uris(&leaf), vec!["icu://icu.example.org:4711".to_string()]);
        let eku = x.extended_key_usage().unwrap().unwrap().value;
        assert!(eku.client_auth && !eku.server_auth);
        let lifetime = x.validity().not_after.timestamp() - x.validity().not_before.timestamp();
        assert_eq!(lifetime, 3600 + 3600, "ttl plus the hour of clock slack");

        let key = KeyPair::try_from(key_der.as_slice()).unwrap();
        assert_eq!(leaf_public_key(&leaf), rcgen::PublicKeyData::subject_public_key_info(&key));
        assert!(ca
            .issue_registration("bad\nname", Duration::from_secs(60), HOST)
            .is_err());
    }

    #[test]
    fn csr_public_key_matches_the_signing_key() {
        let (key, csr) = csr_with_cn("alice");
        assert_eq!(csr_public_key(&csr).unwrap(), rcgen::PublicKeyData::subject_public_key_info(&key));
        assert!(csr_public_key(b"junk").is_err());
        assert!(leaf_public_key(b"junk").is_empty());
    }

    /// §10: fingerprint stability — the same DER always hashes the same, two
    /// certificates never collide.
    #[test]
    fn fingerprints_are_stable_and_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let (_key, csr) = csr_with_cn("alice");

        let (a, _) = ca.sign_client_csr(&csr, 365, HOST).unwrap();
        let (b, _) = ca.sign_client_csr(&csr, 365, HOST).unwrap();
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

        let err = ca.sign_client_csr(b"not a csr", 365, HOST).unwrap_err();
        assert_eq!(err.code(), Some(icu_proto::error::ENROLL_BAD_CSR));

        let err = ca.sign_client_csr(&vec![0u8; 3000], 365, HOST).unwrap_err();
        assert_eq!(err.code(), Some(icu_proto::error::ENROLL_BAD_CSR));

        // RSA keys are refused: §6.3.3 allows P-256 and P-384 only. The CN is
        // valid so the key check is what fires.
        let rsa = KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256);
        if let Ok(rsa) = rsa {
            let mut params = CertificateParams::default();
            params.distinguished_name.push(DnType::CommonName, "alice");
            let csr = params.serialize_request(&rsa).unwrap();
            let err = ca.sign_client_csr(csr.der(), 365, HOST).unwrap_err();
            assert_eq!(err.code(), Some(icu_proto::error::ENROLL_BAD_CSR));
        }
    }

    #[test]
    fn a_csr_without_a_cn_is_rejected() {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        // rcgen's default DN carries a placeholder CN; an empty subject is the
        // case under test.
        params.distinguished_name = DistinguishedName::new();
        let csr = params.serialize_request(&key).unwrap();
        let err = csr_common_name(csr.der()).unwrap_err();
        assert_eq!(err.code(), Some(icu_proto::error::ENROLL_BAD_CSR));
    }

    /// DER: tag + definite length + contents.
    fn der(tag: u8, contents: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        let n = contents.len();
        if n < 0x80 {
            out.push(n as u8);
        } else if n < 0x100 {
            out.extend_from_slice(&[0x81, n as u8]);
        } else {
            out.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]);
        }
        out.extend_from_slice(contents);
        out
    }

    /// The reason the CN is read with x509-parser: rcgen's `DistinguishedName`
    /// is keyed by `DnType`, so a second CN attribute would silently collapse
    /// into the first and this case would be unobservable there.
    ///
    /// The CSR is rebuilt by hand around a real public key. Its signature does
    /// not match any more, which is fine: `csr_common_name` reads the subject,
    /// and a CSR that gets past it still has to survive rcgen's signature check
    /// in `sign_client_csr`.
    #[test]
    fn a_csr_with_two_cns_is_rejected() {
        use x509_parser::prelude::FromDer;

        let (_key, csr) = csr_with_cn("alice");
        let (_, parsed) =
            x509_parser::certification_request::X509CertificationRequest::from_der(&csr).unwrap();
        let info = &parsed.certification_request_info;

        // AttributeTypeAndValue { OID 2.5.4.3 (CN), UTF8String }
        let cn_attr = |name: &str| {
            let oid = der(0x06, &[0x55, 0x04, 0x03]);
            let value = der(0x0c, name.as_bytes());
            let mut inner = oid;
            inner.extend_from_slice(&value);
            der(0x31, &der(0x30, &inner))
        };
        let mut rdns = cn_attr("alice");
        rdns.extend_from_slice(&cn_attr("mallory"));
        let subject = der(0x30, &rdns);

        let mut cri = vec![0x02, 0x01, 0x00]; // version 0
        cri.extend_from_slice(&subject);
        cri.extend_from_slice(info.subject_pki.raw);
        cri.extend_from_slice(&[0xa0, 0x00]); // attributes [0] IMPLICIT, empty
        let cri = der(0x30, &cri);

        let mut body = cri;
        body.extend_from_slice(parsed.signature_algorithm.oid().as_bytes());
        body.extend_from_slice(&der(0x30, &[])); // placeholder algorithm params
        body.extend_from_slice(&der(0x03, &[0x00, 0x00])); // placeholder signature
        let forged = der(0x30, &body);

        let err = csr_common_name(&forged).unwrap_err();
        assert_eq!(err.code(), Some(icu_proto::error::ENROLL_BAD_CSR));
    }

    #[test]
    fn a_csr_cn_must_pass_validate_name() {
        for bad in ["", "a\nb", &"x".repeat(33)] {
            let key = KeyPair::generate().unwrap();
            let mut params = CertificateParams::default();
            params.distinguished_name.push(DnType::CommonName, bad);
            let csr = params.serialize_request(&key).unwrap();
            let err = csr_common_name(csr.der()).unwrap_err();
            assert_eq!(
                err.code(),
                Some(icu_proto::error::ENROLL_BAD_CSR),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn leaf_common_name_degrades_to_empty_on_garbage() {
        assert_eq!(leaf_common_name(b"not a certificate"), "");
        assert_eq!(leaf_common_name(&[]), "");
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
