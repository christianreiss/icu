//! `icu` — the ICU voice client.
//!
//! `icu` starts. `icu <file>` opens an invite or passport in the first-start
//! window (with `--name`/`--yes`/`--skip-settings`/`--headless` it is taken
//! on the terminal), then starts; `icu -` reads it from standard input.
//! `--config-dir` allows two instances on one machine,
//! `--headless` runs without a window,
//! `--debug-stats` prints latency histograms to stderr every 2 s,
//! `--test-tone` replaces the microphone with a synthetic voice.
//!
//! On Windows `icu.exe` is a GUI program: a double-click opens no console.
//! Started from a terminal it writes to that terminal (`AttachConsole`);
//! otherwise the log goes to `%LOCALAPPDATA%\icu\data\icu.log`, and an
//! error that ends ICU is shown in a message box as well. Elsewhere the log
//! goes to stderr, and without a terminal on it also to
//! `$XDG_STATE_HOME/icu/icu.log` (`~/.local/state/icu/icu.log`).

// A desktop start must not open a console window next to ICU's own.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{path::PathBuf, process::ExitCode, time::Duration};

use clap::Parser;
use icu_client::{
    config::{Identity, Paths, Settings},
    engine::{Engine, EngineConfig},
    net::register::register,
};

/// ICU voice client.
#[derive(Debug, Parser)]
#[command(name = "icu", version, about, long_about = None)]
struct Cli {
    /// Invite (registration bundle) or passport to take over: an `.icu`
    /// file, or `-` for its text on standard input. Never the text itself:
    /// it carries a private key, and arguments are visible to every local
    /// user and end up in the shell history. It opens in the first-start
    /// window, which shows where it leads and joins on a click; with --yes,
    /// --name, --skip-settings or --headless it is taken on the terminal
    /// instead.
    bundle: Option<String>,
    /// The name to register with. It goes into the certificate, so it is what
    /// everyone sees. Needed for a registration bundle, ignored for a
    /// passport.
    #[arg(long)]
    name: Option<String>,
    /// Take the bundle or passport without asking. Without it the server,
    /// address and name it leads to are shown and have to be confirmed.
    #[arg(long)]
    yes: bool,
    /// Take only the identity from a passport. By default the settings it
    /// carries replace this device's, except devices and camera.
    #[arg(long)]
    skip_settings: bool,
    /// Configuration directory (default `$XDG_CONFIG_HOME/icu`, on Windows
    /// `%APPDATA%\icu\config`).
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// Run without a window.
    #[arg(long)]
    headless: bool,
    /// Print counters and latency histograms to stderr every 2 s.
    #[arg(long)]
    debug_stats: bool,
    /// Send a synthetic voice-like signal instead of the microphone, gate
    /// always open (integration checks without a microphone).
    #[arg(long)]
    test_tone: bool,
    /// Use the silent fake audio backend (no sound system needed).
    #[arg(long)]
    fake_audio: bool,
    /// Override the server address (tests: `127.0.0.1:port`).
    #[arg(long)]
    server_addr: Option<std::net::SocketAddr>,
    /// Do not register ICU for invites and passports (`.icu` files, MIME
    /// type application/x-icu).
    #[arg(long)]
    no_uri_handler: bool,
    /// Do not start global hotkeys.
    #[arg(long)]
    no_hotkeys: bool,
    /// Start with the camera on: a device index, or `synthetic` for a test
    /// pattern (no hardware needed).
    #[arg(long)]
    camera: Option<String>,
}

