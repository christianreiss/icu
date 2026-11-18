//! `icu-server doctor` — read-only preflight diagnosis (§5).
//!
//! `status` answers "how is the running server doing?". `doctor` answers the
//! two questions that come before it: "will this server start?" and "is this
//! host set up the way the unit expects?". The failures it reports are the
//! ones §6.1 documents as fatal at startup — a CA that names another server,
//! a `server.key` the group can read, a port already taken — except that it
//! reports all of them in one pass, before the start, instead of one per
//! failed start.
//!
//! **Doctor never writes and never blocks.** In particular it does not go
//! through [`cli::dispatch`](crate::cli::dispatch): that falls back to
//! `Server::offline` when no server answers, and opening the state creates
//! the directory, takes `flock(state.lock)` and writes a first `users.toml`.
//! Doctor connects the admin socket itself and treats "no socket" as a
//! finding rather than as permission to touch the state. For the same reason
//! it uses `Config::read` and never `Config::load`, parses `users.toml` by
//! hand instead of asking [`Store`](crate::store::Store), and reads
//! certificates as bytes instead of opening the CA.
//!
//! It has to stay useful for three very different callers: root, a member of
//! the socket group (who cannot read `/var/lib/private/icu`), and the
//! `scratch` container (which has no `sh`, `getent` or `hostname`). Every
//! check therefore degrades to `skip` with the reason instead of ending the
//! run.

use std::{
    io::ErrorKind,
    net::SocketAddr,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::Path,
    time::Duration,
};

use serde_json::json;

use crate::{
    admin, backup, ca,
    config::Config,
    error::{Error, Result},
    store, AdminRequest,
};

/// How long to wait for the running server's `status`. `admin::exchange`
/// would wait 30 s; the Checkmk check gives it 10, and a doctor run that a
/// person is watching should not sit longer than monitoring does.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Below this many days left a certificate is a warning, below [`CERT_CRIT`]
/// a failure — the thresholds `deploy/checkmk/icu_server` already alarms on,
/// so doctor and monitoring never disagree about the same certificate.
const CERT_WARN: i64 = 30;
const CERT_CRIT: i64 = 7;

/// `sun_path` holds 108 bytes including the terminating NUL. A longer path
/// does not fail the start: binding the socket is non-fatal, and the server
/// serves on without an admin socket (`lib.rs`), which is a much harder
/// thing to notice than an error would have been.
const SUN_PATH_MAX: usize = 108;

/// How a single check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// As it should be.
    Ok,
    /// Worth knowing, nothing to do — a state that has not been created yet,
    /// a feature that is switched off.
    Info,
    /// Not checkable from here: no permission, or something else holds what
    /// would have to be looked at. Never a verdict about the server.
    Skip,
    /// Works now, will not keep working, or works less safely than intended.
    Warn,
    /// Broken, or will stop the next start.
    Fail,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Ok => "ok",
            Severity::Info => "info",
            Severity::Skip => "skip",
            Severity::Warn => "warn",
            Severity::Fail => "fail",
        }
    }
}

/// One finding. `id` is stable and machine-readable (`tls.ca_name`), `detail`
/// is the sentence a person reads; both are English like the rest of the
/// `icu-server` CLI.
#[derive(Debug, Clone)]
pub struct Check {
    pub id: String,
    pub severity: Severity,
    pub detail: String,
}

