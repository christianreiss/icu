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
    store::{fingerprint, now, parse_rfc3339, rfc3339, unix_millis, CertRecord, Store},
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
            conn.close(quinn::VarInt::from_u32(code), ec::name(code).as_bytes());
            return Err(e);
        }
    };

    // The registration budget comes before the slot: a refused attempt must
    // not first close a registration of the same bundle that is in progress.
    if identity.registration {
        if let Err(e) = enroll_budget(&server, &conn) {
            tracing::info!(%remote, user_id = identity.user_id, error = %e, "rejected");
            let code = ec::ENROLL_RATE_LIMITED;
            conn.close(quinn::VarInt::from_u32(code), ec::name(code).as_bytes());
            return Err(e);
        }
    }

    // One live connection per certificate: this one wins (`PROTOCOL.md` §7).
    let _live = LiveCert::claim(&server, &identity.cert_fp, &conn);

    let served = if identity.registration {
        serve_registration(&server, &conn, &identity).await
    } else {
        serve(&server, &conn, &identity).await
    };
    match served {
        Ok(()) => Ok(()),
        Err(e) => {
            let code = e.code().unwrap_or(ec::PROTOCOL_VIOLATION);
            tracing::info!(%remote, user_id = identity.user_id, error = %e, "closing");
            conn.close(quinn::VarInt::from_u32(code), ec::name(code).as_bytes());
            Err(e)
        }
    }
}

/// Holds a certificate's slot in [`Server::live_certs`] for as long as its
/// connection lives. A newer connection with the same certificate takes the
/// slot over and closes this one with `SESSION_REPLACED`: newest wins, so a
/// client whose old path is still waiting for its idle timeout is not locked
/// out, and a stolen passport in use shows up as a kicked owner.
struct LiveCert {
    server: Arc<Server>,
    fp: String,
    id: usize,
}

impl LiveCert {
    fn claim(server: &Arc<Server>, fp: &str, conn: &quinn::Connection) -> LiveCert {
        let old = server
            .live_certs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(fp.to_string(), conn.clone());
        if let Some(old) = old.filter(|old| old.stable_id() != conn.stable_id()) {
            // Mark before closing, like `kick_user`, so `UserLeft` carries the
            // code. An old session still before `room.join` sees its closed
            // connection right after joining and leaves again.
            if let Some(session) = server.room.session_of_connection(old.stable_id()) {
                server.room.mark_closing(session, ec::SESSION_REPLACED);
            }
            old.close(
                quinn::VarInt::from_u32(ec::SESSION_REPLACED),
                ec::name(ec::SESSION_REPLACED).as_bytes(),
            );
            tracing::info!(
                fingerprint = fp,
                old = %old.remote_address(),
                new = %conn.remote_address(),
                "certificate connected again, older connection replaced"
            );
        }
        LiveCert {
            server: server.clone(),
            fp: fp.to_string(),
            id: conn.stable_id(),
        }
    }
}

