//! Spike 2: QUIC (quinn) mit mutual TLS. Server-CA, Client-Cert, Datagram-Roundtrip,
//! Ablehnung ohne Cert, Fingerprint-Auslesen nach Handshake, Datagram-Maxgröße.
use anyhow::{anyhow, Result};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::{net::SocketAddr, sync::Arc, time::Instant};

fn ca() -> (rcgen::Certificate, rcgen::KeyPair) {
    let mut p = rcgen::CertificateParams::new(vec![]).unwrap();
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    p.distinguished_name.push(rcgen::DnType::CommonName, "ICU CA");
    let kp = rcgen::KeyPair::generate().unwrap();
    (p.self_signed(&kp).unwrap(), kp)
}
fn leaf(ca: &rcgen::Certificate, cakp: &rcgen::KeyPair, cn: &str, san: Vec<String>, client: bool) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let mut p = rcgen::CertificateParams::new(san).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, cn);
    p.extended_key_usages = vec![if client { rcgen::ExtendedKeyUsagePurpose::ClientAuth } else { rcgen::ExtendedKeyUsagePurpose::ServerAuth }];
    let kp = rcgen::KeyPair::generate().unwrap();
    let cert = p.signed_by(&kp, ca, cakp).unwrap();
    (cert.der().clone(), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(kp.serialize_der())))
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider().install_default().ok();
    let (ca_cert, ca_kp) = ca();
    let ca_der = ca_cert.der().clone();
    let (srv_cert, srv_key) = leaf(&ca_cert, &ca_kp, "icu-server", vec!["localhost".into()], false);
    let (cli_cert, cli_key) = leaf(&ca_cert, &ca_kp, "chris", vec![], true);

    // --- Server: Client-Cert PFLICHT
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.clone())?;
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots.clone())).build()?;
    let mut tls = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![srv_cert], srv_key)?;
    tls.alpn_protocols = vec![b"icu/1".to_vec()];
    let mut scfg = ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(tls)?));
    Arc::get_mut(&mut scfg.transport).unwrap().max_idle_timeout(Some(std::time::Duration::from_secs(5).try_into()?));
    let server = Endpoint::server(scfg, "127.0.0.1:0".parse()?)?;
    let saddr: SocketAddr = server.local_addr()?;

    let srv = tokio::spawn(async move {
        let mut n = 0;
        while let Some(inc) = server.accept().await {
            match inc.await {
                Ok(conn) => {
                    let id = conn.peer_identity().and_then(|i| i.downcast::<Vec<CertificateDer>>().ok());
                    let fp = id.as_ref().and_then(|c| c.first()).map(|c| {
                        use ring_like::*;
                        sha256_hex(c.as_ref())
                    });
                    let cn = id.as_ref().and_then(|c| c.first()).and_then(|c| {
                        let (_, x) = x509_parser::parse_x509_certificate(c.as_ref()).ok()?;
                        let r = x.subject().iter_common_name().next().and_then(|a| a.as_str().ok()).map(|s| s.to_string());
                        r
                    });
                    println!("[server] accepted, client CN={cn:?} fp={fp:?} max_datagram={:?}", conn.max_datagram_size());
                    // echo datagrams
                    loop {
                        match conn.read_datagram().await {
                            Ok(d) => { let _ = conn.send_datagram(d); }
                            Err(e) => { println!("[server] conn closed: {e}"); break; }
                        }
                    }
                    n += 1;
                    if n >= 1 { break; }
                }
                Err(e) => println!("[server] handshake rejected: {e}"),
            }
        }
    });

    // --- Client OHNE Cert: muss scheitern
    let client_tls_nocert = rustls::ClientConfig::builder().with_root_certificates(roots.clone()).with_no_client_auth();
    let r = connect(saddr, client_tls_nocert).await;
    match r {
        Err(e) => println!("[client-nocert] connect failed immediately: {e}"),
        Ok(c) => {
            // TLS 1.3: Client sieht Handshake als fertig, bevor der Server das fehlende Cert ablehnt.
            let t = Instant::now();
            let closed = tokio::time::timeout(std::time::Duration::from_secs(3), c.closed()).await;
            println!("[client-nocert] connect() returned Ok; closed after {:?}: {:?}", t.elapsed(), closed.as_ref().map(|e| e.to_string()));
            assert!(closed.is_ok(), "Verbindung ohne Client-Cert blieb offen!");
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // --- Client MIT Cert
    let client_tls = rustls::ClientConfig::builder().with_root_certificates(roots).with_client_auth_cert(vec![cli_cert], cli_key)?;
    let t0 = Instant::now();
    let conn = connect(saddr, client_tls).await?;
    println!("[client] handshake+connect {:?}, max_datagram={:?}, rtt={:?}", t0.elapsed(), conn.max_datagram_size(), conn.rtt());

    // Datagram-Roundtrips
    let mut rtts = vec![];
    for i in 0..200u32 {
        let payload = [&i.to_be_bytes()[..], &[0u8; 100]].concat(); // ~104 B, wie ein Opus-Frame
        let t = Instant::now();
        conn.send_datagram(payload.clone().into())?;
        let back = conn.read_datagram().await?;
        assert_eq!(&back[..], &payload[..]);
        rtts.push(t.elapsed());
    }
    rtts.sort();
    println!("[client] 200 datagram roundtrips: min={:?} p50={:?} p99={:?}", rtts[0], rtts[100], rtts[198]);

    // Maximale Datagramgröße
    let max = conn.max_datagram_size().ok_or(anyhow!("no datagrams"))?;
    conn.send_datagram(vec![7u8; max].into())?;
    let back = conn.read_datagram().await?;
    println!("[client] datagram of {} bytes echoed ok ({} bytes)", max, back.len());
    let too_big = conn.send_datagram(vec![7u8; max + 1].into());
    println!("[client] datagram of {} bytes: {:?}", max + 1, too_big.err().map(|e| e.to_string()));

    conn.close(0u32.into(), b"done");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), srv).await;
    println!("SPIKE OK");
    Ok(())
}

async fn connect(saddr: SocketAddr, mut tls: rustls::ClientConfig) -> Result<quinn::Connection> {
    tls.alpn_protocols = vec![b"icu/1".to_vec()];
    let mut ep = Endpoint::client("127.0.0.1:0".parse()?)?;
    ep.set_default_client_config(ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls)?)));
    let c = ep.connect(saddr, "localhost")?.await?;
    Ok(c)
}

mod ring_like {
    pub fn sha256_hex(b: &[u8]) -> String {
        let d = ring::digest::digest(&ring::digest::SHA256, b);
        d.as_ref().iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
    }
}
