//! Server configuration (`Server/IMPLEMENTATION.md` §4).
//!
//! The on-disk format is `/etc/icu/server.toml`. Two sections beyond the spec
//! exist so that the server can run as a library in integration tests:
//! `[paths]` (state and runtime directory) and `[timing]` (timeouts that the
//! spec fixes, with the spec values as defaults).

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use icu_proto::{limits as pl, Version};
use serde::{Deserialize, Serialize};

use crate::error::{Context, Error, Result};

/// Complete server configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Listen addresses, room size, names.
    pub server: ServerSection,
    /// Rate limits, defaults from `PROTOCOL.md` §7.
    pub limits: LimitsSection,
    /// Certificate validity periods.
    pub tls: TlsSection,
    /// Logging.
    pub log: LogSection,
    /// State and runtime paths. Not in the spec; needed to run out of tree.
    pub paths: PathsSection,
    /// Timeouts the spec fixes. Not in the spec; the defaults are the spec
    /// values, tests shorten them.
    pub timing: TimingSection,
}

/// `[server]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    /// UDP addresses to bind. One endpoint per entry.
    pub listen: Vec<SocketAddr>,
    /// SNI in the server certificate; also part of the CA's CN, which is
    /// where clients take their SNI from.
    pub server_name: String,
    /// `host:port` clients dial. Written into every client certificate as
    /// URI SAN `icu://<public_host>` (`PROTOCOL.md` §2.3).
    pub public_host: String,
    /// Room size, 1..=64.
    pub max_users: u16,
    /// Lowest allowed client version, SemVer.
    pub min_client: String,
    /// Group that owns the admin socket. `None` leaves the group alone.
    pub admin_socket_group: Option<String>,
}

impl Default for ServerSection {
    fn default() -> Self {
        ServerSection {
            listen: vec![
                SocketAddr::from(([0, 0, 0, 0], pl::DEFAULT_PORT)),
                SocketAddr::from(([0u16; 8], pl::DEFAULT_PORT)),
            ],
            server_name: String::new(),
            public_host: String::new(),
            max_users: pl::DEFAULT_MAX_USERS,
            min_client: "0.1.0".to_string(),
            admin_socket_group: None,
        }
    }
}

/// `[limits]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsSection {
    /// Audio datagrams per second and client.
    pub audio_datagrams_per_s: u32,
    /// Control messages per second and client.
    pub control_msgs_per_s: u32,
    /// `SetState` messages per second and client.
    pub set_state_per_s: u32,
    /// Video frames per second and client.
    pub video_frames_per_s: u32,
    /// Video bitrate per sender, sliding over 2 s.
    pub video_kbit_s: u32,
    /// Registration sessions per minute and IP.
    pub enroll_per_ip_per_min: u32,
    /// Registration sessions per minute, server wide.
    pub enroll_global_per_min: u32,
    /// Handshakes per second and IP (IPv6: per /64).
    pub handshakes_per_ip_per_s: u32,
    /// Handshakes per second, server wide.
    pub handshakes_global_per_s: u32,
    /// Handshakes in flight, over all listen addresses, before new ones are
    /// refused. Above `HANDSHAKES_IN_FLIGHT_RETRY` (32), or a flood of
    /// unvalidated handshakes could take every slot before Retry sets in.
    pub max_handshakes: u32,
    /// Established connections before a completed handshake is closed with
    /// `SERVER_FULL`; at least `2 x max_users`.
    pub max_connections: u32,
    /// Invitations not yet confirmed, before a new one is refused with
    /// `RATE_LIMITED`. Unset: `2 x max_users`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_pending_invites: Option<u32>,
    /// Bundles one admin may mint per hour in-band (`CreateInviteRequest`).
    pub invites_per_admin_per_hour: u32,
}

impl Default for LimitsSection {
    fn default() -> Self {
        LimitsSection {
            audio_datagrams_per_s: pl::AUDIO_DATAGRAMS_PER_S,
            control_msgs_per_s: pl::CONTROL_MSGS_PER_S,
            set_state_per_s: pl::SET_STATE_PER_S,
            video_frames_per_s: pl::VIDEO_FRAMES_PER_S,
            video_kbit_s: pl::VIDEO_KBIT_S,
            enroll_per_ip_per_min: pl::ENROLL_PER_IP_PER_MIN,
            enroll_global_per_min: pl::ENROLL_GLOBAL_PER_MIN,
            handshakes_per_ip_per_s: pl::HANDSHAKES_PER_IP_PER_S,
            handshakes_global_per_s: pl::HANDSHAKES_GLOBAL_PER_S,
            max_handshakes: pl::MAX_HANDSHAKES,
            max_connections: pl::MAX_CONNECTIONS,
            max_pending_invites: None,
            invites_per_admin_per_hour: pl::INVITES_PER_ADMIN_PER_HOUR,
        }
    }
}

