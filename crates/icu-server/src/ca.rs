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
use x509_parser::certificate::X509Certificate;

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
/// Replaced server leaves that are not on the CRL yet, PEM one after the
/// other. Gone again once the store has them ([`Ca::retired_recorded`]).
const SERVER_RETIRED: &str = "server.crt.retired";

/// CA and server certificate material.
pub struct Ca {
    dir: PathBuf,
    ca_der: CertificateDer<'static>,
    ca_key_pem: String,
    /// The CA, ready to sign: parsed once, not per leaf or CRL.
    issuer: Issuer<'static, KeyPair>,
    server_der: CertificateDer<'static>,
    server_key: PrivateKeyDer<'static>,
    /// Server leaves replaced here but not on the CRL yet: every rotation
    /// adds the leaf it replaces, and loading adds a leaf that did not match
    /// its key. Whoever holds the store records them
    /// ([`Ca::take_retired_leaves`]). Kept in `server.crt.retired` as well,
    /// from before the rotation replaces anything until they are recorded,
    /// so a crash or a failed store write in between loses no revocation.
    retired: Vec<CertificateDer<'static>>,
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
    ///
    /// A new CA is created only where there has never been one: `ca.crt` and
    /// `ca.key` both absent. Anything else that keeps them from loading — one
    /// of them lost, unreadable for this user after a restore, garbled, or not
    /// a pair — is an error: a new CA would silently invalidate every
    /// certificate, passport and bundle ever issued, and the old key with it.
    ///
    /// The server leaf is disposable. When it does not load, does not match
    /// its key or was not issued by this CA — a crash between the two writes
    /// of a rotation, a restore mixing files — a new one is issued, and the
    /// one found is kept for the CRL ([`Ca::take_retired_leaves`]), as is
    /// every leaf `server.crt.retired` lists, except the one in use.
    pub fn load_or_create(dir: &Path, tls: &TlsSection, server_name: &str) -> Result<Ca> {
        Ca::load_or_create_if(dir, tls, server_name, || Ok(()))
    }

    /// [`Ca::load_or_create`], asking `may_create` first where it would
    /// create a new CA; its error is returned instead. What a new CA needs
    /// beyond the two files missing — a state that never saw a user — is the
    /// caller's to know.
    pub fn load_or_create_if(
        dir: &Path,
        tls: &TlsSection,
        server_name: &str,
        may_create: impl FnOnce() -> Result<()>,
    ) -> Result<Ca> {
        use std::io::ErrorKind::NotFound;
        std::fs::create_dir_all(dir)?;
        let (ca_der, ca_key_pem) = match (
            std::fs::read(dir.join(CA_CRT)),
            std::fs::read_to_string(dir.join(CA_KEY)),
        ) {
            (Ok(crt), Ok(key)) => (read_cert_pem(&crt).context("ca.crt")?, key),
            (Err(crt), Err(key)) if crt.kind() == NotFound && key.kind() == NotFound => {
                may_create()?;
                tracing::warn!(
                    dir = %dir.display(),
                    server_name,
                    "no CA in the state directory, creating a new one"
                );
                create_ca(dir, tls, server_name)?
            }
            (crt, key) => {
                return Err(Error::msg(format!(
                    "cannot load the CA from {} ({}; {}). Refusing to create a new \
                     one: it would invalidate every certificate and bundle issued so \
                     far. Restore ca.crt and ca.key together, readable for the \
                     server, or move the state directory aside to start over with a \
                     new CA",
                    dir.display(),
                    file_state(CA_CRT, &crt),
                    file_state(CA_KEY, &key)
                )));
            }
        };
        let key = KeyPair::from_pem(&ca_key_pem).context("cannot load ca.key")?;
        if rcgen::PublicKeyData::subject_public_key_info(&key) != leaf_public_key(&ca_der) {
            return Err(Error::msg(format!(
                "ca.key in {} does not belong to ca.crt; refusing to start. Restore \
                 the pair from the same backup",
                dir.display()
            )));
        }

        let mut ca = Ca {
            dir: dir.to_path_buf(),
            issuer: issuer(&ca_der, &ca_key_pem)?,
            ca_der,
            ca_key_pem,
            server_der: CertificateDer::from(Vec::new()),
            server_key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(Vec::new())),
            retired: read_retired(dir)?,
        };

