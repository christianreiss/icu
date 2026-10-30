//! Small owned Win32 primitives. No process-name based killing or shell command
//! execution: we retain handles for the parent and the one child we start.
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
};
use windows::{
    core::{HSTRING, PCWSTR, PWSTR},
    Win32::{
        Foundation::*,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        System::{JobObjects::*, Threading::*},
        UI::{Shell::*, WindowsAndMessaging::SW_SHOWNORMAL},
    },
};

pub(super) struct Handle(OwnedHandle);
impl Handle {
    // SAFETY: callers transfer a newly returned, valid owning kernel handle.
    unsafe fn own(handle: HANDLE) -> Self {
        Self(unsafe { OwnedHandle::from_raw_handle(handle.0) })
    }
    pub fn raw(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
    pub fn exited(&self) -> bool {
        unsafe { WaitForSingleObject(self.raw(), 0) == WAIT_OBJECT_0 }
    }
    pub fn wait(&self, ms: u32) -> Result<(), String> {
        match unsafe { WaitForSingleObject(self.raw(), ms) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err("Zeitüberschreitung beim Warten auf ICU.".into()),
            _ => Err(format!(
                "Windows konnte den Prozess nicht abwarten: {}",
                std::io::Error::last_os_error()
            )),
        }
    }
    pub fn exit_code(&self) -> Result<u32, String> {
        let mut code = 0;
        unsafe { GetExitCodeProcess(self.raw(), &mut code) }.map_err(|e| e.to_string())?;
        Ok(code)
    }
}
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Descriptor {
    fn new() -> Result<Self, String> {
        let sid = crate::instance::windows::user_sid().map_err(|e| e.to_string())?;
        let mut raw = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(format!("O:{sid}D:P(A;;GA;;;{sid})(A;;GA;;;SY)(A;;GA;;;BA)")),
                SDDL_REVISION_1,
                &mut raw,
                None,
            )
        }
        .map_err(|e| e.to_string())?;
        Ok(Self(raw))
    }
    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0 .0,
            bInheritHandle: false.into(),
        }
    }
}
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.0 .0)));
        }
    }
}

fn mutex_name(root: &Path) -> String {
    format!(
        "Global\\ICU.Update.{}",
        hex(&Sha256::digest(root.to_string_lossy().to_lowercase().as_bytes())[..16])
    )
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn nonce() -> String {
    hex(&rand::random::<[u8; 16]>())
}
pub(super) fn valid_nonce(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
/// Existence is the lease. Both parent and broker retain an unowned mutex, so
/// there is no ownership transfer gap when the parent exits.
pub(super) fn acquire(root: &Path) -> Result<Handle, String> {
    let descriptor = Descriptor::new()?;
    let attributes = descriptor.attributes();
    let raw = unsafe { CreateMutexW(Some(&attributes), false, &HSTRING::from(mutex_name(root))) }
        .map_err(|e| e.to_string())?;
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let owned = unsafe { Handle::own(raw) };
    if existed {
        Err("Für diese Installation läuft bereits ein Update.".into())
    } else {
        Ok(owned)
    }
}
pub(super) fn join(root: &Path) -> Result<Handle, String> {
    unsafe {
        OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            &HSTRING::from(mutex_name(root)),
        )
        .map(|h| Handle::own(h))
    }
    .map_err(|e| e.to_string())
}
pub(super) fn blocked(root: &Path) -> Result<bool, String> {
    match unsafe {
        OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            &HSTRING::from(mutex_name(root)),
        )
    } {
        Ok(raw) => {
            drop(unsafe { Handle::own(raw) });
            Ok(true)
        }
        Err(error)
            if error.code() == windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) =>
        {
            Ok(false)
        }
        Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_ACCESS_DENIED.0) => {
            Ok(true)
        }
        Err(error) => Err(format!(
            "Update-Sperre konnte nicht geprüft werden: {error}"
        )),
    }
}

