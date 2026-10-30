//! The engine: connection loop, DSP thread, playout mixer, state and command
//! bus (`docs/reference/client.md` §4, §5, §6).
//!
//! Threads:
//! * backend callbacks (real-time) → ring buffers only;
//! * `icu-dsp`: 480-sample blocks, AEC → denoise → VAD/gate → Opus → datagram;
//! * `icu-mixer`: jitter buffers → decoders → mixer/limiter → playback ring;
//! * `icu-net`: a tokio runtime running the connection with reconnect.
//!
//! The UI only reads a `watch<AppState>` and sends [`Command`]s.

use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use icu_proto::{
    error as codes, header::AUDIO_FLAG_END, limits as pl, AudioHeader, ControlMsg, Rgb, Session,
    UserFlags, UserId, UserInfo,
};
use ringbuf::{
    traits::{Consumer, Observer, Producer, Split},
    HeapCons, HeapProd, HeapRb,
};
use tokio::sync::{mpsc, watch};

use crate::{
    audio::{AudioBackend, BackendEvent, DeviceInfo, DeviceKind, StreamKind},
    config::{Identity, Paths, Settings},
    dsp::{
        aec::{Aec, AecStats},
        chain::{Analysis, SendChain, SendSettings},
        gate::{Decision, Gate, Mode},
        jitter::{JitterBuffer, JitterConfig, Packet, Pop},
        meter::{self, Level},
        mixer,
        normalize::Normalizer,
        opus,
        preroll::{self, PreRoll},
        refalign::{self, CycleClock},
        vad, vad_auto,
        voice::VoiceStrength,
    },
    net::{
        conn::{self, ConnectError},
        NetError,
    },
    netcond::{NetworkTracker, Sample},
    problem::Problem,
    quality::QualityController,
    stats::DebugStats,
    sync::lock,
    video::VideoTier,
};

/// Samples per DSP block (10 ms).
pub const BLOCK: usize = 480;
/// Capture samples the DSP thread may fall behind (100 ms) before it skips
/// to the newest two blocks: bounds send latency after a stall of the thread
/// and keeps its catch-up burst far below an RTTIME limit. A graph cycle
/// larger than that raises it ([`capture_backlog_limit`]).
pub const MAX_CAPTURE_BACKLOG: usize = 10 * BLOCK;

/// The capture backlog the DSP thread tolerates with the backend delivering
/// `cycle` samples at a time: [`MAX_CAPTURE_BACKLOG`], or a whole cycle and
/// two blocks. A quantum above 4320 samples (forced, pro audio) arrives in
/// one piece; held to the fixed cap, every cycle was trimmed to two blocks
/// and most of the microphone thrown away.
fn capture_backlog_limit(cycle: usize) -> usize {
    MAX_CAPTURE_BACKLOG.max(cycle + 2 * BLOCK)
}

/// `skipped` capture samples were thrown away ahead of the frame being
/// collected: that frame is dropped too, and `ts` moves past both, so the
/// sample clock stays true and receivers see a pause — they start a new
/// spurt — instead of a delay spike, which their jitter buffers remember
/// for 20–40 s.
fn skip_capture(acc: &mut Vec<f32>, ts: &mut u32, skipped: usize) {
    *ts = ts.wrapping_add((acc.len() + skipped) as u32);
    acc.clear();
}

/// Discard exactly the observed backlog; callbacks may append concurrently.
fn discard_capture(cap: &mut HeapCons<f32>, acc: &mut Vec<f32>, ts: &mut u32, lost: usize) {
    let queued = cap.occupied_len();
    cap.skip(queued);
    skip_capture(acc, ts, queued + lost);
}

/// How the engine is started.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Enrolled identity.
    pub identity: Identity,
    /// Settings (the engine keeps and persists its own copy).
    pub settings: Settings,
    /// Where to persist settings.
    pub paths: Paths,
    /// Replaces DNS resolution of the identity's host (tests).
    pub server_addr: Option<SocketAddr>,
    /// Replace the microphone with a synthetic voice-like signal and keep the
    /// gate open.
    pub test_tone: bool,
    /// `Hello.client`.
    pub client_string: String,
    /// A problem found before the engine started — a `settings.toml` that
    /// did not parse (`Settings::load_or_recover`) — shown as the first
    /// banner.
    pub banner: Option<Problem>,
}

impl EngineConfig {
    /// Defaults for `identity` with everything else stock.
    pub fn new(identity: Identity, settings: Settings, paths: Paths) -> EngineConfig {
        EngineConfig {
            identity,
            settings,
            paths,
            server_addr: None,
            test_tone: false,
            client_string: client_string(),
            banner: None,
        }
    }
}

/// What this build sends as `Hello.client`: `icu/<version> <os>`.
pub fn client_string() -> String {
    format!("icu/{} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS)
}

/// Connection status for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnStatus {
    /// Not connected, will retry after `retry_in`.
    Disconnected {
        /// Why, worded for the user.
        reason: Problem,
        /// Attempt counter.
        attempt: u32,
    },
    /// Handshake or waiting for `Welcome`.
    Connecting,
    /// `Welcome` received.
    Connected,
    /// Fatal close; no reconnect.
    Fatal(Problem),
}

/// One participant as the UI sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Peer {
    /// Wire info.
    pub info: UserInfo,
    /// Speaking, from the local datagram path or the server flag.
    pub speaking: bool,
    /// Volume in percent.
    pub volume: f32,
    /// Server-measured RTT to this peer, milliseconds. `None` until the
    /// first `ControlMsg::UserRtt` arrives (`docs/reference/protocol.md` §2.2).
    pub rtt_ms: Option<u16>,
    /// What this client loses of *this* peer's audio, per mille, as the
    /// jitter buffer last measured it. Already computed for `LossReport`;
    /// kept here so the Verbindung window can show it per participant.
    pub recv_loss_permille: Option<u16>,
    /// On-demand public certificate, scoped to this exact live session.
    pub certificate: Option<Result<Arc<crate::conninfo::CertInfo>, String>>,
}

impl Peer {
    /// A peer as `Welcome` or `UserJoined` announces it: nothing measured
    /// yet, speaking as the server flag says.
    pub fn new(info: UserInfo, volume: f32) -> Peer {
        Peer {
            speaking: info.flags.speaking(),
            info,
            volume,
            rtt_ms: None,
            recv_loss_permille: None,
            certificate: None,
        }
    }
}

/// Everything the UI renders.
#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    /// Connection.
    pub conn: ConnStatus,
    /// `host:port` dialed (may be an IP or alternate name).
    pub server: String,
    /// The certificate-verified FQDN/IP (`Identity.server.server_name`,
    /// the same value used as TLS SNI). Distinct from `server` above.
    pub server_name: String,
    /// Own entry after `Welcome`.
    pub me: Option<UserInfo>,
    /// Everyone else.
    pub peers: Vec<Peer>,
    /// Peers that reported presenting one of our frames within three seconds.
    pub video_viewers: Vec<Session>,
    /// Local mute.
    pub muted: bool,
    /// Temporary setup inhibition, independent of the user's persisted mute.
    pub setup_active: bool,
    /// Session-local privacy guard; does not overwrite saved mute/camera choices.
    pub privacy_active: bool,
    /// Source epoch: frames from an older camera/privacy source are discarded.
    pub privacy_epoch: usize,
    /// Local deafen.
    pub deafened: bool,
    /// The OS mutes the recording device (`UserFlags::SYSTEM_MUTED`).
    pub system_muted: bool,
    /// PTT key held.
    pub ptt_down: bool,
    /// Gate open (audio is going out).
    pub sending: bool,
    /// The voice detector heard speech within the VAD hangover, muted or
    /// not: the main window warns about talking while muted, held through
    /// the gaps between words like an open gate (`voice_held`).
    pub voice: bool,
    /// Microphone level after processing.
    pub level: Level,
    /// Raw capture reached full scale within the last second, before AEC,
    /// gain or denoise. Also measured while muted or setting up.
    pub input_clipping: bool,
    /// RNNoise voice probability.
    pub vad_prob: f32,
    /// The VAD threshold currently in effect (manual, or auto-calibrated
    /// when `vad_auto` is on).
    pub vad_threshold: f32,
    /// Last RTT, as this client's own `Ping`/`Pong` measured it.
    pub rtt_ms: Option<f32>,
    /// Own round trip as the *server* measured it (`UserRtt`), which every
    /// participant sees the same way — unlike [`Self::rtt_ms`].
    pub server_rtt_ms: Option<u16>,
    /// Offset between the server's clock and ours, milliseconds, from
    /// `Pong.server_time_ms`. An estimate: it assumes a symmetric path, so
    /// it is good to about ±RTT/2.
    pub clock_offset_ms: Option<i64>,
    /// Everything about the connection that holds still for one session.
    /// `None` while not connected.
    pub conn_info: Option<std::sync::Arc<crate::conninfo::ConnInfo>>,
    /// Transport counters, refreshed every 2 s. `None` while not connected.
    pub conn_stats: Option<crate::conninfo::ConnStats>,
    /// `Welcome.min_client` is above our version.
    pub update_required: Option<String>,
    /// Non-modal error banner, dismissible.
    pub banner: Option<Problem>,
    /// Something in progress worth a line (a registration running).
    pub notice: Option<String>,
    /// Rename error (`NAME_TAKEN`, `RENEW_REJECTED`, or a local one).
    pub name_error: Option<String>,
    /// A rename the user asked for is waiting for its new certificate.
    pub rename_pending: bool,
    /// `Welcome.is_admin`: whether `CreateInviteRequest` is allowed.
    pub is_admin: bool,
    /// The most recently issued registration bundle and its expiry, if any.
    pub last_invite: Option<(String, String)>,
    /// `CreateInviteRequest` error.
    pub invite_error: Option<String>,
    /// Every registered user, from the last `UserListResponse`, for the
    /// Nutzer tab. Empty until the tab has requested it at least once.
    pub user_list: Vec<icu_proto::UserSummary>,
    /// Advances on every actual user-list response, including unchanged lists.
    /// Moderation views use it to wait for their initial refresh.
    pub user_list_generation: u64,
    /// Rejection of the most recent user-manager action (promote/demote/
    /// disable/enable/revoke) — kept separate from `invite_error` so the two
    /// admin features never clobber each other's error slot.
    pub user_action_error: Option<String>,
    /// An admin enforces our mute (`UserRestrictionChanged`): unmuting is
    /// refused until it is lifted. Cleared with the session.
    pub restricted_muted: bool,
    /// An admin enforces our deafen, like `restricted_muted`.
    pub restricted_deafened: bool,
    /// Devices.
    pub devices: Vec<DeviceInfo>,
    /// The microphone actually open: the saved choice
    /// (`settings.audio.input_device`) while the backend lists it, `None`
    /// (the system default) when it is missing — unplugged, say. The saved
    /// choice stays and is used again once the device is back.
    pub active_input: Option<String>,
    /// The playback device actually open, like [`AppState::active_input`].
    pub active_output: Option<String>,
    /// Backend name.
    pub backend: &'static str,
    /// Current automatic capture and peer-assistance status.
    pub tuning_status: String,
    /// Temporary attenuation of other applications, measured by the guardian.
    pub ducking_status: crate::audio::ducking::Status,
    pub audio_health: crate::audio::health::AudioHealth,
    pub can_restore_audio: bool,
    /// AEC could be created.
    pub aec_available: bool,
    /// AEC statistics.
    pub aec_stats: AecStats,
    /// The AEC reference stream is open (false on backends without one).
    pub aec_reference: bool,
    /// Echo is audible in the far-end signal but the AEC removes almost
    /// none of it for 10 s: the reference is probably the wrong device
    /// (`dsp::aec::Diagnosis`).
    pub aec_suspect: bool,
    /// 30 s of far-end sound without any echo path: the canceller does
    /// nothing here (a headset) and may be switched off to save 9 ms.
    pub aec_no_echo: bool,
    /// The mixer plays sound but the reference stream stays silent for
    /// 10 s: it is not capturing what is played.
    pub aec_reference_silent: bool,
    /// The mic test is on (`Command::SetMicTest`): the own processed voice
    /// plays locally. Never persisted; ends by itself after a minute.
    pub mic_test: bool,
    /// Camera state.
    pub camera_on: bool,
    /// Hotkey backend in use.
    pub hotkey_backend: String,
    /// Settings as the engine holds them.
    pub settings: Settings,
    /// Bumped by `Command::RetryHotkeys`; the hotkey supervisor
    /// (`crate::hotkey::supervise`) binds again whenever it changes.
    pub hotkey_retries: u32,
}

impl AppState {
    /// The active voice detector and its threshold on a shared meter axis.
    pub fn vad_meter(&self) -> (f32, f32) {
        vad::meter_values(
            self.settings.audio.denoise,
            self.vad_prob,
            self.vad_threshold,
            self.level.rms_db,
        )
    }

    /// Account for immediately edited switches and temporary transmission
    /// stops before the next periodic audio report reaches the UI.
    pub fn effective_audio_health(
        &self,
        audio: &crate::config::AudioSettings,
    ) -> crate::audio::health::AudioHealth {
        use crate::audio::health::State;
        let state = if !audio.auto_level || !audio.denoise {
            Some(State::Disabled)
        } else if self.conn != ConnStatus::Connected || !audio.peer_assistance {
            Some(State::LocalOnly)
        } else if self.setup_active || self.privacy_active || self.muted || self.deafened {
            Some(if self.audio_health.peer_trim_db != 0.0 {
                State::Held
            } else {
                State::NoLocalSpeech
            })
        } else {
            None
        };
        state.map_or_else(
            || self.audio_health.clone(),
            |state| health_without_feedback(&self.audio_health, state),
        )
    }
}

fn health_without_feedback(
    health: &crate::audio::health::AudioHealth,
    state: crate::audio::health::State,
) -> crate::audio::health::AudioHealth {
    crate::audio::health::AudioHealth {
        state,
        reporters: 0,
        local_correction_db: None,
        remote_correction_db: None,
        feedback_age_secs: None,
        ..health.clone()
    }
}

fn publish_audio_profile(
    health: &mut crate::audio::health::AudioHealth,
    saved_at: Option<u64>,
) -> bool {
    health.profile_saved = saved_at.is_some();
    health.profile_saved_at = saved_at;
    saved_at.is_some()
}

/// Commands from UI, tray and hotkeys.
#[derive(Debug, Clone)]
pub enum Command {
    /// A frame from this peer was presented by the overlay.
    VideoDisplayed(Session),
    /// Mute/unmute.
    SetMuted(bool),
    /// Temporarily inhibit audio and replace active camera video with TV static.
    SetPrivacy(bool),
    /// Prevent transmission during setup without changing the user's mute.
    SetSetupActive(bool),
    /// Deafen/undeafen.
    SetDeafened(bool),
    /// PTT or VAD.
    SetMode(Mode),
    /// PTT key state.
    Ptt(bool),
    /// Toggle mute.
    ToggleMute,
    /// Toggle deafen.
    ToggleDeafen,
    /// Toggle mode.
    ToggleMode,
    /// Per peer volume in percent.
    SetVolume(UserId, f32),
    /// Rename. Travels as a certificate renewal carrying the new CN, because
    /// the name lives in the certificate (`docs/reference/protocol.md` §2.2).
    RequestRename(String),
    /// Replace the whole identity (passport import): persist it, then
    /// reconnect with it. The settings the passport carried come along when
    /// the user took them: applied like [`Command::UpdateSettings`] once the
    /// identity is stored, around this machine's devices
    /// (`Settings::take_portable`).
    ReplaceIdentity(Box<Identity>, Option<Box<Settings>>),
    /// Issue a registration bundle. Admin only (`AppState.is_admin`); a
    /// non-admin send is rejected by the server, non-fatally.
    CreateInvite {
        /// Placeholder name until the invited user picks their own. Empty
        /// means a generated one.
        name: String,
        /// Lifetime in seconds. `0` means the server default.
        ttl_s: u64,
    },
    /// Promote/demote another user. Admin only.
    SetUserAdmin {
        /// The user whose role changes.
        user_id: UserId,
        /// The new role.
        is_admin: bool,
    },
    /// Disable/enable another user. Admin only.
    SetUserDisabled {
        /// The affected user.
        user_id: UserId,
        /// The new disabled state.
        disabled: bool,
    },
    /// Revoke another user's certificates and kick them. Admin only.
    RevokeUser {
        /// The affected user.
        user_id: UserId,
    },
    /// Refresh the full user list for the Nutzer tab. Admin only.
    RequestUserList,
    /// Fetch the public certificate of an online peer.
    RequestPeerCertificate(Session),
    /// Close another user's connections; they may come back. Admin only.
    KickUser {
        /// The affected user.
        user_id: UserId,
    },
    /// Enforce or lift another user's mute and deafen. Admin only.
    SetUserRestriction {
        /// The affected user.
        user_id: UserId,
        /// Enforced mute.
        muted: bool,
        /// Enforced deafen.
        deafened: bool,
    },
    /// Connect again right away: after a fatal close (a kick), or to skip
    /// the backoff wait.
    Reconnect,
    /// Replace the settings (device change, thresholds, …); effective at once.
    UpdateSettings(Box<Settings>),
    /// Restore processing settings only; preserve mute, PTT and devices.
    RestoreGoodAudio,
    /// Forget calibration of the actual microphone and restart learning.
    ForgetGoodAudio,
    /// Mic test („Mikrofontest“) on/off: the microphone after the whole
    /// send chain — what the others would hear, before Opus — also plays
    /// locally, at unity gain, whatever the gate, mute or deafen say. Off by
    /// default, never persisted, ends by itself after [`MIC_TEST_MAX`].
    SetMicTest(bool),
    /// Camera on/off.
    SetCamera(bool),
    /// Overlay on/off.
    SetOverlay(bool),
    /// The main window became visible (`true`) or went to the tray or was
    /// minimized (`false`). Together with the overlay this decides
    /// `UserFlags::VIEWING`.
    SetWindowVisible(bool),
    /// Clear the banner.
    DismissBanner,
    /// Force an IDR (from a `KeyframeRequest`, or tests).
    ForceKeyframe,
    /// The hotkey backend that came up (for the settings view).
    HotkeyBackend(String),
    /// Reopen the audio streams after a backend error (`icu-audio-events`).
    ReopenStreams,
    /// A live capture stream selected another channel; keep that stream open.
    CaptureChannelChanged(u16),
    /// Try the streams that did not open again, only those
    /// (`icu-audio-events`, internal).
    RetryStreams,
    /// The device list changed (`icu-audio-events`, internal): reopen the
    /// streams if a saved device came back or went away.
    DevicesChanged,
    /// Stop everything.
    Quit,
    /// Bind the hotkeys again with the current settings, e.g. to have the
    /// portal ask again after its dialog was turned down
    /// (`hotkey_backend == "portal-declined"`).
    RetryHotkeys,
    /// Toggle the camera (hotkey).
    ToggleCamera,
    /// The camera would not start (`crate::camera::CameraHandle` gave up
    /// after repeated failures): show why, and switch it off if it is on.
    CameraFailed(Problem),
}

/// An `f32` the audio threads read every block without a lock.
#[derive(Debug)]
struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn new(v: f32) -> AtomicF32 {
        AtomicF32(AtomicU32::new(v.to_bits()))
    }

    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// Atomics the DSP thread reads every block.
#[derive(Debug)]
pub struct DspControl {
    muted: AtomicBool,
    setup_active: AtomicBool,
    /// Odd while privacy is active. Every transition invalidates captured media.
    privacy_epoch: AtomicUsize,
    ptt_down: AtomicBool,
    /// A connection is up; without one the gate stays shut, so nothing is
    /// encoded and the UI never claims to be sending into the void.
    online: AtomicBool,
    /// Someone else is in the room. Alone, nothing is encoded or sent — no
    /// one would hear it; the gate itself still follows PTT/VAD, so the
    /// VAD's noise floor and the pre-roll stay true for when someone joins.
    audience: AtomicBool,
    vad_mode: AtomicBool,
    denoise: AtomicBool,
    aec: AtomicBool,
    voice_opt: AtomicBool,
    voice_strength: AtomicU32,
    /// Mic auto leveling; only with denoise (the energy VAD would open on
    /// boosted room noise).
    auto_level: AtomicBool,
    normalize_outgoing: AtomicBool,
    vad_threshold: AtomicF32,
    vad_auto: AtomicBool,
    vad_effective_threshold: AtomicF32,
    gain: AtomicF32,
    release_ms: AtomicU32,
    hangover_ms: AtomicU32,
    attack_ms: AtomicU32,
    frame_samples: AtomicU32,
    loss_permille: AtomicU32,
    level_peak: AtomicF32,
    level_rms: AtomicF32,
    raw_rms: AtomicF32,
    raw_peak: AtomicF32,
    post_aec_rms: AtomicF32,
    output_rms: AtomicF32,
    vad_prob: AtomicF32,
    sending: AtomicBool,
    aec_available: AtomicBool,
    reinit_aec: AtomicBool,
    stop: AtomicBool,
    test_tone: AtomicBool,
    /// `audio.cue_tx`: a cue when sending starts and ends.
    cue_tx: AtomicBool,
    /// The mic test is on (`Command::SetMicTest`).
    mic_test: AtomicBool,
}

impl DspControl {
    /// Builds the control block and derives every settings-driven field
    /// through [`DspControl::apply_settings`], so the two never drift.
    fn from_settings(s: &Settings, test_tone: bool) -> DspControl {
        let ctl = DspControl {
            // The one settings-driven field `apply_settings` leaves alone:
            // after the start only the mute commands move it.
            muted: AtomicBool::new(s.audio.muted),
            setup_active: AtomicBool::new(false),
            privacy_epoch: AtomicUsize::new(0),
            ptt_down: AtomicBool::new(false),
            online: AtomicBool::new(false),
            audience: AtomicBool::new(false),
            vad_mode: AtomicBool::new(false),
            denoise: AtomicBool::new(false),
            aec: AtomicBool::new(false),
            voice_opt: AtomicBool::new(false),
            voice_strength: AtomicU32::new(0),
            auto_level: AtomicBool::new(false),
            normalize_outgoing: AtomicBool::new(false),
            vad_threshold: AtomicF32::new(0.0),
            vad_auto: AtomicBool::new(false),
            vad_effective_threshold: AtomicF32::new(0.0),
            gain: AtomicF32::new(0.0),
            release_ms: AtomicU32::new(0),
            hangover_ms: AtomicU32::new(0),
            attack_ms: AtomicU32::new(0),
            frame_samples: AtomicU32::new(0),
            loss_permille: AtomicU32::new(0),
            level_peak: AtomicF32::new(-100.0),
            level_rms: AtomicF32::new(-100.0),
            raw_rms: AtomicF32::new(-100.0),
            raw_peak: AtomicF32::new(-100.0),
            post_aec_rms: AtomicF32::new(-100.0),
            output_rms: AtomicF32::new(-100.0),
            vad_prob: AtomicF32::new(0.0),
            sending: AtomicBool::new(false),
            aec_available: AtomicBool::new(false),
            reinit_aec: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            test_tone: AtomicBool::new(test_tone),
            cue_tx: AtomicBool::new(false),
            mic_test: AtomicBool::new(false),
        };
        ctl.apply_settings(s);
        ctl
    }

    fn transmission_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
            || self.setup_active.load(Ordering::Relaxed)
            || self.privacy_epoch.load(Ordering::SeqCst) % 2 == 1
    }

    fn apply_settings(&self, s: &Settings) {
        let test_tone = self.test_tone.load(Ordering::Relaxed);
        self.vad_mode
            .store(Mode::from(s.audio.mode) == Mode::Vad, Ordering::Relaxed);
        self.denoise
            .store(s.audio.denoise && !test_tone, Ordering::Relaxed);
        self.aec.store(s.audio.aec && !test_tone, Ordering::Relaxed);
        self.voice_opt
            .store(s.audio.voice_opt && !test_tone, Ordering::Relaxed);
        self.voice_strength
            .store(s.audio.voice_strength.to_u32(), Ordering::Relaxed);
        self.auto_level.store(
            s.audio.auto_level && s.audio.denoise && !test_tone,
            Ordering::Relaxed,
        );
        self.normalize_outgoing
            .store(s.audio.normalize_outgoing && !test_tone, Ordering::Relaxed);
        self.vad_threshold.store(s.audio.vad_threshold);
        self.vad_auto.store(s.audio.vad_auto, Ordering::Relaxed);
        self.gain.store(meter::from_db(s.audio.gain_db));
        self.release_ms.store(s.audio.release_ms, Ordering::Relaxed);
        self.hangover_ms
            .store(s.audio.hangover_ms, Ordering::Relaxed);
        self.attack_ms.store(s.audio.attack_ms, Ordering::Relaxed);
        self.frame_samples.store(
            if s.audio.frame_ms == 10 { 480 } else { 960 },
            Ordering::Relaxed,
        );
        self.cue_tx.store(s.audio.cue_tx, Ordering::Relaxed);
    }

    /// Current level.
    pub fn level(&self) -> Level {
        Level {
            peak_db: self.level_peak.load(),
            rms_db: self.level_rms.load(),
        }
    }
}

/// One remote speaker on the playout side.
struct Speaker {
    user_id: UserId,
    jitter: JitterBuffer,
    decoder: opus::Decoder,
    pending: VecDeque<f32>,
    last_packet: Option<Instant>,
    volume: f32,
    /// Loudness normalization, ahead of `volume`.
    norm: Normalizer,
    /// Speech selection only, independent of user/master gains and normalization.
    duck_voice: crate::dsp::normalize::SpeechActivity,
    /// The previous user/master gain belongs to this speaker, never to its
    /// current position in the mixer's unordered input list.
    volume_ramp: mixer::GainRamp,
    feedback: crate::dsp::feedback::Measurement,
    /// Queued samples up to the end of the last concealed or FEC-recovered
    /// frame; `norm` holds while they play.
    concealed: usize,
    /// The last datagram carried `END`: the talk spurt is over.
    ended: bool,
    /// What the local path last said about this speaker talking
    /// ([`local_indicator`]); `None` while the server's flag decides.
    indicator: Option<bool>,
    /// Loss accounting for `LossReport`: (played, lost) at the last report.
    report_base: (u64, u64),
    last_reported_permille: u16,
    /// When a `LossReport` for this speaker last went out.
    last_report_at: Option<Instant>,
    /// The last report window saw none of its traffic: the server has
    /// forgotten the figure by the time it speaks again.
    loss_idle: bool,
    /// The frame last queued for this speaker was quiet (below
    /// [`QUIET_RMS`]): the jitter buffer may adjust its depth now without
    /// anyone hearing it (`JitterBuffer::pop_quiet`).
    quiet: bool,
}

/// RMS below which a decoded frame counts as quiet for depth adjustments:
/// −40 dBFS, the level of a pause between words after the denoiser.
const QUIET_RMS: f32 = 0.01;

/// Whether `samples` is quiet enough to hide a concealed or skipped frame.
fn frame_quiet(samples: &[f32]) -> bool {
    if samples.is_empty() {
        return true;
    }
    let energy: f32 = samples.iter().map(|s| s * s).sum();
    energy < QUIET_RMS * QUIET_RMS * samples.len() as f32
}

impl Speaker {
    /// A speaker whose session the peer list does not know: audio ahead of
    /// its `UserJoined`, or a straggler after `UserLeft`. Silent until
    /// `admit` sets the user's volume — a peer set to 0 % („lokal stumm“)
    /// was audible at 100 % in between.
    fn stray(frame_samples: u32) -> Result<Speaker, opus::Error> {
        Speaker::new(0, 0.0, frame_samples)
    }

    fn new(user_id: UserId, volume: f32, frame_samples: u32) -> Result<Speaker, opus::Error> {
        Ok(Speaker {
            user_id,
            jitter: JitterBuffer::new(JitterConfig {
                frame_samples,
                ..JitterConfig::default()
            }),
            decoder: opus::Decoder::new()?,
            pending: VecDeque::with_capacity(4 * BLOCK),
            last_packet: None,
            ended: false,
            indicator: None,
            volume,
            norm: Normalizer::new(),
            duck_voice: Default::default(),
            volume_ramp: mixer::GainRamp::default(),
            feedback: crate::dsp::feedback::Measurement::default(),
            concealed: 0,
            report_base: (0, 0),
            last_reported_permille: 0,
            last_report_at: None,
            loss_idle: false,
            quiet: true,
        })
    }
}

#[derive(Default)]
struct AudioTuning {
    outgoing: crate::dsp::feedback::Measurement,
    capture: crate::dsp::feedback::CaptureWindow,
    peer: crate::dsp::feedback::PeerTuner,
    capture_delta: f32,
    software_gain_db: f32,
    capture_identity: Option<crate::audio::level::CaptureLevel>,
    warm_device: Option<String>,
    seed_gain: Option<f32>,
    clip_until: Option<Instant>,
    overload_until: Option<Instant>,
    software_until: Option<Instant>,
    heavy_samples: usize,
    limited_samples: usize,
    dynamics_samples: usize,
    max_reduction: f32,
    system_gain_db: f32,
    force_epoch: bool,
}

impl AudioTuning {
    fn capture_discontinuity(&mut self) {
        self.outgoing = Default::default();
        self.capture = Default::default();
        self.force_epoch = true;
    }

    fn new_connection(&mut self, epoch: u32) {
        // Native gain compensation and local calibration belong to the
        // microphone, not the connection. Only forget remote evidence.
        self.peer = Default::default();
        self.peer.set_epoch(epoch);
        self.outgoing = Default::default();
        self.force_epoch = true;
    }
}

fn restore_capture(shared: &Arc<Shared>, backend: &Arc<Mutex<Box<dyn AudioBackend>>>) {
    let delta = lock(&shared.system_tuner).restore(&mut **lock(backend));
    let mut tuning = AudioTuning {
        capture_delta: delta,
        ..Default::default()
    };
    tuning
        .peer
        .set_epoch(shared.gain_epoch.load(Ordering::Relaxed));
    *lock(&shared.tuning) = tuning;
}

