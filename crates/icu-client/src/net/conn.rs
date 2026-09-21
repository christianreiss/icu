//! The main connection: client config with the client certificate, connect,
//! `Hello`/`Welcome`, control stream helpers (`PROTOCOL.md` §1, §2, §5).

use std::{net::SocketAddr, sync::Arc, time::Duration};

use icu_proto::{limits as pl, ControlMsg, UserFlags, MAX_CONTROL_FRAME};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::config::Identity;

use super::{read_frame, NetError};

/// Builds the QUIC client configuration for ALPN `icu/1` with the client
/// certificate. Never installs a process wide crypto provider.
pub fn client_config(identity: &Identity) -> Result<quinn::ClientConfig, NetError> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(identity.ca_der.clone()))
        .map_err(|e| NetError::Tls(format!("ca: {e}")))?;
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| NetError::Tls(e.to_string()))?
    .with_root_certificates(roots)
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
    Ok(config)
}

/// What `Welcome` told us.
#[derive(Debug, Clone)]
pub struct Welcome {
    /// Negotiated minor.
    pub minor: u16,
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
pub async fn connect(
    endpoint: &quinn::Endpoint,
    config: quinn::ClientConfig,
    addr: SocketAddr,
    server_name: &str,
    client: &str,
    initial: UserFlags,
    name: &str,
) -> Result<Session, ConnectError> {
    let connecting = endpoint
        .connect_with(config, addr, server_name)
        .map_err(|e| ConnectError::Transport(e.to_string()))?;
    let conn = tokio::time::timeout(Duration::from_secs(10), connecting)
        .await
        .map_err(|_| ConnectError::Transport("handshake timed out".into()))?
        .map_err(ConnectError::from_connection_error)?;
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
            you,
            audio,
            users,
            min_client,
            is_admin,
            ..
        } => Ok(Session {
            conn,
            send,
            recv,
            welcome: Welcome {
                minor,
                you,
                audio,
                users,
                min_client,
                is_admin,
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
}

impl ConnectError {
    /// Maps a quinn error.
    pub fn from_connection_error(e: quinn::ConnectionError) -> ConnectError {
        match e {
            quinn::ConnectionError::ApplicationClosed(c) => ConnectError::Closed {
                code: u64::from(c.error_code) as u32,
                reason: String::from_utf8_lossy(&c.reason).into_owned(),
            },
            other => ConnectError::Transport(other.to_string()),
        }
    }

    /// Whether the client must not reconnect (`PROTOCOL.md` §5).
    pub fn is_fatal(&self) -> bool {
        match self {
            ConnectError::Closed { code, .. } => icu_proto::error::is_fatal(*code),
            ConnectError::Transport(_) => false,
            ConnectError::Protocol(_) => true,
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
    }
}
