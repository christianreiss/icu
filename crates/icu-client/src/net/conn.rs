//! The main connection: client config with the client certificate, connect,
//! `Hello`/`Welcome`, control stream helpers (`PROTOCOL.md` §1, §2, §5).

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use icu_proto::{limits as pl, ControlMsg, UserFlags, MAX_CONTROL_FRAME};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

use crate::config::Identity;

use super::{read_frame, NetError};

/// Delegates every check to a standard `WebPkiServerVerifier` unchanged, but
/// stashes the failure into a [`CertFailure`] slot so `connect()` can turn
/// quinn's opaque transport error back into the specific [`rustls::Error`]
/// that caused it. Purely an observation tap: it never overrides the
/// underlying accept/reject decision.
#[derive(Debug)]
struct ReportingVerifier {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    last_failure: CertFailure,
}

/// Shared slot a [`ReportingVerifier`] writes into and `connect()` reads
/// from. A fresh instance is created per [`client_config`] call (i.e. per
/// connection attempt), so there is no cross-attempt leakage.
#[derive(Clone, Default, Debug)]
pub struct CertFailure(Arc<Mutex<Option<rustls::Error>>>);

impl CertFailure {
    fn record(&self, e: &rustls::Error) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.clone());
    }

    /// Takes and clears the last recorded verification failure, if any.
    fn take(&self) -> Option<rustls::Error> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl ServerCertVerifier for ReportingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .inspect_err(|e| self.last_failure.record(e))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        self.inner.requires_raw_public_keys()
    }

    fn root_hint_subjects(&self) -> Option<&[rustls::DistinguishedName]> {
        self.inner.root_hint_subjects()
    }
}

/// Builds the QUIC client configuration for ALPN `icu/1` with the client
/// certificate. Never installs a process wide crypto provider. The returned
/// [`CertFailure`] captures the specific TLS error (if any) from the next
/// handshake attempt made with this config, for [`connect`] to inspect.
pub fn client_config(identity: &Identity) -> Result<(quinn::ClientConfig, CertFailure), NetError> {
    // The server would refuse an expired certificate with a bare TLS alert,
    // which reads like a network error and would be retried forever. No
    // reconnect can fix it, so say so before trying.
    check_not_expired(&identity.leaf_der, unix_now())?;
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(identity.ca_der.clone()))
        .map_err(|e| NetError::Tls(format!("ca: {e}")))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let webpki_verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .map_err(|e| NetError::Tls(e.to_string()))?;
    let cert_failure = CertFailure::default();
    let verifier = Arc::new(ReportingVerifier {
        inner: webpki_verifier,
        last_failure: cert_failure.clone(),
    });

    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(
            vec![CertificateDer::from(identity.leaf_der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der.clone())),
        )
        .map_err(|e| NetError::Tls(format!("client auth: {e}")))?;
    tls.alpn_protocols = vec![pl::ALPN_MAIN.to_vec()];
    tls.enable_early_data = false;

    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|e| NetError::Tls(e.to_string()))?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(pl::KEEP_ALIVE_INTERVAL_S)));
    transport.max_idle_timeout(Some(
        Duration::from_secs(pl::MAX_IDLE_TIMEOUT_S)
            .try_into()
            .expect("idle timeout fits"),
    ));
    transport.datagram_receive_buffer_size(Some(pl::DATAGRAM_BUFFER_BYTES));
    transport.datagram_send_buffer_size(pl::DATAGRAM_BUFFER_BYTES);
    transport.max_concurrent_uni_streams(quinn::VarInt::from_u32(pl::max_uni_streams(
        pl::MAX_MAX_USERS,
    )));
    config.transport_config(Arc::new(transport));
    Ok((config, cert_failure))
}

