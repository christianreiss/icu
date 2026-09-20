//! `icu-server revoke <name|user_id|fingerprint>`

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Revokes every certificate of a user and closes its connections.
pub async fn run(config: &Config, user: &str) -> Result<()> {
    let value = cli::dispatch(
        config,
        AdminRequest::Revoke {
            user: user.to_string(),
        },
    )
    .await?;
    cli::require_ok(&value)?;
    println!(
        "revoked user_id {}, closed {} connection(s)",
        value.get("user_id").and_then(|v| v.as_u64()).unwrap_or(0),
        value.get("closed").and_then(|v| v.as_u64()).unwrap_or(0)
    );
    Ok(())
}