/// `[tls]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TlsSection {
    /// CA validity in days.
    pub ca_validity_days: u32,
    /// Client certificate validity in days.
    pub client_validity_days: u32,
    /// Server certificate validity in days.
    pub server_validity_days: u32,
}

impl Default for TlsSection {
    fn default() -> Self {
        TlsSection {
            ca_validity_days: 3650,
            client_validity_days: 365,
            server_validity_days: 365,
        }
    }
}

/// `[log]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogSection {
    /// `tracing` filter string.
    pub level: String,
    /// `"text"` or `"json"`.
    pub format: String,
}

impl Default for LogSection {
    fn default() -> Self {
        LogSection {
            level: "info".to_string(),
            format: "text".to_string(),
        }
    }
}

/// `[paths]` — beyond the spec, see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathsSection {
    /// Directory holding `ca.key`, `users.toml`, `state.lock`, …
    pub state_dir: PathBuf,
    /// Directory holding `admin.sock`.
    pub runtime_dir: PathBuf,
    /// Explicit admin socket path. Defaults to `runtime_dir/admin.sock`.
    pub admin_socket: Option<PathBuf>,
}

impl Default for PathsSection {
    fn default() -> Self {
        PathsSection {
            state_dir: PathBuf::from("/var/lib/icu"),
            runtime_dir: PathBuf::from("/run/icu"),
            admin_socket: None,
        }
    }
}

/// `[timing]` — beyond the spec, see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimingSection {
    /// Milliseconds a client has to send `Hello`.
    pub hello_timeout_ms: u64,
    /// Silence after which a talk spurt ends.
    pub speaking_silence_ms: u64,
    /// Lifetime of a `LossReport`.
    pub loss_report_ttl_ms: u64,
    /// Minimum interval between forwarded `KeyframeRequest`s per speaker.
    pub keyframe_min_interval_ms: u64,
    /// Window in which rate limit violations are counted.
    pub rate_violation_window_ms: u64,
    /// How long shutdown waits after sending `SHUTDOWN`.
    pub shutdown_grace_ms: u64,
    /// Idle timeout announced to peers.
    pub max_idle_timeout_ms: u64,
    /// How long a QUIC handshake may take.
    pub handshake_timeout_ms: u64,
    /// How long a video frame may take to arrive, header and payload.
    pub video_frame_timeout_ms: u64,
}

impl Default for TimingSection {
    fn default() -> Self {
        TimingSection {
            hello_timeout_ms: pl::HELLO_TIMEOUT_S * 1000,
            speaking_silence_ms: pl::SPEAKING_SILENCE_MS,
            loss_report_ttl_ms: pl::LOSS_REPORT_TTL_S * 1000,
            keyframe_min_interval_ms: 1000 / u64::from(pl::KEYFRAME_REQUESTS_PER_S),
            rate_violation_window_ms: pl::RATE_VIOLATION_WINDOW_S * 1000,
            shutdown_grace_ms: 1000,
            max_idle_timeout_ms: pl::MAX_IDLE_TIMEOUT_S * 1000,
            handshake_timeout_ms: pl::HANDSHAKE_TIMEOUT_S * 1000,
            video_frame_timeout_ms: pl::VIDEO_FRAME_TIMEOUT_S * 1000,
        }
    }
}

impl TimingSection {
    /// `hello_timeout_ms` as a `Duration`.
    pub fn hello_timeout(&self) -> Duration {
        Duration::from_millis(self.hello_timeout_ms)
    }
    /// `handshake_timeout_ms` as a `Duration`.
    pub fn handshake_timeout(&self) -> Duration {
        Duration::from_millis(self.handshake_timeout_ms)
    }
    /// `video_frame_timeout_ms` as a `Duration`.
    pub fn video_frame_timeout(&self) -> Duration {
        Duration::from_millis(self.video_frame_timeout_ms)
    }
    /// `speaking_silence_ms` as a `Duration`.
    pub fn speaking_silence(&self) -> Duration {
        Duration::from_millis(self.speaking_silence_ms)
    }
    /// `loss_report_ttl_ms` as a `Duration`.
    pub fn loss_report_ttl(&self) -> Duration {
        Duration::from_millis(self.loss_report_ttl_ms)
    }
    /// `keyframe_min_interval_ms` as a `Duration`.
    pub fn keyframe_min_interval(&self) -> Duration {
        Duration::from_millis(self.keyframe_min_interval_ms)
    }
    /// `rate_violation_window_ms` as a `Duration`.
    pub fn rate_violation_window(&self) -> Duration {
        Duration::from_millis(self.rate_violation_window_ms)
    }
}

