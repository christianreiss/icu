//! `icu-server status`

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Prints uptime, online count and relay counters. `--json` is what a Checkmk
/// local check reads (§7).
pub async fn run(config: &Config, json: bool) -> Result<()> {
    let value = cli::dispatch(config, AdminRequest::Status).await?;
    cli::require_ok(&value)?;
    if json {
        println!("{}", serde_json::to_string(&value)?);
        return Ok(());
    }
    let get = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    println!("uptime    {} s", get("uptime_s"));
    println!("online    {}/{}", get("online"), get("max_users"));
    println!(
        "connects  {} open, {} in handshake, {} since start",
        get("open_connections"),
        get("handshakes"),
        get("connections")
    );
    if let Some(relay) = value.get("relay") {
        let r = |key: &str| relay.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        println!(
            "datagrams in {} out {}   video in {} out {}   control in {}",
            r("datagrams_in"),
            r("datagrams_out"),
            r("video_frames_in"),
            r("video_frames_out"),
            r("control_in")
        );
        if let Some(dropped) = relay.get("dropped").and_then(|v| v.as_object()) {
            let mut items: Vec<String> = dropped
                .iter()
                .filter(|(_, v)| v.as_u64().unwrap_or(0) > 0)
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            items.sort();
            println!(
                "dropped   {}",
                if items.is_empty() {
                    "none".to_string()
                } else {
                    items.join(" ")
                }
            );
        }
    }
    Ok(())
}
