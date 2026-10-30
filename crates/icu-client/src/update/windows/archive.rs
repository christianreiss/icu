use super::super::{manifest::MAX_ARTIFACT, transaction::FILES};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
/// Only the four build-produced files, in exactly one fixed release directory.
/// ZIP names are never passed to join/extract: the allowlisted basename is used.
pub(super) fn extract(file: File, version: &semver::Version, dest: &Path) -> Result<(), String> {
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("Ungültiges Update-Archiv: {e}"))?;
    if zip.len() > FILES.len() + 1 {
        return Err("Das Update-Archiv enthält unerwartete Dateien.".into());
    }
    let prefix = format!("icu-{version}-windows-x86_64/");
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?.into_owned();
        if !seen.insert(name.clone()) || entry.encrypted() || entry.is_symlink() {
            return Err("Unsicherer oder doppelter Eintrag im Update-Archiv.".into());
        }
        if name == prefix && entry.is_dir() {
            continue;
        }
        let base = name
            .strip_prefix(&prefix)
            .filter(|s| FILES.contains(s))
            .ok_or("Unsicherer Pfad im Update-Archiv.")?;
        if !entry.is_file()
            || entry
                .unix_mode()
                .is_some_and(|m| m & 0o170000 != 0 && m & 0o170000 != 0o100000)
        {
            return Err("Das Update-Archiv enthält keine reguläre Datei.".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Update-Archiv ist zu groß.")?;
        if total > MAX_ARTIFACT || entry.size() == 0 {
            return Err("Update-Archiv ist zu groß oder enthält eine leere Datei.".into());
        }
        let mut target = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dest.join(base))
            .map_err(|e| e.to_string())?;
        let expected = entry.size();
        let copied = std::io::copy(&mut entry.by_ref().take(expected + 1), &mut target)
            .map_err(|e| e.to_string())?;
        if copied != expected {
            return Err("Unvollständige Datei im Update-Archiv.".into());
        }
        target
            .flush()
            .and_then(|_| target.sync_all())
            .map_err(|e| e.to_string())?;
    }
    if FILES
        .iter()
        .any(|name| !seen.contains(&format!("{prefix}{name}")))
    {
        return Err("Im Update-Archiv fehlen Programmdateien.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::{write::SimpleFileOptions, ZipWriter};
    fn archive(path: &Path, names: &[&str], symlink: bool) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        for name in names {
            if symlink {
                writer
                    .add_symlink(*name, "../../outside", SimpleFileOptions::default())
                    .unwrap();
            } else {
                writer
                    .start_file(*name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"program-file").unwrap();
            }
        }
        writer.finish().unwrap();
    }
    #[test]
    fn only_fixed_regular_files_from_the_exact_release_directory() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input.zip");
        let dest = temp.path().join("output");
        fs_create(&dest);
        let names: Vec<String> = FILES
            .iter()
            .map(|name| format!("icu-1.2.3-windows-x86_64/{name}"))
            .collect();
        archive(
            &source,
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
            false,
        );
        extract(
            File::open(&source).unwrap(),
            &semver::Version::new(1, 2, 3),
            &dest,
        )
        .unwrap();
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 4);
        for bad in [
            "../outside",
            "icu-1.2.3-windows-x86_64/../outside",
            "icu-1.2.3-windows-x86_64/icu.exe:stream",
            "icu-1.2.4-windows-x86_64/icu.exe",
            "icu-1.2.3-windows-x86_64/\\outside",
        ] {
            archive(&source, &[bad], false);
            assert!(
                extract(
                    File::open(&source).unwrap(),
                    &semver::Version::new(1, 2, 3),
                    &dest
                )
                .is_err(),
                "{bad}"
            );
        }
        archive(&source, &[&names[0]], true);
        assert!(extract(
            File::open(&source).unwrap(),
            &semver::Version::new(1, 2, 3),
            &dest
        )
        .is_err());
        assert!(!temp.path().join("outside").exists());
    }
    fn fs_create(path: &Path) {
        std::fs::create_dir(path).unwrap();
    }
}
