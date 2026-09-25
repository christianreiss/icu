//! `icu-server users`

use crate::{admin::AdminUser, cli, config::Config, error::Result, AdminRequest};

/// Prints the user table.
pub async fn run(config: &Config, json: bool) -> Result<()> {
    let mut value = cli::dispatch(config, AdminRequest::Users).await?;
    cli::require_ok(&value)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    // The socket answer is a serialised `Vec<AdminUser>`; read it back as one.
    let users: Vec<AdminUser> = serde_json::from_value(value["users"].take())?;
    let flag = |b: bool| if b { "yes" } else { "no" };
    println!(
        "{:>4}  {:<20} {:<14} {:<21} {:<21} {:<8} {:<6} {:<9} {:<8} online",
        "id", "name", "fingerprint", "issued", "expires", "revoked", "admin", "disabled", "pending"
    );
    for u in users {
        println!(
            "{:>4}  {:<20} {:<14} {:<21} {:<21} {:<8} {:<6} {:<9} {:<8} {}",
            u.user_id,
            u.name,
            u.fingerprint,
            u.issued,
            u.expires,
            flag(u.revoked),
            flag(u.is_admin),
            flag(u.disabled),
            flag(u.pending),
            flag(u.online),
        );
    }
    Ok(())
}