pub(super) struct Event(Handle);
impl Event {
    fn name(nonce: &str, kind: &str) -> Result<HSTRING, String> {
        if !valid_nonce(nonce)
            || !matches!(
                kind,
                "staged"
                    | "parent-exited"
                    | "replaced"
                    | "started"
                    | "healthy"
                    | "rollback"
                    | "failed"
            )
        {
            return Err("Ungültige Update-Sitzung.".into());
        }
        Ok(HSTRING::from(format!("Local\\ICU.Update.{nonce}.{kind}")))
    }
    pub fn create(nonce: &str, kind: &str) -> Result<Self, String> {
        let descriptor = Descriptor::new()?;
        let attributes = descriptor.attributes();
        let raw =
            unsafe { CreateEventW(Some(&attributes), true, false, &Self::name(nonce, kind)?) }
                .map_err(|e| e.to_string())?;
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let owned = unsafe { Handle::own(raw) };
        if existed {
            Err("Die Update-Sitzung existiert bereits.".into())
        } else {
            Ok(Self(owned))
        }
    }
    pub fn open(nonce: &str, kind: &str) -> Result<Self, String> {
        unsafe {
            OpenEventW(EVENT_ALL_ACCESS, false, &Self::name(nonce, kind)?)
                .map(|h| Self(Handle::own(h)))
        }
        .map_err(|e| e.to_string())
    }
    pub fn set(&self) -> Result<(), String> {
        unsafe { SetEvent(self.0.raw()) }.map_err(|e| e.to_string())
    }
    pub fn ready(&self) -> bool {
        self.0.exited()
    }
}
pub(super) fn parent(pid: u32, creation: u64, exe: &Path) -> Result<Handle, String> {
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
        .map(|h| Handle::own(h))
    }
    .map_err(|e| format!("Der ursprüngliche ICU-Prozess läuft nicht mehr: {e}"))?;
    let mut name = vec![0u16; 32768];
    let mut len = name.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            handle.raw(),
            PROCESS_NAME_WIN32,
            PWSTR(name.as_mut_ptr()),
            &mut len,
        )
    }
    .map_err(|e| e.to_string())?;
    let actual = PathBuf::from(String::from_utf16_lossy(&name[..len as usize]))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if actual != exe || created(handle.raw())? != creation {
        return Err("Der Update-Auftrag gehört nicht zu diesem ICU-Prozess.".into());
    }
    Ok(handle)
}
fn created(handle: HANDLE) -> Result<u64, String> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
        .map_err(|e| e.to_string())?;
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}
pub(super) fn creation() -> Result<u64, String> {
    created(unsafe { GetCurrentProcess() })
}
/// Quote one Windows CRT argument, including trailing backslashes and quotes.
fn quote(arg: &OsStr) -> Vec<u16> {
    let wide: Vec<_> = arg.encode_wide().collect();
    // NSIS parses /S before its CRT-compatible option helpers and ignores a
    // quoted "/S". Leave simple arguments literal; this still invokes no shell.
    if !wide.is_empty() && !wide.iter().any(|c| matches!(c, 9..=13 | 32 | 34)) {
        return wide;
    }
    let mut result = vec![b'"' as u16];
    let mut slashes = 0;
    for c in arg.encode_wide() {
        if c == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            b'\\' as u16,
            if c == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        result.push(c);
    }
    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    result.push(b'"' as u16);
    result
}
pub(super) fn parameters(args: &[std::ffi::OsString]) -> HSTRING {
    let wide: Vec<u16> = args
        .iter()
        .enumerate()
        .flat_map(|(n, a)| {
            std::iter::once(b' ' as u16)
                .take(usize::from(n > 0))
                .chain(quote(a))
        })
        .collect();
    HSTRING::from_wide(&wide)
}
pub(super) fn elevate(exe: &Path, args: &[std::ffi::OsString]) -> Result<Handle, String> {
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    struct Com(bool);
    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() }
            }
        }
    }
    let initialized =
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if initialized.is_err() && initialized != RPC_E_CHANGED_MODE {
        return Err(format!(
            "Windows-Freigabe konnte nicht vorbereitet werden: {initialized:?}"
        ));
    }
    let _com = Com(initialized.is_ok());
    let exe = HSTRING::from(exe.as_os_str());
    let args = parameters(args);
    let verb = HSTRING::from("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
        return Err(
            if error.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) {
                "Die Windows-Freigabe wurde abgebrochen. ICU läuft unverändert weiter.".into()
            } else {
                format!("Windows konnte das Update nicht starten: {error}")
            },
        );
    }
    if info.hProcess.is_invalid() {
        return Err("Windows lieferte keinen Update-Prozess.".into());
    }
    Ok(unsafe { Handle::own(info.hProcess) })
}