fn tune_capture(shared: &Arc<Shared>, local: &LocalState) {
    let a = &local.settings.audio;
    let current = lock(&local.backend)
        .capture_level()
        .ok()
        .flatten()
        .filter(|c| c.valid());
    let capture_channel = lock(&local.backend).capture_channel_index();
    {
        let mut t = lock(&shared.tuning);
        if t.capture_identity
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(old, new)| old.device != new.device || old.revision != new.revision)
        {
            t.peer = Default::default();
            t.peer.set_epoch(shared.gain_epoch.load(Ordering::Relaxed));
            t.force_epoch = true;
            t.warm_device = None;
        }
        t.capture_identity = current.clone();
        if let Some(c) = current.as_ref() {
            let key = crate::audio::calibration::device_channel_key(
                shared.state.borrow().backend,
                &c.device,
                capture_channel,
            );
            if t.warm_device.as_ref() != Some(&key) {
                t.warm_device = Some(key.clone());
                if capture_channel.is_some() && a.remember_good_levels && a.auto_level && a.denoise
                {
                    if let Some((gain, peer)) = local.calibration.warm_start(
                        &key,
                        a,
                        c.db,
                        crate::net::conn::unix_now().max(0) as u64,
                    ) {
                        t.seed_gain = Some(gain);
                        t.peer.seed_trim(peer);
                    }
                }
            }
        }
    }
    let enabled = a.auto_level
        && a.denoise
        && a.tune_system_mic
        && !shared.dsp.test_tone.load(Ordering::Relaxed);
    if !enabled {
        let delta = lock(&shared.system_tuner).restore(&mut **lock(&local.backend));
        let mut t = lock(&shared.tuning);
        t.capture_delta += delta;
        t.capture = Default::default();
        t.system_gain_db = 0.0;
        let (software_db, offset) = (t.software_gain_db, t.peer.offset_db);
        drop(t);
        shared.state.send_modify(|s| {
            s.audio_health.system_status = "Systemregelung aus".into();
            s.audio_health.system_gain_db = 0.0;
            s.audio_health.software_gain_db = software_db;
            s.audio_health.peer_trim_db = offset;
            s.tuning_status = s.audio_health.diagnostic();
        });
        return;
    }
    let w = std::mem::take(&mut lock(&shared.tuning).capture);
    let paused = shared.dsp.transmission_muted() || !shared.dsp.online.load(Ordering::Relaxed);
    let (delta, status, system_db) = {
        let mut tuner = lock(&shared.system_tuner);
        let delta = tuner.tick(
            &mut **lock(&local.backend),
            if paused { Default::default() } else { w },
            Instant::now(),
        );
        (delta, tuner.status.clone(), tuner.delta_db())
    };
    let (offset, software_db) = {
        let mut t = lock(&shared.tuning);
        if delta != 0.0 {
            t.capture_delta += delta;
            t.outgoing = Default::default();
            t.capture = Default::default();
        }
        t.system_gain_db = system_db;
        (t.peer.offset_db, t.software_gain_db)
    };
    shared.state.send_modify(|s| {
        s.audio_health.system_status = status;
        s.audio_health.capture_channel = capture_channel;
        s.audio_health.system_gain_db = system_db;
        s.audio_health.software_gain_db = software_db;
        s.audio_health.peer_trim_db = offset;
        s.tuning_status = s.audio_health.diagnostic();
    });
}

fn update_audio_health(
    t: &mut AudioTuning,
    measured: Option<icu_proto::AudioLevel>,
    enabled: bool,
    now: Instant,
    shared: &Arc<Shared>,
) {
    use crate::audio::health::{AudioHealth, State};
    let remote = t.peer.correction(now);
    let local = measured.map(|l| l.correction_cdb as f32 / 100.0);
    let state = if t.clip_until.is_some_and(|until| now < until) {
        State::InputClipping
    } else if t.overload_until.is_some_and(|until| now < until) {
        State::Limited
    } else if t.software_until.is_some_and(|until| now < until) {
        State::SoftwareOverload
    } else if !shared.dsp.auto_level.load(Ordering::Relaxed)
        || !shared.dsp.denoise.load(Ordering::Relaxed)
    {
        State::Disabled
    } else if !enabled {
        State::LocalOnly
    } else if t.peer.conflicting(now) {
        State::Conflicting
    } else if remote.is_none() && t.peer.offset_db != 0.0 {
        State::Held
    } else if remote.is_none() {
        State::NoFeedback
    } else if measured.is_none() && t.peer.offset_db != 0.0 {
        State::Held
    } else if measured.is_none() {
        State::NoLocalSpeech
    } else if local.zip(remote).is_some_and(|(l, r)| {
        (l - r).abs() > 2.0
            || (r.abs() > icu_proto::audio_level::ACCEPT_CDB as f32 / 100.0
                && l.signum() != r.signum())
    }) {
        State::Conflicting
    } else if remote.is_some_and(|r| r.abs() <= icu_proto::audio_level::ACCEPT_CDB as f32 / 100.0) {
        State::Good
    } else if t.peer.status(now) == "Regelgrenze" {
        State::Bound
    } else {
        State::Adjusting
    };
    let fraction = t.limited_samples as f32 / t.dynamics_samples.max(1) as f32;
    let mut health = AudioHealth {
        state,
        reporters: t.peer.reporters(),
        agreeing: t.peer.agreeing(),
        speech_ms: measured.map_or(0, |m| m.speech_ms),
        gain_epoch: t.peer.epoch(),
        observation_window: t.peer.window(),
        raw_rms_db: Some(shared.dsp.raw_rms.load()),
        raw_peak_db: Some(shared.dsp.raw_peak.load()),
        post_aec_rms_db: Some(shared.dsp.post_aec_rms.load()),
        output_rms_db: Some(shared.dsp.output_rms.load()),
        capture_channel: shared.state.borrow().audio_health.capture_channel,
        local_correction_db: local,
        remote_correction_db: remote,
        software_gain_db: t.software_gain_db,
        peer_trim_db: t.peer.offset_db,
        limiter_reduction_db: t.max_reduction,
        limited_fraction: fraction,
        feedback_age_secs: t.peer.hint_age_secs(now),
        ..Default::default()
    };
    health.system_gain_db = t.system_gain_db;
    shared.state.send_modify(|s| {
        health.profile_saved = s.can_restore_audio;
        health.profile_saved_at = s.audio_health.profile_saved_at;
        health.system_status = s.audio_health.system_status.clone();
        s.tuning_status = health.diagnostic();
        s.audio_health = health;
    });
    t.limited_samples = 0;
    t.dynamics_samples = 0;
    t.max_reduction = 0.0;
}

/// Shared between all engine threads.
struct Shared {
    state: watch::Sender<AppState>,
    dsp: Arc<DspControl>,
    stats: Arc<DebugStats>,
    speakers: Mutex<HashMap<Session, Speaker>>,
    /// The live connection for `send_datagram` from the DSP thread.
    conn: Mutex<Option<quinn::Connection>>,
    deafened: AtomicBool,
    output_gain: AtomicF32,
    /// `audio.normalize_incoming`, read by the mixer.
    normalize_incoming: AtomicBool,
    /// Peak of the mixer's output since the DSP thread last took it (bits
    /// of a non-negative `f32`, which order like the value): whether the
    /// loudspeaker plays something the AEC reference should carry.
    play_peak: AtomicU32,
    /// Cues for the mixer, bounded and without repeats (`cues::CueQueue`).
    cues: Mutex<crate::cues::CueQueue>,
    /// A transmit cue the DSP thread asks the mixer to play
    /// ([`TX_CUE_START`], [`TX_CUE_STOP`], 0 = none): the DSP thread neither
    /// renders nor takes the cue lock.
    pending_tx_cue: AtomicU8,
    /// The AEC reference stream is open.
    reference_open: AtomicBool,
    /// Push times of the capture and reference callbacks, so the DSP thread
    /// pairs blocks from the same graph cycle (`dsp::refalign`).
    cycle: Arc<CycleClock>,
    /// Requests an IDR from the camera thread.
    keyframe_request: AtomicBool,
    /// Video frames received, keyed by session (M4), and the connection
    /// they belong to.
    video: Mutex<VideoIn>,
    /// Called on the decode thread for every new picture, so a window shows
    /// it when it arrives instead of on its next timed repaint
    /// ([`VideoFrames::set_waker`]).
    video_waker: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// Video sender (M4). Taken out of the lock for each frame, so the
    /// camera thread never holds it across a send.
    video_tx: Mutex<Option<Arc<crate::video::Sender>>>,
    /// Ring buffer ends the backend callbacks write/read. Behind a mutex so
    /// streams can be reopened with a fresh closure; the callbacks only
    /// `try_lock`, which is uncontended in practice.
    cap_prod: Arc<Mutex<HeapProd<f32>>>,
    /// Samples callbacks could not enqueue, consumed by the DSP clock.
    capture_dropped: Arc<AtomicUsize>,
    ref_prod: Arc<Mutex<HeapProd<crate::audio::backend::ReferenceFrame>>>,
    reference_channels: Arc<AtomicUsize>,
    ducking: Arc<crate::audio::ducking::Signals>,
    play_cons: Arc<Mutex<HeapCons<[f32; 2]>>>,
    /// Largest block the playback callback has asked for since the mixer
    /// last took it ([`RecentRequest`]); the mixer keeps at least that much
    /// plus a block queued, whatever the graph quantum.
    play_request: Arc<AtomicUsize>,
    /// The streams the last open could not open (`FAILED_CAPTURE` …); the
    /// audio events thread has them retried alone ([`StreamRetry`]).
    failed_streams: AtomicU8,
    /// The engine is shutting down: nothing may be written to `identity/`
    /// any more — a logout deletes it as soon as `Engine::shutdown` returns.
    closing: AtomicBool,
    /// Detected network condition (RTT, loss, congestion pressure); folded
    /// from quinn's own connection stats plus `LossHint`/
    /// `VideoCongestionHint` (`crate::netcond`).
    net: Mutex<NetworkTracker>,
    /// Outgoing camera quality tier, published by the `QualityController`
    /// and read once per iteration by the camera thread (`crate::quality`,
    /// `crate::camera::run_sender`). Video absorbs all of the network
    /// adaptation; this atomic is the only channel for it.
    video_tier: AtomicU8,
    tuning: Mutex<AudioTuning>,
    system_tuner: Mutex<crate::audio::level::SystemTuner>,
    feedback_capable: AtomicBool,
    gain_epoch: AtomicU32,
}

/// Received video, tagged with the connection it belongs to. Session IDs
/// are only valid within one connection (`docs/reference/protocol.md` §5): the decode
/// thread of a connection that ended can still hold frames, and delivered
/// after a reconnect they would appear under a session ID that now belongs
/// to somebody else — a peer briefly showing another person's picture.
#[derive(Default)]
struct VideoIn {
    receivers: crate::video::Receivers,
    /// Bumped by every [`VideoIn::reset`], at `Welcome` and at session end.
    generation: u64,
}

impl std::ops::Deref for VideoIn {
    type Target = crate::video::Receivers;
    fn deref(&self) -> &crate::video::Receivers {
        &self.receivers
    }
}

impl std::ops::DerefMut for VideoIn {
    fn deref_mut(&mut self) -> &mut crate::video::Receivers {
        &mut self.receivers
    }
}

impl VideoIn {
    /// Forgets all received video and starts a new generation: frames of an
    /// earlier connection are dropped from now on.
    fn reset(&mut self) -> u64 {
        self.receivers.clear();
        self.generation += 1;
        self.generation
    }

    /// [`crate::video::Receivers::accept`] for a frame of connection
    /// `generation`; `None` for a stale one, which creates nothing.
    fn accept_for(
        &mut self,
        generation: u64,
        session: Session,
        frame: &crate::video::EncodedFrame,
    ) -> Option<crate::video::Decoder> {
        if generation != self.generation {
            return None;
        }
        self.receivers.accept(session, frame)
    }

    /// [`crate::video::Receivers::finish`], unless the connection the
    /// frame came from is gone by now.
    fn finish_for(
        &mut self,
        generation: u64,
        session: Session,
        decoder: crate::video::Decoder,
        picture: Option<crate::video::Frame>,
    ) {
        if generation == self.generation {
            self.receivers.finish(session, decoder, picture);
        }
    }
}

/// A running engine.
pub struct Engine {
    shared: Arc<Shared>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    state_rx: watch::Receiver<AppState>,
    runtime: Option<tokio::runtime::Runtime>,
    /// Disconnects when the network task has ended (its sender is dropped
    /// with it), for [`Engine::shutdown`] to wait on without a runtime.
    net_done: Option<std::sync::mpsc::Receiver<()>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    ducking_service: Option<crate::audio::ducking::Service>,
    backend: Arc<Mutex<Box<dyn AudioBackend>>>,
    /// Where this instance's config/data live (`--config-dir` or default).
    /// Only the spawned connection task holds its own copy for persisting
    /// settings; this clone exists so the UI layer (the About tab's
    /// histogram export) can resolve the same directory without a second
    /// accessor path.
    paths: Paths,
}

/// Engine start failure.
#[derive(Debug)]
pub struct EngineError(pub String);

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EngineError {}

impl Engine {
    /// Starts all threads and the connection loop.
    pub fn start(
        cfg: EngineConfig,
        mut backend: Box<dyn AudioBackend>,
    ) -> Result<Engine, EngineError> {
        let settings = cfg.settings.clone();
        let synthetic_audio = cfg.test_tone;
        let paths = cfg.paths.clone();
        let dsp = Arc::new(DspControl::from_settings(&settings, cfg.test_tone));
        let stats = Arc::new(DebugStats::default());
        let backend_name = backend.name();
        let events = backend.take_events();
        let initial = AppState {
            conn: ConnStatus::Connecting,
            server: format!("{}:{}", cfg.identity.server.host, cfg.identity.server.port),
            server_name: cfg.identity.server.server_name.clone(),
            me: None,
            peers: Vec::new(),
            video_viewers: Vec::new(),
            // A mute kept from the last run shows from the first frame.
            muted: settings.audio.muted,
            setup_active: false,
            privacy_active: false,
            privacy_epoch: 0,
            deafened: false,
            ptt_down: false,
            sending: false,
            voice: false,
            level: Level::default(),
            input_clipping: false,
            vad_prob: 0.0,
            vad_threshold: settings.audio.vad_threshold,
            rtt_ms: None,
            server_rtt_ms: None,
            clock_offset_ms: None,
            conn_info: None,
            conn_stats: None,
            update_required: None,
            banner: cfg.banner.clone(),
            notice: None,
            name_error: None,
            rename_pending: false,
            is_admin: false,
            last_invite: None,
            invite_error: None,
            user_list: Vec::new(),
            user_list_generation: 0,
            user_action_error: None,
            restricted_muted: false,
            system_muted: false,
            restricted_deafened: false,
            devices: backend.devices(),
            active_input: None,
            active_output: None,
            backend: backend_name,
            tuning_status: String::new(),
            ducking_status: Default::default(),
            audio_health: Default::default(),
            can_restore_audio: false,
            aec_available: false,
            aec_stats: AecStats::default(),
            aec_reference: false,
            aec_suspect: false,
            aec_no_echo: false,
            aec_reference_silent: false,
            mic_test: false,
            camera_on: settings.camera.enabled,
            hotkey_backend: String::new(),
            settings: settings.clone(),
            hotkey_retries: 0,
        };
        let (state_tx, state_rx) = watch::channel(initial);
        // Ring buffers between backend callbacks and the DSP/mixer threads.
        let (cap_prod, cap_cons) = HeapRb::<f32>::new(48_000).split();
        let (ref_prod, ref_cons) =
            HeapRb::<crate::audio::backend::ReferenceFrame>::new(48_000).split();
        // The playback ring only ever holds `lead` (2 blocks, or one graph
        // quantum + 1 block); 1 s of capacity just makes sure `lead` is never
        // clamped below what a large quantum (Bluetooth, pro audio) asks for.
        let (play_prod, play_cons) = HeapRb::<[f32; 2]>::new(48_000).split();
        // The mic test's hand-off from the DSP thread to the mixer: lock
        // free, allocated once here.
        let (mic_prod, mic_cons) = HeapRb::<f32>::new(8 * BLOCK).split();

        let shared = Arc::new(Shared {
            state: state_tx,
            dsp: dsp.clone(),
            stats: stats.clone(),
            speakers: Mutex::new(HashMap::new()),
            conn: Mutex::new(None),
            deafened: AtomicBool::new(false),
            output_gain: AtomicF32::new(mixer::volume_gain(settings.audio.output_volume)),
            normalize_incoming: AtomicBool::new(settings.audio.normalize_incoming),
            play_peak: AtomicU32::new(0),
            // Allocated once, so queueing one in the mixer thread never
            // allocates.
            cues: Mutex::new(crate::cues::CueQueue::new()),
            pending_tx_cue: AtomicU8::new(0),
            reference_open: AtomicBool::new(false),
            cycle: Arc::new(CycleClock::default()),
            keyframe_request: AtomicBool::new(false),
            video: Mutex::new(VideoIn::default()),
            video_waker: Mutex::new(None),
            video_tx: Mutex::new(None),
            cap_prod: Arc::new(Mutex::new(cap_prod)),
            capture_dropped: Arc::new(AtomicUsize::new(0)),
            ref_prod: Arc::new(Mutex::new(ref_prod)),
            reference_channels: Arc::new(AtomicUsize::new(1)),
            ducking: Arc::new(crate::audio::ducking::Signals::default()),
            play_cons: Arc::new(Mutex::new(play_cons)),
            play_request: Arc::new(AtomicUsize::new(0)),
            failed_streams: AtomicU8::new(0),
            closing: AtomicBool::new(false),
            net: Mutex::new(NetworkTracker::new()),
            tuning: Mutex::new(AudioTuning::default()),
            system_tuner: Mutex::new(crate::audio::level::SystemTuner::default()),
            feedback_capable: AtomicBool::new(false),
            gain_epoch: AtomicU32::new(0),
            video_tier: AtomicU8::new(VideoTier::T0.as_u8()),
        });

        open_streams(&mut *backend, &settings, &shared);
        let backend = Arc::new(Mutex::new(backend));

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
        let mut threads = Vec::new();
        let reopen_tx = cmd_tx.clone();

        // DSP thread.
        {
            let shared = shared.clone();
            let test_tone = cfg.test_tone;
            threads.push(
                std::thread::Builder::new()
                    .name("icu-dsp".into())
                    .spawn(move || dsp_thread(shared, cap_cons, ref_cons, mic_prod, test_tone))
                    .map_err(|e| EngineError(e.to_string()))?,
            );
        }
        // Mixer thread.
        {
            let shared = shared.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("icu-mixer".into())
                    .spawn(move || mixer_thread(shared, play_prod, mic_cons))
                    .map_err(|e| EngineError(e.to_string()))?,
            );
        }
        // Backend events → state. Detached: it ends when the backend (and
        // with it the event sender) is dropped — which is why it holds the
        // backend only weakly. A strong reference kept the backend alive,
        // and with it (PipeWire) the thread owning the event sender, so each
        // engine leaked a PipeWire connection and two threads.
        if let Some(events) = events {
            let shared = shared.clone();
            let backend = Arc::downgrade(&backend);
            std::thread::Builder::new()
                .name("icu-audio-events".into())
                .spawn(move || {
                    // A stream that died is reopened through the connection
                    // task (`Command::ReopenStreams`, which owns the settings
                    // and the AEC reinit), after a backoff of 1, 2, 4 … 30 s
                    // that starts over once a minute passes without an error.
                    // One reopen at a time: capture and playback usually die
                    // together when a device vanishes.
                    let mut backoff = Duration::from_secs(1);
                    let mut last_error: Option<Instant> = None;
                    // A stream that never opened is retried alone.
                    let mut retry = StreamRetry::default();
                    let pending = Arc::new(AtomicBool::new(false));
                    let schedule_reopen = |delay: Duration| {
                        if pending.swap(true, Ordering::Relaxed) {
                            return;
                        }
                        let tx = reopen_tx.clone();
                        let pending = pending.clone();
                        let _ = std::thread::Builder::new()
                            .name("icu-audio-reopen".into())
                            .spawn(move || {
                                std::thread::sleep(delay);
                                pending.store(false, Ordering::Relaxed);
                                let _ = tx.send(Command::ReopenStreams);
                            });
                    };
                    loop {
                        let missing = shared.failed_streams.load(Ordering::Relaxed) != 0;
                        if retry.due(Instant::now(), missing) {
                            let _ = reopen_tx.send(Command::RetryStreams);
                        }
                        let ev = match events.recv_timeout(RETRY_POLL) {
                            Ok(ev) => ev,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        };
                        match ev {
                            BackendEvent::DefaultDeviceChanged => {
                                schedule_reopen(Duration::from_millis(100));
                            }
                            BackendEvent::CaptureChannelChanged { channel } => {
                                let _ = reopen_tx.send(Command::CaptureChannelChanged(channel));
                            }
                            BackendEvent::StreamDiscontinuity { stream } => {
                                tracing::debug!(?stream, "audio capture timeline changed");
                                schedule_reopen(Duration::from_millis(100));
                            }
                            BackendEvent::DevicesChanged(list) => {
                                shared.state.send_modify(|s| s.devices = list);
                                // Decided against the backend's current list
                                // in the connection task: this one may be one
                                // of a burst of partial lists.
                                let _ = reopen_tx.send(Command::DevicesChanged);
                                // A device came or went shortly after an
                                // error: most likely the one that failed
                                // (USB re-plug); try right away.
                                if last_error
                                    .is_some_and(|t| t.elapsed() < Duration::from_secs(300))
                                {
                                    schedule_reopen(Duration::from_secs(1));
                                }
                            }
                            BackendEvent::StreamError { stream, message } => {
                                tracing::warn!(?stream, %message, "audio stream error");
                                // The AEC reference is no device of its own:
                                // like a reference that does not open, it
                                // shows in the settings, not as a banner
                                // about the microphone.
                                let device = match stream {
                                    StreamKind::Capture => Some("Das Mikrofon"),
                                    StreamKind::Playback => Some("Der Lautsprecher"),
                                    StreamKind::Reference => None,
                                };
                                if let Some(device) = device {
                                    shared.state.send_modify(|s| {
                                        s.banner = Some(crate::problem::audio(
                                            device,
                                            &format!("{stream:?}: {message}"),
                                        ))
                                    });
                                }
                                if let Some(backend) = backend.upgrade() {
                                    lock(&backend).close(stream);
                                }
                                if stream == StreamKind::Reference {
                                    // Until the reopen (1–30 s of backoff) the
                                    // DSP thread must not wait for a reference
                                    // that is not coming: with the flag still
                                    // set it stalled every block for the stall
                                    // wait and, counting the misses as a
                                    // dropout, cleared the capture ring and
                                    // reinitialized the AEC every 10 s — a gap
                                    // in the outgoing audio each time.
                                    shared.reference_open.store(false, Ordering::Relaxed);
                                    shared.state.send_modify(|s| s.aec_reference = false);
                                }
                                backoff = match last_error {
                                    Some(t) if t.elapsed() < Duration::from_secs(60) => {
                                        (backoff * 2).min(Duration::from_secs(30))
                                    }
                                    _ => Duration::from_secs(1),
                                };
                                last_error = Some(Instant::now());
                                schedule_reopen(backoff);
                            }
                        }
                    }
                })
                .map_err(|e| EngineError(e.to_string()))?;
        }

        // Network runtime.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("icu-net")
            .enable_all()
            .build()
            .map_err(|e| EngineError(e.to_string()))?;
        let (done_tx, net_done) = std::sync::mpsc::channel::<()>();
        {
            let shared = shared.clone();
            let backend = backend.clone();
            runtime.spawn(async move {
                let _done = done_tx;
                net_task(cfg, shared, cmd_rx, backend).await;
            });
        }

        let ducking_service = if matches!(backend_name, "pipewire" | "wasapi") && !synthetic_audio {
            let input_shared = shared.clone();
            let status_shared = shared.clone();
            crate::audio::ducking::Service::start(
                move || ducking_input(&input_shared),
                move |status| {
                    status_shared.state.send_if_modified(|state| {
                        if state.ducking_status == status {
                            false
                        } else {
                            state.ducking_status = status;
                            true
                        }
                    });
                },
            )
            .ok()
        } else {
            if backend_name == "cpal" {
                shared.state.send_modify(|s| {
                    s.ducking_status.detail =
                        "Absenkung für diesen Ausgabeweg nicht verfügbar".into()
                });
            }
            None
        };
        Ok(Engine {
            shared,
            cmd_tx,
            state_rx,
            runtime: Some(runtime),
            net_done: Some(net_done),
            threads,
            ducking_service,
            backend,
            paths,
        })
    }

    /// Sends a command.
    pub fn command(&self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// A sender for hotkeys, tray and overlay threads.
    pub fn command_sender(&self) -> mpsc::UnboundedSender<Command> {
        self.cmd_tx.clone()
    }

    /// The state channel.
    pub fn state(&self) -> watch::Receiver<AppState> {
        self.state_rx.clone()
    }

    /// Current state snapshot.
    pub fn snapshot(&self) -> AppState {
        self.state_rx.borrow().clone()
    }

    /// Counters.
    pub fn stats(&self) -> Arc<DebugStats> {
        self.shared.stats.clone()
    }

    /// Where this instance's config/data live, for UI-side file exports
    /// (the About tab's "Histogramm exportieren").
    pub fn paths(&self) -> Paths {
        self.paths.clone()
    }

    /// `PACKET_LOSS_PERC` currently applied to the encoder.
    pub fn packet_loss_perc(&self) -> i64 {
        self.shared.stats.packet_loss_perc.load(Ordering::Relaxed)
    }

    /// The runtime handle, for spawning hotkey/camera tasks.
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.runtime
            .as_ref()
            .expect("runtime alive")
            .handle()
            .clone()
    }

    /// Latest decoded video frame per session (M4).
    pub fn video_frames(&self) -> Vec<(Session, crate::video::Frame)> {
        self.shared.video.lock().unwrap().latest()
    }

    /// Sample-free evidence consumed by the native ducking controller.
    pub fn ducking_input(&self) -> crate::audio::ducking::Input {
        ducking_input(&self.shared)
    }

    /// Stops everything.
    pub fn shutdown(mut self) {
        self.stop();
        if let Ok(mut b) = self.backend.lock() {
            b.close(StreamKind::Capture);
            b.close(StreamKind::Reference);
            b.close(StreamKind::Playback);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Ends the network task — the session closes, the settings are
    /// written, an identity write in flight finishes — and then the runtime
    /// and the audio threads. Bounded: a logout deletes `identity/` right
    /// after this returns, and a renewal stored after that would bring it
    /// back, so nothing may be written to it from here on. Waits on a plain
    /// channel, not the runtime: callers may be inside another runtime.
    fn stop(&mut self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        self.shared.ducking.clear();
        self.ducking_service.take();
        let _ = self.cmd_tx.send(Command::Quit);
        if let Some(done) = self.net_done.take() {
            if done.recv_timeout(NET_STOP_WAIT) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                tracing::warn!("network task did not end within {NET_STOP_WAIT:?}");
            }
        }
        self.shared.dsp.stop.store(true, Ordering::Relaxed);
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
        restore_capture(&self.shared, &self.backend);
    }
}

fn ducking_input(shared: &Shared) -> crate::audio::ducking::Input {
    let state = shared.state.borrow();
    let (tx, rx) = shared.ducking.snapshot();
    let online =
        shared.dsp.online.load(Ordering::Relaxed) && !shared.closing.load(Ordering::Relaxed);
    crate::audio::ducking::Input {
        mode: state.settings.audio.ducking,
        route: state.active_output.clone(),
        online,
        transmitting_voice: online && tx && !shared.dsp.transmission_muted(),
        speech_db: if !online || state.deafened || state.settings.audio.output_volume <= 0.0 {
            None
        } else {
            rx
        },
        noisy_room: false,
    }
}

/// How often the audio events thread looks whether a stream that did not
/// open is due for a retry ([`StreamRetry`]).
const RETRY_POLL: Duration = Duration::from_millis(250);

/// How long [`Engine::shutdown`] waits for the network task: the close
/// handshake (≤ 0.5 s), the last settings write (≤ [`SETTINGS_FLUSH_WAIT`]).
const NET_STOP_WAIT: Duration = Duration::from_secs(3);

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The devices to open for the saved choices: `(input, output)`, each the
/// saved one while `devices` lists it and the system default (`None`) while
/// it does not ([`effective_device`]).
fn wanted_devices(settings: &Settings, devices: &[DeviceInfo]) -> (Option<String>, Option<String>) {
    let audio = &settings.audio;
    (
        effective_device(audio.input_device.as_deref(), devices, DeviceKind::Input),
        effective_device(audio.output_device.as_deref(), devices, DeviceKind::Output),
    )
}

/// The device to open for a saved choice: the saved one while the backend
/// lists it, the system default (`None`) when it does not — unplugged, say
/// — instead of targeting a device that is not there on every reopen. A
/// backend that lists no device of that kind cannot tell; the choice is
/// tried as it is.
fn effective_device(
    saved: Option<&str>,
    devices: &[DeviceInfo],
    kind: DeviceKind,
) -> Option<String> {
    let saved = saved?;
    let mut of_kind = devices.iter().filter(|d| d.kind == kind).peekable();
    if of_kind.peek().is_none() || of_kind.any(|d| d.id == saved) {
        Some(saved.to_string())
    } else {
        None
    }
}

/// [`Shared::failed_streams`]: the microphone did not open.
const FAILED_CAPTURE: u8 = 1;
/// [`Shared::failed_streams`]: the AEC reference did not open, or was
/// closed with a playback that did not ([`reference_needs_playback`]).
const FAILED_REFERENCE: u8 = 2;
/// [`Shared::failed_streams`]: the playback did not open.
const FAILED_PLAYBACK: u8 = 4;

/// Opens the microphone into the capture ring.
fn open_capture_stream(
    backend: &mut dyn AudioBackend,
    input: Option<&str>,
    channel: Option<u16>,
    shared: &Arc<Shared>,
) -> Result<(), crate::audio::BackendError> {
    let stats = shared.stats.clone();
    let cap_prod = shared.cap_prod.clone();
    let dropped = shared.capture_dropped.clone();
    let cycle = shared.cycle.clone();
    backend.set_capture_channel(channel)?;
    backend.open_capture(
        input,
        Box::new(move |block| {
            // A failed `try_lock` (stream reopen in flight) loses the block
            // like a full ring does; count both.
            let pushed = cap_prod
                .try_lock()
                .map(|mut p| p.push_slice(block))
                .unwrap_or(0);
            if pushed < block.len() {
                DebugStats::inc(&stats.capture_overruns);
                dropped.fetch_add(block.len() - pushed, Ordering::Relaxed);
            }
            cycle.capture_pushed(block.len());
        }),
    )
}

/// Opens the AEC reference into its ring.
fn open_reference_stream(
    backend: &mut dyn AudioBackend,
    output: Option<&str>,
    shared: &Arc<Shared>,
) -> Result<(), crate::audio::BackendError> {
    let ref_prod = shared.ref_prod.clone();
    let channels = shared.reference_channels.clone();
    let cycle = shared.cycle.clone();
    let stats = shared.stats.clone();
    backend.open_reference(
        output,
        Box::new(move |block, count| {
            channels.store(count, Ordering::Relaxed);
            let pushed = ref_prod
                .try_lock()
                .map(|mut p| p.push_slice(block))
                .unwrap_or(0);
            if pushed < block.len() {
                DebugStats::inc(&stats.reference_overruns);
            }
            cycle.reference_pushed();
        }),
    )?;
    shared.cycle.set_slack(backend.reference_slack());
    Ok(())
}

/// Opens the playback from the playback ring.
fn open_playback_stream(
    backend: &mut dyn AudioBackend,
    output: Option<&str>,
    shared: &Arc<Shared>,
) -> Result<(), crate::audio::BackendError> {
    let play_cons = shared.play_cons.clone();
    let ctl = shared.dsp.clone();
    let play_request = shared.play_request.clone();
    let ducking = shared.ducking.clone();
    let stats = shared.stats.clone();
    // The new device may run a smaller quantum; let the mixer's lead shrink
    // back to it instead of keeping the old device's for the session.
    play_request.store(0, Ordering::Relaxed);
    backend.open_playback(
        output,
        Box::new(move |out| {
            play_request.fetch_max(out.len(), Ordering::Relaxed);
            // Audio and its speech power travel in the same ring item. A stalled
            // output therefore cannot duck on audio it has not actually played.
            let mut speech_power = 0.0;
            let mut output_power = 0.0;
            let n = match play_cons.try_lock() {
                Ok(mut c) => {
                    let count = c.occupied_len().min(out.len());
                    for dst in &mut out[..count] {
                        let [sample, power] = c.try_pop().unwrap_or([0.0; 2]);
                        *dst = sample;
                        speech_power += power;
                        output_power += sample * sample;
                    }
                    count
                }
                Err(_) => 0,
            };
            ctl.output_rms
                .store(10.0 * (output_power / out.len().max(1) as f32).max(1e-10).log10());
            if speech_power > 0.0 && !out.is_empty() {
                ducking.played(speech_power / out.len() as f32);
            }
            if n < out.len() {
                // The mixer always queues (silence included): a gap.
                DebugStats::inc(&stats.playback_underruns);
            }
            out[n..].fill(0.0);
        }),
    )
}

