//! Windows: gives `icu.exe` its icon (Explorer, the `.icu` file type), a
//! version resource and an application manifest — a resource script
//! compiled by the MinGW `windres` and linked into the binaries only.
//! Nothing to do for other targets.
//!
//! Without `windres` the build goes on without resources and says so;
//! `packaging/windows/build.sh` refuses such an `icu.exe`.

use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icu.ico");
    println!("cargo:rerun-if-changed=assets/icu.manifest");
    println!("cargo:rerun-if-env-changed=WINDRES");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if let Err(e) = resources() {
        println!("cargo:warning=icu.exe without icon, version and manifest: {e}");
    }
}

fn resources() -> Result<(), String> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let out = PathBuf::from(env::var("OUT_DIR").map_err(|e| e.to_string())?);
    // windres reads backslashes in strings as escapes.
    let asset = |name: &str| {
        manifest_dir
            .join("assets")
            .join(name)
            .display()
            .to_string()
            .replace('\\', "/")
    };
    let version = env::var("CARGO_PKG_VERSION").map_err(|e| e.to_string())?;
    // `0.1.0-beta.2` → 0,1,0,0: the numeric form has no room for the rest.
    let mut numbers = version
        .split(['.', '-', '+'])
        .take(3)
        .map(|n| n.parse::<u16>().unwrap_or(0));
    let [major, minor, patch] = [(); 3].map(|()| numbers.next().unwrap_or(0));
    let script = format!(
        r#"1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "000004B0"
    BEGIN
      VALUE "FileDescription", "ICU"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "icu"
      VALUE "LegalCopyright", "Apache-2.0"
      VALUE "OriginalFilename", "icu.exe"
      VALUE "ProductName", "ICU"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0000, 1200
  END
END
"#,
        icon = asset("icu.ico"),
        manifest = asset("icu.manifest"),
    );
    let rc = out.join("icu.rc");
    let object = out.join("icu-resources.o");
    std::fs::write(&rc, script).map_err(|e| format!("{}: {e}", rc.display()))?;
    let windres = env::var("WINDRES").unwrap_or_else(|_| {
        if cfg!(windows) {
            "windres".into()
        } else {
            let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
            format!("{arch}-w64-mingw32-windres")
        }
    });
    let status = Command::new(&windres)
        .arg("--input-format=rc")
        .arg("--output-format=coff")
        .arg("-i")
        .arg(&rc)
        .arg("-o")
        .arg(&object)
        .status()
        .map_err(|e| format!("{windres}: {e}"))?;
    if !status.success() {
        return Err(format!("{windres}: {status}"));
    }
    // An object on the link line, not a library: nothing references it, and
    // the linker would drop it from an archive.
    println!("cargo:rustc-link-arg-bins={}", object.display());
    Ok(())
}
