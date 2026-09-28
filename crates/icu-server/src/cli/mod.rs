//! The admin CLI (`Server/IMPLEMENTATION.md` §5).
//!
//! Every command except `run` talks to the running server over the admin
//! socket, so a change takes effect immediately instead of at the next
//! restart. Only when no server is running — socket missing or connection
//! refused — does the CLI open the state itself, holding
//! `flock(state.lock)` while it does.

pub mod backup;
pub mod doctor;
pub mod invite;
pub mod setup;
pub mod status;
pub mod users;

use crate::{admin, config::Config, error::Result, AdminRequest};

/// Sends a command to the running server, or executes it locally when none is
/// running.
///
/// Only a socket that is missing (`NotFound`, also for a missing runtime
/// directory) or refuses the connection (`ConnectionRefused`, the stale file
/// a crash leaves) means "no server". Any other failure — `EACCES` for
/// someone outside the socket's group, a server that hangs up without an
/// answer — is reported as it is: the offline path would only fail later on
/// the state lock or the state directory, and hide the real cause.
pub async fn dispatch(config: &Config, request: AdminRequest) -> Result<serde_json::Value> {
    use std::io::ErrorKind::{ConnectionRefused, NotFound};
    let socket = config.admin_socket();
    match tokio::net::UnixStream::connect(&socket).await {
        Ok(stream) => {
            return admin::exchange(stream, &request)
                .await
                .map_err(|e| e.context(format!("admin socket {}", socket.display())));
        }
        Err(e) if matches!(e.kind(), NotFound | ConnectionRefused) => {
            tracing::debug!(error = %e, "no server on the admin socket, using the state directly");
        }
        Err(e) => {
            return Err(crate::Error::msg(format!(
                "cannot connect to the admin socket {}: {e}. It takes root or \
                 membership in its group (server.admin_socket_group)",
                socket.display()
            )));
        }
    }
    let server = crate::Server::offline(config.clone().resolve()?)?;
    let changes = !matches!(
        request,
        AdminRequest::Users | AdminRequest::Status | AdminRequest::Backup
    );
    let response = admin::handle(&server, request).await.to_json();
    // No server runs to notice the change: the snapshot is taken here.
    if changes && config.backup.keep > 0 {
        if let Err(e) = crate::backup::snapshot(&server, crate::backup::Reason::Change).await {
            eprintln!("icu-server: warning: no snapshot of the changed state: {e}");
        }
    }
    Ok(response)
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

/// Runs an admin command whose only output is a confirmation line.
pub async fn simple(config: &Config, request: AdminRequest, done: &str) -> Result<()> {
    let value = dispatch(config, request).await?;
    require_ok(&value)?;
    println!("{done}");
    Ok(())
}

/// Runs an admin command aimed at one user and says whom the selector hit:
/// `{verb} <name> (user_id <id>){rest}`.
pub async fn for_user(
    config: &Config,
    request: AdminRequest,
    selector: &str,
    verb: &str,
    rest: &str,
) -> Result<()> {
    let value = dispatch(config, request).await?;
    require_ok(&value)?;
    println!("{verb} {}{rest}", who(&value, selector));
    Ok(())
}

/// Runs an admin command that also reports how many live connections it
/// closed (`disable`, `kick`, `revoke`).
pub async fn with_closed(
    config: &Config,
    request: AdminRequest,
    selector: &str,
    verb: &str,
) -> Result<()> {
    let value = dispatch(config, request).await?;
    require_ok(&value)?;
    println!(
        "{verb} {}, closed {} connection(s)",
        who(&value, selector),
        value.get("closed").and_then(|v| v.as_u64()).unwrap_or(0)
    );
    Ok(())
}

/// The user an answer names: name and `user_id`; only the id from a server
/// whose answers do not carry the name yet (an update installed, the old
/// process still running); the selector as typed if neither.
fn who(value: &serde_json::Value, selector: &str) -> String {
    let name = value.get("name").and_then(|v| v.as_str());
    match (name, value.get("user_id").and_then(|v| v.as_u64())) {
        (Some(name), Some(id)) => format!("{name} (user_id {id})"),
        (None, Some(id)) => format!("user_id {id}"),
        _ => selector.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// The confirmation names whom the selector hit, and degrades for an
    /// older server.
    #[test]
    fn the_confirmation_names_the_resolved_user() {
        let new = json!({"ok": true, "user_id": 7, "name": "1", "closed": 0});
        assert_eq!(super::who(&new, "name:1"), "1 (user_id 7)");
        let old = json!({"ok": true, "user_id": 7, "closed": 0});
        assert_eq!(super::who(&old, "1"), "user_id 7");
        assert_eq!(super::who(&json!({"ok": true}), "bob"), "bob");
    }
}
