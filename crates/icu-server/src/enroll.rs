//! Enrollment over ALPN `icu-enroll/1` (`Server/IMPLEMENTATION.md` §6.3).
//!
//! One request, one response, then close. A client certificate presented on
//! this ALPN is ignored.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use icu_proto::{
    error as ec, invite_blob::InviteBlob, limits as pl, EnrollMsg, MAX_ENROLL_FRAME,
};

use crate::{
    ca,
    error::{Error, Result},
    room::Counters,
    store::{fingerprint, now, rfc3339, CertRecord},
    wire, Server,
};

/// How long to wait for the peer to acknowledge the response before closing.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Handles one enrollment connection.
///
/// The answer — success or error — always goes back on the stream the client
/// opened; opening a second one would leave the client reading an empty
/// stream.
pub async fn handle(
    server: Arc<Server>,
    conn: quinn::Connection,
    remote: SocketAddr,
) -> Result<()> {
    Counters::inc(&server.counters.enroll_attempts);

    let streams =
        tokio::time::timeout(Duration::from_secs(pl::HELLO_TIMEOUT_S), conn.accept_bi()).await;
    let (mut send, mut recv) = match streams {
        Ok(Ok(streams)) => streams,
        Ok(Err(e)) => {
            Counters::inc(&server.counters.enroll_failed);
            conn.close(quinn::VarInt::from_u32(ec::ENROLL_BAD_TOKEN), b"");
            return Err(Error::coded(ec::ENROLL_BAD_TOKEN, e.to_string()));
        }
        Err(_) => {
            Counters::inc(&server.counters.enroll_failed);
            conn.close(quinn::VarInt::from_u32(ec::ENROLL_BAD_TOKEN), b"no stream");
            return Err(Error::coded(
                ec::ENROLL_BAD_TOKEN,
                "no enrollment stream within 5 s",
            ));
        }
    };

    match run(&server, remote, &mut send, &mut recv).await {
        Ok(()) => {
            Counters::inc(&server.counters.enroll_ok);
            finish_and_close(&conn, &mut send, ec::NO_ERROR, b"").await;
            Ok(())
        }
        Err(e) => {
            Counters::inc(&server.counters.enroll_failed);
            let code = e.code().unwrap_or(ec::ENROLL_BAD_TOKEN);
            tracing::info!(%remote, error = %e, "enrollment rejected");
            // `PROTOCOL.md` §2.3: first the Error message, then finish(), then
            // close with the same code — otherwise the dialog shows a bare
            // number.
            let msg = EnrollMsg::Error {
                code,
                detail: e.to_string(),
            };
            if let Ok(frame) = msg.encode() {
                let _ = wire::write_frame(&mut send, &frame).await;
            }
            finish_and_close(&conn, &mut send, code, ec::name(code).as_bytes()).await;
            Err(e)
        }
    }
}

/// Ends the enrollment stream the way `PROTOCOL.md` §2.3 requires: finish the
/// stream, wait for the peer to have the bytes, only then close the
/// connection with `code`. `close()` discards unacknowledged stream data, so
/// without the flush the client sees a bare number instead of the message
/// (A5: for "delivered" one awaits `stopped()`).
async fn finish_and_close(
    conn: &quinn::Connection,
    send: &mut quinn::SendStream,
    code: u32,
    reason: &[u8],
) {
    let _ = send.finish();
    let _ = tokio::time::timeout(FLUSH_TIMEOUT, send.stopped()).await;
    conn.close(quinn::VarInt::from_u32(code), reason);
}