        match (
            std::fs::read(dir.join(SERVER_CRT)),
            std::fs::read_to_string(dir.join(SERVER_KEY)),
        ) {
            (Err(crt), Err(key)) if crt.kind() == NotFound && key.kind() == NotFound => {
                ca.rotate_server_cert(tls, server_name)?;
            }
            (crt, key) => {
                let found = crt.as_deref().ok().and_then(|pem| read_cert_pem(pem).ok());
                match ca.server_pair(found.as_ref(), key.as_deref().ok()) {
                    Ok((der, key)) => {
                        // Listed, yet still the pair on disk: the rotation
                        // that listed it never got to replace it. In use,
                        // so not retired.
                        ca.retired.retain(|leaf| *leaf != der);
                        ca.server_der = der;
                        ca.server_key = key;
                    }
                    Err(why) => {
                        tracing::warn!(
                            dir = %dir.display(),
                            reason = %why,
                            "server certificate unusable, issuing a new one"
                        );
                        // Whatever leaf was found goes on the CRL: after a
                        // crash mid-rotation it is the one being replaced.
                        // As the outgoing leaf, the rotation lists it before
                        // it writes the new pair.
                        if let Some(found) = found.filter(|der| !der.is_empty()) {
                            ca.server_der = found;
                        }
                        ca.rotate_server_cert(tls, server_name)?;
                    }
                }
            }
        }
        Ok(ca)
    }

    /// The server leaf and key as found on disk, if they are a pair and the
    /// leaf was issued by this CA; else why not.
    fn server_pair(
        &self,
        crt: Option<&CertificateDer<'static>>,
        key_pem: Option<&str>,
    ) -> std::result::Result<(CertificateDer<'static>, PrivateKeyDer<'static>), String> {
        let crt = crt.ok_or_else(|| format!("{SERVER_CRT} missing or unreadable"))?;
        let key = key_pem
            .ok_or_else(|| format!("{SERVER_KEY} missing or unreadable"))
            .and_then(|pem| KeyPair::from_pem(pem).map_err(|e| format!("{SERVER_KEY}: {e}")))?;
        if rcgen::PublicKeyData::subject_public_key_info(&key) != leaf_public_key(crt) {
            return Err(format!("{SERVER_KEY} does not belong to {SERVER_CRT}"));
        }
        if !issued_by(crt, &self.ca_der) {
            return Err(format!("{SERVER_CRT} was not issued by this CA"));
        }
        Ok((crt.clone(), pkcs8(&key)))
    }

    /// Server leaves that were replaced but are not on the CRL yet; the
    /// caller puts them there and then calls [`Ca::retired_recorded`]. Hand
    /// them back with [`Ca::keep_retired_leaves`] when that fails.
    pub fn take_retired_leaves(&mut self) -> Vec<CertificateDer<'static>> {
        std::mem::take(&mut self.retired)
    }

    /// Returns leaves from [`Ca::take_retired_leaves`] that could not be put
    /// on the CRL, for the next attempt.
    pub fn keep_retired_leaves(&mut self, leaves: Vec<CertificateDer<'static>>) {
        self.retired.extend(leaves);
    }

    /// Brings `server.crt.retired` in line once the leaves from
    /// [`Ca::take_retired_leaves`] are on the CRL: it keeps only what is
    /// still pending, normally nothing, and then the file goes.
    pub fn retired_recorded(&self) -> Result<()> {
        write_retired(&self.dir, &self.retired)
    }

    /// Issues a fresh server leaf from the same CA. Clients need no change
    /// (`Server/IMPLEMENTATION.md` §5, `rotate-server-cert`). The leaf it
    /// replaces is retired ([`Ca::take_retired_leaves`]).
    ///
    /// The leaf being replaced goes into `server.crt.retired` before anything
    /// is replaced, so no crash or failed store write afterwards can keep it
    /// off the CRL for good: [`Ca::load_or_create`] retires it from there. A
    /// crash before the new pair is written leaves the live leaf listed,
    /// which loading drops again. The key is written before the certificate:
    /// a crash between the two leaves a pair that does not match, which
    /// [`Ca::load_or_create`] repairs with a new leaf, retiring the old one
    /// on disk as this rotation meant to.
    pub fn rotate_server_cert(&mut self, tls: &TlsSection, server_name: &str) -> Result<()> {
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
        set_validity(&mut params, tls.server_validity_days)?;
        params.serial_number = Some(random_serial());

        // rcgen 0.14: the direct form takes the public key AND the issuer
        // (A10); only the CSR form takes the issuer alone.
        let cert = params.signed_by(&key, &self.issuer)?;
        let mut retired = self.retired.clone();
        if !self.server_der.is_empty() && !retired.contains(&self.server_der) {
            retired.push(self.server_der.clone());
        }
        write_retired(&self.dir, &retired)?;
        let written = store::write_private(&self.dir.join(SERVER_KEY), &key.serialize_pem())
            .and_then(|()| store::write_atomic(&self.dir.join(SERVER_CRT), &cert.pem()));
        if let Err(e) = written {
            // The live leaf stays in use, so off the list again; should even
            // that fail, loading drops it.
            let _ = write_retired(&self.dir, &self.retired);
            return Err(e);
        }
        self.server_der = cert.der().clone();
        self.server_key = pkcs8(&key);
        self.retired = retired;
        Ok(())
    }

    /// Re-issues the CA certificate under the **same key and subject** with a
    /// fresh validity. Nothing else has to follow: rustls anchors trust on
    /// subject and public key only, never on the anchor's dates, and the key
    /// identifier is derived from the key, so every leaf, CRL and client
    /// holding the old `ca.der` keeps verifying. It keeps tooling that does
    /// look at the dates (openssl, Checkmk) quiet, and new bundles carry it.
    pub fn renew_ca_cert(&mut self, tls: &TlsSection) -> Result<()> {
        // The subject is rebuilt exactly as `create_ca` built it, a lone CN;
        // `ca_renewal_keeps_subject_and_key` pins the bytes.
        let cn = leaf_common_name(&self.ca_der);
        if cn.is_empty() {
            return Err(Error::msg("ca.crt has no readable CN"));
        }
        let cert = ca_params(&cn, tls.ca_validity_days)?.self_signed(self.issuer.key())?;
        store::write_atomic(&self.dir.join(CA_CRT), &cert.pem())?;
        self.ca_der = cert.der().clone();
        self.issuer = issuer(&self.ca_der, &self.ca_key_pem)?;
        Ok(())
    }

    /// Signs a client CSR. The public key and the validated subject CN come
    /// from the CSR; SAN, EKU, basic constraints, key usage, validity and
    /// serial are server-set. The only SAN is `icu://<public_host>`. Returns
    /// the leaf and the CN it carries.
    pub fn sign_client_csr(
        &self,
        csr_der: &[u8],
        lifetime: impl Into<Lifetime>,
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
        match lifetime.into() {
            Lifetime::Days(days) => set_validity(&mut csr.params, days)?,
            Lifetime::Keep {
                not_before,
                not_after,
            } => {
                csr.params.not_before = not_before;
                csr.params.not_after = not_after;
            }
        }
        csr.params.serial_number = Some(random_serial());

        let cert = csr.signed_by(&self.issuer).map_err(|e| {
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
        let now = store::now();
        params.not_before = now - time::Duration::hours(1);
        params.not_after = now + ttl;
        params.serial_number = Some(random_serial());
        let cert = params.signed_by(&key, &self.issuer)?;
        Ok((cert.der().clone(), key.serialize_der()))
    }

    /// Signs a CRL listing `serials` (lowercase hex, see [`leaf_serial`]) with
    /// the CA key (`PROTOCOL.md` §1). One CRL covers client and server leaves:
    /// the server checks client certificates against it during the
    /// handshake, clients check the server leaf.
    pub fn crl(
        &self,
        number: u64,
        serials: &[String],
    ) -> Result<rustls::pki_types::CertificateRevocationListDer<'static>> {
        let now = store::now();
        let revoked_certs = serials
            .iter()
            .filter_map(|hex| store::decode_hex(hex))
            .filter(|raw| !raw.is_empty())
            .map(|raw| rcgen::RevokedCertParams {
                serial_number: SerialNumber::from_slice(&raw),
                revocation_time: now,
                reason_code: None,
                invalidity_date: None,
            })
            .collect();
        let params = rcgen::CertificateRevocationListParams {
            this_update: now - time::Duration::hours(1),
            next_update: now + CRL_NEXT_UPDATE,
            crl_number: SerialNumber::from_slice(&minimal_be(number)),
            issuing_distribution_point: None,
            revoked_certs,
            key_identifier_method: rcgen::KeyIdMethod::Sha256,
        };
        let crl = params
            .signed_by(&self.issuer)
            .map_err(|e| Error::msg(format!("cannot sign the CRL: {e}")))?;
        Ok(crl.der().clone())
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
    pub fn server_key(&self) -> PrivateKeyDer<'static> {
        self.server_key.clone_key()
    }

    /// SHA-256 of the CA DER, lowercase hex.
    pub fn ca_fingerprint(&self) -> String {
        store::fingerprint(&self.ca_der)
    }
}