/// A crash of the broker must not leave a failed new EXE locked while the
/// installer attempts rollback. Assignment happens before its first instruction.
pub(super) struct Candidate {
    job: Handle,
    process: Handle,
}
impl Candidate {
    pub fn start(exe: &Path, args: &[std::ffi::OsString]) -> Result<Self, String> {
        let job = unsafe { CreateJobObjectW(None, None).map(|h| Handle::own(h)) }
            .map_err(|e| e.to_string())?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // The ducking worker must survive the GUI long enough to restore other
        // applications' gains on pipe EOF. Kill only our candidate process.
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
        unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|e| e.to_string())?;
        let mut all = vec![exe.as_os_str().to_owned()];
        all.extend_from_slice(args);
        let mut command = parameters(&all).to_vec();
        command.push(0);
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                &HSTRING::from(exe.as_os_str()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_SUSPENDED,
                None,
                &HSTRING::from(exe.parent().ok_or("ICU-Pfad fehlt")?.as_os_str()),
                &startup,
                &mut info,
            )
        }
        .map_err(|e| e.to_string())?;
        let process = unsafe { Handle::own(info.hProcess) };
        let thread = unsafe { Handle::own(info.hThread) };
        if let Err(error) = unsafe { AssignProcessToJobObject(job.raw(), process.raw()) } {
            // Only the suspended child just created here; never another ICU.
            unsafe {
                let _ = TerminateProcess(process.raw(), 1);
            };
            return Err(error.to_string());
        }
        if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
            return Err("Der neue ICU-Prozess konnte nicht fortgesetzt werden.".into());
        }
        Ok(Self { job, process })
    }
    pub fn exited(&self) -> bool {
        self.process.exited()
    }
    pub fn keep(self) -> Result<(), String> {
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        unsafe {
            SetInformationJobObject(
                self.job.raw(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|e| e.to_string())
    }
    pub fn stop(self) {
        unsafe {
            let _ = TerminateJobObject(self.job.raw(), 1);
        };
        let _ = self.process.wait(5000);
    }
}

pub(super) fn private_directory(path: &Path) -> Result<(), String> {
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;
    let sid = crate::instance::windows::user_sid().map_err(|e| e.to_string())?;
    let mut raw = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(format!(
                "O:{sid}D:P(A;OICI;GA;;;{sid})(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)"
            )),
            SDDL_REVISION_1,
            &mut raw,
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    let descriptor = Descriptor(raw);
    let attributes = descriptor.attributes();
    unsafe { CreateDirectoryW(&HSTRING::from(path.as_os_str()), Some(&attributes)) }
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    #[test]
    fn lease_survives_parent_handoff_and_blocks_a_parallel_update() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(!blocked(&root).unwrap());
        let parent = acquire(&root).unwrap();
        assert!(blocked(&root).unwrap());
        assert!(acquire(&root).is_err());
        let broker = join(&root).unwrap();
        drop(parent);
        assert!(blocked(&root).unwrap());
        drop(broker);
        assert!(!blocked(&root).unwrap());
    }
    #[test]
    fn windows_argument_roundtrip_preserves_spaces_quotes_and_trailing_slashes() {
        let args: Vec<OsString> = [
            "icu.exe",
            r"C:\Ein Ordner\Mikrofon ä\",
            r#"a\"b"#,
            "",
            r"\\server\share\",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        let command = parameters(&args);
        let mut count = 0;
        let pointer = unsafe { CommandLineToArgvW(&command, &mut count) };
        assert!(!pointer.is_null());
        let actual: Vec<OsString> = (0..count as usize)
            .map(|i| OsString::from(unsafe { (*pointer.add(i)).to_string().unwrap() }))
            .collect();
        unsafe {
            LocalFree(Some(HLOCAL(pointer.cast())));
        }
        assert_eq!(actual, args);
    }
    #[test]
    fn nsis_silent_switch_is_not_hidden_inside_quotes() {
        let args = [
            "/S".into(),
            "/ICUUpdate=0123456789abcdef0123456789abcdef".into(),
        ];
        assert_eq!(
            parameters(&args).to_string_lossy(),
            "/S /ICUUpdate=0123456789abcdef0123456789abcdef"
        );
    }
}