/// WASAPI loopback has no clock while the endpoint is idle: without ICU's
/// own playback it can stop delivering, stalling every outgoing block.
fn reference_needs_playback(backend: &dyn AudioBackend) -> bool {
    cfg!(windows) && backend.name() == "wasapi"
}

fn open_streams(backend: &mut dyn AudioBackend, settings: &Settings, shared: &Arc<Shared>) {
    // Replace stale device errors with the outcome of this attempt. Keep
    // unrelated storage/network errors; failed opens below report a fresh one.
    shared.state.send_modify(|s| {
        if s.banner
            .as_ref()
            .is_some_and(crate::problem::Problem::is_audio)
        {
            s.banner = None;
        }
    });
    let (input, output) = wanted_devices(settings, &backend.devices());
    if input.is_none() && settings.audio.input_device.is_some() {
        tracing::info!(saved = ?settings.audio.input_device, "microphone missing, using the default");
    }
    if output.is_none() && settings.audio.output_device.is_some() {
        tracing::info!(saved = ?settings.audio.output_device, "speaker missing, using the default");
    }
    let mut failed = 0;
    if let Err(e) = open_capture_stream(
        backend,
        input.as_deref(),
        settings.audio.input_channel,
        shared,
    ) {
        tracing::warn!(error = %e, "cannot open capture");
        failed |= FAILED_CAPTURE;
        shared
            .state
            .send_modify(|s| s.banner = Some(crate::problem::audio("Das Mikrofon", &e)));
    }
    // Start the render clock before its loopback. Starting playback after the
    // reference can flag a WASAPI discontinuity and immediately kill the AEC
    // timeline again on every coordinated reopen. Match the retry order.
    if let Err(e) = open_playback_stream(backend, output.as_deref(), shared) {
        tracing::warn!(error = %e, "cannot open playback");
        failed |= FAILED_PLAYBACK;
        shared
            .state
            .send_modify(|s| s.banner = Some(crate::problem::audio("Der Lautsprecher", &e)));
    }
    let mut reference_open = false;
    if settings.audio.aec {
        if failed & FAILED_PLAYBACK != 0 && reference_needs_playback(backend) {
            backend.close(StreamKind::Reference);
            failed |= FAILED_REFERENCE;
        } else {
            match open_reference_stream(backend, output.as_deref(), shared) {
                Ok(()) => reference_open = true,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot open AEC reference");
                    failed |= FAILED_REFERENCE;
                }
            }
        }
    }
    shared
        .reference_open
        .store(reference_open, Ordering::Relaxed);
    shared.failed_streams.store(failed, Ordering::Relaxed);
    // Published last: whoever sees the devices of this attempt sees every
    // one of its streams opened.
    shared.state.send_modify(|s| {
        s.aec_reference = reference_open;
        s.active_input = input;
        s.active_output = output;
    });
}

/// Opens what [`open_streams`] could not, and only that
/// ([`Shared::failed_streams`]): the streams that run are left alone. A
/// desktop without a microphone used to reopen all three every 30 s — a
/// gap in the playback and an AEC restart each time. On the devices the
/// last open chose; a device that came or went reopens everything
/// (`Command::DevicesChanged`). A microphone or reference that opens
/// restarts the DSP's rings together, so the reference lag reads true
/// again.
fn retry_failed_streams(backend: &mut dyn AudioBackend, settings: &Settings, shared: &Arc<Shared>) {
    let failed = shared.failed_streams.load(Ordering::Relaxed);
    if failed == 0 {
        return;
    }
    let (input, output) = {
        let s = shared.state.borrow();
        (s.active_input.clone(), s.active_output.clone())
    };
    let mut still = 0;
    let mut restart_dsp = false;
    if failed & FAILED_CAPTURE != 0 {
        match open_capture_stream(
            backend,
            input.as_deref(),
            settings.audio.input_channel,
            shared,
        ) {
            Ok(()) => {
                tracing::info!("microphone opened on a retry");
                restart_dsp = true;
            }
            Err(e) => {
                tracing::debug!(error = %e, "microphone still does not open");
                still |= FAILED_CAPTURE;
            }
        }
    }
    if failed & FAILED_PLAYBACK != 0 {
        match open_playback_stream(backend, output.as_deref(), shared) {
            Ok(()) => tracing::info!("playback opened on a retry"),
            Err(e) => {
                tracing::debug!(error = %e, "playback still does not open");
                still |= FAILED_PLAYBACK;
            }
        }
    }
    let mut reference_open = shared.reference_open.load(Ordering::Relaxed);
    if failed & FAILED_REFERENCE != 0 && settings.audio.aec {
        if still & FAILED_PLAYBACK != 0 && reference_needs_playback(backend) {
            still |= FAILED_REFERENCE;
        } else {
            match open_reference_stream(backend, output.as_deref(), shared) {
                Ok(()) => {
                    tracing::info!("AEC reference opened on a retry");
                    reference_open = true;
                    restart_dsp = true;
                }
                Err(e) => {
                    tracing::debug!(error = %e, "AEC reference still does not open");
                    still |= FAILED_REFERENCE;
                }
            }
        }
    }
    if restart_dsp {
        shared.dsp.reinit_aec.store(true, Ordering::Relaxed);
    }
    shared
        .reference_open
        .store(reference_open, Ordering::Relaxed);
    shared.failed_streams.store(still, Ordering::Relaxed);
    shared.state.send_modify(|s| {
        s.aec_reference = reference_open;
        if still & (FAILED_CAPTURE | FAILED_PLAYBACK) == 0
            && s.banner
                .as_ref()
                .is_some_and(crate::problem::Problem::is_audio)
        {
            s.banner = None;
        }
    });
}

/// When the streams that did not open are tried again
/// ([`Command::RetryStreams`]): 1, 2, 4 … 30 s after the failure was
/// noticed, starting over once nothing is missing.
#[derive(Debug)]
struct StreamRetry {
    backoff: Duration,
    next: Option<Instant>,
}

impl Default for StreamRetry {
    fn default() -> Self {
        StreamRetry {
            backoff: Duration::from_secs(1),
            next: None,
        }
    }
}

