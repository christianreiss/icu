//! The few registry writes ICU makes on Windows, all under
//! `HKEY_CURRENT_USER\Software\Classes` (no administrator needed): the
//! portable `.icu` file association (`uri`) and the app id toasts (`notify`).
//! The installer owns the machine-wide `.icu` association.

use windows::{
    core::{HSTRING, PCWSTR},
    Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{
            RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
            RRF_RT_REG_SZ,
        },
    },
};

/// The app id ICU's toasts and taskbar button go by.
pub const APP_ID: &str = "ICU";

/// Sets the string `name` (`None`: the key's default value) of
/// `HKCU\Software\Classes\<key>` to `value`, creating the key. Returns
/// whether anything changed.
pub fn set_class_string(key: &str, name: Option<&str>, value: &str) -> Result<bool, String> {
    let path = format!(r"Software\Classes\{key}");
    if get(HKEY_CURRENT_USER, &path, name).as_deref() == Some(value) {
        return Ok(false);
    }
    let mut hkey = HKEY::default();
    // SAFETY: plain registry calls; `hkey` is closed below.
    unsafe {
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path.as_str()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            None,
            &mut hkey,
            None,
        );
        if status != ERROR_SUCCESS {
            return Err(format!("HKCU\\{path}: {status:?}"));
        }
        let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
        let name = name.map(HSTRING::from);
        let status = RegSetValueExW(
            hkey,
            name.as_ref()
                .map(|n| PCWSTR(n.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            None,
            REG_SZ,
            Some(bytes),
        );
        let _ = RegCloseKey(hkey);
        if status != ERROR_SUCCESS {
            return Err(format!("HKCU\\{path}: {status:?}"));
        }
    }
    Ok(true)
}

/// A string value under `root\<path>`, if there is one.
fn get(root: HKEY, path: &str, name: Option<&str>) -> Option<String> {
    let name = name.map(HSTRING::from);
    let name = name
        .as_ref()
        .map(|n| PCWSTR(n.as_ptr()))
        .unwrap_or(PCWSTR::null());
    let path = HSTRING::from(path);
    let mut size = 0u32;
    // SAFETY: first call asks for the size, second fills a buffer that big.
    unsafe {
        if RegGetValueW(
            root,
            &path,
            name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        if RegGetValueW(
            root,
            &path,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// The installer registers `.icu` for all users. An installed client must
/// leave that association alone so uninstall cannot strand a per-user entry.
pub fn is_machine_installed_exe(exe: &std::path::Path) -> bool {
    let Some(dir) = get(
        HKEY_LOCAL_MACHINE,
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\ICU",
        Some("InstallLocation"),
    ) else {
        return false;
    };
    let Some(parent) = exe.parent() else {
        return false;
    };
    match (
        parent.canonicalize(),
        std::path::Path::new(&dir).canonicalize(),
    ) {
        (Ok(actual), Ok(installed)) => actual
            .to_string_lossy()
            .eq_ignore_ascii_case(&installed.to_string_lossy()),
        _ => false,
    }
}
