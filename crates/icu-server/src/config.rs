//! Server configuration (`Server/IMPLEMENTATION.md` §4).
//!
//! The on-disk format is `/etc/icu/server.toml`. Two sections beyond the spec
//! exist so that the server can run as a library in integration tests:
//! `[paths]` (state and runtime directory) and `[timing]` (timeouts that the
//! spec fixes, with the spec values as defaults). `[backup]` (2026-09-27) is
//! in the spec; its defaults are on, because the packaged config file is not
//! replaced on update and an existing install never gets the section.

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
    /// Automatic snapshots of the state directory (`crate::backup`).
    pub backup: BackupSection,
}

/// `[server]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    /// UDP addresses to bind. One endpoint per entry.
    pub listen: Vec<SocketAddr>,
    /// SNI in the server certificate; also part of the CA's CN, which is
    /// where clients take their SNI from. Unset in a config file: the name
    /// an existing CA carries, else the host's FQDN if it resolves to an
    /// address of this host ([`Config::resolve`]). Set, it must match the
    /// name an existing CA carries (`open_ca` in `lib.rs`).
    pub server_name: String,
    /// `host:port` clients dial. Written into every client certificate as
    /// URI SAN `icu://<public_host>` (`PROTOCOL.md` §2.3).
    pub public_host: String,
    /// Room size, 1..=64.
    pub max_users: u16,
    /// Lowest allowed client version, SemVer.
    pub min_client: String,
    /// Group that owns the admin socket: a name, looked up in `/etc/group`
    /// and then through the host's `getent`, or a gid. `None` leaves the
    /// group alone.
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
    /// Bundles one admin may mint per hour in-band (`CreateInviteRequest`);
    /// 0 turns in-band invitations off, the host's CLI still issues them.
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

/// `[backup]` — automatic snapshots of the state directory after every
/// change (`crate::backup`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BackupSection {
    /// Snapshots kept, the oldest go first. `0` turns the automatic ones
    /// off; `icu-server backup` still writes one.
    pub keep: u32,
    /// Where the snapshots go. Unset: `backups/` in the state directory, the
    /// only place the systemd unit lets the server write besides the state.
    pub dir: Option<PathBuf>,
    /// How long a snapshot waits after a change, so that the writes of one
    /// action — a rotation is five — land in one snapshot.
    pub settle_ms: u64,
}

impl Default for BackupSection {
    fn default() -> Self {
        BackupSection {
            keep: 30,
            dir: None,
            settle_ms: 5000,
        }
    }
}

/// Most snapshots `backup.keep` may ask for.
pub const MAX_BACKUP_KEEP: u32 = 10_000;

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
    /// How long the admin socket waits for a request line, and for the
    /// client to take the answer (`Server/IMPLEMENTATION.md` §5).
    pub admin_timeout_ms: u64,
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
            admin_timeout_ms: 10_000,
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
    /// `admin_timeout_ms` as a `Duration`.
    pub fn admin_timeout(&self) -> Duration {
        Duration::from_millis(self.admin_timeout_ms)
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

/// `server_name` for a config file that leaves it unset: the name an existing
/// CA carries — it is baked in there for good, and a changed hostname must not
/// move it — else the host's FQDN, confirmed by DNS ([`verify_fqdn`]).
pub(crate) fn default_server_name(state_dir: &Path) -> Result<String> {
    if let Some(name) = crate::ca::ca_server_name(state_dir)? {
        return Ok(name);
    }
    let name = host_fqdn();
    verify_fqdn(
        &name,
        |name| {
            use std::net::ToSocketAddrs;
            (name, 0)
                .to_socket_addrs()
                .map(|addrs| addrs.map(|a| a.ip()).collect())
                .unwrap_or_default()
        },
        // Binding succeeds only for an address of this host.
        |ip| std::net::UdpSocket::bind((ip, 0)).is_ok(),
    )
    .map_err(|why| {
        Error::msg(format!(
            "server.server_name is not set and the host's FQDN {name:?} cannot stand \
             in for it: {why}. Set server.server_name (and server.public_host, if \
             clients connect through NAT) to the DNS name or IP address clients \
             connect to"
        ))
    })
}

/// `169.254.0.0/16` or `fe80::/10`: no client elsewhere can dial it.
fn link_local(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_link_local(),
        std::net::IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 == 0xfe80,
    }
}

/// What `hostname --fqdn` prints, else the kernel's host name.
fn host_fqdn() -> String {
    std::process::Command::new("hostname")
        .arg("--fqdn")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            rustix::system::uname()
                .nodename()
                .to_string_lossy()
                .into_owned()
        })
}

