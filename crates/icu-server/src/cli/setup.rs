//! `icu-server setup` — interactive first-time bootstrap.
//!
//! Walks a fresh (or existing) install through the handful of fields an
//! admin actually needs to set (`server_name`, `public_host`, the UDP port,
//! `max_users`, `admin_socket_group`), writes `server.toml`, and can issue
//! the first admin invite bundle. Everything else — `[log]`, `[backup]`,
//! `[limits]`, `[tls]` — is left at its existing or default value, matching
//! what `packaging/files/server.toml` already treats as advanced.
//!
//! Re-running `setup` on an already-bootstrapped install is not destructive:
//! an existing config's values become the defaults shown at each prompt, so
//! hitting enter through the whole thing writes back what was already there.
//! Nothing beyond `server.toml` is created directly — issuing the first
//! invite (`cli::invite::run`) is what bootstraps the CA, com key and state
//! directory, exactly as it already does when `icu-server invite` runs with
//! no server up.

use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

use icu_proto::limits as pl;

use crate::{
    cli,
    config::{default_server_name, valid_public_host, Config},
    error::Result,
};

/// Runs the interactive wizard against `config_path`, then writes it.
pub async fn run(config_path: &Path) -> Result<()> {
    run_with(
        config_path,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout(),
    )
    .await
}

/// The actual wizard, over injectable streams so it can be tested without a
/// real terminal (a piped, non-tty stdin already works today — see the
/// shell's own binary test — so this gates on nothing but EOF).
pub(crate) async fn run_with(
    config_path: &Path,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<()> {
    let mut config = if config_path.exists() {
        Config::read(config_path)?
    } else {
        Config::default()
    };

    writeln!(out, "icu-server setup — first-time server configuration")?;
    writeln!(out, "Press enter to keep the value shown in [brackets].\n")?;

    let suggested_name = if config.server.server_name.trim().is_empty() {
        default_server_name(&config.paths.state_dir)
            .ok()
            .unwrap_or_default()
    } else {
        config.server.server_name.clone()
    };
    let server_name = prompt(
        out,
        input,
        "DNS name or IP address clients connect to (server_name)",
        &suggested_name,
        |value| {
            rustls::pki_types::ServerName::try_from(value.to_string())
                .map(|_| ())
                .map_err(|_| format!("{value:?} is not a valid DNS name or IP address"))
        },
    )?;

    let current_port = config
        .server
        .listen
        .first()
        .map(|a| a.port())
        .unwrap_or(pl::DEFAULT_PORT);
    let port = prompt(
        out,
        input,
        "UDP port to listen on",
        &current_port.to_string(),
        |value| {
            value
                .parse::<u16>()
                .map(|_| ())
                .map_err(|_| format!("{value:?} is not a valid port"))
        },
    )?
    .parse::<u16>()
    .expect("validated above");

    let suggested_public_host = if config.server.public_host.trim().is_empty() {
        format!("{server_name}:{port}")
    } else {
        config.server.public_host.clone()
    };
    let public_host = prompt(
        out,
        input,
        "host:port clients dial (public_host, baked into every certificate)",
        &suggested_public_host,
        |value| {
            valid_public_host(value)
                .then_some(())
                .ok_or_else(|| format!("{value:?} must be ASCII host:port"))
        },
    )?;

    let max_users = prompt(
        out,
        input,
        "Maximum users in the room (max_users)",
        &config.server.max_users.to_string(),
        |value| {
            let n: u16 = value
                .parse()
                .map_err(|_| format!("{value:?} is not a number"))?;
            if !(1..=pl::MAX_MAX_USERS).contains(&n) {
                return Err(format!("must be 1..={}, got {n}", pl::MAX_MAX_USERS));
            }
            Ok(())
        },
    )?
    .parse::<u16>()
    .expect("validated above");

    let admin_group = prompt(
        out,
        input,
        "Group allowed to reach the admin socket (admin_socket_group)",
        if config.server.admin_socket_group.is_none() {
            "icu"
        } else {
            config.server.admin_socket_group.as_deref().unwrap()
        },
        |_| Ok(()),
    )?;

    config.server.server_name = server_name;
    config.server.public_host = public_host;
    config.server.listen = listen_addrs(port);
    config.server.max_users = max_users;
    config.server.admin_socket_group = if admin_group.trim().is_empty() {
        None
    } else {
        Some(admin_group)
    };

    config.validate()?;
    let reserved = config.reserved_names();
    if !reserved.is_empty() {
        return Err(crate::Error::msg(reserved.join("; ")));
    }

    if let Some(parent) = config_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let text = render(&config);
    std::fs::write(config_path, &text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o644))?;
    }
    writeln!(out, "\nconfig written to {}", config_path.display())?;

    let issue = prompt(
        out,
        input,
        "Issue the first admin invite now? (y/n)",
        "y",
        |value| match value.to_ascii_lowercase().as_str() {
            "y" | "yes" | "n" | "no" => Ok(()),
            _ => Err("answer y or n".to_string()),
        },
    )?;
    if matches!(issue.to_ascii_lowercase().as_str(), "y" | "yes") {
        let ttl = prompt(out, input, "Invite lifetime", "24h", |_| Ok(()))?;
        let out_path = prompt(
            out,
            input,
            "Save the bundle to a file instead of printing it (blank = print)",
            "",
            |_| Ok(()),
        )?;
        let out_path = (!out_path.trim().is_empty()).then(|| PathBuf::from(out_path.trim()));
        cli::invite::run(&config, None, &ttl, out_path.as_deref()).await?;
    }

    writeln!(
        out,
        "\nRemaining steps: sudo systemctl enable --now icu-server, then open the firewall,\n\
         e.g. sudo firewall-cmd --permanent --add-port={port}/udp && sudo firewall-cmd --reload\n\
         (see deploy/README.md)."
    )?;
    Ok(())
}