impl Drop for LiveCert {
    fn drop(&mut self) {
        let mut live = self
            .server
            .live_certs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Only our own entry: a replacement already sits there otherwise.
        if live.get(&self.fp).is_some_and(|c| c.stable_id() == self.id) {
            live.remove(&self.fp);
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
    /// Authenticated with a registration certificate (`PROTOCOL.md` §2.3).
    registration: bool,
    /// SubjectPublicKeyInfo of the presented leaf. A registration has to
    /// bring a different key.
    cert_spki: Vec<u8>,
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

    let mut store = server.store.lock().await;
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
    let registration = cert.registration;
    // The first login with a real certificate proves the client stored it,
    // whether or not the registration session saw the clean close. A failed
    // write must not cost the login: the bundle just stays valid a little
    // longer, as before the receipt, and the next login tries again.
    let consumed = !registration
        && store.consume_registration(user_id).unwrap_or_else(|e| {
            tracing::warn!(user_id, error = %e, "cannot consume the registration");
            false
        });
    let spki = ca::leaf_public_key(leaf);
    drop(store);
    if consumed {
        crate::endpoint::reload_tls_logged(server).await;
    }
    Ok(Identity {
        user_id,
        cert_fp: fp,
        cert_cn,
        registration,
        cert_spki: spki,
    })
}

/// What `Hello` said, plus the control stream it arrived on.
struct Hello {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    minor: u16,
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
    let _ = send.set_priority(1); // control beats video (`PROTOCOL.md` §4)

    let body = tokio::time::timeout_at(deadline, wire::read_frame(&mut recv, MAX_CONTROL_FRAME))
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
    Ok(Hello {
        send,
        recv,
        minor,
        client,
        initial,
    })
}

async fn serve(server: &Arc<Server>, conn: &quinn::Connection, identity: &Identity) -> Result<()> {
    let Hello {
        send,
        mut recv,
        minor,
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
    let (_, user, cert) = store.by_fingerprint(&identity.cert_fp).ok_or_else(|| {
        Error::coded(
            ec::AUTH_UNKNOWN_CERT,
            format!("unknown user_id {}", identity.user_id),
        )
    })?;
    if cert.revoked {
        return Err(Error::coded(ec::AUTH_REVOKED, "certificate revoked"));
    }
    if user.disabled {
        return Err(Error::coded(ec::AUTH_DISABLED, "user disabled"));
    }
    let stored_name = user.name.clone();
    let is_admin = user.is_admin;

    // 3. Join, `Welcome`, `UserJoined`.
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(crate::limits::CONTROL_QUEUE_FRAMES);
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
        registration: false,
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
    };
    drop(store);
    // The client checks the server leaf against the newest CRL it holds;
    // this is how it gets one (`PROTOCOL.md` §2.2).
    let crl_der = server.crl.read().unwrap_or_else(|e| e.into_inner()).clone();
    if !crl_der.is_empty() {
        server
            .room
            .send_to(session, &ControlMsg::CrlUpdate { crl_der });
    }

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

/// How long a registration session may stay open in total. Long enough for a
/// user to retry a taken name a few times, short enough that a bundle cannot
/// park a connection.
const REGISTRATION_SESSION: Duration = Duration::from_secs(120);

/// How long a finished registration session waits for the client to close.
const REGISTRATION_LINGER: Duration = Duration::from_secs(5);

/// `RenewRequest`s one registration session may send before it is closed.
const REGISTRATION_ATTEMPTS: u32 = 10;

/// A session authenticated with a registration certificate
/// (`PROTOCOL.md` §2.3). It never joins the room: after `Welcome` with
/// `registration = true` the only useful message is a `RenewRequest` with a
/// fresh key. On success the answer is `RenewResponse`, the registration
/// certificate is revoked, and the client reconnects with its own.
async fn serve_registration(
    server: &Arc<Server>,
    conn: &quinn::Connection,
    identity: &Identity,
) -> Result<()> {
    // The cap covers the whole session, writes included: a client that stops
    // reading stalls a write on flow control, where no `select!` around a
    // read could end it.
    let deadline = tokio::time::Instant::now() + REGISTRATION_SESSION;
    tokio::time::timeout_at(
        deadline + REGISTRATION_LINGER,
        registration_exchange(server, conn, identity, deadline),
    )
    .await
    .unwrap_or_else(|_| Err(Error::coded(ec::TIMEOUT, "registration took too long")))
}

/// Registration sessions: 5/min per IP, 20/min global (`PROTOCOL.md` §7).
fn enroll_budget(server: &Arc<Server>, conn: &quinn::Connection) -> Result<()> {
    let allowed = server
        .ip_limits
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .allow_enroll(conn.remote_address().ip());
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

/// Hello, `Welcome` and the `RenewRequest` loop of [`serve_registration`].
async fn registration_exchange(
    server: &Arc<Server>,
    conn: &quinn::Connection,
    identity: &Identity,
    deadline: tokio::time::Instant,
) -> Result<()> {
    let Hello {
        mut send,
        mut recv,
        minor,
        client,
        initial: _,
    } = read_hello(server, conn).await?;
    let (name, is_admin) = {
        let store = server.store.lock().await;
        let user = store.user(identity.user_id).ok_or_else(|| {
            Error::coded(ec::AUTH_UNKNOWN_CERT, "registration user vanished")
        })?;
        (user.name.clone(), user.is_admin)
    };
    let welcome = ControlMsg::Welcome {
        minor,
        caps: icu_proto::CAPS,
        you: UserInfo {
            user_id: identity.user_id,
            session: 0,
            name,
            flags: icu_proto::UserFlags::NONE,
        },
        audio: AudioParams::default(),
        users: Vec::new(),
        min_client: server.config.server.min_client.clone(),
        is_admin,
        registration: true,
    };
    write_msg(&mut send, &welcome).await?;
    tracing::info!(
        user_id = identity.user_id,
        remote = %conn.remote_address(),
        client = %client,
        "registration session"
    );

    // Nothing else bounds `Ping` here; past the rate it goes unanswered.
    let mut pings = TokenBucket::per_second(server.config.limits.control_msgs_per_s);
    let mut attempts = 0u32;
    loop {
        let body = tokio::select! {
            body = wire::read_frame(&mut recv, MAX_CONTROL_FRAME) => body,
            _ = tokio::time::sleep_until(deadline) => {
                return Err(Error::coded(ec::TIMEOUT, "registration took too long"));
            }
            _ = server.shutdown_requested() => {
                return Err(Error::coded(ec::SHUTDOWN, "server is shutting down"));
            }
        };
        let body = match body {
            Ok(body) => body,
            Err(e) if e.code() == Some(ec::PROTOCOL_VIOLATION) => return Err(e),
            Err(_) => return Ok(()),
        };
        let msg = ControlMsg::decode(&body)
            .map_err(|e| Error::coded(ec::PROTOCOL_VIOLATION, e.to_string()))?;
        match msg {
            ControlMsg::Ping { nonce } => {
                if pings.take_one() {
                    let pong = ControlMsg::Pong {
                        nonce,
                        server_time_ms: unix_millis(),
                    };
                    write_msg(&mut send, &pong).await?;
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
    icu_proto::validate_name(&requested)
        .map_err(|e| Error::coded(ec::RENEW_REJECTED, format!("bad name: {e}")))?;
    if ca::csr_public_key(csr_der)? == identity.cert_spki {
        return Err(Error::coded(
            ec::RENEW_REJECTED,
            "registration needs a freshly generated key",
        ));
    }
    let mut store = server.store.lock().await;
    // A placeholder whose bundle expired unused frees its name (`PROTOCOL.md`
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
    if store.name_taken(&requested, Some(identity.user_id)) {
        return Err(Error::coded(
            ec::NAME_TAKEN,
            format!("name {requested:?} is already taken"),
        ));
    }
    let (leaf, cn) = {
        let ca = server.ca.read().await;
        ca.sign_client_csr(
            csr_der,
            server.config.tls.client_validity_days,
            &server.config.public_host(),
        )
        .map_err(|e| Error::coded(ec::RENEW_REJECTED, e.to_string()))?
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
        registration: false,
        serial: Some(ca::leaf_serial(&leaf)),
    };
    store.redeem_registration(identity.user_id, &identity.cert_fp, cert)?;
    drop(store);
    // A repeated redeem revoked the certificates of earlier ones.
    crate::endpoint::reload_tls_logged(server).await;
    tracing::info!(user_id = identity.user_id, name = %cn, "registered");
    Ok(leaf.to_vec())
}

/// Encodes and writes one control message straight onto the stream, for the
/// registration session, which has no room queue.
async fn write_msg(send: &mut quinn::SendStream, msg: &ControlMsg) -> Result<()> {
    let frame = msg
        .encode()
        .map_err(|e| Error::coded(ec::PROTOCOL_VIOLATION, e.to_string()))?;
    wire::write_frame(send, &frame).await
}

/// Drains the outgoing control queue onto the control stream.
async fn writer_task(mut send: quinn::SendStream, mut rx: mpsc::Receiver<Vec<u8>>) {
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
            // Both name a speaker, and both key server state by it: only a
            // peer that is in the room right now counts, or a client could
            // fill the maps with all 65 535 ids.
            ControlMsg::LossReport { speaker, .. } | ControlMsg::KeyframeRequest { speaker }
                if speaker == session || server.room.flags(speaker).is_none() => {}
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

/// A plain renewal is accepted from this many days before the presented
/// certificate expires (`PROTOCOL.md` §2.2 Nr. 9; the client asks at the
/// same threshold).
const RENEW_WINDOW_DAYS: i64 = 30;

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
    // A new name meets today's rules; the stored one is carried over as it
    // is, even if it predates them.
    if rename {
        if let Err(e) = icu_proto::validate_name(&requested) {
            server.room.send_to(
                session,
                &reject(ec::RENEW_REJECTED, format!("bad name: {e}")),
            );
            return;
        }
    }

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
    // A plain renewal only inside the last 30 days of the presented
    // certificate (`PROTOCOL.md` §2.2 Nr. 9); otherwise one renewal a day
    // would pile up a year's worth of live certificates.
    if !rename && !catch_up {
        let expires = store
            .by_fingerprint(&identity.cert_fp)
            .and_then(|(_, _, c)| parse_rfc3339(&c.expires).ok());
        if let Some(expires) = expires {
            if expires - now() > time::Duration::days(RENEW_WINDOW_DAYS) {
                server.room.send_to(
                    session,
                    &reject(
                        ec::RENEW_REJECTED,
                        format!("renewal opens {RENEW_WINDOW_DAYS} days before expiry"),
                    ),
                );
                return;
            }
        }
    }

    let signed = {
        let ca = server.ca.read().await;
        ca.sign_client_csr(
            csr_der,
            server.config.tls.client_validity_days,
            &server.config.public_host(),
        )
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
        registration: false,
        serial: Some(ca::leaf_serial(&leaf)),
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

/// Issues a registration and returns its bundle (`CreateInviteRequest`,
/// `PROTOCOL.md` §2.3). Admin only; rejected non-fatally otherwise.
async fn handle_create_invite(
    server: &Arc<Server>,
    session: Session,
    user_id: UserId,
    name: &str,
    ttl_s: u64,
) {
    let reject = |code: u32, detail: String| ControlMsg::Error { code, detail };

    if !server.store.lock().await.is_admin(user_id) {
        server
            .room
            .send_to(session, &reject(ec::NOT_ADMIN, "not an admin".into()));
        return;
    }
    // The label is only a placeholder name, so the failures are a malformed
    // or taken one, or a failed write. `NAME_TAKEN` is the non-fatal code the
    // client already shows next to the invite field.
    match crate::registration::issue(server, name, ttl_s).await {
        Ok(issued) => {
            tracing::info!(user_id, invited = issued.user_id, "registration bundle minted");
            server.room.send_to(
                session,
                &ControlMsg::CreateInviteResponse {
                    bundle: issued.bundle,
                    expires: issued.expires,
                },
            );
        }
        Err(e) => {
            server.room.send_to(
                session,
                &reject(e.code().unwrap_or(ec::NAME_TAKEN), e.to_string()),
            );
        }
    }
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
) -> bool {
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
    let applied = outcome.is_ok();
    let answer = match outcome {
        Ok(_) => done(),
        Err(e) => ControlMsg::AdminActionError {
            user_id: target,
            code: e.code().unwrap_or(ec::LAST_ADMIN),
            detail: e.to_string(),
        },
    };
    server.room.send_to(session, &answer);
    applied
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
            announce_admin_change(server, target, is_admin);
            tracing::info!(actor, target, is_admin, "admin role changed");
            ControlMsg::AdminSetRoleOk {
                user_id: target,
                is_admin,
            }
        },
    )
    .await;
}

/// Puts a role change live without a reconnect: `UserAdminChanged` to every
/// one of the target's own sessions, then a `UserState` broadcast per session
/// whose `ADMIN` bit flipped, which is what moves the badge on everybody
/// else's roster. Shared by the in-band handler and the admin socket.
pub fn announce_admin_change(server: &Arc<Server>, target: UserId, is_admin: bool) {
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
    .await;
}

/// `AdminRevokeUser`: revokes every certificate of `target` and kicks any live
/// connection with `KICKED`.
async fn handle_admin_revoke_user(
    server: &Arc<Server>,
    session: Session,
    actor: UserId,
    target: UserId,
) {
    let revoked = admin_action(
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
    .await;
    if revoked {
        crate::endpoint::reload_tls_logged(server).await;
    }
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
            pending: u.is_pending(),
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
