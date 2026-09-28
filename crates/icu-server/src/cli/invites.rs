//! `icu-server invites`

use crate::{admin::AdminUser, cli, config::Config, display::Style, error::Result, AdminRequest};

/// Prints the open-invitations table: not yet confirmed, not revoked, not
/// expired (`UserRecord::is_open_invite`).
pub async fn run(config: &Config, json: bool) -> Result<()> {
    let mut value = cli::dispatch(config, AdminRequest::Invites).await?;
    cli::require_ok(&value)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    // The socket answer is a serialised `Vec<AdminUser>`; read it back as one.
    let invites: Vec<AdminUser> = serde_json::from_value(value["invites"].take())?;
    print!("{}", crate::display::invites(Style::current(), &invites));
    Ok(())
}
