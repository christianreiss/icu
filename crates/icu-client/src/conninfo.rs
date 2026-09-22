//! Everything the Verbindung window shows about the live connection, in the
//! form the UI can render without doing any work itself.
//!
//! Two halves, because they have two very different lifetimes and costs:
//! [`ConnInfo`] is collected once per session (it parses three X.509
//! certificates, which must never happen on a render thread), while
//! [`ConnStats`] is refreshed from `quinn::Connection::stats()` on the
//! 2 s `net_tick` that `crate::netcond` already runs. Both travel to the UI
//! inside `AppState`, like every other piece of state — nothing here reaches
//! into the engine's private `Shared`.

use std::{
    net::{IpAddr, SocketAddr},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use icu_proto::AudioParams;
use sha2::{Digest, Sha256};

use crate::{config::Identity, net::conn::Welcome, netcond::NetworkCondition};

/// Placeholder for any field a malformed certificate does not yield. A
/// cosmetic window must never be able to fail a connection, so every parse
/// step degrades to this instead of erroring.
const UNKNOWN: &str = "unbekannt";

/// What one X.509 certificate says, already rendered to strings. Produced in
/// the engine at welcome time; the UI only formats and copies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// Common name, or the full RDN sequence when there is none.
    pub subject: String,
    /// Issuer common name, same fallback.
    pub issuer: String,
    /// `YYYY-MM-DD`.
    pub not_before: String,
    /// `YYYY-MM-DD`.
    pub not_after: String,
    /// Days until `not_after`; negative once expired.
    pub expires_in_days: i64,
    /// SHA-256 over the DER, as 32 colon-separated uppercase hex pairs.
    pub fingerprint: String,
    /// Signature algorithm short name, e.g. `ecdsa-with-SHA256`.
    pub sig_alg: String,
}

impl Default for CertInfo {
    fn default() -> CertInfo {
        CertInfo {
            subject: UNKNOWN.to_string(),
            issuer: UNKNOWN.to_string(),
            not_before: UNKNOWN.to_string(),
            not_after: UNKNOWN.to_string(),
            expires_in_days: 0,
            fingerprint: UNKNOWN.to_string(),
            sig_alg: UNKNOWN.to_string(),
        }
    }
}

impl CertInfo {
    /// Parses one DER certificate. Never fails: whatever cannot be read
    /// stays `unbekannt`, and the fingerprint is computed over the raw bytes
    /// regardless, since it does not depend on the structure parsing.
    pub fn from_der(der: &[u8]) -> CertInfo {
        let mut out = CertInfo {
            fingerprint: fingerprint(der),
            ..CertInfo::default()
        };
        let Ok((_, cert)) = x509_parser::parse_x509_certificate(der) else {
            return out;
        };
        out.subject = common_name(cert.subject());
        out.issuer = common_name(cert.issuer());
        out.not_before = fmt_date(cert.validity().not_before.timestamp());
        out.not_after = fmt_date(cert.validity().not_after.timestamp());
        out.expires_in_days = (cert.validity().not_after.timestamp() - unix_now()) / 86_400;
        out.sig_alg = x509_parser::objects::oid2sn(
            &cert.signature_algorithm.algorithm,
            x509_parser::objects::oid_registry(),
        )
        .map(str::to_string)
        .unwrap_or_else(|_| cert.signature_algorithm.algorithm.to_id_string());
        out
    }
}

/// Whether `leaf_der`'s public key is the one belonging to `key_der`
/// (PKCS#8).
///
/// Guards the renewal path: persisting a certificate that does not match the
/// local key would make the next connection fail with no way back but a
/// passport from another device.
pub fn leaf_matches_key(leaf_der: &[u8], key_der: &[u8]) -> bool {
    let Ok(key) = rcgen::KeyPair::try_from(key_der) else {
        return false;
    };
    let Ok((_, cert)) = x509_parser::parse_x509_certificate(leaf_der) else {
        return false;
    };
    cert.public_key().subject_public_key.data.as_ref() == key.public_key_raw()
}

/// The certificate's common name, or the whole RDN sequence when it has none.
fn common_name(name: &x509_parser::x509::X509Name<'_>) -> String {
    name.iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string())
}

/// `YYYY-MM-DD` from a Unix timestamp; [`UNKNOWN`] if it is not a real date.
fn fmt_date(unix_s: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(unix_s) {
        Ok(t) => format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day()),
        Err(_) => UNKNOWN.to_string(),
    }
}

