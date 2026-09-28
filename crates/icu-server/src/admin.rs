//! Admin socket: `/run/icu/admin.sock`, one JSON request line, one JSON
//! response line, then close (`Server/IMPLEMENTATION.md` §5). A request line
//! is at most [`MAX_REQUEST_LINE`], at most [`MAX_CONNECTIONS`] are served at
//! once, and `timing.admin_timeout_ms` bounds the wait for the request and
//! for the client to take the answer.
//!
//! A running server is the only writer of `users.toml`, so
//! every CLI command except `run` goes through here. Authentication is the
//! file permissions of the socket, nothing else.

use std::{path::PathBuf, sync::Arc};

use icu_proto::{error as ec, ControlMsg, UserId};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    task::AbortHandle,
};

use crate::{
    error::{Context, Error, Result},
    room::Counters,
    store::{parse_rfc3339, unix_millis, Store},
    Server,
};

/// One admin command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum AdminRequest {
    /// Issue a registration bundle (`PROTOCOL.md` §2.3).
    Invite {
        /// Placeholder name until the invited user picks their own. Empty or
        /// absent means a generated one.
        #[serde(default)]
        name: String,
        /// Lifetime in seconds. Defaults to 24 h, at most 7 days.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_s: Option<u64>,
    },
    /// List all users.
    Users,
    /// Revoke every certificate of a user and kick it.
    Revoke {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Grant admin rights.
    Promote {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Revoke admin rights.
    Demote {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Reversibly block login without touching certificates.
    Disable {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Restore login after `Disable`.
    Enable {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Close a user's live connections; they may connect again.
    Kick {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Enforce mute: the user's microphone stays off until `unmute`.
    Mute {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Lift an enforced mute.
    Unmute {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Enforce deafen: the user receives no audio until `undeafen`.
    Deafen {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Lift an enforced deafen.
    Undeafen {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous ([`Store::resolve`]).
        user: String,
    },
    /// Uptime, online count and relay counters.
    Status,
    /// Issue a fresh server leaf from the same CA.
    RotateServerCert,
    /// Re-issue the CA certificate under the same key with a fresh validity.
    RenewCa,
    /// Take a snapshot of the state now (`crate::backup`).
    Backup,
}

/// The answer to an [`AdminRequest`].
#[derive(Debug, Clone, PartialEq)]
pub enum AdminResponse {
    /// `invite`
    Invite {
        /// The armored registration bundle.
        bundle: String,
        /// Expiry, RFC 3339 UTC.
        expires: String,
        /// The invited user's `user_id`.
        user_id: UserId,
        /// Their placeholder name.
        name: String,
        /// Whether they are marked admin (the first bundle of an empty
        /// server, provisionally).
        is_admin: bool,
        /// Whether some admin has confirmed a registration; while none has,
        /// whoever confirms one first becomes admin.
        admin_confirmed: bool,
    },
    /// `users`
    Users(Vec<AdminUser>),
    /// `revoke`
    Revoke {
        /// The affected user.
        user_id: UserId,
        /// Their name, so the CLI can say whom the selector hit.
        name: String,
        /// How many live connections were closed.
        closed: usize,
    },
    /// `rotate-server-cert`, `renew-ca`
    Ok,
    /// `promote`, `demote`, `enable`, `mute`, `unmute`, `deafen`, `undeafen`
    User {
        /// The affected user.
        user_id: UserId,
        /// Their name, so the CLI can say whom the selector hit.
        name: String,
    },
    /// `kick`
    Kick {
        /// The affected user.
        user_id: UserId,
        /// Their name, so the CLI can say whom the selector hit.
        name: String,
        /// How many live connections were closed.
        closed: usize,
    },
    /// `disable`
    Disable {
        /// The affected user.
        user_id: UserId,
        /// Their name, so the CLI can say whom the selector hit.
        name: String,
        /// How many live connections were closed.
        closed: usize,
    },
    /// `status`
    Status(serde_json::Value),
    /// `backup`: where the snapshot went. Only the path: the socket's group
    /// may run the server, but not read its keys.
    Backup {
        /// The snapshot directory.
        path: String,
    },
    /// Any failure.
    Error {
        /// Short form, e.g. `NAME_TAKEN`.
        error: String,
        /// Human readable detail.
        detail: String,
    },
}

/// One row of the `users` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminUser {
    /// Stable identity.
    pub user_id: UserId,
    /// Display name.
    pub name: String,
    /// Fingerprint of the newest certificate, shortened to 12 characters.
    pub fingerprint: String,
    /// When the newest certificate was issued.
    pub issued: String,
    /// When the newest certificate expires.
    pub expires: String,
    /// Whether every certificate is revoked.
    pub revoked: bool,
    /// Whether the user is connected right now.
    pub online: bool,
    /// Whether the user may send `CreateInviteRequest`.
    pub is_admin: bool,
    /// Reversible login block, independent of `revoked`.
    pub disabled: bool,
    /// Invited, registration not confirmed yet.
    #[serde(default)]
    pub pending: bool,
    /// Mute enforced by an admin.
    #[serde(default)]
    pub muted: bool,
    /// Deafen enforced by an admin.
    #[serde(default)]
    pub deafened: bool,
}

impl AdminResponse {
    /// The wire form: `{"ok":true,…}` or `{"ok":false,"error":…}` (§5).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            AdminResponse::Invite {
                bundle,
                expires,
                user_id,
                name,
                is_admin,
                admin_confirmed,
            } => json!({
                "ok": true,
                "bundle": bundle,
                "expires": expires,
                "user_id": user_id,
                "name": name,
                "is_admin": is_admin,
                "admin_confirmed": admin_confirmed,
            }),
            AdminResponse::Users(users) => json!({"ok": true, "users": users}),
            AdminResponse::Ok => json!({"ok": true}),
            AdminResponse::User { user_id, name } => {
                json!({"ok": true, "user_id": user_id, "name": name})
            }
            AdminResponse::Revoke {
                user_id,
                name,
                closed,
            }
            | AdminResponse::Disable {
                user_id,
                name,
                closed,
            }
            | AdminResponse::Kick {
                user_id,
                name,
                closed,
            } => {
                json!({"ok": true, "user_id": user_id, "name": name, "closed": closed})
            }
            AdminResponse::Status(value) => {
                let mut out = json!({"ok": true});
                if let (Some(out), Some(value)) = (out.as_object_mut(), value.as_object()) {
                    for (k, v) in value {
                        out.insert(k.clone(), v.clone());
                    }
                }
                out
            }
            AdminResponse::Backup { path } => json!({"ok": true, "path": path}),
            AdminResponse::Error { error, detail } => {
                json!({"ok": false, "error": error, "detail": detail})
            }
        }
    }

    fn err(error: &str, detail: impl std::fmt::Display) -> AdminResponse {
        AdminResponse::Error {
            error: error.to_string(),
            detail: detail.to_string(),
        }
    }
}

/// Runs one admin command. Used by the socket and by [`crate::Handle::admin`].
pub async fn handle(server: &Arc<Server>, request: AdminRequest) -> AdminResponse {
    match &request {
        AdminRequest::Invite { name, ttl_s } => {
            // `issue` owns the default and the clamp; absent or 0 means default.
            match crate::registration::issue(server, None, name, ttl_s.unwrap_or(0)).await {
                Ok(issued) => AdminResponse::Invite {
                    bundle: issued.bundle,
                    expires: issued.expires,
                    user_id: issued.user_id,
                    name: issued.name,
                    is_admin: issued.is_admin,
                    admin_confirmed: issued.admin_confirmed,
                },
                Err(e) => store_err(e, "INVITE_FAILED"),
            }
        }
        AdminRequest::Users => {
            let store = server.store.lock().await;
            let online = server.room.online_users();
            let mut users = Vec::new();
            for (user_id, record) in store.users() {
                let newest = record
                    .certs
                    .iter()
                    .max_by_key(|c| parse_rfc3339(&c.issued).ok());
                users.push(AdminUser {
                    user_id,
                    name: record.name.clone(),
                    fingerprint: newest
                        .map(|c| c.fingerprint.chars().take(12).collect())
                        .unwrap_or_default(),
                    issued: newest.map(|c| c.issued.clone()).unwrap_or_default(),
                    expires: newest.map(|c| c.expires.clone()).unwrap_or_default(),
                    revoked: record.is_revoked(),
                    online: online.contains(&user_id),
                    is_admin: record.is_admin,
                    disabled: record.disabled,
                    pending: record.is_pending(),
                    muted: record.server_muted,
                    deafened: record.server_deafened,
                });
            }
            AdminResponse::Users(users)
        }
        AdminRequest::Revoke { user } => match resolve(server, user).await {
            Ok((user_id, name)) => match revoke_user(server, None, user_id).await {
                Ok(closed) => AdminResponse::Revoke {
                    user_id,
                    name,
                    closed,
                },
                Err(e) => store_err(e, "REVOKE_FAILED"),
            },
            Err(response) => response,
        },
        AdminRequest::Promote { user } | AdminRequest::Demote { user } => {
            let is_admin = matches!(request, AdminRequest::Promote { .. });
            match resolve(server, user).await {
                Ok((user_id, name)) => match set_role(server, None, user_id, is_admin).await {
                    Ok(()) => AdminResponse::User { user_id, name },
                    Err(e) => store_err(e, "PROMOTE_FAILED"),
                },
                Err(response) => response,
            }
        }
        AdminRequest::Enable { user } | AdminRequest::Disable { user } => {
            let disabled = matches!(request, AdminRequest::Disable { .. });
            match resolve(server, user).await {
                Ok((user_id, name)) => match set_disabled(server, None, user_id, disabled).await {
                    Ok(closed) if disabled => AdminResponse::Disable {
                        user_id,
                        name,
                        closed,
                    },
                    Ok(_) => AdminResponse::User { user_id, name },
                    Err(e) => store_err(e, "DISABLE_FAILED"),
                },
                Err(response) => response,
            }
        }
        AdminRequest::Kick { user } => match resolve(server, user).await {
            Ok((user_id, name)) => match kick(server, None, user_id).await {
                Ok(closed) => AdminResponse::Kick {
                    user_id,
                    name,
                    closed,
                },
                Err(e) => store_err(e, "KICK_FAILED"),
            },
            Err(response) => response,
        },
        AdminRequest::Mute { user }
        | AdminRequest::Unmute { user }
        | AdminRequest::Deafen { user }
        | AdminRequest::Undeafen { user } => match resolve(server, user).await {
            Ok((user_id, name)) => {
                // One side of the pair changes, the other stays as it is at
                // the moment of the write: computed under the store lock, so
                // a `mute` and a `deafen` at once cannot undo each other.
                let update = |(muted, deafened): (bool, bool)| match request {
                    AdminRequest::Mute { .. } => (true, deafened),
                    AdminRequest::Unmute { .. } => (false, deafened),
                    AdminRequest::Deafen { .. } => (muted, true),
                    _ => (muted, false),
                };
                match set_restriction(server, None, user_id, update).await {
                    Ok(_) => AdminResponse::User { user_id, name },
                    Err(e) => store_err(e, "RESTRICT_FAILED"),
                }
            }
            Err(response) => response,
        },
        AdminRequest::Status => AdminResponse::Status(status(server)),
        AdminRequest::RotateServerCert => match rotate_server_cert(server).await {
            Ok(()) => AdminResponse::Ok,
            Err(e) => AdminResponse::err("ROTATE_FAILED", e),
        },
        AdminRequest::RenewCa => match renew_ca(server).await {
            Ok(()) => AdminResponse::Ok,
            Err(e) => AdminResponse::err("RENEW_CA_FAILED", e),
        },
        AdminRequest::Backup => {
            match crate::backup::snapshot(server, crate::backup::Reason::Manual).await {
                Ok(Some(path)) => AdminResponse::Backup {
                    path: path.display().to_string(),
                },
                Ok(None) => AdminResponse::err("BACKUP_FAILED", "no snapshot taken"),
                Err(e) => AdminResponse::err("BACKUP_FAILED", e),
            }
        }
    }
}

/// Issues a fresh server leaf and puts the old one on the CRL, live without a
/// restart. Behind `rotate-server-cert` and the automatic rotation before
/// expiry (`housekeeping`). Handshakes already done keep the old leaf until
/// they reconnect.
///
/// Rotation first, the CRL entry after: a rotation that fails must leave
/// the live leaf alone — on the CRL, every client would refuse the server
/// until the next rotation succeeded. When the CRL entry cannot be written,
/// the old leaf stays pending in the CA and in `server.crt.retired`
/// ([`Ca::keep_retired_leaves`](crate::ca::Ca::keep_retired_leaves)) for
/// the next rotation or start. The error says which of the two halves is in
/// force: the new leaf serves only once the TLS config is rebuilt, which
/// needs a store write for the CRL number as well.
pub async fn rotate_server_cert(server: &Arc<Server>) -> Result<()> {
    let recorded = {
        let mut store = server.store.lock().await;
        let mut ca = server.ca.write().await;
        ca.rotate_server_cert(&server.config.tls, &server.config.server.server_name)
            .context("no new server certificate, the current one stays in use")?;
        record_retired_leaves(&mut store, &mut ca)
    };
    // The new leaf and the CRL go live without a restart.
    let reloaded = crate::endpoint::reload_tls(server).await;
    const PENDING: &str = "the old one is not on the CRL yet (kept in server.crt.retired, \
                           recorded with the next rotation or restart)";
    match (recorded, reloaded) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) => Err(e.context(format!(
            "the new server certificate is in use, but {PENDING}"
        ))),
        (Ok(()), Err(e)) => Err(e.context(
            "the new server certificate is issued and the old one recorded for the CRL, \
             but neither is in force before the next successful reload or restart",
        )),
        (Err(e), Err(_)) => Err(e.context(format!(
            "the new server certificate is issued, but not in use before the next \
             successful reload or restart, and {PENDING}"
        ))),
    }
}

/// Puts the server leaves the CA retired on the CRL, in one store write, and
/// then clears them from `server.crt.retired`; keeps them pending in the CA
/// and in that file when the write fails.
pub fn record_retired_leaves(store: &mut Store, ca: &mut crate::ca::Ca) -> Result<()> {
    let retired = ca.take_retired_leaves();
    if !retired.is_empty() {
        let entries: Vec<(String, String)> = retired
            .iter()
            .map(|der| (crate::ca::leaf_serial(der), crate::ca::leaf_not_after(der)))
            .filter(|(serial, _)| !serial.is_empty())
            .collect();
        if let Err(e) = store.revoke_server_certs(&entries) {
            ca.keep_retired_leaves(retired);
            return Err(e);
        }
    }
    // Also with nothing taken: a file still listing the leaf in use, from a
    // rotation that failed, goes. One that cannot be removed only gets its
    // leaves recorded once more at the next start.
    if let Err(e) = ca.retired_recorded() {
        tracing::warn!(error = %e, "cannot clear server.crt.retired");
    }
    Ok(())
}

/// Re-issues the CA certificate with the same key and subject, live without
/// a restart. Behind `renew-ca` and the automatic renewal a year before the CA
/// expires (`housekeeping`). Clients keep their stored CA certificate: the
/// trust anchor is subject and key, not the dates (`Ca::renew_ca_cert`).
pub async fn renew_ca(server: &Arc<Server>) -> Result<()> {
    server.ca.write().await.renew_ca_cert(&server.config.tls)?;
    crate::endpoint::reload_tls(server).await
}

/// Resolves a CLI selector to exactly one user and the name they have now
/// ([`Store::resolve`]); an unknown or ambiguous one comes back as the
/// response to send. Ambiguous is an error, never a guess: `revoke` is final.
async fn resolve(
    server: &Arc<Server>,
    user: &str,
) -> std::result::Result<(UserId, String), AdminResponse> {
    let store = server.store.lock().await;
    let name = |id: UserId| store.user(id).map(|u| u.name.clone()).unwrap_or_default();
    match store.resolve(user).as_slice() {
        [id] => Ok((*id, name(*id))),
        [] => Err(AdminResponse::err(
            "NO_SUCH_USER",
            format!("no user {user:?}"),
        )),
        ids => {
            let who: Vec<String> = ids
                .iter()
                .map(|id| format!("user_id {id} ({:?})", name(*id)))
                .collect();
            Err(AdminResponse::err(
                "AMBIGUOUS_USER",
                format!(
                    "{user:?} matches {}; say id:<user_id> or name:<name>",
                    who.join(" and ")
                ),
            ))
        }
    }
}

/// Who asks for an in-band admin action: the user and the certificate the
/// session authenticated with. The socket has none — its authentication is
/// the file permissions.
#[derive(Debug, Clone)]
pub struct Actor {
    /// The session's user.
    pub user_id: UserId,
    /// SHA-256 of the session's leaf, as the store keys it.
    pub cert_fp: String,
}

impl Actor {
    /// Whether `store`, as it is now, still lets this actor act as admin:
    /// its certificate admitted (known, not revoked, the user not disabled)
    /// and its user an admin. Checked under the same lock as the change it
    /// guards — the session was admitted earlier, and an action queued on
    /// the store lock may run after the disable or revoke that kicks it.
    pub fn is_admin(&self, store: &Store) -> bool {
        crate::session::admit(store, &self.cert_fp)
            .is_ok_and(|(user_id, _, _)| user_id == self.user_id)
            && store.is_admin(self.user_id)
    }
}

/// Under one store lock: an in-band `actor` must still be an admin
/// ([`Actor::is_admin`]) and `target` must exist (the socket passes `None`),
/// then `change` runs. The lock is released before this returns, so the
/// callers' asynchronous side effects — the CRL reload — never run under
/// it.
async fn change<T>(
    server: &Arc<Server>,
    actor: Option<&Actor>,
    target: UserId,
    change: impl FnOnce(&mut Store) -> Result<T>,
) -> Result<T> {
    let mut store = server.store.lock().await;
    if let Some(actor) = actor {
        if !actor.is_admin(&store) {
            return Err(Error::coded(ec::NOT_ADMIN, "not an admin"));
        }
        if store.user(target).is_none() {
            return Err(Error::coded(ec::NO_SUCH_USER, "unknown user"));
        }
    }
    change(&mut store)
}

/// Revokes every certificate of `target` and kicks its live connections with
/// `KICKED`; the CRL goes live at once (§5). Returns how many were closed.
/// Behind `revoke` and `AdminRevokeUser`.
pub async fn revoke_user(
    server: &Arc<Server>,
    actor: Option<&Actor>,
    target: UserId,
) -> Result<usize> {
    change(server, actor, target, |store| store.revoke(target)).await?;
    let closed = kick_user(server, target, ec::KICKED);
    tracing::info!(actor = actor.map(|a| a.user_id), target, closed, "revoked");
    crate::endpoint::reload_tls_logged(server).await;
    Ok(closed)
}

/// Grants or revokes admin rights, live: `UserAdminChanged` to every one of
/// the target's own sessions, so their cached `is_admin` updates without a
/// reconnect, then a `UserState` broadcast per session whose `ADMIN` bit
/// flipped, which is what moves the badge on everybody else's roster. Behind
/// `promote`/`demote` and `AdminSetRole`.
///
/// Store write, room flag and pushes happen under the store lock, like
/// [`set_restriction`]: a `promote` and a `demote` at once cannot leave the
/// room showing one role while the store holds the other, nor arrive the
/// wrong way round.
pub async fn set_role(
    server: &Arc<Server>,
    actor: Option<&Actor>,
    target: UserId,
    is_admin: bool,
) -> Result<()> {
    change(server, actor, target, |store| {
        store.set_admin(target, is_admin)?;
        for s in server.room.sessions_of_user(target) {
            server
                .room
                .send_to(s, &ControlMsg::UserAdminChanged { is_admin });
        }
        for s in server.room.set_admin_flag(target, is_admin) {
            if let Some(flags) = server.room.flags(s) {
                server
                    .room
                    .broadcast(&ControlMsg::UserState { session: s, flags });
            }
        }
        Ok(())
    })
    .await?;
    tracing::info!(
        actor = actor.map(|a| a.user_id),
        target,
        is_admin,
        "admin role changed"
    );
    Ok(())
}

/// Blocks or restores login. Disabling takes effect immediately: the
/// target's live sessions are closed with `AUTH_DISABLED` — the close itself
/// is the notification — and how many is returned; enabling closes nothing.
/// Behind `disable`/`enable` and `AdminSetDisabled`.
///
/// Store write and close happen under the store lock, like [`set_role`]:
/// a `disable` and an `enable` at once used to write in one order and kick
/// after both, leaving the user enabled but thrown out with the fatal
/// `AUTH_DISABLED` (external review 2026-09-27, S-5).
pub async fn set_disabled(
    server: &Arc<Server>,
    actor: Option<&Actor>,
    target: UserId,
    disabled: bool,
) -> Result<usize> {
    let closed = change(server, actor, target, |store| {
        store.set_disabled(target, disabled)?;
        Ok(if disabled {
            kick_user(server, target, ec::AUTH_DISABLED)
        } else {
            0
        })
    })
    .await?;
    tracing::info!(
        actor = actor.map(|a| a.user_id),
        target,
        disabled,
        closed,
        "disabled state changed"
    );
    Ok(closed)
}

/// Closes the target's live connections with `KICKED` and changes nothing
/// else: they may connect again. Returns how many were closed. Behind
/// `kick` and `AdminKick`.
pub async fn kick(server: &Arc<Server>, actor: Option<&Actor>, target: UserId) -> Result<usize> {
    change(server, actor, target, |_| Ok(false)).await?;
    let closed = kick_user(server, target, ec::KICKED);
    tracing::info!(actor = actor.map(|a| a.user_id), target, closed, "kicked");
    Ok(closed)
}

/// Enforces or lifts mute and deafen, persisted and live: the target's own
/// sessions get `UserRestrictionChanged`, and every session whose flags
/// changed a `UserState` broadcast. `update` maps the current `(muted,
/// deafened)` to the new pair, which is returned. Behind `mute`/`unmute`/
/// `deafen`/`undeafen` and `AdminSetRestriction`.
///
/// Store write, room change and pushes happen under the store lock, all of
/// it synchronous: two admins at once cannot leave the room enforcing one
/// pair while the store holds the other, their pushes cannot arrive the
/// wrong way round, and a session joining meanwhile, which reads the store
/// under that lock too, sees either the old pair and gets the new one from
/// here or sees the new one.
pub async fn set_restriction(
    server: &Arc<Server>,
    actor: Option<&Actor>,
    target: UserId,
    update: impl FnOnce((bool, bool)) -> (bool, bool),
) -> Result<(bool, bool)> {
    let (muted, deafened) = change(server, actor, target, |store| {
        let (muted, deafened) = update(store.restriction(target));
        store.set_restriction(target, muted, deafened)?;
        let pushed = crate::session::restriction_changed(muted, deafened);
        for s in server.room.sessions_of_user(target) {
            server.room.send_to(s, &pushed);
        }
        let forced = crate::session::restriction_flags(muted, deafened);
        for (session, flags) in server.room.set_forced(target, forced) {
            server
                .room
                .broadcast(&ControlMsg::UserState { session, flags });
        }
        Ok((muted, deafened))
    })
    .await?;
    tracing::info!(
        actor = actor.map(|a| a.user_id),
        target,
        muted,
        deafened,
        "restriction changed"
    );
    Ok((muted, deafened))
}

/// Closes every connection of a user with `code`, room or not
/// (`session::close_user`).
fn kick_user(server: &Arc<Server>, user_id: UserId, code: u32) -> usize {
    crate::session::close_user(server, user_id, code)
}

/// Maps a store failure onto a response. A coded error reports under its own
/// protocol name, so the socket does not invent a second name for something
/// `icu_proto::error` already names; anything uncoded becomes `failed`.
fn store_err(e: Error, failed: &str) -> AdminResponse {
    match e.code().map(ec::name) {
        Some(name) if name != "UNKNOWN" => AdminResponse::err(name, e),
        _ => AdminResponse::err(failed, e),
    }
}

/// `status`: everything under `relay` and `enroll`, and `connections`, are
/// counters since the start — a monitor forms rates from them; `online`,
/// `open_connections`, `handshakes` and `roomless_sessions` are what is open
/// right now (S8, readiness review 2026-09-27: `connections` used to be
/// graphed as if it were the latter).
fn status(server: &Arc<Server>) -> serde_json::Value {
    use std::sync::atomic::Ordering::Relaxed;
    let c = &server.counters;
    json!({
        "uptime_s": server.uptime().as_secs(),
        "online": server.room.len(),
        "max_users": server.config.server.max_users,
        "server_time_ms": unix_millis(),
        "connections": Counters::get(&c.connections),
        "open_connections": server.open_connections.load(Relaxed),
        "handshakes": server.handshakes.load(Relaxed),
        // Datagrams without a valid com tag (`PROTOCOL.md` §1): port noise,
        // or a client whose passport belongs to another key.
        "com_dropped": server.com_dropped.load(Relaxed),
        "roomless_sessions": crate::lock(&server.roomless).len(),
        "tls": tls_status(server),
        "backup": crate::backup::status_json(server),
        "relay": {
            "datagrams_in": Counters::get(&c.datagrams_in),
            "datagrams_out": Counters::get(&c.datagrams_out),
            "video_frames_in": Counters::get(&c.video_frames_in),
            "video_frames_out": Counters::get(&c.video_frames_out),
            "control_in": Counters::get(&c.control_in),
            "dropped": {
                "malformed": Counters::get(&c.dropped_malformed),
                "rate": Counters::get(&c.dropped_rate),
                "muted": Counters::get(&c.dropped_muted),
                "send_too_large": Counters::get(&c.send_too_large),
                "send_unsupported": Counters::get(&c.send_unsupported),
                "send_disabled": Counters::get(&c.send_disabled),
                "send_connection_lost": Counters::get(&c.send_connection_lost),
                "control_rate_limited": Counters::get(&c.control_rate_limited),
                "video_no_receiver": Counters::get(&c.video_dropped_no_receiver),
                "video_rate": Counters::get(&c.video_dropped_rate),
                "video_slow_receiver": Counters::get(&c.video_dropped_slow_receiver),
                "video_not_allowed": Counters::get(&c.video_dropped_not_allowed),
                "video_stalled": Counters::get(&c.video_dropped_stalled),
            }
        },
        "enroll": {
            "attempts": Counters::get(&c.enroll_attempts),
            "ok": Counters::get(&c.enroll_ok),
            "failed": Counters::get(&c.enroll_failed),
        }
    })
}

/// Expiry of the server leaf and the CA. The days are integers for the
/// Checkmk check, the timestamps are for people.
fn tls_status(server: &Arc<Server>) -> serde_json::Value {
    let Ok(ca) = server.ca.try_read() else {
        // A rotation holds the lock right now; the next poll has it.
        return json!({});
    };
    json!({
        "server_cert_expires": crate::ca::leaf_not_after(ca.server_der()),
        "server_cert_days_left": crate::ca::leaf_days_left(ca.server_der()),
        "ca_expires": crate::ca::leaf_not_after(ca.ca_der()),
        "ca_days_left": crate::ca::leaf_days_left(ca.ca_der()),
    })
}

/// Opens the admin socket and serves it until shutdown.
///
/// Returns the socket path and the serving task. Failure to bind is not fatal
/// for the server; only the CLI becomes unavailable.
pub async fn listen(server: Arc<Server>) -> Result<(PathBuf, AbortHandle)> {
    let path = server.config.admin_socket();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context(format!("cannot create {}", dir.display()))?;
    }
    // A leftover socket from a crash would block the bind.
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path).context(format!("cannot bind {}", path.display()))?;

    // Mode 0660, group from the configuration (§5, §8).
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))
        .context("cannot chmod the admin socket")?;
    if let Some(group) = &server.config.server.admin_socket_group {
        if let Err(e) = crate::store::blocking(|| chgrp(&path, group)) {
            tracing::warn!(group = %group, error = %e, "cannot set the admin socket group");
        }
    }

    let task = crate::spawn_logged("admin socket", serve(server, listener));
    Ok((path, task))
}

