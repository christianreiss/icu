use super::manifest::{self, Release, MAX_MANIFEST};
use std::{io::Read, time::Duration};

fn root() -> Result<&'static str, String> {
    let production = super::RELEASE_INFO.trim_end_matches('/');
    #[cfg(feature = "update-acceptance")]
    let root = option_env!("ICU_UPDATE_ACCEPTANCE_ROOT").unwrap_or(production);
    #[cfg(not(feature = "update-acceptance"))]
    let root = production;
    let uri: ureq::http::Uri = root.parse().map_err(|_| "Ungültiger Update-Kanal.")?;
    if uri.scheme_str() != Some("https")
        || uri.authority().map(|a| a.as_str()) != Some("icu.uggs.io")
        || uri.query().is_some()
        || (uri.path() != "/" && !uri.path().starts_with("/icu"))
        || root.ends_with('/')
    {
        return Err("Ungültiger Update-Kanal.".into());
    }
    Ok(root)
}

#[cfg(all(test, not(feature = "update-acceptance")))]
mod tests {
    #[test]
    fn production_manifest_uses_the_new_domain_root() {
        let root = super::root().unwrap();
        assert_eq!(
            format!("{root}/downloads/latest.json"),
            "https://icu.uggs.io/downloads/latest.json"
        );
    }
}
pub(super) fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .timeout_global(Some(timeout))
        // Per read, not for the body as a whole: `timeout_recv_body` is one
        // budget for the entire download, and 20 s of it ended every update
        // on a line that needs longer for the installer.
        .timeout_per_read(Some(Duration::from_secs(20)))
        .user_agent(concat!("ICU/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}
pub(super) fn check() -> Result<Release, String> {
    let url = format!("{}/downloads/latest.json", root()?);
    let mut response = agent(Duration::from_secs(10))
        .get(&url)
        .header("Accept-Encoding", "identity")
        .call()
        .map_err(|_| {
            "Update-Prüfung momentan nicht möglich. Die Verbindung und ICU laufen weiter."
        })?;
    if response.status() != 200 {
        return Err("Der Update-Server hat keine gültige Antwort geliefert.".into());
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Update-Metadaten konnten nicht vollständig geladen werden.")?;
    manifest::verify(&bytes)
}
#[cfg(windows)]
pub(super) fn artifact_url(
    release: &Release,
    artifact: &manifest::Artifact,
) -> Result<String, String> {
    // Both components were authenticated and validated; metadata never supplies
    // a URL, host, redirect target, executable path or installer argument.
    Ok(format!(
        "{}/downloads/{}/{}",
        root()?,
        release.version,
        artifact.name
    ))
}
