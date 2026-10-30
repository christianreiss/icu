//! The main connection: client config with the client certificate, connect,
//! `Hello`/`Welcome`, control stream helpers (`docs/reference/protocol.md` §1, §2, §5).

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use icu_proto::{limits as pl, ControlMsg, UserFlags, MAX_CONTROL_FRAME};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

use crate::{
    config::Identity,
    problem::{self, Problem},
    sync::lock,
};

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
pub struct CertFailure {
    failure: Arc<Mutex<Option<rustls::Error>>>,
    /// The server's `CertificateVerify` checked out: from here on a close
    /// comes from the server, not from whoever can read our Initial packets.
    proven: Arc<AtomicBool>,
}

impl CertFailure {
    fn record(&self, e: &rustls::Error) {
        *lock(&self.failure) = Some(e.clone());
    }

    /// Takes and clears the last recorded verification failure, if any.
    fn take(&self) -> Option<rustls::Error> {
        lock(&self.failure).take()
    }

    /// Whether the server proved it holds the key of its certificate.
    fn server_proven(&self) -> bool {
        self.proven.load(Ordering::Acquire)
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
        // Every handshake runs this: each config is fresh, so there is no
        // session to resume without the server's signature.
        self.inner
            .verify_tls13_signature(message, cert, dss)
            .inspect(|_| self.last_failure.proven.store(true, Ordering::Release))
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
    client_config_with_crl(identity, None)
}

/// [`client_config`], with the server leaf also checked against `crl`, the
/// newest CRL the server sent (`crate::crl`). A leaf on it fails the
/// handshake like any other certificate error.
pub fn client_config_with_crl(
    identity: &Identity,
    crl: Option<&[u8]>,
) -> Result<(quinn::ClientConfig, CertFailure), NetError> {
    // The server would refuse an expired certificate with a bare TLS alert,
    // which reads like a network error and would be retried forever. No
    // reconnect can fix it, so say so before trying.
    check_not_expired(&identity.leaf_der, unix_now())?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let webpki_verifier = server_verifier(&identity.ca_der, crl, provider.clone())?;
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
    transport.datagram_send_buffer_size(pl::CLIENT_DATAGRAM_SEND_BUFFER_BYTES);
    transport.datagram_send_max_age(Some(Duration::from_millis(pl::AUDIO_QUEUE_MAX_AGE_MS)));
    transport.max_concurrent_uni_streams(quinn::VarInt::from_u32(pl::max_uni_streams(
        pl::MAX_MAX_USERS,
    )));
    // The com tag rides on every datagram (`docs/reference/protocol.md` §1).
    transport.mtu_discovery_config(Some(icu_proto::com::mtu_discovery()));
    config.transport_config(Arc::new(transport));
    Ok((config, cert_failure))
}

/// The check of the server's certificate: against our CA only and, with a
/// CRL, against that for the leaf (`docs/reference/protocol.md` §1). Also how `crate::crl`
/// finds out whether rustls takes a CRL at all, so the trial and the real
/// build cannot drift apart.
pub(crate) fn server_verifier(
    ca_der: &[u8],
    crl: Option<&[u8]>,
    provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<Arc<rustls::client::WebPkiServerVerifier>, NetError> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca_der.to_vec()))
        .map_err(|e| NetError::Tls(format!("ca: {e}")))?;
    let mut builder =
        rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider);
    if let Some(crl) = crl {
        builder = builder
            .with_crls([rustls::pki_types::CertificateRevocationListDer::from(
                crl.to_vec(),
            )])
            .only_check_end_entity_revocation();
    }
    builder.build().map_err(|e| NetError::Tls(e.to_string()))
}

