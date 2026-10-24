//! Per-client state machine on ALPN `icu/1`
//! (`docs/reference/server.md` §6.4).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::Bytes;
use icu_proto::{
    error as ec, limits as pl, name_key, AudioParams, ControlMsg, Rgb, Session, UserId, UserInfo,
    MAX_CONTROL_FRAME,
};
use rustls::pki_types::CertificateDer;
use tokio::sync::mpsc;

use crate::{
    admin, ca,
    error::{Context, Error, Result},
    limits::{TokenBucket, ViolationCounter},
    lock,
    relay::{self, VideoBudget, VideoSender},
    room::{Counters, JoinError},
    store::{fingerprint, now, parse_rfc3339, rfc3339, unix_millis, CertRecord, Store, UserRecord},
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
            // An error without a code is the server's own trouble, not the
            // client's: close with a code it retries on, not a fatal one.
            let code = e.code().unwrap_or(ec::SHUTDOWN);
            // Anyone on the internet can complete a handshake without a
            // certificate; logging each one at `info` would let scanners
            // fill the journal.
            if code == ec::AUTH_REQUIRED {
                tracing::debug!(%remote, error = %e, "rejected");
            } else if e.code().is_none() {
                tracing::warn!(%remote, error = %e, "cannot authenticate");
            } else {
                tracing::info!(%remote, error = %e, "rejected");
            }
            wire::close(&conn, code);
            return Err(e);
        }
    };

    let roomless = identity.registration || identity.expired;
    if roomless {
        // Registration and renew-only sessions read nothing but their control
        // stream, whose credit the handshake already granted. Streams nobody
        // accepts are buffered by quinn: no credit for any further one.
        conn.set_max_concurrent_uni_streams(quinn::VarInt::from_u32(0));
        conn.set_max_concurrent_bi_streams(quinn::VarInt::from_u32(0));
    }

    // One live connection per certificate: this one wins (`docs/reference/protocol.md` §7).
    // A renew-only session has a table of its own: it must not kick a room
    // session still running on the same certificate.
    let slots = if roomless {
        Slots::Roomless
    } else {
        Slots::Room
    };
    // A registration pays its budget once there is room for it and before it
    // takes the slot. Turned away as full — not fatal, the client comes back
    // — it has spent nothing, or its retries would end in the fatal
    // `ENROLL_RATE_LIMITED`; refused by the budget, it has not first closed
    // a registration of the same bundle that is in progress.
    let charge = || {
        if identity.registration {
            enroll_budget(&server, &conn)
        } else {
            Ok(())
        }
    };
    let _live = match LiveCert::claim(&server, slots, &identity, &conn, charge) {
        Ok(live) => live,
        Err(e) => {
            tracing::info!(%remote, user_id = identity.user_id, error = %e, "rejected");
            wire::close(&conn, e.code().unwrap_or(ec::SERVER_FULL));
            return Err(e);
        }
    };

    let served = if identity.registration {
        let exchange = |deadline| registration_exchange(&server, &conn, &identity, deadline);
        renew_only(exchange, "registration took too long").await
    } else if identity.expired {
        let exchange = |deadline| recovery_exchange(&server, &conn, &identity, deadline);
        renew_only(exchange, "renewal took too long").await
    } else {
        serve(&server, &conn, &identity).await
    };
    match served {
        Ok(()) => Ok(()),
        Err(e) => {
            let code = e.code().unwrap_or(ec::PROTOCOL_VIOLATION);
            tracing::info!(%remote, user_id = identity.user_id, error = %e, "closing");
            wire::close(&conn, code);
            Err(e)
        }
    }
}

/// The table a connection's certificate slot is kept in.
#[derive(Debug, Clone, Copy)]
enum Slots {
    /// Room sessions, and those still waiting for `Hello`
    /// ([`Server::live_certs`]).
    Room,
    /// Registration and renew-only sessions ([`Server::roomless`]), capped
    /// at [`pl::MAX_ROOMLESS_SESSIONS`].
    Roomless,
}

impl Slots {
    fn table(self, server: &Server) -> &Mutex<crate::CertSlots> {
        match self {
            Slots::Room => &server.live_certs,
            Slots::Roomless => &server.roomless,
        }
    }
}

/// Holds a certificate's slot for as long as its connection lives. A newer
/// connection with the same certificate takes the slot over and closes this
/// one with `SESSION_REPLACED`: newest wins, so a client whose old path is
/// still waiting for its idle timeout is not locked out, and a stolen
/// passport in use shows up as a kicked owner.
struct LiveCert {
    server: Arc<Server>,
    slots: Slots,
    fp: String,
    id: usize,
}

impl LiveCert {
    /// Takes the slot of `identity`'s certificate for `conn`. `SERVER_FULL`
    /// when the roomless table is full and `conn` would not replace an entry
    /// in it; a replacement always gets in, the connection it replaces
    /// leaves. With room, `charge` runs first, under the table's lock, and
    /// its refusal is returned as it is.
    fn claim(
        server: &Arc<Server>,
        slots: Slots,
        identity: &Identity,
        conn: &quinn::Connection,
        charge: impl FnOnce() -> Result<()>,
    ) -> Result<LiveCert> {
        let fp = identity.cert_fp.clone();
        let old = {
            let mut table = lock(slots.table(server));
            if matches!(slots, Slots::Roomless)
                && table.len() >= pl::MAX_ROOMLESS_SESSIONS
                && !table.contains_key(&fp)
            {
                return Err(Error::coded(
                    ec::SERVER_FULL,
                    "too many registration and renew-only sessions",
                ));
            }
            // Lock order: the slot table, then `ip_limits` in `charge`;
            // nothing takes them the other way round.
            charge()?;
            table.insert(fp.clone(), (identity.user_id, conn.clone()))
        };
        if let Some((_, old)) = old.filter(|(_, old)| old.stable_id() != conn.stable_id()) {
            close_connection(server, &old, ec::SESSION_REPLACED);
            tracing::info!(
                fingerprint = %fp,
                old = %old.remote_address(),
                new = %conn.remote_address(),
                "certificate connected again, older connection replaced"
            );
        }
        Ok(LiveCert {
            server: server.clone(),
            slots,
            fp,
            id: conn.stable_id(),
        })
    }
}

impl Drop for LiveCert {
    fn drop(&mut self) {
        let mut table = lock(self.slots.table(&self.server));
        // Only our own entry: a replacement already sits there otherwise.
        if table
            .get(&self.fp)
            .is_some_and(|(_, c)| c.stable_id() == self.id)
        {
            table.remove(&self.fp);
        }
    }
}

/// Closes `conn` with `code`: through the room when it is in there, so
/// `UserLeft` carries the code. A session still before `room.join` sees its
/// closed connection right after joining and leaves again.
fn close_connection(server: &Server, conn: &quinn::Connection, code: u32) {
    if !server.room.close_connection(conn.stable_id(), code) {
        wire::close(conn, code);
    }
}

