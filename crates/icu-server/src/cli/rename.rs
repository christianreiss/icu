//! `icu-server rename <name|user_id> <new>`

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Changes a display name. A case-insensitive collision aborts without a
/// change (§5).
pub async fn run(config: &Config, user: &str, new: &str) -> Result<()> {
    let value = cli::dispatch(
        config,
        AdminRequest::Rename {
            user: user.to_string(),
            new: new.to_string(),
        },
    )
    .await?;
    cli::require_ok(&value)?;
    println!("renamed {user} to {new}");
    Ok(())
}
