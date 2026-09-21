//! `icu-server promote <name|user_id|fingerprint>`

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Grants admin rights.
pub async fn run(config: &Config, user: &str) -> Result<()> {
    let value = cli::dispatch(
        config,
        AdminRequest::Promote {
            user: user.to_string(),
        },
    )
    .await?;
    cli::require_ok(&value)?;
    println!("promoted {user} to admin");
    Ok(())
}
