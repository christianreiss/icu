//! `icu-server users`

use crate::{admin::AdminUser, cli, config::Config, display::Style, error::Result, AdminRequest};

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
    print!("{}", crate::display::users(Style::current(), &users));
    Ok(())
}