/// Every finding of one run, in catalogue order — not worst-first, so the
/// same host always prints the same shape and a diff between two runs is
/// readable.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    fn push(&mut self, id: impl Into<String>, severity: Severity, detail: impl Into<String>) {
        self.checks.push(Check {
            id: id.into(),
            severity,
            detail: detail.into(),
        });
    }

    fn ok(&mut self, id: impl Into<String>, detail: impl Into<String>) {
        self.push(id, Severity::Ok, detail);
    }
    fn info(&mut self, id: impl Into<String>, detail: impl Into<String>) {
        self.push(id, Severity::Info, detail);
    }
    fn skip(&mut self, id: impl Into<String>, detail: impl Into<String>) {
        self.push(id, Severity::Skip, detail);
    }
    fn warn(&mut self, id: impl Into<String>, detail: impl Into<String>) {
        self.push(id, Severity::Warn, detail);
    }
    fn fail(&mut self, id: impl Into<String>, detail: impl Into<String>) {
        self.push(id, Severity::Fail, detail);
    }

    /// How many checks came out at `severity`.
    pub fn count(&self, severity: Severity) -> usize {
        self.checks
            .iter()
            .filter(|c| c.severity == severity)
            .count()
    }

    /// The whole report as one JSON object. Keys are stable and English, as
    /// in `status --json`; `ok` is what the exit code says.
    pub fn to_json(&self) -> serde_json::Value {
        let checks: Vec<serde_json::Value> = self
            .checks
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "severity": c.severity.as_str(),
                    "detail": c.detail,
                })
            })
            .collect();
        json!({
            "ok": self.count(Severity::Fail) == 0,
            "checks": checks,
            "counts": {
                "ok": self.count(Severity::Ok),
                "info": self.count(Severity::Info),
                "skip": self.count(Severity::Skip),
                "warn": self.count(Severity::Warn),
                "fail": self.count(Severity::Fail),
            },
        })
    }

    /// The human view: one aligned line per check, then a tally.
    pub fn print(&self) {
        let style = crate::display::Style::current();
        println!("{}", style.heading("Doctor"));
        let width = self.checks.iter().map(|c| c.id.len()).max().unwrap_or(0);
        for check in &self.checks {
            let severity = format!("{:<4}", check.severity.as_str());
            let severity = match check.severity {
                Severity::Ok => style.good(&severity),
                Severity::Warn => style.caution(&severity),
                Severity::Fail => style.bad(&severity),
                Severity::Info | Severity::Skip => style.subtle(&severity),
            };
            println!("{}  {:<width$}  {}", severity, check.id, check.detail);
        }
        let tally: Vec<String> = [
            Severity::Ok,
            Severity::Info,
            Severity::Skip,
            Severity::Warn,
            Severity::Fail,
        ]
        .into_iter()
        .filter_map(|s| match self.count(s) {
            0 => None,
            n => Some(format!("{n} {}", s.as_str())),
        })
        .collect();
        println!("{}", tally.join(", "));
    }
}

/// Runs every check against `config_path` and prints the result.
pub async fn run(config_path: &Path, json: bool) -> Result<()> {
    let report = diagnose(config_path).await;
    if json {
        println!("{}", serde_json::to_string(&report.to_json())?);
    } else {
        report.print();
    }
    match report.count(Severity::Fail) {
        0 => Ok(()),
        n => Err(Error::msg(format!("{n} check(s) failed"))),
    }
}

/// Every check, in catalogue order. Returns rather than prints, so the shell
/// can show the same report without the CLI's non-zero exit.
pub async fn diagnose(config_path: &Path) -> Report {
    let mut report = Report::default();
    let config = match Config::read(config_path) {
        Ok(config) => {
            report.ok("config.file", format!("{} parses", config_path.display()));
            config
        }
        Err(e) => {
            // Everything below needs the paths out of the config, so this is
            // the one finding that ends the run.
            report.fail(
                "config.file",
                format!("{e}; the remaining checks need a parsable configuration"),
            );
            return report;
        }
    };
    config_perms(&mut report, config_path);
    config_valid(&mut report, &config);

    // Probed before the checks that depend on it, reported in catalogue
    // order below.
    let live = probe(&config).await;

    state(&mut report, &config);
    tls(&mut report, &config);
    socket(&mut report, &config);
    live_server(&mut report, &live);
    listeners(&mut report, &config, &live);
    backups(&mut report, &config);
    host(&mut report);
    report
}

// ---------------------------------------------------------------- config

fn config_perms(report: &mut Report, path: &Path) {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            report.skip(
                "config.perms",
                format!("cannot stat {}: {e}", path.display()),
            );
            return;
        }
    };
    let mode = meta.permissions().mode() & 0o777;
    // The package ships it 0640 root:icu. It names the state directory and
    // the socket, so every local user knowing them is a head start nobody
    // needs; it holds no secret, hence warn rather than fail.
    if mode & 0o004 != 0 {
        report.warn(
            "config.perms",
            format!(
                "{} is {mode:04o}: readable by every local user. The package ships it \
                 0640 root:{}",
                path.display(),
                "icu"
            ),
        );
    } else {
        report.ok("config.perms", format!("{} is {mode:04o}", path.display()));
    }
}

