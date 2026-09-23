//! Thin clap wrapper around [`icu_server`] (`Server/IMPLEMENTATION.md` §5).

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use icu_server::{cli, config::Config, error::Result, AdminRequest};

/// ICU voice server.
#[derive(Debug, Parser)]
#[command(name = "icu-server", version, about, long_about = None)]
struct Cli {
    /// Path of the configuration file.
    #[arg(long, global = true, default_value = "/etc/icu/server.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the server. Creates CA and server certificate on first run.
    Run,
    /// Issue a registration bundle and print it. The first one on an empty
    /// server is for an admin.
    Invite {
        /// Placeholder name until the invited user picks their own.
        #[arg(long)]
        name: Option<String>,
        /// Lifetime, e.g. `24h`, `30m`, `7d` (at most 7 days).
        #[arg(long, default_value = "24h")]
        ttl: String,
        /// Write the bundle to this file (mode 0600) instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// List all users.
    Users {
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Revoke every certificate of a user and kick it.
    Revoke {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Grant admin rights.
    Promote {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Revoke admin rights. Rejected if `user` is the sole remaining admin.
    Demote {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Reversibly block login without touching certificates.
    Disable {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Restore login after `disable`.
    Enable {
        /// `user_id`, name or fingerprint.
        user: String,
    },
    /// Show uptime, online users and relay counters.
    Status {
        /// Print raw JSON, for a Checkmk local check.
        #[arg(long)]
        json: bool,
    },
    /// Issue a fresh server leaf from the same CA.
    RotateServerCert,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("icu-server: cannot start the tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("icu-server: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let config = Config::load(&cli.config)?;
    match cli.command {
        Command::Run => {
            icu_server::init_tracing(&config.log);
            let handle = icu_server::run(config).await?;
            wait_for_signal().await;
            tracing::info!("shutting down");
            handle.shutdown().await;
            Ok(())
        }
        Command::Invite { name, ttl, out } => {
            cli::invite::run(&config, name.as_deref(), &ttl, out.as_deref()).await
        }
        Command::Users { json } => cli::users::run(&config, json).await,
        Command::Revoke { user } => {
            cli::with_closed(&config, AdminRequest::Revoke { user }, "revoked").await
        }
        Command::Promote { user } => {
            let done = format!("promoted {user} to admin");
            cli::simple(&config, AdminRequest::Promote { user }, &done).await
        }
        Command::Demote { user } => {
            let done = format!("demoted {user}");
            cli::simple(&config, AdminRequest::Demote { user }, &done).await
        }
        Command::Disable { user } => {
            cli::with_closed(&config, AdminRequest::Disable { user }, "disabled").await
        }
        Command::Enable { user } => {
            let done = format!("enabled {user}");
            cli::simple(&config, AdminRequest::Enable { user }, &done).await
        }
        Command::Status { json } => cli::status::run(&config, json).await,
        Command::RotateServerCert => {
            cli::simple(
                &config,
                AdminRequest::RotateServerCert,
                "new server certificate issued; restart the server to present it",
            )
            .await
        }
    }
}

/// SIGTERM or Ctrl-C (§6.1.8).
async fn wait_for_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(term) => term,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
