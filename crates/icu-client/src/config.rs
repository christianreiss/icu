//! `settings.toml`, the identity directory and the paths per OS
//! (`docs/reference/client.md` §5, §11).

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use icu_proto::com::ComKey;
use serde::{Deserialize, Serialize};

pub use crate::dsp::voice::VoiceStrength;

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
    /// `invites/`: registration bundles saved from the Nutzer tab.
    pub fn invites_dir(&self) -> PathBuf {
        self.config_dir.join("invites")
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

impl From<ModeSetting> for crate::dsp::gate::Mode {
    fn from(m: ModeSetting) -> Self {
        match m {
            ModeSetting::Ptt => Self::Ptt,
            ModeSetting::Vad => Self::Vad,
        }
    }
}

impl From<crate::dsp::gate::Mode> for ModeSetting {
    fn from(m: crate::dsp::gate::Mode) -> Self {
        match m {
            crate::dsp::gate::Mode::Ptt => ModeSetting::Ptt,
            crate::dsp::gate::Mode::Vad => ModeSetting::Vad,
        }
    }
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
    /// Mic auto leveling (`dsp::normalize::Profile::MIC_LEVEL`) after
    /// `gain_db`, ahead of denoise and VAD. Requires `denoise`.
    pub auto_level: bool,
    /// Peer measurements refine automatic leveling.
    pub peer_assistance: bool,
    /// Temporarily tune system capture volume; restore on call end.
    pub tune_system_mic: bool,
    /// RNNoise on.
    pub denoise: bool,
    /// AEC3 on.
    pub aec: bool,
    /// Voice optimizer (high-pass, compressor, limiter) on.
    pub voice_opt: bool,
    /// Voice optimizer preset.
    pub voice_strength: VoiceStrength,
    /// PTT release delay in ms.
    pub release_ms: u32,
    /// VAD hangover in ms.
    pub hangover_ms: u32,
    /// How long voice must last in ms before the VAD opens the gate, so a
    /// key click does not (0 opens on the first voiced frame).
    pub attack_ms: u32,
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
    /// Bring every incoming voice to a common loudness
    /// (`dsp::normalize`), before the per-user volume.
    pub normalize_incoming: bool,
    /// Bring the own voice to a common loudness right before the encoder
    /// (`dsp::normalize::Profile::OUTBOUND` plus a peak limiter).
    pub normalize_outgoing: bool,
    /// Muted by the user: a mute survives a restart, as in Mumble. Written
    /// by the engine alone (`engine::LocalState::own_mute`); deafen is not
    /// kept, a restart undeafens.
    pub muted: bool,
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
            auto_level: true,
            peer_assistance: true,
            tune_system_mic: true,
            denoise: true,
            aec: true,
            voice_opt: true,
            voice_strength: VoiceStrength::Medium,
            release_ms: 150,
            hangover_ms: 1000,
            attack_ms: 40,
            frame_ms: 20,
            cue_join: true,
            cue_tx: false,
            cue_mute: true,
            output_volume: 100.0,
            normalize_incoming: true,
            normalize_outgoing: true,
            muted: false,
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
    /// Portal first; evdev only when there is no portal or it fails. A
    /// declined portal dialog stays declined (`hotkey::BACKEND_PORTAL_DECLINED`).
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
    /// Camera on/off toggle. Unset by default (and in files from before it
    /// existed).
    pub camera: KeyBinding,
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
            camera: KeyBinding::default(),
        }
    }
}

/// `[camera]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CameraSettings {
    /// Camera index or name.
    pub device: Option<String>,
    /// Last camera switch state, restored on startup.
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

/// The overlay's camera-tile diameters the size slider offers, in layout
/// pixels; a voice-only plate is 60 % of that.
pub const TILE_SIZES: std::ops::RangeInclusive<u32> = 80..=480;

/// What the „Wechselsperre“ slider offers for `overlay.shy_hold_ms`.
pub const SHY_HOLDS: std::ops::RangeInclusive<u32> = 0..=5000;

/// Return-delay slider range for `overlay.shy_return_ms`.
pub const SHY_RETURNS: std::ops::RangeInclusive<u32> = 0..=60_000;

/// `overlay.size`: a diameter in pixels, or — in a file from before the
/// slider — one of the three fixed sizes it replaced.
fn tile_size<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Size {
        Px(u32),
        Named(String),
    }
    Ok(match Size::deserialize(d)? {
        Size::Px(px) => px,
        Size::Named(name) => match name.as_str() {
            "medium" => 240,
            "large" => 360,
            _ => 120,
        },
    })
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

/// The corona color of everyone who chose none, the own one included.
pub const DEFAULT_FRAME_COLOR: [u8; 3] = [0x2e, 0x9b, 0xff];

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
    /// Camera-tile diameter in layout pixels while its person is silent,
    /// within [`TILE_SIZES`].
    #[serde(deserialize_with = "tile_size")]
    pub size: u32,
    /// The same while its person speaks; unset, 20 % more than `size`
    /// (see [`OverlaySettings::speaking_diameter`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaking_size: Option<u32>,
    /// Stack direction for corner positions.
    pub expand: Expand,
    /// Dim tiles of peers who aren't currently speaking.
    pub dim_non_speaking: bool,
    /// Draw a highlighted frame around the tile of whoever is speaking.
    /// Only here: whether others see coronas is theirs to choose.
    pub speaking_frame: bool,
    /// This user's own corona color, RGB: stored on the server and shown
    /// for them on every client (protocol minor 5); [`DEFAULT_FRAME_COLOR`]
    /// is the server's "none chosen".
    pub frame_color: [u8; 3],
    /// Show smaller name plates for everyone without a camera, including
    /// while silent, with the same speaking and dimming rules as video tiles.
    pub voice_plates: bool,
    /// Tiles get out of the mouse pointer's way: the pointer on any tile
    /// fades the whole row out, letting clicks through to what is below,
    /// and back in on the other side of the screen.
    pub shy: bool,
    /// How long the shy row stays where it showed up before the pointer
    /// can send it off again, in ms, within [`SHY_HOLDS`] — so a pointer
    /// over both places does not bounce it to and fro.
    pub shy_hold_ms: u32,
    /// Return to the configured position after this long without the pointer
    /// over the displaced row, in ms, within [`SHY_RETURNS`].
    pub shy_return_ms: u32,
    /// Show the own tile too (own camera, or a plate as for the others),
    /// after everyone else's.
    pub show_self: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        OverlaySettings {
            enabled: false,
            corner: Corner::TopRight,
            offset: [40, 40],
            size: 120,
            speaking_size: None,
            expand: Expand::Vertical,
            dim_non_speaking: true,
            speaking_frame: true,
            frame_color: DEFAULT_FRAME_COLOR,
            voice_plates: true,
            shy: false,
            shy_hold_ms: 1000,
            shy_return_ms: 3000,
            show_self: false,
        }
    }
}

impl OverlaySettings {
    /// A speaking person's camera-tile diameter: `speaking_size`, or —
    /// until its slider is touched — 20 % more than `size`, as before there
    /// was one.
    pub fn speaking_diameter(&self) -> u32 {
        self.speaking_size.unwrap_or(self.size * 6 / 5)
    }
}