fn chgrp(path: &std::path::Path, group: &str) -> Result<()> {
    let gid = resolve_group(group)?;
    // `Gid::from_raw` is safe in rustix 1; the name is historical.
    rustix::fs::chown(path, None, Some(rustix::fs::Gid::from_raw(gid)))?;
    Ok(())
}

/// The gid of `group`, like chown(1): a name in `/etc/group`, else a number
/// as it is, else what the host's `getent group` says. The server is a
/// static musl binary without NSS, so a group only a directory service
/// knows — FreeIPA/SSSD, LDAP, systemd-userdb — used to be ignored with a
/// warning (external review 2026-09-27); `getent` asks the host's NSS for
/// it. Where there is none (the `scratch` image), a number still works.
pub fn resolve_group(group: &str) -> Result<u32> {
    if let Some(gid) = std::fs::read_to_string("/etc/group")
        .ok()
        .and_then(|text| group_gid(&text, group))
    {
        return Ok(gid);
    }
    if let Ok(gid) = group.parse::<u32>() {
        return Ok(gid);
    }
    getent_gid(group)
}

/// The gid `getent group -- <group>` names: the host's NSS. Starts a
/// process, which is why the tests that call it live in `tests/cli_binary.rs`.
pub fn getent_gid(group: &str) -> Result<u32> {
    let out = std::process::Command::new("getent")
        .args(["group", "--", group])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            Error::msg(format!(
                "no group {group:?} in /etc/group, and no getent to ask the host ({e})"
            ))
        })?;
    out.status
        .success()
        .then(|| group_gid(&String::from_utf8_lossy(&out.stdout), group))
        .flatten()
        .ok_or_else(|| {
            Error::msg(format!(
                "no group {group:?} in /etc/group or through getent"
            ))
        })
}