impl StreamRetry {
    /// Whether a retry is due at `now`, `missing` saying whether a stream
    /// is missing.
    fn due(&mut self, now: Instant, missing: bool) -> bool {
        if !missing {
            *self = StreamRetry::default();
            return false;
        }
        match self.next {
            None => {
                self.next = Some(now + self.backoff);
                false
            }
            Some(at) if now >= at => {
                self.backoff = (self.backoff * 2).min(Duration::from_secs(30));
                self.next = Some(now + self.backoff);
                true
            }
            Some(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// DSP thread
// ---------------------------------------------------------------------------

fn dsp_thread(
    shared: Arc<Shared>,
    mut cap: HeapCons<f32>,
    mut reference: HeapCons<crate::audio::backend::ReferenceFrame>,
    mut mic_test: HeapProd<f32>,
    test_tone: bool,
) {
    let _priority = crate::audio::rt::promote_current_thread("icu-dsp");
    let ctl = shared.dsp.clone();
    let stats = shared.stats.clone();
    // Since when the mic test is on, for its time limit.
    let mut mic_test_since: Option<Instant> = None;
    let mut aec = match Aec::new() {
        Ok(a) => {
            ctl.aec_available.store(true, Ordering::Relaxed);
            shared.state.send_modify(|s| s.aec_available = true);
            Some(a)
        }
        Err(e) => {
            tracing::info!(%e, "AEC unavailable");
            None
        }
    };
    // Everything between the AEC and the encoder (`dsp::chain`).
    let mut chain = SendChain::new();
    let mut encoder = match opus::Encoder::new() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(%e, "opus encoder");
            return;
        }
    };
    stats
        .packet_loss_perc
        .store(encoder.loss_perc() as i64, Ordering::Relaxed);
    let mut auto_threshold = vad_auto::AutoThreshold::new(Instant::now());
    let mut gate = Gate::new(
        mode_of(ctl.vad_mode.load(Ordering::Relaxed)),
        Duration::from_millis(ctl.release_ms.load(Ordering::Relaxed) as u64),
        Duration::from_millis(ctl.hangover_ms.load(Ordering::Relaxed) as u64),
    );
    let tone = if test_tone {
        crate::dsp::testsignal::harmonic_voice(48_000 * 4, 0.15)
    } else {
        Vec::new()
    };
    let mut tone_pos = 0usize;
    let mut block = vec![0.0f32; BLOCK];
    let mut ref_block = vec![[0.0f32; crate::audio::backend::MAX_REFERENCE_CHANNELS]; BLOCK];
    let mut frame_acc: Vec<f32> = Vec::with_capacity(960);
    // The frame length `frame_acc` is being collected for.
    let mut acc_frame = 0usize;
    // The frames the gate held back, for the VAD pre-roll (`preroll_now`).
    let mut preroll = PreRoll::default();
    let mut prev_sent = false;
    let mut seq: u16 = 0;
    let mut ts: u32 = 0;
    let mut voice_in_frame = false;
    let mut duck_voice_in_frame = false;
    let mut duck_voice = crate::dsp::normalize::SpeechActivity::default();
    let mut ref_underruns_window = 0u32;
    let mut window_start = Instant::now();
    let mut last_state_push = Instant::now();
    // Speech heard since the last push into the UI state.
    let mut voice_since_push = false;
    // The last block the voice detector heard speech in (`voice_held`).
    let mut last_voice: Option<Instant> = None;
    let mut last_input_clip: Option<Instant> = None;
    let mut last_aec_stats = Instant::now();
    // Reference energy since the last stats push, folded with the
    // statistics into the user hints (`dsp::aec::Diagnosis`).
    let mut ref_energy = 0.0f64;
    let mut ref_samples = 0usize;
    let mut diagnosis = crate::dsp::aec::Diagnosis::default();
    let mut aec_was_on = false;
    let mut loss_applied = 0u32;
    let mut applied_send: Option<SendSettings> = None;
    let mut applied_peer = 0.0;
    let start = Instant::now();
    let mut tone_ticks: u64 = 0;
    // The network may connect during AEC/encoder initialization. Initial
    // offline capture must still be discarded at the first eligible block.
    let mut capture_was_muted = true;
    let mut capture_epoch = ctl.privacy_epoch.load(Ordering::SeqCst);

    loop {
        if ctl.stop.load(Ordering::Relaxed) {
            return;
        }
        // 1. Obtain one capture block.
        if test_tone {
            tone_ticks += 1;
            let deadline = start + Duration::from_millis(10 * tone_ticks);
            let now = Instant::now();
            if deadline > now {
                std::thread::sleep(deadline - now);
            }
            for s in block.iter_mut() {
                *s = tone[tone_pos];
                tone_pos = (tone_pos + 1) % tone.len();
            }
            // Drain whatever the real capture delivered so it does not pile up.
            let mut sink = [0.0f32; BLOCK];
            while cap.occupied_len() >= BLOCK {
                cap.pop_slice(&mut sink);
            }
            reference.clear();
            ref_block.fill([0.0; crate::audio::backend::MAX_REFERENCE_CHANNELS]);
        } else {
            // Pair the capture block with the reference block of the same
            // graph cycle (`dsp::refalign`).
            let reopened = ctl.reinit_aec.swap(false, Ordering::Relaxed);
            let lost = shared.capture_dropped.swap(0, Ordering::Relaxed);
            if reopened || lost > 0 {
                // Streams were reopened: restart both rings together so
                // their occupancy difference is the reference lag again.
                discard_capture(&mut cap, &mut frame_acc, &mut ts, lost);
                reference.clear();
                if let Some(a) = aec.as_mut() {
                    a.reinitialize();
                }
                if reopened {
                    chain.restart();
                    last_input_clip = None;
                    applied_send = None;
                    *lock(&shared.tuning) = AudioTuning::default();
                    reset_capture_detection(&mut gate, &mut auto_threshold, Instant::now());
                    diagnosis.reset();
                    ref_energy = 0.0;
                    ref_samples = 0;
                    ref_underruns_window = 0;
                    window_start = Instant::now();
                    shared.state.send_modify(|s| {
                        s.input_clipping = false;
                        s.aec_suspect = false;
                        s.aec_no_echo = false;
                        s.aec_reference_silent = false;
                    });
                } else {
                    chain.reset_history();
                    lock(&shared.tuning).capture_discontinuity();
                    lock(&shared.system_tuner).invalidate_evidence();
                }
                gate.reset();
                preroll.clear();
                voice_in_frame = false;
                duck_voice_in_frame = false;
                duck_voice = Default::default();
                voice_since_push = false;
                last_voice = None;
                prev_sent = false;
                ctl.sending.store(false, Ordering::Relaxed);
            }
            // Bound the backlog: after a stall of this thread, everything
            // older than 100 ms is stale — sending it late only adds latency
            // that the receiver's jitter buffer drops anyway, and catching up
            // on a full second in one burst is exactly the no-sleep CPU burst
            // an RTTIME limit kills. Skip the same amount from both rings so
            // the reference lag `dsp::refalign` reads off their occupancy
            // difference stays valid.
            let backlog = cap.occupied_len();
            if backlog > capture_backlog_limit(shared.cycle.capture_len()) {
                let excess = backlog - 2 * BLOCK;
                cap.skip(excess);
                reference.skip(excess);
                DebugStats::add(&stats.capture_backlog_dropped, excess as u64);
                skip_capture(&mut frame_acc, &mut ts, excess);
                preroll.clear();
                voice_in_frame = false;
                duck_voice_in_frame = false;
                duck_voice = Default::default();
                gate.reset();
                chain.reset_history();
                lock(&shared.tuning).capture_discontinuity();
                lock(&shared.system_tuner).invalidate_evidence();
            }
            let mut waited = 0u32;
            let mut ready_since: Option<Instant> = None;
            loop {
                if ctl.stop.load(Ordering::Relaxed) {
                    return;
                }
                let ref_open = shared.reference_open.load(Ordering::Relaxed);
                let step = refalign::step(
                    cap.occupied_len(),
                    reference.occupied_len(),
                    BLOCK,
                    ref_open,
                    shared.cycle.cycle_complete(),
                    ready_since.map_or(Duration::ZERO, |t| t.elapsed()),
                    shared.cycle.stall_wait(),
                );
                match step {
                    refalign::Step::Wait => {
                        if cap.occupied_len() >= BLOCK {
                            ready_since.get_or_insert_with(Instant::now);
                            std::thread::sleep(Duration::from_micros(200));
                            continue;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                        waited += 1;
                        if waited > 2000 {
                            // No capture for 2 s: keep the pipeline alive
                            // with silence.
                            block.fill(0.0);
                            ref_block.fill([0.0; crate::audio::backend::MAX_REFERENCE_CHANNELS]);
                            break;
                        }
                    }
                    refalign::Step::Pair { drop_ref } => {
                        reference.skip(drop_ref);
                        cap.pop_slice(&mut block);
                        reference.pop_slice(&mut ref_block);
                        break;
                    }
                    refalign::Step::Silent { underrun } => {
                        cap.pop_slice(&mut block);
                        ref_block.fill([0.0; crate::audio::backend::MAX_REFERENCE_CHANNELS]);
                        if underrun {
                            DebugStats::inc(&stats.reference_underruns);
                            ref_underruns_window += 1;
                        }
                        break;
                    }
                }
            }
        }
        // A reopen/overflow may have happened while waiting for capture.
        // Do not run even one block through the old microphone's state.
        if !test_tone
            && (ctl.reinit_aec.load(Ordering::Relaxed)
                || shared.capture_dropped.load(Ordering::Relaxed) > 0)
        {
            skip_capture(&mut frame_acc, &mut ts, BLOCK);
            continue;
        }
        let capture_muted = ctl.transmission_muted() || !ctl.online.load(Ordering::Relaxed);
        let block_epoch = ctl.privacy_epoch.load(Ordering::SeqCst);
        let privacy_changed = std::mem::replace(&mut capture_epoch, block_epoch) != block_epoch;
        if privacy_changed || (capture_was_muted && !capture_muted) {
            // Audio processed for meters/the mic test while muted may still
            // live in the capture ring, partial frame or filter history.
            discard_capture(&mut cap, &mut frame_acc, &mut ts, BLOCK);
            reference.clear();
            chain.reset_history();
            lock(&shared.tuning).capture_discontinuity();
            lock(&shared.system_tuner).invalidate_evidence();
            if let Some(a) = aec.as_mut() {
                a.reinitialize();
            }
            gate.reset();
            preroll.clear();
            voice_in_frame = false;
            duck_voice_in_frame = false;
            duck_voice = Default::default();
            capture_was_muted = false;
            continue;
        }
        capture_was_muted = capture_muted;
        // Whatever the capture path delivered, the AEC and the denoiser only
        // ever see numbers within ±4 (`dsp::sanitize`).
        let mut raw_capture = [0.0; BLOCK];
        raw_capture.copy_from_slice(&block);
        let raw_level = meter::measure(&raw_capture);
        ctl.raw_rms.store(raw_level.rms_db);
        ctl.raw_peak.store(raw_level.peak_db);
        crate::dsp::sanitize(&mut block);
        for frame in &mut ref_block {
            crate::dsp::sanitize(frame);
        }
        let t_block = Instant::now();
        let input_clipped = raw_capture.iter().any(|x| x.is_finite() && x.abs() >= 0.99);
        if !test_tone && input_clipped {
            last_input_clip = Some(t_block);
        }
        DebugStats::inc(&stats.dsp_blocks);

        // 2. AEC: reference first, then capture.
        let aec_on =
            ctl.aec.load(Ordering::Relaxed) && shared.reference_open.load(Ordering::Relaxed);
        if let Some(a) = aec.as_mut() {
            if aec_on != aec_was_on {
                aec_was_on = aec_on;
                // Switched on again: the render timeline has a gap the
                // filter must not carry. Switched off: the hints are stale.
                if aec_on {
                    a.reinitialize();
                }
                diagnosis.reset();
                shared.state.send_modify(|s| {
                    s.aec_suspect = false;
                    s.aec_no_echo = false;
                    s.aec_reference_silent = false;
                });
            }
            if aec_on {
                if window_start.elapsed() > Duration::from_secs(10) {
                    if ref_underruns_window > 5 {
                        tracing::info!("AEC reference dropped out, reinitializing");
                        cap.clear();
                        reference.clear();
                        a.reinitialize();
                    }
                    ref_underruns_window = 0;
                    window_start = Instant::now();
                }
                ref_energy += ref_block
                    .iter()
                    .flatten()
                    .map(|&x| (x as f64) * (x as f64))
                    .sum::<f64>();
                ref_samples += BLOCK * shared.reference_channels.load(Ordering::Relaxed).max(1);
                a.process_multichannel(
                    &ref_block,
                    shared.reference_channels.load(Ordering::Relaxed),
                    &mut block,
                );
                if last_aec_stats.elapsed() > Duration::from_secs(2) {
                    last_aec_stats = Instant::now();
                    let st = a.stats();
                    let ref_db = 10.0 * (ref_energy / ref_samples.max(1) as f64 + 1e-12).log10();
                    ref_energy = 0.0;
                    ref_samples = 0;
                    // Self-diagnosis (`docs/research/aec.md`): what the far end
                    // and the loudspeaker did since the last report, and
                    // what the canceller made of it.
                    let played = f32::from_bits(shared.play_peak.swap(0, Ordering::Relaxed));
                    let verdict =
                        diagnosis.report(ref_db > -50.0, meter::to_db(played) > -40.0, &st);
                    shared.state.send_modify(|s| {
                        s.aec_stats = st;
                        s.aec_suspect = verdict.suspect;
                        s.aec_no_echo = verdict.no_echo;
                        s.aec_reference_silent = verdict.reference_silent;
                    });
                }
            }
        }

        ctl.post_aec_rms.store(meter::measure(&block).rms_db);

        // 3. Gain, auto level, denoise, VAD (`dsp::chain`).
        let send = SendSettings {
            gain: ctl.gain.load(),
            auto_level: ctl.auto_level.load(Ordering::Relaxed),
            denoise: ctl.denoise.load(Ordering::Relaxed),
            voice_opt: ctl.voice_opt.load(Ordering::Relaxed),
            voice_strength: VoiceStrength::from_u32(ctl.voice_strength.load(Ordering::Relaxed)),
            normalize_outgoing: ctl.normalize_outgoing.load(Ordering::Relaxed),
        };
        if applied_send.is_some_and(|old| old.denoise != send.denoise) {
            reset_capture_detection(&mut gate, &mut auto_threshold, t_block);
            voice_in_frame = false;
            duck_voice_in_frame = false;
            duck_voice = Default::default();
        }
        if let Ok(mut tuning) = shared.tuning.try_lock() {
            let delta = std::mem::take(&mut tuning.capture_delta);
            let seed = tuning.seed_gain.take();
            let force_epoch = std::mem::take(&mut tuning.force_epoch);
            if applied_send != Some(send)
                || applied_peer != tuning.peer.offset_db
                || delta != 0.0
                || seed.is_some()
                || force_epoch
            {
                let epoch = shared
                    .gain_epoch
                    .fetch_add(1, Ordering::Relaxed)
                    .wrapping_add(1);
                tuning.peer.set_epoch(epoch);
                tuning.outgoing = Default::default();
                preroll.clear();
                applied_send = Some(send);
                applied_peer = tuning.peer.offset_db;
            }
            chain.tune(tuning.peer.offset_db, delta);
            if let Some(db) = seed {
                chain.warm_start(db);
            }
        }
        let analysis = chain.analyze(&mut block, &send);
        let Analysis { prob, level } = analysis;
        ctl.level_peak.store(level.peak_db);
        ctl.level_rms.store(level.rms_db);
        ctl.vad_prob.store(prob.unwrap_or(0.0));
        let manual = (!ctl.vad_auto.load(Ordering::Relaxed)).then(|| ctl.vad_threshold.load());
        let (threshold, voice) =
            analysis.classify(&mut auto_threshold, t_block, gate.is_open(), manual);
        ctl.vad_effective_threshold.store(threshold);
        voice_in_frame |= voice && !capture_muted;
        let moving_voice = !test_tone && duck_voice.observe(&block, !capture_muted);
        duck_voice_in_frame |=
            voice && !capture_muted && (prob.is_some_and(|p| p >= 0.6) || moving_voice);
        voice_since_push |= voice;
        if voice {
            last_voice = Some(t_block);
        }
        // Voice optimizer, outbound normalization and limiter.
        if ctl.vad_mode.load(Ordering::Relaxed) {
            chain.finish_vad(&mut block, voice, &send);
        } else {
            chain.finish(&mut block, voice, &send);
        }
        if !test_tone
            && ctl.online.load(Ordering::Relaxed)
            && !ctl.transmission_muted()
            && (ctl.vad_mode.load(Ordering::Relaxed) || ctl.ptt_down.load(Ordering::Relaxed))
        {
            if let Ok(mut tuning) = shared.tuning.try_lock() {
                tuning.software_gain_db = chain.gains_db().0;
                let now = Instant::now();
                let dynamics = chain.dynamics();
                if input_clipped {
                    tuning.clip_until = Some(now + Duration::from_secs(10));
                }
                if dynamics.headroom_limited {
                    tuning.software_until = Some(now + Duration::from_secs(3));
                }
                if voice {
                    tuning.dynamics_samples += dynamics.samples;
                    tuning.limited_samples += dynamics.limited_samples;
                    tuning.max_reduction = tuning.max_reduction.max(dynamics.reduction_db);
                    if dynamics.reduction_db > 3.0 {
                        tuning.heavy_samples += dynamics.samples;
                    } else {
                        tuning.heavy_samples = 0;
                    }
                    if tuning.heavy_samples >= 48_000 {
                        tuning.overload_until = Some(now + Duration::from_secs(10));
                    }
                }
                if raw_capture.iter().all(|x| x.is_finite()) {
                    tuning.outgoing.observe_at(
                        &block,
                        Some(voice),
                        shared.gain_epoch.load(Ordering::Relaxed),
                        ts.wrapping_add(frame_acc.len() as u32),
                    );
                }
                tuning
                    .capture
                    .observe(&raw_capture, voice, chain.gains_db().0);
            }
        }

        // Mic test: this block, as the encoder gets it, also to the mixer.
        if ctl.mic_test.load(Ordering::Relaxed) {
            let since = *mic_test_since.get_or_insert(t_block);
            if t_block.duration_since(since) >= MIC_TEST_MAX {
                ctl.mic_test.store(false, Ordering::Relaxed);
                mic_test_since = None;
                shared.state.send_modify(|s| s.mic_test = false);
            } else {
                mic_test.push_slice(&block);
            }
        } else {
            mic_test_since = None;
        }

        // 4. Accumulate to an Opus frame.
        // Eligibility belongs to each 10-ms block, not only the final
        // 20-ms frame: unmuting between its halves must not release the
        // half captured while muted. The mic test above still hears it.
        if capture_muted {
            block.fill(0.0);
        }
        let frame_samples = ctl.frame_samples.load(Ordering::Relaxed) as usize;
        collect_block(
            &mut frame_acc,
            &mut acc_frame,
            &mut ts,
            frame_samples,
            &block,
        );
        if frame_acc.len() >= frame_samples {
            let now = Instant::now();
            gate.set_mode(mode_of(ctl.vad_mode.load(Ordering::Relaxed)));
            gate.set_release(Duration::from_millis(
                ctl.release_ms.load(Ordering::Relaxed) as u64,
            ));
            gate.set_hangover(Duration::from_millis(
                ctl.hangover_ms.load(Ordering::Relaxed) as u64,
            ));
            let attack_ms = ctl.attack_ms.load(Ordering::Relaxed);
            gate.set_attack(Duration::from_millis(attack_for(
                attack_ms,
                crate::hotkey::typing::recent(),
            ) as u64));
            gate.set_ptt(ctl.ptt_down.load(Ordering::Relaxed) || test_tone);
            let online = ctl.online.load(Ordering::Relaxed);
            let muted = ctl.transmission_muted() || !online;
            gate.set_muted(muted);
            let was_open = gate.is_open();
            let frame_has_voice = voice_in_frame;
            let frame_has_duck_voice = std::mem::take(&mut duck_voice_in_frame);
            let decision = gate.update_audio(
                now,
                frame_has_voice,
                Duration::from_micros(frame_samples as u64 * 1_000_000 / 48_000),
            );
            voice_in_frame = false;

            // LossHint → encoder.
            let permille = ctl.loss_permille.load(Ordering::Relaxed);
            if permille != loss_applied {
                loss_applied = permille;
                let _ = encoder.set_loss_permille(permille as u16);
                stats
                    .packet_loss_perc
                    .store(encoder.loss_perc() as i64, Ordering::Relaxed);
            }

            let audience = ctl.audience.load(Ordering::Relaxed);
            let sending = decision != Decision::Silent && online && audience;
            ctl.sending.store(sending, Ordering::Relaxed);
            if ctl.cue_tx.load(Ordering::Relaxed) {
                if let Some(cue) = tx_cue(prev_sent, sending, decision) {
                    shared.pending_tx_cue.store(cue, Ordering::Relaxed);
                }
            }
            if sending {
                // Pre-roll: the frames the gate held back, and the one before
                // them that RNNoise reacted to late, carry the onset; send
                // them first with the seq/ts they would have had, so the
                // receiver sees an ordinary spurt start.
                if preroll_now(was_open, prev_sent, decision, gate.mode()) {
                    let held = preroll.take(gate.withheld() as usize + 1);
                    let n = held.len() as u32;
                    for (i, frame) in held.iter().enumerate() {
                        if frame.len() != frame_samples {
                            continue;
                        }
                        match encoder.encode(frame) {
                            Ok(packet) => {
                                send_audio(
                                    &shared,
                                    &stats,
                                    0,
                                    seq,
                                    ts.wrapping_sub((n - i as u32) * frame_samples as u32),
                                    packet,
                                    (t_block, block_epoch),
                                );
                                seq = seq.wrapping_add(1);
                            }
                            Err(e) => tracing::warn!(%e, "opus encode (pre-roll)"),
                        }
                    }
                }
                // Muted, the gate ends the spurt with this frame — collected
                // up to the mute, and partly after it. The `END` packet still
                // goes out so receivers stop at once, but as silence: nothing
                // captured while muted may leave the machine.
                if muted {
                    frame_acc[..frame_samples].fill(0.0);
                }
                match encoder.encode(&frame_acc[..frame_samples]) {
                    Ok(packet) => {
                        let flags = if decision == Decision::SendEnd {
                            AUDIO_FLAG_END
                        } else {
                            0
                        };
                        let sent = send_audio(
                            &shared,
                            &stats,
                            flags,
                            seq,
                            ts,
                            packet,
                            (t_block, block_epoch),
                        );
                        if sent && frame_has_duck_voice && !muted && !test_tone {
                            shared.ducking.transmitted();
                        }
                        seq = seq.wrapping_add(1);
                    }
                    Err(e) => tracing::warn!(%e, "opus encode"),
                }
            }
            ts = ts.wrapping_add(frame_samples as u32);
            // Keep this frame as the next one's pre-roll, unless the gate was
            // muted: nothing captured while muted may leave the machine.
            if muted || sending {
                preroll.clear();
            } else {
                preroll.push(
                    &frame_acc[..frame_samples],
                    preroll::depth(attack_ms.saturating_mul(2), frame_samples as u32 / 48),
                );
            }
            prev_sent = sending;
            frame_acc.drain(..frame_samples);
        }

        stats.dsp_block.lock().unwrap().record(t_block.elapsed());

        // 5. Push level/sending into the UI state at ~33 Hz (the meter
        // wants ≥ 30 Hz): checked once per 10-ms block, so ≥ 25 ms means
        // every third block.
        if last_state_push.elapsed() >= Duration::from_millis(25) {
            last_state_push = Instant::now();
            let sending = ctl.sending.load(Ordering::Relaxed);
            let ptt = ctl.ptt_down.load(Ordering::Relaxed);
            let p = ctl.vad_prob.load();
            let vad_threshold = ctl.vad_effective_threshold.load();
            let hangover = Duration::from_millis(ctl.hangover_ms.load(Ordering::Relaxed) as u64);
            let voice = std::mem::take(&mut voice_since_push)
                || voice_held(last_voice, last_state_push, hangover);
            let input_clipping = last_input_clip.is_some_and(|when| {
                last_state_push.saturating_duration_since(when) < Duration::from_secs(1)
            });
            shared.state.send_modify(|s| {
                s.level = level;
                s.input_clipping = input_clipping;
                s.vad_prob = p;
                s.vad_threshold = vad_threshold;
                s.sending = sending;
                s.ptt_down = ptt;
                s.voice = voice;
            });
        }
    }
}

/// Adds one capture block to the Opus frame being collected. When the frame
/// length changed since (`Command::SetFrameMs`), what was collected towards
/// the old length is dropped first and `ts` moves past it, so the sample
/// clock stays true and receivers see a new talk spurt. Kept, it stayed in
/// front of every later frame: 20 → 10 ms with half a long frame collected
/// sent everything 10 ms late until the next change.
fn collect_block(
    acc: &mut Vec<f32>,
    acc_frame: &mut usize,
    ts: &mut u32,
    frame_samples: usize,
    block: &[f32],
) {
    if *acc_frame != frame_samples {
        *acc_frame = frame_samples;
        *ts = ts.wrapping_add(acc.len() as u32);
        acc.clear();
    }
    acc.extend_from_slice(block);
}

/// A new microphone or probability source needs a fresh threshold and attack.
fn reset_capture_detection(gate: &mut Gate, threshold: &mut vad_auto::AutoThreshold, now: Instant) {
    gate.reset();
    *threshold = vad_auto::AutoThreshold::new(now);
}

/// Keep both admitted and candidate speech out of the automatic noise floor.
/// A closed gate may be waiting for the VAD attack, muted, or waiting for PTT;
/// none makes an above-threshold block evidence of ambient noise. The
/// AutoThreshold starvation watchdog still recovers from sustained noise.
#[cfg(test)]
fn floor_sample_is_speech(gate_open: bool, prob: f32, threshold: f32) -> bool {
    gate_open || prob >= threshold
}

/// Whether the voice detector still counts as hearing speech at `now`
/// ([`AppState::voice`]): its last voiced block is at most `hold` — the VAD
/// hangover — old. Speech dips below the threshold between words; without
/// the hold the talking-while-muted cue flickered at the push rate where an
/// open gate stays open.
fn voice_held(last_voice: Option<Instant>, now: Instant, hold: Duration) -> bool {
    last_voice.is_some_and(|t| now.saturating_duration_since(t) <= hold)
}

/// The gate mode the `vad_mode` atomic encodes.
fn mode_of(vad: bool) -> Mode {
    if vad {
        Mode::Vad
    } else {
        Mode::Ptt
    }
}

/// The attack time in effect: while keys are being struck, voice has to last
/// twice as long to open the gate (`hotkey::typing`). Off with the setting.
fn attack_for(attack_ms: u32, typing: bool) -> u32 {
    if typing {
        attack_ms.saturating_mul(2)
    } else {
        attack_ms
    }
}

/// Whether the frame before this one goes out first: the gate just opened in
/// VAD mode. RNNoise flags voice a block or two into a word, so the onset is
/// in the frame that was *not* sent; sending it too costs one packet and
/// 20 ms of pre-speech audio per spurt, not latency — both packets arrive
/// together and the jitter buffer's prefill starts on the pair
/// (`docs/reference/client.md` §6). PTT users press before they speak. A
/// frame that already went out (the END frame, when voice returns right
/// after the hangover) is never sent twice.
fn preroll_now(was_open: bool, prev_sent: bool, decision: Decision, mode: Mode) -> bool {
    !was_open && !prev_sent && decision != Decision::Silent && mode == Mode::Vad
}

/// How long the mic test (`Command::SetMicTest`) plays before it switches
/// itself off.
pub const MIC_TEST_MAX: Duration = Duration::from_secs(60);

/// [`Shared::pending_tx_cue`]: sending started.
const TX_CUE_START: u8 = 1;
/// [`Shared::pending_tx_cue`]: sending ended.
const TX_CUE_STOP: u8 = 2;

/// The transmit cue for one frame (`audio.cue_tx`): when sending starts,
/// and when its last packet — the one with `END` — goes out. VAD flutter
/// within the hangover or the PTT release delay keeps the gate open, so it
/// makes no edge and no cue.
fn tx_cue(was_sending: bool, sending: bool, decision: Decision) -> Option<u8> {
    if !sending {
        None
    } else if !was_sending {
        Some(TX_CUE_START)
    } else {
        (decision == Decision::SendEnd).then_some(TX_CUE_STOP)
    }
}

/// Wraps one encoded frame in its header and hands it to the connection.
/// Counted only once quinn accepted it.
fn send_audio(
    shared: &Arc<Shared>,
    stats: &DebugStats,
    flags: u8,
    seq: u16,
    ts: u32,
    packet: &[u8],
    captured: (Instant, usize),
) -> bool {
    let (t_block, epoch) = captured;
    let header = AudioHeader {
        gain_epoch: shared.gain_epoch.load(Ordering::Relaxed),
        flags,
        speaker: 0,
        seq,
        ts,
    };
    let Ok(datagram) = header.datagram(packet) else {
        return false;
    };
    // `into()` moves the buffer, so take its length first.
    let wire_bytes = datagram.len() as u64;
    let conn = lock(&shared.conn);
    // Serialized with SetPrivacy: no block from an earlier privacy epoch
    // can be submitted after the switch, even when toggled twice quickly.
    if epoch % 2 == 1 || shared.dsp.privacy_epoch.load(Ordering::SeqCst) != epoch {
        return false;
    }
    if let Some(c) = conn.as_ref() {
        if c.send_datagram(datagram.into()).is_ok() {
            DebugStats::inc(&stats.datagrams_sent);
            DebugStats::add(&stats.audio_bytes_sent, wire_bytes);
            stats
                .capture_to_send
                .lock()
                .unwrap()
                .record(t_block.elapsed());
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Mixer thread
// ---------------------------------------------------------------------------

fn mixer_thread(shared: Arc<Shared>, mut play: HeapProd<[f32; 2]>, mut mic_test: HeapCons<f32>) {
    let _priority = crate::audio::rt::promote_current_thread("icu-mixer");
    let stats = shared.stats.clone();
    let mut out = vec![0.0f32; BLOCK];
    let mut speech_mix = [0.0f32; BLOCK];
    let mut played = [[0.0f32; 2]; BLOCK];
    let mut mic_buf = vec![0.0f32; BLOCK];
    // The mic test plays once two blocks are queued, and primes again after
    // running dry: capture and playback callbacks do not tick in lockstep.
    let mut mic_primed = false;
    // One block per speaker, kept across iterations: nothing is allocated on
    // this thread once every speaker has its buffer.
    let mut inputs: Vec<(Vec<f32>, f32)> = Vec::new();
    let mut cue_buf = vec![0.0f32; BLOCK];
    // Rendered once: this thread must not allocate per block.
    let tx_start = crate::cues::render(Cue::TxStart);
    let tx_stop = crate::cues::render(Cue::TxStop);
    // Speaking indicator changes of this block, published after the
    // speakers lock is released; sized for `MAX_MAX_USERS` speakers.
    let mut indicator_changes: Vec<(Session, Option<bool>)> =
        Vec::with_capacity(pl::MAX_MAX_USERS as usize);
    let mut stalled_since: Option<Instant> = None;
    let mut request = RecentRequest::new(Instant::now());
    // 120 ms, the longest frame Opus allows: a peer picks its frame length.
    let mut fec_scratch = vec![0.0f32; 5760];
    loop {
        if shared.dsp.stop.load(Ordering::Relaxed) {
            return;
        }
        // Keep two blocks (20 ms) ahead of the playback callback, or one
        // callback plus a block when the graph quantum is larger than that.
        let lead = (2 * BLOCK)
            .max(request.get(&shared.play_request, Instant::now()) + BLOCK)
            .min(play.capacity().get() - BLOCK);
        if play.occupied_len() >= lead {
            // If nobody consumes (no playback stream), keep the jitter buffers
            // flowing by discarding.
            let stalled = match stalled_since {
                Some(t) => t.elapsed() > Duration::from_millis(200),
                None => {
                    stalled_since = Some(Instant::now());
                    false
                }
            };
            if !stalled {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            // Nobody consumes: discard at playback pace, not CPU speed, or the
            // jitter buffers drain instantly and report fake losses.
            std::thread::sleep(Duration::from_millis(10));
        } else {
            stalled_since = None;
        }

        let now = Instant::now();
        let deafened = shared.deafened.load(Ordering::Relaxed);
        let master = shared.output_gain.load();
        let normalize = shared.normalize_incoming.load(Ordering::Relaxed);
        let mut n_inputs = 0;
        speech_mix.fill(0.0);
        {
            let mut speakers = lock(&shared.speakers);
            for (session, sp) in speakers.iter_mut() {
                // Speaking indicator from the local datagram path, every
                // block: on within a block of the first packet, off two
                // frames after the last or at once on `END`.
                let local = local_indicator(sp.last_packet, sp.ended, now);
                if local != sp.indicator {
                    sp.indicator = local;
                    indicator_changes.push((*session, local));
                }
                let before = sp.jitter.stats();
                while sp.pending.len() < BLOCK {
                    match sp.jitter.pop_quiet_at(
                        now,
                        now + Duration::from_secs_f64(
                            (play.occupied_len() + sp.pending.len()) as f64 / 48_000.0,
                        ),
                        sp.quiet,
                    ) {
                        Pop::Packet(p) => {
                            if let Some(d) = sp.jitter.last_delay() {
                                stats.jitter_delay.lock().unwrap().record(d);
                            }
                            let n = sp.decoder.decode(&p.payload, &mut fec_scratch).unwrap_or(0);
                            DebugStats::inc(&stats.frames_decoded);
                            if !deafened && sp.user_id != 0 {
                                sp.feedback
                                    .observe_at(&fec_scratch[..n], None, p.gain_epoch, p.ts);
                            }
                            sp.quiet = frame_quiet(&fec_scratch[..n]);
                            queue_decoded(sp, n, &fec_scratch, false);
                        }
                        Pop::Lost {
                            fec_source,
                            samples,
                            ..
                        } => {
                            let n = match fec_source {
                                Some(next) => {
                                    DebugStats::inc(&stats.frames_fec_attempted);
                                    let decoded = sp
                                        .decoder
                                        .decode_fec_for(
                                            &next.payload,
                                            &mut fec_scratch,
                                            samples as usize,
                                        )
                                        .unwrap_or(0);
                                    let recovered = sp.decoder.last_fec_samples();
                                    if recovered > 0 {
                                        DebugStats::inc(&stats.frames_fec);
                                        DebugStats::add(
                                            &stats.frames_fec_samples,
                                            recovered as u64,
                                        );
                                    } else {
                                        DebugStats::inc(&stats.frames_fec_fallback);
                                        DebugStats::inc(&stats.frames_plc);
                                    }
                                    decoded
                                }
                                None => {
                                    DebugStats::inc(&stats.frames_plc);
                                    sp.decoder
                                        .conceal_for(&mut fec_scratch, samples as usize)
                                        .unwrap_or(0)
                                }
                            };
                            sp.quiet = frame_quiet(&fec_scratch[..n]);
                            queue_decoded(sp, n, &fec_scratch, true);
                        }
                        Pop::Silence => {
                            sp.quiet = true;
                            sp.pending.extend(std::iter::repeat_n(0.0, BLOCK));
                        }
                    }
                }
                let after = sp.jitter.stats();
                DebugStats::add(&stats.jitter_underruns, after.underruns - before.underruns);
                DebugStats::add(&stats.jitter_stretched, after.stretched - before.stretched);
                DebugStats::add(&stats.jitter_skipped, after.skipped - before.skipped);
                DebugStats::add(&stats.jitter_dropped, after.dropped - before.dropped);
                DebugStats::add(&stats.jitter_lost, after.lost - before.lost);
                if inputs.len() <= n_inputs {
                    inputs.push((vec![0.0f32; BLOCK], 0.0));
                }
                let (buf, gain) = &mut inputs[n_inputs];
                n_inputs += 1;
                let have = sp.pending.len().min(BLOCK);
                for (dst, src) in buf.iter_mut().zip(sp.pending.drain(..have)) {
                    *dst = src;
                }
                buf[have..].fill(0.0);
                // A concealed or FEC-recovered frame says nothing about how
                // loud the speaker is: the normalizer holds on it.
                let hint = if sp.concealed > 0 { Some(false) } else { None };
                sp.concealed = sp.concealed.saturating_sub(have);
                let speech = sp
                    .duck_voice
                    .observe(buf, hint != Some(false) && have == BLOCK);
                if normalize {
                    sp.norm.process_hinted(buf, hint);
                } else {
                    sp.norm.reset();
                }
                let volume = if deafened {
                    0.0
                } else {
                    mixer::volume_gain(sp.volume) * master
                };
                sp.volume_ramp.apply(buf, volume);
                if speech && volume > 0.0 && shared.dsp.online.load(Ordering::Relaxed) {
                    for (sum, sample) in speech_mix.iter_mut().zip(buf.iter()) {
                        *sum += *sample;
                    }
                }
                *gain = 1.0;
            }
        }
        // Cues are heard even when deafened.
        let cue = {
            let mut cues = shared.cues.lock().unwrap();
            match shared.pending_tx_cue.swap(0, Ordering::Relaxed) {
                TX_CUE_START => {
                    cues.push(Cue::TxStart, &tx_start);
                }
                TX_CUE_STOP => {
                    cues.push(Cue::TxStop, &tx_stop);
                }
                _ => {}
            }
            if cues.fill(&mut cue_buf) {
                Some((cue_buf.as_slice(), 1.0))
            } else {
                None
            }
        };
        // The mic test, like the cues: unity gain, heard even when deafened.
        let mic = if shared.dsp.mic_test.load(Ordering::Relaxed) {
            let queued = mic_test.occupied_len();
            if queued > 4 * BLOCK {
                // Fell behind (a stall): back to two blocks of delay.
                mic_test.skip(queued - 2 * BLOCK);
            }
            mic_primed |= mic_test.occupied_len() >= 2 * BLOCK;
            if mic_primed {
                let n = mic_test.pop_slice(&mut mic_buf);
                mic_primed = n == BLOCK;
                mic_buf[n..].fill(0.0);
                Some((mic_buf.as_slice(), 1.0))
            } else {
                None
            }
        } else {
            mic_primed = false;
            mic_test.clear();
            None
        };
        mixer::mix(
            inputs[..n_inputs]
                .iter()
                .map(|(b, g)| (b.as_slice(), *g))
                .chain(cue)
                .chain(mic),
            &mut out,
        );
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        shared
            .play_peak
            .fetch_max(peak.to_bits(), Ordering::Relaxed);
        if play.occupied_len() < lead {
            for ((item, sample), speech) in played.iter_mut().zip(&out).zip(&speech_mix) {
                *item = [*sample, speech.clamp(-1.0, 1.0).powi(2)];
            }
            let n = play.push_slice(&played);
            DebugStats::add(&stats.samples_played, n as u64);
        }

        // Published only when the local path changed its mind, and outside
        // the speakers lock: the window is not woken per block.
        if !indicator_changes.is_empty() {
            shared.state.send_if_modified(|s| {
                let mut changed = false;
                for p in s.peers.iter_mut() {
                    if let Some((_, local)) = indicator_changes
                        .iter()
                        .find(|(session, _)| *session == p.info.session)
                    {
                        let want = local.unwrap_or(p.info.flags.speaking());
                        changed |= std::mem::replace(&mut p.speaking, want) != want;
                    }
                }
                changed
            });
            indicator_changes.clear();
        }
    }
}

/// How long a playback request sets the mixer's lead ([`RecentRequest`]).
const PLAY_REQUEST_WINDOW: Duration = Duration::from_secs(2);

/// The largest playback request of the current and the previous
/// [`PLAY_REQUEST_WINDOW`]: the callback raises [`Shared::play_request`],
/// the mixer takes it every window. A graph quantum that grew for a while —
/// another client asking for 8192 samples — used to hold the lead at ~190 ms
/// until the next reopen; now it follows the quantum back within 2–4 s.
struct RecentRequest {
    prev: usize,
    since: Instant,
}

impl RecentRequest {
    fn new(now: Instant) -> RecentRequest {
        RecentRequest {
            prev: 0,
            since: now,
        }
    }

    fn get(&mut self, current: &AtomicUsize, now: Instant) -> usize {
        if now.saturating_duration_since(self.since) >= PLAY_REQUEST_WINDOW {
            self.prev = current.swap(0, Ordering::Relaxed);
            self.since = now;
        }
        self.prev.max(current.load(Ordering::Relaxed))
    }
}

/// How long a speaker counts as talking after its last datagram: two
/// 20-ms frames plus a little arrival jitter.
const SPEAKING_HOLD: Duration = Duration::from_millis(60);

/// How long after its last datagram the local path still decides a
/// speaker's indicator; after that the server's `speaking` flag does
/// (`docs/reference/client.md` §6: deafened, peer at volume 0, no audio).
const LOCAL_AUTHORITY: Duration = Duration::from_secs(1);

/// The speaking indicator as the local datagram path sees it: `None` when
/// none of the speaker's audio arrived within [`LOCAL_AUTHORITY`] — the
/// server's flag decides then — otherwise whether a datagram arrived within
/// [`SPEAKING_HOLD`] and the last one did not end the talk spurt. Without
/// the `None`, the server's flag, cleared only on `END` or after 500 ms
/// without audio, kept every indicator lit long after the voice stopped.
fn local_indicator(last_packet: Option<Instant>, ended: bool, now: Instant) -> Option<bool> {
    let age = now.saturating_duration_since(last_packet?);
    (age < LOCAL_AUTHORITY).then_some(!ended && age < SPEAKING_HOLD)
}

/// Queues `n` decoded samples from `scratch` for playout — or, when the
/// decoder produced nothing, the speaker's frame length of silence, so its
/// timeline keeps its shape rather than gaining a block. `concealed`
/// marks PLC/FEC output, which the normalizer must not learn from.
fn queue_decoded(sp: &mut Speaker, n: usize, scratch: &[f32], concealed: bool) {
    if n == 0 {
        sp.pending
            .extend(std::iter::repeat_n(0.0, sp.jitter.frame_samples() as usize));
    } else {
        sp.pending.extend(scratch[..n].iter().copied());
    }
    if concealed {
        sp.concealed = sp.pending.len();
    }
}

// ---------------------------------------------------------------------------
// Network task
// ---------------------------------------------------------------------------

async fn net_task(
    cfg: EngineConfig,
    shared: Arc<Shared>,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    backend: Arc<Mutex<Box<dyn AudioBackend>>>,
) {
    // Settings reach the disk from their own task, off the two workers that
    // also time the audio datagrams (`settings_writer`).
    let (settings_tx, settings_rx) = mpsc::unbounded_channel();
    let writer = tokio::spawn(settings_writer(cfg.paths.clone(), settings_rx));
    let mut local = LocalState {
        // `Hello` carries the switches kept from the last run.
        flags: UserFlags::NONE
            .with_bit(UserFlags::MUTED, cfg.settings.audio.muted)
            .with_bit(UserFlags::CAMERA_ON, cfg.settings.camera.enabled),
        video_viewers: HashMap::new(),
        last_display_report: HashMap::new(),
        settings: cfg.settings.clone(),
        settings_tx,
        paths: cfg.paths.clone(),
        // The name is whatever the certificate says, so the pre-`Welcome`
        // display cannot contradict it (`docs/reference/protocol.md` §2.1).
        name: crate::conninfo::CertInfo::from_der(&cfg.identity.leaf_der).subject,
        backend,
        identity: cfg.identity.clone(),
        pending_renew: None,
        renew_only: false,
        invite_pending: false,
        renew_attempted_at: None,
        window_visible: !cfg.settings.ui.start_minimized,
        muted_before_deafen: false,
        notifier: crate::notify::Notifier::default(),
        trial: None,
        calibration: blocking(|| crate::audio::calibration::Store::load(&cfg.paths)),
        calibration_learner: Default::default(),
        corona_seen: cfg.settings.overlay.frame_color,
        corona_pending: None,
    };
    local.update_viewing();
    blocking(|| tune_capture(&shared, &local));
    let mut attempt: u32 = 0;
    let mut first = true;
    loop {
        if !first {
            DebugStats::inc(&shared.stats.reconnects);
        }
        first = false;
        shared
            .state
            .send_modify(|s| s.conn = ConnStatus::Connecting);
        let addr = match cfg.server_addr {
            Some(a) => Ok(a),
            // Commands keep working while the resolver stalls — mute and
            // deafen must not wait for getaddrinfo's timeout — and the
            // lookup itself is bounded. Quit ends it; a new identity starts
            // over with its own host.
            None => {
                let (host, port) = (
                    local.identity.server.host.clone(),
                    local.identity.server.port,
                );
                tokio::select! {
                    r = tokio::time::timeout(RESOLVE_TIMEOUT, crate::net::resolve(&host, port)) => {
                        r.unwrap_or_else(|_| Err(NetError::Resolve(format!(
                            "{host}: no answer within {RESOLVE_TIMEOUT:?}"
                        ))))
                    }
                    end = drain_until_quit(&mut cmd_rx, &mut local, &shared) => match end {
                        SessionEnd::Quit => break,
                        _ => {
                            attempt = 0;
                            continue;
                        }
                    }
                }
            }
        };
        let mut was_connected = false;
        let outcome = match addr {
            Ok(addr) => {
                run_session(
                    &cfg,
                    &shared,
                    &mut local,
                    &mut cmd_rx,
                    addr,
                    &mut was_connected,
                )
                .await
            }
            Err(e) => SessionEnd::Retry(e.problem()),
        };
        if was_connected {
            attempt = 0;
        }
        // Here, not at the end of `run_session`: that function returns early
        // from half a dozen places, and a stale `conn_info` outliving the
        // connection it describes is exactly the lie the Verbindung window
        // must not tell.
        end_session(&shared);
        local.forget_session(&shared);
        let slowly = matches!(outcome, SessionEnd::RetrySlowly(_));
        match outcome {
            SessionEnd::Quit => break,
            // An import that failed before it proved itself gives way to
            // the identity it replaced, which connects again at once.
            SessionEnd::Fatal(reason) if local.undo_import(&reason, &shared) => {
                attempt = 0;
            }
            SessionEnd::Fatal(reason) => {
                shared
                    .state
                    .send_modify(|s| s.conn = ConnStatus::Fatal(reason));
                // Keep serving local commands (mute etc.) until Quit. A new
                // identity (passport import) is the way out: reconnect with it.
                match drain_until_quit(&mut cmd_rx, &mut local, &shared).await {
                    SessionEnd::Quit => break,
                    _ => attempt = 0,
                }
            }
            SessionEnd::Retry(reason) | SessionEnd::RetrySlowly(reason) => {
                let wait = if slowly {
                    conn::slow_backoff(attempt)
                } else {
                    conn::backoff(attempt)
                };
                attempt += 1;
                shared.state.send_modify(|s| {
                    s.conn = ConnStatus::Disconnected { reason, attempt };
                });
                // A new identity is worth trying right away.
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    end = drain_until_quit(&mut cmd_rx, &mut local, &shared) => match end {
                        SessionEnd::Quit => break,
                        _ => attempt = 0,
                    }
                }
            }
        }
    }
    // Quit. Dropping the sender makes the writer store what is still
    // pending: a change right before quitting must not be lost to the
    // debounce.
    drop(local);
    if tokio::time::timeout(SETTINGS_FLUSH_WAIT, writer)
        .await
        .is_err()
    {
        tracing::warn!("settings not written within the shutdown wait");
    }
}

/// How long a name lookup of the server may take before it counts as
/// failed and is retried with the backoff.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long changes must be quiet before `settings.toml` is written.
const SETTINGS_QUIET: Duration = Duration::from_millis(500);

/// How long a quitting engine waits for the last settings write.
const SETTINGS_FLUSH_WAIT: Duration = Duration::from_secs(2);

/// Writes `settings.toml` once changes have been quiet for
/// [`SETTINGS_QUIET`], through `spawn_blocking`. A slider drag commits a
/// change every UI frame; each used to be an atomic write with `fsync` right
/// on one of the two network workers, which also stamp the arrival of the
/// audio datagrams. When the sender is dropped, whatever is pending is
/// written before the task ends. Returns the number of writes (tests).
async fn settings_writer(paths: Paths, mut rx: mpsc::UnboundedReceiver<Settings>) -> usize {
    let mut writes = 0;
    while let Some(mut latest) = rx.recv().await {
        // Newer ones replace it until the changes are quiet or the engine
        // quits; then it is written.
        while let Ok(Some(newer)) = tokio::time::timeout(SETTINGS_QUIET, rx.recv()).await {
            latest = newer;
        }
        let paths = paths.clone();
        match tokio::task::spawn_blocking(move || latest.save(&paths)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "cannot write settings.toml"),
            Err(e) => tracing::warn!(error = %e, "settings writer failed"),
        }
        writes += 1;
    }
    writes
}

enum SessionEnd {
    Quit,
    Fatal(Problem),
    Retry(Problem),
    /// Retried after [`conn::slow_backoff`]: a server certificate nobody
    /// vouched for (`ConnectError::retries_slowly`).
    RetrySlowly(Problem),
}

impl From<ConnectError> for SessionEnd {
    /// Fatal codes end the reconnect loop (`docs/reference/protocol.md` §5); everything
    /// else is retried.
    fn from(e: ConnectError) -> SessionEnd {
        if e.is_fatal() {
            SessionEnd::Fatal(e.problem())
        } else if e.retries_slowly() {
            SessionEnd::RetrySlowly(e.problem())
        } else {
            SessionEnd::Retry(e.problem())
        }
    }
}

impl From<quinn::ConnectionError> for SessionEnd {
    fn from(reason: quinn::ConnectionError) -> SessionEnd {
        ConnectError::from_connection_error(reason).into()
    }
}

/// Why `connection` ended, as a session end — `None` while it is still open.
fn closed_end(connection: &quinn::Connection) -> Option<SessionEnd> {
    connection.close_reason().map(SessionEnd::from)
}

/// The connection went away under a running session, `technical` saying
/// how that was noticed; retried like any other transport failure.
fn connection_lost(technical: &str) -> SessionEnd {
    SessionEnd::Retry(Problem::new(
        "Die Verbindung zum Server ist abgebrochen. ICU verbindet sich neu.",
        technical,
    ))
}

/// A control-stream write failed: the connection is gone. Its close code,
/// when there is one, says why — a write racing the server's `SHUTDOWN`
/// must not turn a restart into an anonymous abort.
fn send_failed(connection: &quinn::Connection) -> SessionEnd {
    closed_end(connection).unwrap_or_else(|| connection_lost("send failed"))
}

/// Forgets what the previous connection learned about the network: the
/// `LossHint` the encoder's FEC follows, and the tracker's hints and
/// transport history the video tier follows. They describe a connection
/// that ended — often a bad one — and a clean network sends nothing to
/// replace them: receivers report only a change of ≥ 10 ‰, the server's
/// congestion hint only a change of ≥ 20 ‰. Left standing, a `LossHint` of
/// ≥ 30 ‰ held the FEC high and walked the video down to paused for good.
fn forget_network_feedback(shared: &Shared) {
    shared.dsp.loss_permille.store(0, Ordering::Relaxed);
    *lock(&shared.net) = NetworkTracker::new();
}

/// Forgets everything scoped to one session: the connection the DSP thread
/// sends on, the speakers, the received video, and what the window shows
/// about the connection and the people on it.
fn end_session(shared: &Arc<Shared>) {
    *lock(&shared.video_tx) = None;
    *lock(&shared.conn) = None;
    shared.dsp.online.store(false, Ordering::Relaxed);
    shared.dsp.audience.store(false, Ordering::Relaxed);
    drop_speakers(shared);
    lock(&shared.video).reset();
    shared.state.send_modify(|s| {
        s.peers.clear();
        s.video_viewers.clear();
        s.conn_info = None;
        s.conn_stats = None;
        s.server_rtt_ms = None;
        s.clock_offset_ms = None;
        s.notice = None;
        s.restricted_muted = false;
        s.restricted_deafened = false;
        s.audio_health = health_without_feedback(
            &s.audio_health,
            if s.settings.audio.auto_level && s.settings.audio.denoise {
                crate::audio::health::State::LocalOnly
            } else {
                crate::audio::health::State::Disabled
            },
        );
        s.audio_health.system_status = "Keine Verbindung – Systemregelung pausiert".into();
        s.tuning_status = s.audio_health.diagnostic();
    });
}

/// Stores an identity the server just issued on `connection` and takes it
/// into use: written first, then the session closed with `ok` — for a
/// registration that close is the receipt which spends the bundle — so the
/// next connect runs under the new leaf. When the write fails the session
/// closes with `fail` and the identity in force stays; the problem names
/// `what` could not be stored.
fn adopt_issued(
    local: &mut LocalState,
    shared: &Arc<Shared>,
    connection: &quinn::Connection,
    identity: Identity,
    ok: quinn::VarInt,
    fail: quinn::VarInt,
    what: &str,
) -> Result<(), Problem> {
    match store_identity(&identity, &local.paths, shared, false) {
        Ok(()) => {
            connection.close(ok, b"");
            local.adopt(identity, shared);
            // Issued by the server and on disk: whatever import led here
            // has proven itself.
            local.trial = None;
            Ok(())
        }
        Err(e) => {
            connection.close(fail, b"");
            Err(crate::problem::storage(what, &e))
        }
    }
}

/// Writes `identity` to disk before the engine takes it into use — unless
/// the engine is shutting down: a logout deletes `identity/` as soon as
/// `Engine::shutdown` returns, and a write after that would bring it back.
/// `keep_previous` keeps the set it replaces for an import on trial
/// (`Identity::save_keeping_previous`, [`ImportTrial`]).
fn store_identity(
    identity: &Identity,
    paths: &Paths,
    shared: &Shared,
    keep_previous: bool,
) -> Result<(), crate::config::ConfigError> {
    if shared.closing.load(Ordering::SeqCst) {
        return Err(crate::config::ConfigError::Io(std::io::Error::other(
            "the engine is shutting down",
        )));
    }
    if keep_previous {
        identity.save_keeping_previous(paths)
    } else {
        identity.save(paths)
    }
}

/// Runs `f`, which may block for seconds — opening an audio device waits up
/// to 5 s per stream (WASAPI) — through `block_in_place`: the connection's
/// other tasks on this worker (the datagram reader stamping arrival times,
/// quinn's driver) move to another thread meanwhile, instead of waiting
/// behind it. Still in order with every other command: this task waits.
/// Outside a multi-thread runtime `f` just runs.
fn blocking<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// Deliberate reconnect with new credentials; not a failure.
fn reconnect_with_new_identity() -> SessionEnd {
    SessionEnd::Retry(Problem::plain(
        "Neue Zugangsdaten übernommen – verbinde neu …",
    ))
}

struct LocalState {
    calibration: crate::audio::calibration::Store,
    calibration_learner: crate::audio::calibration::Learner,
    flags: UserFlags,
    video_viewers: HashMap<Session, Instant>,
    last_display_report: HashMap<Session, Instant>,
    settings: Settings,
    /// Every committed change of `settings`, for `settings_writer`.
    settings_tx: mpsc::UnboundedSender<Settings>,
    paths: Paths,
    name: String,
    backend: Arc<Mutex<Box<dyn AudioBackend>>>,
    /// The identity in force. Lives here and not in `EngineConfig` because a
    /// renewal replaces the leaf mid-run and the next reconnect has to use the
    /// new one; `LocalState` is the only thing that survives a session.
    identity: Identity,
    /// The renewal this client is waiting for, if any.
    pending_renew: Option<PendingRenew>,
    /// The server said `RENEW_REQUIRED`: the next session renews first.
    renew_only: bool,
    /// A `CreateInviteRequest` is waiting for its answer.
    invite_pending: bool,
    /// When the last automatic renewal was sent. The server allows one a
    /// day, so the next try waits that long.
    renew_attempted_at: Option<Instant>,
    /// Whether the main window is on screen. Starts from
    /// `start_minimized`, so a client without a window (tests) views.
    window_visible: bool,
    /// The mute from before deafening, restored by undeafening.
    muted_before_deafen: bool,
    /// Join/leave notifications (`UiSettings::notify_join`).
    notifier: crate::notify::Notifier,
    /// An imported identity that has yet to prove itself, and what to go
    /// back to if it cannot.
    trial: Option<ImportTrial>,
    /// The own corona color as last committed, to tell a picked color from
    /// settings that changed elsewhere.
    corona_seen: [u8; 3],
    /// When a color the server has not heard of yet was picked. It goes
    /// out [`CORONA_DEBOUNCE`] after the last pick — the picker reports
    /// every step of a drag — or right after the next `Welcome`.
    corona_pending: Option<Instant>,
}

/// How long a picked corona color settles before it is sent.
const CORONA_DEBOUNCE: Duration = Duration::from_millis(500);

/// The own corona color as the server keeps it: the default is `None`.
fn corona_wire(color: [u8; 3]) -> Option<Rgb> {
    (color != crate::config::DEFAULT_FRAME_COLOR).then_some(color)
}

/// The identity in force before an import, kept until the imported one
/// proved itself — a passport by its first `Welcome`, a registration bundle
/// by the identity its registration stored (`adopt_issued`). An import that
/// ends fatally before that gives way to it again: an old invitation or a
/// revoked passport must not cost the identity that worked (review
/// 2026-09-27, C-2). A second import first puts the previous one back, so
/// the fallback is always the last identity that worked.
enum ImportTrial {
    /// A passport, saved with the set it replaced kept in `identity.old/`.
    Passport {
        previous: Identity,
        /// The settings it replaced, if it brought any.
        settings: Option<Box<Settings>>,
    },
    /// A registration bundle, not written at all: registering stores the
    /// identity it earns, and until then `identity/` holds `previous`, as
    /// in the first-start window.
    Registration { previous: Identity },
}

/// A renewal in flight: which name it asks for, and why it was sent.
#[derive(Debug, Clone)]
struct PendingRenew {
    cn: String,
    reason: RenewReason,
}

/// Why a certificate is being renewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenewReason {
    /// The user asked for a different name.
    UserRename,
    /// The certificate carries a name the server no longer stores — it was
    /// renamed on another device.
    CatchUp,
    /// The certificate is about to expire.
    Expiry,
}

/// Why a renewal could not be sent.
enum RenewSendError {
    /// Refused locally — a bad name or an unusable key. The connection is fine.
    Local(String),
    /// The control stream broke; the caller has to reconnect.
    Transport,
}

/// Builds a CSR for `cn` with the *existing* key and sends it as a renewal.
///
/// The key is reused deliberately: a rename must not orphan it, or a passport
/// exported before the rename would stop matching.
async fn send_renew(
    send: &mut quinn::SendStream,
    local: &mut LocalState,
    shared: &Arc<Shared>,
    cn: &str,
    reason: RenewReason,
) -> Result<(), RenewSendError> {
    // A new name has to meet today's rules; the stored one is carried over
    // as it is, or a name from before them could never be renewed.
    let checked = match reason {
        RenewReason::UserRename => icu_proto::validate_name(cn),
        RenewReason::CatchUp | RenewReason::Expiry => icu_proto::validate_stored_name(cn),
    };
    checked.map_err(|e| RenewSendError::Local(crate::problem::name_error_text(&e)))?;
    let key = local.identity.key_pair().map_err(|p| {
        tracing::warn!(error = ?p.technical, "own key unreadable");
        RenewSendError::Local(p.text)
    })?;
    let csr_der = crate::net::register::csr_for(cn, &key)
        .map_err(|e| RenewSendError::Local(e.problem().text))?;
    if conn::write_msg(send, &ControlMsg::RenewRequest { csr_der })
        .await
        .is_err()
    {
        return Err(RenewSendError::Transport);
    }
    local.set_pending_renew(
        Some(PendingRenew {
            cn: cn.to_string(),
            reason,
        }),
        shared,
    );
    Ok(())
}

/// Whether this client should renew now, and why. `stored` is the name the
/// server just reported in `Welcome`.
///
/// The CN is compared exactly: the server stores names verbatim, so `alice` →
/// `Alice` is a real rename this client has to pick up.
fn renewal_due(cert: &crate::conninfo::CertInfo, stored: &str) -> Option<RenewReason> {
    if cert.subject != stored {
        return Some(RenewReason::CatchUp);
    }
    // In the last third of the lifetime, and at least the last 30 days,
    // where the server opens the window (`docs/reference/protocol.md` §2.2 Nr. 9). Both day
    // counts are floored, which keeps this a little inside the server's
    // window rather than before it: 30 floored days are up to 31 left.
    if cert.expires_in_days < (cert.valid_days / 3).max(RENEW_BEFORE_EXPIRY_DAYS) {
        return Some(RenewReason::Expiry);
    }
    None
}

/// Whether an automatic renewal lost with its session (no answer) is tried
/// again on the next connect rather than a day later: it may never have
/// reached the server. A rename the user asked for is simply over — the
/// field unlocks; if the server did apply it, the next `Welcome` reports the
/// new name and the catch-up renewal picks it up.
fn retry_lost_renewal_at_once(reason: RenewReason) -> bool {
    reason != RenewReason::UserRename
}

/// Renew at the latest this many days before the certificate expires
/// (`docs/reference/client.md` §5).
const RENEW_BEFORE_EXPIRY_DAYS: i64 = 30;

/// How long an automatic renewal waits after the previous one, matching the
/// server's one per day.
const AUTO_RENEW_RETRY: Duration = Duration::from_secs(24 * 3600);

/// Sends the automatic renewal if one is due and none was tried within
/// [`AUTO_RENEW_RETRY`]: catch-up after a rename on another device, or the
/// last third of the certificate's lifetime. `stored` is the name the server holds. Silent:
/// no banner, no `name_error`, and `rename_pending` stays false — this is not
/// something the user asked for. `Err` only when the control stream broke.
async fn maybe_auto_renew(
    send: &mut quinn::SendStream,
    local: &mut LocalState,
    shared: &Arc<Shared>,
    stored: &str,
) -> Result<(), ()> {
    if local.pending_renew.is_some()
        || local
            .renew_attempted_at
            .is_some_and(|t| t.elapsed() < AUTO_RENEW_RETRY)
    {
        return Ok(());
    }
    let cert = crate::conninfo::CertInfo::from_der(&local.identity.leaf_der);
    let Some(reason) = renewal_due(&cert, stored) else {
        return Ok(());
    };
    local.renew_attempted_at = Some(Instant::now());
    match send_renew(send, local, shared, stored, reason).await {
        Ok(()) => Ok(()),
        Err(RenewSendError::Local(e)) => {
            tracing::warn!(error = %e, ?reason, "automatic renewal not possible");
            Ok(())
        }
        Err(RenewSendError::Transport) => Err(()),
    }
}

impl LocalState {
    fn current_audio_device(&self, shared: &Arc<Shared>) -> Option<String> {
        let current = blocking(|| lock(&self.backend).capture_level().ok().flatten())
            .filter(|c| c.valid())?;
        Some(crate::audio::calibration::device_channel_key(
            shared.state.borrow().backend,
            &current.device,
            lock(&self.backend).capture_channel_index(),
        ))
    }

    fn learn_audio(
        &mut self,
        shared: &Arc<Shared>,
        measured: Option<(icu_proto::AudioLevel, icu_proto::AudioWindow)>,
        enabled: bool,
        now: Instant,
    ) {
        let (identity, gain, peer, remote, safe) = {
            let t = lock(&shared.tuning);
            (
                t.capture_identity.clone(),
                t.software_gain_db,
                t.peer.offset_db,
                t.peer.correction(now),
                t.clip_until.is_none_or(|until| now >= until)
                    && t.overload_until.is_none_or(|until| now >= until)
                    && t.software_until.is_none_or(|until| now >= until),
            )
        };
        let Some(identity) = identity else {
            self.calibration_learner.reset();
            shared.state.send_modify(|s| {
                s.can_restore_audio = publish_audio_profile(&mut s.audio_health, None);
            });
            return;
        };
        let channel = lock(&self.backend).capture_channel_index();
        if channel.is_none() {
            self.calibration_learner.reset();
            return;
        }
        let key = crate::audio::calibration::device_channel_key(
            shared.state.borrow().backend,
            &identity.device,
            channel,
        );
        let unix = crate::net::conn::unix_now().max(0) as u64;
        let saved_at = self.calibration.profile(&key, unix).map(|p| p.saved_at);
        shared.state.send_modify(|s| {
            s.can_restore_audio = publish_audio_profile(&mut s.audio_health, saved_at);
        });
        if !enabled || !self.settings.audio.remember_good_levels {
            self.calibration_learner.reset();
            return;
        }
        let Some((level, window)) = measured else {
            return;
        };
        let health = shared.state.borrow().audio_health.clone();
        let good = safe
            && !identity.muted
            && level.valid()
            && level.near_full_permille == 0
            && remote.is_some_and(|r| {
                r.abs() <= icu_proto::audio_level::ACCEPT_CDB as f32 / 100.0
                    && (r - level.correction_cdb as f32 / 100.0).abs() <= 1.5
            })
            && health.limiter_reduction_db <= 1.0
            && health.limited_fraction <= 0.01;
        let system_delta = lock(&shared.system_tuner).delta_db();
        let profile = crate::audio::calibration::Profile {
            device: key,
            saved_at: unix,
            settings: crate::audio::calibration::ProcessingSettings::from(&self.settings.audio),
            capture_db: identity.db - system_delta,
            mic_gain_db: (gain + system_delta).clamp(-15.0, 15.0),
            peer_trim_db: peer,
        };
        if let Some(profile) = self
            .calibration_learner
            .observe_window(profile, good, window, now)
        {
            match self.calibration.save_profile(&self.paths, profile, unix) {
                Ok(()) => shared.state.send_modify(|s| {
                    s.can_restore_audio = true;
                    s.audio_health.profile_saved = true;
                    s.audio_health.profile_saved_at = Some(unix);
                }),
                Err(e) => tracing::warn!(error=%e,"cannot save last good audio calibration"),
            }
        }
    }

    /// Recomputes `UserFlags::VIEWING` (overlay on or main window visible)
    /// and `UserFlags::PTT` (the talk mode). Returns `true` when a bit
    /// changed and `SetState` is due.
    fn update_viewing(&mut self) -> bool {
        let viewing = self.settings.overlay.enabled || self.window_visible;
        let ptt = Mode::from(self.settings.audio.mode) == Mode::Ptt;
        let before = self.flags;
        self.flags = self
            .flags
            .with_bit(UserFlags::VIEWING, viewing)
            .with_bit(UserFlags::PTT, ptt);
        self.flags != before
    }

    /// The one writer of `pending_renew`. The window's "rename in progress"
    /// flag is derived from it here, so the two cannot disagree: it shows
    /// for a rename the user asked for, never for a background renewal.
    fn set_pending_renew(&mut self, pending: Option<PendingRenew>, shared: &Arc<Shared>) {
        let user_rename = pending
            .as_ref()
            .is_some_and(|p| p.reason == RenewReason::UserRename);
        self.pending_renew = pending;
        shared.state.send_if_modified(|s| {
            std::mem::replace(&mut s.rename_pending, user_rename) != user_rename
        });
    }

    /// Forgets the requests of a session that ended before their answers.
    /// A pending renewal locked the name field until restart ("a change is
    /// still running"), and a stale invite request routed the next
    /// unrelated `Error` into `invite_error`.
    fn forget_session(&mut self, shared: &Arc<Shared>) {
        if let Some(lost) = self.take_pending_renew(shared) {
            if retry_lost_renewal_at_once(lost.reason) {
                self.renew_attempted_at = None;
            }
        }
        self.invite_pending = false;
    }

    /// Takes the renewal in flight, clearing the window's flag with it.
    fn take_pending_renew(&mut self, shared: &Arc<Shared>) -> Option<PendingRenew> {
        let pending = self.pending_renew.take();
        shared
            .state
            .send_if_modified(|s| std::mem::replace(&mut s.rename_pending, false));
        pending
    }

    /// Commands that do not need a connection. Returns `true` if `SetState`
    /// should be sent.
    fn handle_local(&mut self, cmd: Command, shared: &Arc<Shared>) -> bool {
        let mut set_state = false;
        match cmd {
            Command::SetPrivacy(active) => {
                // The connection lock is the audio send boundary. Stored user
                // mute/camera choices remain untouched, so leaving restores them.
                let _conn = lock(&shared.conn);
                if shared.state.borrow().privacy_active != active {
                    let epoch = shared.dsp.privacy_epoch.fetch_add(1, Ordering::SeqCst) + 1;
                    shared.state.send_modify(|s| {
                        s.privacy_active = active;
                        s.privacy_epoch = epoch;
                        s.sending = false;
                    });
                    shared.keyframe_request.store(true, Ordering::Relaxed);
                    set_state = true;
                }
            }
            Command::SetMuted(false) | Command::ToggleMute
                if shared.state.borrow().privacy_active => {}
            Command::SetCamera(_) | Command::ToggleCamera
                if shared.state.borrow().privacy_active => {}
            Command::SetSetupActive(active) => {
                shared.dsp.setup_active.store(active, Ordering::Relaxed);
                shared.state.send_modify(|s| s.setup_active = active);
            }
            // An enforced deafen keeps the microphone off as well: deafen
            // implies mute, whoever set it.
            Command::SetMuted(false)
                if {
                    let s = shared.state.borrow();
                    s.restricted_muted || s.restricted_deafened
                } => {}
            Command::SetDeafened(false) if shared.state.borrow().restricted_deafened => {}
            Command::SetMuted(m) => {
                let deafened = self.flags.deafened();
                if deafened && m {
                    // Stays deafened; undeafening keeps the microphone off.
                    self.muted_before_deafen = true;
                }
                // Deafen implies mute, so unmuting while deafened undeafens
                // as well (as in Mumble), instead of sending into a room one
                // cannot hear.
                self.apply_mute_deafen(m, deafened && m, shared);
                self.cue(shared, if m { Cue::Mute } else { Cue::Unmute });
                set_state = true;
            }
            Command::ToggleMute => {
                let m = !self.flags.muted();
                return self.handle_local(Command::SetMuted(m), shared);
            }
            Command::SetDeafened(d) => {
                let was = self.flags.deafened();
                if d && !was {
                    self.muted_before_deafen = self.flags.muted();
                }
                // Undeafening restores the mute from before deafening
                // (`docs/reference/features.md`), unless an admin enforces it.
                let muted = if d {
                    true
                } else if was {
                    self.muted_before_deafen || shared.state.borrow().restricted_muted
                } else {
                    self.flags.muted()
                };
                self.apply_mute_deafen(muted, d, shared);
                self.cue(shared, if d { Cue::Mute } else { Cue::Unmute });
                set_state = true;
            }
            Command::ToggleDeafen => {
                let d = !self.flags.deafened();
                return self.handle_local(Command::SetDeafened(d), shared);
            }
            Command::SetMode(mode) => {
                self.settings.audio.mode = mode.into();
                set_state = self.commit_settings(shared, false);
            }
            Command::ToggleMode => {
                let mode = match Mode::from(self.settings.audio.mode) {
                    Mode::Vad => Mode::Ptt,
                    Mode::Ptt => Mode::Vad,
                };
                return self.handle_local(Command::SetMode(mode), shared);
            }
            Command::Ptt(down) => {
                tracing::debug!(down, "ptt");
                shared.dsp.ptt_down.store(down, Ordering::Relaxed);
                shared.state.send_modify(|s| s.ptt_down = down);
            }
            Command::SetVolume(user_id, percent) => {
                let percent = self.settings.set_volume(user_id, percent);
                for sp in lock(&shared.speakers).values_mut() {
                    if sp.user_id == user_id {
                        sp.volume = percent;
                    }
                }
                shared.state.send_modify(|s| {
                    for p in s.peers.iter_mut() {
                        if p.info.user_id == user_id {
                            p.volume = percent;
                        }
                    }
                });
                set_state = self.commit_settings(shared, false);
            }
            Command::RestoreGoodAudio => {
                let device = self.current_audio_device(shared);
                if let Some(profile) = device
                    .and_then(|key| {
                        self.calibration
                            .profile(&key, crate::net::conn::unix_now().max(0) as u64)
                    })
                    .cloned()
                {
                    let mut restored = self.settings.clone();
                    profile.settings.restore(&mut restored.audio);
                    restore_capture(shared, &self.backend);
                    set_state = self.replace_settings(restored, shared);
                    shared.dsp.reinit_aec.store(true, Ordering::Relaxed);
                }
            }
            Command::ForgetGoodAudio => {
                let device = self.current_audio_device(shared);
                let mut failed = false;
                if let Some(key) = device {
                    if let Err(e) = blocking(|| {
                        self.calibration.forget(
                            &self.paths,
                            &key,
                            crate::net::conn::unix_now().max(0) as u64,
                        )
                    }) {
                        tracing::warn!(error=%e,"cannot forget audio calibration");
                        failed = true;
                        shared.state.send_modify(|s| {
                            s.banner = Some(crate::problem::storage("Das Audio-Profil", &e))
                        });
                    }
                }
                if !failed {
                    self.calibration_learner.reset();
                    restore_capture(shared, &self.backend);
                    shared.dsp.reinit_aec.store(true, Ordering::Relaxed);
                    shared.state.send_modify(|s| {
                        s.can_restore_audio = false;
                        s.audio_health.profile_saved = false;
                        s.audio_health.profile_saved_at = None;
                    });
                }
            }
            Command::UpdateSettings(new) => {
                set_state = self.replace_settings(*new, shared);
            }
            Command::SetMicTest(on) => {
                shared.dsp.mic_test.store(on, Ordering::Relaxed);
                shared.state.send_modify(|s| s.mic_test = on);
            }
            Command::SetCamera(on) => {
                self.settings.camera.enabled = on;
                set_state = self.commit_settings(shared, false);
            }
            Command::SetOverlay(on) => {
                self.settings.overlay.enabled = on;
                set_state = self.commit_settings(shared, false);
            }
            Command::SetWindowVisible(visible) => {
                self.window_visible = visible;
                set_state = self.update_viewing();
            }
            Command::DismissBanner => {
                shared.state.send_modify(|s| {
                    s.banner = None;
                    s.name_error = None;
                    s.invite_error = None;
                });
            }
            Command::ForceKeyframe => {
                shared.keyframe_request.store(true, Ordering::Relaxed);
            }
            Command::HotkeyBackend(name) => {
                shared.state.send_modify(|s| s.hotkey_backend = name);
            }
            Command::ReopenStreams => {
                tracing::info!("reopening audio streams after a stream error");
                self.reopen_streams(shared);
            }
            Command::CaptureChannelChanged(channel) => {
                // Reopening would reset the automatic selector to channel zero
                // and repeat forever when the live mic is on channel two.
                self.calibration_learner.reset();
                restore_capture(shared, &self.backend);
                shared.dsp.reinit_aec.store(true, Ordering::Relaxed);
                shared
                    .state
                    .send_modify(|s| s.audio_health.capture_channel = Some(channel));
            }
            Command::RetryStreams => self.retry_streams(shared),
            Command::DevicesChanged => {
                // A saved device came back or went away: follow it. Anything
                // else in the list changing leaves the running streams alone
                // and tries the ones that did not open — the device they
                // were missing may be the one that came.
                let want = wanted_devices(&self.settings, &lock(&self.backend).devices());
                let active = {
                    let s = shared.state.borrow();
                    (s.active_input.clone(), s.active_output.clone())
                };
                if want != active {
                    tracing::info!(?want, ?active, "audio devices changed, reopening");
                    self.reopen_streams(shared);
                } else {
                    self.retry_streams(shared);
                }
            }
            Command::RequestRename(_) => {
                shared.state.send_modify(|s| {
                    s.name_error = Some(
                        "Nicht mit dem Server verbunden – der Name lässt sich erst ändern, \
                             wenn die Verbindung steht."
                            .into(),
                    )
                });
            }
            Command::ReplaceIdentity(identity, settings) => {
                // Taken into use at the next connect; the callers reconnect
                // right away when it was stored.
                self.replace_identity(*identity, settings.map(|s| *s), shared);
            }
            Command::CreateInvite { .. }
            | Command::VideoDisplayed(_)
            | Command::SetUserAdmin { .. }
            | Command::SetUserDisabled { .. }
            | Command::RevokeUser { .. }
            | Command::RequestPeerCertificate(_)
            | Command::RequestUserList
            | Command::KickUser { .. }
            | Command::SetUserRestriction { .. }
            | Command::Reconnect
            | Command::Quit => {}
            Command::RetryHotkeys => {
                shared
                    .state
                    .send_modify(|s| s.hotkey_retries = s.hotkey_retries.wrapping_add(1));
            }
            Command::ToggleCamera => {
                let on = !self.flags.camera_on();
                return self.handle_local(Command::SetCamera(on), shared);
            }
            Command::CameraFailed(problem) => {
                shared.state.send_modify(|s| s.banner = Some(problem));
                if self.flags.camera_on() {
                    return self.handle_local(Command::SetCamera(false), shared);
                }
            }
        }
        set_state
    }

    /// Takes mute and deafen into the flags, the send gate, the mixer and the
    /// window. Deafen implies mute. The user's own mute is kept for the next
    /// start (`AudioSettings::muted`).
    fn apply_mute_deafen(&mut self, muted: bool, deafened: bool, shared: &Arc<Shared>) {
        let muted = muted || deafened;
        self.flags = self
            .flags
            .with_bit(UserFlags::MUTED, muted)
            .with_bit(UserFlags::DEAFENED, deafened);
        shared.dsp.muted.store(muted, Ordering::Relaxed);
        shared.deafened.store(deafened, Ordering::Relaxed);
        let own = self.own_mute();
        let kept = (self.settings.audio.muted != own).then(|| {
            self.settings.audio.muted = own;
            let _ = self.settings_tx.send(self.settings.clone());
            self.settings.clone()
        });
        shared.state.send_modify(|s| {
            s.muted = muted;
            s.deafened = deafened;
            if let Some(settings) = kept {
                s.settings = settings;
            }
        });
    }

    /// Effective wire mute includes privacy without changing the saved choice.
    fn wire_flags(&self, shared: &Shared) -> UserFlags {
        self.flags.with_bit(
            UserFlags::MUTED,
            self.flags.muted() || shared.state.borrow().privacy_active,
        )
    }

    /// The mute the next start begins with: the current one — or, while
    /// deafened, the one from before deafening, since a restart undeafens
    /// and undeafening restores that one.
    fn own_mute(&self) -> bool {
        if self.flags.deafened() {
            self.muted_before_deafen
        } else {
            self.flags.muted()
        }
    }

    /// Takes a changed `self.settings` into effect. Everything that follows
    /// the settings — the DSP atomics, the master gain, the streams (when
    /// `reopen` says a device or the AEC switch changed), the published
    /// state and the file (debounced, `settings_writer`) — is re-derived
    /// here, in one place, so none of them can drift from the settings.
    /// Returns whether `SetState` is due: the camera switch and the overlay
    /// (through `VIEWING`) are part of the advertised flags.
    fn commit_settings(&mut self, shared: &Arc<Shared>, reopen: bool) -> bool {
        // The mute is the engine's: a settings window's copy taken before a
        // mute hotkey, or a passport's, must not undo it on disk.
        self.settings.audio.muted = self.own_mute();
        if self.settings.overlay.frame_color != self.corona_seen {
            self.corona_seen = self.settings.overlay.frame_color;
            self.corona_pending = Some(Instant::now());
        }
        shared.dsp.apply_settings(&self.settings);
        shared
            .output_gain
            .store(mixer::volume_gain(self.settings.audio.output_volume));
        shared
            .normalize_incoming
            .store(self.settings.audio.normalize_incoming, Ordering::Relaxed);
        if reopen {
            self.reopen_streams(shared);
        }
        if shared.state.borrow().privacy_active {
            self.settings.camera.enabled = self.flags.camera_on();
        }
        let camera_changed = self.flags.camera_on() != self.settings.camera.enabled;
        self.flags = self
            .flags
            .with_bit(UserFlags::CAMERA_ON, self.settings.camera.enabled);
        let settings = self.settings.clone();
        shared.state.send_modify(|s| {
            s.camera_on = settings.camera.enabled;
            s.settings = settings;
        });
        let _ = self.settings_tx.send(self.settings.clone());
        self.update_viewing() || camera_changed
    }

    /// Takes the own corona color the server has (`None`: the default) into
    /// the settings, without counting it as a pick to send back.
    fn adopt_corona(&mut self, color: Option<Rgb>, shared: &Arc<Shared>) {
        let color = color.unwrap_or(crate::config::DEFAULT_FRAME_COLOR);
        if color != self.settings.overlay.frame_color {
            self.settings.overlay.frame_color = color;
            self.corona_seen = color;
            self.commit_settings(shared, false);
        }
    }

    /// After `Welcome`: the server's color wins, unless one was picked here
    /// that it has not heard of yet. A server without one gets this
    /// client's, so a color picked while it was still local carries over.
    fn sync_corona(&mut self, stored: Option<Rgb>, shared: &Arc<Shared>) {
        if self.corona_pending.is_some() {
            return;
        }
        if stored.is_some() {
            self.adopt_corona(stored, shared);
        } else if corona_wire(self.settings.overlay.frame_color).is_some() {
            self.corona_pending = Some(Instant::now() - CORONA_DEBOUNCE);
        }
    }

    /// Replaces the settings and takes them into effect. The streams are
    /// opened against the devices and the AEC switch; a change there needs
    /// them opened anew. Returns whether `SetState` is due.
    fn replace_settings(&mut self, new: Settings, shared: &Arc<Shared>) -> bool {
        let old = &self.settings.audio;
        let reopen = new.audio.input_channel != old.input_channel
            || new.audio.input_device != old.input_device
            || new.audio.output_device != old.output_device
            || new.audio.aec != old.aec;
        if crate::audio::calibration::ProcessingSettings::from(&new.audio)
            != crate::audio::calibration::ProcessingSettings::from(old)
        {
            self.calibration_learner.reset();
            lock(&shared.system_tuner).invalidate_evidence();
            let mut t = lock(&shared.tuning);
            t.capture = Default::default();
            t.peer = Default::default();
            t.peer.set_epoch(shared.gain_epoch.load(Ordering::Relaxed));
            t.force_epoch = true;
            t.outgoing = Default::default();
        }
        self.settings = new;
        self.commit_settings(shared, reopen)
    }

    /// Takes over an imported identity (passport import): stored first and
    /// taken into use only once it is, like a renewed certificate
    /// (`apply_renewed_leaf`). Connecting with one that is not on disk would
    /// leave the next start on the old one — revoked, if the imported
    /// certificate's first login retired it. A failed write keeps the
    /// identity in force and says so, and leaves the settings alone too.
    /// Settings that came with it replace this machine's portable ones
    /// (`Settings::take_portable`). The identity it replaces stays until the
    /// new one proved itself ([`ImportTrial`]). A registration bundle is not
    /// stored at all: it is only a way to the identity its registration
    /// stores, and brings no settings. Returns whether it was taken over.
    fn replace_identity(
        &mut self,
        identity: Identity,
        settings: Option<Settings>,
        shared: &Arc<Shared>,
    ) -> bool {
        if !self.abandon_trial(shared) {
            return false;
        }
        let previous = self.identity.clone();
        if identity.is_registration() {
            self.adopt(identity, shared);
            self.trial = Some(ImportTrial::Registration { previous });
            return true;
        }
        if let Err(e) = store_identity(&identity, &self.paths, shared, true) {
            tracing::warn!(error = %e, "cannot store the imported identity, keeping the old one");
            shared.state.send_modify(|s| {
                s.banner = Some(crate::problem::storage("Der Passport", &e));
            });
            return false;
        }
        self.adopt(identity, shared);
        let replaced = settings.map(|incoming| {
            let before = self.settings.clone();
            let mut taken = before.clone();
            taken.take_portable(incoming);
            // A `SetState` for a changed overlay switch is moot: the callers
            // reconnect, and the new `Hello` carries the flags.
            self.replace_settings(taken, shared);
            tracing::info!("settings taken over from the passport");
            Box::new(before)
        });
        self.trial = Some(ImportTrial::Passport {
            previous,
            settings: replaced,
        });
        true
    }

    /// The first `Welcome` with an imported passport: it works, the set it
    /// replaced can go. A registration bundle proves itself later, by what
    /// its registration stores (`adopt_issued`).
    fn welcomed(&mut self) {
        if matches!(self.trial, Some(ImportTrial::Passport { .. })) {
            self.trial = None;
            if let Err(e) = Identity::drop_previous(&self.paths) {
                tracing::warn!(error = %e, "cannot remove identity.old/");
            }
        }
    }

    /// An import on trial ended fatally, `why`: puts the identity it
    /// replaced back — on disk, in force and with its settings — and says
    /// so. Returns whether it is back; when it cannot be written back, the
    /// fatal end stands (reconnecting would only fail the same way).
    fn undo_import(&mut self, why: &Problem, shared: &Arc<Shared>) -> bool {
        let Some(trial) = self.trial.take() else {
            return false;
        };
        let what = match &trial {
            ImportTrial::Passport { .. } => "Der eingespielte Passport",
            ImportTrial::Registration { .. } => "Die eingespielte Einladung",
        };
        tracing::warn!(problem = %why.full(), "import failed, back to the previous identity");
        if !self.restore(trial, shared) {
            return false;
        }
        let text = format!(
            "{what} ließ sich nicht verwenden: {} Die bisherigen Zugangsdaten gelten weiter.",
            why.text
        );
        let problem = match &why.technical {
            Some(t) => Problem::new(text, t.clone()),
            None => Problem::plain(text),
        };
        shared.state.send_modify(|s| s.banner = Some(problem));
        true
    }

    /// Before a second import: puts the identity the first one replaced
    /// back, so the new trial keeps the last identity that worked. `false`
    /// when that cannot be written back — the import is refused then, or
    /// the kept set would be overwritten with an untested one.
    fn abandon_trial(&mut self, shared: &Arc<Shared>) -> bool {
        match self.trial.take() {
            None => true,
            Some(trial) => self.restore(trial, shared),
        }
    }

    /// Takes the identity `trial` replaced back into use. Returns `false`
    /// when a passport's kept set could not be put back on disk; the trial
    /// then stays, and the window says so.
    fn restore(&mut self, trial: ImportTrial, shared: &Arc<Shared>) -> bool {
        match trial {
            ImportTrial::Registration { previous } => self.adopt(previous, shared),
            ImportTrial::Passport { previous, settings } => {
                if let Err(e) = Identity::restore_previous(&self.paths) {
                    tracing::warn!(error = %e, "cannot put the previous identity back");
                    shared.state.send_modify(|s| {
                        s.banner = Some(crate::problem::storage("Die bisherige Identität", &e));
                    });
                    self.trial = Some(ImportTrial::Passport { previous, settings });
                    return false;
                }
                self.adopt(previous, shared);
                if let Some(settings) = settings {
                    self.replace_settings(*settings, shared);
                }
            }
        }
        true
    }

    /// Takes an identity that is already on disk into use: the next connect
    /// runs with it, and nothing of the old one's renewal state carries over.
    fn adopt(&mut self, identity: Identity, shared: &Arc<Shared>) {
        self.identity = identity;
        self.set_pending_renew(None, shared);
        self.renew_attempted_at = None;
        self.renew_only = false;
        self.name = crate::conninfo::CertInfo::from_der(&self.identity.leaf_der).subject;
        let server = format!(
            "{}:{}",
            self.identity.server.host, self.identity.server.port
        );
        let server_name = self.identity.server.server_name.clone();
        shared.state.send_modify(|s| {
            s.server = server;
            s.server_name = server_name;
            s.name_error = None;
        });
        tracing::info!(name = %self.name, "identity replaced");
    }

    /// Takes a freshly issued leaf into use.
    ///
    /// Checked before it is written: a leaf that does not match the request or
    /// the local key would make the next `client_config` fail, and the only way
    /// back from that is a passport from another device.
    fn apply_renewed_leaf(&mut self, leaf_der: Vec<u8>, shared: &Arc<Shared>) {
        let Some(pending) = self.take_pending_renew(shared) else {
            tracing::warn!("unsolicited RenewResponse");
            return;
        };
        let info = match crate::conninfo::check_issued_leaf(
            &leaf_der,
            &pending.cn,
            &self.identity.key_der,
            &self.identity.ca_der,
        ) {
            Ok(info) => info,
            Err(detail) => {
                tracing::warn!(%detail, "renewal rejected locally, keeping the old certificate");
                if pending.reason == RenewReason::UserRename {
                    shared.state.send_modify(|s| {
                        s.name_error = Some(
                            "Der Server hat die Namensänderung falsch beantwortet. Der \
                             bisherige Name bleibt."
                                .into(),
                        );
                    });
                }
                return;
            }
        };

        // A server that moved says so in the certificates it issues; the next
        // reconnect goes to the new address.
        let mut renewed = self.identity.clone();
        renewed.server.update_from_certs(&leaf_der, &renewed.ca_der);
        renewed.leaf_der = leaf_der;
        renewed.server.name = pending.cn.clone();
        // Adopted only once stored: its first login revokes the old one
        // (`docs/reference/protocol.md` §2.2 Nr. 10), so reconnecting with a leaf that is
        // not on disk would leave the next start with a revoked certificate.
        // The old one stays valid until then; the next renewal tries again.
        if let Err(e) = store_identity(&renewed, &self.paths, shared, false) {
            tracing::warn!(error = %e, "cannot store the renewed certificate, keeping the old one");
            shared.state.send_modify(|s| {
                s.banner = Some(crate::problem::storage("Das neue Zertifikat", &e));
            });
            return;
        }
        self.identity = renewed;
        self.name = pending.cn.clone();
        tracing::info!(name = %pending.cn, reason = ?pending.reason, "certificate renewed");
        shared.state.send_modify(|s| {
            s.name_error = None;
            // Otherwise the Verbindung window keeps showing the old subject
            // and fingerprint for the rest of the session.
            if let Some(old) = s.conn_info.as_ref() {
                let mut fresh = (**old).clone();
                fresh.client_cert = info.clone();
                let crl = std::fs::read(crate::crl::path(&self.paths)).ok();
                fresh
                    .client_cert
                    .check_trust(&self.identity.ca_der, crl.as_deref());
                s.conn_info = Some(Arc::new(fresh));
            }
        });
    }

    fn reopen_streams(&mut self, shared: &Arc<Shared>) {
        blocking(|| {
            restore_capture(shared, &self.backend);
            let mut backend = lock(&self.backend);
            shared.reference_open.store(false, Ordering::Relaxed);
            backend.close(StreamKind::Capture);
            backend.close(StreamKind::Reference);
            backend.close(StreamKind::Playback);
            shared.dsp.reinit_aec.store(true, Ordering::Relaxed);
            open_streams(&mut **backend, &self.settings, shared);
            let devices = backend.devices();
            shared.state.send_modify(|s| s.devices = devices);
        });
    }

    /// [`retry_failed_streams`], if anything is missing.
    fn retry_streams(&mut self, shared: &Arc<Shared>) {
        if shared.failed_streams.load(Ordering::Relaxed) == 0 {
            return;
        }
        blocking(|| {
            let mut backend = lock(&self.backend);
            retry_failed_streams(&mut **backend, &self.settings, shared);
        });
    }

    fn cue(&self, shared: &Arc<Shared>, cue: Cue) {
        let audio = &self.settings.audio;
        let enabled = match cue {
            Cue::Join | Cue::Leave => audio.cue_join,
            Cue::TxStart | Cue::TxStop => audio.cue_tx,
            Cue::Mute | Cue::Unmute => audio.cue_mute,
        };
        if enabled {
            let samples = crate::cues::render(cue);
            shared.cues.lock().unwrap().push(cue, &samples);
        }
    }
}

pub use crate::cues::Cue;

async fn run_session(
    cfg: &EngineConfig,
    shared: &Arc<Shared>,
    local: &mut LocalState,
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
    addr: SocketAddr,
    was_connected: &mut bool,
) -> SessionEnd {
    let crl = crate::crl::load(&local.paths, &local.identity.ca_der);
    let (client_config, cert_failure) =
        match conn::client_config_with_crl(&local.identity, crl.as_deref()) {
            Ok(c) => c,
            Err(e) => return SessionEnd::Fatal(e.problem()),
        };
    let endpoint = match crate::net::bind_endpoint(addr, &local.identity.com_key) {
        Ok(e) => e,
        Err(e) => return SessionEnd::Retry(e.problem()),
    };
    let sni = crate::net::server_name_for(&local.identity.server.server_name);
    let hello_flags = local.wire_flags(shared);
    let hello_name = local.name.clone();
    let connect = conn::connect(
        &endpoint,
        client_config,
        addr,
        &sni,
        &cfg.client_string,
        hello_flags,
        &hello_name,
        &cert_failure,
    );
    let session = tokio::select! {
        r = connect => r,
        // Commands keep working while connecting; Quit aborts, and a new
        // identity abandons the attempt made with the old one — with a
        // close, or the server holds the half-open connection until its
        // idle timeout.
        end = drain_until_quit(cmd_rx, local, shared) => {
            endpoint.close(quinn::VarInt::from_u32(codes::NO_ERROR), b"");
            tokio::time::timeout(Duration::from_millis(100), endpoint.wait_idle())
                .await
                .ok();
            return end;
        }
    };
    let session = match session {
        Ok(s) => s,
        Err(e) => return e.into(),
    };
    let conn::Session {
        conn: connection,
        mut send,
        mut recv,
        welcome,
    } = session;
    if !welcome.registration {
        local.welcomed();
    }

    // A registration bundle imported into a running client: swap it for a
    // certificate of our own under the placeholder name, which the user can
    // change like any other name (`docs/reference/protocol.md` §2.3). The first-run window
    // does this itself, with the name the user typed.
    if welcome.registration {
        shared.state.send_modify(|s| {
            s.conn = ConnStatus::Connecting;
            s.notice = Some("Anmeldung läuft …".into());
        });
        let name = welcome.you.name.clone();
        let bundle = local.identity.clone();
        let registered = tokio::select! {
            r = crate::net::register::complete(&mut send, &mut recv, &bundle, &name) => r,
            end = drain_until_quit(cmd_rx, local, shared) => return end,
        };
        // Stored first, then closed: the clean close is the receipt that
        // makes the server revoke the bundle (`docs/reference/protocol.md` §2.3).
        return match registered {
            // Saved before it is taken into use: an identity that only lives
            // in memory would consume the bundle on the next login and be
            // gone after a restart.
            Ok(identity) => match adopt_issued(
                local,
                shared,
                &connection,
                identity,
                crate::net::register::receipt(true),
                crate::net::register::receipt(false),
                "Die Anmeldung",
            ) {
                Ok(()) => {
                    SessionEnd::Retry(Problem::plain("Anmeldung gespeichert – verbinde neu …"))
                }
                Err(mut p) => {
                    p.text.push_str(" Die Einladung bleibt gültig.");
                    SessionEnd::Fatal(p)
                }
            },
            // No answer: the bundle is still valid, so try again — unless the
            // server closed with a fatal code (replaced by another device,
            // revoked), where retrying would only burn the registration budget.
            Err(e @ crate::net::NetError::Connect(_)) => {
                if let Some(end) = closed_end(&connection) {
                    return end;
                }
                connection.close(crate::net::register::receipt(false), b"");
                SessionEnd::Retry(Problem::new(
                    "Die Anmeldung wurde unterbrochen. ICU versucht es gleich noch einmal.",
                    e.to_string(),
                ))
            }
            Err(e) => {
                connection.close(crate::net::register::receipt(false), b"");
                let mut p = e.problem();
                p.text = format!("Anmeldung fehlgeschlagen: {}", p.text);
                SessionEnd::Fatal(p)
            }
        };
    }

    // An expired certificate still gets through the handshake, into a
    // renew-only session (`docs/reference/protocol.md` §2.3): renew it under the stored name
    // with the same key, store it, reconnect with it. `renew_only` covers a
    // server whose clock already disagrees with ours.
    let expired = conn::leaf_expired(&local.identity.leaf_der, conn::unix_now());
    if expired || std::mem::take(&mut local.renew_only) {
        shared.state.send_modify(|s| {
            s.conn = ConnStatus::Connecting;
            s.notice = Some("Zertifikat abgelaufen – wird erneuert …".into());
        });
        let name = welcome.you.name.clone();
        let identity = local.identity.clone();
        let renewed = tokio::select! {
            r = crate::net::register::renew_expired(&mut send, &mut recv, &identity, &name) => r,
            end = drain_until_quit(cmd_rx, local, shared) => return end,
        };
        return match renewed {
            Ok(identity) => match adopt_issued(
                local,
                shared,
                &connection,
                identity,
                quinn::VarInt::from_u32(codes::NO_ERROR),
                quinn::VarInt::from_u32(codes::SHUTDOWN),
                "Das neue Zertifikat",
            ) {
                Ok(()) => {
                    tracing::info!(name = %name, "expired certificate renewed");
                    SessionEnd::Retry(Problem::plain("Zertifikat erneuert – verbinde neu …"))
                }
                Err(p) => SessionEnd::Fatal(p),
            },
            Err(crate::net::NetError::Rejected { code, detail }) if codes::is_fatal(code) => {
                SessionEnd::Fatal(crate::problem::from_code(code, &detail))
            }
            Err(e) => {
                if let Some(end) = closed_end(&connection) {
                    return end;
                }
                connection.close(quinn::VarInt::from_u32(codes::SHUTDOWN), b"");
                // Still expired: the next connect lands here again, after
                // the backoff.
                SessionEnd::Retry(Problem::new(
                    "Das abgelaufene Zertifikat konnte noch nicht erneuert werden. ICU versucht \
                     es gleich noch einmal.",
                    e.to_string(),
                ))
            }
        };
    }

    // Connected: install the peer state, clear everything session scoped.
    // The speakers are emptied here and not only at session end: the previous
    // session's datagram reader may have been mid-insert when it was aborted.
    // Likewise the video: the previous connection's decode thread may still
    // deliver queued frames, which the new generation drops.
    drop_speakers(shared);
    let video_generation = lock(&shared.video).reset();
    forget_network_feedback(shared);
    let peers: Vec<Peer> = welcome
        .users
        .iter()
        .map(|u| admit(shared, &local.settings, u))
        .collect();
    let update_required = match (
        icu_proto::Version::parse(&welcome.min_client),
        icu_proto::Version::parse(env!("CARGO_PKG_VERSION")),
    ) {
        (Ok(min), Ok(mine)) if min > mine => Some(welcome.min_client.clone()),
        _ => None,
    };
    local.name = welcome.you.name.clone();
    local.video_viewers.clear();
    local.last_display_report.clear();
    {
        let me = welcome.you.clone();
        tracing::info!(peers = peers.len(), "welcome: initial peer list");
        // Parsed here, once: three X.509 certificates is far too much work
        // for a render thread that repaints at 30 fps.
        // Behind an `Arc`: `AppState` is cloned on every change — the
        // speaking tick alone fires ten times a second — and this is a
        // couple of dozen strings that are written exactly once.
        let mut info = crate::conninfo::ConnInfo::collect(
            &connection,
            &welcome,
            &local.identity,
            &cfg.client_string,
        );
        let crl = std::fs::read(crate::crl::path(&local.paths)).ok();
        info.check_trust(&local.identity.ca_der, crl.as_deref());
        let info = Arc::new(info);
        shared.state.send_modify(|s| {
            s.conn = ConnStatus::Connected;
            s.me = Some(me);
            s.peers = peers;
            s.video_viewers.clear();
            shared
                .dsp
                .audience
                .store(!s.peers.is_empty(), Ordering::Relaxed);
            s.update_required = update_required;
            s.is_admin = welcome.is_admin;
            s.conn_info = Some(info);
        });
    }
    *lock(&shared.conn) = Some(connection.clone());
    lock(&shared.tuning).new_connection(shared.gain_epoch.load(Ordering::Relaxed));
    shared.feedback_capable.store(
        welcome.caps & icu_proto::CAP_AUDIO_FEEDBACK != 0,
        Ordering::Relaxed,
    );
    shared.dsp.online.store(true, Ordering::Relaxed);
    *was_connected = true;
    tracing::info!(
        session = welcome.you.session,
        user_id = welcome.you.user_id,
        "connected"
    );

    // Reader tasks, aborted with the endpoint closed however this returns.
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<Result<ControlMsg, String>>();
    let session_tasks = SessionTasks {
        tasks: vec![
            tokio::spawn(control_reader(recv, ctrl_tx)),
            tokio::spawn(datagram_reader(connection.clone(), shared.clone())),
            tokio::spawn(crate::video::accept_loop(
                connection.clone(),
                SessionVideo {
                    shared: shared.clone(),
                    generation: video_generation,
                },
            )),
        ],
        endpoint: endpoint.clone(),
    };
    *lock(&shared.video_tx) = Some(Arc::new(crate::video::Sender::new(connection.clone())));

    // Flags may have changed while the handshake was in flight (a command
    // handled during `connect`); the server only knows `Hello.initial`.
    // Compare only the client-settable bits: `welcome.you.flags` may carry
    // server-authoritative `SPEAKING`/`ADMIN` bits `local.flags` never has,
    // which would otherwise trigger a spurious `SetState` on every connect.
    let mut sent_flags = welcome.you.flags;
    let mut set_state_budget = SetStateBudget::new(Instant::now());
    if flush_state(
        &mut send,
        local.wire_flags(shared),
        &mut sent_flags,
        &mut set_state_budget,
    )
    .await
    .is_err()
    {
        return send_failed(&connection);
    }

    local.sync_corona(welcome.you.corona_color, shared);

    // Pick up a rename that happened on another device, or renew a
    // certificate that is about to expire (`docs/reference/protocol.md` §2.2); again every
    // hour, since a session can outlive the window.
    if maybe_auto_renew(&mut send, local, shared, &welcome.you.name)
        .await
        .is_err()
    {
        return send_failed(&connection);
    }
    let mut renew_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(3600),
        Duration::from_secs(3600),
    );
    renew_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut ping = tokio::time::interval(Duration::from_secs(5));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tuning_tick = tokio::time::interval(Duration::from_secs(1));
    tuning_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut loss_tick = tokio::time::interval(Duration::from_secs(2));
    loss_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut video_tick = tokio::time::interval(Duration::from_millis(200));
    video_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Network-condition sampling (`crate::netcond`) and the quality-tier
    // state machine (`crate::quality`) share one cadence: both are cheap to
    // evaluate and the controller's own hysteresis, not the tick rate,
    // governs how fast the tier actually moves.
    let net_tick_period = Duration::from_secs(2);
    let mut net_tick = tokio::time::interval(net_tick_period);
    net_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut quality = QualityController::new(Instant::now());
    let mut pings: HashMap<u64, Instant> = HashMap::new();
    let mut next_nonce: u64 = 1;
    // Previous tick's cumulative byte counters, so `net_tick` can turn them
    // into rates. quinn only ever reports totals. The `DebugStats` counters
    // are process-global and survive a reconnect, so they are seeded from
    // where they stand now — starting from zero would bill this session's
    // first tick for everything the previous one sent.
    let mut prev_bytes = ByteMarks {
        udp_tx: 0,
        udp_rx: 0,
        audio_tx: DebugStats::get(&shared.stats.audio_bytes_sent),
        audio_rx: DebugStats::get(&shared.stats.audio_bytes_received),
        video_tx: DebugStats::get(&shared.stats.video_bytes_sent),
        video_rx: DebugStats::get(&shared.stats.video_bytes_received),
    };
    // `net_tick` uses `MissedTickBehavior::Delay`, so a tick can slip under
    // load; the rate must divide by what actually elapsed, not by the
    // nominal period.
    let mut prev_tick = Instant::now();

    let end = loop {
        // A flag change the budget could not send yet goes out as soon as the
        // next token is there.
        let state_due = (!same_settable(local.wire_flags(shared), sent_flags))
            .then(|| set_state_budget.ready_at(Instant::now()));
        let corona_due = local.corona_pending.map(|picked| picked + CORONA_DEBOUNCE);
        tokio::select! {
            msg = ctrl_rx.recv() => {
                let msg = match msg {
                    Some(Ok(msg)) => msg,
                    Some(Err(detail)) => break protocol_violation(&connection, detail),
                    None => {
                        // FIN on the control stream can beat the QUIC close carrying
                        // SHUTDOWN. Give that reason a bounded chance to arrive.
                        break match closed_end(&connection) {
                            Some(end) => end,
                            None => tokio::time::timeout(Duration::from_millis(100), connection.closed())
                                .await.map(SessionEnd::from)
                                .unwrap_or_else(|_| connection_lost("control stream ended")),
                        }
                    }
                };
                if let Some(end) = handle_control(msg, shared, local, &mut pings).await {
                    break end;
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break SessionEnd::Quit };
                match cmd {
                    Command::Quit => break SessionEnd::Quit,
                    Command::VideoDisplayed(speaker) => {
                        let now = Instant::now();
                        let eligible = local.flags.viewing()
                            && shared.state.borrow().peers.iter().any(|p| p.info.session == speaker && p.info.flags.camera_on());
                        if eligible && local.last_display_report.get(&speaker).is_none_or(|last| now.duration_since(*last) >= Duration::from_secs(1)) {
                            local.last_display_report.insert(speaker, now);
                            if conn::write_msg(&mut send, &ControlMsg::VideoDisplayed { speaker }).await.is_err() {
                                break send_failed(&connection);
                            }
                        }
                    }
                    Command::RequestRename(name) => {
                        let name = icu_proto::normalize_name(&name);
                        if local.pending_renew.is_some() {
                            shared.state.send_modify(|s| {
                                s.name_error = Some(
                                    "Die letzte Namensänderung läuft noch – bitte kurz warten."
                                        .into(),
                                )
                            });
                        } else {
                            match send_renew(&mut send, local, shared, &name, RenewReason::UserRename).await {
                                Ok(()) => shared.state.send_modify(|s| s.name_error = None),
                                Err(RenewSendError::Local(e)) => {
                                    shared.state.send_modify(|s| s.name_error = Some(e));
                                }
                                Err(RenewSendError::Transport) => {
                                    break send_failed(&connection);
                                }
                            }
                        }
                    }
                    Command::ReplaceIdentity(identity, settings) => {
                        if local.replace_identity(*identity, settings.map(|s| *s), shared) {
                            break reconnect_with_new_identity();
                        }
                    }
                    Command::CreateInvite { name, ttl_s } => {
                        let name = icu_proto::normalize_name(&name);
                        // Empty asks the server for a generated placeholder.
                        let checked = if name.is_empty() {
                            Ok(())
                        } else {
                            icu_proto::validate_name(&name)
                        };
                        if let Err(e) = checked {
                            shared.state.send_modify(|s| {
                                s.invite_error = Some(crate::problem::name_error_text(&e))
                            });
                        } else if conn::write_msg(&mut send, &ControlMsg::CreateInviteRequest { name, ttl_s }).await.is_err() {
                            break send_failed(&connection);
                        } else {
                            local.invite_pending = true;
                        }
                    }
                    Command::SetUserAdmin { user_id, is_admin } => {
                        if conn::write_msg(&mut send, &ControlMsg::AdminSetRole { user_id, is_admin }).await.is_err() {
                            break send_failed(&connection);
                        }
                    }
                    Command::SetUserDisabled { user_id, disabled } => {
                        if conn::write_msg(&mut send, &ControlMsg::AdminSetDisabled { user_id, disabled }).await.is_err() {
                            break send_failed(&connection);
                        }
                    }
                    Command::RevokeUser { user_id } => {
                        if conn::write_msg(&mut send, &ControlMsg::AdminRevokeUser { user_id }).await.is_err() {
                            break send_failed(&connection);
                        }
                    }
                    Command::RequestPeerCertificate(session) => {
                        let needed = shared.state.borrow().peers.iter().any(|p|
                            p.info.session == session && !matches!(p.certificate, Some(Ok(_))));
                        if needed && conn::write_msg(&mut send, &ControlMsg::PeerCertificateRequest { session }).await.is_err() {
                            break send_failed(&connection);
                        }
                    }
                    Command::RequestUserList => {
                        if conn::write_msg(&mut send, &ControlMsg::UserListRequest).await.is_err() {
                            break send_failed(&connection);
                        }
                    }
                    Command::KickUser { user_id } => {
                        if conn::write_msg(&mut send, &ControlMsg::AdminKick { user_id }).await.is_err() {
                            break send_failed(&connection);
                        }
                    }
                    Command::SetUserRestriction { user_id, muted, deafened } => {
                        let msg = ControlMsg::AdminSetRestriction { user_id, muted, deafened };
                        if conn::write_msg(&mut send, &msg).await.is_err() {
                            break send_failed(&connection);
                        }
                    }
                    cmd => {
                        local.handle_local(cmd, shared);
                    }
                }
            }
            _ = tokio::time::sleep_until(
                state_due.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600)).into()
            ), if state_due.is_some() => {}
            _ = tokio::time::sleep_until(
                corona_due.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600)).into()
            ), if corona_due.is_some() => {
                local.corona_pending = None;
                let color = corona_wire(local.settings.overlay.frame_color);
                let stored = shared.state.borrow().me.as_ref().and_then(|m| m.corona_color);
                if color != stored
                    && conn::write_msg(&mut send, &ControlMsg::SetCoronaColor { color }).await.is_err()
                {
                    break send_failed(&connection);
                }
            }
            _ = renew_tick.tick() => {
                let stored = local.name.clone();
                if maybe_auto_renew(&mut send, local, shared, &stored).await.is_err() {
                    break send_failed(&connection);
                }
            }
            _ = ping.tick() => {
                let nonce = next_nonce;
                next_nonce += 1;
                pings.insert(nonce, Instant::now());
                pings.retain(|_, t| t.elapsed() < Duration::from_secs(30));
                if conn::write_msg(&mut send, &ControlMsg::Ping { nonce }).await.is_err() {
                    break send_failed(&connection);
                }
            }
            _ = tuning_tick.tick() => {
                blocking(|| tune_capture(shared, local));
                let system_muted = blocking(|| {
                    lock(&local.backend)
                        .capture_level()
                        .ok()
                        .flatten()
                        .is_some_and(|l| l.muted)
                });
                local.flags = local.flags.with_bit(UserFlags::SYSTEM_MUTED, system_muted);
                shared.state.send_if_modified(|s| {
                    std::mem::replace(&mut s.system_muted, system_muted) != system_muted
                });
            }
            _ = loss_tick.tick() => {
                let now = Instant::now();
                let enabled = local.settings.audio.auto_level && local.settings.audio.denoise
                    && local.settings.audio.peer_assistance;
                let may_measure=!shared.dsp.transmission_muted() && !shared.dsp.test_tone.load(Ordering::Relaxed);
                {
                    let mut t = lock(&shared.tuning);
                    let _ = t.outgoing.take_window();
                    let overloaded=t.overload_until.is_some_and(|until|now<until)
                        || t.clip_until.is_some_and(|until|now<until)
                        || t.software_until.is_some_and(|until|now<until);
                    t.peer.block_boost(overloaded);
                    let matched = t.peer.window().and_then(|w| t.outgoing.measure_window(w).map(|l|(l,w))).filter(|_|may_measure);
                    t.peer.tick_matched(matched, enabled, now);
                    update_audio_health(&mut t, matched.map(|(l,_)|l), enabled, now, shared);
                    drop(t);
                    blocking(|| local.learn_audio(shared, matched, enabled, now));
                }
                let levels: Vec<_> = lock(&shared.speakers).iter_mut()
                    .filter_map(|(speaker, sp)| sp.feedback.take_window().map(|(level,window)| (*speaker,level,window))).collect();
                if shared.feedback_capable.load(Ordering::Relaxed) && !shared.deafened.load(Ordering::Relaxed) {
                    for (speaker,level,window) in levels {
                        if conn::write_msg(&mut send, &ControlMsg::AudioLevelReport { speaker,level,window }).await.is_err() { break; }
                    }
                }

                prune_stray_speakers(shared);
                let (reports, measured) = collect_loss_reports(shared, Instant::now());
                // After `collect_loss_reports` returned, so the `speakers`
                // lock it holds is gone: `send_modify` while holding one is
                // how this engine would deadlock.
                shared.state.send_modify(|s| {
                    for p in s.peers.iter_mut() {
                        if let Some((_, permille)) =
                            measured.iter().find(|(sess, _)| *sess == p.info.session)
                        {
                            p.recv_loss_permille = Some(*permille);
                        }
                    }
                });
                for (speaker, loss_permille) in reports {
                    if conn::write_msg(&mut send, &ControlMsg::LossReport { speaker, loss_permille }).await.is_err() {
                        break;
                    }
                }
            }
            _ = video_tick.tick() => {
                let viewers = fresh_video_viewers(&mut local.video_viewers, Instant::now());
                local.last_display_report.retain(|_, at| at.elapsed() < Duration::from_secs(3));
                if shared.state.borrow().video_viewers != viewers {
                    shared.state.send_modify(|s| s.video_viewers = viewers);
                }
                let requests = shared.video.lock().unwrap().take_keyframe_requests();
                for speaker in requests {
                    if conn::write_msg(&mut send, &ControlMsg::KeyframeRequest { speaker }).await.is_err() {
                        break;
                    }
                }
            }
            _ = net_tick.tick() => {
                let stats = connection.stats();
                let sample = Sample {
                    rtt: stats.path.rtt,
                    min_rtt: stats.path.min_rtt,
                    congestion_events: stats.path.congestion_events,
                    lost_packets: stats.path.lost_packets,
                    sent_packets: stats.path.sent_packets,
                    cwnd: stats.path.cwnd,
                    udp_tx_bytes: stats.udp_tx.bytes,
                    datagram_queue_bytes: icu_proto::limits::CLIENT_DATAGRAM_SEND_BUFFER_BYTES
                        .saturating_sub(connection.datagram_send_buffer_space())
                        as u64,
                };
                let cond = lock(&shared.net)
                    .observe(sample, net_tick_period);
                let conn_stats = {
                    let now = ByteMarks {
                        udp_tx: stats.udp_tx.bytes,
                        udp_rx: stats.udp_rx.bytes,
                        audio_tx: DebugStats::get(&shared.stats.audio_bytes_sent),
                        audio_rx: DebugStats::get(&shared.stats.audio_bytes_received),
                        video_tx: DebugStats::get(&shared.stats.video_bytes_sent),
                        video_rx: DebugStats::get(&shared.stats.video_bytes_received),
                    };
                    let tick_at = Instant::now();
                    let secs = tick_at.duration_since(prev_tick).as_secs_f64();
                    prev_tick = tick_at;
                    let rate = |a: u64, b: u64| crate::conninfo::kbps(a.saturating_sub(b), secs);
                    let out = crate::conninfo::ConnStats {
                        rtt_ms: stats.path.rtt.as_secs_f32() * 1000.0,
                        min_rtt_ms: stats.path.min_rtt.as_secs_f32() * 1000.0,
                        cwnd: stats.path.cwnd,
                        congestion_events: stats.path.congestion_events,
                        black_holes: stats.path.black_holes_detected,
                        lost_packets: stats.path.lost_packets,
                        sent_packets: stats.path.sent_packets,
                        lost_bytes: stats.path.lost_bytes,
                        current_mtu: stats.path.current_mtu,
                        max_datagram_size: connection.max_datagram_size(),
                        udp_tx_bytes: now.udp_tx,
                        udp_rx_bytes: now.udp_rx,
                        udp_tx_datagrams: stats.udp_tx.datagrams,
                        udp_rx_datagrams: stats.udp_rx.datagrams,
                        tx_kbps: rate(now.udp_tx, prev_bytes.udp_tx),
                        rx_kbps: rate(now.udp_rx, prev_bytes.udp_rx),
                        audio_tx_kbps: rate(now.audio_tx, prev_bytes.audio_tx),
                        audio_rx_kbps: rate(now.audio_rx, prev_bytes.audio_rx),
                        video_tx_kbps: rate(now.video_tx, prev_bytes.video_tx),
                        video_rx_kbps: rate(now.video_rx, prev_bytes.video_rx),
                        datagram_queue_bytes: sample.datagram_queue_bytes,
                        datagrams_expired: stats.datagrams_expired,
                        cond,
                    };
                    prev_bytes = now;
                    out
                };
                shared.state.send_modify(|s| s.conn_stats = Some(conn_stats));
                let local_drops = DebugStats::get(&shared.stats.video_open_uni_drops);
                let tier = quality.tick(&cond, local_drops, Instant::now());
                let previous = shared.video_tier.swap(tier.as_u8(), Ordering::Relaxed);
                if previous != tier.as_u8() {
                    DebugStats::inc(&shared.stats.video_tier_changes);
                    tracing::info!(?tier, ?cond, local_drops, "video quality tier changed");
                }
                shared.stats.video_tier.store(tier.as_u8() as i64, Ordering::Relaxed);
            }
            reason = connection.closed() => {
                break reason.into();
            }
        }
        // Whatever the arm changed — a command, a restriction, the viewing
        // bit — reaches the server here, within the budget.
        if flush_state(
            &mut send,
            local.wire_flags(shared),
            &mut sent_flags,
            &mut set_state_budget,
        )
        .await
        .is_err()
        {
            break send_failed(&connection);
        }
    };

    *shared.video_tx.lock().unwrap() = None;
    *lock(&shared.conn) = None;
    shared.dsp.online.store(false, Ordering::Relaxed);
    blocking(|| restore_capture(shared, &local.backend));
    shared.dsp.audience.store(false, Ordering::Relaxed);
    if matches!(end, SessionEnd::Quit) {
        connection.close(quinn::VarInt::from_u32(codes::NO_ERROR), b"bye");
    }
    drop(session_tasks);
    tokio::time::timeout(Duration::from_millis(500), endpoint.wait_idle())
        .await
        .ok();
    end
}

