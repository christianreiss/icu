//! `settings.toml`, the identity directory and the paths per OS
//! (`Client/IMPLEMENTATION.md` §5, §11).

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Where this client keeps its files.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `$XDG_CONFIG_HOME/icu` (or `--config-dir`).
    pub config_dir: PathBuf,
}

impl Paths {
    /// Default paths, or the override from `--config-dir`.
    pub fn new(override_dir: Option<PathBuf>) -> Paths {
        let config_dir = override_dir.unwrap_or_else(|| {
            directories::ProjectDirs::from("", "", "icu")
                .map(|d| d.config_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".icu"))
        });
        Paths { config_dir }
    }

    /// `settings.toml`.
    pub fn settings(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }
    /// `identity/`.
    pub fn identity_dir(&self) -> PathBuf {
        self.config_dir.join("identity")
    }
    /// `identity/key.pk8`.
    pub fn key(&self) -> PathBuf {
        self.identity_dir().join("key.pk8")
    }
    /// `identity/leaf.der`.
    pub fn leaf(&self) -> PathBuf {
        self.identity_dir().join("leaf.der")
    }
    /// `identity/ca.der`.
    pub fn ca(&self) -> PathBuf {
        self.identity_dir().join("ca.der")
    }
    /// `identity/server.toml`.
    pub fn server(&self) -> PathBuf {
        self.identity_dir().join("server.toml")
    }
}

/// Transmission mode as written to the settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModeSetting {
    /// Push-to-talk.
    Ptt,
    /// Voice activation.
    #[default]
    Vad,
}

/// `[audio]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Capture device id, `None` = default.
    pub input_device: Option<String>,
    /// Playback device id, `None` = default.
    pub output_device: Option<String>,
    /// PTT or VAD.
    pub mode: ModeSetting,
    /// RNNoise voice probability threshold.
    pub vad_threshold: f32,
    /// Auto-calibrate the VAD threshold to the ambient noise floor instead
    /// of using `vad_threshold` directly. Requires `denoise`.
    pub vad_auto: bool,
    /// Microphone gain in dB.
    pub gain_db: f32,
    /// RNNoise on.
    pub denoise: bool,
    /// AEC3 on.
    pub aec: bool,
    /// PTT release delay in ms.
    pub release_ms: u32,
    /// VAD hangover in ms.
    pub hangover_ms: u32,
    /// Opus frame length: 20 or 10.
    pub frame_ms: u8,
    /// Join/leave cue.
    pub cue_join: bool,
    /// Transmit start/stop cue.
    pub cue_tx: bool,
    /// Mute/unmute cue.
    pub cue_mute: bool,
    /// Master playback volume in percent.
    pub output_volume: f32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings {
            input_device: None,
            output_device: None,
            mode: ModeSetting::Vad,
            vad_threshold: 0.5,
            vad_auto: true,
            gain_db: 0.0,
            denoise: true,
            aec: true,
            release_ms: 150,
            hangover_ms: 1000,
            frame_ms: 20,
            cue_join: true,
            cue_tx: false,
            cue_mute: true,
            output_volume: 100.0,
        }
    }
}

/// A backend neutral key binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct KeyBinding {
    /// evdev key name (`KEY_F12`, `BTN_SIDE`) or `code:<n>`.
    pub key: String,
    /// Portal trigger description (`CTRL+SHIFT+F12`), optional.
    pub trigger: Option<String>,
}
/// Which hotkey backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HotkeyBackend {
    /// Portal first, evdev if the portal cancels.
    #[default]
    Auto,
    /// Portal only.
    Portal,
    /// evdev only.
    Evdev,
}

/// `[hotkeys]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeySettings {
    /// Push-to-talk.
    pub ptt: KeyBinding,
    /// Mute toggle.
    pub mute: KeyBinding,
    /// Deafen toggle.
    pub deafen: KeyBinding,
    /// PTT/VAD toggle.
    pub toggle_mode: KeyBinding,
    /// Backend selection.
    pub backend: HotkeyBackend,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        HotkeySettings {
            ptt: KeyBinding {
                key: "KEY_RIGHTCTRL".into(),
                trigger: Some("CTRL+SHIFT+F12".into()),
            },
            mute: KeyBinding::default(),
            deafen: KeyBinding::default(),
            toggle_mode: KeyBinding::default(),
            backend: HotkeyBackend::Auto,
        }
    }
}