fn config_valid(report: &mut Report, config: &Config) {
    // Independent of the server name, so it is worth saying even where the
    // name cannot be derived.
    if let Err(e) = config.validate_lifetimes() {
        report.fail("config.validate", e.to_string());
        return;
    }
    let configured = !config.server.server_name.trim().is_empty();
    // `resolve` is the only way to run `validate`, and with an unset
    // `server_name` it derives one: `ca.crt` first, else `hostname --fqdn`
    // plus a DNS lookup and a test bind. A member who cannot read the state
    // therefore gets the host name, which need not be what the CA carries —
    // worth naming in the report.
    let source = if configured {
        "from the configuration"
    } else if ca_name(config).is_some() {
        "derived from the CA"
    } else {
        "derived from the host name, not from the CA"
    };
    match config.clone().resolve() {
        Ok(resolved) => report.ok(
            "config.validate",
            format!(
                "valid; server_name {:?} ({source}), listen {:?}",
                resolved.server.server_name, resolved.server.listen
            ),
        ),
        // An explicit name that does not validate is a real defect. An unset
        // one that cannot be derived says nothing about the config: under
        // `DynamicUser` only root gets to read the CA it would come from.
        Err(e) if configured => report.fail("config.validate", e.to_string()),
        Err(e) => report.skip(
            "config.validate",
            format!("server.server_name is unset and cannot be derived here: {e}"),
        ),
    }
}

// ----------------------------------------------------------------- state

/// What a read of a state file found. Permission denied is not absence: the
/// difference decides `skip` against `fail`.
enum Bytes {
    Found(Vec<u8>),
    Missing,
    Denied(String),
}

fn read_bytes(path: &Path) -> Bytes {
    match std::fs::read(path) {
        Ok(bytes) => Bytes::Found(bytes),
        Err(e) if e.kind() == ErrorKind::NotFound => Bytes::Missing,
        Err(e) => Bytes::Denied(format!("{}: {e}", path.display())),
    }
}

fn state(report: &mut Report, config: &Config) {
    let dir = &config.paths.state_dir;
    match std::fs::metadata(dir) {
        Ok(meta) if !meta.is_dir() => {
            report.fail("state.dir", format!("{} is not a directory", dir.display()));
            return;
        }
        Ok(meta) => {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                report.warn(
                    "state.dir",
                    format!(
                        "{} is {mode:04o}; it holds ca.key and com.key, and the unit \
                         creates it 0700 (StateDirectoryMode)",
                        dir.display()
                    ),
                );
            } else {
                report.ok("state.dir", format!("{} is {mode:04o}", dir.display()));
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            report.info(
                "state.dir",
                format!(
                    "{} does not exist yet; the first start creates it",
                    dir.display()
                ),
            );
        }
        Err(e) => {
            report.skip(
                "state.dir",
                format!(
                    "cannot stat {}: {e}. Under DynamicUser the real directory is \
                     /var/lib/private/icu, 0700 root",
                    dir.display()
                ),
            );
            return;
        }
    }
    state_files(report, config);
    let lock = dir.join(store::LOCK_FILE);
    // Never locked, only looked at: taking it is what `Store::open` does,
    // and it would fail against a running server or lock one out.
    report.info(
        "state.lock",
        match lock.exists() {
            true => format!("{} is there (not taken by doctor)", lock.display()),
            false => format!("{} does not exist yet", lock.display()),
        },
    );
}

