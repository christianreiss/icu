//! Enrollment over ALPN `icu-enroll/1` (`PROTOCOL.md` §2.3,
//! `Client/IMPLEMENTATION.md` §5).

use std::{net::SocketAddr, sync::Arc, time::Duration};

use icu_proto::{EnrollMsg, MAX_ENROLL_FRAME};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};
use sha2::Digest;

use crate::config::{Identity, ServerInfo};

use super::{read_frame, resolve, NetError};

/// A parsed invite, in one of three shapes (`PROTOCOL.md` §2.3, §2.3.1):
/// an `icu://…` link, a self-contained `ICU1-…` blob, or a bare `host[:port]`
/// for the one-time bootstrap enrollment against a fresh server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteLink {
    /// `icu://host:port/enroll?t=…&ca=…&n=…`.
    Link {
        /// Host (DNS name or IP literal).
        host: String,
        /// Port.
        port: u16,
        /// Token, 22 base64url characters.
        token: String,
        /// CA pin, base64url without padding of SHA-256(CA DER).
        ca_pin: String,
        /// Display name, carried by the link itself.
        name: String,
    },
    /// `ICU1-…`, minted by an admin via `CreateInviteRequest`.
    Blob {
        /// Host (DNS name or IP literal).
        host: String,
        /// Port.
        port: u16,
        /// The server's CA certificate, DER, carried in full.
        ca_der: Vec<u8>,
        /// The blob's body, re-encoded, relayed opaquely as
        /// `EnrollRequest.token`.
        passthrough_token: String,
        /// SNI the blob was minted for, for display only.
        server_name_hint: String,
    },
    /// Bare `host[:port]`, no token: enrollment against a store with no
    /// users yet (`ENROLL_BOOTSTRAP_CLOSED` once one exists).
    Bootstrap {
        /// Host (DNS name or IP literal).
        host: String,
        /// Port.
        port: u16,
    },
}

impl InviteLink {
    /// Parses any of the three accepted shapes. Accepts a missing port
    /// (default 4711).
    pub fn parse(link: &str) -> Result<InviteLink, NetError> {
        let link = link.trim();
        if let Some(rest) = link.strip_prefix("icu://") {
            return Self::parse_link(rest);
        }
        if link.starts_with(icu_proto::invite_blob::ARMOR_PREFIX) {
            return Self::parse_blob(link);
        }
        let (host, port) = split_host_port(link)?;
        Ok(InviteLink::Bootstrap { host, port })
    }

    fn parse_link(rest: &str) -> Result<InviteLink, NetError> {
        let (authority, path_query) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => return Err(NetError::BadLink("missing /enroll path".into())),
        };
        let (path, query) = match path_query.find('?') {
            Some(i) => (&path_query[..i], &path_query[i + 1..]),
            None => (path_query, ""),
        };
        if path != "/enroll" {
            return Err(NetError::BadLink(format!("unexpected path {path}")));
        }
        let (host, port) = split_host_port(authority)?;
        let mut token = None;
        let mut ca_pin = None;
        let mut name = None;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match k {
                "t" => token = Some(v.to_string()),
                "ca" => ca_pin = Some(v.to_string()),
                "n" => name = Some(percent_decode(v)),
                _ => {}
            }
        }
        let token = token.ok_or_else(|| NetError::BadLink("missing t=".into()))?;
        if token.len() != EnrollMsg::TOKEN_LEN {
            return Err(NetError::BadLink("token must be 22 characters".into()));
        }
        let ca_pin = ca_pin.ok_or_else(|| NetError::BadLink("missing ca=".into()))?;
        let name = name.ok_or_else(|| NetError::BadLink("missing n=".into()))?;
        icu_proto::validate_name(&name).map_err(|e| NetError::BadLink(e.to_string()))?;
        Ok(InviteLink::Link {
            host,
            port,
            token,
            ca_pin,
            name,
        })
    }

    fn parse_blob(armored: &str) -> Result<InviteLink, NetError> {
        let blob = icu_proto::invite_blob::InviteBlob::decode(armored)
            .map_err(|e| NetError::BadLink(e.to_string()))?;
        let (host, port) = split_host_port(&blob.host_port)?;
        Ok(InviteLink::Blob {
            host,
            port,
            ca_der: blob.ca_der.clone(),
            passthrough_token: blob.body_base64(),
            server_name_hint: blob.server_name,
        })
    }

    /// Host and port to dial, regardless of shape.
    pub fn host_port(&self) -> (&str, u16) {
        match self {
            InviteLink::Link { host, port, .. }
            | InviteLink::Blob { host, port, .. }
            | InviteLink::Bootstrap { host, port } => (host.as_str(), *port),
        }
    }
}