/// The CA as a signer, from its certificate and PEM key.
fn issuer(ca_der: &CertificateDer<'static>, key_pem: &str) -> Result<Issuer<'static, KeyPair>> {
    let key = KeyPair::from_pem(key_pem).context("cannot load ca.key")?;
    Issuer::from_ca_cert_der(ca_der, key).context("cannot load ca.crt as issuer")
}

/// A key pair as rustls wants it.
fn pkcs8(key: &KeyPair) -> PrivateKeyDer<'static> {
    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()))
}

/// The display name a CSR asks for: exactly one subject CN, readable as UTF-8,
/// passing `validate_stored_name` (`PROTOCOL.md` §2.3). Whether it also has
/// to pass the strict `validate_name` is the caller's business: a new name
/// does, a stored name carried into a renewal does not, or a user whose name
/// predates the strict rules could never renew.
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
    let (_, csr) = x509_parser::certification_request::X509CertificationRequest::from_der(csr_der)
        .map_err(|e| {
            Error::coded(
                icu_proto::error::ENROLL_BAD_CSR,
                format!("csr not parsable: {e}"),
            )
        })?;
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
    icu_proto::validate_stored_name(cn)
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

/// Runs `f` on a parsed certificate, `None` when the DER does not parse.
/// Every `leaf_*` reader goes through here.
fn with_leaf<R>(der: &[u8], f: impl FnOnce(&X509Certificate<'_>) -> R) -> Option<R> {
    use x509_parser::prelude::FromDer;
    X509Certificate::from_der(der)
        .ok()
        .map(|(_, cert)| f(&cert))
}

/// What a session needs to know about a presented leaf, from one parse.
/// Each field degrades on its own when the DER does not parse, like the
/// `leaf_*` readers: authentication is the fingerprint, never these.
#[derive(Debug, Clone, Default)]
pub struct LeafInfo {
    /// Subject CN, `""` when unreadable.
    pub cn: String,
    /// Raw SubjectPublicKeyInfo, empty when unreadable.
    pub spki: Vec<u8>,
    /// notBefore and notAfter, `None` when unreadable.
    pub validity: Option<(OffsetDateTime, OffsetDateTime)>,
}

/// Reads [`LeafInfo`] from a certificate.
pub fn leaf_info(der: &[u8]) -> LeafInfo {
    with_leaf(der, |cert| LeafInfo {
        cn: common_name(cert),
        spki: cert.public_key().raw.to_vec(),
        validity: Some(validity(cert)),
    })
    .unwrap_or_default()
}

/// The server name an existing CA in `dir` carries in its CN
/// (`ICU CA <server_name>`): `Ok(None)` without a `ca.crt`, an error when it
/// cannot be read or names no server — a CA from before the name moved into
/// its CN.
pub fn ca_server_name(dir: &Path) -> Result<Option<String>> {
    let path = dir.join(CA_CRT);
    let pem = match std::fs::read(&path) {
        Ok(pem) => pem,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::msg(format!("cannot read {}: {e}", path.display()))),
    };
    let der = read_cert_pem(&pem).context("ca.crt")?;
    let cn = with_leaf(&der, common_name).unwrap_or_default();
    match cn
        .strip_prefix(CA_COMMON_NAME)
        .and_then(|rest| rest.strip_prefix(' '))
    {
        Some(name) if !name.trim().is_empty() => Ok(Some(name.trim().to_string())),
        _ => Err(Error::msg(format!(
            "the CA in {} ({cn:?}) names no server; set server.server_name to the \
             name its server certificate carries",
            path.display()
        ))),
    }
}