/// The reader tasks of a session and its endpoint: dropped — at the end of
/// `run_session` or on any early return — the tasks are aborted and the
/// endpoint closed. Two early returns used to skip that and left the old
/// connection's readers running, the datagram reader filling the speakers
/// of the next session, until the idle timeout.
struct SessionTasks {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    endpoint: quinn::Endpoint,
}

impl Drop for SessionTasks {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        self.endpoint
            .close(quinn::VarInt::from_u32(codes::NO_ERROR), b"");
    }
}

/// Where one connection's video frames go: into [`Shared::video`], as long
/// as that connection is the current one ([`VideoIn`]).
#[derive(Clone)]
struct SessionVideo {
    shared: Arc<Shared>,
    generation: u64,
}

impl crate::video::VideoSink for SessionVideo {
    fn deliver(&self, session: Session, frame: crate::video::EncodedFrame) {
        let stats = &self.shared.stats;
        DebugStats::inc(&stats.video_received);
        DebugStats::add(
            &stats.video_bytes_received,
            (icu_proto::header::VIDEO_HEADER_LEN + frame.data.len()) as u64,
        );
        // The decode runs between the two locks: the windows take this one
        // on every repaint, and an H.264 decode is milliseconds.
        let Some(mut decoder) =
            lock(&self.shared.video).accept_for(self.generation, session, &frame)
        else {
            return;
        };
        let picture = crate::video::decode(&mut decoder, &frame.data);
        let new_picture = picture.is_some();
        lock(&self.shared.video).finish_for(self.generation, session, decoder, picture);
        if new_picture {
            if let Some(wake) = lock(&self.shared.video_waker).as_ref() {
                wake();
            }
        }
    }