async fn run(
    server: &Arc<Server>,
    remote: SocketAddr,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
) -> Result<()> {
    let allowed = server
        .ip_limits
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .allow_enroll(remote.ip());
    if !allowed {
        return Err(Error::coded(
            ec::ENROLL_RATE_LIMITED,
            "enrollment rate limit exceeded",
        ));
    }

    let body = wire::read_frame(recv, MAX_ENROLL_FRAME).await?;
    let request = EnrollMsg::decode(&body)
        .map_err(|e| Error::coded(ec::ENROLL_BAD_TOKEN, format!("bad request: {e}")))?;
    // `name` is obsolete: the display name is the CSR's subject CN
    // (`PROTOCOL.md` §2.3, revised 2026-09-22).
    let EnrollMsg::EnrollRequest {
        token,
        csr_der,
        name: _,
    } = request
    else {
        return Err(Error::coded(
            ec::ENROLL_BAD_TOKEN,
            "first message must be EnrollRequest",
        ));
    };
    // `token` has three shapes (`PROTOCOL.md` §2.3):
    //  - empty: bootstrap, only while the store has no users at all yet;
    //  - `EnrollMsg::TOKEN_LEN` chars: a plain invite token;
    //  - anything longer: the base64url body of a sealed `InviteBlob`
    //    (`§2.3.1`), relayed back opaquely by the client.
    enum TokenKind {
        Bootstrap,
        Invite(String),
    }

    // The name the client asks for. Parsed before the lock: a malformed CSR
    // must not hold up other enrollments.
    let cn = ca::csr_common_name(&csr_der)?;

    let mut store = server.store.lock().await;

    let kind = if token.is_empty() {
        if !store.is_empty() {
            return Err(Error::coded(
                ec::ENROLL_BOOTSTRAP_CLOSED,
                "an admin is already enrolled; ask them for an invite",
            ));
        }
        TokenKind::Bootstrap
    } else if token.len() == EnrollMsg::TOKEN_LEN {
        TokenKind::Invite(token.clone())
    } else {
        let blob = InviteBlob::decode_body_base64(&token).map_err(|e| {
            Error::coded(ec::ENROLL_BAD_TOKEN, format!("malformed invite blob: {e}"))
        })?;
        let aad = blob.aad();
        let mut sealed = blob.sealed.clone();
        let real_token = server.invite_key.open(blob.nonce, &mut sealed, &aad)?;
        TokenKind::Invite(real_token)
    };

    if let TokenKind::Invite(real_token) = &kind {
        store.peek_invite(real_token).ok_or_else(|| {
            Error::coded(ec::ENROLL_BAD_TOKEN, "token unknown, used or expired")
        })?;
    }

    // Uniqueness before signing, and signing before the token is consumed: a
    // collision leaves the invite usable for another attempt (§6.3).
    if store.name_taken(&cn, None) {
        return Err(Error::coded(
            ec::NAME_TAKEN,
            format!("name {cn:?} is already taken"),
        ));
    }

    let (leaf, cn) = {
        let ca = server.ca.read().await;
        ca.sign_client_csr(&csr_der, server.config.tls.client_validity_days)?
    };

    // Compare-and-set under the state lock: two parallel enrollments with the
    // same token, exactly one wins (§6.3.2). The same lock makes the bootstrap
    // check above and `is_first` below race-safe against each other and
    // against a concurrent invite redemption: whichever `add_user` lands
    // first is the one that makes the store non-empty.
    let reserved = store.peek_next_user_id();
    let is_first = store.is_empty();

    if let TokenKind::Invite(real_token) = &kind {
        store.consume_invite(real_token, reserved)?;
    }

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
    // Any first successful enrollment against an empty store grants admin,
    // whichever of the three paths above it took (clarified requirement,
    // not just the no-token bootstrap path).
    let user_id = match store.add_user(cert, is_first) {
        Ok(id) => id,
        Err(e) => {
            // Nothing was delivered, so the token goes back (§6.3.4).
            if let TokenKind::Invite(real_token) = &kind {
                let _ = store.release_invite(real_token);
            }
            let code = e.code().unwrap_or(ec::ENROLL_BAD_TOKEN);
            return Err(Error::coded(code, format!("state write failed: {e}")));
        }
    };
    debug_assert_eq!(user_id, reserved);
    drop(store);

    let ca_der = server.ca.read().await.ca_der().to_vec();
    let response = EnrollMsg::EnrollResponse {
        leaf_der: leaf.to_vec(),
        ca_der,
        server_name: server.config.server.server_name.clone(),
    };
    wire::write_frame(send, &response.encode()?).await?;
    tracing::info!(user_id, name = %cn, is_admin = is_first, %remote, "enrolled");
    Ok(())
}
