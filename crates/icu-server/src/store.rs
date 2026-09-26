//! Persistent state: `users.toml`, `state.lock` (`Server/IMPLEMENTATION.md`
//! §4).
//!
//! There is no invite table: an invitation is a registration certificate,
//! recorded like any other certificate with `registration = true`
//! (`PROTOCOL.md` §2.3). A leftover `invites.toml` from before is ignored.
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

use icu_proto::{error as ec, name_key, validate_name, UserId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::error::{Context, Error, Result};

/// Schema version of the state file.
pub const SCHEMA: u32 = 1;

/// Redeems of one registration bundle without a receipt (`PROTOCOL.md`
/// §2.3). One is the rule, a second repairs a lost answer; past this many
/// something is wrong, and each one costs a certificate on the CRL.
pub const REDEEMS_PER_BUNDLE: usize = 5;

/// File name of the user database.
pub const USERS_FILE: &str = "users.toml";
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

/// Inverse of [`encode_hex`]; `None` for anything that is not even-length
/// hex.
pub fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
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
    /// Subject CN this certificate carries, i.e. the display name it was
    /// issued for. `""` for records written before the name moved into the
    /// certificate — the DER is not kept, so it cannot be back-filled, and
    /// `""` therefore means "unknown", never "empty name".
    #[serde(default)]
    pub cn: String,
    /// When it was issued, RFC 3339 UTC.
    pub issued: String,
    /// When it expires, RFC 3339 UTC.
    pub expires: String,
    /// Whether it was revoked.
    pub revoked: bool,
    /// When this certificate was last used for a *catch-up* renewal, RFC 3339
    /// UTC. Those are exempt from the per-user daily limit, so this is what
    /// bounds them. A plain renewal or a rename does not stamp it: the
    /// certificate it was requested with may well be sitting on a second
    /// device that still has its own catch-up to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewed: Option<String>,
    /// A registration certificate (`PROTOCOL.md` §2.3): its session may only
    /// swap it for a certificate on the client's own key, and it is revoked
    /// once the client confirms it stored that one
    /// ([`Store::consume_registration`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub registration: bool,
    /// Revoked because a later certificate of the same user logged in
    /// ([`Store::retire_predecessors`]) or a later redeem of the same bundle
    /// replaced it ([`Store::redeem_registration`]), not by an admin. Once
    /// expired, such a record goes and leaves the CRL: its fingerprint is
    /// then simply unknown.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub superseded: bool,
    /// Serial number, lowercase hex of the DER integer's content octets —
    /// what the CRL lists (`PROTOCOL.md` §1). `None` for records written
    /// before the CRL existed; those stay covered by the fingerprint check
    /// alone, since the DER is not kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

impl CertRecord {
    /// The record of a leaf just issued for `cn`, unrevoked; `expires` and
    /// the serial are read from the DER. `issued` is passed in rather than
    /// read as notBefore, which the CA backdates by an hour of clock slack.
    pub fn issued(leaf: &[u8], cn: String, issued: String, registration: bool) -> CertRecord {
        CertRecord {
            fingerprint: fingerprint(leaf),
            cn,
            issued,
            expires: crate::ca::leaf_not_after(leaf),
            revoked: false,
            renewed: None,
            registration,
            superseded: false,
            serial: Some(crate::ca::leaf_serial(leaf)),
        }
    }

    fn expired(&self, now: OffsetDateTime) -> bool {
        parse_rfc3339(&self.expires)
            .map(|e| e <= now)
            .unwrap_or(false)
    }
}

/// One user. A user has a stable [`UserId`] and over time several certificates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    /// Display name — a mirror of the subject CN of the most recently issued
    /// certificate (`PROTOCOL.md` §2.1). Only `Store::add_user` and
    /// `Store::add_cert` write it.
    pub name: String,
    /// When the user was enrolled, RFC 3339 UTC.
    pub created: String,
    /// When the last certificate was issued through `RenewRequest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_renew: Option<String>,
    /// All certificates ever issued to this user.
    #[serde(default, rename = "cert")]
    pub certs: Vec<CertRecord>,
    /// Grants `CreateInviteRequest` and other admin actions. Set for the first
    /// user to confirm a registration while no confirmed admin exists
    /// (`Store::consume_registration`; marked provisionally before that);
    /// changed afterwards only through `Store::set_admin`.
    #[serde(default)]
    pub is_admin: bool,
    /// Reversible login block, independent of certificate revocation. Unlike
    /// `revoke`, `Store::set_disabled(id, false)` restores access without
    /// re-enrollment.
    #[serde(default)]
    pub disabled: bool,
    /// Mute enforced by an admin (`AdminSetRestriction`): bit0 of the user's
    /// `UserFlags` stays set whatever the client sends. Survives reconnects
    /// and restarts, or it would be undone by reconnecting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub server_muted: bool,
    /// Deafen enforced by an admin, like `server_muted` for bit1.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub server_deafened: bool,
}

impl UserRecord {
    /// Whether every certificate this user ever had is revoked. A user with
    /// no certificate at all is not "revoked", it never had one.
    pub fn is_revoked(&self) -> bool {
        !self.certs.is_empty() && self.certs.iter().all(|c| c.revoked)
    }

    /// Whether the user was invited but has not confirmed a registration yet:
    /// every certificate is a registration certificate, or one is still
    /// unrevoked — then a redeem was never confirmed, and the certificate it
    /// produced may never have reached anybody.
    pub fn is_pending(&self) -> bool {
        !self.certs.is_empty()
            && (self.certs.iter().all(|c| c.registration)
                || self.certs.iter().any(|c| c.registration && !c.revoked))
    }
}

/// `users.toml`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsersFile {
    /// Schema version.
    pub schema: u32,
    /// Next `user_id` to hand out.
    pub next_user_id: UserId,
    /// Number of the most recently issued CRL; strictly increasing, so a
    /// client can refuse an older one (`PROTOCOL.md` §2.2 `CrlUpdate`).
    #[serde(default)]
    pub crl_number: u64,
    /// Server leaves replaced by `rotate-server-cert`. They stay on the CRL
    /// until they expire, so a leaked old server key is worthless.
    #[serde(default, rename = "revoked_server")]
    pub revoked_server: Vec<RevokedServerCert>,
    /// Users, keyed by the decimal `user_id` (TOML keys are strings).
    #[serde(default, rename = "user")]
    pub users: BTreeMap<String, UserRecord>,
}

