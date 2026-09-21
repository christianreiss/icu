//! `icu-server enable <name|user_id|fingerprint>`

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Restores login after `disable`, without re-enrollment.
pub async fn run(config: &Config, user: &str) -> Result<()> {
    let value = cli::dispatch(
        config,
        AdminRequest::Enable {
            user: user.to_string(),
        },
    )
    .await?;
    cli::require_ok(&value)?;
    println!("enabled {user}");
    Ok(())
}
