//! Invites and passports open ICU (`Client/IMPLEMENTATION.md` §5): `*.icu`
//! files, and any file that starts with the passport armor, are
//! `application/x-icu`, opened with `icu %f`. The rpm/deb install the
//! definition and the desktop entry system-wide; a binary run from anywhere
//! else registers itself for the user, under `~/.local/share`. Switched
//! with `[ui] register_uri_handler` (the name is older than the file type:
//! until 2026-09-24 this registered `icu://` links, which no longer exist).
//! On Windows the association goes under `HKCU\Software\Classes` (`.icu` →
//! `ICU.Invite`, opened with `"icu.exe" "%1"`), which needs no administrator
//! and follows the exe wherever the ZIP was unpacked.

#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

/// The MIME type of invites and passports.
pub const MIME_TYPE: &str = "application/x-icu";

/// The shared-mime-info definition: the same file the packages install as
/// `/usr/share/mime/packages/icu.xml`.
pub const MIME_XML: &str = include_str!("../../../packaging/files/icu-mime.xml");

/// The per-user desktop entry that opens files with `exe`.
#[cfg(target_os = "linux")]
fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=ICU\nComment=ICU voice client\nExec={} %f\n\
         Terminal=false\nCategories=Network;Audio;\nMimeType={MIME_TYPE};\nNoDisplay=false\n",
        exec_quoted(exe)
    )
}