fn common_name(cert: &X509Certificate<'_>) -> String {
    cert.subject()
        .iter_common_name()
        .next()
        .and_then(|a| a.as_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn validity(cert: &X509Certificate<'_>) -> (OffsetDateTime, OffsetDateTime) {
    let validity = cert.validity();
    (
        validity.not_before.to_datetime(),
        validity.not_after.to_datetime(),
    )
}

/// The raw SubjectPublicKeyInfo of a leaf, empty when it does not parse.
pub fn leaf_public_key(der: &[u8]) -> Vec<u8> {
    with_leaf(der, |cert| cert.public_key().raw.to_vec()).unwrap_or_default()
}

/// Serial number of a certificate as lowercase hex of the DER integer's
/// content octets, the form the store keeps for the CRL. Empty when it does
/// not parse.
pub fn leaf_serial(der: &[u8]) -> String {
    with_leaf(der, |cert| store::encode_hex(cert.raw_serial())).unwrap_or_default()
}

/// A certificate's notAfter as RFC 3339 UTC; far future when it does not
/// parse, so an unreadable entry stays on the CRL rather than drop off.
pub fn leaf_not_after(der: &[u8]) -> String {
    leaf_validity(der)
        .map(|(_, not_after)| store::rfc3339(not_after))
        .unwrap_or_else(|| "9999-12-31T23:59:59Z".to_string())
}

/// How long a signed client certificate is valid.
#[derive(Debug, Clone, Copy)]
pub enum Lifetime {
    /// From an hour ago for this many days: a fresh lifetime.
    Days(u32),
    /// Exactly the validity of the certificate it replaces: a re-issue that
    /// must not extend it (rename or catch-up outside the renewal window,
    /// `PROTOCOL.md` §2.2 Nr. 9).
    Keep {
        not_before: OffsetDateTime,
        not_after: OffsetDateTime,
    },
}

impl From<u32> for Lifetime {
    fn from(days: u32) -> Lifetime {
        Lifetime::Days(days)
    }
}

/// A certificate's notBefore and notAfter, `None` when it does not parse.
pub fn leaf_validity(der: &[u8]) -> Option<(OffsetDateTime, OffsetDateTime)> {
    with_leaf(der, validity)
}

/// Whole days until a certificate's notAfter, negative once it has passed.
/// `None` when it does not parse.
pub fn leaf_days_left_checked(der: &[u8]) -> Option<i64> {
    leaf_validity(der).map(|(_, not_after)| (not_after - store::now()).whole_days())
}

/// [`leaf_days_left_checked`] for reporting: `i64::MAX` when it does not
/// parse, so monitoring does not alarm on a number that means nothing.
pub fn leaf_days_left(der: &[u8]) -> i64 {
    leaf_days_left_checked(der).unwrap_or(i64::MAX)
}

/// Whether a certificate's notAfter has passed. `false` when it does not
/// parse: such a leaf never got through the handshake anyway.
pub fn leaf_expired(der: &[u8]) -> bool {
    leaf_validity(der).is_some_and(|(_, not_after)| not_after <= store::now())
}

/// How long a CRL claims to be current. Nobody enforces it (rustls does not
/// by default, and the client keeps the newest it has); the server issues a
/// fresh one on every revocation and at startup anyway.
const CRL_NEXT_UPDATE: time::Duration = time::Duration::days(30);

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
    with_leaf(der, common_name).unwrap_or_default()
}

/// Whether `leaf` carries a valid signature by `ca`'s key.
fn issued_by(leaf: &[u8], ca: &[u8]) -> bool {
    use x509_parser::prelude::FromDer;
    let (Ok((_, leaf)), Ok((_, ca))) = (
        X509Certificate::from_der(leaf),
        X509Certificate::from_der(ca),
    ) else {
        return false;
    };
    leaf.verify_signature(Some(ca.public_key())).is_ok()
}

/// How reading one of the state files went, for an error message.
fn file_state<T>(name: &str, read: &std::io::Result<T>) -> String {
    match read {
        Ok(_) => format!("{name} present"),
        Err(e) => format!("{name}: {e}"),
    }
}

fn create_ca(
    dir: &Path,
    tls: &TlsSection,
    server_name: &str,
) -> Result<(CertificateDer<'static>, String)> {
    let key = KeyPair::generate()?; // P-256 by default (Spike 2b)
    let cert = ca_params(
        &format!("{CA_COMMON_NAME} {server_name}"),
        tls.ca_validity_days,
    )?
    .self_signed(&key)?;
    let key_pem = key.serialize_pem();
    // Key first: a crash in between leaves a key without a certificate,
    // which the next start refuses with a clear message, instead of a
    // certificate whose key is gone.
    store::write_private(&dir.join(CA_KEY), &key_pem)?;
    store::write_atomic(&dir.join(CA_CRT), &cert.pem())?;
    Ok((cert.der().clone(), key_pem))
}

/// Parameters of the self-signed CA certificate: new, and re-issued under the
/// same key by [`Ca::renew_ca_cert`].
fn ca_params(common_name: &str, days: u32) -> Result<CertificateParams> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    set_validity(&mut params, days)?;
    params.serial_number = Some(random_serial());
    Ok(params)
}

