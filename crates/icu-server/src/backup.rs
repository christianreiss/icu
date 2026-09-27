//! Automatic snapshots of the state directory, and restoring one
//! (`Server/IMPLEMENTATION.md` §4, runbook "Backup und Restore" in
//! `deploy/README.md`).
//!
//! A snapshot is a directory `<backup dir>/<UTC time>/` with copies of the
//! state files and a `MANIFEST`: plain files, so `cp` alone can still bring
//! one back, as the runbook always had it. The server takes one when it
//! starts and `backup.settle_ms` after every change — a write of `users.toml`
//! ([`Store::changes`]) or of a certificate file ([`Ca::changes`]) — and keeps
//! the newest `backup.keep`.
//!
//! A change is a change of the state, not of `crl_number`: that moves with
//! every start and every CRL rebuild, and would turn every restart into a
//! snapshot and push the one from before a mistake out of the window.
//! Restoring an older number is harmless, it never falls below the clock
//! ([`Store::next_crl_number`]).
//!
//! Every writer of the backup directory holds `flock(state.lock)`: the
//! running server, the offline CLI, `restore`. So leftovers of an interrupted
//! write (`.icu-*`) can go whenever a snapshot is written.
//!
//! [`Store::changes`]: crate::store::Store::changes
//! [`Ca::changes`]: crate::ca::Ca::changes
//! [`Store::next_crl_number`]: crate::store::Store::next_crl_number

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    config::Config,
    error::{Context, Error, Result},
    store::{self, Store},
    Server,
};

/// File name of the manifest in a snapshot.
pub const MANIFEST: &str = "MANIFEST";
/// Version of the snapshot layout.
const FORMAT: u32 = 1;
/// Prefix of anything half-written; never a snapshot.
const TEMP_PREFIX: &str = ".icu-";

/// A file of the state directory a snapshot carries.
struct StateFile {
    name: &'static str,
    /// Mode 0600 when put back.
    secret: bool,
    /// Without it a snapshot cannot be restored. The server leaf is
    /// disposable: the server issues a new one when it is missing.
    required: bool,
}

/// Everything in the state directory that is state; `state.lock` and
/// leftovers are not.
const STATE_FILES: [StateFile; 7] = [
    StateFile {
        name: "ca.crt",
        secret: false,
        required: true,
    },
    StateFile {
        name: "ca.key",
        secret: true,
        required: true,
    },
    StateFile {
        name: store::COM_KEY_FILE,
        secret: true,
        required: true,
    },
    StateFile {
        name: store::USERS_FILE,
        secret: false,
        required: true,
    },
    StateFile {
        name: "server.crt",
        secret: false,
        required: false,
    },
    StateFile {
        name: "server.key",
        secret: true,
        required: false,
    },
    StateFile {
        name: SERVER_RETIRED,
        secret: false,
        required: false,
    },
];

/// Replaced server leaves not on the CRL yet ([`crate::ca::Ca::retired_recorded`]).
const SERVER_RETIRED: &str = "server.crt.retired";

/// Why a snapshot was taken; in its `MANIFEST`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The server started.
    Start,
    /// The state changed while the server ran, or through the offline CLI.
    Change,
    /// A change was still waiting for `settle_ms` when the server stopped.
    Shutdown,
    /// `icu-server backup`.
    Manual,
    /// The state `restore` replaced.
    PreRestore,
    /// The state `restore` put in place, after its rotation.
    Restore,
}

impl Reason {
    /// As written to the manifest.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Start => "start",
            Reason::Change => "change",
            Reason::Shutdown => "shutdown",
            Reason::Manual => "manual",
            Reason::PreRestore => "pre-restore",
            Reason::Restore => "restore",
        }
    }

    /// Whether the snapshot is written even when the state equals the newest
    /// one: whoever asks for it by hand wants a path to copy.
    fn always(self) -> bool {
        matches!(self, Reason::Manual | Reason::PreRestore)
    }
}

/// `MANIFEST` of a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Layout version, [`FORMAT`].
    pub format: u32,
    /// When it was taken, RFC 3339 UTC.
    pub created: String,
    /// [`Reason::as_str`].
    pub reason: String,
    /// Version of the icu-server that took it.
    pub server_version: String,
    /// `schema` of its `users.toml`; 0 when that did not parse.
    pub schema: u32,
    /// Users on record.
    pub users: usize,
    /// What decides whether the state changed since: SHA-256 over the files,
    /// `users.toml` without `crl_number`.
    pub fingerprint: String,
    /// SHA-256 of every file, lowercase hex.
    pub files: BTreeMap<String, String>,
}