/// A server leaf on the CRL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokedServerCert {
    /// Serial, lowercase hex (see [`CertRecord::serial`]).
    pub serial: String,
    /// When it expires, RFC 3339 UTC; afterwards it leaves the CRL.
    pub expires: String,
}

impl RevokedServerCert {
    /// Whether the leaf still has to be listed; an unreadable expiry keeps it.
    fn live(&self, now: OffsetDateTime) -> bool {
        parse_rfc3339(&self.expires)
            .map(|e| e > now)
            .unwrap_or(true)
    }
}

impl Default for UsersFile {
    fn default() -> Self {
        UsersFile {
            schema: SCHEMA,
            next_user_id: 1,
            crl_number: 0,
            revoked_server: Vec::new(),
            users: BTreeMap::new(),
        }
    }
}

/// The persistent state, guarded by `flock(state.lock)`.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    users: UsersFile,
    /// Held for as long as the `Store` lives; dropping it releases the lock.
    _lock: File,
}

impl Store {
    /// Opens the state directory, takes the exclusive lock and loads the user
    /// database. A missing file means an empty one.
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
        if users.schema != SCHEMA {
            return Err(Error::msg(format!(
                "{USERS_FILE}: schema {} is not supported (expected {SCHEMA})",
                users.schema
            )));
        }
        Ok(Store {
            dir: dir.to_path_buf(),
            users,
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

    /// Applies `change` to a copy of the user database, writes the copy and
    /// only then adopts it. A failed write (disk full, permissions) therefore
    /// leaves the running server exactly as it was, instead of serving a
    /// state that is gone after the next restart.
    fn update_users<R>(&mut self, change: impl FnOnce(&mut UsersFile) -> Result<R>) -> Result<R> {
        let mut next = self.users.clone();
        let out = change(&mut next)?;
        write_atomic(&self.dir.join(USERS_FILE), &toml::to_string(&next)?)?;
        self.users = next;
        Ok(out)
    }

    /// Whether `id` is an admin. `false` for an unknown id.
    pub fn is_admin(&self, id: UserId) -> bool {
        self.user(id).map(|u| u.is_admin).unwrap_or(false)
    }

    /// Whether the store has no users at all yet. The first registration
    /// issued against an empty store is marked admin — provisionally: the
    /// first user to confirm a registration becomes admin (§6.3).
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

    /// Resolves a CLI selector: `user_id`, name or certificate fingerprint.
    /// A name matches exactly first, else case-insensitively if that is
    /// unambiguous. Never by `name_key`: that also folds look-alikes, and a
    /// revoke must not land on the wrong account.
    pub fn resolve(&self, selector: &str) -> Option<UserId> {
        if let Ok(id) = selector.parse::<UserId>() {
            if self.user(id).is_some() {
                return Some(id);
            }
        }
        if let Some((id, _)) = self.users().find(|(_, u)| u.name == selector) {
            return Some(id);
        }
        let lower = selector.to_lowercase();
        let mut folded = self
            .users()
            .filter(|(_, u)| u.name.to_lowercase() == lower)
            .map(|(id, _)| id);
        if let (Some(id), None) = (folded.next(), folded.next()) {
            return Some(id);
        }
        self.by_fingerprint(selector).map(|(id, _, _)| id)
    }

    /// Whether `name` is already taken, ignoring `except`.
    pub fn name_taken(&self, name: &str, except: Option<UserId>) -> bool {
        let key = name_key(name);
        self.users()
            .any(|(id, u)| Some(id) != except && name_key(&u.name) == key)
    }

    /// Whether `name` may be issued to `except` (or a new user): it meets
    /// today's rules and nobody else holds it, else `NAME_TAKEN`.
    pub fn claim_name(&self, name: &str, except: Option<UserId>) -> Result<()> {
        validate_name(name).context("bad name")?;
        if self.name_taken(name, except) {
            return Err(Error::coded(
                ec::NAME_TAKEN,
                format!("name {name:?} is already taken"),
            ));
        }
        Ok(())
    }

    /// Adds a new user with its first certificate. The display name is the
    /// certificate's CN. Returns the new `user_id`.
    pub fn add_user(&mut self, cert: CertRecord, is_admin: bool) -> Result<UserId> {
        self.claim_name(&cert.cn, None)?;
        self.update_users(|users| {
            let id = users.next_user_id;
            users.next_user_id = id
                .checked_add(1)
                .ok_or_else(|| Error::msg("user_id space exhausted"))?;
            users.users.insert(
                id.to_string(),
                UserRecord {
                    name: cert.cn.clone(),
                    created: rfc3339(now()),
                    last_renew: None,
                    certs: vec![cert],
                    is_admin,
                    disabled: false,
                    server_muted: false,
                    server_deafened: false,
                },
            );
            Ok(id)
        })
    }

    /// Appends a newly issued certificate to an existing user and adopts its
    /// CN as the user's current name — together with [`Store::add_user`] the
    /// only writer of `UserRecord.name`. The older certificates stay valid
    /// until the new one first logs in, which retires them
    /// ([`Store::retire_predecessors`], `PROTOCOL.md` §2.2 Nr. 10).
    ///
    /// `supersedes` stamps the certificate the request was made with, and is
    /// passed only for catch-up renewals — that stamp is what bounds them.
    /// Returns whether the stored name actually changed, i.e. whether to
    /// broadcast `UserRenamed`.
    ///
    /// Cert append, name adoption and the supersede stamp are one write on
    /// purpose: two writes could crash in between and leave a user whose name
    /// does not match their newest certificate.
    pub fn add_cert(
        &mut self,
        id: UserId,
        cert: CertRecord,
        supersedes: Option<&str>,
        renewal: bool,
    ) -> Result<bool> {
        // The stored name carried into a new certificate is kept as it is:
        // it may predate today's rules, or collide under today's
        // `name_key` with a name issued since, and its owner must still be
        // able to renew.
        let keeps_name = self.user(id).is_some_and(|u| u.name == cert.cn);
        if !keeps_name {
            self.claim_name(&cert.cn, Some(id))?;
        }
        let now = now();
        let stamp = rfc3339(now);
        self.update_users(|users| {
            let record = user_entry(users, id)?;
            // A renewal queued behind a revoke or disable must not hand out a
            // fresh, unrevoked certificate and undo it.
            if record.disabled {
                return Err(Error::coded(ec::AUTH_DISABLED, "user is disabled"));
            }
            if record.is_revoked() {
                return Err(Error::coded(ec::AUTH_REVOKED, "user is revoked"));
            }
            let renamed = record.name != cert.cn;
            record.name = cert.cn.clone();
            if let Some(fp) = supersedes {
                if let Some(old) = record.certs.iter_mut().find(|c| c.fingerprint == fp) {
                    old.renewed = Some(stamp.clone());
                }
            }
            // Only spent registration certificates are dropped; see
            // `keep_cert`.
            record.certs.retain(|c| keep_cert(c, now));
            record.certs.push(cert);
            if renewal {
                record.last_renew = Some(stamp);
            }
            Ok(renamed)
        })
    }

    /// When `fingerprint` was last used to obtain a successor certificate.
    pub fn cert_renewed(&self, id: UserId, fingerprint: &str) -> Option<OffsetDateTime> {
        self.user(id)?
            .certs
            .iter()
            .find(|c| c.fingerprint == fingerprint)
            .and_then(|c| c.renewed.as_deref())
            .and_then(|s| parse_rfc3339(s).ok())
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
        self.update_users(|users| {
            let record = user_entry(users, id)?;
            let mut changed = false;
            for cert in &mut record.certs {
                if !cert.revoked {
                    cert.revoked = true;
                    changed = true;
                }
            }
            Ok(changed)
        })
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

    /// Whether some functional admin has confirmed their registration. While
    /// none has, whoever confirms a registration first becomes admin, which
    /// `icu-server invite` points out.
    pub fn has_confirmed_admin(&self) -> bool {
        self.users()
            .any(|(id, u)| !u.is_pending() && self.is_functional_admin(id))
    }

    /// Whether someone other than `except` is a functional admin who has
    /// confirmed their registration. A pending admin — an unused first bundle,
    /// or a redeem that was never confirmed — does not count: they may never
    /// arrive, and the first user who does must become admin (§6.3).
    fn has_enrolled_admin(&self, except: UserId) -> bool {
        self.users()
            .any(|(id, u)| id != except && !u.is_pending() && self.is_functional_admin(id))
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
        self.set_flag(
            id,
            is_admin,
            !is_admin,
            |u| u.is_admin,
            |u, v| u.is_admin = v,
        )
    }

    /// Blocks or restores login without touching certificates. Guarded
    /// against disabling the last remaining functional admin. Restoring
    /// (`disabled: false`) makes existing non-revoked certificates usable
    /// again immediately, no re-enrollment needed. Returns whether anything
    /// changed.
    pub fn set_disabled(&mut self, id: UserId, disabled: bool) -> Result<bool> {
        self.set_flag(
            id,
            disabled,
            disabled,
            |u| u.disabled,
            |u, v| u.disabled = v,
        )
    }

    /// Sets the admin-enforced mute and deafen. Not guarded: it takes no
    /// admin rights away. Returns whether anything changed.
    pub fn set_restriction(&mut self, id: UserId, muted: bool, deafened: bool) -> Result<bool> {
        let current = self
            .user(id)
            .ok_or_else(|| Error::coded(ec::NO_SUCH_USER, format!("unknown user_id {id}")))?;
        if (current.server_muted, current.server_deafened) == (muted, deafened) {
            return Ok(false);
        }
        self.update_users(|users| {
            let user = user_entry(users, id)?;
            user.server_muted = muted;
            user.server_deafened = deafened;
            Ok(true)
        })
    }

    /// The admin-enforced `(muted, deafened)` of a user; both false for an
    /// unknown one.
    pub fn restriction(&self, id: UserId) -> (bool, bool) {
        self.user(id)
            .map_or((false, false), |u| (u.server_muted, u.server_deafened))
    }

    /// Sets one user flag to `want`, writing only when it changes; `guarded`
    /// says whether this direction may not take the last admin away.
    fn set_flag(
        &mut self,
        id: UserId,
        want: bool,
        guarded: bool,
        get: fn(&UserRecord) -> bool,
        set: fn(&mut UserRecord, bool),
    ) -> Result<bool> {
        if guarded {
            self.guard_last_admin(id)?;
        }
        let current = self
            .user(id)
            .ok_or_else(|| Error::msg(format!("unknown user_id {id}")))?;
        if get(current) == want {
            return Ok(false);
        }
        self.update_users(|users| {
            set(user_entry(users, id)?, want);
            Ok(true)
        })
    }

    /// Records an invited user with their registration certificate
    /// (`PROTOCOL.md` §2.3). The certificate's CN is the placeholder name,
    /// which has to be free like any other. The first registration against an
    /// empty store is marked admin — provisionally: who ends up admin is
    /// decided when a registration is confirmed ([`Store::consume_registration`]).
    /// Returns the new `user_id` and whether it is marked admin.
    pub fn add_registration(&mut self, cert: CertRecord) -> Result<(UserId, bool)> {
        let is_admin = self.is_empty();
        let cert = CertRecord {
            registration: true,
            ..cert
        };
        let id = self.add_user(cert, is_admin)?;
        Ok((id, is_admin))
    }

    /// Records the user's first real certificate for a registration, in one
    /// write: the new certificate is added and its CN becomes the name. The
    /// registration certificate stays valid until the client confirms it has
    /// stored the new one ([`Store::consume_registration`]); until then a lost
    /// `RenewResponse` or a failed save can be repaired by redeeming the same
    /// bundle again. Such a repeat revokes the certificates earlier redeems
    /// produced — nobody can hold them, or the registration would have been
    /// consumed by their first login.
    ///
    /// Without a confirmed admin on the server, the redeemer is marked admin
    /// provisionally; [`Store::consume_registration`] makes that final.
    pub fn redeem_registration(
        &mut self,
        id: UserId,
        registration_fp: &str,
        cert: CertRecord,
    ) -> Result<()> {
        self.claim_name(&cert.cn, Some(id))?;
        let promote = !self.has_enrolled_admin(id);
        self.update_users(|users| {
            let record = user_entry(users, id)?;
            if record.disabled {
                return Err(Error::coded(ec::AUTH_DISABLED, "user is disabled"));
            }
            if !record
                .certs
                .iter()
                .any(|c| c.fingerprint == registration_fp && c.registration && !c.revoked)
            {
                return Err(Error::coded(
                    ec::AUTH_REVOKED,
                    "registration certificate already used",
                ));
            }
            // A pending user has had no renewal: every other certificate
            // came from a redeem of this bundle.
            let redeems = record.certs.iter().filter(|c| !c.registration).count();
            if redeems >= REDEEMS_PER_BUNDLE {
                return Err(Error::coded(
                    ec::RENEW_REJECTED,
                    format!(
                        "this invitation was redeemed {redeems} times without a receipt; \
                         ask for a new one"
                    ),
                ));
            }
            // Superseded, not only revoked: once expired they leave the store
            // and the CRL instead of piling up with every repeated redeem.
            for earlier in record.certs.iter_mut().filter(|c| !c.registration) {
                earlier.revoked = true;
                earlier.superseded = true;
            }
            record.name = cert.cn.clone();
            if promote {
                record.is_admin = true;
            }
            record.certs.push(CertRecord {
                registration: false,
                ..cert
            });
            Ok(())
        })
    }

    /// Revokes the user's registration certificates once the client has
    /// provably stored its own: a clean close after `RenewResponse`, or the
    /// first login with a real certificate. From then on the bundle is
    /// worthless. Returns whether anything changed.
    ///
    /// The first user to confirm while the server has no confirmed admin
    /// becomes admin, and every admin mark on a still pending user — the
    /// first bundle's, or a provisional one from an earlier redeem — is
    /// dropped in the same write, so exactly one admin comes out of it.
    pub fn consume_registration(&mut self, id: UserId) -> Result<bool> {
        let open = self
            .user(id)
            .is_some_and(|u| u.certs.iter().any(|c| c.registration && !c.revoked));
        if !open {
            return Ok(false);
        }
        let first_admin = !self.has_enrolled_admin(id);
        self.update_users(|users| {
            let record = user_entry(users, id)?;
            for cert in record.certs.iter_mut().filter(|c| c.registration) {
                cert.revoked = true;
            }
            if first_admin {
                record.is_admin = true;
                let key = id.to_string();
                for (other_key, other) in users.users.iter_mut() {
                    if *other_key != key && other.is_pending() {
                        other.is_admin = false;
                    }
                }
            }
            Ok(true)
        })
    }

    /// Revokes every certificate of the user issued before `fingerprint`,
    /// once that one logs in: its first login proves the client stored it,
    /// so whatever it replaced — on this device or on any other holding a
    /// copy — is invalid from then on, expired or not (`PROTOCOL.md` §2.2
    /// Nr. 10). Registration certificates are left to
    /// [`Store::consume_registration`]. Returns the fingerprints revoked.
    pub fn retire_predecessors(&mut self, id: UserId, fingerprint: &str) -> Result<Vec<String>> {
        let Some(user) = self.user(id) else {
            return Ok(Vec::new());
        };
        // `certs` is in issue order: `add_user`, `redeem_registration` and
        // `add_cert` only ever push.
        let Some(position) = user.certs.iter().position(|c| c.fingerprint == fingerprint) else {
            return Ok(Vec::new());
        };
        let retire = |c: &CertRecord| !c.revoked && !c.registration;
        if !user.certs[..position].iter().any(retire) {
            return Ok(Vec::new());
        }
        self.update_users(|users| {
            let record = user_entry(users, id)?;
            let mut retired = Vec::new();
            for cert in record.certs[..position].iter_mut().filter(|c| retire(c)) {
                cert.revoked = true;
                cert.superseded = true;
                retired.push(cert.fingerprint.clone());
            }
            Ok(retired)
        })
    }

    /// Serials for the CRL: every revoked certificate that has a recorded
    /// serial — client leaves for good, consumed registration certificates,
    /// superseded client leaves and replaced server leaves until they expire.
    pub fn crl_serials(&self) -> Vec<String> {
        let now = now();
        let clients = self
            .users
            .users
            .values()
            .flat_map(|u| &u.certs)
            // Expired ones too: an expired client certificate still gets
            // through the handshake for a renewal, so its revocation has to
            // outlive it. Spent registrations and superseded leaves are the
            // exception: once expired, their records go (`keep_cert`), and an
            // unknown fingerprint is refused anyway.
            .filter(|c| c.revoked && !((c.registration || c.superseded) && c.expired(now)))
            .filter_map(|c| c.serial.clone());
        let servers = self
            .users
            .revoked_server
            .iter()
            .filter(|s| s.live(now))
            .map(|s| s.serial.clone());
        clients.chain(servers).collect()
    }

    /// Serials of the replaced server leaves that have not expired yet: the
    /// CRL clients get (`CrlUpdate`), since the server leaf is all they check.
    pub fn revoked_server_serials(&self) -> Vec<String> {
        let now = now();
        self.users
            .revoked_server
            .iter()
            .filter(|s| s.live(now))
            .map(|s| s.serial.clone())
            .collect()
    }

    /// Reserves the next CRL number and drops expired server entries in the
    /// same write.
    pub fn next_crl_number(&mut self) -> Result<u64> {
        let now = now();
        self.update_users(|users| {
            // Never below the clock: a state restored from a backup would
            // otherwise count up from an older number, and clients, which
            // only take a higher one, would ignore every CRL until it caught
            // up (`deploy/README.md`, restore).
            users.crl_number = (users.crl_number + 1).max(now.unix_timestamp().max(0) as u64);
            users.revoked_server.retain(|s| s.live(now));
            Ok(users.crl_number)
        })
    }

    /// Puts replaced server leaves on the CRL (`rotate-server-cert`), each as
    /// `(serial, expires)`, in one write.
    pub fn revoke_server_certs(&mut self, leaves: &[(String, String)]) -> Result<()> {
        self.update_users(|users| {
            for (serial, expires) in leaves {
                if !users.revoked_server.iter().any(|s| &s.serial == serial) {
                    users.revoked_server.push(RevokedServerCert {
                        serial: serial.clone(),
                        expires: expires.clone(),
                    });
                }
            }
            Ok(())
        })
    }

    /// Drops users who were invited and never confirmed a registration before
    /// their registration certificate expired, which frees the name. A redeem
    /// that was never confirmed counts as unused: nobody may hold its
    /// certificate, and a user who does but never logged in within the
    /// bundle's lifetime needs a new invite. Called at startup (§6.1.4).
    /// `user_id`s are still never reused.
    pub fn purge_expired_registrations(&mut self) -> Result<usize> {
        let now = now();
        let stale = |u: &UserRecord| {
            u.is_pending()
                && u.certs
                    .iter()
                    .filter(|c| c.registration)
                    .all(|c| c.expired(now))
        };
        let removed = self.users.users.values().filter(|u| stale(u)).count();
        if removed > 0 {
            self.update_users(|users| {
                users.users.retain(|_, u| !stale(u));
                Ok(())
            })?;
        }
        Ok(removed)
    }
}

/// Whether a certificate record is still worth keeping. An expired client
/// certificate still authenticates a renew-only session, so its holder can
/// renew after missing the window (`PROTOCOL.md` §2.3); the record is what
/// admits it, with no time limit. Revoked ones stay too: they are what makes a
/// revoke visible, and what `is_revoked` counts. Only a consumed registration
/// certificate goes once expired, its revocation only ever meant "used". At
/// most one record per user and day accrues (renewals are rate-limited). A
/// superseded certificate goes once expired as well: it was revoked because
/// its successor logged in, and the successor's record admits that user.
fn keep_cert(c: &CertRecord, now: OffsetDateTime) -> bool {
    if (c.registration || c.superseded) && c.revoked {
        return !c.expired(now);
    }
    true
}

/// Looks a user up by id for mutation. The `BTreeMap` key is the decimal
/// `user_id`; this is the only place that encoding is spelled out.
fn user_entry(users: &mut UsersFile, id: UserId) -> Result<&mut UserRecord> {
    users
        .users
        .get_mut(&id.to_string())
        .ok_or_else(|| Error::msg(format!("unknown user_id {id}")))
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
///
/// The fsyncs can take tens of milliseconds on network storage, and every
/// caller is async code on a runtime worker. `block_in_place` hands that
/// worker's other tasks — quinn's endpoint driver among them — to another
/// worker meanwhile, so audio keeps flowing.
fn write_mode(path: &Path, contents: &str, mode: u32) -> Result<()> {
    blocking(|| write_mode_now(path, contents, mode))
}

/// Runs `f` with `block_in_place` on a multi-thread runtime, directly
/// everywhere else (`current_thread` runtimes such as `#[tokio::test]`, where
/// `block_in_place` would panic, and plain threads).
fn blocking<R>(f: impl FnOnce() -> R) -> R {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(f),
        _ => f(),
    }
}

fn write_mode_now(path: &Path, contents: &str, mode: u32) -> Result<()> {
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

    fn named_cert(fp: &str, cn: &str) -> CertRecord {
        let t = now();
        CertRecord {
            fingerprint: fp.to_string(),
            cn: cn.to_string(),
            issued: rfc3339(t),
            expires: rfc3339(t + Duration::from_secs(365 * 86400)),
            revoked: false,
            renewed: None,
            registration: false,
            superseded: false,
            serial: None,
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    /// The CRL number never falls below the clock, so a state restored from
    /// a backup still issues numbers above what clients have stored.
    #[test]
    fn crl_numbers_follow_the_clock_and_stay_monotonic() {
        let (_dir, mut s) = store();
        let now = super::now().unix_timestamp() as u64;
        let a = s.next_crl_number().unwrap();
        let b = s.next_crl_number().unwrap();
        assert!(a >= now, "{a} < {now}");
        assert!(b > a);
    }

    /// §10: `user_id` assignment and stability across renewal.
    #[test]
    fn user_ids_are_monotonic_and_stable_across_renewal() {
        let (_dir, mut s) = store();
        let alice = s.add_user(named_cert("aa", "alice"), false).unwrap();
        let bob = s.add_user(named_cert("bb", "bob"), false).unwrap();
        assert_eq!((alice, bob), (1, 2));

        s.add_cert(
            alice,
            named_cert("cc", &s.user(alice).unwrap().name.clone()),
            None,
            true,
        )
        .unwrap();
        assert_eq!(s.user(alice).unwrap().certs.len(), 2);
        assert_eq!(s.by_fingerprint("cc").unwrap().0, alice, "same user_id");
        assert_eq!(s.by_fingerprint("aa").unwrap().0, alice, "old cert stays");
        assert!(s.last_renew(alice).is_some());

        // Ids are never reused: removing is not even offered, and next_user_id
        // only grows.
        let carol = s.add_user(named_cert("dd", "carol"), false).unwrap();
        assert_eq!(carol, 3);
    }

    /// §10: revoke persists across **all** certificates of a user.
    #[test]
    fn revoke_marks_every_certificate_and_persists() {
        let (dir, mut s) = store();
        let alice = s.add_user(named_cert("aa", "alice"), false).unwrap();
        s.add_cert(
            alice,
            named_cert("bb", &s.user(alice).unwrap().name.clone()),
            None,
            true,
        )
        .unwrap();
        assert!(s.revoke(alice).unwrap());
        assert!(!s.revoke(alice).unwrap(), "second revoke changes nothing");
        drop(s);

        let s = Store::open(dir.path()).unwrap();
        let user = s.user(alice).unwrap();
        assert_eq!(user.certs.len(), 2);
        assert!(user.certs.iter().all(|c| c.revoked));
    }

    /// A renewal that was queued behind a revoke or disable must not hand
    /// the user a fresh, unrevoked certificate.
    #[test]
    fn renewal_is_rejected_after_revoke_or_disable() {
        let (_dir, mut s) = store();
        let alice = s.add_user(named_cert("aa", "alice"), false).unwrap();
        s.revoke(alice).unwrap();
        let e = s
            .add_cert(alice, named_cert("bb", "alice"), None, true)
            .unwrap_err();
        assert_eq!(e.code(), Some(ec::AUTH_REVOKED));
        assert!(s.user(alice).unwrap().is_revoked());

        let bob = s.add_user(named_cert("cc", "bob"), false).unwrap();
        s.set_disabled(bob, true).unwrap();
        let e = s
            .add_cert(bob, named_cert("dd", "bob"), None, true)
            .unwrap_err();
        assert_eq!(e.code(), Some(ec::AUTH_DISABLED));
        assert_eq!(s.user(bob).unwrap().certs.len(), 1);
    }

    /// §10: promote/demote updates `is_admin` and persists.
    #[test]
    fn promoting_and_demoting_updates_is_admin() {
        let (dir, mut s) = store();
        let alice = s.add_user(named_cert("aa", "alice"), true).unwrap();
        let bob = s.add_user(named_cert("bb", "bob"), false).unwrap();
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
        let alice = s.add_user(named_cert("aa", "alice"), true).unwrap();
        let bob = s.add_user(named_cert("bb", "bob"), false).unwrap();
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
        let alice = s.add_user(named_cert("aa", "alice"), true).unwrap();
        let err = s.set_admin(alice, false).unwrap_err();
        assert_eq!(err.code(), Some(ec::LAST_ADMIN));
        assert!(s.is_admin(alice), "unchanged");
    }

    /// §10: the last-admin guard — disable.
    #[test]
    fn disabling_the_last_functional_admin_is_rejected() {
        let (_dir, mut s) = store();
        let alice = s.add_user(named_cert("aa", "alice"), true).unwrap();
        let err = s.set_disabled(alice, true).unwrap_err();
        assert_eq!(err.code(), Some(ec::LAST_ADMIN));
        assert!(!s.user(alice).unwrap().disabled, "unchanged");
    }

    /// §10: the last-admin guard — revoke.
    #[test]
    fn revoking_the_last_functional_admin_is_rejected() {
        let (_dir, mut s) = store();
        let alice = s.add_user(named_cert("aa", "alice"), true).unwrap();
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
        let alice = s.add_user(named_cert("aa", "alice"), true).unwrap();
        let bob = s.add_user(named_cert("bb", "bob"), true).unwrap();

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
        s.add_user(named_cert("aa", "alice"), true).unwrap();
        let bob = s.add_user(named_cert("bb", "bob"), false).unwrap();
        assert!(s.revoke(bob).unwrap());
        assert!(s.set_disabled(bob, true).unwrap());
    }

    #[test]
    fn add_user_refuses_a_case_insensitive_collision() {
        let (_dir, mut s) = store();
        s.add_user(named_cert("aa", "alice"), false).unwrap();
        let err = s.add_user(named_cert("bb", "ALICE"), false).unwrap_err();
        assert_eq!(err.code(), Some(ec::NAME_TAKEN));
        assert_eq!(s.users().count(), 1, "nothing was written");
    }

    /// A renewal carrying a new CN is how a rename happens, and the stored
    /// name follows the newest certificate.
    #[test]
    fn resolve_never_picks_a_look_alike() {
        let (_dir, mut s) = store();
        let latin = s.add_user(named_cert("aa", "Chris"), true).unwrap();
        // Stored before the skeleton key existed: both names coexist.
        s.users.users.insert(
            "9".into(),
            UserRecord {
                name: "\u{0421}\u{04BB}ris".into(),
                created: rfc3339(now()),
                last_renew: None,
                certs: vec![named_cert("bb", "\u{0421}\u{04BB}ris")],
                is_admin: false,
                disabled: false,
                server_muted: false,
                server_deafened: false,
            },
        );
        assert_eq!(s.resolve("Chris"), Some(latin));
        assert_eq!(s.resolve("chris"), Some(latin));
        assert_eq!(s.resolve("\u{0421}\u{04BB}ris"), Some(9));
        // A renewal keeping a name that now collides still goes through.
        let mut renewed = named_cert("cc", "\u{0421}\u{04BB}ris");
        renewed.expires = rfc3339(now() + Duration::from_secs(86400));
        assert!(
            !s.add_cert(9, renewed, None, true).unwrap(),
            "name unchanged"
        );
    }

    #[test]
    fn add_cert_adopts_the_cn_and_reports_the_change() {
        let (_dir, mut s) = store();
        let alice = s.add_user(named_cert("aa", "alice"), false).unwrap();

        // Plain renewal: same name, no rename reported, the superseded
        // certificate is stamped.
        assert!(!s
            .add_cert(alice, named_cert("bb", "alice"), Some("aa"), true)
            .unwrap());
        assert_eq!(s.user(alice).unwrap().name, "alice");
        assert!(s.cert_renewed(alice, "aa").is_some());
        assert!(s.cert_renewed(alice, "bb").is_none());

        // Rename, including a case-only one: the exact string decides.
        assert!(s
            .add_cert(alice, named_cert("cc", "Alice"), Some("bb"), true)
            .unwrap());
        assert_eq!(s.user(alice).unwrap().name, "Alice");
        assert!(s
            .add_cert(alice, named_cert("dd", "carol"), Some("cc"), true)
            .unwrap());
        assert_eq!(s.user(alice).unwrap().name, "carol");
        assert_eq!(s.user(alice).unwrap().certs.len(), 4, "old certs stay");
    }

    #[test]
    fn a_login_retires_every_earlier_certificate_and_they_leave_once_expired() {
        let (_dir, mut s) = store();
        let alice = s.add_user(named_cert("aa", "alice"), false).unwrap();
        s.add_cert(alice, named_cert("bb", "alice"), None, true)
            .unwrap();
        s.add_cert(alice, named_cert("cc", "alice"), None, true)
            .unwrap();

        // The middle one logging in retires only what came before it.
        assert_eq!(s.retire_predecessors(alice, "bb").unwrap(), ["aa"]);
        assert!(s.retire_predecessors(alice, "bb").unwrap().is_empty());
        assert_eq!(s.retire_predecessors(alice, "cc").unwrap(), ["bb"]);
        let user = s.user(alice).unwrap();
        assert!(user.certs[..2].iter().all(|c| c.revoked && c.superseded));
        assert!(!user.certs[2].revoked);
        assert!(!user.is_revoked());

        // An expired superseded record leaves the store with the next write.
        let past = rfc3339(now() - Duration::from_secs(60));
        s.update_users(|users| {
            user_entry(users, alice)?.certs[0].expires = past.clone();
            Ok(())
        })
        .unwrap();
        s.add_cert(alice, named_cert("dd", "alice"), None, true)
            .unwrap();
        let user = s.user(alice).unwrap();
        let fps: Vec<_> = user.certs.iter().map(|c| c.fingerprint.as_str()).collect();
        assert_eq!(fps, ["bb", "cc", "dd"]);
    }

    #[test]
    fn add_cert_refuses_another_users_name_and_writes_nothing() {
        let (_dir, mut s) = store();
        let alice = s.add_user(named_cert("aa", "alice"), false).unwrap();
        s.add_user(named_cert("bb", "bob"), false).unwrap();
        let err = s
            .add_cert(alice, named_cert("cc", "BOB"), Some("aa"), true)
            .unwrap_err();
        assert_eq!(err.code(), Some(ec::NAME_TAKEN));
        let user = s.user(alice).unwrap();
        assert_eq!(user.name, "alice");
        assert_eq!(user.certs.len(), 1);
        assert!(user.last_renew.is_none());
    }

    /// Records written before the name moved into the certificate load with an
    /// empty `cn`, which is what makes the migration a no-op.
    #[test]
    fn legacy_records_without_cn_load_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(USERS_FILE),
            "schema = 1\nnext_user_id = 2\n\n[user.1]\nname = \"alice\"\ncreated = \"2026-01-01T00:00:00Z\"\nis_admin = true\ndisabled = false\n\n[[user.1.cert]]\nfingerprint = \"aa\"\nissued = \"2026-01-01T00:00:00Z\"\nexpires = \"2027-01-01T00:00:00Z\"\nrevoked = false\n",
        )
        .unwrap();
        let s = Store::open(dir.path()).unwrap();
        let user = s.user(1).unwrap();
        assert_eq!(user.name, "alice");
        assert_eq!(user.certs[0].cn, "", "unknown, not an empty name");
        assert!(user.certs[0].renewed.is_none());
    }

    #[test]
    fn resolve_by_id_name_and_fingerprint() {
        let (_dir, mut s) = store();
        let alice = s.add_user(named_cert("9f86d0", "alice"), false).unwrap();
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
            let id = s.add_user(named_cert("aa", "alice"), false).unwrap();
            s.add_registration(named_cert("bb", "bob")).unwrap();
            assert_eq!(id, 1);
        }
        let text = std::fs::read_to_string(dir.path().join(USERS_FILE)).unwrap();
        assert!(text.contains("schema = 1"), "{text}");
        assert!(text.contains("[user.1]"), "{text}");
        assert!(text.contains("[[user.1.cert]]"), "{text}");

        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.user(1).unwrap().name, "alice");
        assert!(s.user(2).unwrap().is_pending());
        assert!(s.user(2).unwrap().certs[0].registration);
        assert_eq!(s.users.next_user_id, 3);
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

    /// The first registration against an empty store is for an admin, every
    /// later one is not; the placeholder name must be free.
    #[test]
    fn registrations_create_pending_users() {
        let (_dir, mut s) = store();
        let (first, admin) = s.add_registration(named_cert("aa", "neu-0001")).unwrap();
        assert!(admin);
        assert!(s.is_admin(first));
        assert!(s.user(first).unwrap().is_pending());
        let (second, admin) = s.add_registration(named_cert("bb", "neu-0002")).unwrap();
        assert!(!admin);
        assert!(!s.is_admin(second));
        let err = s
            .add_registration(named_cert("cc", "NEU-0001"))
            .unwrap_err();
        assert_eq!(err.code(), Some(ec::NAME_TAKEN));
    }

    /// Redeeming adopts the new name but keeps the bundle usable until the
    /// client confirms; a repeat redeem replaces the unconfirmed certificate,
    /// and once consumed the bundle is dead.
    #[test]
    fn a_registration_is_redeemed_until_consumed() {
        let (_dir, mut s) = store();
        let (id, _) = s.add_registration(named_cert("reg", "neu-0001")).unwrap();
        s.add_user(named_cert("bb", "bob"), false).unwrap();
        let err = s
            .redeem_registration(id, "reg", named_cert("new", "Bob"))
            .unwrap_err();
        assert_eq!(err.code(), Some(ec::NAME_TAKEN));
        assert!(!s.user(id).unwrap().certs[0].revoked, "nothing was written");

        s.redeem_registration(id, "reg", named_cert("new", "alice"))
            .unwrap();
        let user = s.user(id).unwrap();
        assert_eq!(user.name, "alice");
        assert!(user.is_pending(), "unconfirmed");
        assert!(!user.is_revoked());
        let (_, _, reg) = s.by_fingerprint("reg").unwrap();
        assert!(!reg.revoked, "unconfirmed: the bundle still works");

        // The answer was lost: the same bundle redeems again, and the first
        // certificate, which nobody holds, is revoked — as superseded, so it
        // leaves store and CRL once expired.
        s.redeem_registration(id, "reg", named_cert("again", "alice2"))
            .unwrap();
        assert_eq!(s.user(id).unwrap().name, "alice2");
        let first = s.by_fingerprint("new").unwrap().2;
        assert!(first.revoked && first.superseded);
        assert!(!s.by_fingerprint("again").unwrap().2.revoked);

        assert!(s.consume_registration(id).unwrap());
        assert!(!s.consume_registration(id).unwrap(), "idempotent");
        assert!(!s.user(id).unwrap().is_pending());
        let (_, _, reg) = s.by_fingerprint("reg").unwrap();
        assert!(reg.revoked && reg.registration);
        assert!(!s.by_fingerprint("again").unwrap().2.revoked);
        let err = s
            .redeem_registration(id, "reg", named_cert("third", "alice3"))
            .unwrap_err();
        assert_eq!(err.code(), Some(ec::AUTH_REVOKED));
    }

    /// Unconfirmed redeems of one bundle are capped: each leaves a
    /// certificate on the CRL.
    #[test]
    fn redeems_of_one_bundle_are_capped() {
        let (_dir, mut s) = store();
        let (id, _) = s.add_registration(named_cert("reg", "neu-0001")).unwrap();
        for i in 0..REDEEMS_PER_BUNDLE {
            s.redeem_registration(
                id,
                "reg",
                named_cert(&format!("r{i}"), &format!("alice{i}")),
            )
            .unwrap();
        }
        let err = s
            .redeem_registration(id, "reg", named_cert("one-more", "alice"))
            .unwrap_err();
        assert_eq!(err.code(), Some(ec::RENEW_REJECTED));
        assert!(
            s.by_fingerprint("one-more").is_none(),
            "nothing was written"
        );
        // Earlier redeems are superseded: they leave the CRL once expired.
        let user = s.user(id).unwrap();
        assert_eq!(
            user.certs.iter().filter(|c| c.superseded).count(),
            REDEEMS_PER_BUNDLE - 1
        );
    }

    /// Clients get a CRL of the replaced server leaves only: revoked client
    /// certificates would grow it past a control frame.
    #[test]
    fn the_client_crl_lists_only_server_leaves() {
        let (_dir, mut s) = store();
        let mut cert = named_cert("aa", "alice");
        cert.serial = Some("0a".into());
        let alice = s.add_user(cert, false).unwrap();
        s.add_user(named_cert("bb", "bob"), true).unwrap();
        s.revoke(alice).unwrap();
        s.revoke_server_certs(&[("0b".into(), rfc3339(now() + Duration::from_secs(60)))])
            .unwrap();
        s.revoke_server_certs(&[("0c".into(), rfc3339(now() - Duration::from_secs(60)))])
            .unwrap();
        assert_eq!(s.crl_serials(), vec!["0a".to_string(), "0b".to_string()]);
        assert_eq!(s.revoked_server_serials(), vec!["0b".to_string()]);
    }

    /// An unused first bundle that expired must not cost the next one its
    /// admin rights.
    #[test]
    fn the_admin_slot_frees_up_when_the_first_bundle_expires() {
        let (_dir, mut s) = store();
        let (first, admin) = s.add_registration(named_cert("aa", "neu-0001")).unwrap();
        assert!(admin);
        user_entry(&mut s.users, first).unwrap().certs[0].expires =
            rfc3339(now() - Duration::from_secs(10));
        s.purge_expired_registrations().unwrap();
        let (_, admin) = s.add_registration(named_cert("bb", "neu-0002")).unwrap();
        assert!(admin);
    }

    /// Registers `id` completely: redeem, then the client's confirmation.
    fn confirm(s: &mut Store, id: UserId, reg_fp: &str, fp: &str, name: &str) {
        s.redeem_registration(id, reg_fp, named_cert(fp, name))
            .unwrap();
        s.consume_registration(id).unwrap();
    }

    /// The first user to confirm is admin, not the holder of the first bundle.
    #[test]
    fn the_first_to_confirm_is_admin_not_the_first_bundle() {
        let (_dir, mut s) = store();
        let (a, admin) = s.add_registration(named_cert("ra", "neu-0001")).unwrap();
        assert!(admin, "marked at issue time");
        let (b, _) = s.add_registration(named_cert("rb", "neu-0002")).unwrap();
        confirm(&mut s, b, "rb", "b1", "bob");
        assert!(s.is_admin(b));
        assert!(!s.is_admin(a), "the unused first bundle lost its mark");
        confirm(&mut s, a, "ra", "a1", "alice");
        assert!(!s.is_admin(a));
        assert!(s.is_admin(b));
    }

    /// A redeem whose answer was lost holds only a provisional mark: someone
    /// else confirming first takes the admin slot, and the lost one's later
    /// expiry leaves the server with that admin.
    #[test]
    fn a_lost_response_does_not_block_the_admin_slot() {
        let (_dir, mut s) = store();
        let (a, _) = s.add_registration(named_cert("ra", "neu-0001")).unwrap();
        let (b, _) = s.add_registration(named_cert("rb", "neu-0002")).unwrap();
        s.redeem_registration(b, "rb", named_cert("b1", "bob"))
            .unwrap();
        assert!(s.is_admin(b), "provisional");
        confirm(&mut s, a, "ra", "a1", "alice");
        assert!(s.is_admin(a));
        assert!(!s.is_admin(b), "the provisional mark is gone");

        user_entry(&mut s.users, b).unwrap().certs[0].expires =
            rfc3339(now() - Duration::from_secs(10));
        assert_eq!(s.purge_expired_registrations().unwrap(), 1);
        assert!(s.is_admin(a));
        assert_eq!(s.functional_admin_count(), 1);
    }

    /// Once a confirmed admin exists, later registrations stay plain users.
    #[test]
    fn a_later_confirm_does_not_steal_admin() {
        let (_dir, mut s) = store();
        let (a, _) = s.add_registration(named_cert("ra", "neu-0001")).unwrap();
        confirm(&mut s, a, "ra", "a1", "alice");
        let (b, admin) = s.add_registration(named_cert("rb", "neu-0002")).unwrap();
        assert!(!admin);
        s.redeem_registration(b, "rb", named_cert("b1", "bob"))
            .unwrap();
        assert!(!s.is_admin(b), "no provisional mark either");
        s.consume_registration(b).unwrap();
        assert!(s.is_admin(a));
        assert!(!s.is_admin(b));
    }

    #[test]
    fn expired_unused_registrations_are_purged() {
        let (_dir, mut s) = store();
        let (stale, _) = s.add_registration(named_cert("aa", "neu-0001")).unwrap();
        let (fresh, _) = s.add_registration(named_cert("bb", "neu-0002")).unwrap();
        let (used, _) = s.add_registration(named_cert("cc", "neu-0003")).unwrap();
        s.redeem_registration(used, "cc", named_cert("dd", "carol"))
            .unwrap();
        s.consume_registration(used).unwrap();
        // Redeemed, but the answer never arrived and nobody tried again.
        let (lost, _) = s.add_registration(named_cert("ee", "neu-0004")).unwrap();
        s.redeem_registration(lost, "ee", named_cert("ff", "dave"))
            .unwrap();
        let past = rfc3339(now() - Duration::from_secs(10));
        for id in [stale, used, lost] {
            user_entry(&mut s.users, id).unwrap().certs[0].expires = past.clone();
        }

        assert_eq!(s.purge_expired_registrations().unwrap(), 2);
        assert!(s.user(stale).is_none());
        assert!(s.user(fresh).is_some());
        assert!(s.user(used).is_some(), "a registered user stays");
        assert!(s.user(lost).is_none(), "an unconfirmed redeem is no ghost");
        assert!(!s.name_taken("dave", None), "its name is free again");
        assert!(
            !s.name_taken("neu-0001", None),
            "the placeholder is free again"
        );
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
    fn timestamps_roundtrip_as_rfc3339_utc() {
        let t = now();
        let s = rfc3339(t);
        assert!(s.ends_with('Z'), "{s}");
        let back = parse_rfc3339(&s).unwrap();
        assert!((back - t).abs() < Duration::from_secs(1));
        assert!(parse_rfc3339("not a date").is_err());
    }
}
