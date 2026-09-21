//! Admin socket: `/run/icu/admin.sock`, one JSON request per line, one JSON
//! response per line, then close (`Server/IMPLEMENTATION.md` §5).
//!
//! A running server is the only writer of `users.toml` and `invites.toml`, so
//! every CLI command except `run` goes through here. Authentication is the
//! file permissions of the socket, nothing else.

use std::{path::PathBuf, sync::Arc, time::Duration};

use icu_proto::{error as ec, UserId};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    task::JoinHandle,
};

use crate::{
    error::{Context, Error, Result},
    room::Counters,
    session,
    store::{parse_rfc3339, unix_millis},
    Server,
};

/// One admin command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum AdminRequest {
    /// Create an invite token.
    Invite {
        /// Name the certificate CN will carry.
        name: String,
        /// Lifetime in seconds. Defaults to 24 h.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_s: Option<u64>,
    },
    /// List all users.
    Users,
    /// Revoke every certificate of a user and kick it.
    Revoke {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Change a display name.
    Rename {
        /// `user_id` or name.
        user: String,
        /// The new name.
        new: String,
    },
    /// Grant admin rights.
    Promote {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Revoke admin rights.
    Demote {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Reversibly block login without touching certificates.
    Disable {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Restore login after `Disable`.
    Enable {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Uptime, online count and relay counters.
    Status,
    /// Issue a fresh server leaf from the same CA.
    RotateServerCert,
}

/// The answer to an [`AdminRequest`].
#[derive(Debug, Clone, PartialEq)]
pub enum AdminResponse {
    /// `invite`
    Invite {
        /// The token.
        token: String,
        /// The complete `icu://` link.
        link: String,
        /// Expiry, RFC 3339 UTC.
        expires: String,
    },
    /// `users`
    Users(Vec<AdminUser>),
    /// `revoke`
    Revoke {
        /// The affected user.
        user_id: UserId,
        /// How many live connections were closed.
        closed: usize,
    },
    /// `rename`, `promote`, `demote`, `enable`, `rotate-server-cert`
    Ok,
    /// `disable`
    Disable {
        /// The affected user.
        user_id: UserId,
        /// How many live connections were closed.
        closed: usize,
    },
    /// `status`
    Status(serde_json::Value),
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
}

impl AdminResponse {
    /// Whether the command succeeded.
    pub fn is_ok(&self) -> bool {
        !matches!(self, AdminResponse::Error { .. })
    }

    /// The wire form: `{"ok":true,…}` or `{"ok":false,"error":…}` (§5).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            AdminResponse::Invite {
                token,
                link,
                expires,
            } => json!({"ok": true, "token": token, "link": link, "expires": expires}),
            AdminResponse::Users(users) => json!({"ok": true, "users": users}),
            AdminResponse::Revoke { user_id, closed } => {
                json!({"ok": true, "user_id": user_id, "closed": closed})
            }
            AdminResponse::Ok => json!({"ok": true}),
            AdminResponse::Disable { user_id, closed } => {
                json!({"ok": true, "user_id": user_id, "closed": closed})
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
    match request {
        AdminRequest::Invite { name, ttl_s } => {
            let ttl = Duration::from_secs(ttl_s.unwrap_or(icu_proto::limits::INVITE_TTL_S).max(1));
            let mut store = server.store.lock().await;
            match store.create_invite(&name, ttl) {
                Ok((token, record)) => {
                    let ca_pin = server.ca.read().await.ca_pin();
                    AdminResponse::Invite {
                        link: invite_link(
                            &server.config.public_host(),
                            &token,
                            &ca_pin,
                            &record.name,
                        ),
                        token,
                        expires: record.expires,
                    }
                }
                Err(e) => AdminResponse::err("BAD_NAME", e),
            }
        }
        AdminRequest::Users => {
            let store = server.store.lock().await;
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
                    revoked: !record.certs.is_empty() && record.certs.iter().all(|c| c.revoked),
                    online: !server.room.sessions_of_user(user_id).is_empty(),
                    is_admin: record.is_admin,
                    disabled: record.disabled,
                });
            }
            AdminResponse::Users(users)
        }
        AdminRequest::Revoke { user } => {
            let mut store = server.store.lock().await;
            let Some(user_id) = store.resolve(&user) else {
                return AdminResponse::err("NO_SUCH_USER", format!("no user {user:?}"));
            };
            match store.revoke(user_id) {
                Ok(_) => {
                    drop(store);
                    // A revoke takes effect immediately (§5).
                    let closed = session::kick_user(server, user_id, ec::KICKED);
                    tracing::info!(user_id, closed, "revoked");
                    AdminResponse::Revoke { user_id, closed }
                }
                Err(e) if e.code() == Some(ec::LAST_ADMIN) => AdminResponse::err("LAST_ADMIN", e),
                Err(e) => AdminResponse::err("REVOKE_FAILED", e),
            }
        }
        AdminRequest::Rename { user, new } => {
            let mut store = server.store.lock().await;
            let Some(user_id) = store.resolve(&user) else {
                return AdminResponse::err("NO_SUCH_USER", format!("no user {user:?}"));
            };
            match store.rename(user_id, &new) {
                Ok(()) => {
                    drop(store);
                    for session in server.room.sessions_of_user(user_id) {
                        server.room.rename(session, &new);
                        server.room.broadcast(&icu_proto::ControlMsg::UserRenamed {
                            session,
                            name: new.clone(),
                        });
                    }
                    AdminResponse::Ok
                }
                Err(e) if e.code() == Some(ec::NAME_TAKEN) => AdminResponse::err("NAME_TAKEN", e),
                Err(e) => AdminResponse::err("RENAME_FAILED", e),
            }
        }
        AdminRequest::Promote { user } => set_role(server, &user, true).await,
        AdminRequest::Demote { user } => set_role(server, &user, false).await,
        AdminRequest::Enable { user } => set_disabled(server, &user, false).await,
        AdminRequest::Disable { user } => {
            let mut store = server.store.lock().await;
            let Some(user_id) = store.resolve(&user) else {
                return AdminResponse::err("NO_SUCH_USER", format!("no user {user:?}"));
            };
            match store.set_disabled(user_id, true) {
                Ok(_) => {
                    drop(store);
                    let closed = session::kick_user(server, user_id, ec::AUTH_DISABLED);
                    tracing::info!(user_id, closed, "disabled");
                    AdminResponse::Disable { user_id, closed }
                }
                Err(e) if e.code() == Some(ec::LAST_ADMIN) => AdminResponse::err("LAST_ADMIN", e),
                Err(e) => AdminResponse::err("DISABLE_FAILED", e),
            }
        }
        AdminRequest::Status => AdminResponse::Status(status(server)),
        AdminRequest::RotateServerCert => {
            let mut ca = server.ca.write().await;
            match ca.rotate_server_cert(&server.config.tls, &server.config.server.server_name) {
                // The new leaf only takes effect on restart: quinn holds the
                // rustls config from startup. Reported as a deviation.
                Ok(()) => AdminResponse::Ok,
                Err(e) => AdminResponse::err("ROTATE_FAILED", e),
            }
        }
    }
}

/// Shared resolve→mutate→map-error body of `Promote`/`Demote`. On success,
/// pushes `UserAdminChanged` live to every one of the target's own online
/// sessions (same live-propagation shape as `Rename`'s `UserRenamed`), so
/// their own cached `is_admin` updates without a reconnect, regardless of
/// whether the action came from the socket or in-band.
async fn set_role(server: &Arc<Server>, user: &str, is_admin: bool) -> AdminResponse {
    let mut store = server.store.lock().await;
    let Some(user_id) = store.resolve(user) else {
        return AdminResponse::err("NO_SUCH_USER", format!("no user {user:?}"));
    };
    match store.set_admin(user_id, is_admin) {
        Ok(_) => {
            drop(store);
            for session in server.room.sessions_of_user(user_id) {
                server
                    .room
                    .send_to(session, &icu_proto::ControlMsg::UserAdminChanged { is_admin });
            }
            tracing::info!(user_id, is_admin, "admin role changed");
            AdminResponse::Ok
        }
        Err(e) if e.code() == Some(ec::LAST_ADMIN) => AdminResponse::err("LAST_ADMIN", e),
        Err(e) => AdminResponse::err("PROMOTE_FAILED", e),
    }
}

/// Shared resolve→mutate→map-error body of `Enable` (and the non-kicking
/// half of `Disable`'s logic; `Disable` itself stays inline in [`handle`]
/// since it also needs to kick).
async fn set_disabled(server: &Arc<Server>, user: &str, disabled: bool) -> AdminResponse {
    let mut store = server.store.lock().await;
    let Some(user_id) = store.resolve(user) else {
        return AdminResponse::err("NO_SUCH_USER", format!("no user {user:?}"));
    };
    match store.set_disabled(user_id, disabled) {
        Ok(_) => {
            tracing::info!(user_id, disabled, "admin disabled state changed");
            AdminResponse::Ok
        }
        Err(e) if e.code() == Some(ec::LAST_ADMIN) => AdminResponse::err("LAST_ADMIN", e),
        Err(e) => AdminResponse::err("DISABLE_FAILED", e),
    }
}

fn status(server: &Arc<Server>) -> serde_json::Value {
    let c = &server.counters;
    json!({
        "uptime_s": server.uptime().as_secs(),
        "online": server.room.len(),
        "max_users": server.config.server.max_users,
        "server_time_ms": unix_millis(),
        "connections": Counters::get(&c.connections),
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
            }
        },
        "enroll": {
            "attempts": Counters::get(&c.enroll_attempts),
            "ok": Counters::get(&c.enroll_ok),
            "failed": Counters::get(&c.enroll_failed),
        }
    })
}