/// State files by name.
type Files = BTreeMap<String, Vec<u8>>;

/// The state files in `dir`. With `partial`, whatever is there, else every
/// required one or an error.
fn read_state(dir: &Path, partial: bool) -> Result<Files> {
    let mut files = Files::new();
    for file in &STATE_FILES {
        let path = dir.join(file.name);
        match std::fs::read(&path) {
            Ok(bytes) => {
                files.insert(file.name.to_string(), bytes);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if file.required && !partial {
                    return Err(Error::msg(format!("{} is missing", path.display())));
                }
            }
            Err(e) => return Err(Error::from(e).context(format!("cannot read {}", path.display()))),
        }
    }
    Ok(files)
}

/// `users.toml` without `crl_number`, or `None` where it does not parse.
fn without_crl_number(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut table: toml::Table = toml::from_str(std::str::from_utf8(bytes).ok()?).ok()?;
    table.remove("crl_number");
    toml::to_string(&table).ok().map(String::into_bytes)
}

/// [`Manifest::fingerprint`] of `files`.
fn fingerprint(files: &Files) -> String {
    let mut hash = Sha256::new();
    for (name, bytes) in files {
        let normalized = (name == store::USERS_FILE)
            .then(|| without_crl_number(bytes))
            .flatten();
        let bytes = normalized.as_deref().unwrap_or(bytes);
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    store::encode_hex(&hash.finalize())
}

impl Manifest {
    fn new(files: &Files, reason: Reason, created: time::OffsetDateTime) -> Manifest {
        let users: Option<store::UsersFile> = files
            .get(store::USERS_FILE)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| toml::from_str(text).ok());
        Manifest {
            format: FORMAT,
            created: store::rfc3339(created),
            reason: reason.as_str().to_string(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            schema: users.as_ref().map_or(0, |u| u.schema),
            users: users.as_ref().map_or(0, |u| u.users.len()),
            fingerprint: fingerprint(files),
            files: files
                .iter()
                .map(|(name, bytes)| (name.clone(), store::encode_hex(&Sha256::digest(bytes))))
                .collect(),
        }
    }
}

/// Creates `dir` with mode 0700 where it is missing.
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .context(format!("cannot create {}", dir.display()))
}

/// Writes a new file with mode 0600 and flushes it to disk.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context(format!("cannot create {}", path.display()))?;
    // Exactly, whatever the umask, like `store::write_private`.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(dir) = std::fs::File::open(dir) {
        let _ = dir.sync_all();
    }
}

/// Name of a snapshot taken at `t`: `YYYYMMDDTHHMMSSZ`, which sorts by time.
fn snapshot_name(t: time::OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// Sort key of a snapshot directory name — its time, then the `-n` suffix
/// of a second one within the same second — or `None` for anything else.
fn snapshot_key(name: &str) -> Option<(String, u32)> {
    let (base, n) = match name.split_once('-') {
        Some((base, n)) => (base, n.parse().ok()?),
        None => (name, 0),
    };
    let b = base.as_bytes();
    let shape = b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit);
    shape.then(|| (base.to_string(), n))
}

/// The snapshots in `dir`, oldest first. A missing directory has none.
pub fn list(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::from(e).context(format!("cannot read {}", dir.display()))),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(key) = entry.file_name().to_str().and_then(snapshot_key) else {
            continue;
        };
        if entry.file_type()?.is_dir() {
            found.push((key, entry.path()));
        }
    }
    found.sort();
    Ok(found.into_iter().map(|(_, path)| path).collect())
}

/// Reads the manifest of the snapshot in `dir`.
pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&path).context(format!("cannot read {}", path.display()))?;
    toml::from_str(&text).context(format!("cannot parse {}", path.display()))
}