/// Refuses a registration certificate whose `notAfter` lies before `now`
/// (Unix seconds): an expired invitation is spent. An expired certificate of
/// our own is let through: the server admits it to a renew-only session
/// (`docs/reference/protocol.md` §2.3). A leaf that does not parse is left to the handshake.
fn check_not_expired(leaf_der: &[u8], now: i64) -> Result<(), NetError> {
    if !crate::net::register::is_registration(leaf_der) || !leaf_expired(leaf_der, now) {
        return Ok(());
    }
    let until = x509_parser::parse_x509_certificate(leaf_der)
        .map(|(_, cert)| cert.validity().not_after.to_string())
        .unwrap_or_default();
    Err(NetError::Expired(format!(
        "Die Einladung ist abgelaufen — bitte eine neue anfordern (gültig bis {until})"
    )))
}

/// Whether a certificate's `notAfter` lies before `now` (Unix seconds).
/// `false` when it does not parse.
pub fn leaf_expired(leaf_der: &[u8], now: i64) -> bool {
    x509_parser::parse_x509_certificate(leaf_der)
        .is_ok_and(|(_, cert)| cert.validity().not_after.timestamp() < now)
}

/// Seconds since the epoch, 0 if the clock is before it.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Turns a failed check of the *server's* certificate into a clear,
/// user-facing reason. None of them is fatal: rustls checks the certificate
/// before the server's `CertificateVerify` signature, so whoever sent it has
/// proven nothing yet (`docs/reference/protocol.md` §1). Expired or revoked are the server
/// operator's to fix (automatic rotation, `rotate-server-cert`): the client
/// keeps retrying and picks up the new leaf, while verification still
/// refuses the old one. Not valid yet is most likely this machine's clock.
/// Everything else — wrong CA, wrong name — is retried slowly
/// ([`ConnectError::Unverified`]).
fn certificate_error(server_name: &str, ce: &rustls::CertificateError) -> ConnectError {
    match ce {
        rustls::CertificateError::Expired | rustls::CertificateError::ExpiredContext { .. } => {
            ConnectError::ServerCert(
                "Serverzertifikat abgelaufen – warte auf Erneuerung durch den Server".into(),
            )
        }
        rustls::CertificateError::Revoked => ConnectError::ServerCert(
            "Serverzertifikat widerrufen – warte auf ein neues vom Server".into(),
        ),
        rustls::CertificateError::NotValidYet
        | rustls::CertificateError::NotValidYetContext { .. } => {
            ConnectError::NotYetValid(describe_certificate_error(ce))
        }
        rustls::CertificateError::NotValidForName
        | rustls::CertificateError::NotValidForNameContext { .. } => ConnectError::Unverified {
            server_name: server_name.to_string(),
            issue: CertIssue::NameMismatch,
            detail: describe_certificate_error(ce),
        },
        _ => ConnectError::Unverified {
            server_name: server_name.to_string(),
            issue: CertIssue::Untrusted,
            detail: describe_certificate_error(ce),
        },
    }
}

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
    /// Capability bits the server announced; bit 0 enables audio feedback.
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
    /// The server's own protocol minor.
    pub server_minor: u16,
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
            // Proof first: what the server says after its signature counts,
            // our own verdict on a certificate nobody vouched for does not.
            let failure = cert_failure.take();
            return Err(if cert_failure.server_proven() {
                ConnectError::from_connection_error(e)
            } else if let Some(rustls::Error::InvalidCertificate(ce)) = failure {
                certificate_error(server_name, &ce)
            } else {
                ConnectError::unauthenticated(e)
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
    // A server that refuses us may close while `Hello` is still on its way;
    // its code decides whether to retry, as on the read below.
    write_msg(&mut send, &hello)
        .await
        .map_err(|e| ConnectError::on(&conn, e))?;
    let body = tokio::time::timeout(
        Duration::from_secs(pl::HELLO_TIMEOUT_S + 5),
        read_frame(&mut recv, MAX_CONTROL_FRAME),
    )
    .await
    .map_err(|_| ConnectError::Transport("no Welcome".into()))?
    .map_err(|e| match e {
        NetError::Protocol(s) => ConnectError::Protocol(s),
        e => ConnectError::on(&conn, e),
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
            server_minor,
        } => {
            if let Err(e) = check_server(server_minor) {
                conn.close(
                    icu_proto::error::VERSION_UNSUPPORTED.into(),
                    b"server protocol minor below minimum",
                );
                return Err(e);
            }
            Ok(Session {
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
                    server_minor,
                },
            })
        }
        other => Err(ConnectError::Protocol(format!(
            "expected Welcome, got {}",
            other.variant()
        ))),
    }
}