/// Refuses a client certificate whose `notAfter` lies before `now` (Unix
/// seconds). A leaf that does not parse is left to the handshake.
fn check_not_expired(leaf_der: &[u8], now: i64) -> Result<(), NetError> {
    let Ok((_, cert)) = x509_parser::parse_x509_certificate(leaf_der) else {
        return Ok(());
    };
    let not_after = cert.validity().not_after.timestamp();
    if not_after < now {
        let what = if crate::net::register::is_registration(leaf_der) {
            "Die Einladung ist abgelaufen — bitte eine neue anfordern"
        } else {
            "Das eigene Zertifikat ist abgelaufen — Passport eines anderen Geräts \
             importieren oder eine neue Einladung anfordern"
        };
        return Err(NetError::Tls(format!(
            "{what} (gültig bis {})",
            cert.validity().not_after
        )));
    }
    Ok(())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Turns a certificate-verification failure into a clear, user-facing reason.
fn describe_certificate_error(ce: &rustls::CertificateError) -> String {
    match ce {
        rustls::CertificateError::NotValidForName
        | rustls::CertificateError::NotValidForNameContext { .. } => {
            "Hostname stimmt nicht mit dem Serverzertifikat überein".to_string()
        }
        other => format!("Zertifikatsprüfung fehlgeschlagen: {other:?}"),
    }
}

/// What `Welcome` told us.
#[derive(Debug, Clone)]
pub struct Welcome {
    /// Negotiated minor.
    pub minor: u16,
    /// Capability bits the server announced. All zero in v1.
    pub caps: u32,
    /// Own entry.
    pub you: icu_proto::UserInfo,
    /// Audio parameters.
    pub audio: icu_proto::AudioParams,
    /// Everyone else.
    pub users: Vec<icu_proto::UserInfo>,
    /// Lowest allowed client version.
    pub min_client: String,
    /// Whether this client may send `CreateInviteRequest`.
    pub is_admin: bool,
    /// Authenticated with a registration certificate: nothing works until it
    /// is swapped (`net::register::complete`).
    pub registration: bool,
}

/// An established, welcomed connection.
pub struct Session {
    /// The QUIC connection.
    pub conn: quinn::Connection,
    /// Control stream, send half.
    pub send: quinn::SendStream,
    /// Control stream, receive half.
    pub recv: quinn::RecvStream,
    /// The `Welcome`.
    pub welcome: Welcome,
}

/// Connects, opens the control stream, sends `Hello` at once and waits for
/// `Welcome`. Only then is the client "connected" (Spike 2).
#[allow(clippy::too_many_arguments)]
pub async fn connect(
    endpoint: &quinn::Endpoint,
    config: quinn::ClientConfig,
    addr: SocketAddr,
    server_name: &str,
    client: &str,
    initial: UserFlags,
    name: &str,
    cert_failure: &CertFailure,
) -> Result<Session, ConnectError> {
    let connecting = endpoint
        .connect_with(config, addr, server_name)
        .map_err(|e| ConnectError::Transport(e.to_string()))?;
    let handshake = tokio::time::timeout(Duration::from_secs(10), connecting)
        .await
        .map_err(|_| ConnectError::Transport("handshake timed out".into()))?;
    let conn = match handshake {
        Ok(c) => c,
        Err(e) => {
            return Err(match cert_failure.take() {
                Some(rustls::Error::InvalidCertificate(ce)) => ConnectError::Certificate {
                    server_name: server_name.to_string(),
                    detail: describe_certificate_error(&ce),
                },
                _ => ConnectError::from_connection_error(e),
            });
        }
    };
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(ConnectError::from_connection_error)?;
    let hello = ControlMsg::Hello {
        minor: icu_proto::MINOR,
        caps: icu_proto::CAPS,
        client: client.to_string(),
        initial: initial.sanitized(),
        name: name.to_string(),
    };
    write_msg(&mut send, &hello)
        .await
        .map_err(|e| ConnectError::Transport(e.to_string()))?;
    let body = tokio::time::timeout(
        Duration::from_secs(pl::HELLO_TIMEOUT_S + 5),
        read_frame(&mut recv, MAX_CONTROL_FRAME),
    )
    .await
    .map_err(|_| ConnectError::Transport("no Welcome".into()))?
    .map_err(|e| match conn.close_reason() {
        Some(reason) => ConnectError::from_connection_error(reason),
        None => ConnectError::Transport(e.to_string()),
    })?;
    match ControlMsg::decode(&body).map_err(|e| ConnectError::Protocol(e.to_string()))? {
        ControlMsg::Welcome {
            minor,
            caps,
            you,
            audio,
            users,
            min_client,
            is_admin,
            registration,
        } => Ok(Session {
            conn,
            send,
            recv,
            welcome: Welcome {
                minor,
                caps,
                you,
                audio,
                users,
                min_client,
                is_admin,
                registration,
            },
        }),
        other => Err(ConnectError::Protocol(format!(
            "expected Welcome, got {}",
            other.variant()
        ))),
    }
}

/// Writes one control message.
pub async fn write_msg(send: &mut quinn::SendStream, msg: &ControlMsg) -> Result<(), NetError> {
    let frame = msg.encode().map_err(|e| NetError::Other(e.to_string()))?;
    send.write_all(&frame)
        .await
        .map_err(|e| NetError::Connect(e.to_string()))
}

/// Why a connection attempt or a session ended.
#[derive(Debug, Clone)]
pub enum ConnectError {
    /// Closed by the server with an application code.
    Closed {
        /// The code.
        code: u32,
        /// The reason string.
        reason: String,
    },
    /// Transport failure (timeout, reset, unreachable).
    Transport(String),
    /// We could not make sense of what the server sent.
    Protocol(String),
    /// TLS certificate verification failed for `server_name`. Never
    /// retryable: retrying will not fix a hostname/cert mismatch.
    Certificate {
        /// The FQDN/IP TLS verification ran against.
        server_name: String,
        /// What went wrong.
        detail: String,
    },
}

impl ConnectError {
    /// Maps a quinn error.
    pub fn from_connection_error(e: quinn::ConnectionError) -> ConnectError {
        match e {
            quinn::ConnectionError::ApplicationClosed(c) => ConnectError::Closed {
                code: u64::from(c.error_code) as u32,
                reason: String::from_utf8_lossy(&c.reason).into_owned(),
            },
            // A TLS alert from the server (QUIC crypto error 0x100–0x1ff): it
            // refused our certificate — unknown CA after a server rebuild, or
            // expired. The same certificate will be refused again.
            quinn::ConnectionError::ConnectionClosed(c)
                if (0x100..0x200).contains(&u64::from(c.error_code)) =>
            {
                ConnectError::Certificate {
                    server_name: String::new(),
                    detail: format!(
                        "Server hat das Client-Zertifikat abgelehnt (TLS-Alert {}): {}",
                        u64::from(c.error_code) - 0x100,
                        String::from_utf8_lossy(&c.reason)
                    ),
                }
            }
            other => ConnectError::Transport(other.to_string()),
        }
    }

    /// Whether the client must not reconnect (`PROTOCOL.md` §5).
    pub fn is_fatal(&self) -> bool {
        match self {
            ConnectError::Closed { code, .. } => icu_proto::error::is_fatal(*code),
            ConnectError::Transport(_) => false,
            ConnectError::Protocol(_) => true,
            ConnectError::Certificate { .. } => true,
        }
    }

    /// The application close code, if any.
    pub fn code(&self) -> Option<u32> {
        match self {
            ConnectError::Closed { code, .. } => Some(*code),
            _ => None,
        }
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Closed { code, reason } => {
                write!(f, "{} ({code}) {reason}", icu_proto::error::name(*code))
            }
            ConnectError::Transport(s) => write!(f, "transport: {s}"),
            ConnectError::Protocol(s) => write!(f, "protocol: {s}"),
            ConnectError::Certificate {
                server_name,
                detail,
            } if server_name.is_empty() => f.write_str(detail),
            ConnectError::Certificate {
                server_name,
                detail,
            } => write!(f, "Zertifikat für „{server_name}“ ungültig: {detail}"),
        }
    }
}

