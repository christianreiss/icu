//! `icu-server disable <name|user_id|fingerprint>`

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Reversibly blocks login without touching certificates, and closes any
/// live connection immediately.
pub async fn run(config: &Config, user: &str) -> Result<()> {
    let value = cli::dispatch(
        config,
        AdminRequest::Disable {
            user: user.to_string(),
        },
    )
    .await?;
    cli::require_ok(&value)?;
    println!(
        "disabled user_id {}, closed {} connection(s)",
        value.get("user_id").and_then(|v| v.as_u64()).unwrap_or(0),
        value.get("closed").and_then(|v| v.as_u64()).unwrap_or(0)
    );
    Ok(())
}
