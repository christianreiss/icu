//! Per-client state machine on ALPN `icu/1`
//! (`Server/IMPLEMENTATION.md` §6.4).

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use icu_proto::{
    error as ec, limits as pl, name_key, AudioParams, ControlMsg, Session, UserId, UserInfo,
    MAX_CONTROL_FRAME,
};
use rustls::pki_types::CertificateDer;
use tokio::sync::mpsc;

use crate::{
    ca,
    error::{Error, Result},
    limits::{TokenBucket, ViolationCounter},
    relay::{self, VideoSender},
    room::{Counters, JoinError},
    store::{fingerprint, now, rfc3339, unix_millis, CertRecord, Store},
    wire, Server,
};

/// Handles one authenticated client connection from handshake to close.
pub async fn handle(server: Arc<Server>, conn: quinn::Connection) -> Result<()> {
    Counters::inc(&server.counters.connections);
    let remote = conn.remote_address();

    // 1. Client certificate: rustls has already checked chain and validity,
    //    so only the fingerprint is left (§6.4.1).
    let identity = match authenticate(&server, &conn).await {
        Ok(identity) => identity,
        Err(e) => {
            let code = e.code().unwrap_or(ec::AUTH_REQUIRED);
            tracing::info!(%remote, error = %e, "rejected");
            conn.close(quinn::VarInt::from_u32(code), ec::name(code).as_bytes());
            return Err(e);
        }
    };

    match serve(&server, &conn, &identity).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let code = e.code().unwrap_or(ec::PROTOCOL_VIOLATION);
            tracing::info!(%remote, user_id = identity.user_id, error = %e, "closing");
            conn.close(quinn::VarInt::from_u32(code), ec::name(code).as_bytes());
            Err(e)
        }
    }
}

/// Who is on the other end.
#[derive(Debug, Clone)]
struct Identity {
    user_id: UserId,
    /// SHA-256 of the presented leaf, lowercase hex.
    cert_fp: String,
    /// Subject CN of the presented leaf, `""` when it does not parse.
    /// Advisory: authentication is the fingerprint, never the name. It only
    /// says whether this client still holds a certificate carrying the current
    /// name, or has to catch up (`PROTOCOL.md` §2.2).
    cert_cn: String,
}

async fn authenticate(server: &Arc<Server>, conn: &quinn::Connection) -> Result<Identity> {
    // Spike 2b: without a client certificate this is `None`, not an empty Vec.
    let certs = conn
        .peer_identity()
        .and_then(|any| any.downcast::<Vec<CertificateDer>>().ok())
        .ok_or_else(|| Error::coded(ec::AUTH_REQUIRED, "no client certificate on icu/1"))?;
    let leaf = certs
        .first()
        .ok_or_else(|| Error::coded(ec::AUTH_REQUIRED, "empty certificate chain"))?;
    let fp = fingerprint(leaf);

    let store = server.store.lock().await;
    let (user_id, user, cert) = store
        .by_fingerprint(&fp)
        .ok_or_else(|| Error::coded(ec::AUTH_UNKNOWN_CERT, format!("unknown fingerprint {fp}")))?;
    if cert.revoked {
        return Err(Error::coded(
            ec::AUTH_REVOKED,
            format!("certificate {fp} is revoked"),
        ));
    }
    if user.disabled {
        return Err(Error::coded(
            ec::AUTH_DISABLED,
            format!("user_id {user_id} is disabled"),
        ));
    }
    let cert_cn = ca::leaf_common_name(leaf);
    if name_key(&cert_cn) != name_key(&user.name) {
        // Not a reason to reject: the store keeps fingerprints, not DER, so
        // certificates issued before the name moved into the certificate have
        // no recorded CN at all. The client notices the same mismatch and
        // renews itself (`PROTOCOL.md` §2.2).
        tracing::debug!(
            user_id,
            cert_cn = %cert_cn,
            stored = %user.name,
            "stale cn, client will catch up"
        );
    }
    Ok(Identity {
        user_id,
        cert_fp: fp,
        cert_cn,
    })
}