/// `[camera]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CameraSettings {
    /// Camera index or name.
    pub device: Option<String>,
    /// Start with the camera on.
    pub enabled: bool,
}

/// Overlay screen position: a corner, or a full top/bottom edge (which is
/// always centered by the compositor and always lays tiles out
/// horizontally, regardless of [`Expand`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    /// Top left.
    TopLeft,
    /// Top right.
    #[default]
    TopRight,
    /// Bottom left.
    BottomLeft,
    /// Bottom right.
    BottomRight,
    /// Top edge, centered.
    Top,
    /// Bottom edge, centered.
    Bottom,
}

/// Overlay tile size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TileSize {
    /// 160×120.
    #[default]
    Small,
    /// 320×240.
    Medium,
    /// 480×360.
    Large,
}

/// Direction the tile stack grows in for a corner position. Ignored for
/// [`Corner::Top`] / [`Corner::Bottom`], which are always horizontal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Expand {
    /// Stack downward/upward (one column).
    #[default]
    Vertical,
    /// Stack sideways (one row).
    Horizontal,
}

/// `[overlay]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySettings {
    /// Overlay on.
    pub enabled: bool,
    /// Which corner (or edge).
    pub corner: Corner,
    /// Offset from the corner in pixels.
    pub offset: [u32; 2],
    /// Tile size.
    pub size: TileSize,
    /// Stack direction for corner positions.
    pub expand: Expand,
    /// Dim tiles of peers who aren't currently speaking.
    pub dim_non_speaking: bool,
    /// Draw a highlighted frame around the tile of whoever is speaking.
    pub speaking_frame: bool,
    /// Color of that frame, RGB.
    pub frame_color: [u8; 3],
}

impl Default for OverlaySettings {
    fn default() -> Self {
        OverlaySettings {
            enabled: false,
            corner: Corner::TopRight,
            offset: [40, 40],
            size: TileSize::Small,
            expand: Expand::Vertical,
            dim_non_speaking: true,
            speaking_frame: true,
            frame_color: [0x2e, 0x9b, 0xff],
        }
    }
}

/// `[ui]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// Legacy. The display name lives in the certificate's CN since the name
    /// moved there (`PROTOCOL.md` §2.1); nothing reads this any more. Kept so
    /// an existing `settings.toml` still round-trips.
    pub name: Option<String>,
    /// Start in the tray.
    pub start_minimized: bool,
    /// Register the `icu://` URI handler on start.
    pub register_uri_handler: bool,
    /// Theme: `system`, `light`, `dark`.
    pub theme: String,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings {
            name: None,
            start_minimized: false,
            register_uri_handler: true,
            theme: "system".into(),
        }
    }
}

/// The whole `settings.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    /// `[audio]`
    pub audio: AudioSettings,
    /// `[hotkeys]`
    pub hotkeys: HotkeySettings,
    /// `[camera]`
    pub camera: CameraSettings,
    /// `[overlay]`
    pub overlay: OverlaySettings,
    /// `[volumes]`: `user_id` → percent.
    pub volumes: BTreeMap<String, f32>,
    /// `[ui]`
    pub ui: UiSettings,
}

impl Settings {
    /// Loads the file, or the defaults if it does not exist.
    pub fn load(paths: &Paths) -> Result<Settings, ConfigError> {
        match fs::read_to_string(paths.settings()) {
            Ok(text) => toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    /// Writes the file atomically.
    pub fn save(&self, paths: &Paths) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Parse(e.to_string()))?;
        write_atomic(&paths.settings(), text.as_bytes(), false)
    }

    /// Volume for a peer in percent (default 100).
    pub fn volume(&self, user_id: icu_proto::UserId) -> f32 {
        self.volumes
            .get(&user_id.to_string())
            .copied()
            .unwrap_or(100.0)
    }

    /// Sets a peer volume in percent, clamped to the allowed range, and
    /// returns the value actually stored.
    pub fn set_volume(&mut self, user_id: icu_proto::UserId, percent: f32) -> f32 {
        let percent = percent.clamp(0.0, 200.0);
        if (percent - 100.0).abs() < f32::EPSILON {
            self.volumes.remove(&user_id.to_string());
        } else {
            self.volumes.insert(user_id.to_string(), percent);
        }
        percent
    }
}