fn state_files(report: &mut Report, config: &Config) {
    let dir = &config.paths.state_dir;
    let (mut present, mut missing, mut loose, mut denied) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for file in backup::STATE_FILES.iter() {
        match std::fs::metadata(dir.join(file.name)) {
            Ok(meta) => {
                present.push(file.name);
                let mode = meta.permissions().mode() & 0o777;
                if file.secret && mode & 0o077 != 0 {
                    loose.push(format!("{} is {mode:04o}, expected 0600", file.name));
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if file.required {
                    missing.push(file.name);
                }
            }
            Err(e) => denied.push(format!("{}: {e}", file.name)),
        }
    }
    if !denied.is_empty() && present.is_empty() {
        report.skip("state.files", denied.join("; "));
        return;
    }
    if !loose.is_empty() {
        report.warn("state.files", loose.join("; "));
        return;
    }
    if missing.is_empty() {
        report.ok(
            "state.files",
            format!("{} of {} present", present.len(), backup::STATE_FILES.len()),
        );
        return;
    }
    // The one missing-file case that is not "a fresh install": the CA files
    // are gone but the state has been used. `lib.rs` refuses to start here
    // rather than make a new CA, which would lock out everyone registered.
    let ca_gone = missing.contains(&ca::CA_CRT) && missing.contains(&ca::CA_KEY);
    if ca_gone && users_used(dir) == Some(true) {
        report.fail(
            "state.files",
            format!(
                "ca.crt and ca.key are both missing, but {} has been in use. The next \
                 start refuses rather than make a new CA, which would lock out everyone \
                 registered so far: restore both from the backup that {} comes from \
                 (runbook \"Backup und Restore\" in deploy/README.md)",
                store::USERS_FILE,
                store::USERS_FILE
            ),
        );
        return;
    }
    if present.is_empty() {
        report.info(
            "state.files",
            "no state yet; the first start (or the first invite) creates it",
        );
        return;
    }
    report.fail(
        "state.files",
        format!("missing beside an existing state: {}", missing.join(", ")),
    );
}

/// Whether `users.toml` shows a state that has been used: a user, a CRL
/// issued, or a revoked server leaf. This is [`crate::store::Store::is_fresh`]
/// inverted, read straight off the file — opening the store would take the
/// lock. `None` when the file is absent or unreadable.
fn users_used(dir: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(dir.join(store::USERS_FILE)).ok()?;
    let users: store::UsersFile = toml::from_str(&text).ok()?;
    Some(!(users.users.is_empty() && users.crl_number == 0 && users.revoked_server.is_empty()))
}

// ------------------------------------------------------------------- tls

fn tls(report: &mut Report, config: &Config) {
    let dir = &config.paths.state_dir;
    let ca_pem = read_bytes(&dir.join(ca::CA_CRT));
    cert(report, "tls.ca", ca::CA_CRT, &ca_pem);
    let server_pem = read_bytes(&dir.join(ca::SERVER_CRT));
    cert(report, "tls.server", ca::SERVER_CRT, &server_pem);
    if let (Bytes::Found(_), Bytes::Missing) = (&server_pem, &read_bytes(&dir.join(ca::SERVER_KEY)))
    {
        report.warn(
            "tls.server_key",
            format!(
                "{} without {}: the pair does not match, and the next start issues a \
                 fresh leaf (§6.1 step 3)",
                ca::SERVER_CRT,
                ca::SERVER_KEY
            ),
        );
    }
    ca_name_matches(report, config, &ca_pem);
}

fn cert(report: &mut Report, id: &str, name: &str, pem: &Bytes) {
    let bytes = match pem {
        Bytes::Missing => {
            report.info(id, format!("{name} is not there yet"));
            return;
        }
        Bytes::Denied(e) => {
            report.skip(id, e.clone());
            return;
        }
        Bytes::Found(bytes) => bytes,
    };
    let der = match ca::cert_der(bytes) {
        Ok(der) => der,
        Err(e) => {
            report.fail(id, format!("{name}: {e}"));
            return;
        }
    };
    // A clock that ran ahead at issue time leaves a leaf nobody accepts yet;
    // housekeeping replaces it, but before a start nothing has run.
    if ca::leaf_not_yet_valid(&der) {
        report.fail(
            id,
            format!(
                "{name} is not valid yet: notBefore lies in the future, so every client \
                 rejects it. The CA backdates by an hour, so the clock ran ahead when it \
                 was issued"
            ),
        );
        return;
    }
    match ca::leaf_days_left_checked(&der) {
        None => report.fail(id, format!("{name} does not parse")),
        Some(days) if days < 0 => report.fail(
            id,
            format!(
                "{name} expired {} day(s) ago ({})",
                -days,
                ca::leaf_not_after(&der)
            ),
        ),
        Some(days) if days < CERT_CRIT => report.fail(
            id,
            format!(
                "{name} expires in {days} day(s) ({})",
                ca::leaf_not_after(&der)
            ),
        ),
        Some(days) if days < CERT_WARN => report.warn(
            id,
            format!(
                "{name} expires in {days} day(s) ({}); the server renews by itself, so \
                 this means the renewal is not running",
                ca::leaf_not_after(&der)
            ),
        ),
        Some(days) => report.ok(
            id,
            format!(
                "{name} valid for {days} more day(s) ({})",
                ca::leaf_not_after(&der)
            ),
        ),
    }
}

/// The server name a CA certificate carries in its CN (`ICU CA <name>`).
fn ca_name_of(der: &[u8]) -> Option<String> {
    let cn = ca::leaf_common_name(der);
    let name = cn.strip_prefix(ca::CA_COMMON_NAME)?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// The name of the CA on disk, without going through the state lock.
fn ca_name(config: &Config) -> Option<String> {
    match read_bytes(&config.paths.state_dir.join(ca::CA_CRT)) {
        Bytes::Found(pem) => ca::cert_der(&pem).ok().as_deref().and_then(ca_name_of),
        _ => None,
    }
}

fn ca_name_matches(report: &mut Report, config: &Config, ca_pem: &Bytes) {
    let configured = config.server.server_name.trim();
    let der = match ca_pem {
        Bytes::Missing => {
            report.info("tls.ca_name", "no CA yet; the first start makes one");
            return;
        }
        Bytes::Denied(e) => {
            report.skip("tls.ca_name", e.clone());
            return;
        }
        Bytes::Found(pem) => match ca::cert_der(pem) {
            Ok(der) => der,
            // Already reported as `tls.ca`.
            Err(_) => return,
        },
    };
    match ca_name_of(&der) {
        None => report.warn(
            "tls.ca_name",
            format!(
                "the CA ({:?}) names no server — it predates the name moving into the CN. \
                 Set server.server_name to the name its server certificate carries",
                ca::leaf_common_name(&der)
            ),
        ),
        Some(name) if configured.is_empty() => report.ok(
            "tls.ca_name",
            format!("server.server_name is unset; the CA's {name:?} is used"),
        ),
        Some(name) if name.eq_ignore_ascii_case(configured) => report.ok(
            "tls.ca_name",
            format!("CA and configuration agree on {name:?}"),
        ),
        // The same refusal `open_ca` raises, said before it stops a start.
        Some(name) => report.fail(
            "tls.ca_name",
            format!(
                "server.server_name is {configured:?}, but the CA in {} was made for \
                 {name:?}, and clients take the server name from there: a server \
                 certificate for {configured:?} would lock every client out at the next \
                 certificate change. Set server.server_name to {name:?} or remove it \
                 (the CA's name is then used); a different name needs a new CA, and \
                 every user registering again",
                config.paths.state_dir.display()
            ),
        ),
    }
}

// ---------------------------------------------------------------- socket

fn socket(report: &mut Report, config: &Config) {
    let path = config.admin_socket();
    let len = path.as_os_str().as_bytes().len();
    if len >= SUN_PATH_MAX {
        report.fail(
            "socket.path",
            format!(
                "{} is {len} bytes; a unix socket path holds at most {} plus a NUL. \
                 Binding it is not fatal, so the server would start and simply have no \
                 admin socket: shorten paths.runtime_dir or paths.admin_socket",
                path.display(),
                SUN_PATH_MAX - 1
            ),
        );
    } else {
        report.ok("socket.path", format!("{} ({len} bytes)", path.display()));
    }
    match path.parent() {
        Some(parent) if !parent.exists() => report.info(
            "socket.dir",
            format!(
                "{} does not exist; the unit creates it (RuntimeDirectory=icu, 0755)",
                parent.display()
            ),
        ),
        Some(parent) => report.ok("socket.dir", format!("{} is there", parent.display())),
        None => {}
    }
    match &config.server.admin_socket_group {
        // Under the shipped unit `Group=icu` owns /run/icu anyway; elsewhere
        // an unset group means only the server's own user reaches the socket.
        None => report.info(
            "socket.group",
            "server.admin_socket_group is unset: the socket keeps the group it is \
             created with, and only that group (plus root) can run admin commands",
        ),
        Some(group) => match admin::resolve_group(group) {
            Ok(gid) => report.ok(
                "socket.group",
                format!(
                    "{group:?} is gid {gid}; note that this group is full admin, not \
                     read-only — a Checkmk agent in it can also revoke users"
                ),
            ),
            Err(e) => report.warn(
                "socket.group",
                format!("{e}. The server logs it and serves on with the group unchanged"),
            ),
        },
    }
}

// -------------------------------------------------------------- liveness

/// What the admin socket said. Only [`Live::Absent`] — the socket is not
/// there, is the stale file a crash leaves, or names a path no socket can
/// live at — means no server is listening on it; `EACCES` and a timeout both
/// mean there is one, we just cannot talk to it.
enum Live {
    Running(serde_json::Value),
    Absent(String),
    Unreachable { detail: String, fail: bool },
}

async fn probe(config: &Config) -> Live {
    let socket = config.admin_socket();
    let stream = match tokio::net::UnixStream::connect(&socket).await {
        Ok(stream) => stream,
        // `InvalidInput` is the over-long path `socket.path` already
        // reported: nothing can be listening there, so it is absence, not an
        // unreachable server — the difference decides whether the listen
        // ports below get tested at all.
        Err(e)
            if matches!(
                e.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::InvalidInput
            ) =>
        {
            return Live::Absent(format!("{}: {e}", socket.display()));
        }
        Err(e) => {
            return Live::Unreachable {
                detail: format!(
                    "cannot connect to {}: {e}. It takes root or membership in its group \
                     (server.admin_socket_group)",
                    socket.display()
                ),
                fail: false,
            }
        }
    };
    match tokio::time::timeout(
        PROBE_TIMEOUT,
        admin::exchange(stream, &AdminRequest::Status),
    )
    .await
    {
        Ok(Ok(value)) if value.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
            Live::Running(value)
        }
        Ok(Ok(value)) => Live::Unreachable {
            detail: format!("the server answered {value}"),
            fail: true,
        },
        Ok(Err(e)) => Live::Unreachable {
            detail: e.to_string(),
            fail: true,
        },
        Err(_) => Live::Unreachable {
            detail: format!(
                "no answer within {} s; the server may be stuck",
                PROBE_TIMEOUT.as_secs()
            ),
            fail: true,
        },
    }
}

fn live_server(report: &mut Report, live: &Live) {
    let status = match live {
        Live::Absent(detail) => {
            report.info("server.live", format!("no server is running ({detail})"));
            return;
        }
        Live::Unreachable { detail, fail } => {
            let severity = if *fail {
                Severity::Fail
            } else {
                Severity::Warn
            };
            report.push("server.live", severity, detail.clone());
            return;
        }
        Live::Running(status) => status,
    };
    let num = |key: &str| status.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    let (online, max_users) = (num("online"), num("max_users"));
    report.ok(
        "server.live",
        format!(
            "up {} s, {online}/{max_users} online, {} open connection(s)",
            num("uptime_s"),
            num("open_connections")
        ),
    );
    if max_users > 0 && online >= max_users {
        report.warn(
            "server.room",
            format!("the room is full ({online}/{max_users}); nobody else can join"),
        );
    } else {
        report.ok("server.room", format!("{online}/{max_users} online"));
    }
    live_tls(report, status);
    live_backup(report, status);
}

fn live_tls(report: &mut Report, status: &serde_json::Value) {
    let Some(tls) = status.get("tls").and_then(|v| v.as_object()) else {
        return;
    };
    if tls.is_empty() {
        // `tls_status` answers `{}` while a rotation holds the CA lock.
        report.info(
            "server.tls",
            "a certificate rotation holds the CA right now; the next run sees it",
        );
        return;
    }
    for (key, what) in [
        ("server_cert_days_left", "the server certificate"),
        ("ca_days_left", "the CA certificate"),
    ] {
        let Some(days) = tls.get(key).and_then(|v| v.as_i64()) else {
            continue;
        };
        let id = format!("server.{key}");
        if days < CERT_CRIT {
            report.fail(id, format!("{what} expires in {days} day(s)"));
        } else if days < CERT_WARN {
            report.warn(
                id,
                format!("{what} expires in {days} day(s); its automatic renewal is not running"),
            );
        } else {
            report.ok(id, format!("{what} is valid for {days} more day(s)"));
        }
    }
}

fn live_backup(report: &mut Report, status: &serde_json::Value) {
    let Some(b) = status.get("backup").and_then(|v| v.as_object()) else {
        return;
    };
    let keep = b.get("keep").and_then(|v| v.as_u64()).unwrap_or(0);
    if keep == 0 {
        report.info("server.backup", "state snapshots are off (backup.keep = 0)");
        return;
    }
    let failed = b.get("failed").and_then(|v| v.as_u64()).unwrap_or(0) == 1;
    let error = b.get("error").and_then(|v| v.as_str()).unwrap_or_default();
    if failed {
        report.warn(
            "server.backup",
            format!("the last state snapshot failed: {error}"),
        );
    } else {
        report.ok("server.backup", format!("snapshots ok, keeping {keep}"));
    }
}

// ------------------------------------------------------------- listeners

fn listeners(report: &mut Report, config: &Config, live: &Live) {
    // Only when the socket says no server is there. `EACCES` and a timeout
    // mean one *is* running and holding these ports, and binding then would
    // report "in use" as a defect to exactly the caller who has no other way
    // of telling — the same distinction `cli::dispatch` and the Checkmk
    // check draw.
    let Live::Absent(_) = live else {
        report.skip(
            "listen",
            "a server holds the admin socket, so it holds these ports too",
        );
        return;
    };
    for addr in &config.server.listen {
        let id = format!("listen.{addr}");
        match probe_bind(*addr) {
            Ok(()) => report.ok(id, format!("{addr} is free")),
            Err(e) if e.kind() == ErrorKind::AddrInUse => {
                report.fail(id, format!("{addr} is in use by another process"))
            }
            Err(e) if e.kind() == ErrorKind::PermissionDenied && addr.port() < 1024 => report.warn(
                id,
                format!(
                    "{addr}: {e}. A port below 1024 needs \
                     AmbientCapabilities=CAP_NET_BIND_SERVICE in the unit, which the \
                     shipped one has commented out"
                ),
            ),
            Err(e) => report.fail(id, format!("{addr}: {e}")),
        }
    }
}

/// Binds and immediately drops a UDP socket, exactly as `endpoint::bind`
/// would: same domain, same `IPV6_V6ONLY`, and no `SO_REUSEADDR`, so "free"
/// here means the server would get it too.
fn probe_bind(addr: SocketAddr) -> std::io::Result<()> {
    let domain = match addr {
        SocketAddr::V4(_) => socket2::Domain::IPV4,
        SocketAddr::V6(_) => socket2::Domain::IPV6,
    };
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&addr.into())
}

