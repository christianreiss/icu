//! `icu-server backup [--list]`, `icu-server restore <snapshot|latest>`
//!
//! `backup` takes a snapshot of the state now, through the running server or,
//! with none running, directly; `--list` shows the snapshots there are.
//! `restore` works only while no server runs (`crate::backup::restore`).

use crate::{backup, cli, config::Config, error::Result, AdminRequest};

/// Takes a snapshot and prints where it went, or lists the snapshots.
pub async fn run(config: &Config, list: bool) -> Result<()> {
    if list {
        return print_list(config);
    }
    let value = cli::dispatch(config, AdminRequest::Backup).await?;
    cli::require_ok(&value)?;
    println!(
        "state backed up to {}",
        value.get("path").and_then(|v| v.as_str()).unwrap_or("?")
    );
    Ok(())
}

fn print_list(config: &Config) -> Result<()> {
    let dir = config.backup_dir();
    let snapshots = backup::list(&dir)?;
    if snapshots.is_empty() {
        println!("no snapshots in {}", dir.display());
        return Ok(());
    }
    println!(
        "{:<20} {:<21} {:<12} {:>5}  version",
        "snapshot", "created", "reason", "users"
    );
    for path in snapshots {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        match backup::read_manifest(&path) {
            Ok(m) => println!(
                "{name:<20} {:<21} {:<12} {:>5}  {}",
                m.created, m.reason, m.users, m.server_version
            ),
            Err(e) => println!("{name:<20} (unreadable: {e})"),
        }
    }
    println!("in {}", dir.display());
    Ok(())
}

/// Restores a snapshot and says what happened and what is next.
pub async fn restore(config: &Config, source: &str) -> Result<()> {
    let done = backup::restore(config, source).await?;
    match &done.manifest {
        Some(m) => println!(
            "restored {} (taken {}, {}, icu-server {})",
            done.source.display(),
            m.created,
            m.reason,
            m.server_version
        ),
        None => println!(
            "restored {} (no MANIFEST: a copy made by hand, loaded and checked)",
            done.source.display()
        ),
    }
    println!(
        "{} user(s) on record, new server certificate issued",
        done.users
    );
    if let Some(pre) = &done.pre_restore {
        println!("the state it replaced is in {}", pre.display());
    }
    println!("start the server now: systemctl start icu-server");
    Ok(())
}
