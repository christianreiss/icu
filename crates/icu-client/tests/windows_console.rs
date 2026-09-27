//! Run natively on Windows. A cross-built test can use ICU_TEST_EXE to
//! point at the packaged executable instead of Cargo's build-host path.
#![cfg(windows)]

use std::{
    fs,
    os::windows::process::CommandExt,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn gui_executable_preserves_cmd_redirection_and_pipes() {
    let exe =
        std::env::var("ICU_TEST_EXE").unwrap_or_else(|_| env!("CARGO_BIN_EXE_icu").to_string());
    assert!(
        !exe.contains(['"', '\r', '\n', '%']),
        "unsupported test path"
    );
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("probe.cmd");
    fs::write(
        &script,
        format!(
            "@echo off\r\n\
         \"{exe}\" --help > help.txt 2> help.err\r\n\
         if errorlevel 1 exit /b 10\r\n\
         \"{exe}\" --icu-invalid-option > NUL 2> error.txt\r\n\
         if not errorlevel 1 exit /b 11\r\n\
         \"{exe}\" --version | findstr /C:\"icu \" > pipe.txt\r\n\
         if errorlevel 1 exit /b 12\r\n"
        ),
    )
    .unwrap();
    // The defect requires a real parent console: AttachConsole replaced
    // valid redirected handles in GUI executables launched by cmd.exe.
    let mut child = Command::new("cmd.exe")
        .args(["/D", "/C"])
        .arg(&script)
        .current_dir(dir.path())
        .creation_flags(0x00000010) // CREATE_NEW_CONSOLE
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("console probe timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let read =
        |name| String::from_utf8_lossy(&fs::read(dir.path().join(name)).unwrap()).into_owned();
    let help = read("help.txt");
    let error = read("error.txt");
    let pipe = read("pipe.txt");
    assert!(
        status.success(),
        "cmd status={status}, help={help:?}, error={error:?}, pipe={pipe:?}"
    );
    assert!(
        help.contains("--headless"),
        "stdout redirection lost: {help:?}"
    );
    assert!(read("help.err").is_empty());
    assert!(
        error.contains("--icu-invalid-option"),
        "stderr redirection lost: {error:?}"
    );
    assert!(pipe.starts_with("icu "), "pipe output lost: {pipe:?}");
}