// --------------------------------------------------------------- backups

fn backups(report: &mut Report, config: &Config) {
    if config.backup.keep == 0 {
        report.info("backup.dir", "state snapshots are off (backup.keep = 0)");
        return;
    }
    let dir = config.backup_dir();
    let snapshots = match backup::list(&dir) {
        Ok(snapshots) => snapshots,
        Err(e) => {
            report.skip(
                "backup.dir",
                format!("{e} (the directory is 0700, as its contents are)"),
            );
            return;
        }
    };
    let Some(newest) = snapshots.last() else {
        let detail = format!("no snapshot in {} yet", dir.display());
        // Before the first start there is nothing to snapshot; afterwards
        // there should be one, because the server takes one at every start.
        match users_used(&config.paths.state_dir) {
            Some(true) => report.warn(
                "backup.dir",
                format!("{detail}, although the state has been used"),
            ),
            _ => report.info("backup.dir", detail),
        }
        return;
    };
    match backup::read_manifest(newest) {
        Ok(manifest) => report.ok(
            "backup.dir",
            format!(
                "{} snapshot(s) in {}, newest {} ({}, {} user(s)), keeping {}",
                snapshots.len(),
                dir.display(),
                manifest.created,
                manifest.reason,
                manifest.users,
                config.backup.keep
            ),
        ),
        Err(e) => report.warn(
            "backup.dir",
            format!(
                "the newest snapshot {} has no usable manifest: {e}",
                newest.display()
            ),
        ),
    }
}