/// From an hour ago (clock slack) for `days` days. Checked: an absurd
/// lifetime is an error here, not a panic in the date arithmetic.
fn set_validity(params: &mut CertificateParams, days: u32) -> Result<()> {
    let now = store::now();
    params.not_before = now - time::Duration::hours(1);
    params.not_after = now
        .checked_add(time::Duration::days(i64::from(days)))
        .ok_or_else(|| Error::msg(format!("a validity of {days} days is out of range")))?;
    Ok(())
}

/// Big-endian bytes of `n` without leading zeros (at least one byte).
fn minimal_be(n: u64) -> Vec<u8> {
    let bytes = n.to_be_bytes();
    let skip = bytes.iter().take(7).take_while(|b| **b == 0).count();
    bytes[skip..].to_vec()
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

/// The leaves `server.crt.retired` lists, none when it does not exist. Any
/// other trouble reading it is an error: what it lists must not escape the
/// CRL unnoticed.
fn read_retired(dir: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let path = dir.join(SERVER_RETIRED);
    let pem = match std::fs::read(&path) {
        Ok(pem) => pem,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(Error::msg(format!(
                "cannot read {}: {e}. It lists replaced server certificates that \
                 still have to go on the CRL; make it readable for the server",
                path.display()
            )));
        }
    };
    let mut leaves: Vec<CertificateDer<'static>> = Vec::new();
    for leaf in CertificateDer::pem_slice_iter(&pem) {
        match leaf {
            Ok(leaf) if !leaves.contains(&leaf) => leaves.push(leaf),
            Ok(_) => {}
            // Written atomically, so only a hand edit gets here.
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "unreadable entry, ignoring the rest");
                break;
            }
        }
    }
    Ok(leaves)
}