    fn overflow(&self) {
        DebugStats::inc(&self.shared.stats.video_decode_dropped);
    }
}

/// Admits `user` to the playout side — a fresh speaker under its session, or
/// the one already there (a datagram may arrive before `UserJoined`) with the
/// user's volume — and returns the entry the window shows for it. `Welcome`
/// and `UserJoined` both land here.
fn admit(shared: &Arc<Shared>, settings: &Settings, user: &UserInfo) -> Peer {
    let volume = settings.volume(user.user_id);
    let frame_samples = shared.dsp.frame_samples.load(Ordering::Relaxed);
    let mut peer = Peer::new(user.clone(), volume);
    // Built before the lock is taken: the real-time mixer takes it for
    // every block, and a decoder with Deep PLC is no quick allocation.
    // Wasted when the session's audio came first and made one already.
    let fresh = if lock(&shared.speakers).contains_key(&user.session) {
        None
    } else {
        Speaker::new(user.user_id, volume, frame_samples).ok()
    };
    let mut speakers = lock(&shared.speakers);
    match speakers.get_mut(&user.session) {
        Some(sp) => {
            sp.user_id = user.user_id;
            sp.volume = volume;
            // Its audio came before `UserJoined`: the local path decides.
            if let Some(local) = sp.indicator {
                peer.speaking = local;
            }
        }
        None => {
            if let Some(sp) = fresh {
                speakers.insert(user.session, sp);
            }
        }
    }
    peer
}