/// Closes every connection of `user_id` with `code` and returns how many:
/// its room sessions — through the room, so `UserLeft` carries the code —
/// and every other connection on one of its certificates, a session still
/// waiting for `Hello`, a registration or a renew-only session. Behind
/// revoke, disable and kick.
pub(crate) fn close_user(server: &Server, user_id: UserId, code: u32) -> usize {
    let mut closed: std::collections::HashSet<usize> =
        server.room.close_user(user_id, code).into_iter().collect();
    let others: Vec<quinn::Connection> = [Slots::Room, Slots::Roomless]
        .into_iter()
        .flat_map(|slots| {
            lock(slots.table(server))
                .values()
                .filter(|(owner, _)| *owner == user_id)
                .map(|(_, conn)| conn.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    for conn in others {
        // One that already ended — a registration session lingering for its
        // store write after the client closed — is not closed again.
        if conn.close_reason().is_none() && closed.insert(conn.stable_id()) {
            wire::close(&conn, code);
        }
    }
    closed.len()
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
    /// name, or has to catch up (`docs/reference/protocol.md` §2.2).
    cert_cn: String,
    /// Authenticated with a registration certificate (`docs/reference/protocol.md` §2.3).
    registration: bool,
    /// SubjectPublicKeyInfo of the presented leaf. A registration has to
    /// bring a different key.
    cert_spki: Vec<u8>,
    /// The presented leaf has expired: a renew-only session
    /// (`docs/reference/protocol.md` §2.3).
    expired: bool,
    /// notBefore and notAfter of the presented leaf, `None` when it does not
    /// parse. A re-issue outside the renewal window carries them over.
    cert_validity: Option<(time::OffsetDateTime, time::OffsetDateTime)>,
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
    let info = ca::leaf_info(leaf);

    // This lookup is the admission, not a second line behind the CRL: on a
    // TLS 1.3 resumption rustls does not run the client verifier again, so
    // the CRL is not checked there. Never drop it as redundant.
    let mut store = server.store.lock().await;
    let (user_id, user, cert) = admit(&store, &fp)?;
    if name_key(&info.cn) != name_key(&user.name) {
        // Not a reason to reject: the store keeps fingerprints, not DER, so
        // certificates issued before the name moved into the certificate have
        // no recorded CN at all. The client notices the same mismatch and
        // renews itself (`docs/reference/protocol.md` §2.2).
        tracing::debug!(
            user_id,
            cert_cn = %info.cn,
            stored = %user.name,
            "stale cn, client will catch up"
        );
    }
    let registration = cert.registration;
    let expired = cert_expired(info.validity, now());
    // The handshake forgives expiry only for renewing a real certificate; a
    // registration certificate that ran out is spent (`docs/reference/protocol.md` §2.3).
    if registration && expired {
        return Err(Error::coded(
            ec::AUTH_UNKNOWN_CERT,
            format!("registration certificate {fp} has expired"),
        ));
    }
    // The first login with a real certificate proves the client stored it,
    // whether or not the registration session saw the clean close. A failed
    // write must not cost the login: the bundle just stays valid a little
    // longer, as before the receipt, and the next login tries again.
    let consumed = !registration
        && !expired
        && store.consume_registration(user_id).unwrap_or_else(|e| {
            tracing::warn!(user_id, error = %e, "cannot consume the registration");
            false
        });
    // Likewise for a renewal: the first login with the new certificate
    // proves it was stored, and everything it replaced is revoked
    // (`docs/reference/protocol.md` §2.2 Nr. 10). Not on a renew-only session, which proves
    // nothing about a successor. A failed write is retried on the next login.
    let retired = if registration || expired {
        Vec::new()
    } else {
        store.retire_predecessors(user_id, &fp).unwrap_or_else(|e| {
            tracing::warn!(user_id, error = %e, "cannot revoke the replaced certificates");
            Vec::new()
        })
    };
    drop(store);
    if !retired.is_empty() {
        tracing::info!(
            user_id,
            count = retired.len(),
            "replaced certificates revoked"
        );
        close_retired(server, &retired);
    }
    if consumed || !retired.is_empty() {
        crate::endpoint::reload_tls_logged(server).await;
    }
    Ok(Identity {
        user_id,
        cert_fp: fp,
        cert_cn: info.cn,
        registration,
        cert_spki: info.spki,
        expired,
        cert_validity: info.validity,
    })
}

/// Closes live connections still running on certificates that were just
/// revoked by their successor's login — a second device holding a copy, a
/// renew-only session on an expired copy, or a connection this client
/// already left.
fn close_retired(server: &Arc<Server>, fingerprints: &[String]) {
    let live: Vec<quinn::Connection> = [Slots::Room, Slots::Roomless]
        .into_iter()
        .flat_map(|slots| {
            let table = lock(slots.table(server));
            fingerprints
                .iter()
                .filter_map(|fp| table.get(fp).map(|(_, conn)| conn.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    for conn in live {
        close_connection(server, &conn, ec::AUTH_REVOKED);
    }
}

/// The store's admission of a presented certificate: known, not revoked, its
/// user not disabled. Run once in [`authenticate`] and again in [`serve`].
pub(crate) fn admit<'a>(
    store: &'a Store,
    fp: &str,
) -> Result<(UserId, &'a UserRecord, &'a CertRecord)> {
    let (user_id, user, cert) = store
        .by_fingerprint(fp)
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
    Ok((user_id, user, cert))
}

/// What `Hello` said, plus the control stream it arrived on.
struct Hello {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    minor: u16,
    caps: u32,
    client: String,
    initial: icu_proto::UserFlags,
}

/// Accepts the control stream and reads `Hello` within the timeout.
async fn read_hello(server: &Arc<Server>, conn: &quinn::Connection) -> Result<Hello> {
    // 2. Control stream and `Hello` within the timeout (§6.4.2) — one
    //    deadline for both, not one each, or a client could take twice as
    //    long as the spec allows.
    let deadline = tokio::time::Instant::now() + server.config.timing.hello_timeout();
    let (send, mut recv) = tokio::time::timeout_at(deadline, conn.accept_bi())
        .await
        .map_err(|_| Error::coded(ec::HELLO_TIMEOUT, "no control stream within the timeout"))?
        .map_err(|e| Error::coded(ec::HELLO_TIMEOUT, e.to_string()))?;
    let _ = send.set_priority(1); // control beats video (`docs/reference/protocol.md` §4)

    let body = tokio::time::timeout_at(deadline, wire::read_frame(&mut recv, MAX_CONTROL_FRAME))
        .await
        .map_err(|_| Error::coded(ec::HELLO_TIMEOUT, "no Hello within the timeout"))??;

    let hello = ControlMsg::decode(&body).context("bad Hello")?;
    let ControlMsg::Hello {
        minor,
        caps,
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
    // Both minimums (`docs/reference/protocol.md` §8): the protocol minor, then the client
    // version against `server.min_client`. A client string without a readable
    // version cannot prove it is new enough.
    if minor < icu_proto::MIN_PEER_MINOR {
        return Err(Error::coded(
            ec::VERSION_UNSUPPORTED,
            format!(
                "protocol minor {minor} is below the minimum {}",
                icu_proto::MIN_PEER_MINOR
            ),
        ));
    }
    let min_client = &server.config.server.min_client;
    match (
        icu_proto::client_version(&client),
        icu_proto::Version::parse(min_client),
    ) {
        (Ok(version), Ok(min)) if version >= min => {}
        (Ok(version), _) => {
            return Err(Error::coded(
                ec::VERSION_UNSUPPORTED,
                format!("client {version} is below min_client {min_client}"),
            ));
        }
        (Err(_), _) => {
            return Err(Error::coded(
                ec::VERSION_UNSUPPORTED,
                format!("Hello.client {client:?} carries no version"),
            ));
        }
    }
    let minor = minor.min(icu_proto::MINOR);

    // `Hello.name` is obsolete: the name lives in the certificate, and the
    // store mirrors the CN of the newest one (§6.4.2).
    let _ = name;
    Ok(Hello {
        send,
        recv,
        minor,
        caps,
        client,
        initial,
    })
}

async fn serve(server: &Arc<Server>, conn: &quinn::Connection, identity: &Identity) -> Result<()> {
    let Hello {
        send,
        mut recv,
        minor,
        caps,
        client,
        initial,
    } = read_hello(server, conn).await?;
    // The store lock is held from here across `room.join`. A revoke or
    // disable during the `Hello` wait happened after `authenticate` and
    // before this session is in the room, where `kick_user` cannot see it;
    // admin actions change the store under this lock and kick only after
    // releasing it, so either this check sees the change or the kick sees
    // the session.
    let store = server.store.lock().await;
    let (_, user, _) = admit(&store, &identity.cert_fp)?;
    let stored_name = user.name.clone();
    let is_admin = user.is_admin;
    let (forced_muted, forced_deafened) = (user.server_muted, user.server_deafened);
    let forced = restriction_flags(forced_muted, forced_deafened);
    let corona_color = user.corona_color;

    // 3. Join, `Welcome`, `UserJoined`.
    let (out_tx, out_rx) = mpsc::channel(crate::limits::CONTROL_QUEUE_FRAMES);
    let welcome = |session, flags, users| {
        let you = UserInfo {
            user_id: identity.user_id,
            session,
            name: stored_name.clone(),
            flags,
            corona_color,
        };
        welcome(server, minor, you, users, is_admin, false)
    };
    let session = match server.room.join(
        identity.user_id,
        stored_name.clone(),
        initial,
        is_admin,
        forced,
        corona_color,
        conn.clone(),
        out_tx,
        welcome,
    ) {
        Ok(session) => session,
        Err(JoinError::Full) => {
            return Err(Error::coded(ec::SERVER_FULL, "room is full"));
        }
    };
    // Still under the store lock, which every restriction change holds while
    // it pushes: an admin lifting the restriction right now reaches this
    // session after this push, not before it with a stale one following.
    if forced_muted || forced_deafened {
        server
            .room
            .send_to(session, &restriction_changed(forced_muted, forced_deafened));
    }
    lock(&server.audio_feedback).join(session, caps);
    drop(store);
    // The client checks the server leaf against the newest CRL it holds;
    // this is how it gets one (`docs/reference/protocol.md` §2.2).
    let crl_der = server.crl.read().unwrap_or_else(|e| e.into_inner()).clone();
    if !crl_der.is_empty() {
        server
            .room
            .send_to(session, &ControlMsg::CrlUpdate { crl_der });
    }

    tracing::info!(
        session,
        user_id = identity.user_id,
        name = %stored_name,
        remote = %conn.remote_address(),
        client = %client,
        "joined"
    );

    // 4. The control writer, datagram and uni-stream readers alongside the
    // control loop, held by the seat: it takes the session out of the room
    // however this function ends.
    let mut seat = Seat {
        server,
        conn,
        session,
        tasks: vec![
            crate::spawn_logged(
                format!("session {session} control writer"),
                writer_task(send, out_rx),
            ),
            crate::spawn_logged(
                format!("session {session} audio"),
                datagram_task(server.clone(), conn.clone(), session),
            ),
            crate::spawn_logged(
                format!("session {session} video"),
                uni_task(server.clone(), conn.clone(), session, identity.user_id),
            ),
        ],
        left: false,
    };

    let outcome = control_loop(server, conn, session, identity, &mut recv).await;

    // 6. Leave and announce.
    seat.abort_tasks();
    let member = server.room.leave(session);
    seat.left = true;
    let listeners = |speaker| server.room.audio_listeners(speaker);
    lock(&server.audio_feedback).forget(session);
    for (speaker, loss_permille) in lock(&server.loss).forget(session, listeners) {
        server
            .room
            .send_to(speaker, &ControlMsg::LossHint { loss_permille });
    }

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

/// A session's place in the room and the tasks serving it. On the normal
/// way out `serve` leaves the room itself and this only stops the tasks.
/// Dropped while unwinding from a panic, it closes the connection with the
/// non-fatal `SHUTDOWN` — the client reconnects — takes the session out of
/// the room and announces it: a bug in one session must not leave a member
/// behind that holds a room slot, with its tasks running, until the next
/// restart.
struct Seat<'a> {
    server: &'a Arc<Server>,
    conn: &'a quinn::Connection,
    session: Session,
    tasks: Vec<tokio::task::AbortHandle>,
    left: bool,
}

impl Seat<'_> {
    fn abort_tasks(&self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Drop for Seat<'_> {
    fn drop(&mut self) {
        self.abort_tasks();
        if self.left {
            return;
        }
        wire::close(self.conn, ec::SHUTDOWN);
        if self.server.room.leave(self.session).is_some() {
            let listeners = |speaker| self.server.room.audio_listeners(speaker);
            lock(&self.server.loss).forget(self.session, listeners);
            lock(&self.server.audio_feedback).forget(self.session);
            self.server.room.broadcast(&ControlMsg::UserLeft {
                session: self.session,
                reason: ec::SHUTDOWN as u8,
            });
        }
        // A panic is a bug worth an error; a task cancelled at shutdown is
        // not.
        if std::thread::panicking() {
            tracing::error!(
                session = self.session,
                "session panicked, taken out of the room"
            );
        } else {
            tracing::debug!(session = self.session, "session cancelled, left the room");
        }
    }
}

/// How long a registration session may stay open in total. Long enough for a
/// user to retry a taken name a few times, short enough that a bundle cannot
/// park a connection.
const REGISTRATION_SESSION: Duration = Duration::from_secs(120);

/// How long a finished registration session waits for the client to close.
const REGISTRATION_LINGER: Duration = Duration::from_secs(5);

/// `RenewRequest`s one registration session may send before it is closed.
const REGISTRATION_ATTEMPTS: u32 = 10;

/// Runs a renew-only exchange — registration or recovery — under the session
/// cap. The cap covers the whole session, writes included: a client that
/// stops reading stalls a write on flow control, where no `select!` around a
/// read could end it.
async fn renew_only<F>(
    exchange: impl FnOnce(tokio::time::Instant) -> F,
    timed_out: &'static str,
) -> Result<()>
where
    F: std::future::Future<Output = Result<()>>,
{
    let deadline = tokio::time::Instant::now() + REGISTRATION_SESSION;
    tokio::time::timeout_at(deadline + REGISTRATION_LINGER, exchange(deadline))
        .await
        .unwrap_or_else(|_| Err(Error::coded(ec::TIMEOUT, timed_out)))
}

/// The start of a renew-only session: `Hello`, then a `Welcome` with no
/// session and no user list, written straight onto the stream (there is no
/// room queue). Returns the control stream.
async fn roomless_welcome(
    server: &Arc<Server>,
    conn: &quinn::Connection,
    identity: &Identity,
    registration: bool,
    what: &str,
) -> Result<(quinn::SendStream, quinn::RecvStream)> {
    let Hello {
        mut send,
        recv,
        minor,
        client,
        initial: _,
        caps: _,
    } = read_hello(server, conn).await?;
    let (name, is_admin, corona_color) = {
        let store = server.store.lock().await;
        let user = store
            .user(identity.user_id)
            .ok_or_else(|| Error::coded(ec::AUTH_UNKNOWN_CERT, "user vanished"))?;
        (user.name.clone(), user.is_admin, user.corona_color)
    };
    let you = UserInfo {
        user_id: identity.user_id,
        session: 0,
        name,
        flags: icu_proto::UserFlags::NONE,
        corona_color,
    };
    let welcome = welcome(server, minor, you, Vec::new(), is_admin, registration);
    write_msg(&mut send, &welcome).await?;
    tracing::info!(
        user_id = identity.user_id,
        remote = %conn.remote_address(),
        client = %client,
        "{what}"
    );
    Ok((send, recv))
}

/// One frame of a renew-only session. `None` once the client ended the
/// stream; an error past `deadline`, on shutdown, or for a bad frame.
async fn read_bounded(
    server: &Arc<Server>,
    recv: &mut quinn::RecvStream,
    deadline: tokio::time::Instant,
    timed_out: &'static str,
) -> Result<Option<Vec<u8>>> {
    let body = tokio::select! {
        body = wire::read_frame(recv, MAX_CONTROL_FRAME) => body,
        _ = tokio::time::sleep_until(deadline) => {
            return Err(Error::coded(ec::TIMEOUT, timed_out));
        }
        _ = server.shutdown_requested() => {
            return Err(Error::coded(ec::SHUTDOWN, "server is shutting down"));
        }
    };
    stream_end(body)
}

/// A closed or reset control stream is a normal end of connection (`None`);
/// only a bad frame, `PROTOCOL_VIOLATION`, is the client's fault.
fn stream_end(read: Result<Vec<u8>>) -> Result<Option<Vec<u8>>> {
    match read {
        Ok(body) => Ok(Some(body)),
        Err(e) if e.code() == Some(ec::PROTOCOL_VIOLATION) => Err(e),
        Err(_) => Ok(None),
    }
}

/// A session authenticated with an **expired** certificate
/// (`docs/reference/protocol.md` §2.3): renew-only. It never joins the room. After `Welcome`
/// (with no users) comes `Error{RENEW_REQUIRED}`, and the only useful message
/// is a `RenewRequest` under the stored name with the existing key; anything
/// else is answered with `RENEW_REQUIRED` again. After `RenewResponse` the
/// client stores the leaf, closes, and reconnects with it. Bounded in time and
/// attempts like a registration session.
async fn recovery_exchange(
    server: &Arc<Server>,
    conn: &quinn::Connection,
    identity: &Identity,
    deadline: tokio::time::Instant,
) -> Result<()> {
    let what = "renew-only session, certificate expired";
    let (mut send, mut recv) = roomless_welcome(server, conn, identity, false, what).await?;
    let required = ControlMsg::Error {
        code: ec::RENEW_REQUIRED,
        detail: "certificate expired, renew it".into(),
    };
    write_msg(&mut send, &required).await?;

    let mut pings = TokenBucket::per_second(server.config.limits.control_msgs_per_s);
    let mut attempts = 0u32;
    loop {
        let Some(body) = read_bounded(server, &mut recv, deadline, "renewal took too long").await?
        else {
            return Ok(());
        };
        match ControlMsg::decode(&body)? {
            ControlMsg::Ping { nonce } => {
                if pings.take_one() {
                    write_msg(&mut send, &pong(nonce)).await?;
                }
                continue;
            }
            ControlMsg::RenewRequest { csr_der } => {
                match renew_leaf(server, identity, &csr_der).await {
                    Ok(renewed) => {
                        let response = ControlMsg::RenewResponse {
                            leaf_der: renewed.leaf,
                        };
                        write_msg(&mut send, &response).await?;
                        let _ = send.finish();
                        // The client closes once it has stored the leaf; a close
                        // from here could overtake the response.
                        let _ = tokio::time::timeout(REGISTRATION_LINGER, conn.closed()).await;
                        return Ok(());
                    }
                    // The certificate lost its admission during the session
                    // (revoked, retired by a successor, user disabled): the
                    // session ends with that code.
                    Err((code, detail)) if ec::is_fatal(code) => {
                        return Err(Error::coded(code, detail));
                    }
                    Err((code, detail)) => {
                        write_msg(&mut send, &ControlMsg::Error { code, detail }).await?;
                    }
                }
            }
            _ => write_msg(&mut send, &required).await?,
        }
        attempts += 1;
        if attempts >= REGISTRATION_ATTEMPTS {
            return Err(Error::coded(
                ec::RATE_LIMITED_FATAL,
                "too many messages in a renew-only session",
            ));
        }
    }
}

/// Registration sessions: 5/min per IP, 20/min global (`docs/reference/protocol.md` §7).
fn enroll_budget(server: &Arc<Server>, conn: &quinn::Connection) -> Result<()> {
    let allowed = lock(&server.ip_limits).allow_enroll(conn.remote_address().ip());
    Counters::inc(&server.counters.enroll_attempts);
    if !allowed {
        Counters::inc(&server.counters.enroll_failed);
        return Err(Error::coded(
            ec::ENROLL_RATE_LIMITED,
            "too many registrations",
        ));
    }
    Ok(())
}

/// A session authenticated with a registration certificate
/// (`docs/reference/protocol.md` §2.3). It never joins the room: after `Welcome` with
/// `registration = true` the only useful message is a `RenewRequest` with a
/// fresh key. On success the answer is `RenewResponse`, the registration
/// certificate is revoked, and the client reconnects with its own.
async fn registration_exchange(
    server: &Arc<Server>,
    conn: &quinn::Connection,
    identity: &Identity,
    deadline: tokio::time::Instant,
) -> Result<()> {
    let (mut send, mut recv) =
        roomless_welcome(server, conn, identity, true, "registration session").await?;

    // Nothing else bounds `Ping` here; past the rate it goes unanswered.
    let mut pings = TokenBucket::per_second(server.config.limits.control_msgs_per_s);
    let mut attempts = 0u32;
    loop {
        let Some(body) =
            read_bounded(server, &mut recv, deadline, "registration took too long").await?
        else {
            return Ok(());
        };
        match ControlMsg::decode(&body)? {
            ControlMsg::Ping { nonce } => {
                if pings.take_one() {
                    write_msg(&mut send, &pong(nonce)).await?;
                }
            }
            ControlMsg::RenewRequest { csr_der } => {
                attempts += 1;
                match redeem(server, identity, &csr_der).await {
                    Ok(leaf_der) => {
                        Counters::inc(&server.counters.enroll_ok);
                        write_msg(&mut send, &ControlMsg::RenewResponse { leaf_der }).await?;
                        let _ = send.finish();
                        // The client closes with REGISTRATION_STORED once it
                        // has stored the certificate; a close from here could
                        // overtake the response. Anything else — another code,
                        // NO_ERROR (quinn's implicit close of a dropped
                        // connection), a timeout, a dropped path — leaves the
                        // bundle valid, so a lost answer, a failed save or a
                        // client that quit mid-way does not burn it. The first
                        // login with the new certificate consumes it as well.
                        let closed = tokio::time::timeout(REGISTRATION_LINGER, conn.closed()).await;
                        if matches!(
                            closed,
                            Ok(quinn::ConnectionError::ApplicationClosed(ref c))
                                if u64::from(c.error_code) == u64::from(ec::REGISTRATION_STORED)
                        ) {
                            let consumed = server
                                .store
                                .lock()
                                .await
                                .consume_registration(identity.user_id)?;
                            tracing::info!(user_id = identity.user_id, "registration confirmed");
                            if consumed {
                                crate::endpoint::reload_tls_logged(server).await;
                            }
                        }
                        return Ok(());
                    }
                    Err(e) => {
                        Counters::inc(&server.counters.enroll_failed);
                        let code = e.code().unwrap_or(ec::RENEW_REJECTED);
                        if ec::is_fatal(code) {
                            return Err(e);
                        }
                        let error = ControlMsg::Error {
                            code,
                            detail: e.to_string(),
                        };
                        write_msg(&mut send, &error).await?;
                        if attempts >= REGISTRATION_ATTEMPTS {
                            return Err(Error::coded(
                                ec::RATE_LIMITED_FATAL,
                                "too many registration attempts",
                            ));
                        }
                    }
                }
            }
            other => {
                return Err(Error::coded(
                    ec::PROTOCOL_VIOLATION,
                    format!("{} during registration", other.variant()),
                ));
            }
        }
    }
}

/// Signs the registering client's CSR and swaps the registration certificate
/// for it. The CSR's CN is the name the user picked, its key must be new.
async fn redeem(server: &Arc<Server>, identity: &Identity, csr_der: &[u8]) -> Result<Vec<u8>> {
    let requested = ca::csr_common_name(csr_der)
        .map_err(|e| Error::coded(ec::RENEW_REJECTED, e.to_string()))?;
    if ca::csr_public_key(csr_der)? == identity.cert_spki {
        return Err(Error::coded(
            ec::RENEW_REJECTED,
            "registration needs a freshly generated key",
        ));
    }
    let mut store = server.store.lock().await;
    // A placeholder whose bundle expired unused frees its name (`docs/reference/protocol.md`
    // §2.3); otherwise only the next invite or a restart would. This user is
    // among them only if its own bundle expired during the session.
    if store.purge_expired_registrations()? > 0 {
        tracing::info!("expired registrations purged");
    }
    if store.user(identity.user_id).is_none() {
        return Err(Error::coded(
            ec::AUTH_UNKNOWN_CERT,
            "registration expired during the session",
        ));
    }
    store.claim_name(&requested, Some(identity.user_id))?;
    let (leaf, cn) = {
        let ca = server.ca.read().await;
        ca.sign_client_csr(
            csr_der,
            server.config.tls.client_validity_days,
            &server.config.public_host(),
        )
        .map_err(|e| Error::coded(ec::RENEW_REJECTED, e.to_string()))?
    };
    let cert = CertRecord::issued(&leaf, cn.clone(), rfc3339(now()), false);
    store.redeem_registration(identity.user_id, &identity.cert_fp, cert)?;
    drop(store);
    // A repeated redeem revoked the certificates of earlier ones.
    crate::endpoint::reload_tls_logged(server).await;
    tracing::info!(user_id = identity.user_id, name = %cn, "registered");
    Ok(leaf.to_vec())
}

/// Encodes and writes one control message straight onto the stream, for the
/// renew-only sessions, which have no room queue.
async fn write_msg(send: &mut quinn::SendStream, msg: &ControlMsg) -> Result<()> {
    wire::write_frame(send, &msg.encode()?).await
}

/// This server's `Welcome`: protocol constants and `min_client` filled in.
fn welcome(
    server: &Server,
    minor: u16,
    you: UserInfo,
    users: Vec<UserInfo>,
    is_admin: bool,
    registration: bool,
) -> ControlMsg {
    ControlMsg::Welcome {
        minor,
        caps: icu_proto::CAPS,
        you,
        audio: AudioParams::default(),
        users,
        min_client: server.config.server.min_client.clone(),
        is_admin,
        registration,
        server_minor: icu_proto::MINOR,
    }
}

/// The answer to a `Ping`.
fn pong(nonce: u64) -> ControlMsg {
    ControlMsg::Pong {
        nonce,
        server_time_ms: unix_millis(),
    }
}

/// Drains the outgoing control queue onto the control stream. The frames are
/// shared with every other recipient, so they are handed to quinn as chunks
/// rather than copied.
async fn writer_task(mut send: quinn::SendStream, mut rx: mpsc::Receiver<Bytes>) {
    while let Some(frame) = rx.recv().await {
        if send.write_chunk(frame).await.is_err() {
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
///
/// A sender has at most `VIDEO_STREAMS_PER_SENDER` frames in flight here: a
/// camera sends one frame at a time, and every frame in flight holds a task
/// and up to 512 KiB of payload. A stream beyond that is stopped unread with
/// `RATE_LIMITED`, which also hands its flow control credit back.
async fn uni_task(server: Arc<Server>, conn: quinn::Connection, session: Session, user_id: UserId) {
    let state = Arc::new(Mutex::new(VideoSender::new()));
    let budget = lock(&server.video_budget)
        .entry(user_id)
        .or_insert_with(|| Arc::new(Mutex::new(VideoBudget::new(&server.config.limits))))
        .clone();
    let in_flight = Arc::new(tokio::sync::Semaphore::new(pl::VIDEO_STREAMS_PER_SENDER));
    loop {
        match conn.accept_uni().await {
            Ok(mut recv) => {
                let Ok(permit) = in_flight.clone().try_acquire_owned() else {
                    Counters::inc(&server.counters.video_dropped_rate);
                    let _ = recv.stop(quinn::VarInt::from_u32(ec::RATE_LIMITED));
                    continue;
                };
                let (server, state, budget) = (server.clone(), state.clone(), budget.clone());
                crate::spawn_logged("video frame", async move {
                    relay::video(server, session, recv, state, budget).await;
                    drop(permit);
                });
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
    let actor = admin::Actor {
        user_id,
        cert_fp: identity.cert_fp.clone(),
    };
    let mut control = TokenBucket::per_second(server.config.limits.control_msgs_per_s);
    let mut set_state = TokenBucket::per_second(server.config.limits.set_state_per_s);
    // Every change rewrites `users.toml`; a client sends one per picked
    // color, debounced, so a burst of three and one a second is plenty.
    let mut corona = TokenBucket::new(1.0, 3.0);
    let mut last_video_report: HashMap<Session, Instant> = HashMap::new();
    let mut violations = ViolationCounter::new(
        pl::RATE_VIOLATIONS_FATAL,
        server.config.timing.rate_violation_window(),
    );

    // Built once: `closed()` takes the connection lock and
    // `shutdown_requested()` subscribes, neither per frame.
    let closed = conn.closed();
    let shutdown = server.shutdown_requested();
    tokio::pin!(closed, shutdown);

    loop {
        let body = tokio::select! {
            body = wire::read_frame(recv, MAX_CONTROL_FRAME) => body,
            _ = &mut closed => return Ok(()),
            _ = &mut shutdown => {
                return Err(Error::coded(ec::SHUTDOWN, "server is shutting down"));
            }
        };
        let Some(body) = stream_end(body)? else {
            return Ok(());
        };

        Counters::inc(&server.counters.control_in);
        if !control.take_one() {
            rate_limited(server, session, &mut violations, "control rate exceeded")?;
            continue;
        }

        let msg = ControlMsg::decode(&body)?;
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
                    rate_limited(server, session, &mut violations, "SetState rate exceeded")?;
                    continue;
                }
                if let Some(flags) = server.room.apply_client_state(session, flags) {
                    server
                        .room
                        .broadcast(&ControlMsg::UserState { session, flags });
                    server.refresh_loss_hints();
                }
            }
            ControlMsg::SetCoronaColor { color } => {
                if !corona.take_one() {
                    rate_limited(
                        server,
                        session,
                        &mut violations,
                        "SetCoronaColor rate exceeded",
                    )?;
                    continue;
                }
                set_corona_color(server, session, user_id, color).await;
            }
            ControlMsg::VideoDisplayed { speaker } => {
                if server.room.video_display_eligible(session, speaker) {
                    let now = Instant::now();
                    if last_video_report
                        .get(&speaker)
                        .is_none_or(|last| now.duration_since(*last) >= Duration::from_secs(1))
                    {
                        last_video_report.insert(speaker, now);
                        server
                            .room
                            .send_to(speaker, &ControlMsg::VideoViewer { viewer: session });
                    }
                }
            }
            ControlMsg::Ping { nonce } => {
                server.room.send_to(session, &pong(nonce));
            }
            ControlMsg::AudioLevelReport {
                speaker,
                level,
                window,
            } => {
                if server.room.feedback_eligible(session, speaker) {
                    lock(&server.audio_feedback).report(
                        session,
                        speaker,
                        level,
                        window,
                        std::time::Instant::now(),
                    );
                }
            }
            // Both name a speaker, and both key server state by it: only a
            // peer that is in the room right now counts, or a client could
            // fill the maps with all 65 535 ids.
            ControlMsg::LossReport { speaker, .. } | ControlMsg::KeyframeRequest { speaker }
                if speaker == session || server.room.flags(speaker).is_none() => {}
            ControlMsg::LossReport {
                speaker,
                loss_permille,
            } => {
                let mut loss = lock(&server.loss);
                if loss_permille > 1000 || !server.room.feedback_eligible(session, speaker) {
                    continue;
                }
                let ttl = server.config.timing.loss_report_ttl();
                let listeners = |speaker| server.room.audio_listeners(speaker);
                let hint = loss.report(speaker, session, loss_permille, ttl, listeners);
                if let Some(loss_permille) = hint {
                    server
                        .room
                        .send_to(speaker, &ControlMsg::LossHint { loss_permille });
                }
            }
            ControlMsg::KeyframeRequest { speaker } => {
                server
                    .room
                    .keyframe_request(speaker, server.config.timing.keyframe_min_interval());
            }
            ControlMsg::RenewRequest { csr_der } => {
                handle_renew(server, session, identity, &csr_der).await;
            }
            ControlMsg::CreateInviteRequest { name, ttl_s } => {
                handle_create_invite(server, session, &actor, &name, ttl_s).await;
            }
            ControlMsg::AdminSetRole {
                user_id: target,
                is_admin,
            } => {
                let done = admin::set_role(server, Some(&actor), target, is_admin).await;
                let answer = admin_answer(target, done, |()| ControlMsg::AdminSetRoleOk {
                    user_id: target,
                    is_admin,
                });
                server.room.send_to(session, &answer);
            }
            ControlMsg::AdminSetDisabled {
                user_id: target,
                disabled,
            } => {
                let done = admin::set_disabled(server, Some(&actor), target, disabled).await;
                let answer = admin_answer(target, done, |_| ControlMsg::AdminSetDisabledOk {
                    user_id: target,
                    disabled,
                });
                server.room.send_to(session, &answer);
            }
            ControlMsg::AdminRevokeUser { user_id: target } => {
                let done = admin::revoke_user(server, Some(&actor), target).await;
                let answer = admin_answer(target, done, |closed| ControlMsg::AdminRevokeUserOk {
                    user_id: target,
                    closed: closed as u32,
                });
                server.room.send_to(session, &answer);
            }
            ControlMsg::PeerCertificateRequest { session: target } => {
                let certificate_der = server.room.certificate(target);
                server.room.send_to(
                    session,
                    &ControlMsg::PeerCertificateResponse {
                        session: target,
                        certificate_der,
                    },
                );
            }
            ControlMsg::UserListRequest => {
                handle_user_list_request(server, session, &actor).await;
            }
            ControlMsg::AdminKick { user_id: target } => {
                let done = admin::kick(server, Some(&actor), target).await;
                let answer = admin_answer(target, done, |closed| ControlMsg::AdminKickOk {
                    user_id: target,
                    closed: closed as u32,
                });
                server.room.send_to(session, &answer);
            }
            ControlMsg::AdminSetRestriction {
                user_id: target,
                muted,
                deafened,
            } => {
                let done =
                    admin::set_restriction(server, Some(&actor), target, |_| (muted, deafened))
                        .await;
                let answer = admin_answer(target, done, |_| ControlMsg::AdminSetRestrictionOk {
                    user_id: target,
                    muted,
                    deafened,
                });
                server.room.send_to(session, &answer);
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

/// Sends `Error(RATE_LIMITED)`; the fatal third violation within the window
/// is the `Err` (§6.6).
/// `SetCoronaColor`: store, room and the `UserCoronaColor` broadcast under
/// the store lock, like an admin restriction (`admin::set_restriction`): a
/// session joining meanwhile reads the store under that lock too, so it
/// either sees the old color and then gets the broadcast, or sees the new
/// one. An unchanged color writes and sends nothing. A failed write is the
/// server's own trouble: logged, the sender gets `SERVER_ERROR`, nothing
/// changes.
async fn set_corona_color(
    server: &Arc<Server>,
    session: Session,
    user_id: UserId,
    color: Option<Rgb>,
) {
    let mut store = server.store.lock().await;
    match store.set_corona_color(user_id, color) {
        Ok(_) => {
            for changed in server.room.set_corona_color(user_id, color) {
                server.room.broadcast(&ControlMsg::UserCoronaColor {
                    session: changed,
                    color,
                });
            }
        }
        Err(e) => {
            tracing::warn!(user_id, error = %e, "cannot store the corona color");
            server.room.send_to(
                session,
                &ControlMsg::Error {
                    code: e.code().unwrap_or(ec::SERVER_ERROR),
                    detail: e.to_string(),
                },
            );
        }
    }
}

fn rate_limited(
    server: &Arc<Server>,
    session: Session,
    violations: &mut ViolationCounter,
    detail: &str,
) -> Result<()> {
    Counters::inc(&server.counters.control_rate_limited);
    server.room.send_to(
        session,
        &ControlMsg::Error {
            code: ec::RATE_LIMITED,
            detail: detail.to_string(),
        },
    );
    if violations.record() {
        return Err(Error::coded(
            ec::RATE_LIMITED_FATAL,
            "third rate limit violation within the window",
        ));
    }
    Ok(())
}

/// Whether a certificate with `validity` (notBefore, notAfter; `None`:
/// unreadable) has expired at `now`.
fn cert_expired(
    validity: Option<(time::OffsetDateTime, time::OffsetDateTime)>,
    now: time::OffsetDateTime,
) -> bool {
    validity.is_some_and(|(_, not_after)| not_after <= now)
}

/// Whether `last` lies less than `bound` before `now`. A stamp from the
/// future — written while the clock ran ahead — counts as long past: it
/// refused every renewal until the clock caught up with it (external review
/// 2026-09-27, S-7).
fn within(last: time::OffsetDateTime, bound: time::Duration, now: time::OffsetDateTime) -> bool {
    let elapsed = now - last;
    elapsed >= time::Duration::ZERO && elapsed < bound
}

/// A plain renewal is accepted once less than a third of the presented
/// certificate's lifetime, and at least 30 days, is left (`docs/reference/protocol.md` §2.2
/// Nr. 9; the client asks at the same point).
fn renew_window(issued: &str, expires: &str) -> time::Duration {
    let third = match (parse_rfc3339(issued), parse_rfc3339(expires)) {
        (Ok(issued), Ok(expires)) if expires > issued => (expires - issued) / 3,
        _ => time::Duration::ZERO,
    };
    third.max(time::Duration::days(30))
}

/// A certificate issued by [`renew_leaf`].
struct Renewed {
    leaf: Vec<u8>,
    cn: String,
    renamed: bool,
}

/// Certificate renewal, and the only way a name changes (`docs/reference/protocol.md` §2.2).
///
/// The CSR's subject CN is the name the client asks for. Four cases, told
/// apart by exact string comparison against the stored name — case-insensitive
/// comparison would misfile a pure case change as a plain renewal:
///
/// * CN == stored, presented certificate current: plain renewal, one per day,
///   in the last third of its lifetime.
/// * CN == stored, presented certificate stale: catch-up after a rename on
///   another device. Exempt from the daily limit, but bounded per certificate
///   so one stale session cannot mint certificates in a loop.
/// * CN != stored: rename. Costs the daily budget and needs the name to be
///   free server-wide.
/// * Presented certificate expired (renew-only session): bounded per
///   certificate (one an hour), so renewing on one device never locks
///   another out of recovering; no rename.
///
/// `Err` carries the code and detail for `Error`.
async fn renew_leaf(
    server: &Arc<Server>,
    identity: &Identity,
    csr_der: &[u8],
) -> std::result::Result<Renewed, (u32, String)> {
    let user_id = identity.user_id;
    let requested =
        ca::csr_common_name(csr_der).map_err(|e| (ec::RENEW_REJECTED, e.to_string()))?;
    // Expired as of now, not as of the handshake: a room session outlives
    // its certificate's notAfter, and one opened just before could renew onto
    // a new key on the strength of an expired certificate (external review
    // 2026-09-27).
    let expired = identity.expired || cert_expired(identity.cert_validity, now());
    // An expired certificate renews onto its own key (`docs/reference/protocol.md` §2.3.3):
    // the handshake proved that key and nothing else, so a new one would be
    // trusted on the strength of an expired certificate alone.
    if expired
        && ca::csr_public_key(csr_der).map_err(|e| (ec::RENEW_REJECTED, e.to_string()))?
            != identity.cert_spki
    {
        return Err((
            ec::RENEW_REJECTED,
            "an expired certificate renews onto its own key only".into(),
        ));
    }
    // A store refusal keeps its own code (`NAME_TAKEN`, `AUTH_REVOKED`, …).
    let store_err = |e: Error| (e.code().unwrap_or(ec::RENEW_REJECTED), e.to_string());

    let mut store = server.store.lock().await;
    // Admission again, under the lock every revoke, disable and retirement
    // happens under: since this session authenticated, its certificate may
    // have been revoked, or retired by a successor's first login — a copy of
    // an expired certificate must not renew once its owner moved on.
    admit(&store, &identity.cert_fp).map_err(store_err)?;
    let Some(user) = store.user(user_id) else {
        return Err((ec::RENEW_REJECTED, "unknown user".into()));
    };
    let stored = user.name.clone();
    let rename = requested != stored;
    if rename && expired {
        return Err((
            ec::RENEW_REJECTED,
            "an expired certificate renews under the stored name only".into(),
        ));
    }
    // A new name meets today's rules; the stored one is carried over as it
    // is, even if it predates them. Uniqueness before the rate check, so a
    // taken name reports NAME_TAKEN instead of a confusing rate message.
    if rename {
        store
            .claim_name(&requested, Some(user_id))
            .map_err(store_err)?;
    }

    let catch_up = !rename && identity.cert_cn != stored;
    // Bounded per presented certificate rather than per user.
    let per_cert = catch_up || expired;
    if per_cert {
        // An hour for an expired certificate: a renewal whose answer got lost
        // must not strand its holder for a day.
        let (bound, detail) = if expired {
            (
                time::Duration::hours(1),
                "this certificate was renewed within the hour",
            )
        } else {
            (
                time::Duration::days(1),
                "this certificate already caught up today",
            )
        };
        if let Some(last) = store.cert_renewed(user_id, &identity.cert_fp) {
            if within(last, bound, now()) {
                return Err((ec::RENEW_REJECTED, detail.into()));
            }
        }
    } else if let Some(last) = store.last_renew(user_id) {
        if within(last, time::Duration::days(1) / pl::RENEW_PER_DAY, now()) {
            return Err((ec::RENEW_REJECTED, "at most one renewal per day".into()));
        }
    }
    // Only inside the last third of the presented certificate's lifetime, or
    // after it expired, does a new certificate get a fresh lifetime. A plain
    // renewal is refused before that (otherwise one renewal a day would pile
    // up a year's worth of live certificates); a rename or catch-up still
    // goes through, but the new certificate keeps the old one's validity, so
    // no re-issue ever extends a certificate early.
    let presented = store
        .by_fingerprint(&identity.cert_fp)
        .map(|(_, _, c)| (c.issued.clone(), c.expires.clone()));
    let window = presented.as_ref().and_then(|(issued, expires)| {
        let expires_at = parse_rfc3339(expires).ok()?;
        Some((expires_at, renew_window(issued, expires)))
    });
    let in_window = expired || window.is_none_or(|(expires, window)| expires - now() <= window);
    if !in_window && !rename && !catch_up {
        let days = window.map(|(_, w)| w.whole_days()).unwrap_or_default();
        return Err((
            ec::RENEW_REJECTED,
            format!("renewal opens {days} days before expiry"),
        ));
    }
    // A kept lifetime keeps its `issued` too, so the renewal window computed
    // from the record stays the presented certificate's.
    let (lifetime, issued) = match (in_window, identity.cert_validity, presented) {
        (false, Some((not_before, not_after)), Some((issued, _))) => (
            ca::Lifetime::Keep {
                not_before,
                not_after,
            },
            issued,
        ),
        _ => (
            ca::Lifetime::Days(server.config.tls.client_validity_days),
            rfc3339(now()),
        ),
    };
    let (leaf, cn) = {
        let ca = server.ca.read().await;
        ca.sign_client_csr(csr_der, lifetime, &server.config.public_host())
            .map_err(|e| (ec::RENEW_REJECTED, e.to_string()))?
    };
    let cert = CertRecord::issued(&leaf, cn.clone(), issued, false);
    // Only a per-certificate renewal stamps the presented certificate: the
    // same certificate may be on a second device (passport) that still has
    // to catch up or recover itself, and the stamp is what bounds it.
    let supersedes = per_cert.then_some(identity.cert_fp.as_str());
    let renamed = store
        .add_cert(user_id, cert, supersedes, !per_cert)
        .map_err(store_err)?;
    drop(store);
    tracing::info!(
        user_id,
        name = %cn,
        renamed,
        catch_up,
        expired,
        "certificate renewed"
    );
    Ok(Renewed {
        leaf: leaf.to_vec(),
        cn,
        renamed,
    })
}

/// `RenewRequest` in the room: [`renew_leaf`], the answer, and a rename
/// pushed to every session of the user.
async fn handle_renew(server: &Arc<Server>, session: Session, identity: &Identity, csr_der: &[u8]) {
    let renewed = match renew_leaf(server, identity, csr_der).await {
        Ok(renewed) => renewed,
        Err((code, detail)) => {
            server
                .room
                .send_to(session, &ControlMsg::Error { code, detail });
            return;
        }
    };
    server.room.send_to(
        session,
        &ControlMsg::RenewResponse {
            leaf_der: renewed.leaf,
        },
    );
    if renewed.renamed {
        for s in server.room.sessions_of_user(identity.user_id) {
            server.room.rename(s, &renewed.cn);
            server.room.broadcast(&ControlMsg::UserRenamed {
                session: s,
                name: renewed.cn.clone(),
            });
        }
    }
}

/// Issues a registration and returns its bundle (`CreateInviteRequest`,
/// `docs/reference/protocol.md` §2.3). Admin only; rejected non-fatally otherwise.
async fn handle_create_invite(
    server: &Arc<Server>,
    session: Session,
    actor: &admin::Actor,
    name: &str,
    ttl_s: u64,
) {
    let reject = |code: u32, detail: String| ControlMsg::Error { code, detail };
    let user_id = actor.user_id;

    if !actor.is_admin(&*server.store.lock().await) {
        server
            .room
            .send_to(session, &reject(ec::NOT_ADMIN, "not an admin".into()));
        return;
    }
    // Each bundle is a user in `users.toml` until it is confirmed or
    // expires; an admin certificate in the wrong hands must not be able to
    // fill it.
    let per_hour = server.config.limits.invites_per_admin_per_hour;
    // Zero means none in-band, only the host's CLI: a bucket of zero would
    // be rounded up to one an hour (external review 2026-09-27).
    if per_hour == 0 {
        server.room.send_to(
            session,
            &reject(
                ec::RATE_LIMITED,
                "invitations are issued on the server host only here".into(),
            ),
        );
        return;
    }
    if !lock(&server.invite_budget)
        .entry(user_id)
        .or_insert_with(|| TokenBucket::per_hour(per_hour))
        .take_one()
    {
        server.room.send_to(
            session,
            &reject(
                ec::RATE_LIMITED,
                format!("at most {per_hour} invitations per hour"),
            ),
        );
        return;
    }
    // `issue` codes its refusals (`NAME_TAKEN` for a malformed or taken
    // label, `RATE_LIMITED`); anything uncoded is the server's own trouble,
    // a failed write, and must not read as a taken name. Refused, it minted
    // nothing: the token goes back, or a taken name, a full server or a disk
    // problem would use up the admin's hour.
    let issued = crate::registration::issue(server, Some(actor), name, ttl_s).await;
    if issued.is_err() {
        if let Some(bucket) = lock(&server.invite_budget).get_mut(&user_id) {
            bucket.refund_one();
        }
    }
    match issued {
        Ok(issued) => {
            tracing::info!(
                user_id,
                invited = issued.user_id,
                "registration bundle minted"
            );
            server.room.send_to(
                session,
                &ControlMsg::CreateInviteResponse {
                    bundle: issued.bundle,
                    expires: issued.expires,
                },
            );
        }
        Err(e) => {
            tracing::warn!(user_id, error = %e, "cannot issue a registration");
            server.room.send_to(
                session,
                &reject(e.code().unwrap_or(ec::SERVER_ERROR), e.to_string()),
            );
        }
    }
}

/// The `UserFlags` bits an admin restriction keeps set. Deafened implies
/// muted: a user who hears nobody must not talk into the room either.
pub(crate) fn restriction_flags(muted: bool, deafened: bool) -> icu_proto::UserFlags {
    icu_proto::UserFlags::NONE
        .with_bit(icu_proto::UserFlags::MUTED, muted || deafened)
        .with_bit(icu_proto::UserFlags::DEAFENED, deafened)
}

/// `UserRestrictionChanged` for a restriction, as enforced: deafened
/// includes muted ([`restriction_flags`]).
pub(crate) fn restriction_changed(muted: bool, deafened: bool) -> ControlMsg {
    ControlMsg::UserRestrictionChanged {
        muted: muted || deafened,
        deafened,
    }
}

/// The in-band answer to an admin action: `ok` on success, else
/// `AdminActionError` with the code the store gave (`docs/reference/protocol.md` §2.2). An
/// uncoded error is the server's own trouble — a failed write — and goes out
/// as `SERVER_ERROR`, not as a policy refusal the admin would read as their
/// mistake.
fn admin_answer<T>(
    target: UserId,
    done: Result<T>,
    ok: impl FnOnce(T) -> ControlMsg,
) -> ControlMsg {
    match done {
        Ok(value) => ok(value),
        Err(e) => {
            if e.code().is_none() {
                tracing::warn!(target, error = %e, "admin action failed");
            }
            ControlMsg::AdminActionError {
                user_id: target,
                code: e.code().unwrap_or(ec::SERVER_ERROR),
                detail: e.to_string(),
            }
        }
    }
}

/// `UserListRequest` (`docs/reference/protocol.md` §2.2). Admin only; every registered user,
/// online or not.
async fn handle_user_list_request(server: &Arc<Server>, session: Session, actor: &admin::Actor) {
    let store = server.store.lock().await;
    if !actor.is_admin(&store) {
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
    let online = server.room.online_users();
    let mut rows: Vec<icu_proto::UserSummary> = store
        .users()
        .map(|(id, u)| icu_proto::UserSummary {
            user_id: id,
            name: u.name.clone(),
            created: u.created.clone(),
            is_admin: u.is_admin,
            disabled: u.disabled,
            revoked: u.is_revoked(),
            online: online.contains(&id),
            pending: u.is_pending(),
            muted: u.server_muted,
            deafened: u.server_deafened,
        })
        .collect();
    drop(store);
    // Most useful first, so a list too long for one frame loses the least
    // useful rows: online, then admins, then everybody else, removed
    // accounts last, the newest first within each.
    rows.sort_by_key(|u| {
        let rank = match () {
            _ if u.online => 0,
            _ if u.is_admin => 1,
            _ if !u.revoked => 2,
            _ => 3,
        };
        (rank, std::cmp::Reverse(u.user_id))
    });
    let users = fit_user_list(rows);
    server
        .room
        .send_to(session, &ControlMsg::UserListResponse { users });
}

/// `users` as they fit one `UserListResponse` (`docs/reference/protocol.md` §2.2 Nr. 27):
/// all of them, or the longest head that fits together with a last row that
/// stands for the rest — `user_id` 0, the number left out in `name`. Without
/// this, a list past a control frame (~230 users) was not sent at all.
fn fit_user_list(users: Vec<icu_proto::UserSummary>) -> Vec<icu_proto::UserSummary> {
    let fits = |users: Vec<icu_proto::UserSummary>| {
        ControlMsg::UserListResponse { users }.encode().is_ok()
    };
    if fits(users.clone()) {
        return users;
    }
    let total = users.len();
    let head = |k: usize| {
        let mut head = users[..k].to_vec();
        head.push(icu_proto::UserSummary {
            user_id: 0,
            name: format!("… und {} weitere", total - k),
            created: String::new(),
            is_admin: false,
            disabled: false,
            revoked: false,
            online: false,
            pending: false,
            muted: false,
            deafened: false,
        });
        head
    };
    // `head(lo)` fits (with `lo` 0, just the rest row), `head(hi)` does not.
    let (mut lo, mut hi) = (0, total);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(head(mid)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    tracing::warn!(
        total,
        listed = lo,
        "user list too long for one frame, shortened"
    );
    head(lo)
}

/// The code the peer closed with, reduced to the `UserLeft.reason` range.
///
/// Anything that is not a clean application close — idle timeout, reset,
/// transport error — is `TIMEOUT` (`docs/reference/protocol.md` §6).
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

#[cfg(test)]
mod tests {
    use super::*;

    /// S-7 (external review 2026-09-27): a renewal stamp from the future
    /// does not hold renewals back.
    #[test]
    fn a_stamp_from_the_future_counts_as_long_past() {
        let now = now();
        let hour = time::Duration::hours(1);
        assert!(within(now - time::Duration::minutes(10), hour, now));
        assert!(!within(now - time::Duration::minutes(70), hour, now));
        assert!(!within(now + time::Duration::days(30), hour, now));
        assert!(within(now, hour, now));
    }

    /// Expiry is a question about a moment: a certificate that was current
    /// at the handshake is expired once its notAfter has passed.
    #[test]
    fn a_certificate_expires_at_its_not_after() {
        let now = now();
        let validity = Some((
            now - time::Duration::days(2),
            now - time::Duration::seconds(1),
        ));
        assert!(cert_expired(validity, now));
        assert!(!cert_expired(validity, now - time::Duration::days(1)));
        assert!(!cert_expired(None, now), "unreadable is not expired");
    }
}
