//! Enrollment over ALPN `icu-enroll/1` (`Server/IMPLEMENTATION.md` §6.3).
//!
//! One request, one response, then close. A client certificate presented on
//! this ALPN is ignored.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use icu_proto::{error as ec, limits as pl, validate_name, EnrollMsg, MAX_ENROLL_FRAME};

use crate::{
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
            let _ = send.finish();
            // `close()` discards unacknowledged stream data, so wait until the
            // peer has it (A5: for "delivered" one awaits `stopped()`).
            let _ = tokio::time::timeout(FLUSH_TIMEOUT, send.stopped()).await;
            conn.close(quinn::VarInt::from_u32(ec::NO_ERROR), b"");
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
            let _ = send.finish();
            let _ = tokio::time::timeout(FLUSH_TIMEOUT, send.stopped()).await;
            conn.close(quinn::VarInt::from_u32(code), ec::name(code).as_bytes());
            Err(e)
        }
    }
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
    let EnrollMsg::EnrollRequest {
        token,
        csr_der,
        name,
    } = request
    else {
        return Err(Error::coded(
            ec::ENROLL_BAD_TOKEN,
            "first message must be EnrollRequest",
        ));
    };
    if token.len() != EnrollMsg::TOKEN_LEN {
        return Err(Error::coded(ec::ENROLL_BAD_TOKEN, "malformed token"));
    }

    let mut store = server.store.lock().await;
    let (_, invite) = store
        .peek_invite(&token)
        .ok_or_else(|| Error::coded(ec::ENROLL_BAD_TOKEN, "token unknown, used or expired"))?;

    // The CN always comes from the invite, never from the CSR or the request
    // (`PROTOCOL.md` §2.3).
    let leaf = {
        let ca = server.ca.read().await;
        ca.sign_client_csr(
            &csr_der,
            &invite.name,
            server.config.tls.client_validity_days,
        )?
    };

    // Compare-and-set under the state lock: two parallel enrollments with the
    // same token, exactly one wins (§6.3.2).
    let reserved = store.peek_next_user_id();
    store.consume_invite(&token, reserved)?;

    // The display name may come from the request; a collision gets a suffix.
    let desired = if validate_name(&name).is_ok() {
        name.as_str()
    } else {
        invite.name.as_str()
    };
    let display_name = store.unique_name(desired);

    let issued = now();
    let cert = CertRecord {
        fingerprint: fingerprint(&leaf),
        issued: rfc3339(issued),
        expires: rfc3339(
            issued + Duration::from_secs(u64::from(server.config.tls.client_validity_days) * 86400),
        ),
        revoked: false,
    };
    let user_id = match store.add_user(&display_name, cert) {
        Ok(id) => id,
        Err(e) => {
            // Nothing was delivered, so the token goes back (§6.3.4).
            let _ = store.release_invite(&token);
            return Err(Error::coded(
                ec::ENROLL_BAD_TOKEN,
                format!("state write failed: {e}"),
            ));
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
    tracing::info!(user_id, name = %display_name, %remote, "enrolled");
    Ok(())
}
