//! A failed/short/oversized/tampered stream never leaves an executable payload.
use super::manifest::Artifact;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
};

pub(super) fn verified_stream(
    mut source: impl Read,
    dest: &Path,
    artifact: &Artifact,
    progress: impl Fn(u64, u64),
) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(dest)
        .map_err(|e| format!("Update-Datei konnte nicht angelegt werden: {e}"))?;
    let result = (|| {
        let mut hash = Sha256::new();
        let mut received = 0u64;
        let mut block = [0u8; 64 * 1024];
        loop {
            let n = source
                .read(&mut block)
                .map_err(|e| format!("Download unterbrochen: {e}"))?;
            if n == 0 {
                break;
            }
            received += n as u64;
            if received > artifact.size {
                return Err("Die Update-Datei ist größer als angekündigt.".into());
            }
            file.write_all(&block[..n])
                .map_err(|e| format!("Update konnte nicht gespeichert werden: {e}"))?;
            hash.update(&block[..n]);
            progress(received, artifact.size);
        }
        if received != artifact.size {
            return Err("Der Download ist unvollständig. Bitte erneut versuchen.".into());
        }
        if format!("{:x}", hash.finalize()) != artifact.sha256.to_ascii_lowercase() {
            return Err(
                "Die Update-Datei stimmt nicht mit der signierten Prüfsumme überein.".into(),
            );
        }
        file.sync_all()
            .map_err(|e| format!("Update konnte nicht sicher gespeichert werden: {e}"))?;
        Ok(())
    })();
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}
#[cfg(windows)]
pub(super) fn recheck(file: &mut std::fs::File, artifact: &Artifact) -> Result<(), String> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let size = std::io::copy(file, &mut hash).map_err(|e| e.to_string())?;
    if size != artifact.size
        || format!("{:x}", hash.finalize()) != artifact.sha256.to_ascii_lowercase()
    {
        return Err("Die Update-Datei wurde nach dem Download verändert.".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn artifact() -> Artifact {
        Artifact {
            name: "icu.zip".into(),
            size: 4,
            sha256: format!("{:x}", Sha256::digest(b"good")),
            kind: "client".into(),
            os: "windows".into(),
            format: "zip".into(),
            arch: "x86_64".into(),
            installation: None,
        }
    }
    #[test]
    fn complete_authenticated_bytes_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update");
        for bytes in [&b"bad!"[..], &b"goo"[..], &b"good extra"[..]] {
            assert!(verified_stream(bytes, &path, &artifact(), |_, _| {}).is_err());
            assert!(!path.exists());
        }
        verified_stream(&b"good"[..], &path, &artifact(), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"good");
        assert!(verified_stream(&b"evil"[..], &path, &artifact(), |_, _| {}).is_err());
        assert_eq!(
            std::fs::read(path).unwrap(),
            b"good",
            "never overwrite an existing file"
        );
    }
    #[test]
    fn interrupted_download_removes_partial_payload() {
        struct Interrupted(bool);
        impl Read for Interrupted {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    Err(std::io::ErrorKind::ConnectionReset.into())
                } else {
                    self.0 = true;
                    b[0] = b'g';
                    Ok(1)
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update");
        assert!(verified_stream(Interrupted(false), &path, &artifact(), |_, _| {}).is_err());
        assert!(!path.exists());
    }
}