/// `exe` as the program of an `Exec` key: quoted when it holds anything
/// but plain path characters, with the escapes the desktop entry spec asks
/// for (a backslash doubled once for the quoting, once for the string; a
/// literal `%` doubled, or it would read as a field code).
#[cfg(target_os = "linux")]
fn exec_quoted(exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    let plain = exe
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c));
    if plain {
        return exe.into_owned();
    }
    let mut out = String::from("\"");
    for c in exe.chars() {
        match c {
            '"' | '`' | '$' => out.push_str("\\\\"),
            '\\' => out.push_str("\\\\\\"),
            '%' => out.push('%'),
            _ => {}
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Which files [`write_files`] (re)wrote.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
struct Written {
    desktop: bool,
    mime: bool,
}

/// Writes the desktop entry and the MIME definition under `data`
/// (`~/.local/share`), each only where it differs from what is there.
#[cfg(target_os = "linux")]
fn write_files(data: &Path, exe: &Path) -> Result<Written, String> {
    Ok(Written {
        desktop: write_if_changed(
            &data.join("applications").join("icu.desktop"),
            &desktop_entry(exe),
        )?,
        mime: write_if_changed(
            &data.join("mime").join("packages").join("icu.xml"),
            MIME_XML,
        )?,
    })
}

#[cfg(target_os = "linux")]
fn write_if_changed(path: &Path, contents: &str) -> Result<bool, String> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == contents) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

/// Registers this binary for invites and passports; returns the desktop
/// entry in effect. Idempotent: files are written, and the databases
/// rebuilt, only when something changed.
#[cfg(target_os = "linux")]
pub fn register() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let run = |program: &str, args: &[&std::ffi::OsStr]| {
        if let Err(e) = std::process::Command::new(program).args(args).status() {
            tracing::debug!(%e, program, "not run");
        }
    };
    let set_default = || {
        run(
            "xdg-mime",
            &[
                "default".as_ref(),
                "icu.desktop".as_ref(),
                MIME_TYPE.as_ref(),
            ],
        )
    };
    let data = directories::BaseDirs::new().map(|d| d.data_local_dir().to_path_buf());
    // Installed from the rpm/deb: the package ships both files (its
    // triggers rebuild the databases), and user copies would shadow them —
    // so one an earlier, unpackaged ICU left behind goes.
    let packaged = Path::new("/usr/share/applications/icu.desktop");
    if exe == Path::new("/usr/bin/icu") && packaged.exists() {
        if let Some(data) = &data {
            match remove_stale_entry(data, &exe) {
                Ok(true) => {
                    run(
                        "update-desktop-database",
                        &[data.join("applications").as_os_str()],
                    );
                    set_default();
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(%e, "old desktop entry not removed"),
            }
        }
        return Ok(packaged.to_path_buf());
    }
    let data = data.ok_or("no home directory")?;
    let written = write_files(&data, &exe)?;
    if written.mime {
        run("update-mime-database", &[data.join("mime").as_os_str()]);
    }
    if written.desktop {
        run(
            "update-desktop-database",
            &[data.join("applications").as_os_str()],
        );
    }
    if written.mime || written.desktop {
        set_default();
    }
    Ok(data.join("applications").join("icu.desktop"))
}

/// Removes the per-user desktop entry under `data` (`~/.local/share`) that
/// an ICU run from elsewhere wrote, when it opens files with another binary
/// than `exe`. It has the package's name, so it shadowed the package's
/// entry, and invites kept opening a build that may be long gone. Only an
/// entry for our MIME type goes; one that already runs `exe` stays. Returns
/// whether one was removed.
#[cfg(target_os = "linux")]
fn remove_stale_entry(data: &Path, exe: &Path) -> Result<bool, String> {
    let path = data.join("applications").join("icu.desktop");
    let Ok(entry) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let ours = entry
        .lines()
        .any(|l| l.trim() == format!("MimeType={MIME_TYPE};"));
    let program = exec_quoted(exe);
    let runs_exe = entry.lines().any(|l| {
        l.trim_end()
            .strip_prefix("Exec=")
            .is_some_and(|rest| rest == program || rest.starts_with(&format!("{program} ")))
    });
    if !ours || runs_exe {
        return Ok(false);
    }
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    tracing::info!(path = %path.display(), "removed an old desktop entry that shadowed the package's");
    Ok(true)
}

/// The ProgID `.icu` files point to.
#[cfg(target_os = "windows")]
const PROG_ID: &str = "ICU.Invite";

/// Registers this exe for `.icu` files for the current user; returns the
/// exe. Idempotent: only differing values are written, and Explorer is
/// told only when something changed.
#[cfg(target_os = "windows")]
pub fn register() -> Result<std::path::PathBuf, String> {
    use crate::winreg::set_class_string;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let shown = exe.display().to_string();
    let mut changed = false;
    for (key, name, value) in [
        (".icu".to_string(), None, PROG_ID.to_string()),
        (
            ".icu".to_string(),
            Some("Content Type"),
            MIME_TYPE.to_string(),
        ),
        (
            PROG_ID.to_string(),
            None,
            "ICU-Einladung oder -Passport".to_string(),
        ),
        (
            format!(r"{PROG_ID}\DefaultIcon"),
            None,
            format!("\"{shown}\",0"),
        ),
        (
            format!(r"{PROG_ID}\shell\open\command"),
            None,
            format!("\"{shown}\" \"%1\""),
        ),
    ] {
        changed |= set_class_string(&key, name, &value)?;
    }
    if changed {
        use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
        // SAFETY: a notification without items.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }
    Ok(exe)
}

/// Other platforms.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn register() -> Result<std::path::PathBuf, String> {
    Err("Unsupported".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_entry_opens_icu_files() {
        let entry = desktop_entry(Path::new("/opt/icu/bin/icu"));
        assert!(entry.contains("\nExec=/opt/icu/bin/icu %f\n"), "{entry}");
        assert!(entry.contains("\nMimeType=application/x-icu;\n"));
        assert!(!entry.contains("x-scheme-handler"), "no more icu:// links");
    }

    #[test]
    fn unusual_paths_are_quoted() {
        assert_eq!(
            exec_quoted(Path::new("/home/a b/icu")),
            "\"/home/a b/icu\"",
            "a space"
        );
        assert_eq!(
            exec_quoted(Path::new("/tmp/$x/icu")),
            "\"/tmp/\\\\$x/icu\"",
            "a dollar sign, escaped for quoting and string"
        );
        assert_eq!(
            exec_quoted(Path::new("/usr/local/bin/icu")),
            "/usr/local/bin/icu"
        );
        assert_eq!(
            exec_quoted(Path::new("/opt/100%/icu")),
            "\"/opt/100%%/icu\"",
            "a percent sign is not a field code"
        );
    }

    /// Running as the package, a user entry an earlier build left behind is
    /// removed — it shadowed the package's — but not one that already runs
    /// the package's binary, nor somebody else's `icu.desktop`.
    #[test]
    fn the_package_removes_a_stale_user_entry() {
        let data = tempfile::tempdir().unwrap();
        let entry = data.path().join("applications/icu.desktop");
        let packaged = Path::new("/usr/bin/icu");
        assert!(
            !remove_stale_entry(data.path(), packaged).unwrap(),
            "none there"
        );

        write_files(data.path(), Path::new("/home/a/src/icu/target/release/icu")).unwrap();
        assert!(remove_stale_entry(data.path(), packaged).unwrap());
        assert!(!entry.exists());

        write_files(data.path(), packaged).unwrap();
        assert!(!remove_stale_entry(data.path(), packaged).unwrap());
        assert!(entry.exists(), "it runs the package's binary");

        std::fs::write(&entry, "[Desktop Entry]\nExec=/opt/other/icu\n").unwrap();
        assert!(!remove_stale_entry(data.path(), packaged).unwrap());
        assert!(entry.exists(), "not ours");
    }

    #[test]
    fn the_mime_definition_covers_files_and_armor() {
        assert!(MIME_XML.contains("<mime-type type=\"application/x-icu\">"));
        assert!(MIME_XML.contains("<glob pattern=\"*.icu\"/>"));
        assert!(MIME_XML.contains("value=\"-----BEGIN ICU PASSPORT-----\""));
    }

    /// Both files land under the data directory, once: a second start with
    /// the same binary writes (and rebuilds) nothing.
    #[test]
    fn files_are_written_once() {
        let data = tempfile::tempdir().unwrap();
        let exe = Path::new("/opt/icu/bin/icu");
        assert_eq!(
            write_files(data.path(), exe).unwrap(),
            Written {
                desktop: true,
                mime: true
            }
        );
        assert_eq!(
            std::fs::read_to_string(data.path().join("mime/packages/icu.xml")).unwrap(),
            MIME_XML
        );
        assert_eq!(
            write_files(data.path(), exe).unwrap(),
            Written {
                desktop: false,
                mime: false
            }
        );
        let moved = Path::new("/opt/icu-2/bin/icu");
        assert_eq!(
            write_files(data.path(), moved).unwrap(),
            Written {
                desktop: true,
                mime: false
            },
            "the binary moved"
        );
    }
}