/// `[ui]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// Legacy. The display name lives in the certificate's CN since the name
    /// moved there (`docs/reference/protocol.md` §2.1); nothing reads this any more. Kept so
    /// an existing `settings.toml` still round-trips.
    pub name: Option<String>,
    /// Start in the tray.
    pub start_minimized: bool,
    /// Register the `.icu` file association on start (`crate::uri`); the
    /// name is from when it was an `icu://` handler.
    pub register_uri_handler: bool,
    /// Theme: `system`, `light`, `dark`.
    pub theme: String,
    /// What closing the main window does: `tray` (hide to the notification
    /// area; minimizes where that is impossible — no tray host, Wayland),
    /// `minimize` (to the task bar/dock), `quit`.
    pub close_action: String,
    /// Settings window shows the technical knobs (codec frame, hangover
    /// times, hotkey backend, passport, diagnostics) next to the everyday
    /// ones. Off by default: most people never need them.
    pub show_advanced: bool,
    /// A desktop notification when someone comes or goes, besides the tone
    /// (`AudioSettings::cue_join`). Off by default.
    pub notify_join: bool,
    /// The first-run setup („ICU — Einrichtung“, `ui::setup`) was finished
    /// or skipped. While `false` the main window opens it; a file from
    /// before the setup existed has no such key, so it shows once there too.
    pub setup_done: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings {
            name: None,
            start_minimized: false,
            register_uri_handler: true,
            theme: "system".into(),
            close_action: "tray".into(),
            show_advanced: false,
            notify_join: false,
            setup_done: false,
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

    /// [`Settings::load`], except that a file that does not parse does not
    /// stop the client — started from the desktop, a line on stderr is
    /// all anybody would have seen. It is moved aside as
    /// `settings.toml.broken-<unix time>`, the defaults apply, and the
    /// returned problem is for the window to show.
    pub fn load_or_recover(
        paths: &Paths,
    ) -> Result<(Settings, Option<crate::problem::Problem>), ConfigError> {
        match Settings::load(paths) {
            Ok(settings) => Ok((settings, None)),
            Err(ConfigError::Parse(e)) => {
                let name = format!("settings.toml.broken-{}", crate::net::conn::unix_now());
                fs::rename(paths.settings(), paths.config_dir.join(&name))?;
                tracing::warn!(error = %e, moved_to = %name, "settings.toml unreadable, using defaults");
                Ok((
                    Settings::default(),
                    Some(crate::problem::Problem::new(
                        format!(
                            "Die Einstellungen ließen sich nicht lesen und stehen wieder auf \
                             Standard. Die alte Datei liegt als „{name}“ im Einstellungsordner."
                        ),
                        e,
                    )),
                ))
            }
            Err(e) => Err(e),
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

    /// Copies from `from` what belongs to this machine rather than to the
    /// person: microphone, speakers and camera, the hotkey backend (a matter
    /// of the desktop), the file association, whether the first-run setup
    /// ran here (it picks this machine's devices) and the legacy name. The
    /// one list of what a passport leaves behind (`docs/reference/protocol.md` §2.3.1).
    fn keep_machine_fields(&mut self, from: &Settings) {
        self.audio.input_device.clone_from(&from.audio.input_device);
        self.audio
            .output_device
            .clone_from(&from.audio.output_device);
        self.hotkeys.backend = from.hotkeys.backend;
        self.camera.clone_from(&from.camera);
        self.ui.register_uri_handler = from.ui.register_uri_handler;
        self.ui.setup_done = from.ui.setup_done;
        self.ui.name.clone_from(&from.ui.name);
    }

    /// What a passport carries: everything but the machine-bound fields,
    /// which read as their defaults.
    pub fn portable(&self) -> Settings {
        let mut portable = self.clone();
        portable.keep_machine_fields(&Settings::default());
        portable
    }

    /// Takes over the settings a passport brought: `incoming` replaces
    /// everything but this machine's own fields. It comes from outside, so
    /// every number is brought into the range the settings window offers,
    /// and one that is no number at all falls back to its default.
    pub fn take_portable(&mut self, incoming: Settings) {
        let mut taken = incoming;
        taken.keep_machine_fields(self);
        taken.clamp_numbers();
        *self = taken;
    }

    /// What taking `incoming` over ([`Settings::take_portable`]) would
    /// change about when and how this device can be heard, one line each in
    /// the words of the settings window: sending mode, threshold, gain,
    /// noise suppression, VAD hangover, the three cues and „Minimiert
    /// starten“. The import confirmation shows them next to the switch
    /// that takes the settings along: a passport from somebody else could
    /// otherwise switch to voice activation at threshold 0 with +20 dB, no
    /// cues, started into the tray, and nothing would show it (review
    /// 2026-09-27, C-3). Empty when none of them changes.
    pub fn send_changes(&self, incoming: &Settings) -> Vec<String> {
        let mut after = self.clone();
        after.take_portable(incoming.clone());
        let (was, will) = (&self.audio, &after.audio);
        let mut lines = Vec::new();
        let mut differs = |what: &str, now: String, before: String| {
            if now != before {
                lines.push(format!("{what}: {now} (bisher {before})"));
            }
        };
        let mode = |a: &AudioSettings| match a.mode {
            ModeSetting::Ptt => "Taste halten".to_string(),
            ModeSetting::Vad => "Automatisch".to_string(),
        };
        let threshold = |a: &AudioSettings| {
            if a.vad_auto {
                "automatisch".to_string()
            } else {
                format!("{:.0} %", a.vad_threshold * 100.0)
            }
        };
        let on = |b: bool| if b { "an" } else { "aus" }.to_string();
        differs("Senden", mode(will), mode(was));
        differs("Schwelle", threshold(will), threshold(was));
        differs(
            "Verstärkung",
            format!("{:+.0} dB", will.gain_db),
            format!("{:+.0} dB", was.gain_db),
        );
        differs("Rauschunterdrückung", on(will.denoise), on(was.denoise));
        differs(
            "VAD-Nachlauf",
            format!("{} ms", will.hangover_ms),
            format!("{} ms", was.hangover_ms),
        );
        differs(
            "VAD-Anlauf",
            format!("{} ms", will.attack_ms),
            format!("{} ms", was.attack_ms),
        );
        differs(
            "Ton „Jemand kommt oder geht“",
            on(will.cue_join),
            on(was.cue_join),
        );
        differs(
            "Ton „Senden beginnt oder endet“",
            on(will.cue_tx),
            on(was.cue_tx),
        );
        differs("Ton „Stummschalten“", on(will.cue_mute), on(was.cue_mute));
        differs(
            "Minimiert starten",
            on(after.ui.start_minimized),
            on(self.ui.start_minimized),
        );
        lines
    }

    /// The ranges of the settings window's sliders and choices.
    fn clamp_numbers(&mut self) {
        let defaults = AudioSettings::default();
        let audio = &mut self.audio;
        audio.output_volume = clamped(audio.output_volume, 0.0, 200.0, defaults.output_volume);
        audio.vad_threshold = clamped(audio.vad_threshold, 0.0, 1.0, defaults.vad_threshold);
        audio.gain_db = clamped(audio.gain_db, -20.0, 20.0, defaults.gain_db);
        audio.release_ms = audio.release_ms.min(1000);
        audio.hangover_ms = audio.hangover_ms.min(3000);
        audio.attack_ms = audio.attack_ms.min(200);
        self.overlay.shy_hold_ms = self.overlay.shy_hold_ms.min(*SHY_HOLDS.end());
        self.overlay.shy_return_ms = self.overlay.shy_return_ms.min(*SHY_RETURNS.end());
        if !matches!(audio.frame_ms, 10 | 20) {
            audio.frame_ms = defaults.frame_ms;
        }
        for percent in self.volumes.values_mut() {
            *percent = clamped(*percent, 0.0, 200.0, 100.0);
        }
    }
}

/// `value` within `min..=max`, or `default` when it is not a finite number.
fn clamped(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

/// `identity/server.toml`: where to connect.
///
/// Derived from the certificates wherever they say it (`docs/reference/protocol.md` §2.3):
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
    /// is the authority (`docs/reference/protocol.md` §2.1).
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
        let name = crate::conninfo::cn(leaf.subject()).unwrap_or_default();
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
    let name = crate::conninfo::cn(ca.subject())?
        .strip_prefix("ICU CA ")?
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// Key, leaf, CA, the server's com key and server coordinates.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    /// PKCS#8 DER private key.
    pub key_der: Vec<u8>,
    /// Client leaf certificate, DER.
    pub leaf_der: Vec<u8>,
    /// Server CA certificate, DER.
    pub ca_der: Vec<u8>,
    /// The server's com key (`docs/reference/protocol.md` §1): every datagram carries its
    /// tag, or the server drops it.
    pub com_key: ComKey,
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
            .field("com_key", &self.com_key)
            .field("server", &self.server)
            .finish()
    }
}

impl Identity {
    /// Reads a pasted passport or registration bundle.
    ///
    /// A passport carries no address of its own, so its certificates must
    /// say where the server is, and the key has to parse, since the codec
    /// only checks the wrapper.
    pub fn from_passport(armored: &str) -> Result<Identity, String> {
        Identity::from_passport_with_settings(armored).map(|(identity, _)| identity)
    }

    /// [`Identity::from_passport`], plus the settings a passport may carry
    /// (`docs/reference/protocol.md` §2.3.1). Settings that do not read are left
    /// behind with a log line: the identity is what a passport is for.
    /// Refused before anything is replaced: what [`Identity::check_importable`]
    /// finds wrong.
    pub fn from_passport_with_settings(
        armored: &str,
    ) -> Result<(Identity, Option<Settings>), String> {
        use icu_proto::passport::Passport;
        let passport = Passport::decode(armored).map_err(|e| {
            tracing::info!(%e, "passport does not decode");
            crate::problem::passport_text(&e)
        })?;
        // A registration bundle must carry a valid signature by its own CA
        // (`docs/reference/protocol.md` §2.3.1): that key never travels inside the bundle,
        // so this catches tampering with the armored text after the server
        // wrote it — the com key included, which nothing else here checks.
        // A personal export normally carries none; if one is present anyway
        // it is still checked rather than silently ignored.
        let is_registration = crate::net::register::is_registration(&passport.leaf_der);
        if (is_registration || !passport.signature.is_empty())
            && !crate::conninfo::passport_signed_by_its_ca(&passport)
        {
            tracing::info!(is_registration, "passport signature missing or invalid");
            return Err(if is_registration {
                "Diese Einladung ist beschädigt (Signatur fehlt oder ist ungültig). Bitte \
                 beim Admin eine neue anfordern."
                    .to_string()
            } else {
                "Dieser Passport ist beschädigt (die Signatur ist ungültig). Bitte auf dem \
                 anderen Gerät neu exportieren."
                    .to_string()
            });
        }
        let settings = if passport.settings.trim().is_empty() {
            None
        } else {
            toml::from_str::<Settings>(&passport.settings)
                .inspect_err(|e| {
                    tracing::info!(error = %e, "passport settings unreadable, taking the identity alone");
                })
                .ok()
        };
        let identity = Identity::addressed_by_certs(
            passport.key_der,
            passport.leaf_der,
            passport.ca_der,
            passport.com_key,
        )?;
        identity.check_importable(crate::net::conn::unix_now())?;
        Ok((identity, settings))
    }

    /// Whether this identity can be taken over as it is, at `now` (Unix
    /// seconds): the key parses and belongs to the certificate, the
    /// certificate is signed by the CA that came with it, an invitation has
    /// not expired, and what the import confirmation shows of it is plain
    /// text. Otherwise the working identity would be replaced by one that
    /// cannot connect, or a forged name could draw lines of its own into
    /// the confirmation (review 2026-09-27, C-2, C-4). An expired own
    /// certificate passes: the server renews it (`docs/reference/protocol.md` §2.3.3).
    fn check_importable(&self, now: i64) -> Result<(), String> {
        let (what, again) = if self.is_registration() {
            ("Diese Einladung", "Bitte beim Admin eine neue anfordern.")
        } else {
            (
                "Dieser Passport",
                "Bitte auf dem anderen Gerät neu exportieren.",
            )
        };
        let broken = |why: &str| format!("{what} ist beschädigt ({why}). {again}");
        if self.key_pair().is_err() {
            return Err(broken("kein gültiger Schlüssel"));
        }
        if !crate::conninfo::leaf_matches_key(&self.leaf_der, &self.key_der) {
            return Err(broken("Schlüssel und Zertifikat gehören nicht zusammen"));
        }
        if !crate::conninfo::leaf_signed_by(&self.leaf_der, &self.ca_der) {
            return Err(broken(
                "das Zertifikat stammt nicht von der mitgelieferten CA",
            ));
        }
        if self.is_registration() && crate::net::conn::leaf_expired(&self.leaf_der, now) {
            let until = crate::conninfo::CertInfo::from_der(&self.leaf_der).not_after;
            return Err(format!(
                "Diese Einladung ist abgelaufen (gültig bis {until}). Bitte beim Admin eine \
                 neue anfordern."
            ));
        }
        let name = crate::conninfo::CertInfo::from_der(&self.leaf_der).subject;
        let shown = [name.as_str(), &self.server.server_name, &self.server.host];
        if !shown.into_iter().all(icu_proto::is_plain_text) {
            return Err(format!(
                "{what} enthält Steuer- oder Formatzeichen im Namen oder in der Serveradresse \
                 und wird nicht übernommen. {again}"
            ));
        }
        Ok(())
    }

    /// An identity whose certificates say where the server is.
    fn addressed_by_certs(
        key_der: Vec<u8>,
        leaf_der: Vec<u8>,
        ca_der: Vec<u8>,
        com_key: ComKey,
    ) -> Result<Identity, String> {
        let server = ServerInfo::from_certs(&leaf_der, &ca_der).ok_or_else(|| {
            "Dieser Passport ist unvollständig (keine Serveradresse). Bitte auf dem \
             anderen Gerät neu exportieren."
                .to_string()
        })?;
        Ok(Identity {
            key_der,
            leaf_der,
            ca_der,
            com_key,
            server,
        })
    }

    /// The armored passport for this identity, without settings. The
    /// certificates say where the server is (`docs/reference/protocol.md` §2.3.1).
    pub fn to_passport(&self) -> String {
        self.passport(String::new()).encode()
    }

    /// The armored passport with this device's settings, their portable part
    /// ([`Settings::portable`]); without them when they would not fit. The
    /// flag says whether they are in.
    pub fn to_passport_with_settings(&self, settings: &Settings) -> (String, bool) {
        match toml::to_string_pretty(&settings.portable()) {
            Ok(text) => {
                let passport = self.passport(text);
                if passport.fits() {
                    return (passport.encode(), true);
                }
                tracing::warn!("settings too large for the passport, exporting without them");
            }
            Err(e) => {
                tracing::warn!(error = %e, "settings do not serialize, exporting without them")
            }
        }
        (self.to_passport(), false)
    }

    fn passport(&self, settings: String) -> icu_proto::passport::Passport {
        icu_proto::passport::Passport {
            key_der: self.key_der.clone(),
            leaf_der: self.leaf_der.clone(),
            ca_der: self.ca_der.clone(),
            com_key: self.com_key.clone(),
            settings,
            // Unsigned: the only key a client could sign with here already
            // travels inside the bundle itself (see `Passport::signature`'s
            // doc). Only a server-issued registration bundle is signed.
            signature: Vec::new(),
        }
    }

    /// The private key as rcgen reads it, for CSRs. One that does not parse
    /// is worded for the user here, the same way at every caller: the stored
    /// credentials are broken, and a passport from another device is the way
    /// back.
    pub fn key_pair(&self) -> Result<rcgen::KeyPair, crate::problem::Problem> {
        rcgen::KeyPair::try_from(self.key_der.as_slice()).map_err(|e| {
            crate::problem::Problem::new(
                "Die gespeicherten Zugangsdaten sind beschädigt. Bitte den Passport erneut \
                 importieren.",
                e.to_string(),
            )
        })
    }

    /// Whether this is a registration bundle that still has to be swapped for
    /// a certificate of its own (`net::register`).
    pub fn is_registration(&self) -> bool {
        crate::net::register::is_registration(&self.leaf_der)
    }

    /// Loads the identity, `Ok(None)` if not enrolled yet.
    ///
    /// Only reads, so the window may call it while the engine saves. A save
    /// interrupted between its two renames left the previous complete set
    /// in `identity.old/`; that one is read, and the next `save` sets the
    /// directories right. (Until 2026-09-25 `load` repaired that by renaming,
    /// and a load from the window between the renames of a running save
    /// made the save fail with `ENOTEMPTY`.)
    pub fn load(paths: &Paths) -> Result<Option<Identity>, ConfigError> {
        let _files = crate::sync::lock(&IDENTITY_FILES);
        let mut dir = paths.identity_dir();
        if !dir.join("server.toml").exists() {
            dir = dir.with_extension("old");
            if !dir.join("server.toml").exists() {
                return Ok(None);
            }
        }
        let key_der = key_seal::open(fs::read(dir.join("key.pk8"))?)?;
        let leaf_der = fs::read(dir.join("leaf.der"))?;
        let ca_der = fs::read(dir.join("ca.der"))?;
        let com_key = load_com_key(&dir)?;
        let server: ServerInfo = toml::from_str(&fs::read_to_string(dir.join("server.toml"))?)
            .map_err(|e| ConfigError::Parse(e.to_string()))?;
        Ok(Some(Identity {
            key_der,
            leaf_der,
            ca_der,
            com_key,
            server,
        }))
    }

    /// Writes all five files as one set; the keys with mode 0600.
    ///
    /// Into a fresh `identity.new/` first, then swapped in by renaming
    /// directories, so a crash never leaves a key next to somebody else's
    /// certificate. The swap takes two renames (a directory cannot replace a
    /// non-empty one everywhere); after a crash between them `load` reads
    /// `identity.old/`, which stays until the new set is in place.
    pub fn save(&self, paths: &Paths) -> Result<(), ConfigError> {
        self.save_set(paths, false)
    }

    /// [`Identity::save`] for an imported identity that has yet to prove
    /// itself: the set it replaces stays in `identity.old/` until
    /// [`Identity::restore_previous`] puts it back or
    /// [`Identity::drop_previous`] lets it go (review 2026-09-27, C-2).
    /// `load` prefers `identity/`, so the kept set changes nothing until
    /// then; the next plain `save` or a logout removes it.
    pub fn save_keeping_previous(&self, paths: &Paths) -> Result<(), ConfigError> {
        self.save_set(paths, true)
    }

    /// Puts the set [`Identity::save_keeping_previous`] kept back in place of
    /// the one saved over it, which is deleted. Two renames like `save`:
    /// after a crash between them `load` reads the kept set.
    pub fn restore_previous(paths: &Paths) -> Result<(), ConfigError> {
        let _files = crate::sync::lock(&IDENTITY_FILES);
        let dir = paths.identity_dir();
        let previous = dir.with_extension("old");
        let failed = dir.with_extension("new");
        if !previous.join("server.toml").exists() {
            return Err(ConfigError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "no previous identity kept",
            )));
        }
        remove_dir_if_present(&failed)?;
        if dir.exists() {
            fs::rename(&dir, &failed)?;
        }
        fs::rename(&previous, &dir)?;
        #[cfg(unix)]
        if let Ok(parent) = fs::File::open(&paths.config_dir) {
            let _ = parent.sync_all();
        }
        let _ = remove_dir_if_present(&failed);
        Ok(())
    }

    /// Lets the set [`Identity::save_keeping_previous`] kept go, once the one
    /// in `identity/` proved itself. Never while `identity/` is incomplete:
    /// then `identity.old/` is the set `load` reads.
    pub fn drop_previous(paths: &Paths) -> Result<(), ConfigError> {
        let _files = crate::sync::lock(&IDENTITY_FILES);
        let dir = paths.identity_dir();
        if !dir.join("server.toml").exists() {
            return Ok(());
        }
        remove_dir_if_present(&dir.with_extension("old"))
    }

    fn save_set(&self, paths: &Paths, keep_previous: bool) -> Result<(), ConfigError> {
        let _files = crate::sync::lock(&IDENTITY_FILES);
        let dir = paths.identity_dir();
        let fresh = dir.with_extension("new");
        let previous = dir.with_extension("old");
        remove_dir_if_present(&fresh)?;
        fs::create_dir_all(&fresh)?;
        write_atomic(
            &fresh.join("key.pk8"),
            &key_seal::seal(&self.key_der)?,
            true,
        )?;
        write_atomic(&fresh.join("leaf.der"), &self.leaf_der, false)?;
        write_atomic(&fresh.join("ca.der"), &self.ca_der, false)?;
        write_atomic(
            &fresh.join(COM_KEY_FILE),
            &key_seal::seal(self.com_key.as_bytes())?,
            true,
        )?;
        let text =
            toml::to_string_pretty(&self.server).map_err(|e| ConfigError::Parse(e.to_string()))?;
        write_atomic(&fresh.join("server.toml"), text.as_bytes(), false)?;

        if dir.exists() {
            // A leftover `identity.old/` is older than `dir`.
            remove_dir_if_present(&previous)?;
            fs::rename(&dir, &previous)?;
        }
        // Without `dir`, `identity.old/` may be the only complete set (a
        // swap interrupted between its renames): it goes only below.
        fs::rename(&fresh, &dir)?;
        #[cfg(unix)]
        if let Ok(parent) = fs::File::open(&paths.config_dir) {
            let _ = parent.sync_all();
        }
        if !keep_previous {
            let _ = remove_dir_if_present(&previous);
        }
        Ok(())
    }

    /// Deletes the identity from this device: logging out. The leftovers of
    /// an interrupted `save` go first, or `load` would read `identity.old/`.
    /// Saved invitations go too: each is a live registration key
    /// (`save_invite`) of the account logging out. An exported
    /// `icu-passport.icu` stays: it is the way back in.
    pub fn remove(paths: &Paths) -> Result<(), ConfigError> {
        let _files = crate::sync::lock(&IDENTITY_FILES);
        let dir = paths.identity_dir();
        remove_dir_if_present(&dir.with_extension("new"))?;
        remove_dir_if_present(&dir.with_extension("old"))?;
        remove_dir_if_present(&dir)?;
        remove_dir_if_present(&paths.invites_dir())
    }
}