/// Builds an invite link (`PROTOCOL.md` §2.3).
pub fn invite_link(public_host: &str, token: &str, ca_pin: &str, name: &str) -> String {
    format!(
        "icu://{public_host}/enroll?t={token}&ca={ca_pin}&n={}",
        percent_encode(name)
    )
}

/// Minimal percent-encoding for the `n=` parameter: everything outside the
/// unreserved set of RFC 3986 is escaped.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Opens the admin socket and serves it until shutdown.
///
/// Returns the socket path and the serving task. Failure to bind is not fatal
/// for the server; only the CLI becomes unavailable.
pub async fn listen(server: Arc<Server>) -> Result<(PathBuf, JoinHandle<()>)> {
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
        if let Err(e) = chgrp(&path, group) {
            tracing::warn!(group = %group, error = %e, "cannot set the admin socket group");
        }
    }

    let task = tokio::spawn(serve(server, listener));
    Ok((path, task))
}

fn chgrp(path: &std::path::Path, group: &str) -> Result<()> {
    // No `nix`/`users` crate in the dependency set, so resolve the group by
    // reading /etc/group.
    let text = std::fs::read_to_string("/etc/group")?;
    let gid = text
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let _passwd = fields.next()?;
            let gid = fields.next()?;
            (name == group).then(|| gid.parse::<u32>().ok())?
        })
        .ok_or_else(|| Error::msg(format!("no group {group:?} in /etc/group")))?;
    rustix::fs::chown(path, None, Some(unsafe_gid(gid)))?;
    Ok(())
}