/// Writes `leaves` to `server.crt.retired` atomically, or removes the file
/// when there are none.
fn write_retired(dir: &Path, leaves: &[CertificateDer<'static>]) -> Result<()> {
    let path = dir.join(SERVER_RETIRED);
    if leaves.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(Error::from(e).context(format!("cannot remove {}", path.display())))
            }
            _ => Ok(()),
        };
    }
    let pem: String = leaves.iter().map(|leaf| cert_pem(leaf)).collect();
    store::write_atomic(&path, &pem)
}

/// A certificate as a PEM block, laid out as rcgen writes `server.crt`.
/// rcgen armors only what it signed itself, and a leaf read back from disk
/// is plain DER.
fn cert_pem(der: &[u8]) -> String {
    pem::encode_config(
        &pem::Pem::new("CERTIFICATE", der.to_vec()),
        pem::EncodeConfig::new().set_line_ending(pem::LineEnding::LF),
    )
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
        assert!(!again.server_key().secret_der().is_empty());
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
        params
            .distinguished_name
            .push(DnType::OrganizationName, "evil corp");
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
        assert_eq!(
            leaf_public_key(&leaf),
            rcgen::PublicKeyData::subject_public_key_info(&key)
        );
        assert!(ca
            .issue_registration("bad\nname", Duration::from_secs(60), HOST)
            .is_err());
    }

    #[test]
    fn csr_public_key_matches_the_signing_key() {
        let (key, csr) = csr_with_cn("alice");
        assert_eq!(
            csr_public_key(&csr).unwrap(),
            rcgen::PublicKeyData::subject_public_key_info(&key)
        );
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
        assert!(
            ca.take_retired_leaves().is_empty(),
            "a first leaf replaces none"
        );
        let ca_fp = ca.ca_fingerprint();
        let old = ca.server_der().clone();
        ca.rotate_server_cert(&tls(), "localhost").unwrap();
        assert_eq!(ca.ca_fingerprint(), ca_fp);
        assert_ne!(ca.server_der(), &old);
        assert!(!ca.server_key().secret_der().is_empty());
        assert_eq!(
            ca.take_retired_leaves(),
            vec![old],
            "the replaced leaf is retired"
        );
    }

    /// A lost or unreadable half of the CA must stop the server, never be
    /// replaced by a new CA: that would invalidate every certificate issued.
    #[test]
    fn a_missing_ca_file_is_an_error_not_a_new_ca() {
        for lost in [CA_CRT, CA_KEY] {
            let dir = tempfile::tempdir().unwrap();
            Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
            let kept = if lost == CA_CRT { CA_KEY } else { CA_CRT };
            let before = std::fs::read(dir.path().join(kept)).unwrap();
            std::fs::remove_file(dir.path().join(lost)).unwrap();

            let err = Ca::load_or_create(dir.path(), &tls(), "localhost")
                .unwrap_err()
                .to_string();
            assert!(err.contains("Refusing to create a new one"), "{err}");
            assert!(err.contains(lost), "names the missing file: {err}");
            assert_eq!(
                std::fs::read(dir.path().join(kept)).unwrap(),
                before,
                "{kept} is left alone"
            );
            assert!(!dir.path().join(lost).exists(), "nothing was created");
        }
    }

    #[test]
    fn an_unreadable_ca_key_is_an_error_not_a_new_ca() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fp = Ca::load_or_create(dir.path(), &tls(), "localhost")
            .unwrap()
            .ca_fingerprint();
        let key = dir.path().join(CA_KEY);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&key).is_ok() {
            // Root reads it anyway; nothing to test then.
            return;
        }
        let result = Ca::load_or_create(dir.path(), &tls(), "localhost");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.is_err(), "a permission problem is not a missing CA");
        let again = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert_eq!(again.ca_fingerprint(), fp, "the CA is still the old one");
    }

    #[test]
    fn a_ca_key_that_does_not_belong_to_the_certificate_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let other = tempfile::tempdir().unwrap();
        Ca::load_or_create(other.path(), &tls(), "localhost").unwrap();
        std::fs::copy(other.path().join(CA_KEY), dir.path().join(CA_KEY)).unwrap();
        let err = Ca::load_or_create(dir.path(), &tls(), "localhost")
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not belong"), "{err}");
    }

    /// A crash between the two writes of a rotation leaves a key that does
    /// not match the certificate; loading issues a new leaf instead of
    /// failing forever, and retires the one it found.
    #[test]
    fn a_server_key_that_does_not_match_is_replaced_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let old = Ca::load_or_create(dir.path(), &tls(), "localhost")
            .unwrap()
            .server_der()
            .clone();
        store::write_private(
            &dir.path().join(SERVER_KEY),
            &KeyPair::generate().unwrap().serialize_pem(),
        )
        .unwrap();

        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert_ne!(ca.server_der(), &old, "a new leaf");
        assert_eq!(ca.take_retired_leaves(), vec![old]);
        client_accepts(ca.ca_der(), &ca).expect("the new pair works");

        // Written as a pair: the next start loads it as it is.
        let again = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert_eq!(again.server_der(), ca.server_der());
    }

    #[test]
    fn a_server_leaf_from_another_ca_or_garbage_is_replaced_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let fp = Ca::load_or_create(dir.path(), &tls(), "localhost")
            .unwrap()
            .ca_fingerprint();
        let other = tempfile::tempdir().unwrap();
        let foreign = Ca::load_or_create(other.path(), &tls(), "localhost")
            .unwrap()
            .server_der()
            .clone();
        for name in [SERVER_CRT, SERVER_KEY] {
            std::fs::copy(other.path().join(name), dir.path().join(name)).unwrap();
        }
        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert_eq!(ca.ca_fingerprint(), fp);
        assert_ne!(ca.server_der(), &foreign);
        assert_eq!(ca.take_retired_leaves(), vec![foreign]);
        ca.retired_recorded().unwrap();
        client_accepts(ca.ca_der(), &ca).expect("issued by this CA again");

        std::fs::write(dir.path().join(SERVER_CRT), b"garbage").unwrap();
        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert!(
            ca.take_retired_leaves().is_empty(),
            "nothing to put on a CRL"
        );
        client_accepts(ca.ca_der(), &ca).expect("a working leaf");
    }

    /// A replaced leaf is listed on disk until it is recorded: a crash or a
    /// failed store write after the rotation must not keep it off the CRL
    /// for good. Before, it lived in memory only.
    #[test]
    fn a_replaced_leaf_stays_listed_until_it_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let old = ca.server_der().clone();
        ca.rotate_server_cert(&tls(), "localhost").unwrap();
        let new = ca.server_der().clone();
        // The process ends before the store has it.
        drop(ca);

        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert_eq!(ca.server_der(), &new, "the new pair is in use");
        assert_eq!(ca.take_retired_leaves(), vec![old.clone()]);
        // Not recorded this time either: handed back, still listed.
        ca.keep_retired_leaves(vec![old.clone()]);
        drop(ca);
        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert_eq!(ca.take_retired_leaves(), vec![old]);
        ca.retired_recorded().unwrap();
        assert!(!dir.path().join(SERVER_RETIRED).exists(), "recorded: gone");

        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert!(ca.take_retired_leaves().is_empty());
    }

    /// The same for a leaf replaced at load because it did not match its
    /// key: the new pair is on disk right away, the old leaf only listed.
    #[test]
    fn a_leaf_replaced_at_load_stays_listed_until_it_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let old = Ca::load_or_create(dir.path(), &tls(), "localhost")
            .unwrap()
            .server_der()
            .clone();
        store::write_private(
            &dir.path().join(SERVER_KEY),
            &KeyPair::generate().unwrap().serialize_pem(),
        )
        .unwrap();
        let repaired = Ca::load_or_create(dir.path(), &tls(), "localhost")
            .unwrap()
            .server_der()
            .clone();
        assert_ne!(repaired, old);

        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert_eq!(ca.server_der(), &repaired, "the repaired pair loads");
        assert_eq!(ca.take_retired_leaves(), vec![old]);
    }

    /// A crash after listing the outgoing leaf and before writing the new
    /// pair, or a rotation whose write failed, leaves the leaf in use
    /// listed. It must not be retired: on the CRL, every client would refuse
    /// the server.
    #[test]
    fn a_listed_leaf_still_in_use_is_not_retired() {
        let dir = tempfile::tempdir().unwrap();
        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let live = ca.server_der().clone();

        // A rotation whose write fails.
        let blocker = dir.path().join("server.key");
        let backup = dir.path().join("server.key.backup");
        std::fs::rename(&blocker, &backup).unwrap();
        std::fs::create_dir(&blocker).unwrap();
        assert!(ca.rotate_server_cert(&tls(), "localhost").is_err());
        std::fs::remove_dir(&blocker).unwrap();
        std::fs::rename(&backup, &blocker).unwrap();
        assert_eq!(ca.server_der(), &live);
        assert!(ca.take_retired_leaves().is_empty());
        assert!(!dir.path().join(SERVER_RETIRED).exists(), "off the list");

        // A crash right after listing it.
        write_retired(dir.path(), std::slice::from_ref(&live)).unwrap();
        let mut ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert!(
            ca.take_retired_leaves().is_empty(),
            "the leaf in use is not retired"
        );
        client_accepts(ca.ca_der(), &ca).expect("still served");
    }

    /// `server.crt.retired` is PEM like `server.crt`, readable with the
    /// usual tools.
    #[test]
    fn retired_leaves_are_written_as_pem() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join(SERVER_CRT)).unwrap();
        assert_eq!(cert_pem(ca.server_der()), on_disk, "as rcgen writes it");
        for len in 0..4 {
            let der = CertificateDer::from(vec![0xa5; len]);
            let pem = cert_pem(&der);
            assert_eq!(
                CertificateDer::from_pem_slice(pem.as_bytes()).unwrap(),
                der,
                "{pem}"
            );
        }
    }

    /// A lifetime the date arithmetic cannot represent is an error, not a
    /// panic that takes the server down on the next registration.
    #[test]
    fn an_absurd_validity_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let absurd = TlsSection {
            server_validity_days: u32::MAX,
            ..tls()
        };
        assert!(Ca::load_or_create(dir.path(), &absurd, "localhost").is_err());
        let ca = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        let (_key, csr) = csr_with_cn("alice");
        let err = ca.sign_client_csr(&csr, u32::MAX, HOST).unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
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

    /// Verifies `ca`'s server leaf the way a client does, with `root` as the
    /// only trust anchor, chain as presented.
    fn client_accepts(
        root: &CertificateDer<'static>,
        ca: &Ca,
    ) -> std::result::Result<(), rustls::Error> {
        use rustls::client::danger::ServerCertVerifier;
        use std::sync::Arc;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(root.clone()).unwrap();
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .unwrap();
        let chain = ca.server_chain();
        verifier
            .verify_server_cert(
                &chain[0],
                &chain[1..],
                &rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                &[],
                rustls::pki_types::UnixTime::now(),
            )
            .map(|_| ())
    }

    /// The trust anchor is subject and key, never the dates: a client holding
    /// an expired CA certificate still accepts the server, before and after
    /// the server re-issues its CA certificate under the same key.
    #[test]
    fn ca_renewal_keeps_subject_and_key() {
        let dir = tempfile::tempdir().unwrap();
        let expired = TlsSection {
            ca_validity_days: 0,
            ..tls()
        };
        let mut ca = Ca::load_or_create(dir.path(), &expired, "localhost").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let old = ca.ca_der().clone();
        assert!(leaf_expired(&old));
        client_accepts(&old, &ca).expect("an expired anchor still anchors");

        ca.renew_ca_cert(&tls()).unwrap();
        let new = ca.ca_der().clone();
        assert!(!leaf_expired(&new));
        assert!(leaf_days_left(&new) > 3600);
        assert_eq!(
            parse(&new).subject().as_raw(),
            parse(&old).subject().as_raw()
        );
        assert_eq!(leaf_public_key(&new), leaf_public_key(&old));
        assert_ne!(leaf_serial(&new), leaf_serial(&old));
        client_accepts(&new, &ca).expect("renewed anchor, old leaf");
        ca.rotate_server_cert(&tls(), "localhost").unwrap();
        client_accepts(&old, &ca).expect("old anchor, new leaf");
        client_accepts(&new, &ca).expect("renewed anchor, new leaf");

        // Written to disk: a restart presents the renewed one.
        let reloaded = Ca::load_or_create(dir.path(), &tls(), "localhost").unwrap();
        assert_eq!(reloaded.ca_der(), &new);
    }
}