/// `identity/com.key`: the raw key, sealed like `key.pk8` on Windows.
const COM_KEY_FILE: &str = "com.key";

/// Reads `com.key` from an identity directory. An identity from before the
/// com key (2026-09-27) has none and cannot reach any server: it reads as
/// broken, and a new passport is the way back.
fn load_com_key(dir: &Path) -> Result<ComKey, ConfigError> {
    let data = match fs::read(dir.join(COM_KEY_FILE)) {
        Ok(data) => key_seal::open(data)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigError::Parse(
                "identity without com key (older than 2026-09-27), import a new passport".into(),
            ))
        }
        Err(e) => return Err(e.into()),
    };
    let bytes = <[u8; icu_proto::com::COM_KEY_LEN]>::try_from(data.as_slice())
        .map_err(|_| ConfigError::Parse(format!("{COM_KEY_FILE}: not a com key")))?;
    Ok(ComKey::from_bytes(bytes))
}

/// Serializes `Identity::load`, `save` and `remove` within the process: the
/// engine stores a renewed certificate while the window may be reading the
/// identity for a passport export, and a load between the two renames of a
/// save could otherwise pair a key with the other set's certificate.
static IDENTITY_FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What importing a passport or registration bundle would switch this client
/// to, shown for the user to confirm before anything is replaced
/// (`docs/reference/protocol.md` §2.3.1). A passport brings its own CA and address, so a
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
    /// SHA-256 of the CA the client will hold the server to, as
    /// `conninfo::CertInfo::fingerprint` prints it: the one thing worth
    /// comparing with whoever sent the passport.
    pub ca_fingerprint: String,
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
            ca_fingerprint: crate::conninfo::CertInfo::from_der(&identity.ca_der).fingerprint,
            ca_changed: current.is_some_and(|c| !same_ca(&c.ca_der, &identity.ca_der)),
        }
    }

    /// The summary as lines of text, for the CLI and the settings window.
    /// The fingerprint takes two lines of 16 pairs, so it can be read out
    /// in halves.
    pub fn lines(&self) -> Vec<String> {
        let pairs: Vec<&str> = self.ca_fingerprint.split(':').collect();
        let (first, second) = pairs.split_at(pairs.len().min(16));
        let mut lines = vec![
            format!("Server:  {}", self.server_name),
            format!("Adresse: {}", self.address),
            if self.registration {
                format!("Name:    {} (Einladung, eigener Name folgt)", self.name)
            } else {
                format!("Name:    {}", self.name)
            },
            format!("CA:      {}", first.join(":")),
        ];
        if !second.is_empty() {
            lines.push(format!("         {}", second.join(":")));
        }
        if self.ca_changed {
            lines.push(
                "Achtung: anderer Server als bisher — nur übernehmen, wenn du das erwartest."
                    .into(),
            );
        }
        lines
    }
}

