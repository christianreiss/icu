//! Registration with a bundle (`PROTOCOL.md` §2.3, `Client/IMPLEMENTATION.md`
//! §5).
//!
//! A registration bundle is a passport whose certificate is a short-lived
//! registration certificate on a key the admin's server generated. The client
//! connects with it over plain `icu/1`, the server answers `Welcome` with
//! `registration = true`, and the client swaps it for a certificate on a key
//! of its own, carrying the name the user picked. There is no separate
//! enrollment protocol and nothing to pin: the bundle brings the CA.

use std::{net::SocketAddr, time::Duration};

use icu_proto::{ControlMsg, MAX_CONTROL_FRAME};

use crate::config::{Identity, Paths, ServerInfo};

use super::{conn, read_frame, resolve, server_name_for, NetError};

pub use icu_proto::limits::REGISTRATION_OU;

/// Whether `leaf_der` is a registration certificate.
pub fn is_registration(leaf_der: &[u8]) -> bool {
    let Ok((_, cert)) = x509_parser::parse_x509_certificate(leaf_der) else {
        return false;
    };
    let found = cert
        .subject()
        .iter_organizational_unit()
        .any(|ou| ou.as_str() == Ok(REGISTRATION_OU));
    found
}

/// Connects with the registration certificate in `bundle`, stores the
/// identity on a fresh key, carrying `name`, under `paths` and returns it.
///
/// Stored before the session closes, because the clean close is the receipt
/// that makes the server revoke the bundle (`PROTOCOL.md` §2.3): when saving
/// fails, the session ends with another code and the bundle stays usable for
/// another attempt. A server that does not take the bundle for a
/// registration (`Welcome.registration` false) gets no receipt and the
/// caller an error: nothing was registered or stored (until 2026-09-27 the
/// bundle came back as if it had been, and the CLI said „identity stored“).
/// `addr_override` replaces the DNS lookup (tests bind ephemeral ports).
pub async fn register(
    bundle: &Identity,
    name: &str,
    client: &str,
    addr_override: Option<SocketAddr>,
    paths: &Paths,
) -> Result<Identity, NetError> {
    let name = &icu_proto::normalize_name(name);
    icu_proto::validate_name(name).map_err(NetError::Name)?;
    let (config, cert_failure) = conn::client_config(bundle)?;
    let addr = match addr_override {
        Some(a) => a,
        None => resolve(&bundle.server.host, bundle.server.port).await?,
    };
    let endpoint = super::bind_endpoint(addr, &bundle.com_key)?;
    let session = conn::connect(
        &endpoint,
        config,
        addr,
        &server_name_for(&bundle.server.server_name),
        client,
        icu_proto::UserFlags::NONE,
        name,
        &cert_failure,
    )
    .await
    .map_err(|e| match e.code() {
        Some(code) => NetError::Rejected {
            code,
            detail: match &e {
                conn::ConnectError::Closed { reason, .. } => reason.clone(),
                other => other.to_string(),
            },
        },
        None => NetError::Handshake(e),
    })?;
    let conn::Session {
        conn: connection,
        mut send,
        mut recv,
        welcome,
    } = session;
    let result = if welcome.registration {
        complete(&mut send, &mut recv, bundle, name)
            .await
            .and_then(|identity| {
                identity.save(paths).map_err(|e| {
                    let mut p = crate::problem::storage("Die Anmeldung", &e);
                    p.text
                        .push_str(" Die Einladung bleibt gültig – bitte erneut versuchen.");
                    NetError::User(p)
                })?;
                Ok(identity)
            })
    } else {
        Err(NetError::User(crate::problem::Problem::new(
            "Der Server hat die Einladung nicht als Anmeldung angenommen. Bitte beim Admin eine \
             neue anfordern.",
            "Welcome.registration = false",
        )))
    };
    connection.close(receipt(result.is_ok()), b"");
    endpoint.wait_idle().await;
    result
}

