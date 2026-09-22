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
    #[default]
    Ptt,
    /// Voice activation.
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
            mode: ModeSetting::Ptt,
            vad_threshold: 0.5,
            vad_auto: false,
            gain_db: 0.0,
            denoise: true,
            aec: true,
            release_ms: 150,
            hangover_ms: 200,
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
            theme: "dark".into(),
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

/// `identity/server.toml`: the only place the server coordinates live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Host from the invite link.
    pub host: String,
    /// Port from the invite link.
    pub port: u16,
    /// SNI from `EnrollResponse.server_name`.
    pub server_name: String,
    /// Last known display name, mirrored from the certificate's CN at
    /// enrollment and after every renewal. Informational: the certificate is
    /// the authority (`PROTOCOL.md` §2.1).
    pub name: String,
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

impl From<&Identity> for icu_proto::passport::PassportV1 {
    fn from(id: &Identity) -> Self {
        icu_proto::passport::PassportV1 {
            host: id.server.host.clone(),
            port: id.server.port,
            server_name: id.server.server_name.clone(),
            name: id.server.name.clone(),
            key_der: id.key_der.clone(),
            leaf_der: id.leaf_der.clone(),
            ca_der: id.ca_der.clone(),
        }
    }
}

impl From<icu_proto::passport::PassportV1> for Identity {
    fn from(p: icu_proto::passport::PassportV1) -> Self {
        Identity {
            key_der: p.key_der,
            leaf_der: p.leaf_der,
            ca_der: p.ca_der,
            server: ServerInfo {
                host: p.host,
                port: p.port,
                server_name: p.server_name,
                name: p.name,
            },
        }
    }
}

impl Identity {
    /// Loads the identity, `Ok(None)` if not enrolled yet.
    pub fn load(paths: &Paths) -> Result<Option<Identity>, ConfigError> {
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

    /// Writes all four files; the key with mode 0600.
    pub fn save(&self, paths: &Paths) -> Result<(), ConfigError> {
        fs::create_dir_all(paths.identity_dir())?;
        write_atomic(&paths.key(), &self.key_der, true)?;
        write_atomic(&paths.leaf(), &self.leaf_der, false)?;
        write_atomic(&paths.ca(), &self.ca_der, false)?;
        let text =
            toml::to_string_pretty(&self.server).map_err(|e| ConfigError::Parse(e.to_string()))?;
        write_atomic(&paths.server(), text.as_bytes(), false)
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
        s.audio.mode = ModeSetting::Vad;
        s.audio.vad_threshold = 0.7;
        s.audio.vad_auto = true;
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
        assert!(text.contains("mode = \"vad\""));
        assert!(text.contains("corner = \"bottom-left\""));
        assert!(text.contains("[volumes]"));
    }

    #[test]
    fn unknown_keys_and_missing_sections_are_tolerated() {
        let s: Settings = toml::from_str("[audio]\nmode = \"vad\"\n[future]\nx = 1\n").unwrap();
        assert_eq!(s.audio.mode, ModeSetting::Vad);
        assert_eq!(s.audio.release_ms, 150);
        assert!(s.overlay.dim_non_speaking, "old settings.toml default on");
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
    }
}
