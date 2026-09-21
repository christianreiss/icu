//! Transport: enrollment, the main connection and the control framing.

pub mod conn;
pub mod enroll;

use std::net::SocketAddr;

/// Network failure.
#[derive(Debug, Clone)]
pub enum NetError {
    /// Invite link malformed.
    BadLink(String),
    /// Name resolution.
    Resolve(String),
    /// TLS configuration.
    Tls(String),
    /// Connection setup or transport.
    Connect(String),
    /// The server answered with an error code.
    Rejected {
        /// Error code (`PROTOCOL.md` §6).
        code: u32,
        /// Detail.
        detail: String,
    },
    /// Anything else.
    Other(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::BadLink(s) => write!(f, "invalid invite link: {s}"),
            NetError::Resolve(s) => write!(f, "cannot resolve: {s}"),
            NetError::Tls(s) => write!(f, "tls: {s}"),
            NetError::Connect(s) => write!(f, "connection: {s}"),
            NetError::Rejected { code, detail } => {
                write!(f, "{} ({code}): {detail}", icu_proto::error::name(*code))
            }
            NetError::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for NetError {}

/// Resolves `host:port` to the first address.
pub async fn resolve(host: &str, port: u16) -> Result<SocketAddr, NetError> {
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| NetError::Resolve(format!("{host}: {e}")))?;
    // Prefer IPv4 when both exist, so a v4-only path never loses to an
    // unreachable v6 route.
    let all: Vec<SocketAddr> = addrs.by_ref().collect();
    all.iter()
        .find(|a| a.is_ipv4())
        .or_else(|| all.first())
        .copied()
        .ok_or_else(|| NetError::Resolve(format!("{host}: no address")))
}

/// Binds a client endpoint in the address family of `remote`.
pub fn bind_endpoint(remote: SocketAddr) -> Result<quinn::Endpoint, NetError> {
    let bind: SocketAddr = if remote.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    quinn::Endpoint::client(bind).map_err(|e| NetError::Connect(format!("bind: {e}")))
}

/// Reads one length prefixed frame (`PROTOCOL.md` §2).
pub async fn read_frame(recv: &mut quinn::RecvStream, max: usize) -> Result<Vec<u8>, NetError> {
    let mut prefix = [0u8; 2];
    recv.read_exact(&mut prefix)
        .await
        .map_err(|e| NetError::Connect(e.to_string()))?;
    let len = u16::from_le_bytes(prefix) as usize;
    icu_proto::framing::check_len(len, max).map_err(|e| NetError::Other(e.to_string()))?;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| NetError::Connect(e.to_string()))?;
    Ok(body)
}