/// Writes `files` as a new snapshot in `dir`: built under a temporary name,
/// renamed into place once complete.
fn write_snapshot(dir: &Path, files: &Files, reason: Reason) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    create_private_dir(dir)?;
    remove_leftovers(dir);
    let now = store::now();
    let manifest = Manifest::new(files, reason, now);
    let tmp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempdir_in(dir)
        .context(format!("cannot create a snapshot in {}", dir.display()))?;
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700))?;
    for (name, bytes) in files {
        write_new(&tmp.path().join(name), bytes)?;
    }
    write_new(
        &tmp.path().join(MANIFEST),
        toml::to_string(&manifest)?.as_bytes(),
    )?;
    sync_dir(tmp.path());
    // After the newest, always: a name freed by pruning within the same
    // second, or a clock set back, would sort first and be pruned at once.
    let newest = list(dir)?
        .last()
        .and_then(|path| path.file_name()?.to_str().and_then(snapshot_key));
    let (base, mut n) = match newest {
        Some((base, n)) if base >= snapshot_name(now) => (base, n + 1),
        _ => (snapshot_name(now), 0),
    };
    let name = |n: u32| match n {
        0 => base.clone(),
        n => format!("{base}-{n}"),
    };
    while dir.join(name(n)).try_exists()? {
        n += 1;
    }
    let target = dir.join(name(n));
    std::fs::rename(tmp.path(), &target)
        .context(format!("cannot rename a snapshot to {}", target.display()))?;
    // Moved: nothing left for the guard to remove.
    let _ = tmp.keep();
    sync_dir(dir);
    Ok(target)
}

/// Removes what an interrupted snapshot left behind in `dir`.
fn remove_leftovers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            let path = entry.path();
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(e) = removed {
                tracing::warn!(path = %path.display(), error = %e, "cannot remove a leftover");
            }
        }
    }
}

/// Removes the oldest snapshots in `dir` until `keep` are left; never the
/// newest.
fn prune(dir: &Path, keep: u32) -> Result<()> {
    let snapshots = list(dir)?;
    let excess = snapshots.len().saturating_sub((keep as usize).max(1));
    for old in &snapshots[..excess] {
        std::fs::remove_dir_all(old).context(format!("cannot remove {}", old.display()))?;
    }
    Ok(())
}

/// Gives `path` — and with `recursive` everything below it — the owner and
/// group of `like` where they differ. Root running the CLI writes as root
/// into a state directory the service owns (`DynamicUser`), and systemd puts
/// ownership right only when the top of `StateDirectory` is off, not for the
/// files inside: a root-owned `ca.key` with mode 0600 keeps the server from
/// starting, a root-owned `backups/` keeps it from taking snapshots. Only
/// root changes owners; everyone else wrote files that are theirs already.
pub(crate) fn match_owner(path: &Path, like: &Path, recursive: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let owner = std::fs::metadata(like).context(format!("cannot stat {}", like.display()))?;
    let (uid, gid) = (owner.uid(), owner.gid());
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::from(e).context(format!("cannot stat {}", path.display()))),
        };
        if meta.uid() != uid || meta.gid() != gid {
            std::os::unix::fs::lchown(&path, Some(uid), Some(gid)).context(format!(
                "cannot give {} the owner of {}",
                path.display(),
                like.display()
            ))?;
        }
        if recursive && meta.is_dir() {
            for entry in std::fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(())
}

/// Outcome of the last snapshot attempt, for `status`.
#[derive(Debug, Default)]
pub struct Status {
    /// When the state was last found backed up, Unix seconds: a snapshot
    /// written, or the newest one found current.
    last_ok: Option<u64>,
    /// Why the last attempt failed; cleared by the next that works.
    error: Option<String>,
}

/// The `backup` object of `status --json`. `failed` is what the Checkmk
/// check reads.
pub fn status_json(server: &Server) -> serde_json::Value {
    let status = crate::lock(&server.backup);
    json!({
        "keep": server.config.backup.keep,
        "failed": u8::from(status.error.is_some()),
        "last_ok_unix": status.last_ok.unwrap_or(0),
        "error": status.error.clone().unwrap_or_default(),
    })
}

/// Takes a snapshot of the state of `server`, unless it equals the newest
/// one and `reason` does not insist ([`Reason::always`]). Returns where it
/// went; `None` when nothing changed.
pub async fn snapshot(server: &Server, reason: Reason) -> Result<Option<PathBuf>> {
    let result = take(server, reason).await;
    let mut status = crate::lock(&server.backup);
    match &result {
        Ok(_) => {
            status.last_ok = Some(store::unix_millis() / 1000);
            status.error = None;
        }
        Err(e) => status.error = Some(e.to_string()),
    }
    result
}