async fn serve(server: &Arc<Server>, conn: &quinn::Connection, identity: &Identity) -> Result<()> {
    // 2. Control stream and `Hello` within the timeout (§6.4.2).
    let (send, mut recv) =
        tokio::time::timeout(server.config.timing.hello_timeout(), conn.accept_bi())
            .await
            .map_err(|_| Error::coded(ec::HELLO_TIMEOUT, "no control stream within the timeout"))?
            .map_err(|e| Error::coded(ec::HELLO_TIMEOUT, e.to_string()))?;
    let _ = send.set_priority(1); // control beats video (`PROTOCOL.md` §4)

    let body = tokio::time::timeout(
        server.config.timing.hello_timeout(),
        wire::read_frame(&mut recv, MAX_CONTROL_FRAME),
    )
    .await
    .map_err(|_| Error::coded(ec::HELLO_TIMEOUT, "no Hello within the timeout"))??;

    let hello = ControlMsg::decode(&body)
        .map_err(|e| Error::coded(ec::PROTOCOL_VIOLATION, format!("bad Hello: {e}")))?;
    let ControlMsg::Hello {
        minor,
        caps: _,
        client,
        initial,
        name,
    } = hello
    else {
        return Err(Error::coded(
            ec::PROTOCOL_VIOLATION,
            "first message must be Hello",
        ));
    };
    if client.len() > ControlMsg::MAX_CLIENT_STRING {
        return Err(Error::coded(
            ec::PROTOCOL_VIOLATION,
            "Hello.client is longer than 64 bytes",
        ));
    }
    // `min(client, server)`; a minor 0 does not exist (`PROTOCOL.md` §8).
    if minor == 0 {
        return Err(Error::coded(
            ec::VERSION_UNSUPPORTED,
            format!("minor {minor} is not supported"),
        ));
    }
    let minor = minor.min(icu_proto::MINOR);

    // `Hello.name` is obsolete: the name lives in the certificate, and the
    // store mirrors the CN of the newest one (§6.4.2).
    let _ = name;
    let (stored_name, is_admin) = {
        let store = server.store.lock().await;
        let stored_name = store
            .user(identity.user_id)
            .map(|u| u.name.clone())
            .ok_or_else(|| {
                Error::coded(
                    ec::AUTH_UNKNOWN_CERT,
                    format!("unknown user_id {}", identity.user_id),
                )
            })?;
        let is_admin = store.is_admin(identity.user_id);
        (stored_name, is_admin)
    };

    // 3. Join, `Welcome`, `UserJoined`.
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let min_client = server.config.server.min_client.clone();
    let welcome = |session, flags, users| ControlMsg::Welcome {
        minor,
        caps: icu_proto::CAPS,
        you: UserInfo {
            user_id: identity.user_id,
            session,
            name: stored_name.clone(),
            flags,
        },
        audio: AudioParams::default(),
        users,
        min_client,
        is_admin,
    };
    let session = match server.room.join(
        identity.user_id,
        stored_name.clone(),
        initial,
        is_admin,
        conn.clone(),
        out_tx,
        welcome,
    ) {
        Ok(session) => session,
        Err(JoinError::Full) => {
            return Err(Error::coded(ec::SERVER_FULL, "room is full"));
        }
        Err(JoinError::SessionSpaceExhausted) => {
            return Err(Error::coded(ec::SERVER_FULL, "session id space exhausted"));
        }
    };

    let writer = tokio::spawn(writer_task(send, out_rx));
    tracing::info!(
        session,
        user_id = identity.user_id,
        name = %stored_name,
        remote = %conn.remote_address(),
        client = %client,
        "joined"
    );

    // 4. Datagram and uni-stream readers alongside the control loop.
    let datagrams = tokio::spawn(datagram_task(server.clone(), conn.clone(), session));
    let uni = tokio::spawn(uni_task(server.clone(), conn.clone(), session));

    let outcome = control_loop(server, conn, session, identity, &mut recv).await;

    // 6. Leave and announce.
    datagrams.abort();
    uni.abort();
    writer.abort();
    let member = server.room.leave(session);
    let hints = server
        .loss
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .forget(session);
    for (speaker, loss_permille) in hints {
        server
            .room
            .send_to(speaker, &ControlMsg::LossHint { loss_permille });
    }
    server
        .keyframes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&session);

    // `UserLeft.reason` (§6.4.6): the code the server closed with if it was the
    // server, otherwise the peer's application close code, otherwise TIMEOUT
    // for an idle timeout or a transport abort.
    let reason = match &outcome {
        Err(e) => e.code().unwrap_or(ec::TIMEOUT),
        Ok(()) => member
            .and_then(|m| m.close_code)
            .unwrap_or_else(|| peer_close_code(conn)),
    };
    server.room.broadcast(&ControlMsg::UserLeft {
        session,
        reason: reason as u8,
    });
    tracing::info!(session, reason = ec::name(reason), "left");
    outcome
}