/// The client's half of the version gate (`docs/reference/protocol.md` §8): the server
/// checked our `Hello.minor` and version, we check its `Welcome.server_minor`.
fn check_server(server_minor: u16) -> Result<(), ConnectError> {
    if server_minor < icu_proto::MIN_PEER_MINOR {
        return Err(ConnectError::ServerTooOld(server_minor));
    }
    Ok(())
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
    /// The server's own certificate is expired or revoked: the operator's to
    /// fix, and it heals on its own once the server rotates it. Not fatal.
    ServerCert(String),
    /// The server's certificate is not valid yet: most likely this machine's
    /// clock is behind (a dual boot keeping local time, right after the
    /// server rotated its leaf). Heals with the clock, so not fatal.
    NotYetValid(String),
    /// The server's certificate failed our check — another CA, another
    /// name — before the server proved it holds the key. Anyone on the path
    /// can present such a certificate (with the com key, or control of DNS),
    /// so it must not end the reconnects or send the user off for a new
    /// invitation, which is what an attacker would hand them next
    /// (`docs/reference/protocol.md` §1). Retried with [`slow_backoff`].
    Unverified {
        /// The FQDN/IP TLS verification ran against.
        server_name: String,
        /// Which kind of failure, for the user-facing text.
        issue: CertIssue,
        /// What went wrong.
        detail: String,
    },
    /// We could not make sense of what the server sent.
    Protocol(String),
    /// The server speaks another protocol version: TLS alert
    /// `protocol_version` or `no_application_protocol` (ALPN, `docs/reference/protocol.md`
    /// §8). Fatal: it takes an update, not a retry.
    Version(String),
    /// Like [`ConnectError::Version`], but the alert came before the server
    /// proved who it is (`protocol_version` and `no_application_protocol`
    /// are sent in reply to our first packet): shown the same, retried with
    /// backoff, because anyone on the path could have sent it.
    VersionHint(String),
    /// The server's protocol minor is below [`icu_proto::MIN_PEER_MINOR`]:
    /// the server needs the update, not this client. Fatal.
    ServerTooOld(u16),
    /// The server, after proving who it is, refused our certificate with a
    /// TLS alert (revoked, unknown). Fatal: the same certificate would be
    /// refused again. `server_name` is empty: the alert names none.
    Certificate {
        /// The FQDN/IP TLS verification ran against.
        server_name: String,
        /// Which kind of failure, for the user-facing text.
        issue: CertIssue,
        /// What went wrong.
        detail: String,
    },
}

/// Why a certificate check failed, as far as the user is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertIssue {
    /// The server's certificate is not for the address we dialled.
    NameMismatch,
    /// The server lists our certificate as revoked.
    RevokedByServer,
    /// The two sides do not trust each other: a different CA, typically
    /// after the server was set up anew.
    Untrusted,
}