fn split_host_port(authority: &str) -> Result<(String, u16), NetError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| NetError::BadLink("bad IPv6 literal".into()))?;
        let host = rest[..end].to_string();
        let port = match rest[end + 1..].strip_prefix(':') {
            Some(p) => p
                .parse()
                .map_err(|_| NetError::BadLink("bad port".into()))?,
            None => icu_proto::limits::DEFAULT_PORT,
        };
        return Ok((host, port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => Ok((
            h.to_string(),
            p.parse()
                .map_err(|_| NetError::BadLink("bad port".into()))?,
        )),
        None => Ok((authority.to_string(), icu_proto::limits::DEFAULT_PORT)),
    }
}

/// Minimal percent-decoding for the `n=` parameter.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// How a [`PinVerifier`] decides whether to trust the CA presented during
/// enrollment.
#[derive(Debug)]
enum Pin {
    /// SHA-256 of the CA DER must match (the `icu://…?ca=` link).
    Hash(Vec<u8>),
    /// The CA DER must match byte for byte (an `ICU1-…` blob, which already
    /// carries the full certificate).
    FullDer(Vec<u8>),
    /// Accept any length-2 chain unconditionally: zero-prior
    /// trust-on-first-use, only for [`InviteLink::Bootstrap`] where no prior
    /// secret can exist yet by construction. Not a regression versus the
    /// other two modes — an `icu://` link already puts 100% of the trust on
    /// whoever handed it out; this just makes that explicit for the one
    /// unavoidable case where no such out-of-band channel exists at all.
    TrustFirst,
}

/// The pinning verifier: accepts exactly a chain of length 2 whose last
/// element matches the configured [`Pin`], and whose leaf covers the dialed
/// hostname. Still ignores validity dates, and does not check that the leaf
/// was signed by the pinned CA — the trust model here is chain-length + pin,
/// not full path validation.
#[derive(Debug)]
pub struct PinVerifier {
    pin: Pin,
    provider: Arc<CryptoProvider>,
}

impl PinVerifier {
    /// From the base64url (unpadded) SHA-256 pin of an `icu://` link.
    pub fn new(ca_pin: &str, provider: Arc<CryptoProvider>) -> Result<PinVerifier, NetError> {
        use base64::Engine;
        let pin = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(ca_pin)
            .map_err(|_| NetError::BadLink("ca= is not base64url".into()))?;
        if pin.len() != 32 {
            return Err(NetError::BadLink("ca= is not a SHA-256".into()));
        }
        Ok(PinVerifier {
            pin: Pin::Hash(pin),
            provider,
        })
    }

    /// From the full CA DER carried by an `ICU1-…` blob.
    pub fn from_ca_der(ca_der: Vec<u8>, provider: Arc<CryptoProvider>) -> PinVerifier {
        PinVerifier {
            pin: Pin::FullDer(ca_der),
            provider,
        }
    }

    /// Zero-prior trust-on-first-use, for [`InviteLink::Bootstrap`] only.
    pub fn trust_first(provider: Arc<CryptoProvider>) -> PinVerifier {
        PinVerifier {
            pin: Pin::TrustFirst,
            provider,
        }
    }
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if intermediates.len() != 1 {
            return Err(rustls::Error::General(format!(
                "expected a chain of length 2, got {}",
                intermediates.len() + 1
            )));
        }
        match &self.pin {
            Pin::Hash(expected) => {
                let hash = sha2::Sha256::digest(intermediates[0].as_ref());
                if hash.as_slice() != expected.as_slice() {
                    return Err(rustls::Error::InvalidCertificate(
                        rustls::CertificateError::UnknownIssuer,
                    ));
                }
            }
            Pin::FullDer(expected) => {
                if intermediates[0].as_ref() != expected.as_slice() {
                    return Err(rustls::Error::InvalidCertificate(
                        rustls::CertificateError::UnknownIssuer,
                    ));
                }
            }
            Pin::TrustFirst => {}
        }
        // The CA/intermediate pin is checked above; also require the leaf to
        // actually cover the host we dialed, for every pin mode including
        // `TrustFirst` — CA pinning alone says nothing about which hostname
        // the presented leaf is valid for.
        let leaf = rustls::server::ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_name(&leaf, server_name)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Runs the enrollment and returns the identity to store.
///
/// `display_name` is used as `EnrollRequest.name` for [`InviteLink::Blob`]
/// and [`InviteLink::Bootstrap`], which carry no name of their own (unlike
/// [`InviteLink::Link`], whose `n=` already fixes it — `display_name` is
/// ignored in that case). `addr_override` replaces the DNS lookup of the
/// link host (tests bind ephemeral ports).
pub async fn enroll(
    link: &InviteLink,
    display_name: &str,
    addr_override: Option<SocketAddr>,
) -> Result<Identity, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(match link {
        InviteLink::Link { ca_pin, .. } => PinVerifier::new(ca_pin, provider.clone())?,
        InviteLink::Blob { ca_der, .. } => PinVerifier::from_ca_der(ca_der.clone(), provider.clone()),
        InviteLink::Bootstrap { .. } => PinVerifier::trust_first(provider.clone()),
    });
    let (host, port) = link.host_port();
    let token = match link {
        InviteLink::Link { token, .. } => token.clone(),
        InviteLink::Blob {
            passthrough_token, ..
        } => passthrough_token.clone(),
        InviteLink::Bootstrap { .. } => String::new(),
    };
    let cn = match link {
        InviteLink::Link { name, .. } => name.clone(),
        InviteLink::Blob { .. } | InviteLink::Bootstrap { .. } => {
            icu_proto::validate_name(display_name).map_err(|e| NetError::BadLink(e.to_string()))?;
            display_name.to_string()
        }
    };

    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![icu_proto::limits::ALPN_ENROLL.to_vec()];

