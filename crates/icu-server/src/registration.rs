//! Registration certificates, the only way onto the server
//! (`PROTOCOL.md` §2.3, `Server/IMPLEMENTATION.md` §6.3).
//!
//! An admin (CLI or `CreateInviteRequest`) gets a **registration bundle**: a
//! freshly generated key, a short-lived certificate on it and the CA, armored
//! as a passport. The invited client connects with it over plain `icu/1`,
//! generates its own key and sends a `RenewRequest` carrying the name it wants;
//! the server issues the real certificate and revokes the registration one in
//! the same write. The session side lives in `session.rs`.

use std::{sync::Arc, time::Duration};

use icu_proto::{error as ec, limits as pl, passport::PassportV2, validate_name, UserId};
use rand::RngCore;

use crate::{
    error::{Error, Result},
    store::{encode_hex, fingerprint, now, rfc3339, CertRecord, Store},
    Server,
};

/// A freshly issued registration.
#[derive(Debug, Clone)]
pub struct Issued {
    /// The armored bundle to hand to the invited user.
    pub bundle: String,
    /// When the registration certificate expires, RFC 3339 UTC.
    pub expires: String,
    /// The `user_id` the invited user will have.
    pub user_id: UserId,
    /// The placeholder name until they pick their own.
    pub name: String,
    /// Whether this is the first user, and therefore an admin.
    pub is_admin: bool,
}

/// Issues a registration. `label` becomes the placeholder name when given,
/// otherwise one is generated; either way the user picks their real name when
/// they register. `ttl_s` of `0` means the default; anything above
/// `limits::MAX_INVITE_TTL_S` is clamped to it.
pub async fn issue(server: &Arc<Server>, label: &str, ttl_s: u64) -> Result<Issued> {
    let ttl_s = if ttl_s == 0 { pl::INVITE_TTL_S } else { ttl_s };
    let ttl = Duration::from_secs(ttl_s.min(pl::MAX_INVITE_TTL_S));
    let public_host = server.config.public_host();

    let mut store = server.store.lock().await;
    // Also at startup, but needed here too: an expired, unused first bundle
    // would otherwise keep the store non-empty and cost the next one its
    // admin rights until a restart.
    store.purge_expired_registrations()?;
    let name = if label.is_empty() {
        placeholder(&store)
    } else {
        validate_name(label)?;
        if store.name_taken(label, None) {
            return Err(Error::coded(
                ec::NAME_TAKEN,
                format!("name {label:?} is already taken"),
            ));
        }
        label.to_string()
    };
    let (leaf, key_der, ca_der) = {
        let ca = server.ca.read().await;
        let (leaf, key_der) = ca.issue_registration(&name, ttl, &public_host)?;
        (leaf, key_der, ca.ca_der().to_vec())
    };
    let issued = now();
    let cert = CertRecord {
        fingerprint: fingerprint(&leaf),
        cn: name.clone(),
        issued: rfc3339(issued),
        expires: rfc3339(issued + ttl),
        revoked: false,
        renewed: None,
        registration: true,
        serial: Some(crate::ca::leaf_serial(&leaf)),
    };
    let expires = cert.expires.clone();
    let (user_id, is_admin) = store.add_registration(cert)?;
    drop(store);

    tracing::info!(user_id, name = %name, is_admin, %expires, "registration issued");
    let bundle = PassportV2 {
        key_der,
        leaf_der: leaf.to_vec(),
        ca_der,
    }
    .encode();
    Ok(Issued {
        bundle,
        expires,
        user_id,
        name,
        is_admin,
    })
}

/// A free placeholder name, `neu-` plus four hex digits.
fn placeholder(store: &Store) -> String {
    loop {
        let mut raw = [0u8; 2];
        rand::rng().fill_bytes(&mut raw);
        let name = format!("neu-{}", encode_hex(&raw));
        if !store.name_taken(&name, None) {
            return name;
        }
    }
}