async fn take(server: &Server, reason: Reason) -> Result<Option<PathBuf>> {
    let config = &server.config;
    let state_dir = &config.paths.state_dir;
    let files = {
        // Store first, then CA, the order every writer takes them in. With
        // both held nothing writes, so this is one state, not the middle
        // of a rotation.
        let _store = server.store.lock().await;
        let _ca = server.ca.read().await;
        store::blocking(|| read_state(state_dir, false))?
    };
    let dir = config.backup_dir();
    store::blocking(|| {
        if !reason.always() {
            let newest = list(&dir)?.pop().and_then(|path| read_manifest(&path).ok());
            if newest.is_some_and(|m| m.fingerprint == fingerprint(&files)) {
                return Ok(None);
            }
        }
        let path = write_snapshot(&dir, &files, reason)?;
        let owned =
            match_owner(&dir, state_dir, false).and_then(|()| match_owner(&path, state_dir, true));
        if let Err(e) = owned {
            tracing::warn!(error = %e, "snapshot written, but not owned like the state");
        }
        if config.backup.keep > 0 {
            if let Err(e) = prune(&dir, config.backup.keep) {
                tracing::warn!(error = %e, "cannot remove old snapshots");
            }
        }
        Ok(Some(path))
    })
}

/// Takes a snapshot and logs the outcome.
async fn take_logged(server: &Server, reason: Reason) {
    match snapshot(server, reason).await {
        Ok(Some(path)) => tracing::info!(
            path = %path.display(),
            reason = reason.as_str(),
            "state backed up"
        ),
        Ok(None) => tracing::debug!("state unchanged since the newest snapshot"),
        Err(e) => tracing::warn!(error = %e, "state backup failed"),
    }
}

/// The backup task of a running server: a snapshot at the start, then one
/// `settle_ms` after every change, and a waiting one at shutdown. Off with
/// `backup.keep = 0`.
pub(crate) async fn task(server: Arc<Server>) {
    if server.config.backup.keep == 0 {
        return;
    }
    let mut users = server.store.lock().await.changes();
    let mut certs = server.ca.read().await.changes();
    take_logged(&server, Reason::Start).await;
    let settle = Duration::from_millis(server.config.backup.settle_ms);
    loop {
        tokio::select! {
            changed = users.changed() => if changed.is_err() { return },
            changed = certs.changed() => if changed.is_err() { return },
            () = server.shutdown_requested() => {
                // A change and the shutdown can arrive in one poll, and the
                // select picks either.
                let pending = |rx: &tokio::sync::watch::Receiver<u64>| {
                    rx.has_changed().unwrap_or(false)
                };
                if pending(&users) || pending(&certs) {
                    take_logged(&server, Reason::Shutdown).await;
                }
                return;
            }
        }
        let shutdown = tokio::select! {
            () = tokio::time::sleep(settle) => false,
            () = server.shutdown_requested() => true,
        };
        // Whatever changed until now is in this snapshot; a change while it
        // is taken wakes the loop again.
        users.mark_unchanged();
        certs.mark_unchanged();
        let reason = if shutdown {
            Reason::Shutdown
        } else {
            Reason::Change
        };
        take_logged(&server, reason).await;
        if shutdown {
            return;
        }
    }
}

/// The snapshot `latest` means: the newest one that is not the state a
/// restore replaced, so `restore latest` twice restores the same thing.
pub fn latest(dir: &Path) -> Result<PathBuf> {
    list(dir)?
        .into_iter()
        .rev()
        .find(|path| read_manifest(path).is_ok_and(|m| m.reason != Reason::PreRestore.as_str()))
        .ok_or_else(|| Error::msg(format!("no snapshot in {}", dir.display())))
}