/// Seconds since the epoch, 0 if the clock is before it.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// SHA-256 over `der`, as 32 colon-separated uppercase hex pairs — the form
/// `openssl x509 -fingerprint -sha256` prints, so the two can be compared by
/// eye. The client already hashes certificates this way for CA pinning in
/// `net::enroll`.
fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let mut out = String::with_capacity(digest.len() * 3 - 1);
    for (i, b) in digest.iter().enumerate() {
        if i > 0 {
            out.push(':');
        }
        out.push_str(&format!("{b:02X}"));
    }
    out
}

/// The facts that hold still for a whole session: who we are talking to, on
/// what terms, and under which certificates.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnInfo {
    /// Peer address actually connected to (after DNS).
    pub remote: SocketAddr,
    /// Local address the socket ended up on, if the platform reports it.
    pub local_ip: Option<IpAddr>,
    /// Negotiated ALPN, normally `icu/1`.
    pub alpn: String,
    /// `Welcome.minor`.
    pub minor: u16,
    /// `Welcome.caps`.
    pub caps: u32,
    /// `Welcome.min_client`.
    pub min_client: String,
    /// What this client sent as `Hello.client`.
    pub client_string: String,
    /// Audio parameters the server announced.
    pub audio: AudioParams,
    /// When `Welcome` arrived, for the uptime readout.
    pub connected_at: Instant,
    /// The server's leaf certificate.
    pub server_cert: CertInfo,
    /// Our own client certificate.
    pub client_cert: CertInfo,
    /// The pinned CA both sides are verified against.
    pub ca_cert: CertInfo,
}

impl ConnInfo {
    /// Collects everything once, right after `Welcome`. Synchronous and
    /// called from the session task, never from the UI.
    pub fn collect(
        conn: &quinn::Connection,
        welcome: &Welcome,
        identity: &Identity,
        client_string: &str,
    ) -> ConnInfo {
        ConnInfo {
            remote: conn.remote_address(),
            local_ip: conn.local_ip(),
            alpn: alpn(conn),
            minor: welcome.minor,
            caps: welcome.caps,
            min_client: welcome.min_client.clone(),
            client_string: client_string.to_string(),
            audio: welcome.audio,
            connected_at: Instant::now(),
            server_cert: server_cert(conn),
            client_cert: CertInfo::from_der(&identity.leaf_der),
            ca_cert: CertInfo::from_der(&identity.ca_der),
        }
    }
}

/// The negotiated ALPN protocol. quinn hands handshake data back as
/// `Box<dyn Any>`; for the rustls backend it is a
/// `quinn::crypto::rustls::HandshakeData`, whose only client-side field is
/// the protocol (there is deliberately no cipher-suite accessor in quinn
/// 0.11 — see the Verschlüsselung card).
fn alpn(conn: &quinn::Connection) -> String {
    conn.handshake_data()
        .and_then(|d| d.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .and_then(|d| d.protocol.clone())
        .map(|p| String::from_utf8_lossy(&p).into_owned())
        .unwrap_or_else(|| UNKNOWN.to_string())
}

/// The peer's leaf certificate. `peer_identity`'s `Any` is documented as
/// `Vec<CertificateDer>`; the first entry is the leaf.
fn server_cert(conn: &quinn::Connection) -> CertInfo {
    conn.peer_identity()
        .and_then(|i| {
            i.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
                .ok()
        })
        .and_then(|chain| chain.first().map(|leaf| CertInfo::from_der(leaf)))
        .unwrap_or_default()
}

/// The transport numbers, sampled every `net_tick`. Everything here is a
/// plain copy out of `quinn::ConnectionStats` plus the two rates and the
/// already-smoothed [`NetworkCondition`] the quality controller computes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ConnStats {
    /// Smoothed path RTT, ms.
    pub rtt_ms: f32,
    /// Lowest RTT seen on this path, ms.
    pub min_rtt_ms: f32,
    /// Congestion window, bytes.
    pub cwnd: u64,
    /// Cumulative congestion events.
    pub congestion_events: u64,
    /// Times a path black hole was detected.
    pub black_holes: u64,
    /// Cumulative packets lost / sent on this path.
    pub lost_packets: u64,
    /// See [`Self::lost_packets`].
    pub sent_packets: u64,
    /// Cumulative bytes lost.
    pub lost_bytes: u64,
    /// Largest UDP payload the path currently carries.
    pub current_mtu: u16,
    /// Largest datagram we may send right now.
    pub max_datagram_size: Option<usize>,
    /// Cumulative UDP bytes / datagrams, both directions.
    pub udp_tx_bytes: u64,
    /// See [`Self::udp_tx_bytes`].
    pub udp_rx_bytes: u64,
    /// See [`Self::udp_tx_bytes`].
    pub udp_tx_datagrams: u64,
    /// See [`Self::udp_tx_bytes`].
    pub udp_rx_datagrams: u64,
    /// Total wire throughput over the last tick, kbit/s.
    pub tx_kbps: f32,
    /// See [`Self::tx_kbps`].
    pub rx_kbps: f32,
    /// Audio-only throughput over the last tick, kbit/s.
    pub audio_tx_kbps: f32,
    /// See [`Self::audio_tx_kbps`].
    pub audio_rx_kbps: f32,
    /// Video-only throughput over the last tick, kbit/s.
    pub video_tx_kbps: f32,
    /// See [`Self::video_tx_kbps`].
    pub video_rx_kbps: f32,
    /// Own outgoing audio still queued unsent.
    pub datagram_queue_bytes: u64,
    /// The smoothed condition `crate::quality` steers on.
    pub cond: NetworkCondition,
}