fn unsafe_gid(gid: u32) -> rustix::fs::Gid {
    // `Gid::from_raw` is safe in rustix 1; the name is historical.
    rustix::fs::Gid::from_raw(gid)
}

async fn serve(server: Arc<Server>, listener: UnixListener) {
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::warn!(error = %e, "admin accept failed");
                    return;
                }
            },
            _ = server.shutdown_requested() => return,
        };
        let server = server.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_one(server, stream).await {
                tracing::debug!(error = %e, "admin connection ended");
            }
        });
    }
}

async fn serve_one(server: Arc<Server>, stream: UnixStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<AdminRequest>(&line) {
            Ok(request) => handle(&server, request).await,
            Err(e) => AdminResponse::err("BAD_REQUEST", e),
        };
        let mut body = serde_json::to_vec(&response.to_json())?;
        body.push(b'\n');
        write.write_all(&body).await?;
        write.flush().await?;
    }
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
    let (read, mut write) = stream.into_split();
    let mut body = serde_json::to_vec(request)?;
    body.push(b'\n');
    write.write_all(&body).await?;
    write.flush().await?;
    let mut lines = BufReader::new(read).lines();
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| Error::msg("admin socket closed without a response"))?;
    Ok(serde_json::from_str(&line)?)
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
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(
                r#"{"cmd":"rename","user":"alice","new":"alice2"}"#
            )
            .unwrap(),
            AdminRequest::Rename { .. }
        ));
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"status"}"#).unwrap(),
            AdminRequest::Status
        ));
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"rotate-server-cert"}"#).unwrap(),
            AdminRequest::RotateServerCert
        ));
        // An invite without a ttl is allowed and means the default.
        assert!(matches!(
            serde_json::from_str::<AdminRequest>(r#"{"cmd":"invite","name":"bob"}"#).unwrap(),
            AdminRequest::Invite { ttl_s: None, .. }
        ));
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
            closed: 1,
        }
        .to_json();
        assert_eq!(revoke, json!({"ok": true, "user_id": 1, "closed": 1}));

        let status = AdminResponse::Status(json!({"uptime_s": 3600, "online": 3})).to_json();
        assert_eq!(status["ok"], json!(true));
        assert_eq!(status["uptime_s"], json!(3600));
    }

    /// `PROTOCOL.md` §2.3: the invite link format.
    #[test]
    fn invite_links_are_well_formed() {
        let link = invite_link(
            "icu.example.org:4711",
            "k3nP8sQ2vL7xR1wY4tZ6bA",
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "bob",
        );
        assert_eq!(
            link,
            "icu://icu.example.org:4711/enroll?t=k3nP8sQ2vL7xR1wY4tZ6bA&ca=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08&n=bob"
        );
    }

    #[test]
    fn names_are_percent_encoded() {
        assert_eq!(percent_encode("bob"), "bob");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("mühle"), "m%C3%BChle");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }
}
