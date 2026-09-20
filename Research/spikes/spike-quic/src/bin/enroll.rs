//! Spike 2b: Enrollment-Fragen aus auth-enrollment.md / wire.md
//! (1) Eine ServerConfig mit allow_unauthenticated: peer_identity() ohne Client-Cert = None oder Some(leer)?
//! (2) ALPN vor accept-Abschluss lesbar (Connecting::handshake_data)?
//! (3) rcgen 0.14: CSR vom Client, Server signiert und überschreibt Subject/EKU/Validity?
//! (4) postcard: unbekannte Enum-Variante -> Err, kein Panic. Cert-DER-Größen.
use anyhow::Result;
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::{net::SocketAddr, sync::Arc};

#[derive(serde::Serialize, serde::Deserialize, Debug)] enum V1 { Hello { minor: u16 }, Ping(u64) }
#[derive(serde::Serialize, serde::Deserialize, Debug)] enum V2 { Hello { minor: u16 }, Ping(u64), NewThing { x: u32 } }

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider().install_default().ok();
    // (4) postcard
    let bytes = postcard::to_allocvec(&V2::NewThing { x: 7 })?;
    let r: Result<V1, _> = postcard::from_bytes(&bytes);
    println!("[postcard] V1 decode of unknown V2 variant -> {:?}", r.map(|_| ()).map_err(|e| e.to_string()));
    let bytes = postcard::to_allocvec(&V2::Hello { minor: 3 })?;
    println!("[postcard] V1 decode of shared variant -> {:?}, Hello{{minor:3}} = {} bytes", postcard::from_bytes::<V1>(&bytes)?, bytes.len());

    // CA
    let ca_kp = rcgen::KeyPair::generate()?;
    let mut cap = rcgen::CertificateParams::new(vec![])?;
    cap.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    cap.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
    cap.distinguished_name.push(rcgen::DnType::CommonName, "ICU CA");
    let ca_cert = cap.self_signed(&ca_kp)?;
    let issuer = rcgen::Issuer::from_params(&cap, &ca_kp);
    // Server leaf
    let srv_kp = rcgen::KeyPair::generate()?;
    let mut sp = rcgen::CertificateParams::new(vec!["localhost".to_string()])?;
    sp.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let srv_cert = sp.signed_by(&srv_kp, &issuer)?;
    println!("[rcgen] CA DER {} B, server leaf DER {} B", ca_cert.der().len(), srv_cert.der().len());

    // (3) Client: CSR mit frechem CN, Server überschreibt
    let cli_kp = rcgen::KeyPair::generate()?;
    let mut csrp = rcgen::CertificateParams::new(vec![])?;
    csrp.distinguished_name.push(rcgen::DnType::CommonName, "admin-impersonator");
    let csr = csrp.serialize_request(&cli_kp)?;
    println!("[rcgen] CSR DER {} B", csr.der().len());
    let mut sreq = rcgen::CertificateSigningRequestParams::from_der(csr.der())?;
    sreq.params.distinguished_name = rcgen::DistinguishedName::new();
    sreq.params.distinguished_name.push(rcgen::DnType::CommonName, "alice");
    sreq.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    sreq.params.not_before = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    sreq.params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(365);
    let cli_cert = sreq.signed_by(&issuer)?;
    let (_, x) = x509_parser::parse_x509_certificate(cli_cert.der())?;
    let cn = x.subject().iter_common_name().next().and_then(|a| a.as_str().ok()).map(|s| s.to_string());
    println!("[rcgen] signed client leaf DER {} B, CN={:?} (Server-Override wirkt: {}), issuer={}", cli_cert.der().len(), cn, cn.as_deref() == Some("alice"), x.issuer());

    // (1)+(2) one ServerConfig, allow_unauthenticated, two ALPNs
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_cert.der().clone())?;
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots.clone())).allow_unauthenticated().build()?;
    let mut tls = rustls::ServerConfig::builder().with_client_cert_verifier(verifier)
        .with_single_cert(vec![srv_cert.der().clone(), ca_cert.der().clone()], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(srv_kp.serialize_der())))?;
    tls.alpn_protocols = vec![b"icu/1".to_vec(), b"icu-enroll/1".to_vec()];
    let scfg = ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(tls)?));
    let server = Endpoint::server(scfg, "127.0.0.1:0".parse()?)?;
    let saddr: SocketAddr = server.local_addr()?;
    let srv = tokio::spawn(async move {
        for _ in 0..2 {
            let inc = server.accept().await.unwrap();
            let mut connecting = inc.accept().unwrap();
            let hd = connecting.handshake_data().await.unwrap();
            let alpn = hd.downcast::<quinn::crypto::rustls::HandshakeData>().ok().and_then(|h| h.protocol).map(|p| String::from_utf8_lossy(&p).to_string());
            let conn = connecting.await.unwrap();
            let id = conn.peer_identity();
            let n = id.as_ref().and_then(|i| i.downcast_ref::<Vec<CertificateDer>>()).map(|v| v.len());
            println!("[server] ALPN(before handshake done)={alpn:?}  peer_identity: is_some={} certs={n:?}", id.is_some());
            conn.close(0u32.into(), b"");
        }
    });
    for (alpn, with_cert) in [("icu-enroll/1", false), ("icu/1", true)] {
        let b = rustls::ClientConfig::builder().with_root_certificates(roots.clone());
        let mut tls = if with_cert { b.with_client_auth_cert(vec![cli_cert.der().clone()], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cli_kp.serialize_der())))? } else { b.with_no_client_auth() };
        tls.alpn_protocols = vec![alpn.as_bytes().to_vec()];
        let mut ep = Endpoint::client("127.0.0.1:0".parse()?)?;
        ep.set_default_client_config(ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls)?)));
        let c = ep.connect(saddr, "localhost")?.await?;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), c.closed()).await;
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), srv).await;
    println!("SPIKE 2b OK");
    Ok(())
}