impl ConnectError {
    /// Why a read or write on `conn` failed: the server's close code when
    /// it closed the connection, otherwise the transport error `e`.
    pub fn on(conn: &quinn::Connection, e: impl std::fmt::Display) -> ConnectError {
        match conn.close_reason() {
            Some(reason) => ConnectError::from_connection_error(reason),
            None => ConnectError::Transport(e.to_string()),
        }
    }

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
                let alert = u64::from(c.error_code) - 0x100;
                match alert {
                    // certificate_revoked: the server's CRL lists our leaf
                    // (`docs/reference/protocol.md` §1).
                    44 => ConnectError::Certificate {
                        server_name: String::new(),
                        issue: CertIssue::RevokedByServer,
                        detail: "Der Server hat dieses Zertifikat widerrufen.".into(),
                    },
                    // protocol_version, no_application_protocol: the server
                    // speaks another TLS or ICU major version (ALPN, §8) —
                    // nothing wrong with the certificate, and only an update
                    // can fix it.
                    70 | 120 => ConnectError::Version(format!(
                        "TLS-Alert {alert}: {}",
                        String::from_utf8_lossy(&c.reason)
                    )),
                    _ => ConnectError::Certificate {
                        server_name: String::new(),
                        issue: CertIssue::Untrusted,
                        detail: format!(
                            "Server hat das Client-Zertifikat abgelehnt (TLS-Alert {alert}): {}",
                            String::from_utf8_lossy(&c.reason)
                        ),
                    },
                }
            }
            other => ConnectError::Transport(other.to_string()),
        }
    }

    /// Maps a close that ended the handshake before the server's signature
    /// was checked. The keys of Initial packets follow from the connection
    /// ID, so such a close proves nothing about the server: a forged
    /// `certificate_revoked` must not end the reconnects for good. A real
    /// server sends a certificate alert only after its own flight, and an
    /// application close only after the handshake (RFC 9000 §10.2.3), so
    /// only the version alerts can be genuine here.
    pub fn unauthenticated(e: quinn::ConnectionError) -> ConnectError {
        match ConnectError::from_connection_error(e.clone()) {
            ConnectError::Version(v) => ConnectError::VersionHint(v),
            _ => ConnectError::Transport(format!("vor der Server-Authentifizierung: {e}")),
        }
    }

    /// Whether the client must not reconnect (`docs/reference/protocol.md` §5).
    pub fn is_fatal(&self) -> bool {
        match self {
            ConnectError::Closed { code, .. } => icu_proto::error::is_fatal(*code),
            ConnectError::Transport(_)
            | ConnectError::ServerCert(_)
            | ConnectError::NotYetValid(_)
            | ConnectError::Unverified { .. }
            | ConnectError::VersionHint(_) => false,
            ConnectError::Protocol(_)
            | ConnectError::Version(_)
            | ConnectError::ServerTooOld(_) => true,
            ConnectError::Certificate { .. } => true,
        }
    }

    /// Whether the next attempt waits by [`slow_backoff`] instead of
    /// [`backoff`]: a certificate nobody vouched for does not heal within
    /// seconds, and each attempt would only hand the path another handshake.
    pub fn retries_slowly(&self) -> bool {
        matches!(self, ConnectError::Unverified { .. })
    }

    /// What the user reads (`crate::problem`); `Display` stays technical
    /// for the log.
    pub fn problem(&self) -> Problem {
        let technical = self.to_string();
        match self {
            ConnectError::Closed { code, reason } => problem::from_code(*code, reason),
            ConnectError::Transport(_) => Problem::new(problem::UNREACHABLE, technical),
            ConnectError::ServerCert(_) => Problem::new(
                "Das Zertifikat des Servers ist abgelaufen oder gesperrt. Das muss der \
                 Server-Betreiber beheben – ICU versucht es weiter.",
                technical,
            ),
            ConnectError::NotYetValid(_) => Problem::new(
                "Das Zertifikat des Servers gilt erst ab später. Meist geht die Uhr dieses \
                 Rechners nach – bitte die Systemuhr prüfen. ICU versucht es weiter.",
                technical,
            ),
            ConnectError::Unverified { issue, .. } => match issue {
                CertIssue::NameMismatch => Problem::new(problem::SERVER_NAME_UNVERIFIED, technical),
                CertIssue::RevokedByServer | CertIssue::Untrusted => {
                    Problem::new(problem::SERVER_UNVERIFIED, technical)
                }
            },
            ConnectError::Protocol(_) => Problem::new(problem::UPDATE_NEEDED, technical),
            ConnectError::Version(_) | ConnectError::VersionHint(_) => Problem::new(
                problem::code_text(icu_proto::error::VERSION_UNSUPPORTED),
                technical,
            ),
            ConnectError::ServerTooOld(_) => Problem::new(problem::SERVER_TOO_OLD, technical),
            ConnectError::Certificate { issue, .. } => match issue {
                CertIssue::NameMismatch => Problem::new(
                    "Die Serveradresse passt nicht zum Zertifikat des Servers. Bitte beim \
                     Admin eine neue Einladung anfordern.",
                    technical,
                ),
                CertIssue::RevokedByServer => Problem::new(
                    problem::code_text(icu_proto::error::AUTH_REVOKED),
                    technical,
                ),
                CertIssue::Untrusted => Problem::new(problem::CREDENTIALS_STALE, technical),
            },
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
            ConnectError::Closed { code, .. } if *code == icu_proto::error::SESSION_REPLACED => {
                f.write_str("Diese Identität wurde auf einem anderen Gerät verbunden.")
            }
            ConnectError::Closed { code, reason } => {
                write!(f, "{} ({code}) {reason}", icu_proto::error::name(*code))
            }
            ConnectError::Transport(s) => write!(f, "transport: {s}"),
            ConnectError::ServerCert(s) => f.write_str(s),
            ConnectError::NotYetValid(s) => write!(f, "Serverzertifikat noch nicht gültig: {s}"),
            ConnectError::Unverified {
                server_name,
                detail,
                ..
            } => write!(
                f,
                "Zertifikat für „{server_name}“ ungültig (Server nicht ausgewiesen): {detail}"
            ),
            ConnectError::Protocol(s) => write!(f, "protocol: {s}"),
            ConnectError::Version(s) => write!(f, "version: {s}"),
            ConnectError::VersionHint(s) => write!(f, "version (unbestätigt): {s}"),
            ConnectError::ServerTooOld(minor) => write!(
                f,
                "server protocol minor {minor} is below the minimum {}",
                icu_proto::MIN_PEER_MINOR
            ),
            ConnectError::Certificate {
                server_name,
                detail,
                ..
            } if server_name.is_empty() => f.write_str(detail),
            ConnectError::Certificate {
                server_name,
                detail,
                ..
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

/// Backoff schedule after a certificate the server did not vouch for
/// ([`ConnectError::retries_slowly`]): 15, 30, 60, 120, 240, 300, 300, …
/// seconds. „Neu verbinden“ tries at once.
pub fn slow_backoff(attempt: u32) -> Duration {
    Duration::from_secs((15u64 << attempt.min(5)).min(300))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_schedule() {
        let s: Vec<u64> = (0..7).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(s, vec![1, 2, 4, 8, 15, 15, 15]);
        let s: Vec<u64> = (0..8).map(|a| slow_backoff(a).as_secs()).collect();
        assert_eq!(s, vec![15, 30, 60, 120, 240, 300, 300, 300]);
        assert_eq!(slow_backoff(u32::MAX).as_secs(), 300);
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
            issue: CertIssue::Untrusted,
            detail: "y".into(),
        }
        .is_fatal());
    }

    #[test]
    fn an_expired_or_revoked_server_certificate_is_retried() {
        let expired = certificate_error("x", &rustls::CertificateError::Expired);
        assert!(matches!(expired, ConnectError::ServerCert(_)));
        assert!(!expired.is_fatal());
        assert!(!expired.retries_slowly());
        assert!(!certificate_error("x", &rustls::CertificateError::Revoked).is_fatal());
    }

    /// Our own verdict on the server's certificate comes before the server
    /// proved anything (review 2026-09-27, C-1): a wrong CA or name is
    /// retried slowly, never fatal, and does not ask for a new invitation.
    #[test]
    fn a_certificate_the_server_did_not_vouch_for_is_retried_slowly() {
        for ce in [
            rustls::CertificateError::NotValidForName,
            rustls::CertificateError::UnknownIssuer,
            rustls::CertificateError::BadSignature,
            rustls::CertificateError::BadEncoding,
        ] {
            let e = certificate_error("icu.example.org", &ce);
            assert!(
                matches!(e, ConnectError::Unverified { .. }),
                "{ce:?}: {e:?}"
            );
            assert!(!e.is_fatal(), "{ce:?}");
            assert!(e.retries_slowly(), "{ce:?}");
            let p = e.problem();
            crate::problem::tests::assert_readable(&p);
            assert!(p.text.contains("anderem Weg"), "{}", p.text);
            assert!(!p.text.contains("neue Einladung"), "{}", p.text);
            assert!(p.technical.unwrap().contains("icu.example.org"));
        }
    }

    /// A server leaf that is not valid yet reads as this machine's clock
    /// being behind, and is retried like an expired one — with the real
    /// verifier, the way a rotation an hour ago meets a slow clock.
    #[test]
    fn a_server_certificate_from_the_future_asks_to_check_the_clock() {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["icu.example.org".into()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2030, 1, 1);
        params.not_after = rcgen::date_time_ymd(2031, 1, 1);
        let leaf = params.signed_by(&key, &issuer).unwrap();

        let verifier = server_verifier(
            ca.der(),
            None,
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .unwrap();
        let before = UnixTime::since_unix_epoch(Duration::from_secs(1_893_456_000 - 3600));
        let err = verifier
            .verify_server_cert(
                leaf.der(),
                &[],
                &ServerName::try_from("icu.example.org").unwrap(),
                &[],
                before,
            )
            .unwrap_err();
        let rustls::Error::InvalidCertificate(ce) = err else {
            panic!("{err:?}");
        };
        let e = certificate_error("icu.example.org", &ce);
        assert!(matches!(e, ConnectError::NotYetValid(_)), "{e:?}");
        assert!(!e.is_fatal());
        assert!(!e.retries_slowly(), "heals with the clock");
        let p = e.problem();
        crate::problem::tests::assert_readable(&p);
        assert!(p.text.contains("Systemuhr"), "{}", p.text);
    }

    /// A TLS alert from the server, as quinn reports it.
    fn alert(code: u8) -> ConnectError {
        ConnectError::from_connection_error(quinn::ConnectionError::ConnectionClosed(
            quinn::ConnectionClose {
                error_code: quinn::TransportErrorCode::crypto(code),
                frame_type: None,
                reason: Default::default(),
            },
        ))
    }

    /// A server below our minimum protocol minor is fatal and blamed on the
    /// server; one at or above it passes.
    #[test]
    fn server_minor_below_the_minimum_is_fatal() {
        let e = check_server(icu_proto::MIN_PEER_MINOR - 1).unwrap_err();
        assert!(matches!(e, ConnectError::ServerTooOld(_)), "{e:?}");
        assert!(e.is_fatal());
        assert_eq!(e.problem().text, problem::SERVER_TOO_OLD);
        crate::problem::tests::assert_readable(&e.problem());
        assert!(check_server(icu_proto::MIN_PEER_MINOR).is_ok());
        assert!(check_server(icu_proto::MINOR).is_ok());
        assert!(check_server(u16::MAX).is_ok());
    }

    /// `protocol_version` and `no_application_protocol` mean another
    /// protocol version, not a certificate problem: the user is told to
    /// update, and nothing is retried.
    #[test]
    fn version_alerts_ask_for_an_update() {
        for code in [70, 120] {
            let e = alert(code);
            assert!(matches!(e, ConnectError::Version(_)), "{code}: {e:?}");
            assert!(e.is_fatal());
            let p = e.problem();
            assert_eq!(
                p.text,
                problem::code_text(icu_proto::error::VERSION_UNSUPPORTED)
            );
            assert!(p.technical.unwrap().contains(&code.to_string()));
            crate::problem::tests::assert_readable(&e.problem());
        }
        assert!(matches!(
            alert(44),
            ConnectError::Certificate {
                issue: CertIssue::RevokedByServer,
                ..
            }
        ));
        assert!(matches!(
            alert(48),
            ConnectError::Certificate {
                issue: CertIssue::Untrusted,
                ..
            }
        ));
    }

    /// A TLS alert that ended the handshake before the server's signature
    /// was checked, as quinn reports it.
    fn early_alert(code: u8) -> ConnectError {
        ConnectError::unauthenticated(quinn::ConnectionError::ConnectionClosed(
            quinn::ConnectionClose {
                error_code: quinn::TransportErrorCode::crypto(code),
                frame_type: None,
                reason: Default::default(),
            },
        ))
    }

    /// Before the server proved itself, an alert is anybody's: a forged
    /// `certificate_revoked` in an Initial packet is a network error, and
    /// the reconnects go on. The version alerts still read as "update", but
    /// are retried too.
    #[test]
    fn alerts_before_server_authentication_are_retried() {
        for code in [44, 48, 42, 80] {
            let e = early_alert(code);
            assert!(matches!(e, ConnectError::Transport(_)), "{code}: {e:?}");
            assert!(!e.is_fatal());
        }
        for code in [70, 120] {
            let e = early_alert(code);
            assert!(matches!(e, ConnectError::VersionHint(_)), "{code}: {e:?}");
            assert!(!e.is_fatal());
            assert_eq!(
                e.problem().text,
                problem::code_text(icu_proto::error::VERSION_UNSUPPORTED)
            );
            crate::problem::tests::assert_readable(&e.problem());
        }
        let kicked = ConnectError::unauthenticated(quinn::ConnectionError::ApplicationClosed(
            quinn::ApplicationClose {
                error_code: quinn::VarInt::from_u32(icu_proto::error::KICKED),
                reason: Default::default(),
            },
        ));
        assert!(!kicked.is_fatal(), "{kicked:?}");
        // After it, the same alert is the server's word.
        assert!(alert(44).is_fatal());
    }

    /// Every connection failure reads as plain German; the raw error stays
    /// in `technical`.
    #[test]
    fn connection_failures_read_as_plain_german() {
        let cases = [
            ConnectError::Closed {
                code: icu_proto::error::AUTH_UNKNOWN_CERT,
                reason: "AUTH_UNKNOWN_CERT".into(),
            },
            ConnectError::Transport("timed out".into()),
            ConnectError::ServerCert("Serverzertifikat abgelaufen".into()),
            ConnectError::Protocol("expected Welcome".into()),
            certificate_error("x", &rustls::CertificateError::UnknownIssuer),
            certificate_error("x", &rustls::CertificateError::NotValidForName),
        ];
        for e in cases {
            let p = e.problem();
            crate::problem::tests::assert_readable(&p);
            assert!(p.technical.is_some(), "{e}");
        }
        let untrusted = certificate_error("x", &rustls::CertificateError::UnknownIssuer).problem();
        assert_eq!(untrusted.text, crate::problem::SERVER_UNVERIFIED);
        assert!(untrusted.technical.unwrap().contains("UnknownIssuer"));
    }

    #[test]
    fn only_an_expired_invitation_is_refused_locally() {
        let leaf = |ou: Option<&str>| {
            let key = rcgen::KeyPair::generate().unwrap();
            let mut params = rcgen::CertificateParams::default();
            params.not_after = rcgen::date_time_ymd(2030, 1, 1);
            if let Some(ou) = ou {
                params
                    .distinguished_name
                    .push(rcgen::DnType::OrganizationalUnitName, ou);
            }
            params.self_signed(&key).unwrap().der().to_vec()
        };
        let before = 1_893_456_000 - 86_400; // 2029-12-31
        let after = 1_893_456_000 + 86_400; // 2030-01-02
        let invite = leaf(Some(crate::net::register::REGISTRATION_OU));
        assert!(check_not_expired(&invite, before).is_ok());
        let err = check_not_expired(&invite, after).unwrap_err().to_string();
        assert!(err.contains("abgelaufen"), "{err}");
        // Our own certificate goes to the server, which renews it.
        let own = leaf(None);
        assert!(check_not_expired(&own, after).is_ok());
        assert!(leaf_expired(&own, after));
        assert!(!leaf_expired(&own, before));
        assert!(check_not_expired(b"junk", after).is_ok());
    }
}
