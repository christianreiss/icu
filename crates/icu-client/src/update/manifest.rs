//! Authenticate exact signed bytes before interpreting any update metadata.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use semver::Version;
use serde::{Deserialize, Serialize};
use ssh_key::{PublicKey, SshSig};
use std::{collections::HashSet, sync::Arc};

pub const MAX_MANIFEST: u64 = 256 * 1024;
pub const MAX_ARTIFACT: u64 = 256 * 1024 * 1024;
const PUBLIC_KEY: &str = include_str!("../../../../packaging/update-signing.pub");
pub const NAMESPACE: &str = "icu-update";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Installation {
    Portable,
    Nsis,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub kind: String,
    pub os: String,
    pub format: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub installation: Option<Installation>,
}

#[derive(Debug, Clone, Deserialize)]
struct Payload {
    schema: u32,
    version: String,
    complete: bool,
    dirty: bool,
    commit: String,
    base: String,
    files: Vec<Artifact>,
}
#[derive(Deserialize)]
struct Envelope {
    authenticated: Authentication,
}
#[derive(Serialize, Deserialize)]
pub struct Authentication {
    pub payload: String,
    pub signature: String,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub version: Version,
    pub files: Vec<Artifact>,
    /// Kept verbatim so the separate process authenticates it independently.
    pub envelope: Arc<Vec<u8>>,
}
impl Release {
    pub fn windows_artifact(
        &self,
        architecture: &str,
        installation: Installation,
    ) -> Result<&Artifact, String> {
        if architecture != "x86_64" {
            return Err("Für diese Windows-Architektur ist kein Update verfügbar.".into());
        }
        let (suffix, format) = match installation {
            Installation::Portable => (".zip", "zip"),
            Installation::Nsis => ("-setup.exe", "setup"),
        };
        let expected = format!("icu-{}-windows-{architecture}{suffix}", self.version);
        let mut matching = self.files.iter().filter(|a| {
            a.kind == "client"
                && a.os == "windows"
                && a.arch == architecture
                && a.installation == Some(installation)
        });
        let selected = matching
            .next()
            .ok_or("Kein passendes Windows-Update veröffentlicht.")?;
        if matching.next().is_some() || selected.name != expected || selected.format != format {
            return Err("Die Update-Datei passt nicht zur Installation.".into());
        }
        Ok(selected)
    }
}

pub fn newer_stable(candidate: &Version, current: &Version) -> bool {
    candidate.pre.is_empty() && candidate.cmp_precedence(current).is_gt()
}

