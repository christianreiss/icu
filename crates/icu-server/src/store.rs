//! Persistent state: `users.toml`, `invites.toml`, `state.lock`
//! (`Server/IMPLEMENTATION.md` §4).
//!
//! All writes are atomic (temp file + rename) and happen while `state.lock` is
//! held with `flock`. A running server holds that lock for its whole lifetime
//! and is then the only writer (§5).

use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    os::fd::AsFd,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use base64::Engine;
use icu_proto::{error as ec, name_key, validate_name, UserId};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::error::{Context, Error, Result};

/// Schema version of both state files.
pub const SCHEMA: u32 = 1;

/// File name of the user database.
pub const USERS_FILE: &str = "users.toml";
/// File name of the invite database.
pub const INVITES_FILE: &str = "invites.toml";
/// File name of the lock file.
pub const LOCK_FILE: &str = "state.lock";

/// Formats a timestamp as RFC 3339 in UTC.
pub fn rfc3339(t: OffsetDateTime) -> String {
    // Whole seconds, as in the examples of `Server/IMPLEMENTATION.md` §4.
    t.to_offset(time::UtcOffset::UTC)
        .replace_nanosecond(0)
        .unwrap_or(t)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Parses an RFC 3339 timestamp.
pub fn parse_rfc3339(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).context(format!("bad timestamp {s:?}"))
}

/// Now, as an `OffsetDateTime`.
pub fn now() -> OffsetDateTime {
    OffsetDateTime::from(SystemTime::now())
}

/// Unix milliseconds, for `Pong.server_time_ms`.
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

/// Lowercase hex without separators.
pub fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// SHA-256 over a certificate DER, lowercase hex without separators.
pub fn fingerprint(der: &[u8]) -> String {
    encode_hex(&Sha256::digest(der))
}

/// One issued certificate of a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertRecord {
    /// SHA-256 of the leaf DER, lowercase hex.
    pub fingerprint: String,
    /// When it was issued, RFC 3339 UTC.
    pub issued: String,
    /// When it expires, RFC 3339 UTC.
    pub expires: String,
    /// Whether it was revoked.
    pub revoked: bool,
}

/// One user. A user has a stable [`UserId`] and over time several certificates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    /// Display name.
    pub name: String,
    /// When the user was enrolled, RFC 3339 UTC.
    pub created: String,
    /// When the last certificate was issued through `RenewRequest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_renew: Option<String>,
    /// All certificates ever issued to this user.
    #[serde(default, rename = "cert")]
    pub certs: Vec<CertRecord>,
    /// Grants `CreateInviteRequest` and other admin actions. Set once, at the
    /// first enrollment that completes against an empty store; changed
    /// afterwards only through `Store::set_admin`.
    #[serde(default)]
    pub is_admin: bool,
    /// Reversible login block, independent of certificate revocation. Unlike
    /// `revoke`, `Store::set_disabled(id, false)` restores access without
    /// re-enrollment.
    #[serde(default)]
    pub disabled: bool,
}

impl UserRecord {
    /// Whether every certificate this user ever had is revoked. A user with
    /// no certificate at all is not "revoked", it never had one.
    pub fn is_revoked(&self) -> bool {
        !self.certs.is_empty() && self.certs.iter().all(|c| c.revoked)
    }
}

/// `users.toml`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsersFile {
    /// Schema version.
    pub schema: u32,
    /// Next `user_id` to hand out.
    pub next_user_id: UserId,
    /// Users, keyed by the decimal `user_id` (TOML keys are strings).
    #[serde(default, rename = "user")]
    pub users: BTreeMap<String, UserRecord>,
}

impl Default for UsersFile {
    fn default() -> Self {
        UsersFile {
            schema: SCHEMA,
            next_user_id: 1,
            users: BTreeMap::new(),
        }
    }
}

/// One invite token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteRecord {
    /// Name the certificate CN will carry.
    pub name: String,
    /// When it was created, RFC 3339 UTC.
    pub created: String,
    /// When it expires, RFC 3339 UTC.
    pub expires: String,
    /// `0` = unused, otherwise the `user_id` it produced.
    pub used_by: UserId,
}