/// `identity/server.toml`: where to connect.
///
/// Derived from the certificates wherever they say it (`PROTOCOL.md` §2.3):
/// the leaf's URI SAN `icu://host:port` and the CA's CN `ICU CA <server_name>`.
/// The file keeps it for identities whose certificates predate that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Host to dial.
    pub host: String,
    /// Port to dial.
    pub port: u16,
    /// SNI for the connection.
    pub server_name: String,
    /// Last known display name, mirrored from the certificate's CN at
    /// registration and after every renewal. Informational: the certificate
    /// is the authority (`PROTOCOL.md` §2.1).
    pub name: String,
}

impl ServerInfo {
    /// Where to connect, read from the certificates: host and port from the
    /// leaf's `icu://` URI SAN, SNI from the CA's CN `ICU CA <server_name>`
    /// (the host when the CA predates that), name from the leaf's CN. `None`
    /// when the leaf carries no such URI.
    pub fn from_certs(leaf_der: &[u8], ca_der: &[u8]) -> Option<ServerInfo> {
        use x509_parser::extensions::GeneralName;
        let (_, leaf) = x509_parser::parse_x509_certificate(leaf_der).ok()?;
        let authority = leaf
            .subject_alternative_name()
            .ok()??
            .value
            .general_names
            .iter()
            .find_map(|n| match n {
                GeneralName::URI(u) => u.strip_prefix("icu://").map(str::to_string),
                _ => None,
            })?;
        let (host, port) = crate::net::split_host_port(authority.trim_end_matches('/')).ok()?;
        let name = leaf
            .subject()
            .iter_common_name()
            .next()
            .and_then(|cn| cn.as_str().ok())
            .unwrap_or_default()
            .to_string();
        let server_name = ca_server_name(ca_der).unwrap_or_else(|| host.clone());
        Some(ServerInfo {
            host,
            port,
            server_name,
            name,
        })
    }

    /// Takes over what a renewed leaf says about the server. Host and port
    /// always come from its URI; the SNI only when the CA names one — for an
    /// older CA the stored value is the one known to work, and the URI host
    /// may be an IP or alias the server certificate does not cover.
    pub fn update_from_certs(&mut self, leaf_der: &[u8], ca_der: &[u8]) {
        if let Some(derived) = ServerInfo::from_certs(leaf_der, ca_der) {
            self.host = derived.host;
            self.port = derived.port;
            if ca_server_name(ca_der).is_some() {
                self.server_name = derived.server_name;
            }
        }
    }
}

/// The SNI a CA announces in its CN `ICU CA <server_name>`, if it does.
fn ca_server_name(ca_der: &[u8]) -> Option<String> {
    let (_, ca) = x509_parser::parse_x509_certificate(ca_der).ok()?;
    let name = ca
        .subject()
        .iter_common_name()
        .next()?
        .as_str()
        .ok()?
        .strip_prefix("ICU CA ")?
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// Key, leaf, CA and server coordinates.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    /// PKCS#8 DER private key.
    pub key_der: Vec<u8>,
    /// Client leaf certificate, DER.
    pub leaf_der: Vec<u8>,
    /// Server CA certificate, DER.
    pub ca_der: Vec<u8>,
    /// Server coordinates.
    pub server: ServerInfo,
}

/// Hand written so the private key cannot reach a log line: `Identity` travels
/// inside `Command`, which derives `Debug`.
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field(
                "key_der",
                &format_args!("<redacted, {} bytes>", self.key_der.len()),
            )
            .field("leaf_der", &format_args!("{} bytes", self.leaf_der.len()))
            .field("ca_der", &format_args!("{} bytes", self.ca_der.len()))
            .field("server", &self.server)
            .finish()
    }
}

impl Identity {
    /// Reads a pasted passport or registration bundle.
    ///
    /// Version 2 carries no address of its own, so its certificates must say
    /// where the server is; version 1 brings its fields. Either way the key
    /// has to parse, since the codec only checks the wrapper.
    pub fn from_passport(armored: &str) -> Result<Identity, String> {
        use icu_proto::passport::Passport;
        let passport = Passport::decode(armored).map_err(|e| format!("Passport unlesbar: {e}"))?;
        let identity = match passport {
            Passport::V1(p) => Identity {
                key_der: p.key_der,
                leaf_der: p.leaf_der,
                ca_der: p.ca_der,
                server: ServerInfo {
                    host: p.host,
                    port: p.port,
                    server_name: p.server_name,
                    name: p.name,
                },
            },
            Passport::V2(p) => {
                let server = ServerInfo::from_certs(&p.leaf_der, &p.ca_der).ok_or_else(|| {
                    "Passport enthält keine Serveradresse im Zertifikat.".to_string()
                })?;
                Identity {
                    key_der: p.key_der,
                    leaf_der: p.leaf_der,
                    ca_der: p.ca_der,
                    server,
                }
            }
        };
        if rcgen::KeyPair::try_from(identity.key_der.as_slice()).is_err() {
            return Err("Passport enthält keinen gültigen Schlüssel.".into());
        }
        Ok(identity)
    }