    let addr = match addr_override {
        Some(a) => a,
        None => resolve(host, port).await?,
    };
    let endpoint = super::bind_endpoint(addr)?;
    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|e| NetError::Tls(e.to_string()))?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(Duration::from_secs(20).try_into().unwrap()));
    config.transport_config(Arc::new(transport));

    let key = rcgen::KeyPair::generate().map_err(|e| NetError::Other(e.to_string()))?;
    let mut params = rcgen::CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn.clone());
    let csr = params
        .serialize_request(&key)
        .map_err(|e| NetError::Other(e.to_string()))?;

    let connecting = endpoint
        .connect_with(config, addr, &server_name_for(host))
        .map_err(|e| NetError::Connect(e.to_string()))?;
    let conn = tokio::time::timeout(Duration::from_secs(10), connecting)
        .await
        .map_err(|_| NetError::Connect("enrollment handshake timed out".into()))?
        .map_err(|e| NetError::Connect(e.to_string()))?;
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| NetError::Connect(e.to_string()))?;
    let request = EnrollMsg::EnrollRequest {
        token,
        csr_der: csr.der().to_vec(),
        name: cn.clone(),
    };
    send.write_all(
        &request
            .encode()
            .map_err(|e| NetError::Other(e.to_string()))?,
    )
    .await
    .map_err(|e| NetError::Connect(e.to_string()))?;
    let _ = send.finish();

    let body = tokio::time::timeout(
        Duration::from_secs(10),
        read_frame(&mut recv, MAX_ENROLL_FRAME),
    )
    .await
    .map_err(|_| NetError::Connect("enrollment response timed out".into()))?
    .map_err(|e| match conn.close_reason() {
        Some(quinn::ConnectionError::ApplicationClosed(c)) => NetError::Rejected {
            code: u64::from(c.error_code) as u32,
            detail: String::from_utf8_lossy(&c.reason).into_owned(),
        },
        _ => e,
    })?;
    let result = match EnrollMsg::decode(&body).map_err(|e| NetError::Other(e.to_string()))? {
        EnrollMsg::EnrollResponse {
            leaf_der,
            ca_der,
            server_name,
        } => Ok(Identity {
            key_der: key.serialize_der(),
            leaf_der,
            ca_der,
            server: ServerInfo {
                host: host.to_string(),
                port,
                server_name,
                name: cn.clone(),
            },
        }),
        EnrollMsg::Error { code, detail } => Err(NetError::Rejected { code, detail }),
        EnrollMsg::EnrollRequest { .. } => Err(NetError::Other("unexpected request".into())),
    };
    conn.close(quinn::VarInt::from_u32(0), b"");
    endpoint.wait_idle().await;
    result
}