/// Reads the snapshot in `dir` and checks it against its manifest. A
/// directory without one — a copy of a state directory made by hand — is
/// taken as it is; [`check`] still loads it the way the server will.
fn load_snapshot(dir: &Path) -> Result<(Files, Option<Manifest>)> {
    if !dir.is_dir() {
        return Err(Error::msg(format!(
            "{} is not a snapshot directory",
            dir.display()
        )));
    }
    let manifest = match read_manifest(dir) {
        Ok(manifest) => Some(manifest),
        Err(_) if !dir.join(MANIFEST).exists() => None,
        Err(e) => return Err(e),
    };
    let mut files = read_state(dir, false)?;
    if let Some(m) = &manifest {
        if m.format > FORMAT || m.schema != store::SCHEMA {
            return Err(Error::msg(format!(
                "{} was taken by icu-server {} (snapshot format {}, users.toml schema {}); \
                 this one reads format {FORMAT}, schema {}",
                dir.display(),
                m.server_version,
                m.format,
                m.schema,
                store::SCHEMA
            )));
        }
        // What the manifest lists, and nothing that turned up beside it.
        files.retain(|name, _| {
            m.files.contains_key(name) || STATE_FILES.iter().any(|f| f.required && f.name == name)
        });
        for (name, sum) in &m.files {
            let bytes = files.get(name).ok_or_else(|| {
                Error::msg(format!(
                    "{name} is listed in {MANIFEST} but missing in {}",
                    dir.display()
                ))
            })?;
            if store::encode_hex(&Sha256::digest(bytes)) != *sum {
                return Err(Error::msg(format!(
                    "{} does not match its {MANIFEST}: damaged or edited",
                    dir.join(name).display()
                )));
            }
        }
    }
    Ok((files, manifest))
}

/// Loads `files` in a scratch directory under `state_dir` exactly as the
/// server will: `users.toml` and its schema, the CA pair, the server name,
/// the com key. Returns the number of users.
fn check(state_dir: &Path, config: &Config, files: &Files) -> Result<usize> {
    let staging = tempfile::Builder::new()
        .prefix(".icu-restore-")
        .tempdir_in(state_dir)
        .context(format!(
            "cannot create a scratch directory in {}",
            state_dir.display()
        ))?;
    for (name, bytes) in files {
        write_new(&staging.path().join(name), bytes)?;
    }
    let store = Store::open(staging.path())?;
    let users = store.users().count();
    let ca_name = crate::ca::ca_server_name(staging.path())?;
    let configured = config.server.server_name.trim();
    if let Some(ca_name) = &ca_name {
        if !configured.is_empty() && !ca_name.eq_ignore_ascii_case(configured) {
            return Err(Error::msg(format!(
                "the snapshot's CA was made for {ca_name:?}, but server.server_name is \
                 {configured:?}; set it to {ca_name:?} or remove it, then restore again"
            )));
        }
    }
    let name = ca_name.as_deref().unwrap_or(configured);
    if name.is_empty() {
        return Err(Error::msg(
            "the snapshot's CA names no server; set server.server_name to the name \
             clients connect to, then restore again",
        ));
    }
    crate::ca::Ca::load_or_create_if(staging.path(), &config.tls, name, || {
        Err(Error::msg("the snapshot has no CA"))
    })?;
    let com_key = files
        .get(store::COM_KEY_FILE)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    if icu_proto::com::ComKey::from_base64(&com_key).is_none() {
        return Err(Error::msg(format!(
            "{} in the snapshot is not a com key",
            store::COM_KEY_FILE
        )));
    }
    Ok(users)
}

/// What [`restore`] did.
#[derive(Debug)]
pub struct Restored {
    /// The snapshot restored.
    pub source: PathBuf,
    /// Its manifest; `None` for a copy made by hand.
    pub manifest: Option<Manifest>,
    /// Users on record now.
    pub users: usize,
    /// Where the state it replaced went; `None` for an empty state directory.
    pub pre_restore: Option<PathBuf>,
}

