//! `icu-server demote <name|user_id|fingerprint>`

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Revokes admin rights. Rejected with `LAST_ADMIN` when `user` is the sole
/// remaining functional admin.
pub async fn run(config: &Config, user: &str) -> Result<()> {
    let value = cli::dispatch(
        config,
        AdminRequest::Demote {
            user: user.to_string(),
        },
    )
    .await?;
    cli::require_ok(&value)?;
    println!("demoted {user}");
    Ok(())
}
