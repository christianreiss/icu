//! Same-volume file transaction. The old executable is backed up and flushed
//! before its atomic replacement. Recovery addresses only fixed package files.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(super) const FILES: &[&str] = &[
    "icu.exe",
    "LICENSE",
    "THIRD-PARTY-LICENSES.txt",
    "LIESMICH.txt",
];
const JOURNAL: &str = ".icu-update.json";
#[derive(Serialize, Deserialize)]
struct Entry {
    old: Option<String>,
    new: String,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    schema: u32,
    stage: String,
    files: BTreeMap<String, Entry>,
}
pub(super) struct Transaction {
    root: PathBuf,
    journal: Journal,
}

pub(super) fn digest(path: &Path) -> Result<String, String> {
    reject_link(path)?;
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = file.read(&mut b).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
pub(super) fn reject_link(path: &Path) -> Result<(), String> {
    let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err("Update-Pfad ist ein Reparse Point.".into());
        }
    }
    if m.file_type().is_symlink() {
        return Err("Update-Pfad ist eine Verknüpfung.".into());
    }
    Ok(())
}
fn copy_flushed(from: &Path, to: &Path) -> Result<(), String> {
    reject_link(from)?;
    let mut input = fs::File::open(from).map_err(|e| e.to_string())?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| e.to_string())?;
    std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())
}
fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
impl Transaction {
    pub(super) fn prepare(root: &Path, payload: &Path) -> Result<Self, String> {
        reject_link(root)?;
        if root.join(JOURNAL).exists() {
            return Err("Ein vorheriges Update muss zunächst wiederhergestellt werden.".into());
        }
        let nonce: String = rand::random::<[u8; 16]>()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let stage_name = format!(".icu-update-{nonce}");
        let stage = root.join(&stage_name);
        fs::create_dir(&stage)
            .map_err(|e| format!("Im Programmordner kann kein Update vorbereitet werden: {e}"))?;
        let result = (|| {
            fs::create_dir(stage.join("new")).map_err(|e| e.to_string())?;
            fs::create_dir(stage.join("old")).map_err(|e| e.to_string())?;
            let mut files = BTreeMap::new();
            for name in FILES {
                let target = root.join(name);
                let new = stage.join("new").join(name);
                copy_flushed(&payload.join(name), &new)?;
                let old = if target.exists() {
                    let backup = stage.join("old").join(name);
                    copy_flushed(&target, &backup)?;
                    Some(digest(&backup)?)
                } else {
                    None
                };
                files.insert(
                    (*name).into(),
                    Entry {
                        old,
                        new: digest(&new)?,
                    },
                );
            }
            if files["icu.exe"].old.is_none() {
                return Err("Die bisherige ICU-Installation fehlt.".into());
            }
            let transaction = Self {
                root: root.to_path_buf(),
                journal: Journal {
                    schema: 1,
                    stage: stage_name,
                    files,
                },
            };
            let data = serde_json::to_vec(&transaction.journal).map_err(|e| e.to_string())?;
            let mut journal = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(stage.join("journal.json"))
                .map_err(|e| e.to_string())?;
            journal
                .write_all(&data)
                .and_then(|_| journal.sync_all())
                .map_err(|e| e.to_string())?;
            drop(journal);
            replace(&root.join(JOURNAL), &stage.join("journal.json"))?;
            Ok(transaction)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&stage);
        }
        result
    }
    pub(super) fn commit(&self) -> Result<(), String> {
        // Verify the entire before-state before the first replacement.
        for (name, entry) in &self.journal.files {
            let path = self.root.join(name);
            let actual = if path.exists() {
                Some(digest(&path)?)
            } else {
                None
            };
            if actual != entry.old {
                return Err(format!("{name} wurde während der Vorbereitung verändert."));
            }
        }
        // The executable is one atomic exchange. A later metadata failure
        // can restore it from the durable backup without deleting user files.
        for name in FILES {
            let source = self.root.join(&self.journal.stage).join("new").join(name);
            if digest(&source)? != self.journal.files[*name].new {
                return Err("Vorbereitete Update-Datei wurde verändert.".into());
            }
            replace(&self.root.join(name), &source)?;
        }
        Ok(())
    }
    pub(super) fn finish(self) -> Result<(), String> {
        for (name, entry) in &self.journal.files {
            if digest(&self.root.join(name))? != entry.new {
                return Err("Die installierten Dateien konnten nicht bestätigt werden.".into());
            }
        }
        self.mark_healthy()?;
        // The committed marker prevents a later recovery from undoing an
        // already healthy update if an antivirus temporarily blocks cleanup.
        if let Err(error) = self.cleanup() {
            tracing::warn!(%error,"healthy update retains its backup for later cleanup");
        }
        Ok(())
    }
    fn mark_healthy(&self) -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.root.join(&self.journal.stage).join("healthy"))
            .map_err(|e| e.to_string())?;
        file.write_all(b"healthy\n")
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())
    }
    fn cleanup(self) -> Result<(), String> {
        // Removing the journal commits the outcome. A leftover backup folder
        // after a crash is harmless and is never mistaken for a pending update.
        fs::remove_file(self.root.join(JOURNAL)).map_err(|e| e.to_string())?;
        if let Err(error) = fs::remove_dir_all(self.root.join(self.journal.stage)) {
            tracing::warn!(%error,"unused update backup could not be removed");
        }
        Ok(())
    }
}
pub(super) fn pending(root: &Path) -> bool {
    root.join(JOURNAL).exists()
}
pub(super) fn recover(root: &Path) -> Result<bool, String> {
    if !pending(root) {
        return Ok(false);
    }
    reject_link(root)?;
    let path = root.join(JOURNAL);
    reject_link(&path)?;
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .map_err(|e| e.to_string())?
        .take(16385)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 16384 {
        return Err("Ungültiges Wiederherstellungsprotokoll.".into());
    }
    let journal: Journal = serde_json::from_slice(&bytes)
        .map_err(|_| "Beschädigtes Wiederherstellungsprotokoll; Sicherungen wurden beibehalten.")?;
    let suffix = journal.stage.strip_prefix(".icu-update-").unwrap_or("");
    if journal.schema != 1
        || suffix.len() != 32
        || !suffix.bytes().all(|b| b.is_ascii_hexdigit())
        || journal.files.len() != FILES.len()
        || FILES.iter().any(|f| !journal.files.contains_key(*f))
        || journal
            .files
            .values()
            .any(|e| !hash(&e.new) || e.old.as_ref().is_some_and(|h| !hash(h)))
    {
        return Err("Unsichere Pfade im Wiederherstellungsprotokoll wurden abgewiesen.".into());
    }
    let stage = root.join(&journal.stage);
    reject_link(&stage)?;
    reject_link(&stage.join("old"))?;
    match fs::read(stage.join("healthy")) {
        Ok(marker) if marker == b"healthy\n" => {
            for (name, entry) in &journal.files {
                if digest(&root.join(name))? != entry.new {
                    return Err("Dateien eines bereits bestätigten Updates wurden verändert; Sicherungen bleiben erhalten.".into());
                }
            }
            Transaction {
                root: root.to_owned(),
                journal,
            }
            .cleanup()?;
            return Ok(true);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err("Ungültiger Abschlussvermerk; Sicherungen bleiben erhalten.".into()),
    }
    for name in FILES {
        let entry = &journal.files[*name];
        let path = root.join(name);
        let current = if path.exists() {
            Some(digest(&path)?)
        } else {
            None
        };
        if current == entry.old {
            continue;
        }
        if current.is_some() && current.as_ref() != Some(&entry.new) {
            return Err(format!(
                "{name} wurde nach dem Update verändert. Die Sicherung bleibt erhalten."
            ));
        }
        if let Some(old) = &entry.old {
            let backup = stage.join("old").join(name);
            if digest(&backup)? != *old {
                return Err(format!(
                    "Sicherung von {name} ist beschädigt; keine Datei wurde dafür überschrieben."
                ));
            }
            replace(&path, &backup)?;
        } else if current.is_some() {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
    }
    Transaction {
        root: root.to_path_buf(),
        journal,
    }
    .cleanup()?;
    Ok(true)
}

pub(super) fn replace(target: &Path, source: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            core::PCWSTR,
            Win32::Storage::FileSystem::{
                MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH, REPLACE_FILE_FLAGS,
            },
        };
        let wide = |p: &Path| {
            p.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let to = wide(target);
        let from = wide(source);
        // SAFETY: the nul-terminated names live through the synchronous call;
        // source and destination are files in the same installation volume.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            let result = unsafe {
                if target.exists() {
                    ReplaceFileW(
                        PCWSTR(to.as_ptr()),
                        PCWSTR(from.as_ptr()),
                        None,
                        REPLACE_FILE_FLAGS(0),
                        None,
                        None,
                    )
                } else {
                    MoveFileExW(
                        PCWSTR(from.as_ptr()),
                        PCWSTR(to.as_ptr()),
                        MOVEFILE_WRITE_THROUGH,
                    )
                }
            };
            match &result {
                Err(error)
                    if [5, 32, 33, 1175]
                        .iter()
                        .any(|code| error.code() == windows::core::HRESULT::from_win32(*code))
                        && std::time::Instant::now() < deadline =>
                {
                    // The old GUI can exit just before its ducking child has
                    // restored gains and released its executable mapping.
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                _ => break result,
            }
        };
        result.map_err(|e| {
            format!("Dateiaustausch fehlgeschlagen; die Sicherung bleibt erhalten: {e}")
        })
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, target).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("portable");
        let payload = temp.path().join("payload");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&payload).unwrap();
        for name in FILES {
            fs::write(root.join(name), format!("old {name}")).unwrap();
            fs::write(payload.join(name), format!("new {name}")).unwrap();
        }
        fs::write(root.join("settings.toml"), "user settings").unwrap();
        fs::create_dir(root.join("identity")).unwrap();
        fs::write(root.join("identity/key"), "user identity").unwrap();
        (temp, root, payload)
    }
    fn old_and_private(root: &Path) {
        for name in FILES {
            assert_eq!(
                fs::read_to_string(root.join(name)).unwrap(),
                format!("old {name}")
            );
        }
        assert_eq!(
            fs::read_to_string(root.join("settings.toml")).unwrap(),
            "user settings"
        );
        assert_eq!(
            fs::read_to_string(root.join("identity/key")).unwrap(),
            "user identity"
        );
    }
    #[test]
    fn crash_after_health_before_cleanup_does_not_undo_a_completed_update() {
        let (_temp, root, payload) = fixture();
        let tx = Transaction::prepare(&root, &payload).unwrap();
        tx.commit().unwrap();
        tx.mark_healthy().unwrap();
        drop(tx);
        assert!(recover(&root).unwrap());
        assert_eq!(
            fs::read_to_string(root.join("icu.exe")).unwrap(),
            "new icu.exe"
        );
        assert_eq!(
            fs::read_to_string(root.join("identity/key")).unwrap(),
            "user identity"
        );
        assert!(!pending(&root));
    }
    #[test]
    fn failure_after_executable_exchange_restores_all_files() {
        let (_temp, root, payload) = fixture();
        let tx = Transaction::prepare(&root, &payload).unwrap();
        fs::remove_file(root.join(&tx.journal.stage).join("new/LICENSE")).unwrap();
        assert!(tx.commit().is_err());
        assert_eq!(
            fs::read_to_string(root.join("icu.exe")).unwrap(),
            "new icu.exe"
        );
        assert!(recover(&root).unwrap());
        old_and_private(&root);
        assert!(!pending(&root));
    }
    #[test]
    fn process_interruption_before_and_after_swap_is_recoverable() {
        for swap in [false, true] {
            let (_temp, root, payload) = fixture();
            let tx = Transaction::prepare(&root, &payload).unwrap();
            if swap {
                tx.commit().unwrap();
            }
            drop(tx);
            assert!(recover(&root).unwrap());
            old_and_private(&root);
            assert!(!recover(&root).unwrap());
        }
    }
    #[test]
    fn successful_update_preserves_user_data_and_removes_journal() {
        let (_temp, root, payload) = fixture();
        let tx = Transaction::prepare(&root, &payload).unwrap();
        tx.commit().unwrap();
        tx.finish().unwrap();
        assert_eq!(
            fs::read_to_string(root.join("icu.exe")).unwrap(),
            "new icu.exe"
        );
        assert!(!pending(&root));
        assert_eq!(
            fs::read_to_string(root.join("identity/key")).unwrap(),
            "user identity"
        );
    }
    #[test]
    fn recovery_never_follows_metadata_paths_or_overwrites_later_changes() {
        let (_temp, root, payload) = fixture();
        let tx = Transaction::prepare(&root, &payload).unwrap();
        tx.commit().unwrap();
        fs::write(root.join("LICENSE"), "changed later").unwrap();
        assert!(recover(&root).is_err());
        assert_eq!(
            fs::read_to_string(root.join("LICENSE")).unwrap(),
            "changed later"
        );
        let mut journal: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(JOURNAL)).unwrap()).unwrap();
        journal["stage"] = "../payload".into();
        fs::write(root.join(JOURNAL), serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(recover(&root).is_err());
        assert!(payload.join("icu.exe").exists());
    }
}
