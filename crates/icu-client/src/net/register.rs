//! Registration with a bundle (`PROTOCOL.md` §2.3, `Client/IMPLEMENTATION.md`
//! §5).
//!
//! A registration bundle is a passport whose certificate is a short-lived
//! registration certificate on a key the admin's server generated. The client
//! connects with it over plain `icu/1`, the server answers `Welcome` with
//! `registration = true`, and the client swaps it for a certificate on a key
//! of its own, carrying the name the user picked. There is no separate
//! enrollment protocol and nothing to pin: the bundle brings the CA.

use std::net::SocketAddr;

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
/// another attempt. A bundle that is not a registration (a plain passport)
/// comes back as it is, so a caller can hand either here. `addr_override`
/// replaces the DNS lookup (tests bind ephemeral ports).
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
    let endpoint = super::bind_endpoint(addr)?;
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
        Ok(bundle.clone())
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

/// `RenewRequest` for `name` on `key` and the answer, checked against both.
/// `Error{RENEW_REQUIRED}` is the server's prompt for exactly this and is
/// skipped; any other `Error` ends it.
async fn exchange(
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
        match ControlMsg::decode(&body).map_err(|e| NetError::Other(e.to_string()))? {
            ControlMsg::RenewResponse { leaf_der } => {
                let key_der = key.serialize_der();
                if let Err(detail) = crate::conninfo::check_issued_leaf(&leaf_der, name, &key_der) {
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