/// `listen` for `port`: one IPv4 and one IPv6 entry, same as the default and
/// the shipped sample config.
fn listen_addrs(port: u16) -> Vec<std::net::SocketAddr> {
    vec![
        std::net::SocketAddr::from(([0, 0, 0, 0], port)),
        std::net::SocketAddr::from(([0u16; 8], port)),
    ]
}

/// Prints `label [default]: `, reads a line, keeps `default` on empty input,
/// and re-prompts on a validation error (printed as `error: ...`).
fn prompt(
    out: &mut impl Write,
    input: &mut impl BufRead,
    label: &str,
    default: &str,
    validate: impl Fn(&str) -> std::result::Result<(), String>,
) -> Result<String> {
    loop {
        if default.is_empty() {
            write!(out, "{label}: ")?;
        } else {
            write!(out, "{label} [{default}]: ")?;
        }
        out.flush()?;
        let mut line = String::new();
        let n = input.read_line(&mut line)?;
        if n == 0 {
            return Err(crate::Error::msg("setup: unexpected end of input"));
        }
        let value = line.trim();
        let value = if value.is_empty() { default } else { value };
        match validate(value) {
            Ok(()) => return Ok(value.to_string()),
            Err(e) => writeln!(out, "error: {e}")?,
        }
    }
}

/// Renders `config`'s admin-facing fields as a commented `server.toml`, in
/// the same shape as `packaging/files/server.toml`. Deliberately not a raw
/// `toml::to_string(config)`: that would also dump `[paths]`/`[timing]`,
/// which exist only for tests and out-of-tree runs.
fn render(config: &Config) -> String {
    // Each entry needs its own TOML string quotes, e.g. `"0.0.0.0:4711"`.
    let listen = config
        .server
        .listen
        .iter()
        .map(|a| format!("{:?}", a.to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    let admin_group = config.server.admin_socket_group.as_deref().unwrap_or("icu");
    format!(
        "# ICU server configuration, written by `icu-server setup`.\n\
         # Reference: /usr/share/doc/icu-server/README.md\n\
         \n\
         [server]\n\
         listen = [{listen}]\n\
         server_name = {server_name:?}\n\
         public_host = {public_host:?}\n\
         max_users = {max_users}\n\
         min_client = {min_client:?}\n\
         admin_socket_group = {admin_group:?}\n\
         \n\
         [log]\n\
         level = {log_level:?}\n\
         format = {log_format:?}\n\
         \n\
         [backup]\n\
         keep = {backup_keep}\n",
        server_name = config.server.server_name,
        public_host = config.server.public_host,
        max_users = config.server.max_users,
        min_client = config.server.min_client,
        log_level = config.log.level,
        log_format = config.log.format,
        backup_keep = config.backup.keep,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_addrs_bind_v4_and_v6_on_the_same_port() {
        let addrs = listen_addrs(4712);
        assert_eq!(addrs.len(), 2);
        assert_eq!(addrs[0].port(), 4712);
        assert_eq!(addrs[1].port(), 4712);
        assert!(addrs[0].is_ipv4());
        assert!(addrs[1].is_ipv6());
    }

    #[test]
    fn render_produces_a_config_the_server_accepts() {
        let mut config = Config::default();
        config.server.server_name = "icu.home.arpa".into();
        config.server.public_host = "icu.home.arpa:4711".into();
        let text = render(&config);
        let parsed: Config = toml::from_str(&text).expect("renders valid toml");
        assert_eq!(parsed.server.server_name, "icu.home.arpa");
        assert_eq!(parsed.server.public_host, "icu.home.arpa:4711");
        parsed.validate().expect("renders a valid config");
    }

    #[tokio::test]
    async fn the_wizard_writes_a_valid_config_from_piped_answers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");
        let answers = b"icu.home.arpa\n4711\nicu.home.arpa:4711\n8\nicu\nn\n";
        let mut input = std::io::Cursor::new(answers.to_vec());
        let mut out = Vec::new();
        run_with(&path, &mut input, &mut out).await.unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.server.server_name, "icu.home.arpa");
        assert_eq!(config.server.public_host, "icu.home.arpa:4711");
        assert_eq!(config.server.max_users, 8);
        assert_eq!(config.server.admin_socket_group.as_deref(), Some("icu"));
    }

    #[tokio::test]
    async fn a_reserved_example_name_is_refused_without_writing_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");
        let answers = b"icu.example.org\n4711\nicu.example.org:4711\n8\nicu\nn\n";
        let mut input = std::io::Cursor::new(answers.to_vec());
        let mut out = Vec::new();
        let err = run_with(&path, &mut input, &mut out)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("reserved for examples"), "{err}");
        assert!(!path.exists());
    }
}