/// The gid of `group` in `group(5)` lines: `name:password:gid:members`.
fn group_gid(text: &str, group: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _passwd = fields.next()?;
        let gid = fields.next()?;
        (name == group).then(|| gid.parse::<u32>().ok())?
    })
}

/// Longest request line the admin socket reads (external review 2026-09-27,
/// S-6). A request is a few hundred bytes; without a cap a line that never
/// ends was buffered until `MemoryMax` ended the server, and the socket's
/// group — the Checkmk agent among them — is not trusted that far.
pub const MAX_REQUEST_LINE: usize = 64 * 1024;

/// Admin connections served at once. Further ones wait in the listen
/// backlog, where they hold none of the server's file descriptors.
pub const MAX_CONNECTIONS: usize = 16;

/// How long [`exchange`] waits for the answer. Above the server's own
/// `timing.admin_timeout_ms`, which only bounds reading the request and
/// writing the answer, not the work in between.
pub const EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// First pause after a failed `accept` on the admin socket; it doubles with
/// every further failure, up to [`ACCEPT_BACKOFF_MAX`].
const ACCEPT_BACKOFF_MIN: std::time::Duration = std::time::Duration::from_millis(10);
/// Longest pause between two `accept` attempts that fail.
const ACCEPT_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(1);

async fn serve(server: Arc<Server>, listener: UnixListener) {
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let mut backoff = ACCEPT_BACKOFF_MIN;
    loop {
        // A slot before the accept: past the cap, connections wait in the
        // backlog instead of costing a descriptor each.
        let permit = tokio::select! {
            permit = permits.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return,
            },
            _ = server.shutdown_requested() => return,
        };
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    backoff = ACCEPT_BACKOFF_MIN;
                    stream
                }
                Err(e) => {
                    // Out of file descriptors (EMFILE, ENFILE), a connection
                    // aborted before it was taken (ECONNABORTED): the
                    // listener is fine. Returning here left the socket dead
                    // until a restart — CLI and Checkmk locked out while
                    // voice went on. The pause keeps a persistent error from
                    // spinning; the pending connection is still there.
                    tracing::warn!(
                        error = %e,
                        retry_ms = backoff.as_millis() as u64,
                        "admin accept failed"
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = server.shutdown_requested() => return,
                    }
                    backoff = (backoff * 2).min(ACCEPT_BACKOFF_MAX);
                    continue;
                }
            },
            _ = server.shutdown_requested() => return,
        };
        let server = server.clone();
        crate::spawn_logged("admin connection", async move {
            let _permit = permit;
            if let Err(e) = serve_one(server, stream).await {
                tracing::debug!(error = %e, "admin connection ended");
            }
        });
    }
}