/// Drains the outgoing control queue onto the control stream.
async fn writer_task(mut send: quinn::SendStream, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(frame) = rx.recv().await {
        if send.write_all(&frame).await.is_err() {
            return;
        }
    }
    let _ = send.finish();
}

/// Reads audio datagrams and hands them to the relay. The receiver buffer
/// lives here so that the relay allocates nothing per datagram.
async fn datagram_task(server: Arc<Server>, conn: quinn::Connection, session: Session) {
    let mut bucket = TokenBucket::per_second(server.config.limits.audio_datagrams_per_s);
    let mut receivers = Vec::with_capacity(usize::from(server.config.server.max_users));
    loop {
        match conn.read_datagram().await {
            Ok(datagram) => {
                relay::audio(&server, session, datagram, &mut bucket, &mut receivers);
                // Keep the capacity, hold no peer connection across the wait
                // for the next datagram.
                receivers.clear();
            }
            Err(_) => return,
        }
    }
}

/// Accepts one unidirectional stream per video frame.
async fn uni_task(server: Arc<Server>, conn: quinn::Connection, session: Session) {
    let state = Arc::new(Mutex::new(VideoSender::new(&server.config.limits)));
    loop {
        match conn.accept_uni().await {
            Ok(recv) => {
                tokio::spawn(relay::video(server.clone(), session, recv, state.clone()));
            }
            Err(_) => return,
        }
    }
}

/// Reads and dispatches control messages until the stream ends.
async fn control_loop(
    server: &Arc<Server>,
    conn: &quinn::Connection,
    session: Session,
    identity: &Identity,
    recv: &mut quinn::RecvStream,
) -> Result<()> {
    let user_id = identity.user_id;
    let mut control = TokenBucket::per_second(server.config.limits.control_msgs_per_s);
    let mut set_state = TokenBucket::per_second(server.config.limits.set_state_per_s);
    let mut violations = ViolationCounter::new(
        pl::RATE_VIOLATIONS_FATAL,
        server.config.timing.rate_violation_window(),
    );

    loop {
        let body = tokio::select! {
            body = wire::read_frame(recv, MAX_CONTROL_FRAME) => body,
            _ = conn.closed() => return Ok(()),
            _ = server.shutdown_requested() => {
                return Err(Error::coded(ec::SHUTDOWN, "server is shutting down"));
            }
        };
        let body = match body {
            Ok(body) => body,
            Err(e) if e.code() == Some(ec::PROTOCOL_VIOLATION) => return Err(e),
            // A closed or reset stream is a normal end of connection.
            Err(_) => return Ok(()),
        };

        Counters::inc(&server.counters.control_in);
        if !control.take_one() {
            if rate_limited(server, session, &mut violations, "control rate exceeded") {
                return Err(Error::coded(
                    ec::RATE_LIMITED_FATAL,
                    "third rate limit violation within the window",
                ));
            }
            continue;
        }

        let msg = ControlMsg::decode(&body)
            .map_err(|e| Error::coded(ec::PROTOCOL_VIOLATION, e.to_string()))?;
        if !msg.is_client_to_server() {
            return Err(Error::coded(
                ec::PROTOCOL_VIOLATION,
                format!("{} is a server to client message", msg.variant()),
            ));
        }

        match msg {
            ControlMsg::Hello { .. } => {
                return Err(Error::coded(ec::PROTOCOL_VIOLATION, "a second Hello"));
            }
            ControlMsg::SetState { flags } => {
                if !set_state.take_one() {
                    if rate_limited(server, session, &mut violations, "SetState rate exceeded") {
                        return Err(Error::coded(
                            ec::RATE_LIMITED_FATAL,
                            "third rate limit violation within the window",
                        ));
                    }
                    continue;
                }
                if let Some(flags) = server.room.apply_client_state(session, flags) {
                    server
                        .room
                        .broadcast(&ControlMsg::UserState { session, flags });
                }
            }
            ControlMsg::Ping { nonce } => {
                server.room.send_to(
                    session,
                    &ControlMsg::Pong {
                        nonce,
                        server_time_ms: unix_millis(),
                    },
                );
            }
            ControlMsg::LossReport {
                speaker,
                loss_permille,
            } => {
                let ttl = server.config.timing.loss_report_ttl();
                let hint = server
                    .loss
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .report(speaker, session, loss_permille, ttl);
                if let Some(loss_permille) = hint {
                    server
                        .room
                        .send_to(speaker, &ControlMsg::LossHint { loss_permille });
                }
            }
            ControlMsg::KeyframeRequest { speaker } => {
                relay::keyframe_request(server, speaker);
            }
            ControlMsg::RenewRequest { csr_der } => {
                handle_renew(server, session, identity, &csr_der).await;
            }
            ControlMsg::CreateInviteRequest { name, ttl_s } => {
                handle_create_invite(server, session, user_id, &name, ttl_s).await;
            }
            ControlMsg::AdminSetRole {
                user_id: target,
                is_admin,
            } => {
                handle_admin_set_role(server, session, user_id, target, is_admin).await;
            }
            ControlMsg::AdminSetDisabled {
                user_id: target,
                disabled,
            } => {
                handle_admin_set_disabled(server, session, user_id, target, disabled).await;
            }
            ControlMsg::AdminRevokeUser { user_id: target } => {
                handle_admin_revoke_user(server, session, user_id, target).await;
            }
            ControlMsg::UserListRequest => {
                handle_user_list_request(server, session, user_id).await;
            }
            other => {
                return Err(Error::coded(
                    ec::PROTOCOL_VIOLATION,
                    format!("unexpected {}", other.variant()),
                ));
            }
        }
    }
}