/// Puts the snapshot `source` (a directory, or `latest`) back into the state
/// directory of `config` while no server runs, then rotates the server leaf
/// as the runbook asks after every restore.
///
/// Nothing is replaced before the snapshot has loaded the way the server
/// will load it, and the state it replaces is kept as a `pre-restore`
/// snapshot first, so a restore can itself be undone. `server.crt.retired`
/// keeps what it lists: those leaves must reach the CRL either way. Takes
/// `config` unresolved: on a new host the server name comes from the
/// snapshot's CA, not from the host.
pub async fn restore(config: &Config, source: &str) -> Result<Restored> {
    let state_dir = config.paths.state_dir.clone();
    let backup_dir = config.backup_dir();
    let source = if source == "latest" {
        latest(&backup_dir)?
    } else {
        PathBuf::from(source)
    };
    let (files, manifest) = load_snapshot(&source)?;

    create_private_dir(&state_dir)?;
    if std::fs::canonicalize(&source)? == std::fs::canonicalize(&state_dir)? {
        return Err(Error::msg("that is the state directory itself"));
    }
    let lock = store::lock_state(&state_dir)
        .map_err(|e| e.context("stop the server first (systemctl stop icu-server)"))?;
    let users = check(&state_dir, config, &files)?;

    let current = read_state(&state_dir, true)?;
    let pre_restore = if current.is_empty() {
        None
    } else {
        Some(
            write_snapshot(&backup_dir, &current, Reason::PreRestore)
                .context("cannot keep the current state; nothing was restored")?,
        )
    };
    for file in &STATE_FILES {
        let Some(bytes) = files.get(file.name) else {
            // Not in the snapshot: only the disposable server leaf, which the
            // server issues anew, and retired leaves, which stay pending.
            continue;
        };
        let mut text = String::from_utf8(bytes.clone())
            .map_err(|_| Error::msg(format!("{} in the snapshot is not text", file.name)))?;
        if file.name == SERVER_RETIRED {
            if let Some(pending) = current.get(SERVER_RETIRED) {
                text.push_str(&String::from_utf8_lossy(pending));
            }
        }
        let path = state_dir.join(file.name);
        if file.secret {
            store::write_private(&path, &text)?;
        } else {
            store::write_atomic(&path, &text)?;
        }
    }
    drop(lock);

    let rotated = rotate_restored(config).await;
    let mut owned = match_owner(&backup_dir, &state_dir, true);
    for name in STATE_FILES.iter().map(|f| f.name).chain([store::LOCK_FILE]) {
        owned = owned.and(match_owner(&state_dir.join(name), &state_dir, false));
    }
    rotated?;
    owned.context("restored, but the files are not owned like the state directory")?;
    Ok(Restored {
        source,
        manifest,
        users,
        pre_restore,
    })
}

