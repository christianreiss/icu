//! The admin CLI (`Server/IMPLEMENTATION.md` §5).
//!
//! Every command except `run` talks to the running server over the admin
//! socket, so a change takes effect immediately instead of at the next
//! restart. Only when no server is running — socket missing or connection
//! refused — does the CLI open the state itself, holding
//! `flock(state.lock)` while it does.

pub mod demote;
pub mod disable;
pub mod enable;
pub mod invite;
pub mod promote;
pub mod rename;
pub mod revoke;
pub mod status;
pub mod users;

use crate::{admin, config::Config, error::Result, AdminRequest};

/// Sends a command to the running server, or executes it locally when none is
/// running.
pub async fn dispatch(config: &Config, request: AdminRequest) -> Result<serde_json::Value> {
    let socket = config.admin_socket();
    if socket.exists() {
        match admin::request(&socket, &request).await {
            Ok(value) => return Ok(value),
            Err(e) => {
                // A stale socket file after a crash looks exactly like this.
                tracing::debug!(error = %e, "admin socket unusable, falling back to the state file");
            }
        }
    }
    let server = crate::Server::offline(config.clone())?;
    Ok(admin::handle(&server, request).await.to_json())
}

/// Turns a `{"ok":false,…}` answer into an error.
pub fn require_ok(value: &serde_json::Value) -> Result<()> {
    if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(());
    }
    let error = value
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("ERROR");
    let detail = value
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    Err(crate::Error::msg(format!("{error}: {detail}")))
}