/// Sends `Error(RATE_LIMITED)` and reports whether this was the fatal third
/// violation (§6.6).
fn rate_limited(
    server: &Arc<Server>,
    session: Session,
    violations: &mut ViolationCounter,
    detail: &str,
) -> bool {
    Counters::inc(&server.counters.control_rate_limited);
    server.room.send_to(
        session,
        &ControlMsg::Error {
            code: ec::RATE_LIMITED,
            detail: detail.to_string(),
        },
    );
    violations.record()
}

/// Certificate renewal, and the only way a name changes (`PROTOCOL.md` §2.2).
///
/// The CSR's subject CN is the name the client asks for. Three cases, told
/// apart by exact string comparison against the stored name — case-insensitive
/// comparison would misfile a pure case change as a plain renewal:
///
/// * CN == stored, presented certificate current: plain renewal, one per day.
/// * CN == stored, presented certificate stale: catch-up after a rename on
///   another device. Exempt from the daily limit, but bounded per certificate
///   so one stale session cannot mint certificates in a loop.
/// * CN != stored: rename. Costs the daily budget and needs the name to be
///   free server-wide.
async fn handle_renew(
    server: &Arc<Server>,
    session: Session,
    identity: &Identity,
    csr_der: &[u8],
) {
    let reject = |code: u32, detail: String| ControlMsg::Error { code, detail };
    let user_id = identity.user_id;

    let requested = match ca::csr_common_name(csr_der) {
        Ok(cn) => cn,
        Err(e) => {
            server
                .room
                .send_to(session, &reject(ec::RENEW_REJECTED, e.to_string()));
            return;
        }
    };

    let mut store = server.store.lock().await;
    let Some(user) = store.user(user_id) else {
        server
            .room
            .send_to(session, &reject(ec::RENEW_REJECTED, "unknown user".into()));
        return;
    };
    let stored = user.name.clone();
    let rename = requested != stored;

    // Uniqueness before the rate check, so a taken name reports NAME_TAKEN
    // instead of a confusing rate message.
    if rename && store.name_taken(&requested, Some(user_id)) {
        server.room.send_to(
            session,
            &reject(ec::NAME_TAKEN, format!("name {requested:?} is already taken")),
        );
        return;
    }

    let catch_up = !rename && identity.cert_cn != stored;
    if catch_up {
        if let Some(last) = store.cert_renewed(user_id, &identity.cert_fp) {
            if now() - last < time::Duration::days(1) {
                server.room.send_to(
                    session,
                    &reject(
                        ec::RENEW_REJECTED,
                        "this certificate already caught up today".into(),
                    ),
                );
                return;
            }
        }
    } else if let Some(last) = store.last_renew(user_id) {
        if now() - last < time::Duration::days(1) / pl::RENEW_PER_DAY {
            server.room.send_to(
                session,
                &reject(ec::RENEW_REJECTED, "at most one renewal per day".into()),
            );
            return;
        }
    }

    let signed = {
        let ca = server.ca.read().await;
        ca.sign_client_csr(csr_der, server.config.tls.client_validity_days)
    };
    let (leaf, cn) = match signed {
        Ok(signed) => signed,
        Err(e) => {
            server
                .room
                .send_to(session, &reject(ec::RENEW_REJECTED, e.to_string()));
            return;
        }
    };

    let issued = now();
    let cert = CertRecord {
        fingerprint: fingerprint(&leaf),
        cn: cn.clone(),
        issued: rfc3339(issued),
        expires: rfc3339(
            issued + Duration::from_secs(u64::from(server.config.tls.client_validity_days) * 86400),
        ),
        revoked: false,
        renewed: None,
    };
    // Only a catch-up stamps the presented certificate: the same certificate
    // may be on a second device (passport) that still has to catch up itself.
    let supersedes = catch_up.then_some(identity.cert_fp.as_str());
    let renamed = match store.add_cert(user_id, cert, supersedes, !catch_up) {
        Ok(renamed) => renamed,
        Err(e) => {
            let code = e.code().unwrap_or(ec::RENEW_REJECTED);
            server.room.send_to(session, &reject(code, e.to_string()));
            return;
        }
    };
    drop(store);

    tracing::info!(session, user_id, name = %cn, renamed, catch_up, "certificate renewed");
    server.room.send_to(
        session,
        &ControlMsg::RenewResponse {
            leaf_der: leaf.to_vec(),
        },
    );
    if renamed {
        for s in server.room.sessions_of_user(user_id) {
            server.room.rename(s, &cn);
            server.room.broadcast(&ControlMsg::UserRenamed {
                session: s,
                name: cn.clone(),
            });
        }
    }
}