fn main() -> ExitCode {
    // Before anything loads a DLL by name: wgpu asks for `dxcompiler.dll`
    // and `vulkan-1.dll`, which Windows would otherwise also look for in the
    // working directory — the folder of a double-clicked `.icu` file,
    // Downloads — and on PATH. From here on only System32 and ICU's own
    // folder count.
    #[cfg(windows)]
    // SAFETY: a plain Win32 call on process-wide loader state.
    let dll_search = unsafe {
        use windows::Win32::System::LibraryLoader::{
            SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_APPLICATION_DIR,
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        };
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)
    };
    #[cfg(windows)]
    let console = attach_console();
    // Toasts and the taskbar button go by ICU's app id (`notify`).
    #[cfg(windows)]
    // SAFETY: a plain Win32 call, before any window exists.
    let _ = unsafe {
        windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(
            &windows::core::HSTRING::from(icu_client::winreg::APP_ID),
        )
    };
    let cli = Cli::parse();
    // A terminal shows the log as it comes; without one it goes to a file.
    #[cfg(windows)]
    let terminal = console;
    #[cfg(not(windows))]
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let log_file = (!terminal)
        .then(|| log_dir().and_then(|dir| open_log(&dir)))
        .flatten();
    let to_file = log_file.is_some();
    init_tracing(log_file);
    install_panic_hook();
    #[cfg(windows)]
    if let Err(e) = dll_search {
        tracing::warn!(%e, "SetDefaultDllDirectories: DLLs are still searched for everywhere");
    }
    // Started from the desktop, an error that ends ICU would otherwise end
    // it without a word: nobody reads a log file on a double-click.
    #[cfg(windows)]
    let dialog = !console && !cli.headless;
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if to_file {
                tracing::error!(error = %e, "ICU ends");
            } else {
                eprintln!("icu: {e}");
            }
            #[cfg(windows)]
            if dialog {
                message_box(
                    &format!("ICU konnte nicht starten:\n\n{e}"),
                    windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                );
            }
            ExitCode::FAILURE
        }
    }
}

/// Every panic goes to the log before the process ends (the release build
/// aborts on a panic): on a desktop start that is the only trace it leaves.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!(thread = thread.name().unwrap_or("?"), %info, "panic");
        default(info);
    }));
}

/// Shows `text` in a message box of its own (no owner window) and returns
/// the button pressed.
#[cfg(windows)]
fn message_box(
    text: &str,
    style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    use windows::{
        core::HSTRING,
        Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_SETFOREGROUND, MB_TOPMOST},
    };
    // SAFETY: both strings outlive the call.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("ICU"),
            style | MB_SETFOREGROUND | MB_TOPMOST,
        )
    }
}

/// Whether `Identity::load` failed because DPAPI would not unseal the
/// private key (`config::key_seal`): the Windows password was reset by an
/// administrator, or the profile came from another machine. Retrying never
/// helps; only a new invite or a passport does.
#[cfg(windows)]
fn key_unsealable(e: &icu_client::config::ConfigError) -> bool {
    matches!(e, icu_client::config::ConfigError::Io(io)
        if io.to_string().starts_with("CryptUnprotectData"))
}

/// Asks whether to drop credentials this Windows account cannot decrypt.
/// „Nein“ (the default button) changes nothing.
#[cfg(windows)]
fn confirm_forget_identity() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO};
    message_box(
        "ICU kann seine gespeicherten Zugangsdaten mit diesem Windows-Konto nicht \
         entschlüsseln. Das passiert, wenn das Kennwort des Kontos von einem Administrator \
         zurückgesetzt wurde oder das Profil von einem anderen PC stammt.\n\n\
         Zugangsdaten verwerfen und ICU mit einer neuen Einladung oder einem Passport \
         einrichten?\n\n\
         Ja: die Zugangsdaten (und hier gespeicherte Einladungen) werden gelöscht, das \
         Einladungsfenster öffnet sich.\n\
         Nein: ICU beendet sich, nichts wird geändert.",
        MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
    ) == IDYES
}

/// Asks on the terminal whether to take the shown bundle or passport. Without
/// a terminal there is nobody to ask: that needs `--yes`.
fn confirm_import() -> Result<bool, String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(
            "no terminal to confirm the import on; check the lines above and pass --yes".into(),
        );
    }
    eprint!("icu: Diesen Server übernehmen? [j/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    Ok(matches!(
        answer.trim().to_lowercase().as_str(),
        "j" | "ja" | "y" | "yes"
    ))
}

/// The bundle argument as text: a file's contents, or with `-` standard
/// input. A passport is at most 8 KiB decoded; a much larger file is not
/// one and is not read whole.
fn read_bundle(arg: &str) -> Result<String, String> {
    read_bundle_from(arg, std::io::stdin().lock())
}

