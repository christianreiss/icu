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

/// Per-field defaults, so that a config file may omit any single key.
///
/// Each function returns the value from the section's `Default` impl, which is
/// the single source of truth for the spec values.
mod d {
    use super::*;

    pub fn listen() -> Vec<SocketAddr> {
        ServerSection::default().listen
    }
    pub fn server_name() -> String {
        ServerSection::default().server_name
    }
    pub fn public_host() -> String {
        ServerSection::default().public_host
    }
    pub fn max_users() -> u16 {
        ServerSection::default().max_users
    }
    pub fn min_client() -> String {
        ServerSection::default().min_client
    }
    pub fn audio_datagrams_per_s() -> u32 {
        LimitsSection::default().audio_datagrams_per_s
    }
    pub fn control_msgs_per_s() -> u32 {
        LimitsSection::default().control_msgs_per_s
    }
    pub fn set_state_per_s() -> u32 {
        LimitsSection::default().set_state_per_s
    }
    pub fn video_frames_per_s() -> u32 {
        LimitsSection::default().video_frames_per_s
    }
    pub fn video_kbit_s() -> u32 {
        LimitsSection::default().video_kbit_s
    }
    pub fn enroll_per_ip_per_min() -> u32 {
        LimitsSection::default().enroll_per_ip_per_min
    }
    pub fn enroll_global_per_min() -> u32 {
        LimitsSection::default().enroll_global_per_min
    }
    pub fn handshakes_per_ip_per_s() -> u32 {
        LimitsSection::default().handshakes_per_ip_per_s
    }
    pub fn ca_validity_days() -> u32 {
        TlsSection::default().ca_validity_days
    }
    pub fn client_validity_days() -> u32 {
        TlsSection::default().client_validity_days
    }
    pub fn server_validity_days() -> u32 {
        TlsSection::default().server_validity_days
    }
    pub fn level() -> String {
        LogSection::default().level
    }
    pub fn format() -> String {
        LogSection::default().format
    }
    pub fn state_dir() -> PathBuf {
        PathsSection::default().state_dir
    }
    pub fn runtime_dir() -> PathBuf {
        PathsSection::default().runtime_dir
    }
    pub fn hello_timeout_ms() -> u64 {
        TimingSection::default().hello_timeout_ms
    }
    pub fn speaking_silence_ms() -> u64 {
        TimingSection::default().speaking_silence_ms
    }
    pub fn loss_report_ttl_ms() -> u64 {
        TimingSection::default().loss_report_ttl_ms
    }
    pub fn keyframe_min_interval_ms() -> u64 {
        TimingSection::default().keyframe_min_interval_ms
    }
    pub fn rate_violation_window_ms() -> u64 {
        TimingSection::default().rate_violation_window_ms
    }
    pub fn shutdown_grace_ms() -> u64 {
        TimingSection::default().shutdown_grace_ms
    }
    pub fn max_idle_timeout_ms() -> u64 {
        TimingSection::default().max_idle_timeout_ms
    }
}

/// Complete server configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Listen addresses, room size, names.
    #[serde(default)]
    pub server: ServerSection,
    /// Rate limits, defaults from `PROTOCOL.md` §7.
    #[serde(default)]
    pub limits: LimitsSection,
    /// Certificate validity periods.
    #[serde(default)]
    pub tls: TlsSection,
    /// Logging.
    #[serde(default)]
    pub log: LogSection,
    /// State and runtime paths. Not in the spec; needed to run out of tree.
    #[serde(default)]
    pub paths: PathsSection,
    /// Timeouts the spec fixes. Not in the spec; the defaults are the spec
    /// values, tests shorten them.
    #[serde(default)]
    pub timing: TimingSection,
}

/// `[server]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    /// UDP addresses to bind. One endpoint per entry.
    #[serde(default = "d::listen")]
    pub listen: Vec<SocketAddr>,
    /// SNI in the server certificate and in the invite link.
    #[serde(default = "d::server_name")]
    pub server_name: String,
    /// `host:port` as it appears in the invite link.
    #[serde(default = "d::public_host")]
    pub public_host: String,
    /// Room size, 1..=64.
    #[serde(default = "d::max_users")]
    pub max_users: u16,
    /// Lowest allowed client version, SemVer.
    #[serde(default = "d::min_client")]
    pub min_client: String,
    /// Group that owns the admin socket. `None` leaves the group alone.
    #[serde(default)]
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
#[serde(deny_unknown_fields)]
pub struct LimitsSection {
    /// Audio datagrams per second and client.
    #[serde(default = "d::audio_datagrams_per_s")]
    pub audio_datagrams_per_s: u32,
    /// Control messages per second and client.
    #[serde(default = "d::control_msgs_per_s")]
    pub control_msgs_per_s: u32,
    /// `SetState` messages per second and client.
    #[serde(default = "d::set_state_per_s")]
    pub set_state_per_s: u32,
    /// Video frames per second and client.
    #[serde(default = "d::video_frames_per_s")]
    pub video_frames_per_s: u32,
    /// Video bitrate per sender, sliding over 2 s.
    #[serde(default = "d::video_kbit_s")]
    pub video_kbit_s: u32,
    /// Enrollments per minute and IP.
    #[serde(default = "d::enroll_per_ip_per_min")]
    pub enroll_per_ip_per_min: u32,
    /// Enrollments per minute, server wide.
    #[serde(default = "d::enroll_global_per_min")]
    pub enroll_global_per_min: u32,
    /// Handshakes per second and IP.
    #[serde(default = "d::handshakes_per_ip_per_s")]
    pub handshakes_per_ip_per_s: u32,
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
        }
    }
}