/// `invites.toml`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitesFile {
    /// Schema version.
    pub schema: u32,
    /// Invites, keyed by token.
    #[serde(default, rename = "invite")]
    pub invites: BTreeMap<String, InviteRecord>,
}

impl Default for InvitesFile {
    fn default() -> Self {
        InvitesFile {
            schema: SCHEMA,
            invites: BTreeMap::new(),
        }
    }
}

/// The persistent state, guarded by `flock(state.lock)`.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    users: UsersFile,
    invites: InvitesFile,
    /// Held for as long as the `Store` lives; dropping it releases the lock.
    _lock: File,
}

impl Store {
    /// Opens the state directory, takes the exclusive lock and loads both
    /// files. Missing files are created empty.
    ///
    /// Fails with a clear error when another process holds the lock, which is
    /// exactly the "CLI while the server runs" case (§5).
    pub fn open(dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(dir)
            .context(format!("cannot create state dir {}", dir.display()))?;
        let lock_path = dir.join(LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .context(format!("cannot open {}", lock_path.display()))?;
        rustix::fs::flock(
            lock.as_fd(),
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .context(format!(
            "cannot lock {} (is a server running?)",
            lock_path.display()
        ))?;

        let users: UsersFile = load_or_default(&dir.join(USERS_FILE))?;
        let invites: InvitesFile = load_or_default(&dir.join(INVITES_FILE))?;
        if users.schema != SCHEMA {
            return Err(Error::msg(format!(
                "{USERS_FILE}: schema {} is not supported (expected {SCHEMA})",
                users.schema
            )));
        }
        if invites.schema != SCHEMA {
            return Err(Error::msg(format!(
                "{INVITES_FILE}: schema {} is not supported (expected {SCHEMA})",
                invites.schema
            )));
        }
        Ok(Store {
            dir: dir.to_path_buf(),
            users,
            invites,
            _lock: lock,
        })
    }

    /// All users, keyed by `user_id`.
    pub fn users(&self) -> impl Iterator<Item = (UserId, &UserRecord)> {
        self.users
            .users
            .iter()
            .filter_map(|(k, v)| k.parse::<UserId>().ok().map(|id| (id, v)))
    }

    /// Looks a user up by id.
    pub fn user(&self, id: UserId) -> Option<&UserRecord> {
        self.users.users.get(&id.to_string())
    }

    /// Looks a user up by id for mutation. The `BTreeMap` key is the decimal
    /// `user_id`; this is the only place that encoding is spelled out.
    fn user_mut(&mut self, id: UserId) -> Result<&mut UserRecord> {
        self.users
            .users
            .get_mut(&id.to_string())
            .ok_or_else(|| Error::msg(format!("unknown user_id {id}")))
    }

    /// Whether `id` is an admin. `false` for an unknown id.
    pub fn is_admin(&self, id: UserId) -> bool {
        self.user(id).map(|u| u.is_admin).unwrap_or(false)
    }

    /// Whether the store has no users at all yet (bootstrap gate, §6.3).
    pub fn is_empty(&self) -> bool {
        self.users.users.is_empty()
    }

    /// Resolves a certificate fingerprint to its user and certificate.
    pub fn by_fingerprint(&self, fp: &str) -> Option<(UserId, &UserRecord, &CertRecord)> {
        for (id, user) in self.users() {
            if let Some(cert) = user.certs.iter().find(|c| c.fingerprint == fp) {
                return Some((id, user, cert));
            }
        }
        None
    }

    /// Resolves a CLI selector: `user_id`, name (case-insensitive) or
    /// certificate fingerprint.
    pub fn resolve(&self, selector: &str) -> Option<UserId> {
        if let Ok(id) = selector.parse::<UserId>() {
            if self.user(id).is_some() {
                return Some(id);
            }
        }
        let key = name_key(selector);
        for (id, user) in self.users() {
            if name_key(&user.name) == key {
                return Some(id);
            }
        }
        self.by_fingerprint(selector).map(|(id, _, _)| id)
    }

    /// Whether `name` is already taken, ignoring `except`.
    pub fn name_taken(&self, name: &str, except: Option<UserId>) -> bool {
        let key = name_key(name);
        self.users()
            .any(|(id, u)| Some(id) != except && name_key(&u.name) == key)
    }

    /// Makes `desired` unique by appending `-2`, `-3`, … (`PROTOCOL.md` §2.3).
    ///
    /// The base is truncated on a UTF-8 boundary when the suffix would push the
    /// name past 32 bytes.
    pub fn unique_name(&self, desired: &str) -> String {
        if !self.name_taken(desired, None) {
            return desired.to_string();
        }
        for n in 2u32..10_000 {
            let suffix = format!("-{n}");
            let max_base = icu_proto::types::NAME_MAX_LEN - suffix.len();
            let mut base = desired;
            while base.len() > max_base {
                base = &base[..base
                    .char_indices()
                    .take_while(|(i, _)| *i < max_base)
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0)];
            }
            let candidate = format!("{base}{suffix}");
            if !self.name_taken(&candidate, None) {
                return candidate;
            }
        }
        // 10k collisions on one name is not a real case; fall back to the id.
        format!("user-{}", self.users.next_user_id)
    }

    /// The id the next [`Store::add_user`] will hand out.
    pub fn peek_next_user_id(&self) -> UserId {
        self.users.next_user_id
    }

    /// Adds a new user with its first certificate. Returns the new `user_id`.
    pub fn add_user(&mut self, name: &str, cert: CertRecord, is_admin: bool) -> Result<UserId> {
        validate_name(name)?;
        let id = self.users.next_user_id;
        self.users.next_user_id = id
            .checked_add(1)
            .ok_or_else(|| Error::msg("user_id space exhausted"))?;
        self.users.users.insert(
            id.to_string(),
            UserRecord {
                name: name.to_string(),
                created: rfc3339(now()),
                last_renew: None,
                certs: vec![cert],
                is_admin,
                disabled: false,
            },
        );
        self.save_users()?;
        Ok(id)
    }

    /// Appends another certificate to an existing user (renewal). The older
    /// certificate stays valid until it expires.
    pub fn add_cert(&mut self, id: UserId, cert: CertRecord, renewal: bool) -> Result<()> {
        let record = self.user_mut(id)?;
        record.certs.push(cert);
        if renewal {
            record.last_renew = Some(rfc3339(now()));
        }
        self.save_users()
    }

    /// When the user last renewed, if ever.
    pub fn last_renew(&self, id: UserId) -> Option<OffsetDateTime> {
        self.user(id)
            .and_then(|u| u.last_renew.as_deref())
            .and_then(|s| parse_rfc3339(s).ok())
    }

    /// Revokes **all** certificates of a user (§5). Guarded against revoking
    /// the last remaining functional admin.
    pub fn revoke(&mut self, id: UserId) -> Result<bool> {
        self.guard_last_admin(id)?;
        let record = self.user_mut(id)?;
        let mut changed = false;
        for cert in &mut record.certs {
            if !cert.revoked {
                cert.revoked = true;
                changed = true;
            }
        }
        self.save_users()?;
        Ok(changed)
    }

    /// Whether `id` currently provides working admin capability: admin flag
    /// set, not disabled, and at least one certificate not revoked. Cert
    /// expiry is deliberately not checked, matching `AdminUser.revoked`'s
    /// existing simplification.
    fn is_functional_admin(&self, id: UserId) -> bool {
        self.user(id)
            .map(|u| u.is_admin && !u.disabled && u.certs.iter().any(|c| !c.revoked))
            .unwrap_or(false)
    }

    /// How many users currently provide working admin capability.
    fn functional_admin_count(&self) -> usize {
        self.users()
            .filter(|(id, _)| self.is_functional_admin(*id))
            .count()
    }

    /// Rejects an action against `id` that would leave zero functional
    /// admins. A no-op for anyone who is not currently a functional admin
    /// (e.g. already disabled or fully revoked), so it never blocks cleanup
    /// of a non-functional account.
    fn guard_last_admin(&self, id: UserId) -> Result<()> {
        if self.is_functional_admin(id) && self.functional_admin_count() <= 1 {
            return Err(Error::coded(
                ec::LAST_ADMIN,
                "cannot demote, disable or revoke the last remaining admin",
            ));
        }
        Ok(())
    }

    /// Grants or revokes admin rights. Guarded against demoting the last
    /// remaining functional admin. Returns whether anything changed.
    pub fn set_admin(&mut self, id: UserId, is_admin: bool) -> Result<bool> {
        if !is_admin {
            self.guard_last_admin(id)?;
        }
        let record = self.user_mut(id)?;
        let changed = record.is_admin != is_admin;
        record.is_admin = is_admin;
        if changed {
            self.save_users()?;
        }
        Ok(changed)
    }

    /// Blocks or restores login without touching certificates. Guarded
    /// against disabling the last remaining functional admin. Restoring
    /// (`disabled: false`) makes existing non-revoked certificates usable
    /// again immediately, no re-enrollment needed. Returns whether anything
    /// changed.
    pub fn set_disabled(&mut self, id: UserId, disabled: bool) -> Result<bool> {
        if disabled {
            self.guard_last_admin(id)?;
        }
        let record = self.user_mut(id)?;
        let changed = record.disabled != disabled;
        record.disabled = disabled;
        if changed {
            self.save_users()?;
        }
        Ok(changed)
    }

    /// Renames a user. Fails with `NAME_TAKEN` on a case-insensitive
    /// collision.
    pub fn rename(&mut self, id: UserId, new: &str) -> Result<()> {
        validate_name(new)?;
        if self.name_taken(new, Some(id)) {
            return Err(Error::coded(ec::NAME_TAKEN, format!("name {new:?} taken")));
        }
        let record = self.user_mut(id)?;
        record.name = new.to_string();
        self.save_users()
    }

    /// Sets the display name only if the user has none yet. Returns the name
    /// in force afterwards (`PROTOCOL.md` §2.2: `Hello.name`).
    pub fn set_initial_name(&mut self, id: UserId, proposed: &str) -> Result<String> {
        let current = self
            .user(id)
            .map(|u| u.name.clone())
            .ok_or_else(|| Error::msg(format!("unknown user_id {id}")))?;
        if !current.is_empty() {
            return Ok(current);
        }
        if validate_name(proposed).is_err() {
            return Ok(current);
        }
        let unique = self.unique_name(proposed);
        self.rename(id, &unique)?;
        Ok(unique)
    }

    /// All invites.
    pub fn invites(&self) -> &BTreeMap<String, InviteRecord> {
        &self.invites.invites
    }

    /// Creates an invite token, valid for `ttl`.
    pub fn create_invite(&mut self, name: &str, ttl: Duration) -> Result<(String, InviteRecord)> {
        validate_name(name)?;
        let mut raw = [0u8; 16];
        rand::rng().fill_bytes(&mut raw);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let created = now();
        let record = InviteRecord {
            name: name.to_string(),
            created: rfc3339(created),
            expires: rfc3339(created + ttl),
            used_by: 0,
        };
        self.invites.invites.insert(token.clone(), record.clone());
        self.save_invites()?;
        Ok((token, record))
    }

    /// Drops expired, unused invites (called at startup, §6.1.4).
    pub fn purge_expired_invites(&mut self) -> Result<usize> {
        let now = now();
        let before = self.invites.invites.len();
        self.invites.invites.retain(|_, inv| {
            if inv.used_by != 0 {
                return true;
            }
            parse_rfc3339(&inv.expires)
                .map(|e| e > now)
                .unwrap_or(false)
        });
        let removed = before - self.invites.invites.len();
        if removed > 0 {
            self.save_invites()?;
        }
        Ok(removed)
    }

    /// Looks up a token in constant time and returns its record if it is
    /// usable. Does **not** consume it.
    pub fn peek_invite(&self, token: &str) -> Option<(String, InviteRecord)> {
        let probe = token.as_bytes();
        let mut found: Option<(String, InviteRecord)> = None;
        for (key, record) in &self.invites.invites {
            let same_len = key.len() == probe.len();
            let equal = same_len && bool::from(key.as_bytes().ct_eq(probe));
            if equal {
                found = Some((key.clone(), record.clone()));
            }
        }
        let (key, record) = found?;
        if record.used_by != 0 {
            return None;
        }
        if parse_rfc3339(&record.expires).ok()? <= now() {
            return None;
        }
        Some((key, record))
    }

    /// Compare-and-set: marks the token used by `user_id`. Returns the record
    /// as it was before. Fails if the token was consumed meanwhile (§6.3.2).
    pub fn consume_invite(&mut self, token: &str, user_id: UserId) -> Result<InviteRecord> {
        let (key, record) = self
            .peek_invite(token)
            .ok_or_else(|| Error::coded(ec::ENROLL_BAD_TOKEN, "token unknown, used or expired"))?;
        let entry = self
            .invites
            .invites
            .get_mut(&key)
            .ok_or_else(|| Error::coded(ec::ENROLL_BAD_TOKEN, "token vanished"))?;
        if entry.used_by != 0 {
            return Err(Error::coded(ec::ENROLL_BAD_TOKEN, "token already used"));
        }
        entry.used_by = user_id;
        self.save_invites()?;
        Ok(record)
    }

    /// Releases a token again, used when a later step of the enrollment fails.
    pub fn release_invite(&mut self, token: &str) -> Result<()> {
        if let Some(entry) = self.invites.invites.get_mut(token) {
            entry.used_by = 0;
        }
        self.save_invites()
    }

    /// Path of the state directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn save_users(&self) -> Result<()> {
        write_atomic(&self.dir.join(USERS_FILE), &toml::to_string(&self.users)?)
    }