    /// The armored passport for this identity. Version 2 when the
    /// certificates alone lead to the same host, port and SNI this identity
    /// uses, otherwise version 1 with the address spelled out — an older
    /// certificate or CA would send the other device somewhere else.
    pub fn to_passport(&self) -> String {
        use icu_proto::passport::{PassportV1, PassportV2};
        let reproducible = ServerInfo::from_certs(&self.leaf_der, &self.ca_der).is_some_and(|d| {
            d.host == self.server.host
                && d.port == self.server.port
                && d.server_name == self.server.server_name
        });
        if reproducible {
            PassportV2 {
                key_der: self.key_der.clone(),
                leaf_der: self.leaf_der.clone(),
                ca_der: self.ca_der.clone(),
            }
            .encode()
        } else {
            PassportV1 {
                host: self.server.host.clone(),
                port: self.server.port,
                server_name: self.server.server_name.clone(),
                name: self.server.name.clone(),
                key_der: self.key_der.clone(),
                leaf_der: self.leaf_der.clone(),
                ca_der: self.ca_der.clone(),
            }
            .encode()
        }
    }

    /// Whether this is a registration bundle that still has to be swapped for
    /// a certificate of its own (`net::register`).
    pub fn is_registration(&self) -> bool {
        crate::net::register::is_registration(&self.leaf_der)
    }

    /// Loads the identity, `Ok(None)` if not enrolled yet.
    pub fn load(paths: &Paths) -> Result<Option<Identity>, ConfigError> {
        // A save that crashed between its two renames left only the previous
        // set behind (see `save`).
        let previous = paths.identity_dir().with_extension("old");
        if !paths.identity_dir().exists() && previous.join("server.toml").exists() {
            fs::rename(&previous, paths.identity_dir())?;
        }
        if !paths.server().exists() {
            return Ok(None);
        }
        let key_der = fs::read(paths.key())?;
        let leaf_der = fs::read(paths.leaf())?;
        let ca_der = fs::read(paths.ca())?;
        let server: ServerInfo = toml::from_str(&fs::read_to_string(paths.server())?)
            .map_err(|e| ConfigError::Parse(e.to_string()))?;
        Ok(Some(Identity {
            key_der,
            leaf_der,
            ca_der,
            server,
        }))
    }

    /// Writes all four files as one set; the key with mode 0600.
    ///
    /// Into a fresh `identity.new/` first, then swapped in by renaming
    /// directories, so a crash never leaves a key next to somebody else's
    /// certificate. The swap takes two renames (a directory cannot replace a
    /// non-empty one everywhere); `load` repairs a crash between them.
    pub fn save(&self, paths: &Paths) -> Result<(), ConfigError> {
        let dir = paths.identity_dir();
        let fresh = dir.with_extension("new");
        let previous = dir.with_extension("old");
        remove_dir_if_present(&fresh)?;
        fs::create_dir_all(&fresh)?;
        write_atomic(&fresh.join("key.pk8"), &self.key_der, true)?;
        write_atomic(&fresh.join("leaf.der"), &self.leaf_der, false)?;
        write_atomic(&fresh.join("ca.der"), &self.ca_der, false)?;
        let text =
            toml::to_string_pretty(&self.server).map_err(|e| ConfigError::Parse(e.to_string()))?;
        write_atomic(&fresh.join("server.toml"), text.as_bytes(), false)?;

        remove_dir_if_present(&previous)?;
        if dir.exists() {
            fs::rename(&dir, &previous)?;
        }
        fs::rename(&fresh, &dir)?;
        #[cfg(unix)]
        if let Ok(parent) = fs::File::open(&paths.config_dir) {
            let _ = parent.sync_all();
        }
        let _ = remove_dir_if_present(&previous);
        Ok(())
    }
}