/// The close code that ends a registration session: `REGISTRATION_STORED`
/// only once the new identity is stored, which tells the server to revoke the
/// bundle. Anything else keeps it valid, including the code 0 quinn closes
/// with when a connection is merely dropped (`PROTOCOL.md` §2.3).
pub fn receipt(stored: bool) -> quinn::VarInt {
    quinn::VarInt::from_u32(if stored {
        icu_proto::error::REGISTRATION_STORED
    } else {
        icu_proto::error::SHUTDOWN
    })
}

/// The registration step on an already welcomed session: fresh key, CSR for
/// `name`, `RenewRequest`, and the answer. Shared by [`register`] and by the
/// engine, which lands here when a registration bundle was imported into a
/// running client.
pub async fn complete(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    bundle: &Identity,
    name: &str,
) -> Result<Identity, NetError> {
    let key = rcgen::KeyPair::generate().map_err(|e| NetError::Other(e.to_string()))?;
    exchange(send, recv, bundle, &key, name).await
}

/// Renews an **expired** certificate on a renew-only session
/// (`PROTOCOL.md` §2.3): the same key, the stored `name`, a new leaf. The
/// handshake with the expired leaf proved the key; the server was checked
/// against the CA as always. The key is kept so passports exported from this
/// device keep matching.
pub async fn renew_expired(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    identity: &Identity,
    name: &str,
) -> Result<Identity, NetError> {
    let key = identity.key_pair().map_err(NetError::User)?;
    exchange(send, recv, identity, &key, name).await
}

/// How long [`exchange`] waits for the server's answer, writing included:
/// well inside the 120 s a registration session may stay open
/// (`PROTOCOL.md` §2.3), and a server that sends something else or nothing
/// at all must not leave the first-start window busy for good.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// `RenewRequest` for `name` on `key` and the answer, checked against both,
/// within [`EXCHANGE_TIMEOUT`]. `Error{RENEW_REQUIRED}` is the server's
/// prompt for exactly this and is skipped; any other `Error` ends it. Out
/// of time it ends as a broken connection, which the engine retries.
async fn exchange(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    bundle: &Identity,
    key: &rcgen::KeyPair,
    name: &str,
) -> Result<Identity, NetError> {
    exchange_within(send, recv, bundle, key, name, EXCHANGE_TIMEOUT).await
}

/// [`exchange`] with the deadline `limit`.
async fn exchange_within(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    bundle: &Identity,
    key: &rcgen::KeyPair,
    name: &str,
    limit: Duration,
) -> Result<Identity, NetError> {
    tokio::time::timeout(limit, answer(send, recv, bundle, key, name))
        .await
        .unwrap_or_else(|_| {
            Err(NetError::Connect(format!(
                "no RenewResponse within {} s",
                limit.as_secs_f32()
            )))
        })
}

/// The body of [`exchange`], without the deadline.
async fn answer(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    bundle: &Identity,
    key: &rcgen::KeyPair,
    name: &str,
) -> Result<Identity, NetError> {
    let csr_der = csr_for(name, key)?;
    conn::write_msg(send, &ControlMsg::RenewRequest { csr_der }).await?;
    loop {
        let body = read_frame(recv, MAX_CONTROL_FRAME).await?;
        match ControlMsg::decode(&body).map_err(|e| NetError::Protocol(e.to_string()))? {
            ControlMsg::RenewResponse { leaf_der } => {
                let key_der = key.serialize_der();
                if let Err(detail) =
                    crate::conninfo::check_issued_leaf(&leaf_der, name, &key_der, &bundle.ca_der)
                {
                    return Err(NetError::User(crate::problem::Problem::new(
                        "Der Server hat die Anmeldung falsch beantwortet. Bitte noch einmal \
                         versuchen.",
                        detail,
                    )));
                }
                let server =
                    ServerInfo::from_certs(&leaf_der, &bundle.ca_der).unwrap_or_else(|| {
                        ServerInfo {
                            name: name.to_string(),
                            ..bundle.server.clone()
                        }
                    });
                return Ok(Identity {
                    key_der,
                    leaf_der,
                    ca_der: bundle.ca_der.clone(),
                    com_key: bundle.com_key.clone(),
                    server,
                });
            }
            ControlMsg::Error { code, .. } if code == icu_proto::error::RENEW_REQUIRED => {}
            ControlMsg::Error { code, detail } => {
                return Err(NetError::Rejected { code, detail });
            }
            // Nothing else is sent to a registration session; tolerate it
            // rather than fail on a harmless extra.
            _ => {}
        }
    }
}