impl ConnStats {
    /// Transport loss over the life of the connection, in percent.
    pub fn loss_percent(&self) -> f32 {
        if self.sent_packets == 0 {
            0.0
        } else {
            self.lost_packets as f32 * 100.0 / self.sent_packets as f32
        }
    }
}

/// Shortest interval a rate is computed over. `tokio::time::interval` fires
/// its first tick immediately, so without this the very first sample would
/// divide the whole handshake's bytes by a fraction of a millisecond and
/// report a megabit link.
const MIN_RATE_WINDOW_S: f64 = 0.1;

/// Turns a byte delta over `secs` into kbit/s, or 0 when the window is too
/// short to mean anything.
pub fn kbps(delta_bytes: u64, secs: f64) -> f32 {
    if secs < MIN_RATE_WINDOW_S {
        0.0
    } else {
        (delta_bytes as f64 * 8.0 / secs / 1000.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed certificate, the way `icu-server`'s CA builds one.
    fn test_der(cn: &str) -> Vec<u8> {
        let mut params =
            rcgen::CertificateParams::new(vec!["test.invalid".to_string()]).expect("params");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, cn);
        let key = rcgen::KeyPair::generate().expect("key");
        params
            .self_signed(&key)
            .expect("self signed")
            .der()
            .to_vec()
    }

    #[test]
    fn cert_info_reads_a_real_certificate() {
        let info = CertInfo::from_der(&test_der("alice"));
        assert_eq!(info.subject, "alice");
        assert_eq!(info.issuer, "alice");
        assert_eq!(info.fingerprint.split(':').count(), 32);
        assert!(info
            .fingerprint
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':'));
        assert_eq!(info.not_before.len(), 10);
        assert!(info.sig_alg.contains("SHA"), "sig_alg: {}", info.sig_alg);
    }

    #[test]
    fn garbage_der_degrades_instead_of_panicking() {
        let info = CertInfo::from_der(b"not a certificate");
        assert_eq!(info.subject, UNKNOWN);
        // The fingerprint does not depend on parsing, so it is still real.
        assert_eq!(info.fingerprint.split(':').count(), 32);
    }

    #[test]
    fn fingerprint_matches_openssl_form() {
        // SHA-256 of the empty input, as `openssl dgst -sha256` prints it.
        assert!(fingerprint(b"").starts_with("E3:B0:C4:42:98:FC:1C:14"));
    }

    #[test]
    fn dates_and_rates() {
        assert_eq!(fmt_date(0), "1970-01-01");
        assert_eq!(fmt_date(1_758_500_000), "2025-09-22");
        assert_eq!(kbps(1_000, 2.0), 4.0);
        assert_eq!(kbps(1_000, 0.0), 0.0);
        // The first tick fires immediately; a near-zero window must not be
        // turned into an enormous rate.
        assert_eq!(kbps(8_000, 0.0001), 0.0);
    }

    #[test]
    fn loss_percent_is_zero_without_traffic() {
        assert_eq!(ConnStats::default().loss_percent(), 0.0);
        let s = ConnStats {
            sent_packets: 200,
            lost_packets: 4,
            ..ConnStats::default()
        };
        assert_eq!(s.loss_percent(), 2.0);
    }
}