/// What importing a passport or registration bundle would switch this client
/// to, shown for the user to confirm before anything is replaced
/// (`PROTOCOL.md` §2.3.1). A passport brings its own CA and address, so a
/// forged one would lead to somebody else's server; this is where the user
/// sees which one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    /// The server's name (TLS SNI).
    pub server_name: String,
    /// `host:port` the client would dial.
    pub address: String,
    /// The own display name in the certificate; for a registration bundle the
    /// placeholder until a name is picked.
    pub name: String,
    /// A registration bundle (invite) rather than a passport.
    pub registration: bool,
    /// An identity exists and trusts a different CA: another server.
    pub ca_changed: bool,
}

impl ImportSummary {
    /// Summarises `identity`, compared with the one in force, if any.
    pub fn of(identity: &Identity, current: Option<&Identity>) -> ImportSummary {
        let host = &identity.server.host;
        let address = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{}", identity.server.port)
        } else {
            format!("{host}:{}", identity.server.port)
        };
        ImportSummary {
            server_name: identity.server.server_name.clone(),
            address,
            name: crate::conninfo::CertInfo::from_der(&identity.leaf_der).subject,
            registration: identity.is_registration(),
            ca_changed: current.is_some_and(|c| c.ca_der != identity.ca_der),
        }
    }

    /// The summary as lines of text, for the CLI and the windows.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Server:  {}", self.server_name),
            format!("Adresse: {}", self.address),
            if self.registration {
                format!("Name:    {} (Einladung, eigener Name folgt)", self.name)
            } else {
                format!("Name:    {}", self.name)
            },
        ];
        if self.ca_changed {
            lines.push(
                "Achtung: anderer Server als bisher — nur übernehmen, wenn du das erwartest."
                    .into(),
            );
        }
        lines
    }
}

fn remove_dir_if_present(dir: &Path) -> Result<(), ConfigError> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Configuration failure.
#[derive(Debug)]
pub enum ConfigError {
    /// File system.
    Io(io::Error),
    /// TOML.
    Parse(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "io: {e}"),
            ConfigError::Parse(e) => write!(f, "parse: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(e: io::Error) -> Self {
        ConfigError::Io(e)
    }
}

/// Saves an invite bundle as `invites/<name>.icu` next to the
/// configuration, readable only by the owner — it lets anybody in. The name
/// is the placeholder in the bundle's certificate, reduced to characters that
/// are safe in a file name.
pub fn save_invite(paths: &Paths, bundle: &str) -> Result<PathBuf, ConfigError> {
    use icu_proto::passport::Passport;
    let leaf_der = match Passport::decode(bundle) {
        Ok(Passport::V1(p)) => p.leaf_der,
        Ok(Passport::V2(p)) => p.leaf_der,
        Err(e) => return Err(ConfigError::Parse(e.to_string())),
    };
    let name: String = x509_parser::parse_x509_certificate(&leaf_der)
        .ok()
        .and_then(|(_, leaf)| {
            leaf.subject()
                .iter_common_name()
                .next()
                .and_then(|cn| cn.as_str().ok().map(str::to_string))
        })
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = if name.trim_matches('_').is_empty() {
        "einladung".to_string()
    } else {
        name
    };
    let path = paths.config_dir.join("invites").join(format!("{name}.icu"));
    write_atomic(&path, bundle.as_bytes(), true)?;
    Ok(path)
}

