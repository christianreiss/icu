//! Transport: the main connection, registration and the control framing.

pub mod conn;
pub mod register;

use std::net::SocketAddr;

/// Network failure.
#[derive(Debug, Clone)]
pub enum NetError {
    /// Bundle or name unusable.
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
            NetError::BadLink(s) => write!(f, "invalid bundle or name: {s}"),
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

/// Splits `host[:port]`, `[v6]` or `[v6]:port`; a missing port is the
/// default 4711.
pub fn split_host_port(authority: &str) -> Result<(String, u16), NetError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| NetError::BadLink("bad IPv6 literal".into()))?;
        let host = rest[..end].to_string();
        let port = match rest[end + 1..].strip_prefix(':') {
            Some(p) => p
                .parse()
                .map_err(|_| NetError::BadLink("bad port".into()))?,
            None => icu_proto::limits::DEFAULT_PORT,
        };
        return Ok((host, port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => Ok((
            h.to_string(),
            p.parse()
                .map_err(|_| NetError::BadLink("bad port".into()))?,
        )),
        None => Ok((authority.to_string(), icu_proto::limits::DEFAULT_PORT)),
    }
}

/// SNI for a handshake: `host`, or `localhost` when it is not a valid server
/// name.
pub fn server_name_for(host: &str) -> String {
    if rustls::pki_types::ServerName::try_from(host.to_string()).is_ok() {
        host.to_string()
    } else {
        "localhost".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_forms() {
        assert_eq!(
            split_host_port("icu.example.org:4711").unwrap(),
            ("icu.example.org".to_string(), 4711)
        );
        assert_eq!(split_host_port("[::1]:5000").unwrap(), ("::1".to_string(), 5000));
        assert_eq!(split_host_port("[::1]").unwrap(), ("::1".to_string(), 4711));
        assert_eq!(split_host_port("host").unwrap(), ("host".to_string(), 4711));
        assert!(split_host_port("host:port").is_err());
        assert!(split_host_port("[::1").is_err());
    }
}
