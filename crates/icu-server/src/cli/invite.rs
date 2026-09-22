//! `icu-server invite [--name alice] [--ttl 24h]`
//!
//! `--name` only prefills the invited user's name dialog; the name the
//! certificate carries is the one that user picks (`PROTOCOL.md` §2.3).

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Creates an invite token and prints the link.
pub async fn run(config: &Config, name: Option<&str>, ttl: &str) -> Result<()> {
    let request = AdminRequest::Invite {
        name: name.unwrap_or_default().to_string(),
        ttl_s: Some(parse_duration(ttl)?),
    };
    let value = cli::dispatch(config, request).await?;
    cli::require_ok(&value)?;
    println!(
        "{}",
        value
            .get("link")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
    );
    eprintln!(
        "token {}, valid until {}",
        value.get("token").and_then(|v| v.as_str()).unwrap_or("?"),
        value.get("expires").and_then(|v| v.as_str()).unwrap_or("?")
    );
    Ok(())
}

/// Parses `90s`, `30m`, `24h`, `7d` or a bare number of seconds.
pub fn parse_duration(value: &str) -> Result<u64> {
    let value = value.trim();
    let (digits, factor) = match value.chars().last() {
        Some('s') => (&value[..value.len() - 1], 1),
        Some('m') => (&value[..value.len() - 1], 60),
        Some('h') => (&value[..value.len() - 1], 3600),
        Some('d') => (&value[..value.len() - 1], 86400),
        _ => (value, 1),
    };
    let number: u64 = digits
        .parse()
        .map_err(|_| crate::Error::msg(format!("cannot parse duration {value:?}")))?;
    Ok(number * factor)
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("24h").unwrap(), 86400);
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        assert_eq!(parse_duration("90s").unwrap(), 90);
        assert_eq!(parse_duration("7d").unwrap(), 604800);
        assert_eq!(parse_duration("120").unwrap(), 120);
        assert!(parse_duration("soon").is_err());
    }
}
