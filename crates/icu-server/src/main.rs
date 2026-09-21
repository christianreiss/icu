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
    /// Create an invite token and print the link.
    Invite {
        /// Display name of the invited user.
        #[arg(long)]
        name: String,
        /// Lifetime, e.g. `24h`, `30m`, `7d`.
        #[arg(long, default_value = "24h")]
        ttl: String,
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
    /// Change a display name.
    Rename {
        /// `user_id` or current name.
        user: String,
        /// The new name.
        new: String,
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
        Command::Invite { name, ttl } => cli::invite::run(&config, &name, &ttl).await,
        Command::Users { json } => cli::users::run(&config, json).await,
        Command::Revoke { user } => cli::revoke::run(&config, &user).await,
        Command::Rename { user, new } => cli::rename::run(&config, &user, &new).await,
        Command::Promote { user } => cli::promote::run(&config, &user).await,
        Command::Demote { user } => cli::demote::run(&config, &user).await,
        Command::Disable { user } => cli::disable::run(&config, &user).await,
        Command::Enable { user } => cli::enable::run(&config, &user).await,
        Command::Status { json } => cli::status::run(&config, json).await,
        Command::RotateServerCert => {
            let value = cli::dispatch(&config, AdminRequest::RotateServerCert).await?;
            cli::require_ok(&value)?;
            println!("new server certificate issued; restart the server to present it");
            Ok(())
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