/// Forgets every speaker. Their decoders are destroyed after the lock is
/// released: the real-time mixer takes it for every block.
fn drop_speakers(shared: &Shared) {
    let gone = std::mem::take(&mut *lock(&shared.speakers));
    drop(gone);
}

/// Handles commands while a connection is being established. Returns `Quit`
/// when `Quit` arrives (or the channel closes), and `Retry` once the identity
/// was replaced, since the attempt in flight uses the old one.
async fn drain_until_quit(
    cmd_rx: &mut mpsc::UnboundedReceiver<Command>,
    local: &mut LocalState,
    shared: &Arc<Shared>,
) -> SessionEnd {
    loop {
        match cmd_rx.recv().await {
            None | Some(Command::Quit) => return SessionEnd::Quit,
            Some(Command::Reconnect) => {
                return SessionEnd::Retry(Problem::plain("Verbinde neu …"));
            }
            // A new identity abandons the attempt made with the old one —
            // once it is stored.
            Some(Command::ReplaceIdentity(identity, settings)) => {
                if local.replace_identity(*identity, settings.map(|s| *s), shared) {
                    return reconnect_with_new_identity();
                }
            }
            Some(cmd) => {
                local.handle_local(cmd, shared);
            }
        }
    }
}

/// One read from the control stream as the session sees it.
#[derive(Debug)]
enum Inbound {
    /// A message.
    Msg(ControlMsg),
    /// The stream ended or broke: the connection is going away, and its
    /// close code decides whether to retry.
    End,
    /// A frame that breaks the framing or does not decode
    /// (`docs/reference/protocol.md` §2, §8): `PROTOCOL_VIOLATION`, fatal — the same
    /// server would send it again after every reconnect.
    Violation(String),
}

fn inbound(read: Result<Vec<u8>, NetError>) -> Inbound {
    match read {
        Ok(body) => match ControlMsg::decode(&body) {
            Ok(msg) => Inbound::Msg(msg),
            Err(e) => Inbound::Violation(format!("undecodable control message: {e}")),
        },
        Err(NetError::Protocol(e)) => Inbound::Violation(e),
        Err(_) => Inbound::End,
    }
}

/// Forwards control messages until the stream ends; a protocol violation
/// is forwarded as the last item. The channel closing means the stream
/// ended.
async fn control_reader(
    mut recv: quinn::RecvStream,
    tx: mpsc::UnboundedSender<Result<ControlMsg, String>>,
) {
    loop {
        let read = crate::net::read_frame(&mut recv, icu_proto::MAX_CONTROL_FRAME).await;
        let item = match inbound(read) {
            Inbound::Msg(msg) => Ok(msg),
            Inbound::End => return,
            Inbound::Violation(detail) => Err(detail),
        };
        let last = item.is_err();
        if tx.send(item).is_err() || last {
            return;
        }
    }
}

/// Ends a session on a protocol violation: the server is told
/// `PROTOCOL_VIOLATION`, and the client stops instead of reconnecting into
/// the same fault every second (`docs/reference/protocol.md` §5, §6).
fn protocol_violation(connection: &quinn::Connection, detail: String) -> SessionEnd {
    tracing::warn!(%detail, "protocol violation, not reconnecting");
    connection.close(
        quinn::VarInt::from_u32(codes::PROTOCOL_VIOLATION),
        codes::name(codes::PROTOCOL_VIOLATION).as_bytes(),
    );
    SessionEnd::Fatal(Problem::new(crate::problem::UPDATE_NEEDED, detail))
}

async fn datagram_reader(connection: quinn::Connection, shared: Arc<Shared>) {
    let frame_samples = shared.dsp.frame_samples.load(Ordering::Relaxed);
    loop {
        let datagram = match connection.read_datagram().await {
            Ok(d) => d,
            Err(_) => return,
        };
        let now = Instant::now();
        match AudioHeader::decode(&datagram) {
            Ok((header, payload)) => {
                DebugStats::inc(&shared.stats.datagrams_received);
                DebugStats::add(&shared.stats.audio_bytes_received, datagram.len() as u64);
                // The real-time mixer takes the same lock for every block:
                // allocate the payload copy, and a decoder for a session
                // seen for the first time, before taking it.
                let payload = payload.to_vec();
                let mut speakers = lock(&shared.speakers);
                if !speakers.contains_key(&header.speaker) {
                    // Unknown session (UserJoined may still be in flight).
                    drop(speakers);
                    let Ok(fresh) = Speaker::stray(frame_samples) else {
                        continue;
                    };
                    speakers = lock(&shared.speakers);
                    speakers.entry(header.speaker).or_insert(fresh);
                }
                let Some(sp) = speakers.get_mut(&header.speaker) else {
                    continue;
                };
                sp.last_packet = Some(now);
                sp.ended = header.end();
                // The TOC byte gives the peer's frame length (no decode).
                let samples = sp.decoder.nb_samples(&payload);
                let late = sp.jitter.stats().late;
                sp.jitter.push_with_duration(
                    Packet {
                        gain_epoch: header.gain_epoch,
                        seq: header.seq,
                        ts: header.ts,
                        end: header.end(),
                        payload,
                    },
                    samples as u32,
                    now,
                );
                DebugStats::add(&shared.stats.jitter_late, sp.jitter.stats().late - late);
            }
            Err(_) => DebugStats::inc(&shared.stats.datagrams_bad),
        }
    }
}

/// Drops speakers that `datagram_reader` created for a session the peer
/// list does not (or no longer) know, once their datagrams have stopped for
/// 10 s: a straggler after `UserLeft` must not keep a decoder and jitter
/// buffer alive for the rest of the session.
fn prune_stray_speakers(shared: &Arc<Shared>) {
    let known: Vec<Session> = shared
        .state
        .borrow()
        .peers
        .iter()
        .map(|p| p.info.session)
        .collect();
    // Destroyed after the lock is released (`drop_speakers`).
    let gone: Vec<Speaker> = lock(&shared.speakers)
        .extract_if(|session, sp| {
            !known.contains(session)
                && !sp
                    .last_packet
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
        })
        .map(|(_, sp)| sp)
        .collect();
    drop(gone);
}

/// `(session, loss per mille)` for every speaker with traffic this tick.
type LossPerSpeaker = Vec<(Session, u16)>;

/// Reads each speaker's jitter buffer and returns `(to_report, measured)`:
/// the first is what goes on the wire as `LossReport`
/// ([`loss_report_due`]); the second is every speaker's current figure, for
/// the Verbindung window. The gate mutates `last_reported_permille`, so the
/// caller cannot re-derive one from the other — hence both.
fn collect_loss_reports(shared: &Arc<Shared>, now: Instant) -> (LossPerSpeaker, LossPerSpeaker) {
    let mut report = Vec::new();
    let mut all = Vec::new();
    let mut speakers = lock(&shared.speakers);
    for (session, sp) in speakers.iter_mut() {
        let st = sp.jitter.stats();
        let played = st.played - sp.report_base.0;
        let lost = st.lost - sp.report_base.1;
        sp.report_base = (st.played, st.lost);
        let total = played + lost;
        if total == 0 {
            sp.loss_idle = true;
            continue;
        }
        let resumed = std::mem::take(&mut sp.loss_idle);
        let permille = ((lost * 1000) / total) as u16;
        all.push((*session, permille));
        if loss_report_due(
            permille,
            sp.last_reported_permille,
            sp.last_report_at,
            resumed,
            now,
        ) {
            sp.last_reported_permille = permille;
            sp.last_report_at = Some(now);
            report.push((*session, permille));
        }
    }
    (report, all)
}

/// A change smaller than this does not go out on its own: jitter noise must
/// not chatter at the server.
const LOSS_REPORT_STEP: u16 = 10;

/// A figure other than 0 goes out again once the last report is this old:
/// every second 2-s report tick, i.e. every 4 s, inside the server's
/// `LOSS_REPORT_TTL_S`.
const LOSS_REPORT_REFRESH: Duration = Duration::from_secs(3);
const _: () = assert!(LOSS_REPORT_REFRESH.as_secs() + 2 < pl::LOSS_REPORT_TTL_S);

/// Whether this window's figure for a speaker goes out as `LossReport`
/// (`docs/reference/protocol.md` §2.2 Nr. 11): a change of at least [`LOSS_REPORT_STEP`];
/// and a figure other than 0 also once the last report is
/// [`LOSS_REPORT_REFRESH`] old, and in the first window with traffic after
/// one without (`resumed`). The server forgets a report after 6 s and then
/// tells the speaker 0 ‰: sent only on change, a steady loss expired after
/// 6 s, and every push-to-talk sentence after a pause started at the FEC
/// floor.
fn loss_report_due(
    permille: u16,
    last: u16,
    last_at: Option<Instant>,
    resumed: bool,
    now: Instant,
) -> bool {
    permille.abs_diff(last) >= LOSS_REPORT_STEP
        || (permille > 0
            && (resumed
                || last_at.is_none_or(|t| now.saturating_duration_since(t) >= LOSS_REPORT_REFRESH)))
}

/// The cumulative byte counters one `net_tick` reads, so the next tick can
/// turn the difference into a rate. quinn and `DebugStats` both only ever
/// report totals.
#[derive(Debug, Clone, Copy, Default)]
struct ByteMarks {
    udp_tx: u64,
    udp_rx: u64,
    audio_tx: u64,
    audio_rx: u64,
    video_tx: u64,
    video_rx: u64,
}

/// Returns `Some(end)` when the session must end.
async fn handle_control(
    msg: ControlMsg,
    shared: &Arc<Shared>,
    local: &mut LocalState,
    pings: &mut HashMap<u64, Instant>,
) -> Option<SessionEnd> {
    match msg {
        // Kept for the next connect; the current one already passed the
        // handshake (`crate::crl`).
        ControlMsg::CrlUpdate { crl_der } => {
            match crate::crl::accept(&local.paths, &local.identity.ca_der, &crl_der) {
                Ok(true) => tracing::info!(bytes = crl_der.len(), "CRL updated"),
                Ok(false) => tracing::debug!("CRL not newer, kept the stored one"),
                Err(e) => tracing::warn!(error = %e, "CRL refused"),
            }
            let crl = std::fs::read(crate::crl::path(&local.paths)).ok();
            shared.state.send_modify(|s| {
                if let Some(info) = &mut s.conn_info {
                    Arc::make_mut(info).check_trust(&local.identity.ca_der, crl.as_deref());
                }
                for peer in &mut s.peers {
                    if let Some(Ok(cert)) = &mut peer.certificate {
                        Arc::make_mut(cert).check_trust(&local.identity.ca_der, crl.as_deref());
                    }
                }
            });
        }
        ControlMsg::PeerCertificateResponse {
            session,
            certificate_der,
        } => {
            // Reject departed sessions before doing any certificate work.
            if !shared
                .state
                .borrow()
                .peers
                .iter()
                .any(|p| p.info.session == session)
            {
                return None;
            }
            let cert = certificate_der
                .filter(|der| !der.is_empty() && der.len() <= ControlMsg::MAX_CERTIFICATE_DER)
                .ok_or_else(|| "Zertifikat nicht verfügbar".to_string())
                .and_then(|der| {
                    let mut cert = crate::conninfo::CertInfo::from_der(&der);
                    if cert.valid_from.is_none() {
                        return Err("Zertifikat unlesbar".into());
                    }
                    let crl = std::fs::read(crate::crl::path(&local.paths)).ok();
                    cert.check_trust(&local.identity.ca_der, crl.as_deref());
                    Ok(Arc::new(cert))
                });
            shared.state.send_modify(|s| {
                if let Some(p) = s.peers.iter_mut().find(|p| p.info.session == session) {
                    p.certificate = Some(cert);
                }
            });
        }
        ControlMsg::UserJoined { user } => {
            let peer = admit(shared, &local.settings, &user);
            local.cue(shared, Cue::Join);
            if local.settings.ui.notify_join {
                local.notifier.show(crate::notify::Note::joined(&user.name));
            }
            shared.state.send_modify(|s| {
                s.peers.retain(|p| p.info.session != user.session);
                s.peers.push(peer);
                shared.dsp.audience.store(true, Ordering::Relaxed);
                tracing::info!(peers = s.peers.len(), "user joined: peer list updated");
            });
        }
        ControlMsg::UserLeft { session, .. } => {
            local.video_viewers.remove(&session);
            local.last_display_report.remove(&session);
            // Destroyed after the lock is released (`drop_speakers`).
            let gone = lock(&shared.speakers).remove(&session);
            drop(gone);
            shared.video.lock().unwrap().remove(session);
            local.cue(shared, Cue::Leave);
            if local.settings.ui.notify_join {
                let name = shared
                    .state
                    .borrow()
                    .peers
                    .iter()
                    .find(|p| p.info.session == session)
                    .map(|p| p.info.name.clone());
                if let Some(name) = name {
                    local.notifier.show(crate::notify::Note::left(&name));
                }
            }
            shared.state.send_modify(|s| {
                s.peers.retain(|p| p.info.session != session);
                shared
                    .dsp
                    .audience
                    .store(!s.peers.is_empty(), Ordering::Relaxed);
            });
        }
        ControlMsg::UserState { session, flags } => {
            if shared
                .state
                .borrow()
                .me
                .as_ref()
                .is_some_and(|me| me.session == session)
                && !flags.camera_on()
            {
                local.video_viewers.clear();
            }
            // While its audio arrives, the local path decides the indicator
            // (`local_indicator`); read before `send_modify`, never under it.
            let local = lock(&shared.speakers)
                .get(&session)
                .and_then(|sp| sp.indicator);
            shared.state.send_modify(|s| {
                if let Some(me) = s.me.as_mut() {
                    if me.session == session {
                        me.flags = flags;
                    }
                }
                for p in s.peers.iter_mut() {
                    if p.info.session == session {
                        p.info.flags = flags;
                        p.speaking = local.unwrap_or(flags.speaking());
                    }
                }
            });
        }
        ControlMsg::Pong {
            nonce,
            server_time_ms,
        } => {
            if let Some(t) = pings.remove(&nonce) {
                let rtt = t.elapsed();
                shared
                    .stats
                    .rtt_us
                    .store(rtt.as_micros() as i64, Ordering::Relaxed);
                // The server stamped `server_time_ms` roughly half a round
                // trip ago, so compare it against then, not now. Symmetric
                // paths are an assumption, hence ±RTT/2 accuracy.
                let offset = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| {
                        server_time_ms as i64 - (d.as_millis() as i64 - rtt.as_millis() as i64 / 2)
                    });
                shared.state.send_modify(|s| {
                    s.rtt_ms = Some(rtt.as_secs_f32() * 1000.0);
                    if offset.is_some() {
                        s.clock_offset_ms = offset;
                    }
                });
            }
        }
        ControlMsg::UserRtt { rtts } => {
            shared.state.send_modify(|s| {
                // The server measures every member including us, so the
                // vector carries our own entry too — `s.peers` is everyone
                // *else*, which is why it needs matching separately.
                let me = s.me.as_ref().map(|m| m.session);
                for (session, ms) in &rtts {
                    if me == Some(*session) {
                        s.server_rtt_ms = Some(*ms);
                    }
                    for p in s.peers.iter_mut() {
                        if p.info.session == *session {
                            p.rtt_ms = Some(*ms);
                        }
                    }
                }
            });
        }
        // The server considers our certificate expired although we do not:
        // reconnect straight into the renewal.
        ControlMsg::Error { code, .. } if code == codes::RENEW_REQUIRED => {
            local.renew_only = true;
            return Some(SessionEnd::Retry(Problem::plain(
                "Zertifikat abgelaufen – wird erneuert …",
            )));
        }
        ControlMsg::Error { code, detail } => {
            tracing::warn!(code, %detail, "server error");
            let text = crate::problem::from_code(code, &detail).text;
            // A failed invite answers with the placeholder name's code; it
            // belongs under the invite field, not under the own name.
            if local.invite_pending
                && local.pending_renew.is_none()
                && code != codes::RENEW_REJECTED
            {
                local.invite_pending = false;
                shared.state.send_modify(|s| s.invite_error = Some(text));
            } else if code == codes::NAME_TAKEN || code == codes::RENEW_REJECTED {
                // A background renewal must not paint a red error under the
                // name field: only a rename the user asked for lands there.
                let pending = local.take_pending_renew(shared);
                let asked_for_it = pending
                    .map(|p| p.reason == RenewReason::UserRename)
                    .unwrap_or(code == codes::NAME_TAKEN);
                if asked_for_it {
                    shared.state.send_modify(|s| s.name_error = Some(text));
                } else {
                    tracing::warn!(code, %detail, "background renewal rejected");
                }
            } else if code == codes::NOT_ADMIN {
                shared.state.send_modify(|s| s.invite_error = Some(text));
            } else {
                shared
                    .state
                    .send_modify(|s| s.banner = Some(crate::problem::from_code(code, &detail)));
            }
        }
        ControlMsg::AudioLevelHint {
            window,
            correction_cdb,
            reporters,
            agreeing,
            speech_ms,
        } => {
            if shared.feedback_capable.load(Ordering::Relaxed) {
                lock(&shared.tuning).peer.hint_observation(
                    window,
                    correction_cdb,
                    reporters,
                    agreeing,
                    speech_ms,
                    Instant::now(),
                );
            }
        }
        ControlMsg::LossHint { loss_permille } => {
            shared
                .dsp
                .loss_permille
                .store(loss_permille as u32, Ordering::Relaxed);
            lock(&shared.net).set_audio_loss_hint(loss_permille);
            // This hint is about *our* outgoing audio (the encoder above);
            // the incoming buffers' FEC floor follows what we reported for
            // each speaker (`collect_loss_reports`).
        }
        ControlMsg::KeyframeRequest { .. } => {
            shared.keyframe_request.store(true, Ordering::Relaxed);
        }
        ControlMsg::VideoCongestionHint { drop_permille } => {
            lock(&shared.net).set_video_drop_hint(drop_permille);
        }
        ControlMsg::VideoViewer { viewer } => {
            if shared
                .state
                .borrow()
                .peers
                .iter()
                .any(|p| p.info.session == viewer)
            {
                local.video_viewers.insert(viewer, Instant::now());
                let mut viewers: Vec<_> = local.video_viewers.keys().copied().collect();
                viewers.sort_unstable();
                shared.state.send_modify(|s| s.video_viewers = viewers);
            }
        }
        ControlMsg::UserCoronaColor { session, color } => {
            let mine = shared
                .state
                .borrow()
                .me
                .as_ref()
                .is_some_and(|m| m.session == session);
            shared.state.send_modify(|s| {
                if let Some(me) = s.me.as_mut().filter(|me| me.session == session) {
                    me.corona_color = color;
                }
                for p in s.peers.iter_mut().filter(|p| p.info.session == session) {
                    p.info.corona_color = color;
                }
            });
            // Picked on another device of this user: the settings follow,
            // unless a pick here is still on its way.
            if mine && local.corona_pending.is_none() {
                local.adopt_corona(color, shared);
            }
        }
        ControlMsg::UserRenamed { session, name } => {
            let me_session = shared.state.borrow().me.as_ref().map(|m| m.session);
            if me_session == Some(session) {
                local.name = name.clone();
            }
            shared.state.send_modify(|s| {
                if let Some(me) = s.me.as_mut() {
                    if me.session == session {
                        me.name = name.clone();
                        s.name_error = None;
                    }
                }
                for p in s.peers.iter_mut() {
                    if p.info.session == session {
                        p.info.name = name.clone();
                    }
                }
            });
        }
        ControlMsg::RenewResponse { leaf_der } => {
            local.apply_renewed_leaf(leaf_der, shared);
        }
        ControlMsg::CreateInviteResponse { bundle, expires } => {
            local.invite_pending = false;
            shared.state.send_modify(|s| {
                s.last_invite = Some((bundle, expires));
                s.invite_error = None;
            });
        }
        ControlMsg::AdminSetRoleOk { .. }
        | ControlMsg::AdminSetDisabledOk { .. }
        | ControlMsg::AdminRevokeUserOk { .. }
        | ControlMsg::AdminKickOk { .. }
        | ControlMsg::AdminSetRestrictionOk { .. } => {
            // Acks don't carry the full row; the Nutzer tab re-issues
            // RequestUserList right after sending an action, so this just
            // confirms no error banner is needed while that refresh lands.
            shared.state.send_modify(|s| s.user_action_error = None);
        }
        ControlMsg::UserListResponse { users } => {
            shared.state.send_modify(|s| {
                s.user_list = users;
                s.user_list_generation = s.user_list_generation.wrapping_add(1);
            });
        }
        ControlMsg::AdminActionError { code, detail, .. } => {
            tracing::warn!(code, %detail, "admin action rejected");
            let text = crate::problem::from_code(code, &detail).text;
            shared
                .state
                .send_modify(|s| s.user_action_error = Some(text));
        }
        ControlMsg::UserAdminChanged { is_admin } => {
            shared.state.send_modify(|s| {
                s.is_admin = is_admin;
                if !is_admin {
                    s.user_list.clear();
                    s.user_action_error = None;
                }
            });
        }
        ControlMsg::UserRestrictionChanged { muted, deafened } => {
            shared.state.send_modify(|s| {
                s.restricted_muted = muted;
                s.restricted_deafened = deafened;
            });
            // Brings the local pipeline and buttons in line; the changed
            // flags go out as `SetState` like any other change (the session
            // loop compares them with what the server holds from us), so a
            // deafen-only restriction mutes on the server too.
            if deafened && !local.flags.deafened() {
                local.handle_local(Command::SetDeafened(true), shared);
            }
            if muted && !local.flags.muted() {
                local.handle_local(Command::SetMuted(true), shared);
            }
        }
        ControlMsg::Welcome { .. } => {
            return Some(SessionEnd::Fatal(Problem::new(
                crate::problem::UPDATE_NEEDED,
                "second Welcome",
            )));
        }
        ControlMsg::Hello { .. }
        | ControlMsg::SetState { .. }
        | ControlMsg::Ping { .. }
        | ControlMsg::RenewRequest { .. }
        | ControlMsg::AudioLevelReport { .. }
        | ControlMsg::LossReport { .. }
        | ControlMsg::SetName { .. }
        | ControlMsg::CreateInviteRequest { .. }
        | ControlMsg::AdminSetRole { .. }
        | ControlMsg::AdminSetDisabled { .. }
        | ControlMsg::AdminRevokeUser { .. }
        | ControlMsg::PeerCertificateRequest { .. }
        | ControlMsg::VideoDisplayed { .. }
        | ControlMsg::SetCoronaColor { .. }
        | ControlMsg::UserListRequest
        | ControlMsg::AdminKick { .. }
        | ControlMsg::AdminSetRestriction { .. } => {
            return Some(SessionEnd::Fatal(Problem::new(
                crate::problem::UPDATE_NEEDED,
                format!("server sent client message {}", msg.variant()),
            )));
        }
    }
    None
}

/// The `SetState` budget: a token bucket like the server's (`docs/reference/protocol.md`
/// §2.2 Nr. 5, 20 per second, burst 20), at three quarters of its size, so
/// the client never sends what the server would answer with `RATE_LIMITED`
/// and drop — three of those within 60 s would even be `RATE_LIMITED_FATAL`.
/// Exactly the server's figures are not enough: its bucket refills from
/// when it *processed* a burst, which trails the client's clock by the
/// transit and the processing of that burst, so a 21st message sent on the
/// client's schedule arrived to an empty bucket (measured: the test
/// `a_burst_of_mute_changes_ends_on_the_last_one` got `RATE_LIMITED`).
#[derive(Debug)]
struct SetStateBudget {
    tokens: f64,
    last: Instant,
}

impl SetStateBudget {
    /// Tokens per second and burst.
    const PER_S: f64 = pl::SET_STATE_PER_S as f64 * 0.75;

