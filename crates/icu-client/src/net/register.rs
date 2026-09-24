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
    icu_proto::validate_name(name).map_err(|e| NetError::BadLink(e.to_string()))?;
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
            detail: e.to_string(),
        },
        None => NetError::Connect(e.to_string()),
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
                    NetError::Other(format!(
                        "Speichern fehlgeschlagen: {e} — die Einladung bleibt gültig, bitte erneut versuchen"
                    ))
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

/// The close code that ends a registration session: `NO_ERROR` only once the
/// new identity is stored, which tells the server to revoke the bundle.
/// Anything else keeps it valid (`PROTOCOL.md` §2.3).
pub fn receipt(stored: bool) -> quinn::VarInt {
    quinn::VarInt::from_u32(if stored {
        icu_proto::error::NO_ERROR
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
    let csr_der = csr_for(name, &key)?;
    conn::write_msg(send, &ControlMsg::RenewRequest { csr_der }).await?;
    loop {
        let body = read_frame(recv, MAX_CONTROL_FRAME).await?;
        match ControlMsg::decode(&body).map_err(|e| NetError::Other(e.to_string()))? {
            ControlMsg::RenewResponse { leaf_der } => {
                let key_der = key.serialize_der();
                let cn = crate::conninfo::CertInfo::from_der(&leaf_der).subject;
                if cn != name {
                    return Err(NetError::Other(format!(
                        "Server hat ein Zertifikat für {cn:?} ausgestellt, erwartet war {name:?}"
                    )));
                }
                if !crate::conninfo::leaf_matches_key(&leaf_der, &key_der) {
                    return Err(NetError::Other(
                        "Neues Zertifikat passt nicht zum eigenen Schlüssel".into(),
                    ));
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
    icu_proto::validate_name(cn).map_err(|e| NetError::BadLink(e.to_string()))?;
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
