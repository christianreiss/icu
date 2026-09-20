//! `icu` — the ICU voice client.
//!
//! `icu <icu://link>` enrolls (then starts), `icu` starts. `--config-dir`
//! allows two instances on one machine, `--headless` runs without a window,
//! `--debug-stats` prints latency histograms to stderr every 2 s,
//! `--test-tone` replaces the microphone with a synthetic voice.

use std::{path::PathBuf, process::ExitCode, time::Duration};

use clap::Parser;
use icu_client::{
    config::{Identity, Paths, Settings},
    engine::{Engine, EngineConfig},
    net::enroll::{enroll, InviteLink},
};

/// ICU voice client.
#[derive(Debug, Parser)]
#[command(name = "icu", version, about, long_about = None)]
struct Cli {
    /// Invite link `icu://host:port/enroll?t=…&ca=…&n=…` to enroll with.
    link: Option<String>,
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

fn run(cli: Cli) -> Result<(), String> {
    let paths = Paths::new(cli.config_dir.clone());
    std::fs::create_dir_all(&paths.config_dir).map_err(|e| e.to_string())?;
    let settings = Settings::load(&paths).map_err(|e| e.to_string())?;

    if let Some(link) = &cli.link {
        let link = InviteLink::parse(link).map_err(|e| e.to_string())?;
        let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        let identity = rt
            .block_on(enroll(&link, cli.server_addr))
            .map_err(|e| e.to_string())?;
        identity.save(&paths).map_err(|e| e.to_string())?;
        eprintln!(
            "icu: enrolled as {} at {}:{}, identity stored in {}",
            link.name,
            link.host,
            link.port,
            paths.identity_dir().display()
        );
    }

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
                None => {
                    return Err(format!(
                        "not enrolled: run `icu <icu://link>` first (config dir {})",
                        paths.config_dir.display()
                    ))
                }
            }
        }
    };

    if settings.ui.register_uri_handler && !cli.no_uri_handler && !cli.headless {
        match icu_client::uri::register() {
            Ok(p) => tracing::info!(path = %p.display(), "URI handler registered"),
            Err(e) => tracing::info!(%e, "URI handler not registered"),
        }
    }

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

    let mut settings = settings;
    if let Some(cam) = &cli.camera {
        settings.camera.device = Some(cam.clone());
        settings.camera.enabled = true;
    }
    let mut cfg = EngineConfig::new(identity, settings.clone(), paths.clone());
    cfg.server_addr = cli.server_addr;
    cfg.test_tone = cli.test_tone;
    let engine = Engine::start(cfg, backend).map_err(|e| e.to_string())?;

    let hotkeys = if cli.no_hotkeys {
        None
    } else {
        let cmd = engine.command_sender();
        let hk_settings = settings.hotkeys.clone();
        Some(
            engine
                .runtime()
                .block_on(icu_client::hotkey::start(&hk_settings, cmd)),
        )
    };
    if let Some(h) = &hotkeys {
        tracing::info!(backend = %h.backend, "hotkeys");
    }

    if cli.headless {
        run_headless(engine, cli.debug_stats)
    } else {
        #[cfg(feature = "gui")]
        {
            icu_client::ui::run_main_window(engine, hotkeys, cli.debug_stats)
                .map_err(|e| e.to_string())
        }
        #[cfg(not(feature = "gui"))]
        {
            tracing::warn!("built without feature `gui`, running headless");
            run_headless(engine, cli.debug_stats)
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
        let s = state.borrow_and_update().clone();
        if Some(s.conn.clone()) != last_conn {
            eprintln!("icu: connection {:?}, {} peers", s.conn, s.peers.len());
            last_conn = Some(s.conn.clone());
        }
        if debug_stats && last_report.elapsed() >= Duration::from_secs(2) {
            last_report = std::time::Instant::now();
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