/// A PKCS#10 CSR asking for `cn`, signed by `key`.
///
/// The one place a name goes into a certificate request: registration and
/// renewal must not drift apart, because the server reads the CN as the
/// display name (`PROTOCOL.md` §2.3).
pub fn csr_for(cn: &str, key: &rcgen::KeyPair) -> Result<Vec<u8>, NetError> {
    // Only the rules every issued name met: renewals carry a stored name that
    // may predate the strict ones. New names are checked by the callers.
    icu_proto::validate_stored_name(cn).map_err(|e| NetError::BadLink(e.to_string()))?;
    let mut params = rcgen::CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn);
    Ok(params
        .serialize_request(key)
        .map_err(|e| NetError::Other(e.to_string()))?
        .der()
        .to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use x509_parser::prelude::FromDer;

    fn leaf(ou: Option<&str>) -> Vec<u8> {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "neu-0001");
        if let Some(ou) = ou {
            params
                .distinguished_name
                .push(rcgen::DnType::OrganizationalUnitName, ou);
        }
        params.self_signed(&key).unwrap().der().to_vec()
    }

    #[test]
    fn registration_certificates_are_recognised_by_their_ou() {
        assert!(is_registration(&leaf(Some(REGISTRATION_OU))));
        assert!(!is_registration(&leaf(None)));
        assert!(!is_registration(&leaf(Some("something else"))));
        assert!(!is_registration(b"junk"));
    }

    /// A server that takes the `RenewRequest` and never answers ends the
    /// exchange after the deadline, as a broken connection the engine
    /// retries (review 2026-09-27, C-8) — not a busy window for good.
    #[tokio::test]
    async fn a_server_that_never_answers_ends_the_exchange() {
        use std::sync::Arc;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let server_tls = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .unwrap();
        let server = quinn::Endpoint::server(
            quinn::ServerConfig::with_crypto(Arc::new(
                quinn::crypto::rustls::QuicServerConfig::try_from(server_tls).unwrap(),
            )),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let addr = server.local_addr().unwrap();
        // Reads whatever comes and never writes.
        let silent = tokio::spawn(async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            let (_send, mut recv) = conn.accept_bi().await.unwrap();
            let _ = recv.read_to_end(1 << 16).await;
            server
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.der().clone()).unwrap();
        let client_tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let conn = client
            .connect_with(
                quinn::ClientConfig::new(Arc::new(
                    quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap(),
                )),
                addr,
                "localhost",
            )
            .unwrap()
            .await
            .unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();

        let bundle = Identity {
            key_der: key.serialize_der(),
            leaf_der: cert.der().to_vec(),
            ca_der: cert.der().to_vec(),
            com_key: icu_proto::com::ComKey::from_bytes([1; icu_proto::com::COM_KEY_LEN]),
            server: ServerInfo {
                host: "localhost".into(),
                port: addr.port(),
                server_name: "localhost".into(),
                name: "neu-0001".into(),
            },
        };
        let own = rcgen::KeyPair::generate().unwrap();
        let started = std::time::Instant::now();
        let err = exchange_within(
            &mut send,
            &mut recv,
            &bundle,
            &own,
            "alice",
            Duration::from_millis(300),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, NetError::Connect(_)), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        conn.close(0u32.into(), b"");
        drop(silent.await);
    }

    #[test]
    fn csr_carries_the_name() {
        let key = rcgen::KeyPair::generate().unwrap();
        let csr = csr_for("alice", &key).unwrap();
        let (_, parsed) =
            x509_parser::certification_request::X509CertificationRequest::from_der(&csr).unwrap();
        let cn = parsed
            .certification_request_info
            .subject
            .iter_common_name()
            .next()
            .unwrap();
        assert_eq!(cn.as_str().unwrap(), "alice");
        assert!(csr_for("", &key).is_err());
    }
}