/// [`read_bundle`], with `-` reading `stdin`. The armored text itself is
/// refused: with its private key it would sit in `/proc/<pid>/cmdline` for
/// every local user as long as ICU runs, and in the shell history.
fn read_bundle_from(arg: &str, stdin: impl std::io::Read) -> Result<String, String> {
    use std::io::Read;
    const MAX: u64 = 64 * 1024;
    if icu_proto::passport::Passport::looks_like(arg) {
        return Err(
            "Einladung und Passport bitte nicht als Text übergeben: der private \
                    Schlüssel darin wäre für jeden Nutzer dieses Rechners in der Prozessliste \
                    zu sehen und stünde in der Shell-History. Stattdessen als Datei \
                    (`icu datei.icu`) oder über die Standardeingabe (`icu - < datei.icu`)."
                .into(),
        );
    }
    let mut text = String::new();
    if arg == "-" {
        stdin
            .take(MAX)
            .read_to_string(&mut text)
            .map_err(|e| format!("stdin: {e}"))?;
        return Ok(text);
    }
    std::fs::File::open(arg)
        .and_then(|f| f.take(MAX).read_to_string(&mut text))
        .map_err(|e| format!("{arg}: {e}"))?;
    Ok(text)
}

/// Whether `icu <file>` opens the first-start window: a desktop start has
/// no terminal to confirm on, and the window asks for the name itself. A
/// flag for the import means the terminal.
fn in_window(cli: &Cli) -> bool {
    cfg!(feature = "gui") && !cli.headless && !cli.yes && !cli.skip_settings && cli.name.is_none()
}

/// Takes an invite or passport on the terminal: shows where it leads,
/// asks `[j/N]` (or `--yes`), registers under `--name` or stores it, and a
/// passport's settings with it unless `--skip-settings`.
fn import_in_terminal(cli: &Cli, paths: &Paths, text: &str) -> Result<(), String> {
    let (mut identity, settings) = Identity::from_passport_with_settings(text)?;
    // A passport brings its own CA and address: show where it leads
    // before anything is replaced (`PROTOCOL.md` §2.3.1).
    let current = Identity::load(paths).ok().flatten();
    let summary = icu_client::config::ImportSummary::of(&identity, current.as_ref());
    for line in summary.lines() {
        eprintln!("icu: {line}");
    }
    let settings = settings.filter(|_| !identity.is_registration());
    if settings.is_some() {
        eprintln!(
            "icu: {}",
            if cli.skip_settings {
                "Einstellungen: bleiben, wie sie hier sind (--skip-settings)"
            } else {
                "Einstellungen: werden übernommen (Geräte und Kamera bleiben)"
            }
        );
    }
    if !cli.yes && !confirm_import()? {
        return Err("import cancelled".into());
    }
    if identity.is_registration() {
        let name = cli
            .name
            .clone()
            .ok_or("a registration bundle needs --name <display name>")?;
        let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        identity = rt
            .block_on(register(
                &identity,
                &name,
                &icu_client::engine::client_string(),
                cli.server_addr,
                paths,
            ))
            .map_err(|e| e.to_string())?;
    } else {
        identity.save(paths).map_err(|e| e.to_string())?;
        if let Some(settings) = settings.filter(|_| !cli.skip_settings) {
            // The identity is in; without the settings this device keeps
            // its own.
            match icu_client::config::import_settings(paths, settings) {
                Ok(()) => eprintln!("icu: settings taken over from the passport"),
                Err(e) => eprintln!("icu: settings not taken over: {e}"),
            }
        }
    }
    eprintln!(
        "icu: registered as {} at {}:{}, identity stored in {}",
        identity.server.name,
        identity.server.host,
        identity.server.port,
        paths.identity_dir().display()
    );
    Ok(())
}

