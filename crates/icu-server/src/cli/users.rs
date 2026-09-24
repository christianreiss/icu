//! `icu-server users`

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Prints the user table.
pub async fn run(config: &Config, json: bool) -> Result<()> {
    let value = cli::dispatch(config, AdminRequest::Users).await?;
    cli::require_ok(&value)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let users = value
        .get("users")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    println!(
        "{:>4}  {:<20} {:<14} {:<21} {:<21} {:<8} {:<6} {:<9} {:<8} online",
        "id", "name", "fingerprint", "issued", "expires", "revoked", "admin", "disabled", "pending"
    );
    for user in users {
        let get = |key: &str| {
            user.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let flag = |key: &str| {
            if user.get(key).and_then(|v| v.as_bool()).unwrap_or(false) {
                "yes"
            } else {
                "no"
            }
        };
        println!(
            "{:>4}  {:<20} {:<14} {:<21} {:<21} {:<8} {:<6} {:<9} {:<8} {}",
            user.get("user_id").and_then(|v| v.as_u64()).unwrap_or(0),
            get("name"),
            get("fingerprint"),
            get("issued"),
            get("expires"),
            flag("revoked"),
            flag("is_admin"),
            flag("disabled"),
            flag("pending"),
            flag("online"),
        );
    }
    Ok(())
}