pub fn verify(data: &[u8]) -> Result<Release, String> {
    let key = PublicKey::from_openssh(PUBLIC_KEY)
        .map_err(|_| "Der eingebettete Update-Schlüssel ist ungültig.")?;
    verify_with_key(data, &key)
}
fn verify_with_key(data: &[u8], key: &PublicKey) -> Result<Release, String> {
    if data.len() as u64 > MAX_MANIFEST {
        return Err("Update-Metadaten sind zu groß.".into());
    }
    let envelope: Envelope =
        serde_json::from_slice(data).map_err(|_| "Keine signierten Update-Metadaten verfügbar.")?;
    let signed = envelope.authenticated;
    let payload = STANDARD
        .decode(&signed.payload)
        .map_err(|_| "Ungültige Update-Metadaten.")?;
    let signature =
        SshSig::from_pem(&signed.signature).map_err(|_| "Ungültige Update-Signatur.")?;
    key.verify(NAMESPACE, &payload, &signature)
        .map_err(|_| "Die Echtheit des Updates konnte nicht bestätigt werden.")?;
    let payload: Payload =
        serde_json::from_slice(&payload).map_err(|_| "Ungültige signierte Update-Metadaten.")?;
    let version = Version::parse(&payload.version).map_err(|_| "Ungültige Update-Version.")?;
    if payload.schema != 1
        || !payload.complete
        || payload.dirty
        || !version.pre.is_empty()
        || !version.build.is_empty()
        || payload.base != format!("downloads/{version}/")
        || payload.commit.len() != 40
        || !payload.commit.bytes().all(|b| b.is_ascii_hexdigit())
        || payload.files.is_empty()
        || payload.files.len() > 64
    {
        return Err("Das Update ist nicht vollständig als stabile Version veröffentlicht.".into());
    }
    let mut names = HashSet::new();
    for file in &payload.files {
        if !safe_name(&file.name)
            || !names.insert(&file.name)
            || file.size == 0
            || file.size > MAX_ARTIFACT
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Ungültige Datei in den Update-Metadaten.".into());
        }
    }
    Ok(Release {
        version,
        files: payload.files,
        envelope: Arc::new(data.to_vec()),
    })
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 180
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssh_key::{private::Ed25519Keypair, HashAlg, LineEnding, PrivateKey};
    fn fixture() -> serde_json::Value {
        serde_json::json!({"schema":1,"version":"0.2.0","complete":true,"dirty":false,
            "commit":"a".repeat(40),"base":"downloads/0.2.0/","files":[
                {"name":"icu-0.2.0-windows-x86_64.zip","size":100,"sha256":"a".repeat(64),"kind":"client","os":"windows","format":"zip","arch":"x86_64","installation":"portable"},
                {"name":"icu-0.2.0-windows-x86_64-setup.exe","size":100,"sha256":"b".repeat(64),"kind":"client","os":"windows","format":"setup","arch":"x86_64","installation":"nsis"}]})
    }
    fn signed(payload: serde_json::Value, seed: u8, namespace: &str) -> (Vec<u8>, PublicKey) {
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]));
        let bytes = serde_json::to_vec(&payload).unwrap();
        let signature = key
            .sign(namespace, HashAlg::Sha512, &bytes)
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap();
        (serde_json::to_vec(&serde_json::json!({"authenticated": Authentication { payload:STANDARD.encode(bytes),signature }})).unwrap(), key.public_key().clone())
    }
    #[test]
    fn stable_semantic_precedence_never_downgrades() {
        for (new, old, expected) in [
            ("0.1.10", "0.1.9", true),
            ("0.1.9", "0.1.10", false),
            ("1.0.0-rc.1", "0.9.0", false),
            ("1.0.0", "1.0.0-rc.1", true),
            ("1.0.0+build2", "1.0.0+build1", false),
            ("1.0.0", "1.0.0", false),
        ] {
            assert_eq!(
                newer_stable(&Version::parse(new).unwrap(), &Version::parse(old).unwrap()),
                expected,
                "{old} -> {new}"
            );
        }
    }
    #[test]
    fn chooses_only_exact_architecture_and_installation() {
        let (bytes, key) = signed(fixture(), 7, NAMESPACE);
        let r = verify_with_key(&bytes, &key).unwrap();
        assert!(r
            .windows_artifact("x86_64", Installation::Portable)
            .unwrap()
            .name
            .ends_with(".zip"));
        assert!(r
            .windows_artifact("x86_64", Installation::Nsis)
            .unwrap()
            .name
            .ends_with("-setup.exe"));
        assert!(r
            .windows_artifact("aarch64", Installation::Portable)
            .is_err());
        let mut r = r;
        r.files[0].name = "icu-0.1.0-windows-x86_64.zip".into();
        assert!(r
            .windows_artifact("x86_64", Installation::Portable)
            .is_err());
    }
    #[test]
    fn rejects_tampering_wrong_signer_and_namespace() {
        let (bytes, key) = signed(fixture(), 7, NAMESPACE);
        let (_, wrong) = signed(fixture(), 8, NAMESPACE);
        assert!(verify_with_key(&bytes, &wrong).is_err());
        let (wrong_ns, _) = signed(fixture(), 7, "icu-release");
        assert!(verify_with_key(&wrong_ns, &key).is_err());
        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        envelope["authenticated"]["payload"] = serde_json::json!(STANDARD.encode(b"{}"));
        assert!(verify_with_key(&serde_json::to_vec(&envelope).unwrap(), &key).is_err());
        assert!(verify(br#"{"version":"999.0.0","files":[]}"#).is_err());
    }
    #[test]
    fn signed_but_incomplete_or_unsafe_metadata_is_rejected() {
        for (field, value) in [
            ("complete", serde_json::json!(false)),
            ("dirty", serde_json::json!(true)),
            ("base", serde_json::json!("https://evil.invalid/")),
            ("version", serde_json::json!("0.2.0-beta.1")),
        ] {
            let mut p = fixture();
            p[field] = value;
            let (bytes, key) = signed(p, 7, NAMESPACE);
            assert!(verify_with_key(&bytes, &key).is_err(), "{field}");
        }
        for name in [
            "../icu.exe",
            "C:\\icu.exe",
            "https://evil.invalid/icu.exe",
            "icu.exe:stream",
            ".hidden",
        ] {
            let mut p = fixture();
            p["files"][0]["name"] = name.into();
            let (bytes, key) = signed(p, 7, NAMESPACE);
            assert!(verify_with_key(&bytes, &key).is_err(), "{name}");
        }
    }
}
