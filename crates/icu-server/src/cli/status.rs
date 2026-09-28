//! `icu-server status`

use crate::{cli, config::Config, display::Style, error::Result, AdminRequest};

/// Prints uptime, online count and relay counters. `--json` is what a Checkmk
/// local check reads (§7).
pub async fn run(config: &Config, json: bool) -> Result<()> {
    let value = cli::dispatch(config, AdminRequest::Status).await?;
    cli::require_ok(&value)?;
    if json {
        println!("{}", serde_json::to_string(&value)?);
        return Ok(());
    }
    print!("{}", crate::display::status(Style::current(), &value));
    Ok(())
}