/// SNI for the enrollment handshake: the link host, or `localhost` when the
/// host is not a valid server name. Load-bearing for security: the pin
/// verifier now also checks the leaf covers this name.
pub fn server_name_for(host: &str) -> String {
    if ServerName::try_from(host.to_string()).is_ok() {
        host.to_string()
    } else {
        "localhost".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_links() {
        let l = InviteLink::parse(
            "icu://icu.example.org:4711/enroll?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08&n=b%C3%B6b",
        )
        .unwrap();
        let InviteLink::Link {
            host,
            port,
            token,
            name,
            ..
        } = l
        else {
            panic!("expected Link");
        };
        assert_eq!(host, "icu.example.org");
        assert_eq!(port, 4711);
        assert_eq!(token, "k3nP8sQ2vL7xR1wY4tZ6bA");
        assert_eq!(name, "böb");

        let l =
            InviteLink::parse("icu://[::1]:5000/enroll?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=x&n=a").unwrap();
        assert_eq!(l.host_port(), ("::1", 5000));
        let l = InviteLink::parse("icu://host/enroll?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=x&n=a").unwrap();
        assert_eq!(l.host_port().1, 4711);

        assert!(InviteLink::parse("https://x/enroll?t=1&ca=2&n=3").is_err());
        assert!(InviteLink::parse("icu://x/other?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=2&n=3").is_err());
        assert!(InviteLink::parse("icu://x/enroll?t=short&ca=2&n=3").is_err());
        assert!(InviteLink::parse("icu://x/enroll?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=2").is_err());
    }

    #[test]
    fn parses_bootstrap() {
        let l = InviteLink::parse("icu.example.org:4711").unwrap();
        assert_eq!(l.host_port(), ("icu.example.org", 4711));
        assert!(matches!(l, InviteLink::Bootstrap { .. }));

        let l = InviteLink::parse("  icu.example.org  ").unwrap();
        assert_eq!(l.host_port(), ("icu.example.org", icu_proto::limits::DEFAULT_PORT));
    }

    #[test]
    fn parses_blob() {
        let blob = icu_proto::invite_blob::InviteBlob {
            host_port: "icu.example.org:4711".into(),
            server_name: "icu.example.org".into(),
            ca_der: vec![0x30, 0x82, 0x01],
            expires: "2026-09-22T00:00:00Z".into(),
            nonce: [1u8; 12],
            sealed: vec![9, 9, 9],
        };
        let armored = blob.encode();
        let l = InviteLink::parse(&armored).unwrap();
        let InviteLink::Blob {
            host,
            port,
            ca_der,
            passthrough_token,
            server_name_hint,
        } = l
        else {
            panic!("expected Blob");
        };
        assert_eq!(host, "icu.example.org");
        assert_eq!(port, 4711);
        assert_eq!(ca_der, vec![0x30, 0x82, 0x01]);
        assert_eq!(server_name_hint, "icu.example.org");
        assert_eq!(passthrough_token, blob.body_base64());

        assert!(InviteLink::parse("ICU1-not valid base64!!").is_err());
    }

    #[test]
    fn pin_must_be_32_bytes_base64url() {
        let p = Arc::new(rustls::crypto::ring::default_provider());
        assert!(PinVerifier::new("AAAA", p.clone()).is_err());
        assert!(PinVerifier::new("not base64!", p.clone()).is_err());
        let ok =
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, [7u8; 32]);
        assert!(PinVerifier::new(&ok, p).is_ok());
    }

    /// A real self-signed leaf DER covering `host`, for tests that now go
    /// through actual X.509 parsing (hostname verification).
    fn leaf_for(host: &str) -> CertificateDer<'static> {
        let rcgen::CertifiedKey { cert, .. } =
            rcgen::generate_simple_self_signed(vec![host.to_string()]).unwrap();
        CertificateDer::from(cert.der().to_vec())
    }

    #[test]
    fn from_ca_der_matches_byte_exact_only() {
        let p = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = PinVerifier::from_ca_der(vec![1, 2, 3], p);
        let ok = verifier.verify_server_cert(
            &leaf_for("localhost"),
            &[CertificateDer::from(vec![1, 2, 3])],
            &ServerName::try_from("localhost").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(ok.is_ok());
        let bad = verifier.verify_server_cert(
            &leaf_for("localhost"),
            &[CertificateDer::from(vec![9, 9, 9])],
            &ServerName::try_from("localhost").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(bad.is_err());
    }

    #[test]
    fn trust_first_accepts_any_length_2_chain_but_not_others() {
        let p = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = PinVerifier::trust_first(p);
        let name = ServerName::try_from("localhost").unwrap();
        assert!(verifier
            .verify_server_cert(
                &leaf_for("localhost"),
                &[CertificateDer::from(vec![1, 2, 3])],
                &name,
                &[],
                UnixTime::now(),
            )
            .is_ok());
        assert!(verifier
            .verify_server_cert(&leaf_for("localhost"), &[], &name, &[], UnixTime::now())
            .is_err());
    }

    #[test]
    fn hostname_mismatch_is_rejected_even_with_correct_pin() {
        let p = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = PinVerifier::from_ca_der(vec![1, 2, 3], p);
        let bad = verifier.verify_server_cert(
            &leaf_for("other.invalid"),
            &[CertificateDer::from(vec![1, 2, 3])],
            &ServerName::try_from("localhost").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(bad.is_err());
    }

    #[test]
    fn trust_first_still_requires_hostname_match() {
        let p = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = PinVerifier::trust_first(p);
        let bad = verifier.verify_server_cert(
            &leaf_for("other.invalid"),
            &[CertificateDer::from(vec![1, 2, 3])],
            &ServerName::try_from("localhost").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(bad.is_err());
    }
}