/// `[tls]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsSection {
    /// CA validity in days.
    #[serde(default = "d::ca_validity_days")]
    pub ca_validity_days: u32,
    /// Client certificate validity in days.
    #[serde(default = "d::client_validity_days")]
    pub client_validity_days: u32,
    /// Server certificate validity in days.
    #[serde(default = "d::server_validity_days")]
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
#[serde(deny_unknown_fields)]
pub struct LogSection {
    /// `tracing` filter string.
    #[serde(default = "d::level")]
    pub level: String,
    /// `"text"` or `"json"`.
    #[serde(default = "d::format")]
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
#[serde(deny_unknown_fields)]
pub struct PathsSection {
    /// Directory holding `ca.key`, `users.toml`, `state.lock`, …
    #[serde(default = "d::state_dir")]
    pub state_dir: PathBuf,
    /// Directory holding `admin.sock`.
    #[serde(default = "d::runtime_dir")]
    pub runtime_dir: PathBuf,
    /// Explicit admin socket path. Defaults to `runtime_dir/admin.sock`.
    #[serde(default)]
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
#[serde(deny_unknown_fields)]
pub struct TimingSection {
    /// Milliseconds a client has to send `Hello`.
    #[serde(default = "d::hello_timeout_ms")]
    pub hello_timeout_ms: u64,
    /// Silence after which a talk spurt ends.
    #[serde(default = "d::speaking_silence_ms")]
    pub speaking_silence_ms: u64,
    /// Lifetime of a `LossReport`.
    #[serde(default = "d::loss_report_ttl_ms")]
    pub loss_report_ttl_ms: u64,
    /// Minimum interval between forwarded `KeyframeRequest`s per speaker.
    #[serde(default = "d::keyframe_min_interval_ms")]
    pub keyframe_min_interval_ms: u64,
    /// Window in which rate limit violations are counted.
    #[serde(default = "d::rate_violation_window_ms")]
    pub rate_violation_window_ms: u64,
    /// How long shutdown waits after sending `SHUTDOWN`.
    #[serde(default = "d::shutdown_grace_ms")]
    pub shutdown_grace_ms: u64,
    /// Idle timeout announced to peers.
    #[serde(default = "d::max_idle_timeout_ms")]
    pub max_idle_timeout_ms: u64,
}

impl Default for TimingSection {
    fn default() -> Self {
        TimingSection {
            hello_timeout_ms: pl::HELLO_TIMEOUT_S * 1000,
            speaking_silence_ms: pl::SPEAKING_SILENCE_MS,
            loss_report_ttl_ms: pl::LOSS_REPORT_TTL_S * 1000,
            keyframe_min_interval_ms: 1000,
            rate_violation_window_ms: pl::RATE_VIOLATION_WINDOW_S * 1000,
            shutdown_grace_ms: 1000,
            max_idle_timeout_ms: pl::MAX_IDLE_TIMEOUT_S * 1000,
        }
    }
}

impl TimingSection {
    /// `hello_timeout_ms` as a `Duration`.
    pub fn hello_timeout(&self) -> Duration {
        Duration::from_millis(self.hello_timeout_ms)
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

impl Config {
    /// Loads and validates a configuration file.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .context(format!("cannot read config {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).context(format!("cannot parse config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validates the configuration (`Server/IMPLEMENTATION.md` §6.1.1).
    pub fn validate(&self) -> Result<()> {
        if self.server.listen.is_empty() {
            return Err(Error::msg("server.listen is empty"));
        }
        if self.server.server_name.trim().is_empty() {
            return Err(Error::msg("server.server_name must not be empty"));
        }
        if self.server.max_users < 1 || self.server.max_users > pl::MAX_MAX_USERS {
            return Err(Error::msg(format!(
                "server.max_users must be 1..={}, got {}",
                pl::MAX_MAX_USERS,
                self.server.max_users
            )));
        }
        Version::parse(&self.server.min_client)
            .context("server.min_client is not a semver version")?;
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

    /// `host:port` for invite links; falls back to `server_name` and the port
    /// of the first listen address.
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
        format!("{}:{}", self.server.server_name, port)
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
        let config: Config = toml::from_str(text).unwrap();
        config.validate().unwrap();
        assert_eq!(config.server.listen.len(), 2);
        assert_eq!(config.server.max_users, 16);
        assert_eq!(config.public_host(), "icu.example.org:4711");
        assert_eq!(config.admin_socket(), Path::new("/run/icu/admin.sock"));
    }

    #[test]
    fn validation_rejects_bad_values() {
        let mut c = base();
        c.server.server_name = String::new();
        assert!(c.validate().is_err());

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
        c.log.format = "xml".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err =
            toml::from_str::<Config>("[server]\nserver_name = \"x\"\nnope = 1\n").unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
    }
}
