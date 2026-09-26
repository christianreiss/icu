//! `icu` — the ICU voice client.
//!
//! `icu` starts. `icu <file>` opens an invite or passport in the first-start
//! window (with `--name`/`--yes`/`--skip-settings`/`--headless` it is taken
//! on the terminal), then starts. `--config-dir` allows two instances on one machine,
//! `--headless` runs without a window,
//! `--debug-stats` prints latency histograms to stderr every 2 s,
//! `--test-tone` replaces the microphone with a synthetic voice.
//!
//! On Windows `icu.exe` is a GUI program: a double-click opens no console.
//! Started from a terminal it writes to that terminal (`AttachConsole`);
//! otherwise the log goes to `%LOCALAPPDATA%\icu\data\icu.log`.

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
    /// file, or the armored text itself (`-----BEGIN ICU PASSPORT-----`). It
    /// opens in the first-start window, which shows where it leads and joins
    /// on a click; with --yes, --name, --skip-settings or --headless it is
    /// taken on the terminal instead.
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
    #[cfg(windows)]
    if !console {
        if let Some(log) = windows_log_file() {
            init_tracing_to(log);
        } else {
            init_tracing();
        }
    } else {
        init_tracing();
    }
    #[cfg(not(windows))]
    init_tracing();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("icu: {e}");
            ExitCode::FAILURE
        }
    }
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

/// The bundle argument as text: the armored text itself, or a file's
/// contents. A passport is at most 8 KiB decoded; a much larger file is not
/// one and is not read whole.
fn read_bundle(arg: &str) -> Result<String, String> {
    use std::io::Read;
    if icu_proto::passport::Passport::looks_like(arg) {
        return Ok(arg.to_owned());
    }
    let mut text = String::new();
    std::fs::File::open(arg)
        .and_then(|f| f.take(64 * 1024).read_to_string(&mut text))
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
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: a plain Win32 call; fails harmlessly without a parent console.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok()
}

/// `%LOCALAPPDATA%\icu\data\icu.log`, appended to: a second start (which
/// hands over to the running ICU and ends) must not cut the running one's
/// log. Over 1 MiB it moves to `icu.log.1` first.
#[cfg(windows)]
fn windows_log_file() -> Option<std::fs::File> {
    let dir = directories::ProjectDirs::from("", "", "icu")?
        .data_local_dir()
        .to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    let log = dir.join("icu.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&log, dir.join("icu.log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .ok()
}

#[cfg(windows)]
fn init_tracing_to(file: std::fs::File) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing::subscriber::set_global_default(
        fmt::Subscriber::builder()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .finish(),
    );
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing::subscriber::set_global_default(
        fmt::Subscriber::builder()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .finish(),
    );
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
        let identity = match Identity::load(&paths).map_err(|e| e.to_string())? {
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
                    None if logged_out => return Ok(()),
                    None => {
                        return Err(format!(
                        "not registered: run `icu <bundle> --name <name>` first (config dir {})",
                        paths.config_dir.display()
                    ))
                    }
                }
            }
        };

        let backend: Box<dyn icu_client::audio::AudioBackend> = if cli.fake_audio {
            Box::new(icu_client::audio::fake::FakeBackend::silent().0)
        } else {
            match icu_client::audio::native_backend() {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(%e, "no audio backend, using the silent fake backend");
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
        cfg.banner = settings_problem.take().or(problem);
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