/// Joins the console of the terminal ICU was started from, if any, so
/// `--help`, the import questions and the log reach it.
#[cfg(windows)]
fn attach_console() -> bool {
    use windows::Win32::{
        Storage::FileSystem::{GetFileType, FILE_TYPE_UNKNOWN},
        System::Console::{
            AttachConsole, GetConsoleMode, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS,
            CONSOLE_MODE, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        },
    };
    // AttachConsole can replace inherited files/pipes with console handles
    // (e.g. `icu --help > help.txt` from cmd.exe). Preserve redirection,
    // including NUL, while letting Windows supply missing console handles.
    let redirected = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|id| {
        // SAFETY: borrowed process standard handles; never closed here.
        let handle = unsafe { GetStdHandle(id) }.ok().filter(|handle| unsafe {
            !handle.is_invalid()
                && GetFileType(*handle) != FILE_TYPE_UNKNOWN
                && GetConsoleMode(*handle, &mut CONSOLE_MODE::default()).is_err()
        });
        (id, handle)
    });
    // SAFETY: fails harmlessly without a parent console.
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok();
    let mut redirected_error = false;
    for (id, handle) in redirected {
        if let Some(handle) = handle {
            // SAFETY: restores a valid inherited handle, owned by the process.
            if unsafe { SetStdHandle(id, handle) }.is_ok() && id == STD_ERROR_HANDLE {
                redirected_error = true;
            }
        }
    }
    // SSH/pipes need stderr logging even without a parent console.
    attached || redirected_error
}

/// Where the log goes when no terminal shows it: `%LOCALAPPDATA%\icu\data`
/// on Windows, elsewhere the XDG state directory.
fn log_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    return Some(
        directories::ProjectDirs::from("", "", "icu")?
            .data_local_dir()
            .to_path_buf(),
    );
    #[cfg(not(windows))]
    state_dir(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// `$XDG_STATE_HOME/icu`, or `~/.local/state/icu` when that is unset or not
/// absolute (XDG Base Directory Specification).
#[cfg(not(windows))]
fn state_dir(
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let absolute = |p: std::ffi::OsString| Some(PathBuf::from(p)).filter(|p| p.is_absolute());
    let base = xdg_state_home
        .and_then(absolute)
        .or_else(|| home.and_then(absolute).map(|h| h.join(".local/state")))?;
    Some(base.join("icu"))
}

/// `icu.log` in `dir`, appended to: a second start (which hands over to the
/// running ICU and ends) must not cut the running one's log. Over 1 MiB it
/// moves to `icu.log.1` first. Only the owner may read it on Unix.
fn open_log(dir: &std::path::Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(dir).ok()?;
    let log = dir.join("icu.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&log, dir.join("icu.log.1"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(log).ok()
}

/// Logs to stderr, or to `file` — on Linux to both: a desktop start's
/// stderr goes to the journal, which keeps getting it.
fn init_tracing(file: Option<std::fs::File>) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = fmt::Subscriber::builder().with_env_filter(filter);
    let _ = match file {
        #[cfg(windows)]
        Some(file) => tracing::subscriber::set_global_default(
            builder
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .finish(),
        ),
        #[cfg(not(windows))]
        Some(file) => {
            use tracing_subscriber::fmt::writer::MakeWriterExt;
            tracing::subscriber::set_global_default(
                builder
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(file).and(std::io::stderr))
                    .finish(),
            )
        }
        None => {
            tracing::subscriber::set_global_default(builder.with_writer(std::io::stderr).finish())
        }
    };
}