/// `name`, normalised, if it can go into a new CA: a dotted DNS name, not
/// `localhost` or reserved for examples, that `resolve`s to at least one
/// address that is neither loopback, unspecified nor link-local and
/// `is_local` to this host — so a name pointing elsewhere, only at Debian's
/// `127.0.1.1` entry in `/etc/hosts`, or only at the `fe80::` address
/// nss-myhostname hands out, is refused. The error says why.
fn verify_fqdn(
    name: &str,
    resolve: impl Fn(&str) -> Vec<std::net::IpAddr>,
    is_local: impl Fn(std::net::IpAddr) -> bool,
) -> std::result::Result<String, String> {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    if name.is_empty() {
        return Err("the host has no name".into());
    }
    if !name.contains('.') || name.parse::<std::net::IpAddr>().is_ok() {
        return Err("it is not a fully qualified domain name".into());
    }
    if name == "localhost" || name.starts_with("localhost.") {
        return Err("it is a name for loopback".into());
    }
    if reserved_name(&name) {
        return Err("it is reserved for examples (RFC 2606)".into());
    }
    let addrs = resolve(&name);
    if addrs.is_empty() {
        return Err("it does not resolve".into());
    }
    if addrs
        .iter()
        .any(|ip| !ip.is_loopback() && !ip.is_unspecified() && !link_local(*ip) && is_local(*ip))
    {
        Ok(name)
    } else {
        let addrs = addrs
            .iter()
            .map(|ip| ip.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "it resolves to {addrs}, none of them a routable address of this host"
        ))
    }
}

/// Whether `value` is a valid `host:port` for a client certificate's
/// `icu://<public_host>` URI SAN: ASCII, no whitespace, and a trailing
/// `:port` a client's last-colon split can recover — an IPv6 literal needs
/// brackets, or the split lands inside it. Shared between [`Config::validate`]
/// and `cli::setup`'s interactive prompt, so both agree on what is valid.
pub(crate) fn valid_public_host(value: &str) -> bool {
    if !value.is_ascii() || value.contains(char::is_whitespace) {
        return false;
    }
    value.rsplit_once(':').is_some_and(|(host, port)| {
        let bare_v6 = host.contains(':') && !(host.starts_with('[') && host.ends_with(']'));
        !host.is_empty() && !bare_v6 && port.parse::<u16>().is_ok()
    })
}

/// Longest certificate lifetime a configuration may ask for: 100 years. The
/// date arithmetic behind a larger one overflows.
pub const MAX_VALIDITY_DAYS: u32 = 36_500;

/// Longest value any `[timing]` entry may have: an hour. Every one of them
/// is added to a clock reading, which overflows for an absurd value.
pub const MAX_TIMING_MS: u64 = 3_600_000;

impl Config {
    /// Loads and validates a configuration file: [`Config::read`], then
    /// [`Config::resolve`].
    pub fn load(path: &Path) -> Result<Config> {
        Config::read(path)?.resolve()
    }

    /// Reads and parses a configuration file without validating it, and
    /// leaves an unset `server_name` unset. Enough for a CLI command that
    /// only talks to the running server: working out the name reads `ca.crt`
    /// in the state directory, which under `DynamicUser` only root can, so
    /// resolving it up front refused `status` to members of the socket's
    /// group and to Checkmk. [`Config::resolve`] completes it where the
    /// configuration is actually used.
    pub fn read(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .context(format!("cannot read config {}", path.display()))?;
        toml::from_str(&text).context(format!("cannot parse config {}", path.display()))
    }

