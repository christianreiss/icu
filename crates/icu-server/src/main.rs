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
    /// Issue a registration bundle and print it. While no admin has
    /// registered, whoever completes a registration first becomes admin.
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
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
        user: String,
    },
    /// Grant admin rights.
    Promote {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
        user: String,
    },
    /// Revoke admin rights. Rejected if `user` is the sole remaining admin.
    Demote {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
        user: String,
    },
    /// Reversibly block login without touching certificates.
    Disable {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
        user: String,
    },
    /// Restore login after `disable`.
    Enable {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
        user: String,
    },
    /// Close a user's live connections; they may connect again.
    Kick {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
        user: String,
    },
    /// Enforce mute: the user's microphone stays off until `unmute`.
    Mute {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
        user: String,
    },
    /// Lift an enforced mute.
    Unmute {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
        user: String,
    },
    /// Enforce deafen: the user receives no audio until `undeafen`.
    Deafen {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
        user: String,
    },
    /// Lift an enforced deafen.
    Undeafen {
        /// `user_id`, name or fingerprint; `id:<user_id>` or `name:<name>` where
        /// that is ambiguous.
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
    /// Re-issue the CA certificate under the same key with a fresh validity.
    RenewCa,
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
    // Resolved and validated only where it is used: `run` here, the offline
    // CLI in `cli::dispatch`. A command that reaches the running server needs
    // nothing but the socket path.
    let config = Config::read(&cli.config)?;
    match cli.command {
        Command::Run => {
            let config = config.resolve()?;
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
            let request = AdminRequest::Revoke { user: user.clone() };
            cli::with_closed(&config, request, &user, "revoked").await
        }
        Command::Promote { user } => {
            let request = AdminRequest::Promote { user: user.clone() };
            cli::for_user(&config, request, &user, "promoted", " to admin").await
        }
        Command::Demote { user } => {
            let request = AdminRequest::Demote { user: user.clone() };
            cli::for_user(&config, request, &user, "demoted", "").await
        }
        Command::Disable { user } => {
            let request = AdminRequest::Disable { user: user.clone() };
            cli::with_closed(&config, request, &user, "disabled").await
        }
        Command::Enable { user } => {
            let request = AdminRequest::Enable { user: user.clone() };
            cli::for_user(&config, request, &user, "enabled", "").await
        }
        Command::Kick { user } => {
            let request = AdminRequest::Kick { user: user.clone() };
            cli::with_closed(&config, request, &user, "kicked").await
        }
        Command::Mute { user } => {
            let request = AdminRequest::Mute { user: user.clone() };
            cli::for_user(&config, request, &user, "muted", "").await
        }
        Command::Unmute { user } => {
            let request = AdminRequest::Unmute { user: user.clone() };
            cli::for_user(&config, request, &user, "unmuted", "").await
        }
        Command::Deafen { user } => {
            let request = AdminRequest::Deafen { user: user.clone() };
            cli::for_user(&config, request, &user, "deafened", "").await
        }
        Command::Undeafen { user } => {
            let request = AdminRequest::Undeafen { user: user.clone() };
            cli::for_user(&config, request, &user, "undeafened", "").await
        }
        Command::Status { json } => cli::status::run(&config, json).await,
        Command::RotateServerCert => {
            cli::simple(
                &config,
                AdminRequest::RotateServerCert,
                "new server certificate issued and in use",
            )
            .await
        }
        Command::RenewCa => {
            cli::simple(
                &config,
                AdminRequest::RenewCa,
                "CA certificate re-issued under the same key; clients need nothing",
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