/// Whether two CA certificates are the same CA: the same subject on the same
/// key. `icu-server renew-ca` issues a new certificate — new DER — for both,
/// and a passport from before it is no other server (review 2026-09-27,
/// C-5). Certificates that do not parse count as the same only when their
/// bytes are.
fn same_ca(a: &[u8], b: &[u8]) -> bool {
    match (
        x509_parser::parse_x509_certificate(a),
        x509_parser::parse_x509_certificate(b),
    ) {
        (Ok((_, a)), Ok((_, b))) => {
            a.subject().as_raw() == b.subject().as_raw() && a.public_key().raw == b.public_key().raw
        }
        _ => a == b,
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
    let leaf_der = Passport::decode(bundle)
        .map_err(|e| ConfigError::Parse(e.to_string()))?
        .leaf_der;
    let name: String = x509_parser::parse_x509_certificate(&leaf_der)
        .ok()
        .and_then(|(_, leaf)| crate::conninfo::cn(leaf.subject()))
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
    let path = paths.invites_dir().join(format!("{name}.icu"));
    write_atomic(&path, bundle.as_bytes(), true)?;
    Ok(path)
}

/// Takes a passport's settings over into the `settings.toml` of an
/// installation whose engine is not running yet (first-start window, `icu
/// <file>` on the terminal): [`Settings::take_portable`] on what is stored,
/// then saved. A running engine does the same itself
/// (`Command::ReplaceIdentity`).
pub fn import_settings(paths: &Paths, incoming: Settings) -> Result<(), ConfigError> {
    let mut settings = Settings::load(paths)?;
    settings.take_portable(incoming);
    settings.save(paths)
}

/// The private key at rest. On Windows `key.pk8` is sealed for the
/// current user with DPAPI (`CryptProtectData`): another account, or the
/// file copied to another machine, cannot use it. The sealed file starts
/// with [`key_seal::MAGIC`]; a plain PKCS#8 file (written before) is still
/// read and sealed on the next save. Elsewhere the file mode (0600) guards
/// it and the key is stored as it is.
mod key_seal {
    use super::ConfigError;

    /// First bytes of a sealed key; PKCS#8 DER starts with 0x30.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub const MAGIC: &[u8] = b"ICU-DPAPI/1\n";

    #[cfg(not(windows))]
    pub fn seal(der: &[u8]) -> Result<Vec<u8>, ConfigError> {
        Ok(der.to_vec())
    }

    #[cfg(not(windows))]
    pub fn open(data: Vec<u8>) -> Result<Vec<u8>, ConfigError> {
        Ok(data)
    }

    #[cfg(windows)]
    pub fn seal(der: &[u8]) -> Result<Vec<u8>, ConfigError> {
        let mut out = MAGIC.to_vec();
        out.extend(dpapi::protect(der)?);
        Ok(out)
    }

    #[cfg(windows)]
    pub fn open(data: Vec<u8>) -> Result<Vec<u8>, ConfigError> {
        match data.strip_prefix(MAGIC) {
            Some(sealed) => dpapi::unprotect(sealed),
            None => Ok(data),
        }
    }

    #[cfg(windows)]
    mod dpapi {
        use windows::Win32::{
            Foundation::{LocalFree, HLOCAL},
            Security::Cryptography::{
                CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
            },
        };

        use super::ConfigError;

        fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
            CRYPT_INTEGER_BLOB {
                cbData: data.len() as u32,
                pbData: data.as_ptr() as *mut u8,
            }
        }

        /// Copies DPAPI's output and frees it.
        fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
            // SAFETY: DPAPI hands back `cbData` bytes at `pbData`, to be
            // released with LocalFree.
            unsafe {
                let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
                LocalFree(Some(HLOCAL(out.pbData.cast())));
                v
            }
        }

        fn error(what: &str, e: windows::core::Error) -> ConfigError {
            ConfigError::Io(std::io::Error::other(format!("{what}: {e}")))
        }

        pub fn protect(data: &[u8]) -> Result<Vec<u8>, ConfigError> {
            let mut out = CRYPT_INTEGER_BLOB::default();
            // SAFETY: `blob` borrows `data` for the call only.
            unsafe {
                CryptProtectData(
                    &blob(data),
                    windows::core::w!("ICU"),
                    None,
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            }
            .map_err(|e| error("CryptProtectData", e))?;
            Ok(take(out))
        }

        pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, ConfigError> {
            let mut out = CRYPT_INTEGER_BLOB::default();
            // SAFETY: as in `protect`.
            unsafe {
                CryptUnprotectData(
                    &blob(data),
                    None,
                    None,
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            }
            .map_err(|e| error("CryptUnprotectData (Schlüssel eines anderen Kontos?)", e))?;
            Ok(take(out))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Sealed and opened again gives the key back; a key written
        /// before sealing existed still opens.
        #[test]
        fn a_sealed_key_opens_again() {
            let der = vec![0x30, 0x82, 1, 2, 3, 4, 5];
            let sealed = seal(&der).unwrap();
            assert_eq!(open(sealed.clone()).unwrap(), der);
            assert_eq!(open(der.clone()).unwrap(), der, "plain file");
            #[cfg(windows)]
            {
                assert!(sealed.starts_with(MAGIC));
                assert!(!sealed.windows(der.len()).any(|w| w == der.as_slice()));
            }
        }
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
        // Windows: the profile's ACL already keeps others out of
        // %APPDATA%; the private key is sealed with DPAPI on top.
        #[cfg(not(unix))]
        let _ = private;
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

    /// The com key of the test identities.
    fn com() -> ComKey {
        ComKey::from_bytes([3; icu_proto::com::COM_KEY_LEN])
    }

    /// A passport of `key`, `leaf` and `ca` with `settings`, unsigned — the
    /// shape of a personal export, not a registration bundle.
    fn passport(key: Vec<u8>, leaf: Vec<u8>, ca: Vec<u8>, settings: &str) -> String {
        icu_proto::passport::Passport {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
            com_key: com(),
            settings: settings.into(),
            signature: Vec::new(),
        }
        .encode()
    }

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
        s.audio.attack_ms = 80;
        s.audio.voice_opt = true;
        s.audio.voice_strength = VoiceStrength::Strong;
        s.audio.normalize_incoming = false;
        s.audio.auto_level = false;
        s.audio.normalize_outgoing = false;
        s.audio.input_device = Some("alsa_input.foo".into());
        s.hotkeys.ptt.key = "BTN_SIDE".into();
        s.overlay.corner = Corner::BottomLeft;
        s.overlay.expand = Expand::Horizontal;
        s.overlay.dim_non_speaking = false;
        s.overlay.size = 360;
        s.overlay.speaking_size = Some(300);
        s.overlay.speaking_frame = false;
        s.overlay.frame_color = [0x10, 0x20, 0x30];
        s.overlay.shy = true;
        s.overlay.shy_hold_ms = 2500;
        s.overlay.shy_return_ms = 7500;
        s.overlay.show_self = true;
        s.set_volume(7, 120.0);
        s.set_volume(9, 100.0);
        s.ui.name = Some("chris".into());
        s.ui.setup_done = true;
        s.ui.close_action = "minimize".into();
        s.save(&paths).unwrap();

        let back = Settings::load(&paths).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.volume(7), 120.0);
        assert_eq!(back.volume(9), 100.0, "100 % is not stored");
        assert!(!back.volumes.contains_key("9"));
        assert!(!back.overlay.dim_non_speaking);
        assert_eq!(back.overlay.size, 360);
        assert_eq!(
            back.overlay.speaking_diameter(),
            300,
            "smaller than at rest is allowed"
        );
        assert!(!back.overlay.speaking_frame);
        assert_eq!(back.overlay.frame_color, [0x10, 0x20, 0x30]);
        assert!(back.overlay.shy);
        assert_eq!(back.overlay.shy_hold_ms, 2500);
        assert_eq!(back.overlay.shy_return_ms, 7500);
        assert!(back.overlay.show_self);

        let text = fs::read_to_string(paths.settings()).unwrap();
        assert!(text.contains("mode = \"ptt\""));
        assert!(text.contains("voice_strength = \"strong\""));
        assert!(text.contains("corner = \"bottom-left\""));
        assert!(text.contains("size = 360"), "a size is written as pixels");
        assert!(text.contains("speaking_size = 300"));
        assert!(text.contains("[volumes]"));
        assert!(text.contains("setup_done = true"));
        assert!(text.contains("close_action = \"minimize\""));
    }

    #[test]
    fn shy_return_delay_is_clamped_to_the_slider_range() {
        let mut s = Settings::default();
        s.overlay.shy_return_ms = u32::MAX;
        s.clamp_numbers();
        assert_eq!(s.overlay.shy_return_ms, *SHY_RETURNS.end());
        s.overlay.shy_return_ms = 0;
        s.clamp_numbers();
        assert_eq!(s.overlay.shy_return_ms, 0);
        let old: Settings = toml::from_str("[overlay]\nshy = true\nshy_hold_ms = 2500\n").unwrap();
        assert_eq!(old.overlay.shy_return_ms, 3000);
    }

    #[test]
    fn fresh_install_sends_by_voice_with_auto_threshold() {
        let a = AudioSettings::default();
        assert_eq!(a.mode, ModeSetting::Vad);
        assert!(a.vad_auto);
        assert!(a.auto_level);
        assert!(a.normalize_outgoing);
    }

    #[test]
    fn unknown_keys_and_missing_sections_are_tolerated() {
        let s: Settings = toml::from_str("[audio]\nmode = \"vad\"\n[future]\nx = 1\n").unwrap();
        assert_eq!(s.audio.mode, ModeSetting::Vad);
        assert_eq!(s.audio.release_ms, 150);
        assert_eq!(s.audio.hangover_ms, 1000);
        assert_eq!(s.audio.attack_ms, 40, "a file from before the setting");
        assert!(s.audio.vad_auto);
        assert!(s.audio.voice_opt, "voice optimization default on");
        assert!(s.audio.normalize_incoming);
        assert!(s.audio.auto_level);
        assert!(s.audio.normalize_outgoing);
        assert_eq!(s.audio.voice_strength, VoiceStrength::Medium);
        assert!(s.overlay.dim_non_speaking, "old settings.toml default on");
        assert!(!s.overlay.shy, "old settings.toml: tiles stay put");
        assert_eq!(s.overlay.shy_hold_ms, 1000);
        assert_eq!(s.overlay.shy_return_ms, 3000);
        assert!(!s.overlay.show_self);
        assert_eq!(s.overlay.size, 120);
        assert_eq!(s.overlay.speaking_size, None);
        assert_eq!(
            s.overlay.speaking_diameter(),
            144,
            "20 % more while speaking"
        );
        for (name, px) in [("small", 120), ("medium", 240), ("large", 360)] {
            let s: Settings = toml::from_str(&format!("[overlay]\nsize = \"{name}\"\n")).unwrap();
            assert_eq!(s.overlay.size, px, "the old {name} size");
            assert_eq!(s.overlay.speaking_diameter(), px * 6 / 5, "grows as it did");
        }
        assert_eq!(s.ui.close_action, "tray");
        assert!(
            !s.ui.setup_done,
            "a file from before the setup shows it once"
        );
        assert!(
            !Settings::default().ui.setup_done,
            "a fresh install shows it"
        );
    }

    /// A CA named like a current server's and a leaf with the URI SAN, as
    /// `(leaf, ca, key)` DER.
    fn issued(uri: Option<&str>, ca_cn: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        issued_as(uri, ca_cn, "alice", |_| {})
    }

    /// [`issued`] for the leaf name `cn`, with `leaf` adjusting the leaf's
    /// parameters (OU, validity).
    fn issued_as(
        uri: Option<&str>,
        ca_cn: &str,
        cn: &str,
        leaf: impl FnOnce(&mut rcgen::CertificateParams),
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
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
        params.distinguished_name.push(DnType::CommonName, cn);
        if let Some(uri) = uri {
            params.subject_alt_names = vec![SanType::URI(uri.to_string().try_into().unwrap())];
        }
        leaf(&mut params);
        let leaf = params.signed_by(&key, &issuer).unwrap();
        (leaf.der().to_vec(), ca.der().to_vec(), key.serialize_der())
    }

    /// [`issued_as`], plus a real CA signature over the resulting passport's
    /// signable bytes — the shape a genuine registration bundle has
    /// (`registration::issue` in icu-server signs the same way, with the
    /// same CA key that issued the leaf).
    fn issued_and_signed(
        uri: Option<&str>,
        ca_cn: &str,
        cn: &str,
        leaf: impl FnOnce(&mut rcgen::CertificateParams),
    ) -> icu_proto::passport::Passport {
        use rcgen::{
            BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, SanType, SigningKey,
        };
        let ca_key = KeyPair::generate().unwrap();
        let ca_key_pem = ca_key.serialize_pem();
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        ca_params.distinguished_name = rcgen::DistinguishedName::new();
        ca_params.distinguished_name.push(DnType::CommonName, ca_cn);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, cn);
        if let Some(uri) = uri {
            params.subject_alt_names = vec![SanType::URI(uri.to_string().try_into().unwrap())];
        }
        leaf(&mut params);
        let leaf_cert = params.signed_by(&key, &issuer).unwrap();

        let mut bundle = icu_proto::passport::Passport {
            key_der: key.serialize_der(),
            leaf_der: leaf_cert.der().to_vec(),
            ca_der: ca.der().to_vec(),
            com_key: com(),
            settings: String::new(),
            signature: Vec::new(),
        };
        // Reloaded from PEM rather than reusing `ca_key`, which `Issuer`
        // above already took ownership of — same reload `icu-server`'s `Ca`
        // does when it needs its key twice.
        let signing_key = KeyPair::from_pem(&ca_key_pem).unwrap();
        bundle.signature = signing_key.sign(&bundle.signable_bytes()).unwrap();
        bundle
    }

    /// A passport is refused before anything is replaced when its key does
    /// not belong to its certificate, its certificate is not from the CA it
    /// brings, an invitation has expired, or a name it shows is not plain
    /// text (review 2026-09-27, C-2, C-4). An expired own certificate is
    /// taken: the server renews it.
    #[test]
    fn a_passport_that_cannot_work_is_refused_before_the_import() {
        const URI: Option<&str> = Some("icu://icu.example.org:4711");
        const CA: &str = "ICU CA icu.example.org";
        let (leaf, ca, key) = issued(URI, CA);
        assert!(
            Identity::from_passport(&passport(key.clone(), leaf.clone(), ca.clone(), "")).is_ok()
        );

        let (other_leaf, other_ca, other_key) = issued(URI, CA);
        let err = Identity::from_passport(&passport(other_key, leaf.clone(), ca.clone(), ""))
            .unwrap_err();
        assert!(err.contains("gehören nicht zusammen"), "{err}");
        // The same CA name, another CA key.
        let err = Identity::from_passport(&passport(key.clone(), leaf.clone(), other_ca, ""))
            .unwrap_err();
        assert!(err.contains("mitgelieferten CA"), "{err}");
        let err = Identity::from_passport(&passport(key, other_leaf, ca, "")).unwrap_err();
        assert!(err.contains("gehören nicht zusammen"), "{err}");

        let expired = |p: &mut rcgen::CertificateParams| {
            p.not_before = rcgen::date_time_ymd(2020, 1, 1);
            p.not_after = rcgen::date_time_ymd(2021, 1, 1);
        };
        let expired_registration = issued_and_signed(URI, CA, "neu-1a2b", |p| {
            expired(p);
            p.distinguished_name.push(
                rcgen::DnType::OrganizationalUnitName,
                crate::net::register::REGISTRATION_OU,
            );
        });
        let err = Identity::from_passport(&expired_registration.encode()).unwrap_err();
        assert!(err.contains("Einladung ist abgelaufen"), "{err}");
        let (leaf, ca, key) = issued_as(URI, CA, "alice", expired);
        assert!(
            Identity::from_passport(&passport(key, leaf, ca, "")).is_ok(),
            "an expired own certificate is renewed, not refused"
        );

        for (cn, ca_cn) in [
            ("alice\nAdresse: evil.example:4711", CA),
            ("ali\u{202E}ec", CA),
            (
                "alice",
                "ICU CA icu.example.org\nAdresse: evil.example:4711",
            ),
            ("alice", "ICU CA \u{1b}[2Jicu.example.org"),
        ] {
            let (leaf, ca, key) = issued_as(URI, ca_cn, cn, |_| {});
            let err = Identity::from_passport(&passport(key, leaf, ca, "")).unwrap_err();
            assert!(
                err.contains("Steuer- oder Formatzeichen"),
                "{cn:?} {ca_cn:?}: {err}"
            );
        }
    }

    /// The concrete guarantee the signature buys (`docs/reference/protocol.md` §2.3.1): a
    /// registration bundle needs one at all, and it must cover every field,
    /// `com_key` included — nothing else here checks that one.
    #[test]
    fn a_registration_bundle_needs_a_valid_signature() {
        const URI: Option<&str> = Some("icu://icu.example.org:4711");
        const CA: &str = "ICU CA icu.example.org";
        let registration = |p: &mut rcgen::CertificateParams| {
            p.distinguished_name.push(
                rcgen::DnType::OrganizationalUnitName,
                crate::net::register::REGISTRATION_OU,
            );
        };

        // No signature at all: the unsigned `passport()` helper stands in
        // for a stripped or never-signed bundle.
        let (leaf, ca, key) = issued_as(URI, CA, "neu-1a2b", registration);
        let err = Identity::from_passport(&passport(key, leaf, ca, "")).unwrap_err();
        assert!(err.contains("Signatur"), "{err}");

        // A genuinely signed bundle is accepted.
        let bundle = issued_and_signed(URI, CA, "neu-1a2b", registration);
        assert!(Identity::from_passport(&bundle.encode()).is_ok());

        // Tampering with `com_key` after signing — the field nothing else
        // here checks — invalidates the signature and is refused, not
        // silently accepted.
        let mut tampered = bundle.clone();
        tampered.com_key = ComKey::from_bytes([tampered.com_key.as_bytes()[0] ^ 1; 32]);
        let err = Identity::from_passport(&tampered.encode()).unwrap_err();
        assert!(err.contains("Signatur"), "{err}");

        // A flipped signature byte, same result.
        let mut flipped = bundle.clone();
        let last = flipped.signature.len() - 1;
        flipped.signature[last] ^= 1;
        let err = Identity::from_passport(&flipped.encode()).unwrap_err();
        assert!(err.contains("Signatur"), "{err}");

        // A personal export (no registration OU) needs none, exactly as
        // before this feature: unsigned still imports.
        let (leaf, ca, key) = issued(URI, CA);
        assert!(Identity::from_passport(&passport(key, leaf, ca, "")).is_ok());
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
        let bundle = passport(key, leaf, ca, "");
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
            com_key: com(),
            server,
        };
        let summary = ImportSummary::of(&incoming, None);
        assert_eq!(summary.server_name, "icu.example.org");
        assert_eq!(summary.address, "icu.example.org:4711");
        assert!(!summary.registration);
        assert!(
            !summary.ca_changed,
            "nothing to compare with on first import"
        );
        let lines = summary.lines();
        assert_eq!(lines.len(), 5);
        // The fingerprint, in halves of 16 pairs.
        assert_eq!(
            format!("{}:{}", &lines[3][9..], lines[4].trim()),
            summary.ca_fingerprint
        );
        assert_eq!(summary.ca_fingerprint.split(':').count(), 32);
        assert!(!ImportSummary::of(&incoming, Some(&incoming)).ca_changed);

        let (leaf, ca, key) = issued(Some("icu://[2001:db8::1]:4711"), "ICU CA other");
        let current = Identity {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
            com_key: com(),
            server: ServerInfo {
                host: "2001:db8::1".into(),
                port: 4711,
                server_name: "other".into(),
                name: "alice".into(),
            },
        };
        assert_eq!(
            ImportSummary::of(&current, None).address,
            "[2001:db8::1]:4711"
        );
        let changed = ImportSummary::of(&incoming, Some(&current));
        assert!(changed.ca_changed);
        assert!(changed.lines().last().unwrap().contains("anderer Server"));
    }

    /// A CA certificate renewed on the same key and subject
    /// (`icu-server renew-ca`) is the same server; another key or another
    /// name is not (review 2026-09-27, C-5).
    #[test]
    fn a_renewed_ca_is_not_another_server() {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
        let ca = |key: &KeyPair, cn: &str, year: i32| {
            let mut params = CertificateParams::default();
            params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
            params.distinguished_name = rcgen::DistinguishedName::new();
            params.distinguished_name.push(DnType::CommonName, cn);
            params.not_after = rcgen::date_time_ymd(year, 1, 1);
            params.self_signed(key).unwrap().der().to_vec()
        };
        let key = KeyPair::generate().unwrap();
        let before = ca(&key, "ICU CA icu.example.org", 2030);
        let renewed = ca(&key, "ICU CA icu.example.org", 2040);
        assert_ne!(before, renewed, "a new certificate");
        assert!(same_ca(&before, &renewed));
        let other_key = ca(
            &KeyPair::generate().unwrap(),
            "ICU CA icu.example.org",
            2030,
        );
        assert!(!same_ca(&before, &other_key));
        let other_name = ca(&key, "ICU CA other.example.org", 2030);
        assert!(!same_ca(&before, &other_name));
        assert!(same_ca(b"junk", b"junk"));
        assert!(!same_ca(b"junk", &before));

        let identity = |ca_der: Vec<u8>| {
            let (leaf, _, key) = issued(Some("icu://icu.example.org:4711"), "ICU CA x");
            Identity {
                server: ServerInfo::from_certs(&leaf, &ca_der).unwrap(),
                key_der: key,
                leaf_der: leaf,
                ca_der,
                com_key: com(),
            }
        };
        let (old, new) = (identity(before.clone()), identity(renewed));
        assert!(!ImportSummary::of(&new, Some(&old)).ca_changed);
        assert!(ImportSummary::of(&identity(other_key), Some(&old)).ca_changed);
    }

    /// A passport reads back to the same identity, com key included; the
    /// certificates say where the server is.
    #[test]
    fn a_passport_reads_back_to_the_same_identity() {
        let (leaf, ca, key) = issued(Some("icu://icu.example.org:4711"), "ICU CA icu.example.org");
        let server = ServerInfo::from_certs(&leaf, &ca).unwrap();
        let identity = Identity {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
            com_key: com(),
            server,
        };
        let text = identity.to_passport();
        assert_eq!(Identity::from_passport(&text).unwrap(), identity);
        assert!(!identity.is_registration());

        // Without an address in the certificate there is nowhere to go.
        let (leaf, ca, key) = issued(None, "ICU CA icu.example.org");
        assert!(Identity::from_passport(&passport(key, leaf, ca, "")).is_err());

        // Under an old CA without an SNI in its CN, a renewal moves host and
        // port, never the SNI.
        let mut server = ServerInfo {
            host: "192.0.2.10".into(),
            port: 4711,
            server_name: "icu.example.org".into(),
            name: "alice".into(),
        };
        let (leaf, ca, _) = issued(Some("icu://198.51.100.7:5000"), "ICU CA");
        server.update_from_certs(&leaf, &ca);
        assert_eq!((server.host.as_str(), server.port), ("198.51.100.7", 5000));
        assert_eq!(server.server_name, "icu.example.org");

        let broken = passport(
            vec![1, 2, 3],
            identity.leaf_der.clone(),
            identity.ca_der.clone(),
            "",
        );
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
            com_key: com(),
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
            let com_key = paths.identity_dir().join(COM_KEY_FILE);
            let mode = fs::metadata(com_key).unwrap().permissions().mode() & 0o777;
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
        assert!(!paths.identity_dir().exists(), "load only reads");
        // The next save starts from there and keeps the old set until the new
        // one is in place.
        id.save(&paths).unwrap();
        assert_eq!(Identity::load(&paths).unwrap().unwrap(), id);
        assert!(!paths.identity_dir().with_extension("old").exists());

        // Logging out leaves nothing `load` could revive.
        other.save(&paths).unwrap();
        fs::create_dir_all(paths.identity_dir().with_extension("old")).unwrap();
        fs::write(
            paths
                .identity_dir()
                .with_extension("old")
                .join("server.toml"),
            "",
        )
        .unwrap();
        Identity::remove(&paths).unwrap();
        assert!(Identity::load(&paths).unwrap().is_none());
        assert!(!paths.identity_dir().with_extension("old").exists());
        Identity::remove(&paths).unwrap();

        // An identity from before the com key does not load.
        id.save(&paths).unwrap();
        fs::remove_file(paths.identity_dir().join(COM_KEY_FILE)).unwrap();
        assert!(matches!(
            Identity::load(&paths),
            Err(ConfigError::Parse(e)) if e.contains("com key")
        ));
    }

    /// The window reads the identity (passport export) while the engine
    /// saves a renewed one: every read sees one complete set, and no save
    /// fails for it.
    #[test]
    fn loading_while_saving_sees_whole_sets() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let set = |n: u8| Identity {
            key_der: vec![n; 32],
            leaf_der: vec![n; 16],
            ca_der: vec![9],
            com_key: com(),
            server: ServerInfo {
                host: "icu.example.org".into(),
                port: 4711,
                server_name: "icu.example.org".into(),
                name: format!("user{n}"),
            },
        };
        set(1).save(&paths).unwrap();
        let saver = {
            let paths = paths.clone();
            std::thread::spawn(move || {
                for i in 0..60 {
                    set(1 + (i % 2) as u8).save(&paths).expect("save");
                }
            })
        };
        while !saver.is_finished() {
            let id = Identity::load(&paths).expect("load").expect("present");
            assert_eq!(id.key_der[0], id.leaf_der[0], "key and leaf of one set");
            assert_eq!(id.server.name, format!("user{}", id.key_der[0]));
        }
        saver.join().unwrap();
    }

    /// Logging out takes the saved invitations with it: each one is a live
    /// registration key.
    /// An import that has yet to prove itself keeps the set it replaced:
    /// restored, the old identity is back in force; dropped, the new one
    /// stays alone (review 2026-09-27, C-2).
    #[test]
    fn an_import_on_trial_keeps_the_previous_identity() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let identity = |cn: &str| {
            let (leaf, ca, key) = issued_as(
                Some("icu://icu.example.org:4711"),
                "ICU CA icu.example.org",
                cn,
                |_| {},
            );
            Identity::from_passport(&passport(key, leaf, ca, "")).unwrap()
        };
        let (alice, bob) = (identity("alice"), identity("bob"));
        let previous = paths.identity_dir().with_extension("old");
        assert!(Identity::restore_previous(&paths).is_err(), "nothing kept");

        alice.save(&paths).unwrap();
        bob.save_keeping_previous(&paths).unwrap();
        assert_eq!(Identity::load(&paths).unwrap().unwrap(), bob);
        assert!(previous.join("server.toml").exists());
        Identity::restore_previous(&paths).unwrap();
        assert_eq!(Identity::load(&paths).unwrap().unwrap(), alice);
        assert!(!previous.exists());
        assert!(!paths.identity_dir().with_extension("new").exists());

        bob.save_keeping_previous(&paths).unwrap();
        Identity::drop_previous(&paths).unwrap();
        assert!(!previous.exists());
        assert_eq!(Identity::load(&paths).unwrap().unwrap(), bob);
        assert!(Identity::restore_previous(&paths).is_err());
        assert_eq!(Identity::load(&paths).unwrap().unwrap(), bob);

        // A plain save does not keep it.
        alice.save(&paths).unwrap();
        assert!(!previous.exists());
    }

    #[test]
    fn logging_out_deletes_saved_invitations() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let (leaf, ca, key) = issued(Some("icu://icu.example.org:4711"), "ICU CA x");
        let bundle = passport(key, leaf, ca, "");
        let saved = save_invite(&paths, &bundle).unwrap();
        assert!(saved.exists());
        Identity::remove(&paths).unwrap();
        assert!(!paths.invites_dir().exists());
    }

    /// A settings file that does not parse is moved aside and the defaults
    /// apply, with a readable problem for the window.
    #[test]
    fn an_unreadable_settings_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        fs::write(paths.settings(), "[audio\nmode = ").unwrap();
        assert!(matches!(Settings::load(&paths), Err(ConfigError::Parse(_))));
        let (settings, problem) = Settings::load_or_recover(&paths).unwrap();
        assert_eq!(settings, Settings::default());
        let problem = problem.expect("a banner");
        crate::problem::tests::assert_readable(&problem);
        assert!(
            problem.text.contains("settings.toml.broken-"),
            "{}",
            problem.text
        );
        assert!(!paths.settings().exists());
        let aside: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("settings.toml.broken-")
            })
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(
            fs::read_to_string(aside[0].path()).unwrap(),
            "[audio\nmode = "
        );
        // A good file, or none, reads as before, without a problem.
        assert_eq!(Settings::load_or_recover(&paths).unwrap().1, None);
    }

    /// Settings on one machine, with devices, camera and the rest of what
    /// stays behind set to something recognisable.
    fn machine(tag: &str) -> Settings {
        let mut s = Settings::default();
        s.audio.input_device = Some(format!("mic-{tag}"));
        s.audio.output_device = Some(format!("speaker-{tag}"));
        s.hotkeys.backend = HotkeyBackend::Evdev;
        s.camera.device = Some(format!("cam-{tag}"));
        s.camera.enabled = true;
        s.ui.register_uri_handler = false;
        s.ui.setup_done = true;
        s.ui.name = Some(tag.into());
        s
    }

    /// A passport takes the person's settings along and leaves the
    /// machine's behind, on both ends.
    #[test]
    fn a_passport_moves_the_person_not_the_machine() {
        let mut exported = machine("a");
        exported.audio.mode = ModeSetting::Ptt;
        exported.audio.hangover_ms = 1500;
        exported.hotkeys.ptt.key = "BTN_SIDE".into();
        exported.overlay.corner = Corner::BottomLeft;
        exported.ui.theme = "dark".into();
        exported.ui.close_action = "quit".into();
        exported.set_volume(7, 140.0);

        let portable = exported.portable();
        assert_eq!(portable.audio.input_device, None);
        assert_eq!(portable.audio.output_device, None);
        assert_eq!(portable.camera, CameraSettings::default());
        assert_eq!(portable.hotkeys.backend, HotkeyBackend::Auto);
        assert_eq!(portable.ui.name, None);
        assert_eq!(portable.audio.mode, ModeSetting::Ptt);

        let mut here = machine("b");
        here.set_volume(9, 50.0);
        here.take_portable(portable);
        // The person's settings arrived …
        assert_eq!(here.audio.mode, ModeSetting::Ptt);
        assert_eq!(here.audio.hangover_ms, 1500);
        assert_eq!(here.hotkeys.ptt.key, "BTN_SIDE");
        assert_eq!(here.overlay.corner, Corner::BottomLeft);
        assert_eq!(here.ui.theme, "dark");
        assert_eq!(here.ui.close_action, "quit");
        assert_eq!(here.volume(7), 140.0);
        assert_eq!(here.volume(9), 100.0, "volumes come as a whole");
        // … and this machine kept its own.
        let b = machine("b");
        assert_eq!(here.audio.input_device, b.audio.input_device);
        assert_eq!(here.audio.output_device, b.audio.output_device);
        assert_eq!(here.camera, b.camera);
        assert_eq!(here.hotkeys.backend, HotkeyBackend::Evdev);
        assert!(!here.ui.register_uri_handler);
        assert!(here.ui.setup_done);
        assert_eq!(here.ui.name.as_deref(), Some("b"));
    }

    /// Imported numbers land in the ranges the settings window offers.
    /// The import confirmation names every change to when this device can
    /// be heard — here a passport that makes the microphone hot — and
    /// nothing when those settings stay (review 2026-09-27, C-3).
    #[test]
    fn the_import_names_what_changes_about_sending() {
        let here = Settings::default();
        assert!(here.send_changes(&here).is_empty());
        let mut hot = Settings::default();
        hot.audio.vad_auto = false;
        hot.audio.vad_threshold = 0.0;
        hot.audio.gain_db = 20.0;
        hot.audio.denoise = false;
        hot.audio.hangover_ms = 3000;
        hot.audio.cue_join = false;
        hot.audio.cue_mute = false;
        hot.ui.start_minimized = true;
        // Nothing about sending: not listed.
        hot.ui.theme = "dark".into();
        let lines = here.send_changes(&hot);
        assert_eq!(
            lines,
            [
                "Schwelle: 0 % (bisher automatisch)",
                "Verstärkung: +20 dB (bisher +0 dB)",
                "Rauschunterdrückung: aus (bisher an)",
                "VAD-Nachlauf: 3000 ms (bisher 1000 ms)",
                "Ton „Jemand kommt oder geht“: aus (bisher an)",
                "Ton „Stummschalten“: aus (bisher an)",
                "Minimiert starten: an (bisher aus)",
            ]
        );
        let mut ptt = Settings::default();
        ptt.audio.mode = ModeSetting::Ptt;
        assert_eq!(
            hot.send_changes(&ptt)[0],
            "Senden: Taste halten (bisher Automatisch)"
        );
        // What is out of range is shown as it will be taken: clamped.
        let mut loud = Settings::default();
        loud.audio.gain_db = 99.0;
        assert_eq!(
            here.send_changes(&loud),
            ["Verstärkung: +20 dB (bisher +0 dB)"]
        );
    }

    #[test]
    fn imported_numbers_are_clamped() {
        let mut incoming = Settings::default();
        incoming.audio.output_volume = 1e9;
        incoming.audio.vad_threshold = -3.0;
        incoming.audio.gain_db = f32::NAN;
        incoming.audio.release_ms = 99_999;
        incoming.audio.hangover_ms = 1_000_000;
        incoming.audio.attack_ms = 1_000_000;
        incoming.audio.frame_ms = 7;
        incoming.volumes.insert("5".into(), 1e9);
        incoming.volumes.insert("6".into(), f32::INFINITY);
        let mut here = Settings::default();
        here.take_portable(incoming);
        assert_eq!(here.audio.output_volume, 200.0);
        assert_eq!(here.audio.vad_threshold, 0.0);
        assert_eq!(here.audio.gain_db, 0.0);
        assert_eq!(here.audio.release_ms, 1000);
        assert_eq!(here.audio.hangover_ms, 3000);
        assert_eq!(here.audio.attack_ms, 200);
        assert_eq!(here.audio.frame_ms, 20);
        assert_eq!(here.volume(5), 200.0);
        assert_eq!(here.volume(6), 100.0);
    }

    /// A passport carries the settings and reads back to the same identity;
    /// settings that would not fit stay behind.
    #[test]
    fn passport_export_carries_the_settings_where_it_can() {
        let (leaf, ca, key) = issued(Some("icu://icu.example.org:4711"), "ICU CA icu.example.org");
        let server = ServerInfo::from_certs(&leaf, &ca).unwrap();
        let identity = Identity {
            key_der: key,
            leaf_der: leaf,
            ca_der: ca,
            com_key: com(),
            server,
        };
        let mut settings = machine("a");
        settings.audio.mode = ModeSetting::Ptt;
        settings.hotkeys.ptt.trigger = Some("CTRL+ALT+SHIFT+SUPER+F12".repeat(4));
        // A server's worth of people, each with a volume of their own.
        for user_id in 1..=64 {
            settings.set_volume(1000 + user_id, 150.0);
        }
        let (text, included) = identity.to_passport_with_settings(&settings);
        assert!(included, "a full settings file fits");
        let (back, carried) = Identity::from_passport_with_settings(&text).unwrap();
        assert_eq!(back, identity);
        assert_eq!(carried, Some(settings.portable()));
        assert_eq!(Identity::from_passport(&text).unwrap(), identity);

        let mut huge = settings.clone();
        huge.ui.theme = "x".repeat(icu_proto::passport::MAX_PASSPORT_BYTES);
        let (text, included) = identity.to_passport_with_settings(&huge);
        assert!(!included, "too large: the identity goes alone");
        assert_eq!(Identity::from_passport(&text).unwrap(), identity);
    }

    /// Settings that do not read do not cost the identity.
    #[test]
    fn unreadable_passport_settings_leave_the_identity() {
        let (leaf, ca, key) = issued(Some("icu://icu.example.org:4711"), "ICU CA icu.example.org");
        for settings in [
            "[audio\nmode = ",
            "[audio]\nframe_ms = \"zwanzig\"\n",
            "  \n",
        ] {
            let text = passport(key.clone(), leaf.clone(), ca.clone(), settings);
            let (identity, carried) = Identity::from_passport_with_settings(&text).unwrap();
            assert_eq!(identity.server.host, "icu.example.org");
            assert_eq!(carried, None, "{settings:?}");
        }
    }

    /// Without a running engine the settings go straight into the file,
    /// around this machine's devices; a fresh install has no file yet.
    #[test]
    fn imported_settings_are_stored_around_this_machine() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let mut incoming = machine("a").portable();
        incoming.audio.mode = ModeSetting::Ptt;
        import_settings(&paths, incoming.clone()).unwrap();
        let fresh = Settings::load(&paths).unwrap();
        assert_eq!(fresh.audio.mode, ModeSetting::Ptt);
        assert_eq!(fresh.audio.input_device, None);
        assert!(!fresh.ui.setup_done, "a new machine still runs the setup");

        machine("b").save(&paths).unwrap();
        import_settings(&paths, incoming).unwrap();
        let kept = Settings::load(&paths).unwrap();
        assert_eq!(kept.audio.mode, ModeSetting::Ptt);
        assert_eq!(kept.audio.input_device.as_deref(), Some("mic-b"));
        assert_eq!(kept.camera.device.as_deref(), Some("cam-b"));
    }
}