// ------------------------------------------------------------------ host

fn host(report: &mut Report) {
    let version = env!("CARGO_PKG_VERSION");
    // The same condition `lib.rs` warns about at startup: with `panic =
    // "abort"` the per-session containment is gone.
    if cfg!(panic = "abort") {
        report.warn(
            "host.build",
            format!(
                "icu-server {version}, built with panic = \"abort\": a panic in one \
                 session ends the whole server. Build it with `cargo build --profile \
                 release-server`"
            ),
        );
    } else {
        report.ok(
            "host.build",
            format!("icu-server {version}, panic = unwind"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_of(checks: &[(&str, Severity)]) -> Report {
        let mut report = Report::default();
        for (id, severity) in checks {
            report.push(*id, *severity, "");
        }
        report
    }

    /// Only `fail` decides the exit code; a warning is not a defect.
    #[test]
    fn only_a_failure_makes_the_run_fail() {
        let clean = report_of(&[
            ("a", Severity::Ok),
            ("b", Severity::Warn),
            ("c", Severity::Skip),
        ]);
        assert_eq!(clean.count(Severity::Fail), 0);
        assert_eq!(clean.to_json()["ok"], serde_json::json!(true));

        let broken = report_of(&[("a", Severity::Ok), ("b", Severity::Fail)]);
        assert_eq!(broken.count(Severity::Fail), 1);
        assert_eq!(broken.to_json()["ok"], serde_json::json!(false));
    }

    /// The JSON keys are what a monitor reads; they must not drift.
    #[test]
    fn the_json_carries_every_check_and_the_tally() {
        let json = report_of(&[("tls.ca", Severity::Warn), ("host.build", Severity::Ok)]).to_json();
        assert_eq!(json["checks"][0]["id"], serde_json::json!("tls.ca"));
        assert_eq!(json["checks"][0]["severity"], serde_json::json!("warn"));
        assert_eq!(json["counts"]["warn"], serde_json::json!(1));
        assert_eq!(json["counts"]["ok"], serde_json::json!(1));
        assert_eq!(json["counts"]["fail"], serde_json::json!(0));
    }

    /// The thresholds `deploy/checkmk/icu_server` alarms on, at their edges.
    #[test]
    fn certificate_days_map_to_the_checkmk_thresholds() {
        let severity = |days: i64| {
            let mut report = Report::default();
            live_tls(
                &mut report,
                &json!({"tls": {"ca_days_left": days, "server_cert_days_left": days}}),
            );
            report.checks[0].severity
        };
        assert_eq!(severity(30), Severity::Ok);
        assert_eq!(severity(29), Severity::Warn);
        assert_eq!(severity(7), Severity::Warn);
        assert_eq!(severity(6), Severity::Fail);
        assert_eq!(severity(-1), Severity::Fail);
    }

    /// A rotation holding the CA lock answers `{}`; that is not an alarm.
    #[test]
    fn an_empty_tls_object_is_only_a_note() {
        let mut report = Report::default();
        live_tls(&mut report, &json!({"tls": {}}));
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].severity, Severity::Info);
    }

    /// A socket path that does not fit `sun_path` is the failure that would
    /// otherwise leave a server running with no admin socket at all.
    #[test]
    fn an_overlong_socket_path_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config::for_test(dir.path());
        config.paths.admin_socket = Some(std::path::PathBuf::from(format!(
            "/run/{}/admin.sock",
            "x".repeat(SUN_PATH_MAX)
        )));
        let mut report = Report::default();
        socket(&mut report, &config);
        let check = &report.checks[0];
        assert_eq!(check.id, "socket.path");
        assert_eq!(check.severity, Severity::Fail);

        config.paths.admin_socket = Some(std::path::PathBuf::from("/run/icu/admin.sock"));
        let mut report = Report::default();
        socket(&mut report, &config);
        assert_eq!(report.checks[0].severity, Severity::Ok);
    }

    /// The CN a CA carries, and the shape that predates the name being in it.
    #[test]
    fn the_ca_name_comes_out_of_the_common_name() {
        assert_eq!(
            ca_name_of_cn("ICU CA icu.example.org"),
            Some("icu.example.org".to_string())
        );
        assert_eq!(ca_name_of_cn("ICU CA"), None);
        assert_eq!(ca_name_of_cn(""), None);
    }

    /// `ca_name_of` over a CN directly, so the test needs no certificate.
    fn ca_name_of_cn(cn: &str) -> Option<String> {
        let name = cn.strip_prefix(ca::CA_COMMON_NAME)?.trim().to_string();
        (!name.is_empty()).then_some(name)
    }

    /// A configuration that does not parse ends the run with one finding,
    /// and nothing below it pretends to know anything.
    #[tokio::test]
    async fn a_broken_configuration_is_the_only_finding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        std::fs::write(&path, "[server]\nnot_a_key = 1\n").expect("write");
        let report = diagnose(&path).await;
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].id, "config.file");
        assert_eq!(report.checks[0].severity, Severity::Fail);
    }
}