/// Serves one connection: one request, one answer, close. Every client is
/// one-shot — the CLI and through it the Checkmk check — and a connection
/// looping over requests was one more way to hold a slot for good.
async fn serve_one(server: Arc<Server>, stream: UnixStream) -> Result<()> {
    let timeout = server.config.timing.admin_timeout();
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let request = tokio::time::timeout(timeout, read_request(&mut read))
        .await
        .map_err(|_| Error::msg("no request in time"))??;
    let response = match request {
        Request::Closed => return Ok(()),
        Request::TooLong => AdminResponse::err(
            "BAD_REQUEST",
            format!("request line longer than {MAX_REQUEST_LINE} bytes"),
        ),
        Request::Line(line) => match serde_json::from_str::<AdminRequest>(&line) {
            Ok(request) => handle(&server, request).await,
            Err(e) => AdminResponse::err("BAD_REQUEST", e),
        },
    };
    tokio::time::timeout(timeout, write_line(&mut write, &response.to_json()))
        .await
        .map_err(|_| Error::msg("the answer was not taken in time"))?
}

/// What came in on an admin connection.
enum Request {
    /// The request line, without its line end.
    Line(String),
    /// A line longer than [`MAX_REQUEST_LINE`].
    TooLong,
    /// The client closed without sending a request.
    Closed,
}

