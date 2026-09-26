//! Registration certificates, the only way onto the server
//! (`PROTOCOL.md` §2.3, `Server/IMPLEMENTATION.md` §6.3).
//!
//! An admin (CLI or `CreateInviteRequest`) gets a **registration bundle**: a
//! freshly generated key, a short-lived certificate on it and the CA, armored
//! as a passport. The invited client connects with it over plain `icu/1`,
//! generates its own key and sends a `RenewRequest` carrying the name it wants;
//! the server issues the real certificate. The registration certificate stays
//! valid until the client confirms it stored the new one — its close with
//! `REGISTRATION_STORED`, or its first login with it — so a lost answer does
//! not burn the bundle. The session side lives in `session.rs`.

use std::{sync::Arc, time::Duration};

use icu_proto::{error as ec, limits as pl, passport::PassportV2, UserId};
use rand::RngCore;

use crate::{
    error::{Error, Result},
    store::{encode_hex, now, rfc3339, CertRecord, Store},
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
    /// Whether some admin has confirmed a registration. While none has,
    /// whoever confirms one first becomes admin (`PROTOCOL.md` §2.3).
    pub admin_confirmed: bool,
}

/// Issues a registration. `label` becomes the placeholder name when given,
/// otherwise one is generated; either way the user picks their real name when
/// they register. `ttl_s` of `0` means the default; anything above
/// `limits::MAX_INVITE_TTL_S` is clamped to it.
///
/// Refused with `RATE_LIMITED` while `Config::max_pending_invites` are open:
/// every invitation is a user in `users.toml` until it is confirmed or
/// expires, and the whole file is rewritten under the store lock on every
/// change. A malformed or taken label is `NAME_TAKEN`. The first invitation
/// of a server without users is refused while `public_host` is a name
/// reserved for examples (`Config::reserved_names`): it would be the first
/// certificate to carry it.
pub async fn issue(
    server: &Arc<Server>,
    actor: Option<&crate::admin::Actor>,
    label: &str,
    ttl_s: u64,
) -> Result<Issued> {
    let ttl_s = if ttl_s == 0 { pl::INVITE_TTL_S } else { ttl_s };
    let ttl = Duration::from_secs(ttl_s.min(pl::MAX_INVITE_TTL_S));
    let public_host = server.config.public_host();

    let mut store = server.store.lock().await;
    // The in-band caller checked this before its rate limit, under another
    // lock; the actor may have been demoted, disabled or revoked since.
    if actor.is_some_and(|a| !a.is_admin(&store)) {
        return Err(Error::coded(ec::NOT_ADMIN, "not an admin"));
    }
    // Also at startup, but needed here too: an expired, unused first bundle
    // would otherwise keep the store non-empty and cost the next one its
    // admin rights until a restart.
    store.purge_expired_registrations()?;
    if store.is_empty() {
        if let Some(reserved) = server.config.reserved_public_host() {
            return Err(Error::msg(reserved));
        }
    }
    // A revoked invitation is no longer open: it stays in the store until it
    // expires, but must not hold a place until then.
    let pending = store
        .users()
        .filter(|(_, u)| u.is_pending() && !u.is_revoked())
        .count();
    let cap = server.config.max_pending_invites();
    if pending >= cap {
        return Err(Error::coded(
            ec::RATE_LIMITED,
            format!(
                "{pending} invitations are open, at most {cap}: wait until one is \
                 used or expires, or revoke one"
            ),
        ));
    }
    let name = if label.is_empty() {
        placeholder(&store)?
    } else {
        store.claim_name(label, None).map_err(|e| match e.code() {
            Some(_) => e,
            None => Error::coded(ec::NAME_TAKEN, e),
        })?;
        label.to_string()
    };
    let (leaf, key_der, ca_der) = {
        let ca = server.ca.read().await;
        let (leaf, key_der) = ca.issue_registration(&name, ttl, &public_host)?;
        (leaf, key_der, ca.ca_der().to_vec())
    };
    let cert = CertRecord::issued(&leaf, name.clone(), rfc3339(now()), true);
    let expires = cert.expires.clone();
    let (user_id, is_admin) = store.add_registration(cert)?;
    let admin_confirmed = store.has_confirmed_admin();
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
        admin_confirmed,
    })
}

/// Random draws per length before [`placeholder`] tries a longer suffix.
const PLACEHOLDER_TRIES: usize = 64;

/// A free placeholder name, `neu-` plus four hex digits, or eight once four
/// are crowded. Bounded: it runs under the store lock, and a store full of
/// pending invites must not hang the server.
fn placeholder(store: &Store) -> Result<String> {
    for len in [2usize, 4] {
        for _ in 0..PLACEHOLDER_TRIES {
            let mut raw = [0u8; 4];
            rand::rng().fill_bytes(&mut raw[..len]);
            let name = format!("neu-{}", encode_hex(&raw[..len]));
            if !store.name_taken(&name, None) {
                return Ok(name);
            }
        }
    }
    Err(Error::coded(
        ec::NAME_TAKEN,
        "no free placeholder name, pass one explicitly",
    ))
}