/// Writes `data` to `path` via a temporary file and rename.
pub fn write_atomic(path: &Path, data: &[u8], private: bool) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if private { 0o600 } else { 0o644 });
        }
        let mut file = options.open(&tmp)?;
        use io::Write;
        file.write_all(data)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        assert_eq!(Settings::load(&paths).unwrap(), Settings::default());

        let mut s = Settings::default();
        s.audio.mode = ModeSetting::Ptt;
        s.audio.vad_threshold = 0.7;
        s.audio.vad_auto = false;
        s.audio.hangover_ms = 1500;
        s.audio.input_device = Some("alsa_input.foo".into());
        s.hotkeys.ptt.key = "BTN_SIDE".into();
        s.overlay.corner = Corner::BottomLeft;
        s.overlay.expand = Expand::Horizontal;
        s.overlay.dim_non_speaking = false;
        s.overlay.size = TileSize::Large;
        s.overlay.speaking_frame = false;
        s.overlay.frame_color = [0x10, 0x20, 0x30];
        s.set_volume(7, 120.0);
        s.set_volume(9, 100.0);
        s.ui.name = Some("chris".into());
        s.save(&paths).unwrap();

        let back = Settings::load(&paths).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.volume(7), 120.0);
        assert_eq!(back.volume(9), 100.0, "100 % is not stored");
        assert!(!back.volumes.contains_key("9"));
        assert!(!back.overlay.dim_non_speaking);
        assert_eq!(back.overlay.size, TileSize::Large);
        assert!(!back.overlay.speaking_frame);
        assert_eq!(back.overlay.frame_color, [0x10, 0x20, 0x30]);

        let text = fs::read_to_string(paths.settings()).unwrap();
        assert!(text.contains("mode = \"ptt\""));
        assert!(text.contains("corner = \"bottom-left\""));
        assert!(text.contains("[volumes]"));
    }

    #[test]
    fn unknown_keys_and_missing_sections_are_tolerated() {
        let s: Settings = toml::from_str("[audio]\nmode = \"vad\"\n[future]\nx = 1\n").unwrap();
        assert_eq!(s.audio.mode, ModeSetting::Vad);
        assert_eq!(s.audio.release_ms, 150);
        assert_eq!(s.audio.hangover_ms, 1000);
        assert!(s.audio.vad_auto);
        assert!(s.overlay.dim_non_speaking, "old settings.toml default on");
    }

    /// A CA named like a current server's and a leaf with the URI SAN, as
    /// `(leaf, ca, key)` DER.
    fn issued(uri: Option<&str>, ca_cn: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, SanType};
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        ca_params.distinguished_name = rcgen::DistinguishedName::new();
        ca_params.distinguished_name.push(DnType::CommonName, ca_cn);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, "alice");
        if let Some(uri) = uri {
            params.subject_alt_names = vec![SanType::URI(uri.to_string().try_into().unwrap())];
        }
        let leaf = params.signed_by(&key, &issuer).unwrap();
        (leaf.der().to_vec(), ca.der().to_vec(), key.serialize_der())
    }

    #[test]
    fn server_info_comes_from_the_certificates() {
        let (leaf, ca, _) = issued(Some("icu://icu.example.org:4711"), "ICU CA sni.example.org");
        let info = ServerInfo::from_certs(&leaf, &ca).unwrap();
        assert_eq!(info.host, "icu.example.org");
        assert_eq!(info.port, 4711);
        assert_eq!(info.server_name, "sni.example.org");
        assert_eq!(info.name, "alice");

        // A CA from before the FQDN moved into its CN: SNI falls back to the
        // host.
        let (leaf, ca, _) = issued(Some("icu://[::1]:5000"), "ICU CA");
        let info = ServerInfo::from_certs(&leaf, &ca).unwrap();
        assert_eq!((info.host.as_str(), info.port), ("::1", 5000));
        assert_eq!(info.server_name, "::1");

        let (leaf, ca, _) = issued(None, "ICU CA x");
        assert!(ServerInfo::from_certs(&leaf, &ca).is_none());
    }

    /// An invite lands under the certificate's name, owner-only.
    #[test]
    fn saved_invite_is_named_after_its_certificate_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let (leaf, ca, key) = issued(Some("icu://icu.example.org:4711"), "ICU CA x");
        let bundle = icu_proto::passport::PassportV2 {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
        }
        .encode();
        let path = save_invite(&paths, &bundle).unwrap();
        assert_eq!(path, dir.path().join("invites").join("alice.icu"));
        assert_eq!(fs::read_to_string(&path).unwrap(), bundle);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(save_invite(&paths, "kein bundle").is_err());
    }

    /// The import confirmation names server, address and own name, and
    /// warns when the CA differs from the identity in force.
    #[test]
    fn import_summary_names_the_server_and_flags_a_new_ca() {
        let (leaf, ca, key) = issued(Some("icu://icu.example.org:4711"), "ICU CA icu.example.org");
        let server = ServerInfo::from_certs(&leaf, &ca).unwrap();
        let incoming = Identity {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
            server,
        };
        let summary = ImportSummary::of(&incoming, None);
        assert_eq!(summary.server_name, "icu.example.org");
        assert_eq!(summary.address, "icu.example.org:4711");
        assert!(!summary.registration);
        assert!(!summary.ca_changed, "nothing to compare with on first import");
        assert_eq!(summary.lines().len(), 3);
        assert!(!ImportSummary::of(&incoming, Some(&incoming)).ca_changed);

        let (leaf, ca, key) = issued(Some("icu://[2001:db8::1]:4711"), "ICU CA other");
        let current = Identity {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
            server: ServerInfo {
                host: "2001:db8::1".into(),
                port: 4711,
                server_name: "other".into(),
                name: "alice".into(),
            },
        };
        assert_eq!(ImportSummary::of(&current, None).address, "[2001:db8::1]:4711");
        let changed = ImportSummary::of(&incoming, Some(&current));
        assert!(changed.ca_changed);
        assert!(changed.lines().last().unwrap().contains("anderer Server"));
    }

    /// Export picks the version the certificates can support, and import
    /// reads either back to the same identity.
    #[test]
    fn passport_export_falls_back_to_v1_for_old_certificates() {
        let old_server = ServerInfo {
            host: "old.example.org".into(),
            port: 4711,
            server_name: "old.example.org".into(),
            name: "alice".into(),
        };
        let (leaf, ca, key) = issued(None, "ICU CA");
        let old = Identity {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
            server: old_server,
        };
        let text = old.to_passport();
        assert!(matches!(
            icu_proto::passport::Passport::decode(&text).unwrap(),
            icu_proto::passport::Passport::V1(_)
        ));
        assert_eq!(Identity::from_passport(&text).unwrap(), old);

        let (leaf, ca, key) = issued(Some("icu://icu.example.org:4711"), "ICU CA icu.example.org");
        let server = ServerInfo::from_certs(&leaf, &ca).unwrap();
        let new = Identity {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
            server,
        };
        let text = new.to_passport();
        assert!(matches!(
            icu_proto::passport::Passport::decode(&text).unwrap(),
            icu_proto::passport::Passport::V2(_)
        ));
        assert_eq!(Identity::from_passport(&text).unwrap(), new);
        assert!(!new.is_registration());

        // Old CA without an SNI in its CN, but the address is in the leaf:
        // when the stored SNI differs from the URI host, only V1 keeps it.
        let (leaf, ca, key) = issued(Some("icu://192.0.2.10:4711"), "ICU CA");
        let mixed = Identity {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
            server: ServerInfo {
                host: "192.0.2.10".into(),
                port: 4711,
                server_name: "icu.example.org".into(),
                name: "alice".into(),
            },
        };
        let text = mixed.to_passport();
        assert!(matches!(
            icu_proto::passport::Passport::decode(&text).unwrap(),
            icu_proto::passport::Passport::V1(_)
        ));
        assert_eq!(Identity::from_passport(&text).unwrap(), mixed);

        // A renewal under that CA moves host and port, never the SNI.
        let mut server = mixed.server.clone();
        let (leaf, _, _) = issued(Some("icu://198.51.100.7:5000"), "ICU CA");
        server.update_from_certs(&leaf, &mixed.ca_der);
        assert_eq!((server.host.as_str(), server.port), ("198.51.100.7", 5000));
        assert_eq!(server.server_name, "icu.example.org");

        let broken = icu_proto::passport::PassportV2 {
            key_der: vec![1, 2, 3],
            leaf_der: new.leaf_der.clone(),
            ca_der: new.ca_der.clone(),
        }
        .encode();
        assert!(Identity::from_passport(&broken).is_err());
    }

    #[test]
    fn identity_roundtrip_and_key_mode() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        assert!(Identity::load(&paths).unwrap().is_none());
        let id = Identity {
            key_der: vec![1, 2, 3],
            leaf_der: vec![4, 5],
            ca_der: vec![6],
            server: ServerInfo {
                host: "icu.example.org".into(),
                port: 4711,
                server_name: "icu.example.org".into(),
                name: "alice".into(),
            },
        };
        id.save(&paths).unwrap();
        assert_eq!(Identity::load(&paths).unwrap().unwrap(), id);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(paths.key()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Replaced as a set, and a crash between the two renames of the swap
        // falls back to the previous set.
        let other = Identity {
            key_der: vec![9],
            leaf_der: vec![8],
            ..id.clone()
        };
        other.save(&paths).unwrap();
        assert_eq!(Identity::load(&paths).unwrap().unwrap(), other);
        assert!(!paths.identity_dir().with_extension("old").exists());
        fs::rename(
            paths.identity_dir(),
            paths.identity_dir().with_extension("old"),
        )
        .unwrap();
        assert_eq!(Identity::load(&paths).unwrap().unwrap(), other);
    }
}