/// Mints an invite and returns it as a self-contained blob (`CreateInviteRequest`,
/// `PROTOCOL.md` §2.3.1). Admin only; rejected non-fatally otherwise.
async fn handle_create_invite(
    server: &Arc<Server>,
    session: Session,
    user_id: UserId,
    name: &str,
    ttl_s: u64,
) {
    let reject = |code: u32, detail: String| ControlMsg::Error { code, detail };

    let mut store = server.store.lock().await;
    if !store.is_admin(user_id) {
        server
            .room
            .send_to(session, &reject(ec::NOT_ADMIN, "not an admin".into()));
        return;
    }
    let ttl = Duration::from_secs(if ttl_s == 0 { pl::INVITE_TTL_S } else { ttl_s });
    // The label binds nothing (the invited user picks their own name), so the
    // only failure here is a malformed label or a failed write. `NAME_TAKEN` is
    // the non-fatal code the client already shows next to the invite field.
    let (token, record) = match store.create_invite(name, ttl) {
        Ok(v) => v,
        Err(e) => {
            server.room.send_to(
                session,
                &reject(e.code().unwrap_or(ec::NAME_TAKEN), e.to_string()),
            );
            return;
        }
    };
    drop(store);

    let ca_der = server.ca.read().await.ca_der().to_vec();
    let aad_blob = icu_proto::invite_blob::InviteBlob {
        host_port: server.config.public_host(),
        server_name: server.config.server.server_name.clone(),
        ca_der,
        expires: record.expires.clone(),
        nonce: [0u8; 12],
        sealed: Vec::new(),
    };
    let aad = aad_blob.aad();
    let (nonce, sealed) = server.invite_key.seal(&token, &aad);
    let blob = icu_proto::invite_blob::InviteBlob {
        nonce,
        sealed,
        ..aad_blob
    };

    tracing::info!(user_id, name, "invite blob minted");
    server.room.send_to(
        session,
        &ControlMsg::CreateInviteResponse {
            blob: blob.encode(),
            expires: record.expires,
        },
    );
}

/// Shared body of the in-band admin actions (`PROTOCOL.md` §2.2).
///
/// All three have the same shape: under the store lock, check that the actor
/// is still an admin and that the target exists, then apply `change`. Only on
/// success does `done` run, and it runs **outside** the lock, because it kicks
/// connections and sends control frames. Any failure answers with
/// `AdminActionError` and changes nothing.
async fn admin_action(
    server: &Arc<Server>,
    session: Session,
    actor: UserId,
    target: UserId,
    change: impl FnOnce(&mut Store) -> Result<bool>,
    done: impl FnOnce() -> ControlMsg,
) {
    let outcome = {
        let mut store = server.store.lock().await;
        if !store.is_admin(actor) {
            Err(Error::coded(ec::NOT_ADMIN, "not an admin"))
        } else if store.user(target).is_none() {
            Err(Error::coded(ec::NO_SUCH_USER, "unknown user"))
        } else {
            change(&mut store)
        }
    };
    let answer = match outcome {
        Ok(_) => done(),
        Err(e) => ControlMsg::AdminActionError {
            user_id: target,
            code: e.code().unwrap_or(ec::LAST_ADMIN),
            detail: e.to_string(),
        },
    };
    server.room.send_to(session, &answer);
}