// Without `gui` there is no logout, so nothing loops, and no window to show.
#[cfg_attr(
    not(feature = "gui"),
    allow(unused_mut, unused_variables, clippy::never_loop)
)]
fn run(cli: Cli) -> Result<(), String> {
    let paths = Paths::new(cli.config_dir.clone());
    std::fs::create_dir_all(&paths.config_dir).map_err(|e| e.to_string())?;
    // An unreadable settings.toml does not stop a desktop start: it is moved
    // aside and the window says so.
    let (settings, mut settings_problem) =
        Settings::load_or_recover(&paths).map_err(|e| e.to_string())?;

    // As text, read here: the running instance it may be handed to can sit
    // in another working directory.
    let bundle = match cli.bundle.as_deref() {
        // A click on an old invite link still starts ICU.
        Some(link)
            if link
                .get(..6)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("icu://")) =>
        {
            eprintln!(
                "icu: icu://-Links gibt es nicht mehr. Eine Einladung ist eine .icu-Datei oder \
                 ein Textblock „-----BEGIN ICU PASSPORT-----“; ICU startet ohne den Link."
            );
            None
        }
        arg => arg.map(read_bundle).transpose()?,
    };

    // One client per configuration directory: a second start brings the
    // running one to the front and hands over what it was given, instead of
    // knocking its session off the server (`icu_client::instance`).
    #[cfg(feature = "gui")]
    if !cli.headless {
        use icu_client::instance::{self, Claim, Request};
        let request = Request {
            bundle: bundle.clone(),
        };
        match instance::claim(&paths, &request)? {
            Claim::First(listener) => instance::install(listener),
            Claim::Handed => {
                eprintln!("icu: ICU läuft bereits; das Fenster wurde nach vorn geholt.");
                return Ok(());
            }
            Claim::Unguarded(why) => tracing::info!(%why, "no single-instance guard"),
        }
    }

    // Joined through the window: the main window follows on screen, even
    // with „Minimiert starten“.
    let mut show = false;
    if let Some(text) = &bundle {
        if in_window(&cli) {
            // A double-click, a file manager: no terminal to ask on. The
            // first-start window shows where it leads, asks for a name where
            // one is needed, and replaces an existing identity only on
            // „Diesem Server beitreten“. Closed without joining: nothing
            // changed, and nothing starts either.
            #[cfg(feature = "gui")]
            {
                let joined = icu_client::ui::run_enroll_window_with(
                    paths.clone(),
                    cli.server_addr,
                    text.clone(),
                )?;
                if joined.is_none() {
                    return Ok(());
                }
                show = true;
            }
        } else {
            import_in_terminal(&cli, &paths, text)?;
        }
    }

    if settings.ui.register_uri_handler && !cli.no_uri_handler && !cli.headless {
        match icu_client::uri::register() {
            Ok(p) => tracing::info!(path = %p.display(), "ICU opens invites and passports"),
            Err(e) => tracing::info!(%e, "file association not registered"),
        }
    }

    let mut logged_out = false;
    loop {
        let loaded = Identity::load(&paths);
        // A key this account can no longer unseal would end every start
        // before a window opens — no way to log out or take a new invite.
        // Asked once; „Ja“ removes it and the invite window follows.
        #[cfg(windows)]
        let loaded = match loaded {
            Err(e) if key_unsealable(&e) && !cli.headless => {
                tracing::error!(error = %e, "the stored key does not unseal for this account");
                if !confirm_forget_identity() {
                    tracing::info!("stored credentials kept, quitting");
                    return Ok(());
                }
                Identity::remove(&paths).map_err(|e| e.to_string())?;
                tracing::warn!("stored credentials removed, asking for an invite");
                Ok(None)
            }
            other => other,
        };
        let identity = match loaded.map_err(|e| e.to_string())? {
            Some(id) => id,
            None => {
                #[cfg(feature = "gui")]
                let enrolled = if cli.headless {
                    None
                } else {
                    icu_client::ui::run_enroll_window(paths.clone(), cli.server_addr)?
                };
                #[cfg(not(feature = "gui"))]
                let enrolled: Option<Identity> = None;
                match enrolled {
                    Some(id) => id,
                    // The invite window was closed: the user's answer, not
                    // an error to report.
                    None if logged_out || (cfg!(feature = "gui") && !cli.headless) => return Ok(()),
                    None => {
                        return Err(format!(
                        "not registered: run `icu <bundle> --name <name>` first (config dir {})",
                        paths.config_dir.display()
                    ))
                    }
                }
            }
        };

        // Without an audio system ICU still runs, silent — and says so in the
        // window, not only in the log.
        let mut audio_problem = None;
        let backend: Box<dyn icu_client::audio::AudioBackend> = if cli.fake_audio {
            Box::new(icu_client::audio::fake::FakeBackend::silent().0)
        } else {
            match icu_client::audio::native_backend() {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(%e, "no audio backend, using the silent fake backend");
                    audio_problem = Some(icu_client::problem::no_audio_system(&e));
                    Box::new(icu_client::audio::fake::FakeBackend::silent().0)
                }
            }
        };

        let (mut settings, problem) =
            Settings::load_or_recover(&paths).map_err(|e| e.to_string())?;
        if let Some(cam) = &cli.camera {
            settings.camera.device = Some(cam.clone());
            settings.camera.enabled = true;
        }
        let mut cfg = EngineConfig::new(identity, settings.clone(), paths.clone());
        cfg.server_addr = cli.server_addr;
        cfg.test_tone = cli.test_tone;
        cfg.banner = audio_problem
            .or_else(|| settings_problem.take())
            .or(problem);
        let engine = Engine::start(cfg, backend).map_err(|e| e.to_string())?;
        // From here a SIGTERM (a desktop logging out) quits like „Beenden“,
        // with a proper goodbye to the server.
        icu_client::signals::listen();

        // Hotkeys come up in the background: the Portal may show a dialog and
        // the window must not wait for it. The supervisor lives on the
        // runtime and follows the settings (a changed binding applies
        // without a restart).
        if !cli.no_hotkeys {
            // Until the first backend reports — on KDE, until its dialog is
            // answered — the window says they are coming, not that they
            // are off. Before the spawn, so the report cannot overtake it.
            engine.command(icu_client::engine::Command::HotkeyBackend(
                icu_client::hotkey::BACKEND_STARTING.into(),
            ));
            engine.runtime().spawn(icu_client::hotkey::supervise(
                engine.state(),
                engine.command_sender(),
            ));
        }

        if cli.headless {
            return run_headless(engine, cli.debug_stats);
        }
        #[cfg(feature = "gui")]
        match icu_client::ui::run_main_window(engine, cli.debug_stats, logged_out || show)? {
            icu_client::ui::Exit::Quit => return Ok(()),
            // Back to the invite window; closing that one quits.
            icu_client::ui::Exit::LoggedOut => logged_out = true,
        }
        #[cfg(not(feature = "gui"))]
        {
            tracing::warn!("built without feature `gui`, running headless");
            return run_headless(engine, cli.debug_stats);
        }
    }
}