/// Backoff schedule for reconnects: 1, 2, 4, 8, 15, 15, … seconds.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(match attempt {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 8,
        _ => 15,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_schedule() {
        let s: Vec<u64> = (0..7).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(s, vec![1, 2, 4, 8, 15, 15, 15]);
    }

    #[test]
    fn fatality_follows_the_protocol_table() {
        let closed = |code| ConnectError::Closed {
            code,
            reason: String::new(),
        };
        assert!(closed(icu_proto::error::KICKED).is_fatal());
        assert!(closed(icu_proto::error::AUTH_REVOKED).is_fatal());
        assert!(!closed(icu_proto::error::SHUTDOWN).is_fatal());
        assert!(!closed(icu_proto::error::SERVER_FULL).is_fatal());
        assert!(!closed(icu_proto::error::HELLO_TIMEOUT).is_fatal());
        assert!(!ConnectError::Transport("x".into()).is_fatal());
        assert!(ConnectError::Protocol("x".into()).is_fatal());
        assert!(ConnectError::Certificate {
            server_name: "x".into(),
            detail: "y".into(),
        }
        .is_fatal());
    }

    #[test]
    fn an_expired_client_certificate_is_refused_locally() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.not_after = rcgen::date_time_ymd(2030, 1, 1);
        let der = params.self_signed(&key).unwrap().der().to_vec();
        let before = 1_893_456_000 - 86_400; // 2029-12-31
        let after = 1_893_456_000 + 86_400; // 2030-01-02
        assert!(check_not_expired(&der, before).is_ok());
        let err = check_not_expired(&der, after).unwrap_err().to_string();
        assert!(err.contains("abgelaufen"), "{err}");
        assert!(check_not_expired(b"junk", after).is_ok());
    }
}