/// `AdminSetRole`: `UserAdminChanged` goes to every one of the target's own
/// sessions, so their cached `is_admin` updates without a reconnect; the
/// `UserState` broadcast that follows is what puts the admin badge live on
/// everybody else's roster.
async fn handle_admin_set_role(
    server: &Arc<Server>,
    session: Session,
    actor: UserId,
    target: UserId,
    is_admin: bool,
) {
    admin_action(
        server,
        session,
        actor,
        target,
        |store| store.set_admin(target, is_admin),
        || {
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
            tracing::info!(actor, target, is_admin, "admin role changed");
            ControlMsg::AdminSetRoleOk {
                user_id: target,
                is_admin,
            }
        },
    )
    .await
}

/// `AdminSetDisabled`: disabling an online user kicks it immediately with
/// `AUTH_DISABLED` — the close itself is the notification, no separate push.
async fn handle_admin_set_disabled(
    server: &Arc<Server>,
    session: Session,
    actor: UserId,
    target: UserId,
    disabled: bool,
) {
    admin_action(
        server,
        session,
        actor,
        target,
        |store| store.set_disabled(target, disabled),
        || {
            if disabled {
                kick_user(server, target, ec::AUTH_DISABLED);
            }
            tracing::info!(actor, target, disabled, "admin disabled state changed");
            ControlMsg::AdminSetDisabledOk {
                user_id: target,
                disabled,
            }
        },
    )
    .await
}

/// `AdminRevokeUser`: revokes every certificate of `target` and kicks any live
/// connection with `KICKED`.
async fn handle_admin_revoke_user(
    server: &Arc<Server>,
    session: Session,
    actor: UserId,
    target: UserId,
) {
    admin_action(
        server,
        session,
        actor,
        target,
        |store| store.revoke(target),
        || {
            let closed = kick_user(server, target, ec::KICKED) as u32;
            tracing::info!(actor, target, closed, "admin revoked user");
            ControlMsg::AdminRevokeUserOk {
                user_id: target,
                closed,
            }
        },
    )
    .await
}

/// `UserListRequest` (`PROTOCOL.md` §2.2). Admin only; every registered user,
/// online or not.
async fn handle_user_list_request(server: &Arc<Server>, session: Session, actor: UserId) {
    let store = server.store.lock().await;
    if !store.is_admin(actor) {
        drop(store);
        // No target to name, so `user_id` stays 0.
        server.room.send_to(
            session,
            &ControlMsg::AdminActionError {
                user_id: 0,
                code: ec::NOT_ADMIN,
                detail: "not an admin".into(),
            },
        );
        return;
    }
    let users = store
        .users()
        .map(|(id, u)| icu_proto::UserSummary {
            user_id: id,
            name: u.name.clone(),
            created: u.created.clone(),
            is_admin: u.is_admin,
            disabled: u.disabled,
            revoked: u.is_revoked(),
            online: !server.room.sessions_of_user(id).is_empty(),
        })
        .collect();
    drop(store);
    server
        .room
        .send_to(session, &ControlMsg::UserListResponse { users });
}

/// The code the peer closed with, reduced to the `UserLeft.reason` range.
///
/// Anything that is not a clean application close — idle timeout, reset,
/// transport error — is `TIMEOUT` (`PROTOCOL.md` §6).
fn peer_close_code(conn: &quinn::Connection) -> u32 {
    match conn.close_reason() {
        Some(quinn::ConnectionError::ApplicationClosed(closed)) => {
            let code = u64::from(closed.error_code);
            if code < 256 {
                code as u32
            } else {
                ec::TIMEOUT
            }
        }
        _ => ec::TIMEOUT,
    }
}

/// Closes every connection of a user with `code` (`revoke`/`disable`, §5).
pub fn kick_user(server: &Arc<Server>, user_id: UserId, code: u32) -> usize {
    let sessions = server.room.sessions_of_user(user_id);
    let mut closed = 0;
    for session in sessions {
        if let Some(member) = server.room.member(session) {
            // Mark before closing, so the `UserLeft` broadcast says the same
            // code and not "the peer went away".
            server.room.mark_closing(session, code);
            member
                .conn
                .close(quinn::VarInt::from_u32(code), ec::name(code).as_bytes());
            closed += 1;
        }
    }
    closed
}
