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

/// A parsed `icu://host:port/enroll?t=…&ca=…&n=…` link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteLink {
    /// Host (DNS name or IP literal).
    pub host: String,
    /// Port.
    pub port: u16,
    /// Token, 22 base64url characters.
    pub token: String,
    /// CA pin, base64url without padding of SHA-256(CA DER).
    pub ca_pin: String,
    /// Display name.
    pub name: String,
}

impl InviteLink {
    /// Parses a link. Accepts a missing port (default 4711).
    pub fn parse(link: &str) -> Result<InviteLink, NetError> {
        let rest = link
            .trim()
            .strip_prefix("icu://")
            .ok_or_else(|| NetError::BadLink("missing icu:// scheme".into()))?;
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
        Ok(InviteLink {
            host,
            port,
            token,
            ca_pin,
            name,
        })
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

/// The pinning verifier: accepts exactly a chain of length 2 whose last
/// element hashes to the pin. Ignores SNI and validity.
#[derive(Debug)]
pub struct PinVerifier {
    pin: Vec<u8>,
    provider: Arc<CryptoProvider>,
}

impl PinVerifier {
    /// From the base64url (unpadded) pin of the link.
    pub fn new(ca_pin: &str, provider: Arc<CryptoProvider>) -> Result<PinVerifier, NetError> {
        use base64::Engine;
        let pin = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(ca_pin)
            .map_err(|_| NetError::BadLink("ca= is not base64url".into()))?;
        if pin.len() != 32 {
            return Err(NetError::BadLink("ca= is not a SHA-256".into()));
        }
        Ok(PinVerifier { pin, provider })
    }
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if intermediates.len() != 1 {
            return Err(rustls::Error::General(format!(
                "expected a chain of length 2, got {}",
                intermediates.len() + 1
            )));
        }
        let hash = sha2::Sha256::digest(intermediates[0].as_ref());
        if hash.as_slice() != self.pin.as_slice() {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ));
        }
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
/// `addr_override` replaces the DNS lookup of the link host (tests bind
/// ephemeral ports).
pub async fn enroll(
    link: &InviteLink,
    addr_override: Option<SocketAddr>,
) -> Result<Identity, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinVerifier::new(&link.ca_pin, provider.clone())?);
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![icu_proto::limits::ALPN_ENROLL.to_vec()];

    let addr = match addr_override {
        Some(a) => a,
        None => resolve(&link.host, link.port).await?,
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
        .push(rcgen::DnType::CommonName, link.name.clone());
    let csr = params
        .serialize_request(&key)
        .map_err(|e| NetError::Other(e.to_string()))?;

    let connecting = endpoint
        .connect_with(config, addr, &server_name_for(&link.host))
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
        token: link.token.clone(),
        csr_der: csr.der().to_vec(),
        name: link.name.clone(),
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
                host: link.host.clone(),
                port: link.port,
                server_name,
                name: link.name.clone(),
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
/// host is not a valid server name (the pin verifier ignores it anyway).
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
        assert_eq!(l.host, "icu.example.org");
        assert_eq!(l.port, 4711);
        assert_eq!(l.token, "k3nP8sQ2vL7xR1wY4tZ6bA");
        assert_eq!(l.name, "böb");

        let l =
            InviteLink::parse("icu://[::1]:5000/enroll?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=x&n=a").unwrap();
        assert_eq!((l.host.as_str(), l.port), ("::1", 5000));
        let l = InviteLink::parse("icu://host/enroll?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=x&n=a").unwrap();
        assert_eq!(l.port, 4711);

        assert!(InviteLink::parse("https://x/enroll?t=1&ca=2&n=3").is_err());
        assert!(InviteLink::parse("icu://x/other?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=2&n=3").is_err());
        assert!(InviteLink::parse("icu://x/enroll?t=short&ca=2&n=3").is_err());
        assert!(InviteLink::parse("icu://x/enroll?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=2").is_err());
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
}