/// Whether `host` is reserved for documentation and testing (RFC 2606,
/// RFC 6761): `example.com`, `.net`, `.org` and their subdomains, and the
/// top-level domains `.example`, `.invalid` and `.test`. `localhost` stays
/// allowed — tests and trial runs on one machine use it — as do IP
/// addresses.
fn reserved_name(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let tld = host.rsplit('.').next().unwrap_or_default();
    matches!(tld, "example" | "invalid" | "test")
        || ["example.com", "example.net", "example.org"]
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// Longest certificate lifetime a configuration may ask for: 100 years. The
/// date arithmetic behind a larger one overflows.
pub const MAX_VALIDITY_DAYS: u32 = 36_500;

/// Longest value any `[timing]` entry may have: an hour. Every one of them
/// is added to a clock reading, which overflows for an absurd value.
pub const MAX_TIMING_MS: u64 = 3_600_000;

impl Config {
    /// Loads and validates a configuration file.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .context(format!("cannot read config {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).context(format!("cannot parse config {}", path.display()))?;
        config.validate()?;
        config.validate_lifetimes()?;
        Ok(config)
    }

    /// Certificate lifetimes of at least a day. A certificate that expires
    /// as it is issued would send a client with an expired one into a renewal
    /// loop (`PROTOCOL.md` §2.3.3). Checked for configuration files only: the
    /// in-process tests issue already expired certificates on purpose.
    pub fn validate_lifetimes(&self) -> Result<()> {
        for (key, days) in [
            ("tls.ca_validity_days", self.tls.ca_validity_days),
            ("tls.client_validity_days", self.tls.client_validity_days),
            ("tls.server_validity_days", self.tls.server_validity_days),
        ] {
            if days < 1 {
                return Err(Error::msg(format!("{key} must be at least 1, got {days}")));
            }
        }
        Ok(())
    }

    /// Validates the configuration (`Server/IMPLEMENTATION.md` §6.1.1).
    pub fn validate(&self) -> Result<()> {
        if self.server.listen.is_empty() {
            return Err(Error::msg("server.listen is empty"));
        }
        // Upper bounds, before anything computes with them: an absurd
        // lifetime or timeout overflows date or clock arithmetic, which is a
        // panic, not an error.
        for (key, days) in [
            ("tls.ca_validity_days", self.tls.ca_validity_days),
            ("tls.client_validity_days", self.tls.client_validity_days),
            ("tls.server_validity_days", self.tls.server_validity_days),
        ] {
            if days > MAX_VALIDITY_DAYS {
                return Err(Error::msg(format!(
                    "{key} must be at most {MAX_VALIDITY_DAYS}, got {days}"
                )));
            }
        }
        let t = &self.timing;
        for (key, ms) in [
            ("timing.hello_timeout_ms", t.hello_timeout_ms),
            ("timing.speaking_silence_ms", t.speaking_silence_ms),
            ("timing.loss_report_ttl_ms", t.loss_report_ttl_ms),
            (
                "timing.keyframe_min_interval_ms",
                t.keyframe_min_interval_ms,
            ),
            (
                "timing.rate_violation_window_ms",
                t.rate_violation_window_ms,
            ),
            ("timing.shutdown_grace_ms", t.shutdown_grace_ms),
            ("timing.max_idle_timeout_ms", t.max_idle_timeout_ms),
            ("timing.handshake_timeout_ms", t.handshake_timeout_ms),
            ("timing.video_frame_timeout_ms", t.video_frame_timeout_ms),
        ] {
            if ms > MAX_TIMING_MS {
                return Err(Error::msg(format!(
                    "{key} must be at most {MAX_TIMING_MS}, got {ms}"
                )));
            }
        }
        let server_name = self.server.server_name.trim();
        if server_name.is_empty() {
            return Err(Error::msg("server.server_name must not be empty"));
        }
        // Must be exactly what a client's `ServerName::try_from` will accept,
        // since that's what TLS hostname verification runs against
        // (`server_name_for`, `icu-client/src/net/mod.rs`) — a value
        // `ca.rs`'s laxer ASCII check would accept but this rejects gets
        // silently rewritten to "localhost" client-side, which is a far more
        // confusing failure than catching it here at startup.
        if rustls::pki_types::ServerName::try_from(server_name.to_string()).is_err() {
            return Err(Error::msg(format!(
                "server.server_name {server_name:?} is not a valid DNS name or IP address"
            )));
        }
        // A name reserved for examples is not refused here but where it would
        // be baked in anew (`Config::reserved_names`): an existing install
        // already carrying one has to keep starting.
        if self.server.max_users < 1 || self.server.max_users > pl::MAX_MAX_USERS {
            return Err(Error::msg(format!(
                "server.max_users must be 1..={}, got {}",
                pl::MAX_MAX_USERS,
                self.server.max_users
            )));
        }
        // Goes into every client certificate as `icu://<public_host>`, and a
        // client splits it at the last colon.
        let public_host = self.public_host();
        // An IPv6 literal needs brackets, or the split lands inside it.
        let port_ok = public_host.rsplit_once(':').is_some_and(|(host, port)| {
            let bare_v6 = host.contains(':') && !(host.starts_with('[') && host.ends_with(']'));
            !host.is_empty() && !bare_v6 && port.parse::<u16>().is_ok()
        });
        if !public_host.is_ascii() || public_host.contains(char::is_whitespace) || !port_ok {
            return Err(Error::msg(format!(
                "server.public_host {public_host:?} must be ASCII host:port"
            )));
        }
        Version::parse(&self.server.min_client)
            .context("server.min_client is not a semver version")?;
        // A ceiling at or below the Retry threshold would let unvalidated
        // handshakes, which may come from spoofed addresses, take every slot.
        if self.limits.max_handshakes as usize <= pl::HANDSHAKES_IN_FLIGHT_RETRY {
            return Err(Error::msg(format!(
                "limits.max_handshakes must be above {}, got {}",
                pl::HANDSHAKES_IN_FLIGHT_RETRY,
                self.limits.max_handshakes
            )));
        }
        if self.limits.max_pending_invites == Some(0) {
            return Err(Error::msg(
                "limits.max_pending_invites must be at least 1 (unset: 2 x max_users)",
            ));
        }
        // Room for everybody plus connections that have not said `Hello`
        // yet, or a full server would turn away its own users.
        if self.limits.max_connections < u32::from(self.server.max_users) * 2 {
            return Err(Error::msg(format!(
                "limits.max_connections must be at least 2 x max_users = {}, got {}",
                u32::from(self.server.max_users) * 2,
                self.limits.max_connections
            )));
        }
        if !pl::welcome_fits(self.server.max_users) {
            return Err(Error::msg(format!(
                "a full Welcome for {} users does not fit into a {} byte control frame",
                self.server.max_users,
                icu_proto::MAX_CONTROL_FRAME
            )));
        }
        if self.log.format != "text" && self.log.format != "json" {
            return Err(Error::msg(format!(
                "log.format must be \"text\" or \"json\", got {:?}",
                self.log.format
            )));
        }
        Ok(())
    }

    /// `host:port` for client certificates; falls back to `server_name` and
    /// the port of the first listen address. An IPv6 literal needs brackets.
    pub fn public_host(&self) -> String {
        if !self.server.public_host.is_empty() {
            return self.server.public_host.clone();
        }
        let port = self
            .server
            .listen
            .first()
            .map(|a| a.port())
            .unwrap_or(pl::DEFAULT_PORT);
        let host = &self.server.server_name;
        if host.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        }
    }

    /// The names reserved for examples ([`reserved_name`]) among
    /// `server_name` and the host of `public_host`, `server_name` first, each
    /// as the message refusing it. The shipped example must not end up in the
    /// CA and every certificate: fixing that later means a new CA, and every
    /// user registering again. So they are refused where a name is baked in
    /// for the first time — a new CA (`crate::open_ca`), the first invitation
    /// (`registration::issue`) — and only warned about otherwise: an install
    /// whose CA and certificates already carry one must keep starting.
    pub fn reserved_names(&self) -> Vec<String> {
        let server_name = self.server.server_name.trim();
        let server_name = reserved_name(server_name).then(|| {
            format!(
                "server.server_name {server_name:?} is a name reserved for examples \
                 (RFC 2606); set it to the DNS name or IP address clients connect to, \
                 before the first start or invite: it goes into the CA and every \
                 certificate"
            )
        });
        server_name
            .into_iter()
            .chain(self.reserved_public_host())
            .collect()
    }

    /// The part of [`Config::reserved_names`] about `public_host`, which goes
    /// into every client certificate.
    pub fn reserved_public_host(&self) -> Option<String> {
        let public_host = self.public_host();
        let host = public_host
            .rsplit_once(':')
            .map_or(public_host.as_str(), |(host, _)| host);
        reserved_name(host.trim_start_matches('[').trim_end_matches(']')).then(|| {
            format!(
                "server.public_host {public_host:?} names a host reserved for examples \
                 (RFC 2606); set it to the host:port clients dial, it goes into every \
                 client certificate"
            )
        })
    }

    /// How many invitations may be open (issued, not confirmed) at once:
    /// `limits.max_pending_invites`, else `2 x max_users`.
    pub fn max_pending_invites(&self) -> usize {
        self.limits
            .max_pending_invites
            .unwrap_or(2 * u32::from(self.server.max_users)) as usize
    }

    /// Path of the admin socket.
    pub fn admin_socket(&self) -> PathBuf {
        self.paths
            .admin_socket
            .clone()
            .unwrap_or_else(|| self.paths.runtime_dir.join("admin.sock"))
    }

    /// A configuration suitable for tests: loopback, ephemeral port, state in
    /// `dir`, short timeouts.
    pub fn for_test(dir: &Path) -> Config {
        let mut config = Config {
            server: ServerSection {
                listen: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
                server_name: "localhost".to_string(),
                public_host: "localhost:4711".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        config.paths.state_dir = dir.to_path_buf();
        config.paths.runtime_dir = dir.to_path_buf();
        config.timing.shutdown_grace_ms = 50;
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config::for_test(Path::new("/tmp"))
    }

    #[test]
    fn defaults_are_the_protocol_values() {
        let limits = LimitsSection::default();
        assert_eq!(limits.audio_datagrams_per_s, 110);
        assert_eq!(limits.control_msgs_per_s, 50);
        assert_eq!(limits.video_kbit_s, 400);
        assert_eq!(ServerSection::default().max_users, 16);
        assert_eq!(TimingSection::default().hello_timeout().as_secs(), 5);
    }

    #[test]
    fn spec_example_parses() {
        // The example from Server/IMPLEMENTATION.md §4, verbatim.
        let text = r#"
[server]
listen = ["0.0.0.0:4711", "[::]:4711"]
server_name = "icu.example.org"
public_host = "icu.example.org:4711"
max_users = 16
min_client = "0.1.0"
admin_socket_group = "icu"

[limits]
audio_datagrams_per_s = 110
control_msgs_per_s = 50
video_kbit_s = 400
enroll_per_ip_per_min = 5
enroll_global_per_min = 20

[tls]
ca_validity_days = 3650
client_validity_days = 365
server_validity_days = 365

[log]
level = "info"
format = "text"
"#;
        let mut config: Config = toml::from_str(text).unwrap();
        // The example names are placeholders: taken as they are, they would
        // end up in a new CA and every certificate, which refuses them. An
        // install whose CA already carries them still loads its config.
        config.validate().unwrap();
        let err = &config.reserved_names()[0];
        assert!(err.contains("server.server_name"), "{err}");
        config.server.server_name = "icu.home.arpa".into();
        let err = &config.reserved_names()[0];
        assert!(err.contains("server.public_host"), "{err}");
        config.server.public_host = "icu.home.arpa:4711".into();
        assert!(config.reserved_names().is_empty());
        config.validate().unwrap();
        assert_eq!(config.server.listen.len(), 2);
        assert_eq!(config.server.max_users, 16);
        assert_eq!(config.public_host(), "icu.home.arpa:4711");
        assert_eq!(config.admin_socket(), Path::new("/run/icu/admin.sock"));
    }

    #[test]
    fn reserved_example_names_are_refused() {
        for (name, reserved) in [
            ("icu.example.org", true),
            ("example.com", true),
            ("EXAMPLE.NET.", true),
            ("chat.example", true),
            ("host.invalid", true),
            ("icu.test", true),
            ("localhost", false),
            ("127.0.0.1", false),
            ("::1", false),
            ("icu.home.arpa", false),
            ("myexample.org", false),
            ("example.org.de", false),
            ("testing.net", false),
        ] {
            assert_eq!(reserved_name(name), reserved, "{name}");
        }
        let mut c = base();
        c.server.server_name = "icu.example.org".into();
        c.server.public_host = "localhost:4711".into();
        let err = c.reserved_names().join("; ");
        assert!(err.contains("server.server_name"), "{err}");
        assert!(!err.contains("server.public_host"), "{err}");
        assert!(
            c.validate().is_ok(),
            "refused for a new CA, not in the config"
        );
        let mut c = base();
        c.server.public_host = "icu.test:4711".into();
        let err = c.reserved_names().join("; ");
        assert!(err.contains("server.public_host"), "{err}");
        assert_eq!(c.reserved_public_host().as_ref(), Some(&err));
        let mut c = base();
        c.server.public_host = String::new();
        assert!(c.reserved_names().is_empty(), "falls back to localhost");
        c.server.server_name = "icu.example.org".into();
        assert!(
            c.reserved_public_host().is_some(),
            "the fallback carries the reserved server_name"
        );
    }

    #[test]
    fn validation_rejects_bad_values() {
        let mut c = base();
        c.server.server_name = String::new();
        assert!(c.validate().is_err());

        let mut c = base();
        c.server.server_name = "not a host!".into();
        assert!(c.validate().is_err());

        let mut c = base();
        c.server.server_name = "192.0.2.10".into();
        assert!(c.validate().is_ok());

        let mut c = base();
        c.server.server_name = "::1".into();
        assert!(c.validate().is_ok());

        let mut c = base();
        c.server.max_users = 0;
        assert!(c.validate().is_err());
        c.server.max_users = 65;
        assert!(c.validate().is_err());
        c.server.max_users = 64;
        assert!(c.validate().is_ok());

        let mut c = base();
        c.server.min_client = "nope".into();
        assert!(c.validate().is_err());

        let mut c = base();
        c.server.listen.clear();
        assert!(c.validate().is_err());

        let mut c = base();
        c.tls.client_validity_days = 0;
        assert!(c.validate_lifetimes().is_err());
        c.tls.client_validity_days = 1;
        assert!(c.validate_lifetimes().is_ok());
        c.tls.server_validity_days = 0;
        assert!(c.validate_lifetimes().is_err());

        let mut c = base();
        c.limits.max_connections = 31;
        assert!(c.validate().is_err(), "below 2 x max_users");

        let mut c = base();
        c.limits.max_handshakes = 32;
        assert!(c.validate().is_err(), "not above the Retry threshold");
        c.limits.max_handshakes = 33;
        assert!(c.validate().is_ok());

        let mut c = base();
        c.log.format = "xml".into();
        assert!(c.validate().is_err());

        // Upper bounds: absurd values would overflow date or clock
        // arithmetic, a panic rather than an error.
        let mut c = base();
        c.tls.client_validity_days = MAX_VALIDITY_DAYS;
        assert!(c.validate().is_ok());
        c.tls.client_validity_days = u32::MAX;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("tls.client_validity_days"), "{err}");
        let mut c = base();
        c.tls.ca_validity_days = MAX_VALIDITY_DAYS + 1;
        assert!(c.validate().is_err());
        let mut c = base();
        c.timing.handshake_timeout_ms = MAX_TIMING_MS;
        assert!(c.validate().is_ok());
        c.timing.handshake_timeout_ms = u64::MAX;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("timing.handshake_timeout_ms"), "{err}");
        let mut c = base();
        c.timing.hello_timeout_ms = MAX_TIMING_MS + 1;
        assert!(c.validate().is_err());

        // `public_host` goes into client certificates as `icu://host:port`.
        for (bad, ok) in [
            ("icu.example.org", false),
            ("::1:4711", false),
            ("host name:4711", false),
            ("[::1]:4711", true),
            ("192.0.2.10:4711", true),
        ] {
            let mut c = base();
            c.server.public_host = bad.into();
            assert_eq!(c.validate().is_ok(), ok, "{bad}");
        }

        // The fallback brackets an IPv6 server name itself.
        let mut c = base();
        c.server.server_name = "::1".into();
        c.server.public_host = String::new();
        assert!(c.public_host().starts_with("[::1]:"), "{}", c.public_host());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err =
            toml::from_str::<Config>("[server]\nserver_name = \"x\"\nnope = 1\n").unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
    }
}