    /// Fills an unset `server_name` ([`default_server_name`]) and validates
    /// the result: what `run` and the offline CLI need.
    pub fn resolve(mut self) -> Result<Config> {
        if self.server.server_name.trim().is_empty() {
            self.server.server_name = default_server_name(&self.paths.state_dir)?;
        }
        self.validate()?;
        self.validate_lifetimes()?;
        Ok(self)
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
            ("timing.admin_timeout_ms", t.admin_timeout_ms),
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
        if !valid_public_host(&public_host) {
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
        if self.backup.keep > MAX_BACKUP_KEEP {
            return Err(Error::msg(format!(
                "backup.keep must be at most {MAX_BACKUP_KEEP}, got {}",
                self.backup.keep
            )));
        }
        if self.backup.settle_ms > MAX_TIMING_MS {
            return Err(Error::msg(format!(
                "backup.settle_ms must be at most {MAX_TIMING_MS}, got {}",
                self.backup.settle_ms
            )));
        }
        if self.backup_dir() == self.paths.state_dir {
            return Err(Error::msg(
                "backup.dir must not be the state directory itself",
            ));
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

    /// Where snapshots go: `backup.dir`, else `backups/` in the state
    /// directory.
    pub fn backup_dir(&self) -> PathBuf {
        self.backup
            .dir
            .clone()
            .unwrap_or_else(|| self.paths.state_dir.join("backups"))
    }

    /// A configuration suitable for tests: loopback, ephemeral port, state in
    /// `dir`, short timeouts, no automatic backups (the backup tests turn
    /// them on).
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
        config.backup.keep = 0;
        config.backup.settle_ms = 50;
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
    fn fqdn_must_resolve_to_this_host() {
        use std::net::IpAddr;
        let local: IpAddr = "192.0.2.10".parse().unwrap();
        let foreign: IpAddr = "198.51.100.7".parse().unwrap();
        let resolve = |_: &str| vec!["127.0.1.1".parse().unwrap(), foreign, local];
        let is_local = |ip: IpAddr| ip == local || ip.is_loopback() || !ip.is_ipv4();
        assert_eq!(
            verify_fqdn("ICU.Home.Arpa.\n", resolve, is_local).unwrap(),
            "icu.home.arpa"
        );
        for (name, addrs, why) in [
            ("", vec![local], "no name"),
            ("srv1", vec![local], "fully qualified"),
            ("192.0.2.10", vec![local], "fully qualified"),
            ("localhost.localdomain", vec![local], "loopback"),
            ("icu.example.org", vec![local], "reserved"),
            ("icu.home.arpa", vec![], "does not resolve"),
            (
                "icu.home.arpa",
                vec!["127.0.1.1".parse().unwrap()],
                "127.0.1.1",
            ),
            ("icu.home.arpa", vec![foreign], "198.51.100.7"),
            ("icu.home.arpa", vec!["fe80::1".parse().unwrap()], "fe80::1"),
        ] {
            let err = verify_fqdn(name, |_| addrs.clone(), is_local).unwrap_err();
            assert!(err.contains(why), "{name}: {err}");
        }
    }

    #[test]
    fn unset_server_name_comes_from_an_existing_ca() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        crate::ca::Ca::load_or_create(&state, &TlsSection::default(), "icu.internal.lan").unwrap();
        let file = dir.path().join("server.toml");
        std::fs::write(
            &file,
            format!("[paths]\nstate_dir = {:?}\n", state.to_str().unwrap()),
        )
        .unwrap();
        let config = Config::load(&file).unwrap();
        assert_eq!(config.server.server_name, "icu.internal.lan");
        assert_eq!(config.public_host(), "icu.internal.lan:4711");

        // A CA from before the name moved into its CN names no server.
        let old = dir.path().join("old");
        std::fs::create_dir_all(&old).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, crate::ca::CA_COMMON_NAME);
        let cert = params.self_signed(&key).unwrap();
        std::fs::write(old.join("ca.crt"), cert.pem()).unwrap();
        std::fs::write(
            &file,
            format!("[paths]\nstate_dir = {:?}\n", old.to_str().unwrap()),
        )
        .unwrap();
        let err = Config::load(&file).unwrap_err().to_string();
        assert!(err.contains("names no server"), "{err}");
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

[backup]
keep = 30
settle_ms = 5000
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
        assert_eq!(config.backup_dir(), Path::new("/var/lib/icu/backups"));
    }

    #[test]
    fn backups_are_on_by_default_and_bounded() {
        // On without a `[backup]` section: an updated install keeps its old
        // config file.
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.backup.keep, 30);
        assert_eq!(config.backup.settle_ms, 5000);
        let parsed: Config =
            toml::from_str("[backup]\nkeep = 0\ndir = \"/srv/icu-backups\"\n").unwrap();
        assert_eq!(parsed.backup.keep, 0);
        assert_eq!(parsed.backup_dir(), Path::new("/srv/icu-backups"));
        assert!(toml::from_str::<Config>("[backup]\nkepp = 3\n").is_err());

        let mut c = base();
        c.backup.keep = MAX_BACKUP_KEEP + 1;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("backup.keep"), "{err}");
        let mut c = base();
        c.backup.settle_ms = MAX_TIMING_MS + 1;
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("backup.settle_ms"), "{err}");
        let mut c = base();
        c.backup.dir = Some(c.paths.state_dir.clone());
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("backup.dir"), "{err}");
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