/// The rotation after a restore: a leaf the snapshot brought back may be on
/// the clients' CRL already. Then a snapshot of what is in place now.
async fn rotate_restored(config: &Config) -> Result<()> {
    let server = Server::offline(config.clone().resolve()?)?;
    crate::admin::rotate_server_cert(&server).await.context(
        "restored, but the server certificate was not rotated; run icu-server rotate-server-cert",
    )?;
    if let Err(e) = snapshot(&server, Reason::Restore).await {
        tracing::warn!(error = %e, "cannot take a snapshot of the restored state");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(dir: &Path, users: &str) {
        for file in &STATE_FILES {
            if file.required {
                std::fs::write(dir.join(file.name), format!("{} data", file.name)).unwrap();
            }
        }
        std::fs::write(dir.join(store::USERS_FILE), users).unwrap();
    }

    const USERS: &str = "schema = 1\nnext_user_id = 2\ncrl_number = 1790000000\n";

    #[test]
    fn the_crl_number_alone_is_no_change() {
        let dir = tempfile::tempdir().unwrap();
        state(dir.path(), USERS);
        let a = fingerprint(&read_state(dir.path(), false).unwrap());
        state(dir.path(), &USERS.replace("1790000000", "1790000123"));
        let b = fingerprint(&read_state(dir.path(), false).unwrap());
        assert_eq!(a, b);
        state(
            dir.path(),
            &USERS.replace("next_user_id = 2", "next_user_id = 3"),
        );
        let c = fingerprint(&read_state(dir.path(), false).unwrap());
        assert_ne!(a, c);
        std::fs::write(dir.path().join("server.crt"), "leaf").unwrap();
        let d = fingerprint(&read_state(dir.path(), false).unwrap());
        assert_ne!(c, d);
    }

    #[test]
    fn a_state_without_a_required_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        state(dir.path(), USERS);
        std::fs::remove_file(dir.path().join("com.key")).unwrap();
        let err = read_state(dir.path(), false).unwrap_err().to_string();
        assert!(err.contains("com.key is missing"), "{err}");
        assert_eq!(read_state(dir.path(), true).unwrap().len(), 3);
    }

    #[test]
    fn snapshots_sort_by_time_and_prune_from_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        for name in [
            "20260927T120000Z-10",
            "20260927T120000Z",
            "20260101T000000Z",
            "20260927T120000Z-2",
        ] {
            std::fs::create_dir_all(backups.join(name)).unwrap();
        }
        // Not snapshots: left alone by listing and pruning.
        std::fs::create_dir_all(backups.join(".icu-abc")).unwrap();
        std::fs::create_dir_all(backups.join("notes")).unwrap();
        std::fs::write(backups.join("20260927T130000Z"), "a file").unwrap();
        let names = |paths: Vec<PathBuf>| -> Vec<String> {
            paths
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            names(list(&backups).unwrap()),
            [
                "20260101T000000Z",
                "20260927T120000Z",
                "20260927T120000Z-2",
                "20260927T120000Z-10"
            ]
        );
        prune(&backups, 2).unwrap();
        assert_eq!(
            names(list(&backups).unwrap()),
            ["20260927T120000Z-2", "20260927T120000Z-10"]
        );
        assert!(backups.join("notes").exists());
        // Never the newest.
        prune(&backups, 0).unwrap();
        assert_eq!(names(list(&backups).unwrap()), ["20260927T120000Z-10"]);
        assert!(list(&dir.path().join("missing")).unwrap().is_empty());
    }

    #[test]
    fn a_snapshot_is_private_complete_and_verified() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        state(dir.path(), USERS);
        let backups = dir.path().join("backups");
        let files = read_state(dir.path(), false).unwrap();
        let path = write_snapshot(&backups, &files, Reason::Manual).unwrap();
        std::fs::create_dir_all(backups.join(".icu-leftover")).unwrap();
        let second = write_snapshot(&backups, &files, Reason::Manual).unwrap();
        assert_ne!(path, second, "two within one second get two names");
        // Pruning frees the first name; the next still sorts last.
        std::fs::remove_dir_all(&path).unwrap();
        let third = write_snapshot(&backups, &files, Reason::Manual).unwrap();
        assert_eq!(list(&backups).unwrap().last(), Some(&third));
        assert_ne!(third, path);
        std::fs::remove_dir_all(&third).unwrap();
        let path = write_snapshot(&backups, &files, Reason::Manual).unwrap();
        assert!(!backups.join(".icu-leftover").exists());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&backups), 0o700);
        assert_eq!(mode(&path), 0o700);
        assert_eq!(mode(&path.join("ca.key")), 0o600);
        assert_eq!(mode(&path.join("users.toml")), 0o600);

        let manifest = read_manifest(&path).unwrap();
        assert_eq!(manifest.reason, "manual");
        assert_eq!(manifest.schema, 1);
        assert_eq!(manifest.files.len(), 4);
        let (loaded, _) = load_snapshot(&path).unwrap();
        assert_eq!(loaded, files);

        std::fs::write(path.join("ca.key"), "edited").unwrap();
        let err = load_snapshot(&path).unwrap_err().to_string();
        assert!(err.contains("does not match its MANIFEST"), "{err}");

        let newer = std::fs::read_to_string(second.join(MANIFEST))
            .unwrap()
            .replace("schema = 1", "schema = 2");
        std::fs::write(second.join(MANIFEST), newer).unwrap();
        let err = load_snapshot(&second).unwrap_err().to_string();
        assert!(err.contains("schema 2"), "{err}");
    }

    #[test]
    fn latest_skips_the_state_a_restore_replaced() {
        let dir = tempfile::tempdir().unwrap();
        state(dir.path(), USERS);
        let backups = dir.path().join("backups");
        assert!(latest(&backups).is_err());
        let files = read_state(dir.path(), false).unwrap();
        let change = write_snapshot(&backups, &files, Reason::Change).unwrap();
        write_snapshot(&backups, &files, Reason::PreRestore).unwrap();
        assert_eq!(latest(&backups).unwrap(), change);
    }

    #[test]
    fn every_state_file_is_carried() {
        let names: Vec<&str> = STATE_FILES.iter().map(|f| f.name).collect();
        for name in crate::ca::FILES
            .into_iter()
            .chain([store::COM_KEY_FILE, store::USERS_FILE])
        {
            assert!(names.contains(&name), "{name}");
        }
    }
}
