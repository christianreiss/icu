//! Thin clap wrapper around [`icu_server`] (`docs/reference/server.md` §5).
//!
//! One binary, two names (§5.1): as `icu-server` it is everything below, as
//! `icu-cli` — a symlink the package puts beside it — only the admin side,
//! and without a command the interactive shell. `argv[0]` is all that tells
//! the two apart.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use icu_server::{cli, config::Config, display, error::Result, AdminRequest};

/// ICU voice server.
#[derive(Debug, Parser)]
#[command(name = "icu-server", version, about, long_about = None)]
struct Cli {
    /// Path of the configuration file.
    #[arg(long, global = true, default_value = "/etc/icu/server.toml")]
    config: PathBuf,

    /// Color for human-readable output (JSON is always unstyled).
    #[arg(long, global = true, value_enum, default_value_t)]
    color: display::ColorChoice,

    #[command(subcommand)]
    command: Command,
}

/// Everything `icu-server` takes. Flat: the flattened variant is spliced in
/// where it stands, so the help stays the single list it always was.
#[derive(Debug, Subcommand)]
enum Command {
    /// Start the server. Creates CA and server certificate on first run.
    Run,
    #[command(flatten)]
    Admin(AdminCommand),
    /// Put a snapshot back while the server is stopped, then issue a new
    /// server certificate. The state it replaces is kept as a snapshot.
    Restore {
        /// Snapshot directory, or `latest` for the newest one.
        snapshot: String,
    },
    /// Interactive first-time bootstrap: prompts for server_name,
    /// public_host, the UDP port, max_users and admin_socket_group, writes
    /// the config, and can issue the first admin invite. Works on a config
    /// file that does not exist yet.
    Setup,
}

/// The admin side, the part `icu-cli` carries too: every command that reaches
/// a running server over the admin socket, plus the shell that repeats them.
#[derive(Debug, Subcommand)]
enum AdminCommand {
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
    /// List open invitations: not yet confirmed, not revoked, not expired.
    Invites {
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
    /// Cancel a pending invitation. Refuses if the selector does not
    /// currently name an open invite (a confirmed user, already revoked, or
    /// expired) — unlike `revoke`, a typo'd selector here can never fall back
    /// to revoking a real member.
    CancelInvite {
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
    /// Take a snapshot of the state now. The server also takes one after
    /// every change by itself (`[backup]` in the config).
    Backup {
        /// List the snapshots instead.
        #[arg(long)]
        list: bool,
    },
    /// Cisco-style interactive admin shell: persistent prompt, TAB
    /// completion, `?` for context help. `run`, `restore` and `setup` are
    /// not available here.
    Shell,
    /// Read-only diagnosis of configuration, state, certificates, admin
    /// socket, listen ports and snapshots. Works before the first start,
    /// against a running server, and as a member of the socket group; it
    /// never writes and never takes the state lock.
    Doctor {
        /// Print raw JSON.
        #[arg(long)]
        json: bool,
    },
}

/// `icu-cli`: the admin half of `icu-server` under a shorter name.
#[derive(Debug, Parser)]
#[command(
    name = "icu-cli",
    version,
    about = "ICU admin CLI: the icu-server admin commands and the interactive shell.",
    long_about = None
)]
struct CliAlias {
    /// Path of the configuration file.
    #[arg(long, global = true, default_value = "/etc/icu/server.toml")]
    config: PathBuf,

    /// Color for human-readable output (JSON is always unstyled).
    #[arg(long, global = true, value_enum, default_value_t)]
    color: display::ColorChoice,

    /// Without a command: the interactive admin shell.
    #[command(subcommand)]
    command: Option<CliCommand>,
}

/// The alias' commands. `run`, `restore` and `setup` are in here only so that
/// naming one gets a pointer instead of clap's bare "unrecognized
/// subcommand"; `hide` keeps them out of `icu-cli --help`.
#[derive(Debug, Subcommand)]
enum CliCommand {
    #[command(flatten)]
    Admin(AdminCommand),
    #[command(hide = true)]
    Run,
    #[command(hide = true)]
    Restore {
        #[arg(hide = true)]
        snapshot: Option<String>,
    },
    #[command(hide = true)]
    Setup,
}

/// Which name the binary was invoked under (`icu-cli` is a symlink to
/// `icu-server`, so nothing but `argv[0]` distinguishes them).
fn is_cli_name(arg0: &OsStr) -> bool {
    Path::new(arg0)
        .file_stem()
        .is_some_and(|stem| stem == "icu-cli")
}