/// Reads the first line that is not blank, at most [`MAX_REQUEST_LINE`]
/// bytes of it. A last line without a line end counts.
async fn read_request(read: &mut (impl AsyncBufRead + Unpin)) -> Result<Request> {
    loop {
        let mut line = Vec::new();
        let n = read
            .take(MAX_REQUEST_LINE as u64 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if n == 0 {
            return Ok(Request::Closed);
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        } else if line.len() > MAX_REQUEST_LINE {
            return Ok(Request::TooLong);
        }
        let line = String::from_utf8_lossy(&line);
        if !line.trim().is_empty() {
            return Ok(Request::Line(line.into_owned()));
        }
    }
}

/// One JSON document as one line, flushed.
async fn write_line(
    write: &mut (impl AsyncWriteExt + Unpin),
    value: &impl Serialize,
) -> Result<()> {
    let mut body = serde_json::to_vec(value)?;
    body.push(b'\n');
    write.write_all(&body).await?;
    write.flush().await?;
    Ok(())
}

/// Sends one request to a running server's admin socket.
pub async fn request(
    socket: &std::path::Path,
    request: &AdminRequest,
) -> Result<serde_json::Value> {
    let stream = UnixStream::connect(socket)
        .await
        .context(format!("cannot connect to {}", socket.display()))?;
    exchange(stream, request).await
}

/// Sends one request over an already connected admin socket and reads the
/// answer, for at most [`EXCHANGE_TIMEOUT`]: a server that hangs must not
/// hang the CLI, nor the Checkmk agent behind it.
pub async fn exchange(stream: UnixStream, request: &AdminRequest) -> Result<serde_json::Value> {
    let answer = async {
        let (read, mut write) = stream.into_split();
        write_line(&mut write, request).await?;
        let mut lines = BufReader::new(read).lines();
        let line = lines
            .next_line()
            .await?
            .ok_or_else(|| Error::msg("admin socket closed without a response"))?;
        Ok(serde_json::from_str(&line)?)
    };
    tokio::time::timeout(EXCHANGE_TIMEOUT, answer)
        .await
        .map_err(|_| {
            Error::msg(format!(
                "no answer within {} s; the server may be stuck, and the command \
                 may or may not have taken effect",
                EXCHANGE_TIMEOUT.as_secs()
            ))
        })?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §5: the JSON wire forms, verbatim from the table.
    #[test]
    fn request_wire_forms_parse() {
        let invite: AdminRequest =
            serde_json::from_str(r#"{"cmd":"invite","name":"bob","ttl_s":86400}"#).unwrap();
        assert!(matches!(
            invite,
            AdminRequest::Invite {
                ttl_s: Some(86400),
                ..
            }
        ));
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"users"}"#).unwrap(),
            AdminRequest::Users
        ));
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"revoke","user":"alice"}"#).unwrap(),
            AdminRequest::Revoke { .. }
        ));
        // A label-less invite is the normal case now that the name comes
        // from the client's CSR.
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"invite"}"#).unwrap(),
            AdminRequest::Invite { .. }
        ));
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"status"}"#).unwrap(),
            AdminRequest::Status
        ));
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"rotate-server-cert"}"#).unwrap(),
            AdminRequest::RotateServerCert
        ));
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"renew-ca"}"#).unwrap(),
            AdminRequest::RenewCa
        ));
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"backup"}"#).unwrap(),
            AdminRequest::Backup
        ));
        // An invite without a ttl is allowed and means the default.
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"invite","name":"bob"}"#).unwrap(),
            AdminRequest::Invite { ttl_s: None, .. }
        ));
    }

    /// The admin socket's group resolves from `group(5)` lines and as a
    /// number; through the host's NSS in `tests/cli_binary.rs`.
    #[test]
    fn the_socket_group_resolves_by_name_and_number() {
        let text = "root:x:0:\nicu:x:975:checkmk,chris\nbroken\n";
        assert_eq!(group_gid(text, "icu"), Some(975));
        assert_eq!(group_gid(text, "root"), Some(0));
        assert_eq!(group_gid(text, "ic"), None);
        assert_eq!(group_gid(text, "broken"), None);

        // Neither starts `getent`: a child forked here would share the
        // state locks of the tests beside it (`tests/cli_binary.rs`).
        assert_eq!(resolve_group("root").unwrap(), 0);
        let mine = rustix::process::getegid().as_raw();
        assert_eq!(resolve_group(&mine.to_string()).unwrap(), mine);

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("admin.sock");
        std::fs::write(&file, "").unwrap();
        chgrp(&file, &mine.to_string()).unwrap();
    }

    #[test]
    fn response_wire_forms() {
        let ok = AdminResponse::Ok.to_json();
        assert_eq!(ok, json!({"ok": true}));

        let err = AdminResponse::err("NAME_TAKEN", "taken").to_json();
        assert_eq!(err["ok"], json!(false));
        assert_eq!(err["error"], json!("NAME_TAKEN"));
        assert!(err["detail"].is_string());

        let revoke = AdminResponse::Revoke {
            user_id: 1,
            name: "alice".into(),
            closed: 1,
        }
        .to_json();
        assert_eq!(
            revoke,
            json!({"ok": true, "user_id": 1, "name": "alice", "closed": 1})
        );
        let promote = AdminResponse::User {
            user_id: 2,
            name: "1".into(),
        }
        .to_json();
        assert_eq!(promote, json!({"ok": true, "user_id": 2, "name": "1"}));

        let backup = AdminResponse::Backup {
            path: "/var/lib/icu/backups/20260927T120000Z".into(),
        }
        .to_json();
        assert_eq!(
            backup,
            json!({"ok": true, "path": "/var/lib/icu/backups/20260927T120000Z"})
        );

        let status = AdminResponse::Status(json!({"uptime_s": 3600, "online": 3})).to_json();
        assert_eq!(status["ok"], json!(true));
        assert_eq!(status["uptime_s"], json!(3600));
    }
}