    fn save_invites(&self) -> Result<()> {
        write_atomic(
            &self.dir.join(INVITES_FILE),
            &toml::to_string(&self.invites)?,
        )
    }
}

fn load_or_default<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).context(format!("cannot parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(Error::from(e).context(format!("cannot read {}", path.display()))),
    }
}

/// Writes `contents` atomically: temp file in the same directory, fsync,
/// rename, fsync of the directory (`Server/IMPLEMENTATION.md` §4). `mode` is
/// the permission bits of the temp file, and therefore of the result.
fn write_mode(path: &Path, contents: &str, mode: u32) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state")
    ));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)
            .context(format!("cannot create {}", tmp.display()))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path).context(format!("cannot rename onto {}", path.display()))?;
    if let Ok(dirfile) = File::open(dir) {
        let _ = dirfile.sync_all();
    }
    Ok(())
}

/// Writes a world-readable file atomically.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    write_mode(path, contents, 0o644)
}

/// Writes a private file with mode 0600, atomically.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    write_mode(path, contents, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert(fp: &str) -> CertRecord {
        let t = now();
        CertRecord {
            fingerprint: fp.to_string(),
            issued: rfc3339(t),
            expires: rfc3339(t + Duration::from_secs(365 * 86400)),
            revoked: false,
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    /// §10: `user_id` assignment and stability across renewal.
    #[test]
    fn user_ids_are_monotonic_and_stable_across_renewal() {
        let (_dir, mut s) = store();
        let alice = s.add_user("alice", cert("aa"), false).unwrap();
        let bob = s.add_user("bob", cert("bb"), false).unwrap();
        assert_eq!((alice, bob), (1, 2));

        s.add_cert(alice, cert("cc"), true).unwrap();
        assert_eq!(s.user(alice).unwrap().certs.len(), 2);
        assert_eq!(s.by_fingerprint("cc").unwrap().0, alice, "same user_id");
        assert_eq!(s.by_fingerprint("aa").unwrap().0, alice, "old cert stays");
        assert!(s.last_renew(alice).is_some());

        // Ids are never reused: removing is not even offered, and next_user_id
        // only grows.
        let carol = s.add_user("carol", cert("dd"), false).unwrap();
        assert_eq!(carol, 3);
    }

    /// §10: revoke persists across **all** certificates of a user.
    #[test]
    fn revoke_marks_every_certificate_and_persists() {
        let (dir, mut s) = store();
        let alice = s.add_user("alice", cert("aa"), false).unwrap();
        s.add_cert(alice, cert("bb"), true).unwrap();
        assert!(s.revoke(alice).unwrap());
        assert!(!s.revoke(alice).unwrap(), "second revoke changes nothing");
        drop(s);

        let s = Store::open(dir.path()).unwrap();
        let user = s.user(alice).unwrap();
        assert_eq!(user.certs.len(), 2);
        assert!(user.certs.iter().all(|c| c.revoked));
    }

    /// §10: promote/demote updates `is_admin` and persists.
    #[test]
    fn promoting_and_demoting_updates_is_admin() {
        let (dir, mut s) = store();
        let alice = s.add_user("alice", cert("aa"), true).unwrap();
        let bob = s.add_user("bob", cert("bb"), false).unwrap();
        assert!(s.set_admin(bob, true).unwrap());
        assert!(s.is_admin(bob));
        assert!(
            !s.set_admin(bob, true).unwrap(),
            "no-op is reported as unchanged"
        );
        assert!(s.set_admin(bob, false).unwrap());
        assert!(!s.is_admin(bob));
        drop(s);

        let s = Store::open(dir.path()).unwrap();
        assert!(s.is_admin(alice));
        assert!(!s.is_admin(bob));
    }

    /// §10: disable blocks nothing at the store layer (session.rs enforces
    /// it), and enable restores without touching certificates.
    #[test]
    fn disabling_and_enabling_round_trips_without_touching_certs() {
        let (_dir, mut s) = store();
        let alice = s.add_user("alice", cert("aa"), true).unwrap();
        let bob = s.add_user("bob", cert("bb"), false).unwrap();
        assert!(s.set_disabled(bob, true).unwrap());
        assert!(s.user(bob).unwrap().disabled);
        assert!(
            !s.user(bob).unwrap().certs[0].revoked,
            "disable is not revoke"
        );
        assert!(s.set_disabled(bob, false).unwrap());
        assert!(!s.user(bob).unwrap().disabled);
        assert!(s.is_admin(alice), "unaffected");
    }

    /// §10: the last-admin guard — demote.
    #[test]
    fn demoting_the_last_functional_admin_is_rejected() {
        let (_dir, mut s) = store();
        let alice = s.add_user("alice", cert("aa"), true).unwrap();
        let err = s.set_admin(alice, false).unwrap_err();
        assert_eq!(err.code(), Some(ec::LAST_ADMIN));
        assert!(s.is_admin(alice), "unchanged");
    }

    /// §10: the last-admin guard — disable.
    #[test]
    fn disabling_the_last_functional_admin_is_rejected() {
        let (_dir, mut s) = store();
        let alice = s.add_user("alice", cert("aa"), true).unwrap();
        let err = s.set_disabled(alice, true).unwrap_err();
        assert_eq!(err.code(), Some(ec::LAST_ADMIN));
        assert!(!s.user(alice).unwrap().disabled, "unchanged");
    }

    /// §10: the last-admin guard — revoke.
    #[test]
    fn revoking_the_last_functional_admin_is_rejected() {
        let (_dir, mut s) = store();
        let alice = s.add_user("alice", cert("aa"), true).unwrap();
        let err = s.revoke(alice).unwrap_err();
        assert_eq!(err.code(), Some(ec::LAST_ADMIN));
        assert!(!s.user(alice).unwrap().certs[0].revoked, "unchanged");
    }

    /// §10: the guard only counts *functional* admins — disabling one of two
    /// admins is fine, but demoting the resulting sole functional admin is
    /// then rejected, and acting further on the already-disabled admin is
    /// never blocked by the guard (they no longer count).
    #[test]
    fn guard_counts_functional_admins_only() {
        let (_dir, mut s) = store();
        let alice = s.add_user("alice", cert("aa"), true).unwrap();
        let bob = s.add_user("bob", cert("bb"), true).unwrap();

        // Two functional admins: disabling one is fine.
        assert!(s.set_disabled(alice, true).unwrap());

        // Now bob is the sole functional admin: demoting him is rejected.
        let err = s.set_admin(bob, false).unwrap_err();
        assert_eq!(err.code(), Some(ec::LAST_ADMIN));

        // Alice, already non-functional (disabled), is never blocked by the
        // guard — e.g. revoking her outright is unaffected.
        assert!(s.revoke(alice).unwrap());
    }

    /// §10: the guard is a no-op for ordinary, non-admin users.
    #[test]
    fn guard_does_not_affect_non_admins() {
        let (_dir, mut s) = store();
        s.add_user("alice", cert("aa"), true).unwrap();
        let bob = s.add_user("bob", cert("bb"), false).unwrap();
        assert!(s.revoke(bob).unwrap());
        assert!(s.set_disabled(bob, true).unwrap());
    }

    /// §10: name collision gets a `-2` / `-3` suffix.
    #[test]
    fn unique_name_appends_suffixes() {
        let (_dir, mut s) = store();
        s.add_user("alice", cert("aa"), false).unwrap();
        assert_eq!(s.unique_name("bob"), "bob");
        assert_eq!(s.unique_name("alice"), "alice-2");
        // The collision check is case-insensitive, the returned name keeps the
        // requested spelling.
        assert_eq!(s.unique_name("ALICE"), "ALICE-2");
        s.add_user("alice-2", cert("bb"), false).unwrap();
        assert_eq!(s.unique_name("alice"), "alice-3");
    }

    #[test]
    fn unique_name_truncates_on_a_char_boundary() {
        let (_dir, mut s) = store();
        let long = "ä".repeat(16); // 32 bytes
        s.add_user(&long, cert("aa"), false).unwrap();
        let unique = s.unique_name(&long);
        assert!(unique.len() <= icu_proto::types::NAME_MAX_LEN, "{unique}");
        assert!(validate_name(&unique).is_ok());
        assert!(unique.ends_with("-2"));
    }

    #[test]
    fn rename_rejects_collisions() {
        let (_dir, mut s) = store();
        let alice = s.add_user("alice", cert("aa"), false).unwrap();
        s.add_user("bob", cert("bb"), false).unwrap();
        let err = s.rename(alice, "BOB").unwrap_err();
        assert_eq!(err.code(), Some(ec::NAME_TAKEN));
        assert_eq!(s.user(alice).unwrap().name, "alice");
        s.rename(alice, "Alice").unwrap(); // own name, different case
        s.rename(alice, "carol").unwrap();
        assert_eq!(s.user(alice).unwrap().name, "carol");
    }

    #[test]
    fn resolve_by_id_name_and_fingerprint() {
        let (_dir, mut s) = store();
        let alice = s.add_user("alice", cert("9f86d0"), false).unwrap();
        assert_eq!(s.resolve("1"), Some(alice));
        assert_eq!(s.resolve("ALICE"), Some(alice));
        assert_eq!(s.resolve("9f86d0"), Some(alice));
        assert_eq!(s.resolve("nobody"), None);
    }

    /// §10: atomic write — the file is valid TOML after every operation and
    /// survives a reload.
    #[test]
    fn state_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut s = Store::open(dir.path()).unwrap();
            let id = s.add_user("alice", cert("aa"), false).unwrap();
            s.create_invite("bob", Duration::from_secs(60)).unwrap();
            assert_eq!(id, 1);
        }
        let text = std::fs::read_to_string(dir.path().join(USERS_FILE)).unwrap();
        assert!(text.contains("schema = 1"), "{text}");
        assert!(text.contains("[user.1]"), "{text}");
        assert!(text.contains("[[user.1.cert]]"), "{text}");

        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.user(1).unwrap().name, "alice");
        assert_eq!(s.invites().len(), 1);
        assert_eq!(s.users.next_user_id, 2);
    }

    /// §10: schema check on load.
    #[test]
    fn unknown_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(USERS_FILE),
            "schema = 2\nnext_user_id = 1\n",
        )
        .unwrap();
        let err = Store::open(dir.path()).unwrap_err().to_string();
        assert!(err.contains("schema 2"), "{err}");
    }

    /// §10: flock behaviour with a concurrent CLI access.
    #[test]
    fn the_lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let first = Store::open(dir.path()).unwrap();
        let err = Store::open(dir.path()).unwrap_err().to_string();
        assert!(err.contains("cannot lock"), "{err}");
        drop(first);
        // Once released, the next writer gets in.
        assert!(Store::open(dir.path()).is_ok());
    }

    #[test]
    fn invites_are_single_use_and_expire() {
        let (_dir, mut s) = store();
        let (token, record) = s.create_invite("bob", Duration::from_secs(3600)).unwrap();
        assert_eq!(token.len(), 22, "128 bit base64url without padding");
        assert_eq!(record.used_by, 0);
        assert!(s.peek_invite(&token).is_some());
        assert!(s.peek_invite("nope").is_none());

        s.consume_invite(&token, 7).unwrap();
        assert!(s.peek_invite(&token).is_none(), "used once");
        let err = s.consume_invite(&token, 8).unwrap_err();
        assert_eq!(err.code(), Some(ec::ENROLL_BAD_TOKEN));

        // A released token works again (enrollment rollback path).
        s.release_invite(&token).unwrap();
        assert!(s.peek_invite(&token).is_some());
    }

    #[test]
    fn expired_invites_are_purged_but_used_ones_are_kept() {
        let (_dir, mut s) = store();
        let (fresh, _) = s.create_invite("bob", Duration::from_secs(3600)).unwrap();
        let (stale, _) = s.create_invite("carl", Duration::from_secs(3600)).unwrap();
        let (used, _) = s.create_invite("dora", Duration::from_secs(3600)).unwrap();
        s.consume_invite(&used, 1).unwrap();
        // Backdate the stale one.
        let past = rfc3339(now() - Duration::from_secs(10));
        s.invites.invites.get_mut(&stale).unwrap().expires = past;

        assert_eq!(s.purge_expired_invites().unwrap(), 1);
        assert!(s.invites().contains_key(&fresh));
        assert!(s.invites().contains_key(&used));
        assert!(!s.invites().contains_key(&stale));
        assert!(s.peek_invite(&stale).is_none());
    }

    #[test]
    fn fingerprints_are_stable_lowercase_hex() {
        // Known vector: SHA-256 of the empty input.
        assert_eq!(
            fingerprint(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(fingerprint(b"abc").len(), 64);
        assert_eq!(fingerprint(b"abc"), fingerprint(b"abc"));
    }

    #[test]
    fn hello_name_only_applies_when_none_is_stored() {
        let (_dir, mut s) = store();
        let id = s.add_user("alice", cert("aa"), false).unwrap();
        assert_eq!(s.set_initial_name(id, "mallory").unwrap(), "alice");

        // A user without a name (should not happen through enrollment, but the
        // code path must be sound).
        s.users.users.get_mut(&id.to_string()).unwrap().name = String::new();
        assert_eq!(s.set_initial_name(id, "mallory").unwrap(), "mallory");
        assert_eq!(s.user(id).unwrap().name, "mallory");
    }

    #[test]
    fn timestamps_roundtrip_as_rfc3339_utc() {
        let t = now();
        let s = rfc3339(t);
        assert!(s.ends_with('Z'), "{s}");
        let back = parse_rfc3339(&s).unwrap();
        assert!((back - t).abs() < Duration::from_secs(1));
        assert!(parse_rfc3339("not a date").is_err());
    }
}