/// `run`, `restore` and `setup` stay `icu-server`-only (§5.1): the first *is*
/// the service, the second needs it stopped and takes `flock` itself, which a
/// session on the same socket cannot coexist with, and the third configures
/// the host before there is a socket at all.
fn server_only(verb: &str, usage: &str) -> ExitCode {
    eprintln!("icu-cli: `{verb}` is only available as `icu-server {usage}`");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let alias = std::env::args_os()
        .next()
        .is_some_and(|arg0| is_cli_name(&arg0));
    let prog = if alias { "icu-cli" } else { "icu-server" };
    let (config_path, command, color) = if alias {
        let cli = CliAlias::parse();
        let command = match cli.command {
            None => Command::Admin(AdminCommand::Shell),
            Some(CliCommand::Admin(command)) => Command::Admin(command),
            Some(CliCommand::Run) => return server_only("run", "run"),
            Some(CliCommand::Restore { .. }) => {
                return server_only("restore", "restore <snapshot>")
            }
            Some(CliCommand::Setup) => return server_only("setup", "setup"),
        };
        (cli.config, command, cli.color)
    } else {
        let cli = Cli::parse();
        (cli.config, cli.command, cli.color)
    };
    display::configure(color);

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("{prog}: cannot start the tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(&config_path, command)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{prog}: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(config_path: &Path, command: Command) -> Result<()> {
    // `setup` may be writing the config file for the first time, and
    // `doctor` is the one command you run *because* the configuration does
    // not parse. Both would be lost to the shared read below, which ends the
    // process before the command ever starts.
    if let Command::Setup = command {
        return cli::setup::run(config_path).await;
    }
    if let Command::Admin(AdminCommand::Doctor { json }) = command {
        return cli::doctor::run(config_path, json).await;
    }
    // Resolved and validated only where it is used: `run` here, the offline
    // CLI in `cli::dispatch`. A command that reaches the running server needs
    // nothing but the socket path.
    let config = Config::read(config_path)?;
    match command {
        Command::Run => {
            let config = config.resolve()?;
            icu_server::init_tracing(&config.log);
            let handle = icu_server::run(config).await?;
            wait_for_signal().await;
            tracing::info!("shutting down");
            handle.shutdown().await;
            Ok(())
        }
        // Never through the socket or the offline server: that would open,
        // and could write, the very state being replaced.
        Command::Restore { snapshot } => cli::backup::restore(&config, &snapshot).await,
        Command::Admin(command) => run_admin(config_path, &config, command).await,
        Command::Setup => unreachable!("handled above"),
    }
}

async fn run_admin(config_path: &Path, config: &Config, command: AdminCommand) -> Result<()> {
    match command {
        AdminCommand::Invite { name, ttl, out } => {
            cli::invite::run(config, name.as_deref(), &ttl, out.as_deref()).await
        }
        AdminCommand::Users { json } => cli::users::run(config, json).await,
        AdminCommand::Invites { json } => cli::invites::run(config, json).await,
        AdminCommand::Revoke { user } => {
            let request = AdminRequest::Revoke { user: user.clone() };
            cli::with_closed(config, request, &user, "revoked").await
        }
        AdminCommand::CancelInvite { user } => {
            let request = AdminRequest::CancelInvite { user: user.clone() };
            cli::with_closed(config, request, &user, "cancelled the invitation for").await
        }
        AdminCommand::Promote { user } => {
            let request = AdminRequest::Promote { user: user.clone() };
            cli::for_user(config, request, &user, "promoted", " to admin").await
        }
        AdminCommand::Demote { user } => {
            let request = AdminRequest::Demote { user: user.clone() };
            cli::for_user(config, request, &user, "demoted", "").await
        }
        AdminCommand::Disable { user } => {
            let request = AdminRequest::Disable { user: user.clone() };
            cli::with_closed(config, request, &user, "disabled").await
        }
        AdminCommand::Enable { user } => {
            let request = AdminRequest::Enable { user: user.clone() };
            cli::for_user(config, request, &user, "enabled", "").await
        }
        AdminCommand::Kick { user } => {
            let request = AdminRequest::Kick { user: user.clone() };
            cli::with_closed(config, request, &user, "kicked").await
        }
        AdminCommand::Mute { user } => {
            let request = AdminRequest::Mute { user: user.clone() };
            cli::for_user(config, request, &user, "muted", "").await
        }
        AdminCommand::Unmute { user } => {
            let request = AdminRequest::Unmute { user: user.clone() };
            cli::for_user(config, request, &user, "unmuted", "").await
        }
        AdminCommand::Deafen { user } => {
            let request = AdminRequest::Deafen { user: user.clone() };
            cli::for_user(config, request, &user, "deafened", "").await
        }
        AdminCommand::Undeafen { user } => {
            let request = AdminRequest::Undeafen { user: user.clone() };
            cli::for_user(config, request, &user, "undeafened", "").await
        }
        AdminCommand::Status { json } => cli::status::run(config, json).await,
        AdminCommand::RotateServerCert => {
            cli::simple(
                config,
                AdminRequest::RotateServerCert,
                "new server certificate issued and in use",
            )
            .await
        }
        AdminCommand::RenewCa => {
            cli::simple(
                config,
                AdminRequest::RenewCa,
                "CA certificate re-issued under the same key; clients need nothing",
            )
            .await
        }
        AdminCommand::Backup { list } => cli::backup::run(config, list).await,
        AdminCommand::Shell => icu_server::shell::run(config_path, config).await,
        AdminCommand::Doctor { .. } => unreachable!("handled above"),
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

#[cfg(test)]
mod tests {
    use super::is_cli_name;
    use std::ffi::OsStr;

    #[test]
    fn the_alias_is_recognised_by_argv0_alone() {
        for name in ["icu-cli", "./icu-cli", "/usr/bin/icu-cli"] {
            assert!(is_cli_name(OsStr::new(name)), "{name}");
        }
        for name in ["icu-server", "/usr/bin/icu-server", "icu", "icu-client"] {
            assert!(!is_cli_name(OsStr::new(name)), "{name}");
        }
    }
}