fn run_headless(engine: Engine, debug_stats: bool) -> Result<(), String> {
    // The camera follows `camera_on` like in the window (a hotkey may switch
    // it), retried with backoff when it will not start.
    #[cfg(feature = "camera")]
    let mut camera = {
        if engine.snapshot().settings.camera.enabled {
            engine.command(icu_client::engine::Command::SetCamera(true));
        }
        icu_client::camera::CameraHandle::new(engine.camera_link())
    };
    let stats = engine.stats();
    let mut state = engine.state();
    let mut last_report = std::time::Instant::now();
    let mut last_conn = None;
    while !icu_client::signals::requested() {
        std::thread::sleep(Duration::from_millis(100));
        if state.has_changed().unwrap_or(true) {
            let s = state.borrow_and_update();
            if last_conn.as_ref() != Some(&s.conn) {
                eprintln!("icu: connection {:?}, {} peers", s.conn, s.peers.len());
                last_conn = Some(s.conn.clone());
            }
        }
        // On a copy: opening a camera takes a while, and a held borrow
        // would block the engine's next state update.
        #[cfg(feature = "camera")]
        {
            let snapshot = state.borrow().clone();
            camera.sync(&snapshot);
        }
        if debug_stats && last_report.elapsed() >= Duration::from_secs(2) {
            last_report = std::time::Instant::now();
            let s = state.borrow();
            let peers: Vec<String> = s
                .peers
                .iter()
                .map(|p| format!("{}{}", p.info.name, if p.speaking { "*" } else { "" }))
                .collect();
            eprintln!(
                "{}\n  level={:.1} dBFS vad={:.2} sending={} peers=[{}]",
                stats.report(),
                s.level.rms_db,
                s.vad_prob,
                s.sending,
                peers.join(", ")
            );
        }
    }
    #[cfg(feature = "camera")]
    camera.stop();
    engine.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A passport never comes in as an argument (the key would be in the
    /// process list); a file and standard input (`-`) do.
    #[test]
    fn a_passport_is_taken_from_a_file_or_stdin_not_as_an_argument() {
        const TEXT: &str = "-----BEGIN ICU PASSPORT-----\nAAAA\n-----END ICU PASSPORT-----\n";
        let refused = read_bundle_from(TEXT, std::io::empty()).unwrap_err();
        assert!(refused.contains("icu - <"), "{refused}");
        assert!(read_bundle_from(&format!("  {TEXT}"), std::io::empty()).is_err());

        assert_eq!(read_bundle_from("-", TEXT.as_bytes()).unwrap(), TEXT);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("einladung.icu");
        std::fs::write(&file, TEXT).unwrap();
        assert_eq!(
            read_bundle_from(file.to_str().unwrap(), std::io::empty()).unwrap(),
            TEXT
        );
        let missing = dir.path().join("fehlt.icu");
        assert!(read_bundle_from(missing.to_str().unwrap(), std::io::empty()).is_err());
    }

    /// The log sits in `$XDG_STATE_HOME/icu`, or `~/.local/state/icu` when
    /// that is unset or relative.
    #[cfg(not(windows))]
    #[test]
    fn the_log_goes_to_the_xdg_state_directory() {
        let dir = |xdg: Option<&str>, home: Option<&str>| {
            state_dir(xdg.map(Into::into), home.map(Into::into))
        };
        assert_eq!(
            dir(Some("/var/state"), Some("/home/a")),
            Some(PathBuf::from("/var/state/icu"))
        );
        assert_eq!(
            dir(Some("state"), Some("/home/a")),
            Some(PathBuf::from("/home/a/.local/state/icu"))
        );
        assert_eq!(
            dir(None, Some("/home/a")),
            Some(PathBuf::from("/home/a/.local/state/icu"))
        );
        assert_eq!(dir(None, None), None);
        assert_eq!(dir(None, Some("relative")), None);
    }

    /// The log is appended to; over 1 MiB it moves to `icu.log.1` first,
    /// replacing an older one.
    #[test]
    fn the_log_is_appended_and_rotated_at_one_mebibyte() {
        use std::io::Write;
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("icu");
        let mut log = open_log(&dir).expect("created with its directory");
        log.write_all(b"first\n").unwrap();
        drop(log);
        open_log(&dir).unwrap().write_all(b"second\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("icu.log")).unwrap(),
            "first\nsecond\n"
        );
        assert!(!dir.join("icu.log.1").exists());

        std::fs::write(dir.join("icu.log.1"), "oldest").unwrap();
        std::fs::write(dir.join("icu.log"), vec![b'x'; (1 << 20) + 1]).unwrap();
        open_log(&dir).unwrap().write_all(b"fresh\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("icu.log")).unwrap(),
            "fresh\n"
        );
        assert_eq!(
            std::fs::metadata(dir.join("icu.log.1")).unwrap().len(),
            (1 << 20) + 1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("icu.log"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "only the owner reads the log");
        }
    }

    /// A key sealed for another account (or damaged) is told apart from any
    /// other reason the identity does not load: only that one is offered to
    /// be dropped.
    #[cfg(windows)]
    #[test]
    fn a_key_that_does_not_unseal_is_recognised() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let identity = paths.identity_dir();
        std::fs::create_dir_all(&identity).unwrap();
        std::fs::write(identity.join("server.toml"), "").unwrap();
        let mut sealed = b"ICU-DPAPI/1\n".to_vec();
        sealed.extend([0x01, 0x00, 0x00, 0x00, 0xde, 0xad, 0xbe, 0xef]);
        std::fs::write(identity.join("key.pk8"), sealed).unwrap();
        let e = Identity::load(&paths).unwrap_err();
        assert!(key_unsealable(&e), "{e}");

        std::fs::write(identity.join("key.pk8"), [0x30, 0x03, 1, 2, 3]).unwrap();
        let e = Identity::load(&paths).unwrap_err();
        assert!(
            !key_unsealable(&e),
            "a missing certificate is not DPAPI: {e}"
        );
    }
}
