//! `icu-server invite [--name neu-bob] [--ttl 24h] [--out bundle.txt]`
//!
//! Issues a registration bundle (`PROTOCOL.md` §2.3): one-time key,
//! short-lived registration certificate and CA, armored as a passport. The
//! invited user imports it and picks their own name; `--name` is only the
//! placeholder until then. Works without a running server too, which is how
//! the first admin gets onto a fresh one.

use crate::{cli, config::Config, error::Result, AdminRequest};

/// Issues a registration bundle and prints it, or writes it to `out`.
pub async fn run(config: &Config, name: Option<&str>, ttl: &str, out: Option<&std::path::Path>) -> Result<()> {
    let request = AdminRequest::Invite {
        name: name.unwrap_or_default().to_string(),
        ttl_s: Some(parse_duration(ttl)?),
    };
    let value = cli::dispatch(config, request).await?;
    cli::require_ok(&value)?;
    let bundle = value
        .get("bundle")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    match out {
        // Mode 0600: the bundle is a bearer credential until it is used.
        Some(path) => crate::store::write_private(path, bundle)?,
        None => print!("{bundle}"),
    }
    eprintln!(
        "registration for {} (user_id {}{}), valid until {}",
        value.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
        value.get("user_id").and_then(|v| v.as_u64()).unwrap_or(0),
        if value.get("is_admin").and_then(|v| v.as_bool()) == Some(true) {
            ", admin"
        } else {
            ""
        },
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
