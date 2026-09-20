//! `icu://` URI handler registration (`Client/IMPLEMENTATION.md` §5).
//! Linux: a `.desktop` file under `~/.local/share/applications` plus
//! `xdg-mime default`. Windows: M3 stub.

/// Registers this binary as the handler for `icu://` links.
#[cfg(target_os = "linux")]
pub fn register() -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let apps = directories::BaseDirs::new()
        .ok_or("no home directory")?
        .data_local_dir()
        .join("applications");
    std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    let path = apps.join("icu.desktop");
    let contents = format!(
        "[Desktop Entry]\nType=Application\nName=ICU\nComment=ICU voice client\nExec={} %u\nTerminal=false\nCategories=Network;Audio;\nMimeType=x-scheme-handler/icu;\nNoDisplay=false\n",
        exe.display()
    );
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing != contents {
        let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        f.write_all(contents.as_bytes())
            .map_err(|e| e.to_string())?;
        let _ = std::process::Command::new("xdg-mime")
            .args(["default", "icu.desktop", "x-scheme-handler/icu"])
            .status();
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&apps)
            .status();
    }
    Ok(path)
}

/// Windows registration is M3.
#[cfg(target_os = "windows")]
pub fn register() -> Result<std::path::PathBuf, String> {
    Err("Unsupported: URI handler registration not implemented (M3)".into())
}

/// Other platforms.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn register() -> Result<std::path::PathBuf, String> {
    Err("Unsupported".into())
}