    fn new(now: Instant) -> SetStateBudget {
        SetStateBudget {
            tokens: Self::PER_S,
            last: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let earned = now.saturating_duration_since(self.last).as_secs_f64() * Self::PER_S;
        self.tokens = (self.tokens + earned).min(Self::PER_S);
        self.last = now;
    }

    /// Takes a token if there is one.
    fn take_at(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// When the next token is there (a millisecond late rather than early,
    /// so the wake-up never finds the bucket a hair short).
    fn ready_at(&mut self, now: Instant) -> Instant {
        self.refill(now);
        if self.tokens >= 1.0 {
            now
        } else {
            now + Duration::from_secs_f64((1.0 - self.tokens) / Self::PER_S)
                + Duration::from_millis(1)
        }
    }
}

/// Whether `a` and `b` agree on everything a client may set.
fn same_settable(a: UserFlags, b: UserFlags) -> bool {
    (a.0 ^ b.0) & UserFlags::CLIENT_SETTABLE == 0
}

/// Sends the newest flags as `SetState` when they differ from what the
/// server holds from us (`sent`) and the budget has a token. Without one the
/// change waits — coalesced with whatever follows — instead of being
/// dropped: a lost final unmute would leave the server discarding the
/// user's audio. `Err` only when the control stream broke.
async fn flush_state(
    send: &mut quinn::SendStream,
    flags: UserFlags,
    sent: &mut UserFlags,
    budget: &mut SetStateBudget,
) -> Result<(), ()> {
    let flags = flags.sanitized();
    if same_settable(flags, *sent) || !budget.take_at(Instant::now()) {
        return Ok(());
    }
    conn::write_msg(send, &ControlMsg::SetState { flags })
        .await
        .map_err(|_| ())?;
    *sent = flags;
    Ok(())
}

impl From<NetError> for EngineError {
    fn from(e: NetError) -> Self {
        EngineError(e.to_string())
    }
}

/// What the camera thread needs from the engine (M4).
#[derive(Clone)]
pub struct CameraLink {
    source_epoch: Option<usize>,
    shared: Arc<Shared>,
    runtime: tokio::runtime::Handle,
    cmd: mpsc::UnboundedSender<Command>,
}

impl CameraLink {
    #[cfg(feature = "camera")]
    pub(crate) fn shared_epoch(&self) -> usize {
        self.shared.state.borrow().privacy_epoch
    }

    /// Binds a capture source to the state it was opened for.
    #[cfg(feature = "camera")]
    pub(crate) fn for_epoch(&self, epoch: usize) -> Self {
        Self {
            source_epoch: Some(epoch),
            ..self.clone()
        }
    }

    /// Whether a captured frame still belongs to the active source.
    #[cfg(feature = "camera")]
    pub(crate) fn source_current(&self) -> bool {
        let state = self.shared.state.borrow();
        match self.source_epoch {
            Some(epoch) => state.privacy_epoch == epoch,
            None => !state.privacy_active,
        }
    }

    fn send_allowed(&self) -> bool {
        let state = self.shared.state.borrow();
        state.camera_on
            && match self.source_epoch {
                Some(epoch) => state.privacy_epoch == epoch,
                None => !state.privacy_active,
            }
    }

    /// Sends a command to the engine (the camera switching itself off).
    pub fn command(&self, cmd: Command) {
        let _ = self.cmd.send(cmd);
    }

    /// Sends one encoded frame on the live connection; `Err` when not
    /// connected or when flow control dropped it.
    pub fn send_frame(&self, keyframe: bool, ts: u32, data: &[u8]) -> Result<(), String> {
        // A frame encoded just before the camera went off stays here.
        if !self.send_allowed() {
            return Err("camera off".into());
        }
        let Some(sender) = lock(&self.shared.video_tx).clone() else {
            return Err("not connected".into());
        };
        let mut state = self.shared.state.subscribe();
        let cancelled = async {
            loop {
                // Drop the watch read guard before consulting shared state:
                // a waiting writer must never meet a recursive read lock.
                drop(state.borrow_and_update());
                if !self.send_allowed() {
                    return;
                }
                if state.changed().await.is_err() {
                    return;
                }
            }
        };
        let r = self
            .runtime
            .block_on(sender.send_until(keyframe, ts, data, cancelled, |send| {
                let _gate = lock(&self.shared.conn);
                if !self.send_allowed() {
                    let _ = send.reset(0u32.into());
                    Err("camera source changed".into())
                } else {
                    send.finish().map_err(|e| e.to_string())
                }
            }));
        match &r {
            Ok(()) => {
                DebugStats::inc(&self.shared.stats.video_sent);
                DebugStats::add(
                    &self.shared.stats.video_bytes_sent,
                    (icu_proto::header::VIDEO_HEADER_LEN + data.len()) as u64,
                );
            }
            // The 50 ms `open_uni` timeout in `video::Sender::send` — a
            // direct, immediate local congestion signal the quality
            // controller leads on, ahead of the smoothed remote condition
            // (`crate::quality`).
            Err(e) if e == "flow control: frame dropped" => {
                DebugStats::inc(&self.shared.stats.video_open_uni_drops)
            }
            Err(_) => {}
        }
        r
    }

    /// The current outgoing video quality tier, as set by the
    /// `QualityController` (`crate::quality`).
    pub fn video_tier(&self) -> VideoTier {
        VideoTier::from_u8(self.shared.video_tier.load(Ordering::Relaxed))
    }

    /// The tier the camera thread encodes at: [`CameraLink::video_tier`],
    /// or paused while no peer is viewing (`UserFlags::VIEWING`) — the
    /// server would relay the frames to nobody — and while the camera is
    /// off in the engine. The latter at once: the window's
    /// `CameraHandle::sync`, which stops sending too, does not run while
    /// the window is hidden or minimised, and a hotkey switches the camera
    /// off just the same. Resuming builds a fresh encoder, whose first
    /// frame is an IDR.
    pub fn send_tier(&self) -> VideoTier {
        let state = self.shared.state.borrow();
        if !state.camera_on {
            return VideoTier::T4;
        }
        send_tier(self.video_tier(), &state.peers)
    }

    /// Whether an IDR was requested since the last call (server
    /// `KeyframeRequest` or `Command::ForceKeyframe`).
    pub fn take_keyframe_request(&self) -> bool {
        let taken = self.shared.keyframe_request.swap(false, Ordering::Relaxed);
        if taken {
            DebugStats::inc(&self.shared.stats.keyframes_forced);
        }
        taken
    }

    /// Whether frames can be sent right now.
    pub fn connected(&self) -> bool {
        self.shared.video_tx.lock().unwrap().is_some()
    }
}

/// [`CameraLink::send_tier`], without the engine. Also the main window's
/// camera tile, so its rec dot follows the same rule as the camera thread.
pub(crate) fn send_tier(tier: VideoTier, peers: &[Peer]) -> VideoTier {
    if peers.iter().any(|p| p.info.flags.viewing()) {
        tier
    } else {
        VideoTier::T4
    }
}

fn fresh_video_viewers(reports: &mut HashMap<Session, Instant>, now: Instant) -> Vec<Session> {
    reports.retain(|_, at| now.duration_since(*at) < Duration::from_secs(3));
    let mut viewers: Vec<_> = reports.keys().copied().collect();
    viewers.sort_unstable();
    viewers
}

/// Read side of the received video (M4): the overlay's tiles take everything
/// at once with [`VideoFrames::latest`], the participant list picks per
/// session with [`VideoFrames::seqs`] and [`VideoFrames::picture`].
#[derive(Clone)]
pub struct VideoFrames {
    shared: Arc<Shared>,
}

impl VideoFrames {
    /// Latest decoded frame per session.
    pub fn latest(&self) -> Vec<(Session, crate::video::Frame)> {
        self.shared.video.lock().unwrap().latest()
    }

    /// `(session, seq)` per session currently showing video. Copies no
    /// pixels, so a window can ask on every repaint.
    pub fn seqs(&self) -> Vec<(Session, u64)> {
        self.shared.video.lock().unwrap().seqs()
    }

    /// One session's latest picture and its `seq`.
    pub fn picture(&self, session: Session) -> Option<(u64, crate::video::Frame)> {
        self.shared.video.lock().unwrap().picture(session)
    }

    /// Runs `wake` on the decode thread after every new picture; replaces
    /// the one set before (one window draws the participant list).
    pub fn set_waker(&self, wake: impl Fn() + Send + 'static) {
        *self.shared.video_waker.lock().unwrap() = Some(Box::new(wake));
    }
}

impl Engine {
    /// Link for the camera thread.
    pub fn camera_link(&self) -> CameraLink {
        CameraLink {
            source_epoch: None,
            shared: self.shared.clone(),
            runtime: self.runtime(),
            cmd: self.cmd_tx.clone(),
        }
    }

    /// Frames for the overlay.
    pub fn video_frames_source(&self) -> VideoFrames {
        VideoFrames {
            shared: self.shared.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn setup_guard_preserves_user_mute_and_deafen_across_release() {
        use super::*;
        for user_muted in [false, true] {
            for deafened in [false, true] {
                let mut settings = Settings::default();
                settings.audio.muted = user_muted;
                let ctl = DspControl::from_settings(&settings, false);
                // apply_mute_deafen owns this input; setup owns only its
                // separate inhibition, never that input or the settings.
                ctl.muted.store(user_muted || deafened, Ordering::Relaxed);
                ctl.setup_active.store(true, Ordering::Relaxed);
                assert!(ctl.transmission_muted());
                ctl.apply_settings(&settings);
                assert!(ctl.transmission_muted());
                ctl.setup_active.store(false, Ordering::Relaxed);
                assert_eq!(ctl.transmission_muted(), user_muted || deafened);
                assert_eq!(settings.audio.muted, user_muted);
            }
        }
        let ctl = DspControl::from_settings(&Settings::default(), false);
        ctl.setup_active.store(true, Ordering::Relaxed);
        for muted in [true, false, true] {
            ctl.muted.store(muted, Ordering::Relaxed);
            assert!(
                ctl.transmission_muted(),
                "a mute hotkey cannot release setup"
            );
        }
        ctl.setup_active.store(false, Ordering::Relaxed);
        assert!(
            ctl.transmission_muted(),
            "the last explicit mute survives closing"
        );
    }

    #[test]
    fn unavailable_microphone_clears_profile_actions_and_feedback_is_not_current_offline() {
        use super::*;
        use crate::audio::health::{AudioHealth, State};
        let mut health = AudioHealth {
            state: State::Good,
            reporters: 3,
            remote_correction_db: Some(0.0),
            local_correction_db: Some(0.0),
            feedback_age_secs: Some(1.0),
            ..Default::default()
        };
        assert!(publish_audio_profile(&mut health, Some(123)));
        let offline = health_without_feedback(&health, State::LocalOnly);
        assert_eq!(offline.state, State::LocalOnly);
        assert_eq!(offline.reporters, 0);
        assert_eq!(offline.remote_correction_db, None);
        assert_eq!(offline.feedback_age_secs, None);
        assert_eq!(offline.profile_saved_at, Some(123));
        assert!(!publish_audio_profile(&mut health, None));
        assert!(!health.profile_saved);
        assert_eq!(health.profile_saved_at, None);
    }

    #[test]
    fn automatic_threshold_still_rejects_clicks_and_recovers_from_steady_noise() {
        use super::{floor_sample_is_speech, vad_auto::AutoThreshold, Gate, Mode};
        use std::time::{Duration, Instant};

        for attack_ms in [40, 80] {
            let start = Instant::now();
            let mut auto = AutoThreshold::new(start);
            let mut gate = Gate::new(Mode::Vad, Duration::ZERO, Duration::ZERO);
            gate.set_attack(Duration::from_millis(attack_ms));
            let mut frame_voice = false;
            let mut heard_noise = false;
            for block in 0..1100 {
                let now = start + Duration::from_millis(block * 10);
                let prob = if block < 2 {
                    0.9
                } else if block < 100 {
                    0.02
                } else {
                    0.5
                };
                auto.update(
                    now,
                    floor_sample_is_speech(gate.is_open(), prob, auto.threshold()),
                    prob,
                );
                frame_voice |= prob >= auto.threshold();
                if block % 2 == 1 {
                    gate.update(now, frame_voice);
                    frame_voice = false;
                }
                if block < 100 {
                    assert!(!gate.is_open(), "a short click must not open the gate");
                } else {
                    heard_noise |= gate.is_open();
                }
            }
            assert!(
                heard_noise,
                "exercise recovery after a false speech detection"
            );
            assert!(
                !gate.is_open(),
                "watchdog must recover despite candidate protection"
            );
            assert!(auto.threshold() > 0.5);
        }
    }

    #[test]
    fn automatic_threshold_preserves_speech_during_vad_attack() {
        use super::{floor_sample_is_speech, vad_auto::AutoThreshold, Gate, Mode};
        use std::time::{Duration, Instant};

        for attack_ms in [40, 80] {
            let start = Instant::now();
            let mut auto = AutoThreshold::new(start);
            let mut gate = Gate::new(Mode::Vad, Duration::ZERO, Duration::ZERO);
            gate.set_attack(Duration::from_millis(attack_ms));
            let mut frame_voice = false;
            for block in 0..20 {
                let now = start + Duration::from_millis(block * 10);
                let prob = 0.25; // Quiet speech just above the initial threshold.
                auto.update(
                    now,
                    floor_sample_is_speech(gate.is_open(), prob, auto.threshold()),
                    prob,
                );
                frame_voice |= prob >= auto.threshold();
                if block % 2 == 1 {
                    gate.update(now, frame_voice);
                    frame_voice = false;
                }
            }
            assert!(
                gate.is_open(),
                "{attack_ms} ms attack swallowed sustained speech"
            );
            assert!(auto.threshold() < 0.25);
        }
    }

    #[test]
    fn video_viewers_expire_without_new_display_reports() {
        use super::{fresh_video_viewers, HashMap, Instant};
        use std::time::Duration;

        let now = Instant::now();
        let mut reports = HashMap::from([
            (3, now - Duration::from_millis(2999)),
            (2, now - Duration::from_secs(3)),
            (1, now - Duration::from_secs(4)),
        ]);
        assert_eq!(fresh_video_viewers(&mut reports, now), vec![3]);
        assert_eq!(
            fresh_video_viewers(&mut reports, now + Duration::from_millis(1)),
            Vec::<u16>::new()
        );
    }

    #[test]
    fn ptt_users_off_air_speech_does_not_feed_the_noise_floor() {
        use super::floor_sample_is_speech;
        // VAD mode: candidate speech is protected during the attack too.
        assert!(floor_sample_is_speech(true, 0.1, 0.3));
        assert!(floor_sample_is_speech(false, 0.9, 0.3));
        // PTT mode, key up: a block over the threshold is speech all the
        // same; a quiet one feeds the floor.
        assert!(floor_sample_is_speech(false, 0.9, 0.3));
        assert!(!floor_sample_is_speech(false, 0.1, 0.3));
        assert!(floor_sample_is_speech(true, 0.1, 0.3));
    }

    /// Talking while muted in voice activation: the shut gate is no sign of
    /// silence, so the speech stays out of the floor and the voice detector
    /// keeps hearing it — the "Stumm — du sprichst" cue stays on.
    #[test]
    fn muted_speech_does_not_feed_the_noise_floor() {
        use super::{floor_sample_is_speech, vad_auto::AutoThreshold};
        use std::time::{Duration, Instant};
        assert!(floor_sample_is_speech(false, 0.9, 0.3));
        assert!(!floor_sample_is_speech(false, 0.1, 0.3));
        let t0 = Instant::now();
        let mut auto = AutoThreshold::new(t0);
        let start = auto.threshold();
        // Three seconds of steady speech, one 10-ms block at a time.
        for i in 0..300u64 {
            let t = t0 + Duration::from_millis(10 * i);
            let th = auto.threshold();
            auto.update(t, floor_sample_is_speech(false, 0.95, th), 0.95);
        }
        assert_eq!(
            auto.threshold(),
            start,
            "the floor did not absorb the speech"
        );
        assert!(0.95 >= auto.threshold(), "still heard as voice");
    }

    #[test]
    fn the_voice_cue_holds_through_the_hangover() {
        use super::voice_held;
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let hold = Duration::from_millis(1000);
        assert!(!voice_held(None, t0, hold));
        assert!(voice_held(Some(t0), t0, hold));
        assert!(
            voice_held(Some(t0), t0 + Duration::from_millis(400), hold),
            "between words"
        );
        assert!(voice_held(Some(t0), t0 + hold, hold));
        assert!(!voice_held(
            Some(t0),
            t0 + Duration::from_millis(1001),
            hold
        ));
        // A block stamped after the push took `now`.
        assert!(voice_held(Some(t0 + Duration::from_millis(5)), t0, hold));
        assert!(!voice_held(
            Some(t0),
            t0 + Duration::from_millis(1),
            Duration::ZERO
        ));
    }

    #[test]
    fn typing_doubles_the_attack_and_zero_stays_off() {
        use super::attack_for;
        assert_eq!(attack_for(40, false), 40);
        assert_eq!(attack_for(40, true), 80);
        assert_eq!(attack_for(0, true), 0, "attack 0 switches the filter off");
        assert_eq!(attack_for(u32::MAX, true), u32::MAX);
    }

    #[test]
    fn preroll_only_when_a_vad_spurt_opens() {
        use super::{preroll_now, Decision, Mode};
        assert!(preroll_now(false, false, Decision::Send, Mode::Vad));
        assert!(
            !preroll_now(true, false, Decision::Send, Mode::Vad),
            "already open"
        );
        assert!(
            !preroll_now(false, false, Decision::Silent, Mode::Vad),
            "still closed"
        );
        assert!(
            !preroll_now(false, false, Decision::Send, Mode::Ptt),
            "PTT: no pre-roll"
        );
        assert!(
            !preroll_now(false, true, Decision::Send, Mode::Vad),
            "the previous frame was the END packet: never send it twice"
        );
    }

    use super::*;

    fn peer(session: Session, flags: u8) -> Peer {
        Peer::new(
            UserInfo {
                user_id: u32::from(session),
                session,
                name: format!("p{session}"),
                flags: UserFlags(flags),
                corona_color: None,
            },
            100.0,
        )
    }

    /// A frame that does not decode, or breaks the framing, is a protocol
    /// violation and ends the client's attempts; a broken stream is just the
    /// connection going away (`docs/reference/protocol.md` §5, §6).
    #[test]
    fn an_undecodable_control_message_is_a_violation() {
        let ping = ControlMsg::Ping { nonce: 7 }.encode().unwrap();
        let body = ping[icu_proto::framing::LEN_PREFIX..].to_vec();
        assert!(matches!(
            inbound(Ok(body)),
            Inbound::Msg(ControlMsg::Ping { nonce: 7 })
        ));
        // Variant 200 exists in no version of the protocol.
        assert!(matches!(inbound(Ok(vec![200, 1])), Inbound::Violation(_)));
        assert!(matches!(inbound(Ok(Vec::new())), Inbound::Violation(_)));
        assert!(matches!(
            inbound(Err(NetError::Protocol("length 0".into()))),
            Inbound::Violation(_)
        ));
        assert!(matches!(
            inbound(Err(NetError::Connect("reset".into()))),
            Inbound::End
        ));
    }

    /// A saved device the backend no longer lists is replaced by the system
    /// default; a backend that lists nothing of that kind cannot tell.
    #[test]
    fn a_missing_device_means_the_default() {
        let dev = |id: &str, kind| DeviceInfo {
            id: id.into(),
            description: id.into(),
            kind,
        };
        let list = [
            dev("mic", DeviceKind::Input),
            dev("spk", DeviceKind::Output),
        ];
        let input = |saved, list: &[DeviceInfo]| effective_device(saved, list, DeviceKind::Input);
        assert_eq!(input(Some("mic"), &list), Some("mic".into()));
        assert_eq!(input(Some("usb"), &list), None);
        assert_eq!(input(None, &list), None);
        assert_eq!(
            input(Some("usb"), &list[1..]),
            Some("usb".into()),
            "no inputs listed at all"
        );
    }

    /// The transmit cue marks where sending starts and where its `END`
    /// packet goes out, nothing in between.
    #[test]
    fn tx_cue_on_the_edges_only() {
        assert_eq!(tx_cue(false, true, Decision::Send), Some(TX_CUE_START));
        assert_eq!(
            tx_cue(true, true, Decision::Send),
            None,
            "hangover or release"
        );
        assert_eq!(tx_cue(true, true, Decision::SendEnd), Some(TX_CUE_STOP));
        assert_eq!(tx_cue(true, false, Decision::Silent), None);
        assert_eq!(tx_cue(false, false, Decision::Silent), None);
        assert_eq!(
            tx_cue(false, false, Decision::Send),
            None,
            "offline: nothing goes out"
        );
    }

    /// On within a block of the first datagram, off two frames after the
    /// last or at once on `END`; after a second without audio the server's
    /// flag decides again.
    #[test]
    fn the_local_indicator_follows_the_datagrams() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        assert_eq!(local_indicator(None, false, t0), None);
        assert_eq!(local_indicator(Some(t0), false, t0), Some(true));
        assert_eq!(local_indicator(Some(t0), false, at(59)), Some(true));
        assert_eq!(local_indicator(Some(t0), false, at(60)), Some(false));
        assert_eq!(local_indicator(Some(t0), true, at(1)), Some(false), "END");
        assert_eq!(local_indicator(Some(t0), false, at(999)), Some(false));
        assert_eq!(local_indicator(Some(t0), false, at(1000)), None);
        // A datagram stamped after the mixer took `now`.
        assert_eq!(local_indicator(Some(at(5)), false, t0), Some(true));
    }

    /// Switching the Opus frame from 20 to 10 ms halfway through a long
    /// frame leaves nothing queued: every later frame goes out with the
    /// block that completes it, and `ts` still counts every sample.
    #[test]
    fn a_shorter_frame_leaves_no_remainder_behind() {
        let block = [0.0f32; BLOCK];
        let (mut acc, mut acc_frame, mut ts) = (Vec::new(), 0usize, 0u32);
        let mut sent = Vec::new();
        // One DSP tick: collect a block, send a frame if one is complete;
        // returns what stays queued.
        let mut tick = |frame: usize| {
            collect_block(&mut acc, &mut acc_frame, &mut ts, frame, &block);
            if acc.len() >= frame {
                sent.push(ts);
                ts = ts.wrapping_add(frame as u32);
                acc.drain(..frame);
            }
            acc.len()
        };
        assert_eq!(tick(960), 480);
        assert_eq!(tick(960), 0);
        assert_eq!(tick(960), 480, "half a long frame collected");
        for _ in 0..5 {
            assert_eq!(tick(480), 0, "nothing left over");
        }
        // Back to 20 ms from an empty buffer: nothing is dropped.
        assert_eq!(tick(960), 480);
        assert_eq!(tick(960), 0);
        assert_eq!(
            sent,
            [0, 1440, 1920, 2400, 2880, 3360, 3840],
            "the dropped half moves ts on"
        );
    }

    /// A large graph quantum arrives in one piece and is not trimmed; a
    /// stall's backlog is, and the skipped time moves `ts` on.
    #[test]
    fn the_capture_backlog_follows_the_quantum() {
        assert_eq!(capture_backlog_limit(240), MAX_CAPTURE_BACKLOG);
        assert_eq!(capture_backlog_limit(8192), 8192 + 2 * BLOCK);
        let mut acc = vec![0.0; BLOCK];
        let mut ts = 1000u32;
        skip_capture(&mut acc, &mut ts, 3 * BLOCK);
        assert!(acc.is_empty());
        assert_eq!(ts, 1000 + 4 * BLOCK as u32);
    }

    #[test]
    fn overflow_discards_old_audio_and_accounts_for_every_lost_sample() {
        for room in [0, BLOCK / 2] {
            let (mut producer, mut consumer) = HeapRb::<f32>::new(2 * BLOCK).split();
            producer.push_slice(&vec![0.75; 2 * BLOCK - room]);
            let pushed = producer.push_slice(&[0.5; BLOCK]);
            assert_eq!(pushed, room);
            let lost = BLOCK - pushed;
            let mut partial = vec![0.25; BLOCK];
            let mut ts = u32::MAX - 100;
            let before = ts;
            discard_capture(&mut consumer, &mut partial, &mut ts, lost);
            assert!(partial.is_empty());
            assert_eq!(consumer.occupied_len(), 0);
            assert_eq!(ts, before.wrapping_add((3 * BLOCK + lost) as u32));
            producer.push_slice(&[0.125; BLOCK]);
            let mut fresh = [0.0; BLOCK];
            assert_eq!(consumer.pop_slice(&mut fresh), BLOCK);
            assert!(fresh.iter().all(|&x| x == 0.125));
        }
    }

    #[test]
    fn new_microphone_does_not_inherit_a_noisy_devices_threshold_or_attack() {
        let start = Instant::now();
        let mut threshold = vad_auto::AutoThreshold::new(start);
        let mut gate = Gate::new(Mode::Vad, Duration::ZERO, Duration::from_millis(200));
        gate.set_attack(Duration::from_millis(40));
        for i in 0..1000 {
            let now = start + Duration::from_millis(i * 10);
            let speech = floor_sample_is_speech(gate.is_open(), 0.65, threshold.threshold());
            threshold.update(now, speech, 0.65);
            gate.update_audio(
                now,
                0.65 >= threshold.threshold(),
                Duration::from_millis(10),
            );
        }
        assert!(threshold.threshold() > 0.75);
        let now = start + Duration::from_secs(10);
        reset_capture_detection(&mut gate, &mut threshold, now);
        for i in 0..5 {
            let voice = 0.60 >= threshold.threshold();
            let decision = gate.update_audio(now, voice, Duration::from_millis(10));
            assert_eq!(
                decision,
                if i == 4 {
                    Decision::Send
                } else {
                    Decision::Silent
                }
            );
        }
    }

    #[test]
    fn reconnect_preserves_pending_microphone_compensation_and_forces_fresh_feedback() {
        let mut tuning = AudioTuning {
            capture_delta: -1.0,
            seed_gain: Some(3.0),
            software_gain_db: 2.0,
            ..Default::default()
        };
        tuning.peer.offset_db = 1.5;
        tuning.new_connection(17);
        assert_eq!(tuning.capture_delta, -1.0);
        assert_eq!(tuning.seed_gain, Some(3.0));
        assert_eq!(tuning.software_gain_db, 2.0);
        assert_eq!(tuning.peer.offset_db, 0.0);
        assert_eq!(tuning.peer.epoch(), 17);
        assert!(tuning.force_epoch);
    }

    /// Whichever way `run_session` returns, its reader tasks end with it.
    #[tokio::test]
    async fn dropping_the_session_tasks_aborts_them() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _tx = tx;
            std::future::pending::<()>().await
        });
        let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        drop(SessionTasks {
            tasks: vec![task],
            endpoint,
        });
        assert!(rx.await.is_err(), "the task was aborted");
    }

    /// Received video belongs to one connection: after a reset, frames
    /// still queued from the previous one create nothing.
    #[test]
    fn video_of_an_earlier_connection_is_dropped() {
        let frame = || crate::video::EncodedFrame {
            header: icu_proto::VideoHeader {
                flags: icu_proto::header::VIDEO_FLAG_KEYFRAME,
                speaker: 3,
                seq: 1,
                ts: 0,
                codec: icu_proto::header::VIDEO_CODEC_H264,
            },
            data: vec![0, 0, 0, 1],
        };
        let mut video = VideoIn::default();
        let old = video.reset();
        let _ = video.accept_for(old, 3, &frame());
        assert_eq!(video.frames(3), 1);
        let new = video.reset();
        assert_eq!(video.frames(3), 0, "a reset forgets everything");
        assert!(video.accept_for(old, 3, &frame()).is_none());
        assert_eq!(video.frames(3), 0, "a stale frame creates no receiver");
        let _ = video.accept_for(new, 3, &frame());
        assert_eq!(video.frames(3), 1);
    }

    /// Steady loss is reported again every 4 s, so the server's 6-s expiry
    /// never drops it; after a pause the first window reports it at once;
    /// a clean line stays quiet.
    #[test]
    fn a_steady_loss_is_reported_before_the_server_forgets_it() {
        let t0 = Instant::now();
        let mut last = 0u16;
        let mut last_at = None;
        let mut sent = Vec::new();
        // One 2-s window: its figure, or `None` without traffic.
        let mut idle = false;
        let mut window = |s: u64, figure: Option<u16>, sent: &mut Vec<(u64, u16)>| {
            let Some(permille) = figure else {
                idle = true;
                return;
            };
            let now = t0 + Duration::from_secs(s);
            let resumed = std::mem::take(&mut idle);
            if loss_report_due(permille, last, last_at, resumed, now) {
                last = permille;
                last_at = Some(now);
                sent.push((s, permille));
            }
        };
        for s in (0..20).step_by(2) {
            window(s, Some(if s % 4 == 0 { 50 } else { 52 }), &mut sent);
        }
        let times: Vec<u64> = sent.iter().map(|(s, _)| *s).collect();
        assert_eq!(times, [0, 4, 8, 12, 16], "every 4 s: {sent:?}");
        sent.clear();
        // A pause of 10 s: the server has forgotten the figure.
        for s in (20..30).step_by(2) {
            window(s, None, &mut sent);
        }
        window(30, Some(48), &mut sent);
        assert_eq!(sent, [(30, 48)], "at once after the pause");
        sent.clear();
        window(32, Some(0), &mut sent);
        assert_eq!(sent, [(32, 0)], "the change to 0");
        for s in (34..60).step_by(2) {
            window(s, Some(0), &mut sent);
        }
        window(60, None, &mut sent);
        window(62, Some(0), &mut sent);
        assert_eq!(sent, [(32, 0)], "a clean line stays quiet");
    }

    /// A stream that did not open is tried again 1, 2, 4 … 30 s after the
    /// failure was noticed, and the schedule starts over once nothing is
    /// missing.
    #[test]
    fn a_missing_stream_is_retried_with_backoff() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut r = StreamRetry::default();
        assert!(!r.due(t0, false));
        assert!(!r.due(t0, true), "noticed");
        let mut tries = Vec::new();
        for s in 0..120 {
            if r.due(at(s), true) {
                tries.push(s);
            }
        }
        assert_eq!(tries, [1, 3, 7, 15, 31, 61, 91]);
        assert!(!r.due(at(120), false), "open now");
        assert!(!r.due(at(121), true));
        assert!(r.due(at(122), true), "the next failure starts at 1 s again");
    }

    /// The mixer's lead follows a graph quantum back down once the large
    /// requests have stopped for two windows.
    #[test]
    fn the_playback_lead_follows_the_quantum_down() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let callback = AtomicUsize::new(0);
        let mut r = RecentRequest::new(t0);
        callback.fetch_max(8192, Ordering::Relaxed);
        assert_eq!(r.get(&callback, at(10)), 8192);
        callback.fetch_max(256, Ordering::Relaxed);
        assert_eq!(r.get(&callback, at(2100)), 8192, "the last window's");
        callback.fetch_max(256, Ordering::Relaxed);
        assert_eq!(r.get(&callback, at(4200)), 256, "followed it down");
        callback.fetch_max(1024, Ordering::Relaxed);
        assert_eq!(r.get(&callback, at(4300)), 1024, "up again at once");
    }

    /// Audio of a session nobody announced plays at 0 % until `admit`
    /// knows whose it is.
    #[test]
    fn a_stray_speaker_is_silent() {
        let sp = Speaker::stray(960).unwrap();
        assert_eq!(mixer::volume_gain(sp.volume), 0.0);
    }

    /// A lost automatic renewal is retried on the next connect; a lost
    /// rename only unlocks the field.
    #[test]
    fn a_lost_renewal_is_retried_unless_the_user_asked_for_it() {
        assert!(retry_lost_renewal_at_once(RenewReason::Expiry));
        assert!(retry_lost_renewal_at_once(RenewReason::CatchUp));
        assert!(!retry_lost_renewal_at_once(RenewReason::UserRename));
    }

    /// The client's `SetState` budget stays under the server's bucket (20
    /// per second, burst 20): a burst of 15, then one every 67 ms — and it
    /// says when the next token is there.
    #[test]
    fn set_state_budget_stays_under_the_server() {
        assert!(SetStateBudget::PER_S < f64::from(pl::SET_STATE_PER_S));
        let t0 = Instant::now();
        let mut b = SetStateBudget::new(t0);
        for i in 0..15 {
            assert!(b.take_at(t0), "burst token {i}");
        }
        assert!(!b.take_at(t0), "the 16th in the same instant");
        let ready = b.ready_at(t0);
        assert!(ready >= t0 + Duration::from_millis(66));
        assert!(ready <= t0 + Duration::from_millis(69));
        assert!(!b.take_at(t0 + Duration::from_millis(50)));
        assert!(b.take_at(ready));
        // A long rest refills the whole burst, and no more.
        let later = ready + Duration::from_secs(5);
        for _ in 0..15 {
            assert!(b.take_at(later));
        }
        assert!(!b.take_at(later));
        assert!(same_settable(
            UserFlags(UserFlags::MUTED | UserFlags::SPEAKING),
            UserFlags(UserFlags::MUTED | UserFlags::ADMIN)
        ));
        assert!(!same_settable(
            UserFlags(UserFlags::MUTED),
            UserFlags(UserFlags::DEAFENED)
        ));
    }

    /// A slider drag commits a change every frame; the file is written once
    /// the changes are quiet, with the last value, and a quit writes what is
    /// still pending at once.
    #[tokio::test]
    async fn a_burst_of_settings_is_written_once_with_the_last_value() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let (tx, rx) = mpsc::unbounded_channel();
        let writer = tokio::spawn(settings_writer(paths.clone(), rx));
        let volume = |v: f32| {
            let mut s = Settings::default();
            s.audio.output_volume = v;
            s
        };
        for i in 0..100 {
            tx.send(volume(i as f32)).unwrap();
        }
        // Completion also includes a real filesystem flush on a blocking
        // thread. A busy native Windows guest can take longer than three
        // debounce periods; wait for the result, retaining a strict deadline.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if Settings::load(&paths).is_ok_and(|s| s.audio.output_volume == 99.0) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the debounced write must finish");
        assert_eq!(Settings::load(&paths).unwrap().audio.output_volume, 99.0);
        tx.send(volume(7.0)).unwrap();
        drop(tx);
        assert_eq!(writer.await.unwrap(), 2, "one write per quiet burst");
        assert_eq!(Settings::load(&paths).unwrap().audio.output_volume, 7.0);
    }

    #[test]
    fn camera_pauses_while_no_peer_is_viewing() {
        assert_eq!(send_tier(VideoTier::T1, &[]), VideoTier::T4);
        let watching = [peer(1, 0), peer(2, UserFlags::VIEWING)];
        assert_eq!(send_tier(VideoTier::T1, &watching), VideoTier::T1);
        let away = [peer(1, UserFlags::CAMERA_ON), peer(2, 0)];
        assert_eq!(send_tier(VideoTier::T0, &away), VideoTier::T4);
    }

    #[test]
    fn expiry_renewal_waits_until_the_server_accepts_it() {
        let cert = |days| crate::conninfo::CertInfo {
            subject: "alice".into(),
            expires_in_days: days,
            valid_days: 365,
            ..Default::default()
        };
        // A third of 365 is 121 floored days, up to 122 left: the server opens
        // its window at 121.67.
        assert_eq!(renewal_due(&cert(121), "alice"), None);
        assert_eq!(renewal_due(&cert(120), "alice"), Some(RenewReason::Expiry));
        assert_eq!(renewal_due(&cert(-400), "alice"), Some(RenewReason::Expiry));
        // Short lifetimes keep the 30-day floor.
        let short = |days| crate::conninfo::CertInfo {
            valid_days: 20,
            ..cert(days)
        };
        assert_eq!(renewal_due(&short(30), "alice"), None);
        assert_eq!(renewal_due(&short(19), "alice"), Some(RenewReason::Expiry));
        assert_eq!(renewal_due(&cert(200), "bob"), Some(RenewReason::CatchUp));
    }
}
