//! `icu` — the ICU voice client.
//!
//! `icu <bundle> --name <name>` registers (then starts), `icu` starts. `--config-dir`
//! allows two instances on one machine, `--headless` runs without a window,
//! `--debug-stats` prints latency histograms to stderr every 2 s,
//! `--test-tone` replaces the microphone with a synthetic voice.

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
    /// Registration bundle or passport to take over: a file, or the armored
    /// text itself (`-----BEGIN ICU PASSPORT-----`).
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
    /// Configuration directory (default `$XDG_CONFIG_HOME/icu`).
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
    /// Use the silent fake audio backend (no PipeWire needed).
    #[arg(long)]
    fake_audio: bool,
    /// Override the server address (tests: `127.0.0.1:port`).
    #[arg(long)]
    server_addr: Option<std::net::SocketAddr>,
    /// Do not register the `icu://` URI handler.
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
    let cli = Cli::parse();
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

// Without `gui` there is no logout, so nothing loops.
#[cfg_attr(not(feature = "gui"), allow(unused_mut, clippy::never_loop))]
fn run(cli: Cli) -> Result<(), String> {
    let paths = Paths::new(cli.config_dir.clone());
    std::fs::create_dir_all(&paths.config_dir).map_err(|e| e.to_string())?;
    // An unreadable settings.toml does not stop a desktop start: it is moved
    // aside and the window says so.
    let (settings, mut settings_problem) =
        Settings::load_or_recover(&paths).map_err(|e| e.to_string())?;

    if let Some(bundle) = &cli.bundle {
        let text = if icu_proto::passport::Passport::looks_like(bundle) {
            bundle.clone()
        } else {
            std::fs::read_to_string(bundle).map_err(|e| format!("{bundle}: {e}"))?
        };
        let mut identity = Identity::from_passport(&text)?;
        // A passport brings its own CA and address: show where it leads
        // before anything is replaced (`PROTOCOL.md` §2.3.1).
        let current = Identity::load(&paths).ok().flatten();
        let summary = icu_client::config::ImportSummary::of(&identity, current.as_ref());
        for line in summary.lines() {
            eprintln!("icu: {line}");
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
                    &paths,
                ))
                .map_err(|e| e.to_string())?;
        } else {
            identity.save(&paths).map_err(|e| e.to_string())?;
        }
        eprintln!(
            "icu: registered as {} at {}:{}, identity stored in {}",
            identity.server.name,
            identity.server.host,
            identity.server.port,
            paths.identity_dir().display()
        );
    }

    if settings.ui.register_uri_handler && !cli.no_uri_handler && !cli.headless {
        match icu_client::uri::register() {
            Ok(p) => tracing::info!(path = %p.display(), "URI handler registered"),
            Err(e) => tracing::info!(%e, "URI handler not registered"),
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

        // Hotkeys come up in the background: the Portal may show a dialog and
        // the window must not wait for it. The guard lives on the runtime.
        if !cli.no_hotkeys {
            let cmd = engine.command_sender();
            let hk_settings = settings.hotkeys.clone();
            engine.runtime().spawn(async move {
                let hotkeys = icu_client::hotkey::start(&hk_settings, cmd.clone()).await;
                tracing::info!(backend = %hotkeys.backend, "hotkeys");
                let _ = cmd.send(icu_client::engine::Command::HotkeyBackend(
                    hotkeys.backend.clone(),
                ));
                // Keep the guard alive for the lifetime of the runtime.
                std::future::pending::<()>().await;
                drop(hotkeys);
            });
        }

        if cli.headless {
            return run_headless(engine, cli.debug_stats);
        }
        #[cfg(feature = "gui")]
        match icu_client::ui::run_main_window(engine, cli.debug_stats, logged_out)? {
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
    #[cfg(feature = "camera")]
    let mut camera = {
        let snapshot = engine.snapshot();
        let mut handle = icu_client::camera::CameraHandle::new(engine.camera_link());
        if snapshot.settings.camera.enabled {
            engine.command(icu_client::engine::Command::SetCamera(true));
            if let Err(e) = handle.start(snapshot.settings.camera.device.as_deref()) {
                tracing::warn!(%e, "camera");
            }
        }
        handle
    };
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc_handler(move || stop.store(true, std::sync::atomic::Ordering::Relaxed));
    }
    let stats = engine.stats();
    let mut state = engine.state();
    let mut last_report = std::time::Instant::now();
    let mut last_conn = None;
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
        if state.has_changed().unwrap_or(true) {
            let s = state.borrow_and_update();
            if last_conn.as_ref() != Some(&s.conn) {
                eprintln!("icu: connection {:?}, {} peers", s.conn, s.peers.len());
                last_conn = Some(s.conn.clone());
            }
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

fn ctrlc_handler(f: impl Fn() + Send + 'static) -> Result<(), String> {
    // A tiny runtime just for the signal, so the main thread can poll.
    std::thread::Builder::new()
        .name("icu-signal".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("signal runtime");
            rt.block_on(async {
                let _ = tokio::signal::ctrl_c().await;
                f();
            });
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}
