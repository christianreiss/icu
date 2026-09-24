//! The engine: connection loop, DSP thread, playout mixer, state and command
//! bus (`Client/IMPLEMENTATION.md` §4, §5, §6).
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
        atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use icu_proto::{
    error as codes, header::AUDIO_FLAG_END, limits as pl, AudioHeader, ControlMsg, Session,
    UserFlags, UserId, UserInfo,
};
use ringbuf::{
    traits::{Consumer, Observer, Producer, Split},
    HeapCons, HeapProd, HeapRb,
};
use tokio::sync::{mpsc, watch};

use crate::{
    audio::{AudioBackend, BackendEvent, DeviceInfo, StreamKind},
    config::{Identity, ModeSetting, Paths, Settings},
    dsp::{
        aec::{Aec, AecStats},
        denoise::Denoiser,
        gate::{Decision, Gate, Mode},
        jitter::{JitterBuffer, JitterConfig, Packet, Pop},
        meter::{self, Level},
        mixer, opus,
        refalign::{self, CycleClock},
        vad, vad_auto,
    },
    net::{
        conn::{self, ConnectError},
        NetError,
    },
    netcond::{NetworkTracker, Sample},
    quality::QualityController,
    stats::DebugStats,
    video::VideoTier,
};

/// Samples per DSP block (10 ms).
pub const BLOCK: usize = 480;

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
            client_string: format!("icu/{} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS),
        }
    }
}

/// Connection status for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnStatus {
    /// Not connected, will retry after `retry_in`.
    Disconnected {
        /// Why.
        reason: String,
        /// Attempt counter.
        attempt: u32,
    },
    /// Handshake or waiting for `Welcome`.
    Connecting,
    /// `Welcome` received.
    Connected,
    /// Fatal close; no reconnect.
    Fatal(String),
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
    /// first `ControlMsg::UserRtt` arrives (`PROTOCOL.md` §2.2).
    pub rtt_ms: Option<u16>,
    /// What this client loses of *this* peer's audio, per mille, as the
    /// jitter buffer last measured it. Already computed for `LossReport`;
    /// kept here so the Verbindung window can show it per participant.
    pub recv_loss_permille: Option<u16>,
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
    /// Local mute.
    pub muted: bool,
    /// Local deafen.
    pub deafened: bool,
    /// PTT or VAD.
    pub mode: Mode,
    /// PTT key held.
    pub ptt_down: bool,
    /// Gate open (audio is going out).
    pub sending: bool,
    /// Microphone level after processing.
    pub level: Level,
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
    /// Non-modal banner.
    pub banner: Option<String>,
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
    /// Rejection of the most recent user-manager action (promote/demote/
    /// disable/enable/revoke) — kept separate from `invite_error` so the two
    /// admin features never clobber each other's error slot.
    pub user_action_error: Option<String>,
    /// Devices.
    pub devices: Vec<DeviceInfo>,
    /// Backend name.
    pub backend: &'static str,
    /// AEC could be created.
    pub aec_available: bool,
    /// AEC statistics.
    pub aec_stats: AecStats,
    /// The AEC reference stream is open (false on backends without one).
    pub aec_reference: bool,
    /// Echo is audible in the far-end signal but the AEC removes almost
    /// none of it for 10 s: the reference is probably the wrong device.
    pub aec_suspect: bool,
    /// Camera state.
    pub camera_on: bool,
    /// Overlay state.
    pub overlay_on: bool,
    /// Hotkey backend in use.
    pub hotkey_backend: String,
    /// Settings as the engine holds them.
    pub settings: Settings,
}

/// Commands from UI, tray and hotkeys.
#[derive(Debug, Clone)]
pub enum Command {
    /// Mute/unmute.
    SetMuted(bool),
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
    /// the name lives in the certificate (`PROTOCOL.md` §2.2).
    RequestRename(String),
    /// Replace the whole identity (passport import): persist it, then
    /// reconnect with it.
    ReplaceIdentity(Box<Identity>),
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
    /// Replace the settings (device change, thresholds, …); effective at once.
    UpdateSettings(Box<Settings>),
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
    /// Stop everything.
    Quit,
}

/// Atomics the DSP thread reads every block.
#[derive(Debug)]
pub struct DspControl {
    muted: AtomicBool,
    ptt_down: AtomicBool,
    vad_mode: AtomicBool,
    denoise: AtomicBool,
    aec: AtomicBool,
    vad_threshold_bits: AtomicU32,
    vad_auto: AtomicBool,
    vad_effective_threshold_bits: AtomicU32,
    gain_bits: AtomicU32,
    release_ms: AtomicU32,
    hangover_ms: AtomicU32,
    frame_samples: AtomicU32,
    loss_permille: AtomicU32,
    level_peak_bits: AtomicU32,
    level_rms_bits: AtomicU32,
    vad_prob_bits: AtomicU32,
    sending: AtomicBool,
    aec_available: AtomicBool,
    reinit_aec: AtomicBool,
    stop: AtomicBool,
    test_tone: AtomicBool,
}

impl DspControl {
    /// Builds the control block and derives every settings-driven field
    /// through [`DspControl::apply_settings`], so the two never drift.
    fn from_settings(s: &Settings, test_tone: bool) -> DspControl {
        let ctl = DspControl {
            muted: AtomicBool::new(false),
            ptt_down: AtomicBool::new(false),
            vad_mode: AtomicBool::new(false),
            denoise: AtomicBool::new(false),
            aec: AtomicBool::new(false),
            vad_threshold_bits: AtomicU32::new(0),
            vad_auto: AtomicBool::new(false),
            vad_effective_threshold_bits: AtomicU32::new(0),
            gain_bits: AtomicU32::new(0),
            release_ms: AtomicU32::new(0),
            hangover_ms: AtomicU32::new(0),
            frame_samples: AtomicU32::new(0),
            loss_permille: AtomicU32::new(0),
            level_peak_bits: AtomicU32::new((-100.0f32).to_bits()),
            level_rms_bits: AtomicU32::new((-100.0f32).to_bits()),
            vad_prob_bits: AtomicU32::new(0),
            sending: AtomicBool::new(false),
            aec_available: AtomicBool::new(false),
            reinit_aec: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            test_tone: AtomicBool::new(test_tone),
        };
        ctl.apply_settings(s);
        ctl
    }

    fn apply_settings(&self, s: &Settings) {
        let test_tone = self.test_tone.load(Ordering::Relaxed);
        self.vad_mode
            .store(s.audio.mode == ModeSetting::Vad, Ordering::Relaxed);
        self.denoise
            .store(s.audio.denoise && !test_tone, Ordering::Relaxed);
        self.aec.store(s.audio.aec && !test_tone, Ordering::Relaxed);
        self.vad_threshold_bits
            .store(s.audio.vad_threshold.to_bits(), Ordering::Relaxed);
        self.vad_auto.store(s.audio.vad_auto, Ordering::Relaxed);
        self.gain_bits
            .store(meter::from_db(s.audio.gain_db).to_bits(), Ordering::Relaxed);
        self.release_ms.store(s.audio.release_ms, Ordering::Relaxed);
        self.hangover_ms
            .store(s.audio.hangover_ms, Ordering::Relaxed);
        self.frame_samples.store(
            if s.audio.frame_ms == 10 { 480 } else { 960 },
            Ordering::Relaxed,
        );
    }

    /// Current level.
    pub fn level(&self) -> Level {
        Level {
            peak_db: f32::from_bits(self.level_peak_bits.load(Ordering::Relaxed)),
            rms_db: f32::from_bits(self.level_rms_bits.load(Ordering::Relaxed)),
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
    /// Loss accounting for `LossReport`: (played, lost) at the last report.
    report_base: (u64, u64),
    last_reported_permille: u16,
    loss_hinted: bool,
}

impl Speaker {
    fn new(user_id: UserId, volume: f32, frame_samples: u32) -> Result<Speaker, ::opus::Error> {
        Ok(Speaker {
            user_id,
            jitter: JitterBuffer::new(JitterConfig {
                frame_samples,
                ..JitterConfig::default()
            }),
            decoder: opus::Decoder::new()?,
            pending: VecDeque::with_capacity(4 * BLOCK),
            last_packet: None,
            volume,
            report_base: (0, 0),
            last_reported_permille: 0,
            loss_hinted: false,
        })
    }
}

/// Shared between all engine threads.
struct Shared {
    state: watch::Sender<AppState>,
    dsp: Arc<DspControl>,
    stats: Arc<DebugStats>,
    speakers: Mutex<HashMap<Session, Speaker>>,
    /// The live connection for `send_datagram` from the DSP thread.
    conn: Mutex<Option<quinn::Connection>>,
    /// Session → user_id map for volumes and video.
    volumes: Mutex<HashMap<UserId, f32>>,
    deafened: AtomicBool,
    output_gain_bits: AtomicU32,
    cues: Mutex<VecDeque<f32>>,
    cue_settings: Mutex<(bool, bool, bool)>,
    playback_open: Arc<AtomicBool>,
    /// The AEC reference stream is open.
    reference_open: AtomicBool,
    /// Push times of the capture and reference callbacks, so the DSP thread
    /// pairs blocks from the same graph cycle (`dsp::refalign`).
    cycle: Arc<CycleClock>,
    /// Requests an IDR from the camera thread.
    keyframe_request: AtomicBool,
    /// Video frames received, keyed by session (M4).
    video: Mutex<crate::video::Receivers>,
    /// Video sender (M4).
    video_tx: Mutex<Option<crate::video::Sender>>,
    /// Ring buffer ends the backend callbacks write/read. Behind a mutex so
    /// streams can be reopened with a fresh closure; the callbacks only
    /// `try_lock`, which is uncontended in practice.
    cap_prod: Arc<Mutex<HeapProd<f32>>>,
    ref_prod: Arc<Mutex<HeapProd<f32>>>,
    play_cons: Arc<Mutex<HeapCons<f32>>>,
    /// Encoder loss percent mirrored for the tests.
    loss_perc: AtomicI64,
    /// Detected network condition (RTT, loss, congestion pressure); folded
    /// from quinn's own connection stats plus `LossHint`/
    /// `VideoCongestionHint` (`crate::netcond`).
    net: Mutex<NetworkTracker>,
    /// Outgoing camera quality tier, published by the `QualityController`
    /// and read once per iteration by the camera thread (`crate::quality`,
    /// `crate::camera::run_sender`). Video absorbs all of the network
    /// adaptation; this atomic is the only channel for it.
    video_tier: AtomicU8,
}

/// A running engine.
pub struct Engine {
    shared: Arc<Shared>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    state_rx: watch::Receiver<AppState>,
    runtime: Option<tokio::runtime::Runtime>,
    threads: Vec<std::thread::JoinHandle<()>>,
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
            muted: false,
            deafened: false,
            mode: if settings.audio.mode == ModeSetting::Vad {
                Mode::Vad
            } else {
                Mode::Ptt
            },
            ptt_down: false,
            sending: false,
            level: Level::default(),
            vad_prob: 0.0,
            vad_threshold: settings.audio.vad_threshold,
            rtt_ms: None,
            server_rtt_ms: None,
            clock_offset_ms: None,
            conn_info: None,
            conn_stats: None,
            update_required: None,
            banner: None,
            name_error: None,
            rename_pending: false,
            is_admin: false,
            last_invite: None,
            invite_error: None,
            user_list: Vec::new(),
            user_action_error: None,
            devices: backend.devices(),
            backend: backend_name,
            aec_available: false,
            aec_stats: AecStats::default(),
            aec_reference: false,
            aec_suspect: false,
            camera_on: false,
            overlay_on: settings.overlay.enabled,
            hotkey_backend: String::new(),
            settings: settings.clone(),
        };
        let (state_tx, state_rx) = watch::channel(initial);
        let mut volumes = HashMap::new();
        for (k, v) in &settings.volumes {
            if let Ok(id) = k.parse::<UserId>() {
                volumes.insert(id, *v);
            }
        }
        // Ring buffers between backend callbacks and the DSP/mixer threads.
        let (cap_prod, cap_cons) = HeapRb::<f32>::new(48_000).split();
        let (ref_prod, ref_cons) = HeapRb::<f32>::new(48_000).split();
        let (play_prod, play_cons) = HeapRb::<f32>::new(8 * BLOCK).split();

        let shared = Arc::new(Shared {
            state: state_tx,
            dsp: dsp.clone(),
            stats: stats.clone(),
            speakers: Mutex::new(HashMap::new()),
            conn: Mutex::new(None),
            volumes: Mutex::new(volumes),
            deafened: AtomicBool::new(false),
            output_gain_bits: AtomicU32::new(
                mixer::volume_gain(settings.audio.output_volume).to_bits(),
            ),
            cues: Mutex::new(VecDeque::new()),
            cue_settings: Mutex::new((
                settings.audio.cue_join,
                settings.audio.cue_tx,
                settings.audio.cue_mute,
            )),
            playback_open: Arc::new(AtomicBool::new(false)),
            reference_open: AtomicBool::new(false),
            cycle: Arc::new(CycleClock::default()),
            keyframe_request: AtomicBool::new(false),
            video: Mutex::new(crate::video::Receivers::default()),
            video_tx: Mutex::new(None),
            cap_prod: Arc::new(Mutex::new(cap_prod)),
            ref_prod: Arc::new(Mutex::new(ref_prod)),
            play_cons: Arc::new(Mutex::new(play_cons)),
            loss_perc: AtomicI64::new(0),
            net: Mutex::new(NetworkTracker::new()),
            video_tier: AtomicU8::new(VideoTier::T0.as_u8()),
        });

        open_streams(&mut *backend, &settings, &shared);
        let backend = Arc::new(Mutex::new(backend));

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
        let mut threads = Vec::new();

        // DSP thread.
        {
            let shared = shared.clone();
            let test_tone = cfg.test_tone;
            threads.push(
                std::thread::Builder::new()
                    .name("icu-dsp".into())
                    .spawn(move || dsp_thread(shared, cap_cons, ref_cons, test_tone))
                    .map_err(|e| EngineError(e.to_string()))?,
            );
        }
        // Mixer thread.
        {
            let shared = shared.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("icu-mixer".into())
                    .spawn(move || mixer_thread(shared, play_prod))
                    .map_err(|e| EngineError(e.to_string()))?,
            );
        }
        // Backend events → state. Detached: it ends when the backend (and
        // with it the event sender) is dropped.
        if let Some(events) = events {
            let shared = shared.clone();
            let backend = backend.clone();
            std::thread::Builder::new()
                .name("icu-audio-events".into())
                .spawn(move || {
                    while let Ok(ev) = events.recv() {
                        match ev {
                            BackendEvent::DevicesChanged(list) => {
                                shared.state.send_modify(|s| s.devices = list);
                            }
                            BackendEvent::StreamError { stream, message } => {
                                tracing::warn!(?stream, %message, "audio stream error");
                                shared.state.send_modify(|s| {
                                    s.banner = Some(format!("Audio {stream:?}: {message}"))
                                });
                                let _ = backend.lock().map(|mut b| b.close(stream));
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
        {
            let shared = shared.clone();
            let backend = backend.clone();
            runtime.spawn(net_task(cfg, shared, cmd_rx, backend));
        }

        Ok(Engine {
            shared,
            cmd_tx,
            state_rx,
            runtime: Some(runtime),
            threads,
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
        self.shared.loss_perc.load(Ordering::Relaxed)
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

    /// Stops everything.
    pub fn shutdown(mut self) {
        let _ = self.cmd_tx.send(Command::Quit);
        self.shared.dsp.stop.store(true, Ordering::Relaxed);
        if let Some(rt) = self.runtime.take() {
            // Give the session loop a moment to send its close, then drop the
            // runtime without blocking (we may be inside another runtime).
            std::thread::sleep(Duration::from_millis(100));
            rt.shutdown_background();
        }
        if let Ok(mut b) = self.backend.lock() {
            b.close(StreamKind::Capture);
            b.close(StreamKind::Reference);
            b.close(StreamKind::Playback);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.dsp.stop.store(true, Ordering::Relaxed);
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}

fn open_streams(backend: &mut dyn AudioBackend, settings: &Settings, shared: &Arc<Shared>) {
    let stats = shared.stats.clone();
    let cap_prod = shared.cap_prod.clone();
    let cycle = shared.cycle.clone();
    if let Err(e) = backend.open_capture(
        settings.audio.input_device.as_deref(),
        Box::new(move |block| {
            if let Ok(mut p) = cap_prod.try_lock() {
                let n = p.push_slice(block);
                if n < block.len() {
                    DebugStats::inc(&stats.capture_overruns);
                }
            }
            cycle.capture_pushed(block.len());
        }),
    ) {
        tracing::warn!(error = %e, "cannot open capture");
        shared
            .state
            .send_modify(|s| s.banner = Some(format!("Mikrofon: {e}")));
    }
    let mut reference_open = false;
    if settings.audio.aec {
        let ref_prod = shared.ref_prod.clone();
        let cycle = shared.cycle.clone();
        match backend.open_reference(
            settings.audio.output_device.as_deref(),
            Box::new(move |block| {
                if let Ok(mut p) = ref_prod.try_lock() {
                    let _ = p.push_slice(block);
                }
                cycle.reference_pushed();
            }),
        ) {
            Ok(()) => reference_open = true,
            Err(e) => tracing::warn!(error = %e, "cannot open AEC reference"),
        }
    }
    shared
        .reference_open
        .store(reference_open, Ordering::Relaxed);
    shared
        .state
        .send_modify(|s| s.aec_reference = reference_open);
    let play_cons = shared.play_cons.clone();
    match backend.open_playback(
        settings.audio.output_device.as_deref(),
        Box::new(move |out| {
            let n = match play_cons.try_lock() {
                Ok(mut c) => c.pop_slice(out),
                Err(_) => 0,
            };
            out[n..].fill(0.0);
        }),
    ) {
        Ok(()) => shared.playback_open.store(true, Ordering::Relaxed),
        Err(e) => {
            shared.playback_open.store(false, Ordering::Relaxed);
            tracing::warn!(error = %e, "cannot open playback");
            shared
                .state
                .send_modify(|s| s.banner = Some(format!("Wiedergabe: {e}")));
        }
    }
}

// ---------------------------------------------------------------------------
// DSP thread
// ---------------------------------------------------------------------------

fn dsp_thread(
    shared: Arc<Shared>,
    mut cap: HeapCons<f32>,
    mut reference: HeapCons<f32>,
    test_tone: bool,
) {
    let ctl = shared.dsp.clone();
    let stats = shared.stats.clone();
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
    let mut denoiser = Denoiser::new();
    let mut encoder = match opus::Encoder::new() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(%e, "opus encoder");
            return;
        }
    };
    let mut auto_threshold = vad_auto::AutoThreshold::new(Instant::now());
    let mut gate = Gate::new(
        if ctl.vad_mode.load(Ordering::Relaxed) {
            Mode::Vad
        } else {
            Mode::Ptt
        },
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
    let mut ref_block = vec![0.0f32; BLOCK];
    let mut frame_acc: Vec<f32> = Vec::with_capacity(960);
    let mut seq: u16 = 0;
    let mut ts: u32 = 0;
    let mut voice_in_frame = false;
    let mut ref_underruns_window = 0u32;
    let mut window_start = Instant::now();
    let mut last_state_push = Instant::now();
    let mut last_aec_stats = Instant::now();
    // Reference energy since the last stats push, and how many pushes in a
    // row looked like "echo present, not removed".
    let mut ref_energy = 0.0f64;
    let mut ref_samples = 0usize;
    let mut suspect_streak = 0u32;
    let mut loss_applied = 0u32;
    let start = Instant::now();
    let mut tone_ticks: u64 = 0;

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
            ref_block.fill(0.0);
        } else {
            // Pair the capture block with the reference block of the same
            // graph cycle (`dsp::refalign`).
            if ctl.reinit_aec.swap(false, Ordering::Relaxed) {
                // Streams were reopened: restart both rings together so
                // their occupancy difference is the reference lag again.
                cap.clear();
                reference.clear();
                if let Some(a) = aec.as_mut() {
                    a.reinitialize();
                }
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
                            ref_block.fill(0.0);
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
                        ref_block.fill(0.0);
                        if underrun {
                            DebugStats::inc(&stats.reference_underruns);
                            ref_underruns_window += 1;
                        }
                        break;
                    }
                }
            }
        }
        let t_block = Instant::now();
        DebugStats::inc(&stats.dsp_blocks);

        // 2. AEC: reference first, then capture.
        let aec_on =
            ctl.aec.load(Ordering::Relaxed) && shared.reference_open.load(Ordering::Relaxed);
        if let Some(a) = aec.as_mut() {
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
                    .map(|&x| (x as f64) * (x as f64))
                    .sum::<f64>();
                ref_samples += BLOCK;
                a.process(&ref_block, &mut block);
                if last_aec_stats.elapsed() > Duration::from_secs(2) {
                    last_aec_stats = Instant::now();
                    let st = a.stats();
                    let ref_db = 10.0 * (ref_energy / ref_samples.max(1) as f64 + 1e-12).log10();
                    ref_energy = 0.0;
                    ref_samples = 0;
                    // Self-diagnosis (`Research/aec.md`): the far end plays,
                    // the mic hears it loudly (low ERL), yet ERLE stays
                    // below 6 dB for 10 s.
                    let suspect = ref_db > -50.0
                        && matches!((st.erl_db, st.erle_db), (Some(erl), Some(erle)) if erl < 15.0 && erle < 6.0);
                    suspect_streak = if suspect { suspect_streak + 1 } else { 0 };
                    let flag = suspect_streak >= 5;
                    shared.state.send_modify(|s| {
                        s.aec_stats = st;
                        s.aec_suspect = flag;
                    });
                }
            }
        }

        // 3. Gain, denoise, VAD.
        let gain = f32::from_bits(ctl.gain_bits.load(Ordering::Relaxed));
        if gain != 1.0 {
            for s in block.iter_mut() {
                *s *= gain;
            }
        }
        let prob = if ctl.denoise.load(Ordering::Relaxed) {
            Some(denoiser.process(&mut block))
        } else {
            None
        };
        let level = meter::measure(&block);
        ctl.level_peak_bits
            .store(level.peak_db.to_bits(), Ordering::Relaxed);
        ctl.level_rms_bits
            .store(level.rms_db.to_bits(), Ordering::Relaxed);
        ctl.vad_prob_bits
            .store(prob.unwrap_or(0.0).to_bits(), Ordering::Relaxed);
        let threshold = if ctl.vad_auto.load(Ordering::Relaxed) {
            if let Some(p) = prob {
                auto_threshold.update(t_block, gate.is_open(), p);
            }
            auto_threshold.threshold()
        } else {
            f32::from_bits(ctl.vad_threshold_bits.load(Ordering::Relaxed))
        };
        ctl.vad_effective_threshold_bits
            .store(threshold.to_bits(), Ordering::Relaxed);
        let voice = vad::is_voice(vad::VadSource::Rnnoise { threshold }, prob, level);
        voice_in_frame |= voice;

        // 4. Accumulate to an Opus frame.
        frame_acc.extend_from_slice(&block);
        let frame_samples = ctl.frame_samples.load(Ordering::Relaxed) as usize;
        if frame_acc.len() >= frame_samples {
            let now = Instant::now();
            gate.set_mode(if ctl.vad_mode.load(Ordering::Relaxed) {
                Mode::Vad
            } else {
                Mode::Ptt
            });
            gate.set_release(Duration::from_millis(
                ctl.release_ms.load(Ordering::Relaxed) as u64,
            ));
            gate.set_hangover(Duration::from_millis(
                ctl.hangover_ms.load(Ordering::Relaxed) as u64,
            ));
            gate.set_ptt(ctl.ptt_down.load(Ordering::Relaxed) || test_tone);
            gate.set_muted(ctl.muted.load(Ordering::Relaxed));
            let decision = gate.update(now, voice_in_frame);
            voice_in_frame = false;

            // LossHint → encoder.
            let permille = ctl.loss_permille.load(Ordering::Relaxed);
            if permille != loss_applied {
                loss_applied = permille;
                let _ = encoder.set_loss_permille(permille as u16);
                shared
                    .loss_perc
                    .store(encoder.loss_perc() as i64, Ordering::Relaxed);
                stats
                    .packet_loss_perc
                    .store(encoder.loss_perc() as i64, Ordering::Relaxed);
            }

            let sending = decision != Decision::Silent;
            ctl.sending.store(sending, Ordering::Relaxed);
            if sending {
                match encoder.encode(&frame_acc[..frame_samples]) {
                    Ok(packet) => {
                        let header = AudioHeader {
                            flags: if decision == Decision::SendEnd {
                                AUDIO_FLAG_END
                            } else {
                                0
                            },
                            speaker: 0,
                            seq,
                            ts,
                        };
                        if let Ok(datagram) = header.datagram(packet) {
                            // `into()` moves the buffer, so take its length
                            // first; it is only counted once the send is
                            // accepted.
                            let wire_bytes = datagram.len() as u64;
                            let conn = shared.conn.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(c) = conn.as_ref() {
                                if c.send_datagram(datagram.into()).is_ok() {
                                    DebugStats::inc(&stats.datagrams_sent);
                                    DebugStats::add(&stats.audio_bytes_sent, wire_bytes);
                                    stats
                                        .capture_to_send
                                        .lock()
                                        .unwrap()
                                        .record(t_block.elapsed());
                                }
                            }
                        }
                        seq = seq.wrapping_add(1);
                    }
                    Err(e) => tracing::warn!(%e, "opus encode"),
                }
            }
            ts = ts.wrapping_add(frame_samples as u32);
            frame_acc.drain(..frame_samples);
        }

        stats.dsp_block.lock().unwrap().record(t_block.elapsed());

        // 5. Push level/sending into the UI state at ~20 Hz.
        if last_state_push.elapsed() > Duration::from_millis(50) {
            last_state_push = Instant::now();
            let sending = ctl.sending.load(Ordering::Relaxed);
            let ptt = ctl.ptt_down.load(Ordering::Relaxed);
            let p = f32::from_bits(ctl.vad_prob_bits.load(Ordering::Relaxed));
            let vad_threshold =
                f32::from_bits(ctl.vad_effective_threshold_bits.load(Ordering::Relaxed));
            shared.state.send_modify(|s| {
                s.level = level;
                s.vad_prob = p;
                s.vad_threshold = vad_threshold;
                s.sending = sending;
                s.ptt_down = ptt;
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Mixer thread
// ---------------------------------------------------------------------------

fn mixer_thread(shared: Arc<Shared>, mut play: HeapProd<f32>) {
    let stats = shared.stats.clone();
    let mut out = vec![0.0f32; BLOCK];
    let mut inputs: Vec<(Vec<f32>, f32)> = Vec::new();
    let mut last_indicator = Instant::now();
    let mut stalled_since: Option<Instant> = None;
    // 120 ms, the longest frame Opus allows: a peer picks its frame length.
    let mut fec_scratch = vec![0.0f32; 5760];
    loop {
        if shared.dsp.stop.load(Ordering::Relaxed) {
            return;
        }
        // Keep two blocks (20 ms) ahead of the playback callback.
        if play.occupied_len() >= 2 * BLOCK {
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
        inputs.clear();
        {
            let mut speakers = shared.speakers.lock().unwrap_or_else(|e| e.into_inner());
            for sp in speakers.values_mut() {
                while sp.pending.len() < BLOCK {
                    match sp.jitter.pop(now) {
                        Pop::Packet(p) => {
                            if let Some(d) = sp.jitter.last_delay() {
                                stats.jitter_delay.lock().unwrap().record(d);
                            }
                            let n = sp.decoder.decode(&p.payload, &mut fec_scratch).unwrap_or(0);
                            sp.pending.extend(fec_scratch[..n].iter().copied());
                            DebugStats::inc(&stats.frames_decoded);
                            if n == 0 {
                                break;
                            }
                        }
                        Pop::Lost { fec_source, .. } => {
                            let n = match fec_source {
                                Some(next) => {
                                    DebugStats::inc(&stats.frames_fec);
                                    sp.decoder
                                        .decode_fec(&next.payload, &mut fec_scratch)
                                        .unwrap_or(0)
                                }
                                None => {
                                    DebugStats::inc(&stats.frames_plc);
                                    sp.decoder.conceal(&mut fec_scratch).unwrap_or(0)
                                }
                            };
                            if n == 0 {
                                sp.pending.extend(std::iter::repeat_n(0.0, BLOCK));
                            } else {
                                sp.pending.extend(fec_scratch[..n].iter().copied());
                            }
                        }
                        Pop::Silence => {
                            sp.pending.extend(std::iter::repeat_n(0.0, BLOCK));
                        }
                    }
                }
                let mut buf = vec![0.0f32; BLOCK];
                for s in buf.iter_mut() {
                    *s = sp.pending.pop_front().unwrap_or(0.0);
                }
                inputs.push((buf, mixer::volume_gain(sp.volume)));
            }
        }
        let deafened = shared.deafened.load(Ordering::Relaxed);
        let master = f32::from_bits(shared.output_gain_bits.load(Ordering::Relaxed));
        let mut refs: Vec<(&[f32], f32)> = inputs
            .iter()
            .map(|(b, g)| (b.as_slice(), if deafened { 0.0 } else { g * master }))
            .collect();
        // Cues are heard even when deafened.
        let mut cue_buf = vec![0.0f32; BLOCK];
        {
            let mut cues = shared.cues.lock().unwrap();
            if !cues.is_empty() {
                for s in cue_buf.iter_mut() {
                    *s = cues.pop_front().unwrap_or(0.0);
                }
                refs.push((cue_buf.as_slice(), 1.0));
            }
        }
        mixer::mix(&refs, &mut out);
        if play.occupied_len() < 2 * BLOCK {
            let n = play.push_slice(&out);
            DebugStats::add(&stats.samples_played, n as u64);
        }

        // Speaking indicator from the local datagram path, 10 Hz.
        if last_indicator.elapsed() > Duration::from_millis(100) {
            last_indicator = Instant::now();
            let speaking: Vec<(Session, bool)> = {
                let speakers = shared.speakers.lock().unwrap_or_else(|e| e.into_inner());
                speakers
                    .iter()
                    .map(|(s, sp)| {
                        (
                            *s,
                            sp.last_packet
                                .map(|t| t.elapsed() < Duration::from_millis(300))
                                .unwrap_or(false),
                        )
                    })
                    .collect()
            };
            shared.state.send_if_modified(|s| {
                let mut changed = false;
                for p in s.peers.iter_mut() {
                    let local = speaking
                        .iter()
                        .find(|(sess, _)| *sess == p.info.session)
                        .map(|(_, v)| *v)
                        .unwrap_or(false);
                    let want = local || p.info.flags.speaking();
                    if p.speaking != want {
                        p.speaking = want;
                        changed = true;
                    }
                }
                changed
            });
        }
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
    let mut local = LocalState {
        flags: UserFlags::NONE,
        settings: cfg.settings.clone(),
        paths: cfg.paths.clone(),
        // The name is whatever the certificate says, so the pre-`Welcome`
        // display cannot contradict it (`PROTOCOL.md` §2.1).
        name: crate::conninfo::CertInfo::from_der(&cfg.identity.leaf_der).subject,
        backend,
        identity: cfg.identity.clone(),
        pending_renew: None,
        renew_attempted_at: None,
        window_visible: !cfg.settings.ui.start_minimized,
    };
    local.update_viewing();
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
            None => {
                crate::net::resolve(&local.identity.server.host, local.identity.server.port).await
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
            Err(e) => SessionEnd::Retry(e.to_string()),
        };
        if was_connected {
            attempt = 0;
        }
        *shared.conn.lock().unwrap_or_else(|e| e.into_inner()) = None;
        shared
            .speakers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        shared.video.lock().unwrap().clear();
        // Here, not at the end of `run_session`: that function returns early
        // from half a dozen places, and a stale `conn_info` outliving the
        // connection it describes is exactly the lie this window must not
        // tell.
        shared.state.send_modify(|s| {
            s.conn_info = None;
            s.conn_stats = None;
            s.server_rtt_ms = None;
            s.clock_offset_ms = None;
        });
        match outcome {
            SessionEnd::Quit => return,
            SessionEnd::Fatal(reason) => {
                shared.state.send_modify(|s| {
                    s.conn = ConnStatus::Fatal(reason.clone());
                    s.peers.clear();
                });
                // Keep serving local commands (mute etc.) until Quit. A new
                // identity (passport import) is the way out: reconnect with it.
                loop {
                    match cmd_rx.recv().await {
                        None | Some(Command::Quit) => return,
                        Some(cmd) => {
                            let replaced = matches!(cmd, Command::ReplaceIdentity(_));
                            local.handle_local(cmd, &shared, None).await;
                            if replaced {
                                attempt = 0;
                                break;
                            }
                        }
                    }
                }
            }
            SessionEnd::Retry(reason) => {
                let wait = conn::backoff(attempt);
                attempt += 1;
                shared.state.send_modify(|s| {
                    s.conn = ConnStatus::Disconnected {
                        reason: reason.clone(),
                        attempt,
                    };
                    s.peers.clear();
                });
                let deadline = tokio::time::Instant::now() + wait;
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep_until(deadline) => break,
                        cmd = cmd_rx.recv() => match cmd {
                            Some(Command::Quit) | None => return,
                            Some(cmd) => {
                                // A new identity is worth trying right away.
                                let replaced = matches!(cmd, Command::ReplaceIdentity(_));
                                local.handle_local(cmd, &shared, None).await;
                                if replaced {
                                    attempt = 0;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

enum SessionEnd {
    Quit,
    Fatal(String),
    Retry(String),
}

struct LocalState {
    flags: UserFlags,
    settings: Settings,
    paths: Paths,
    name: String,
    backend: Arc<Mutex<Box<dyn AudioBackend>>>,
    /// The identity in force. Lives here and not in `EngineConfig` because a
    /// renewal replaces the leaf mid-run and the next reconnect has to use the
    /// new one; `LocalState` is the only thing that survives a session.
    identity: Identity,
    /// The renewal this client is waiting for, if any.
    pending_renew: Option<PendingRenew>,
    /// When the last automatic renewal was sent. The server allows one a
    /// day, so the next try waits that long.
    renew_attempted_at: Option<Instant>,
    /// Whether the main window is on screen. Starts from
    /// `start_minimized`, so a client without a window (tests) views.
    window_visible: bool,
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
    cn: &str,
    reason: RenewReason,
) -> Result<(), RenewSendError> {
    // A new name has to meet today's rules; the stored one is carried over
    // as it is, or a name from before them could never be renewed.
    let checked = match reason {
        RenewReason::UserRename => icu_proto::validate_name(cn),
        RenewReason::CatchUp | RenewReason::Expiry => icu_proto::validate_stored_name(cn),
    };
    checked.map_err(|e| RenewSendError::Local(e.to_string()))?;
    let key = rcgen::KeyPair::try_from(local.identity.key_der.as_slice())
        .map_err(|e| RenewSendError::Local(format!("eigener Schlüssel unlesbar: {e}")))?;
    let csr_der =
        crate::net::register::csr_for(cn, &key).map_err(|e| RenewSendError::Local(e.to_string()))?;
    if conn::write_msg(send, &ControlMsg::RenewRequest { csr_der })
        .await
        .is_err()
    {
        return Err(RenewSendError::Transport);
    }
    local.pending_renew = Some(PendingRenew {
        cn: cn.to_string(),
        reason,
    });
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
    // `expires_in_days` is floored: 30 means up to 31 days left, which the
    // server still refuses (`PROTOCOL.md` §2.2 Nr. 9).
    if cert.expires_in_days < RENEW_BEFORE_EXPIRY_DAYS {
        return Some(RenewReason::Expiry);
    }
    None
}

/// Renew this many days before the certificate expires
/// (`Client/IMPLEMENTATION.md` §5).
const RENEW_BEFORE_EXPIRY_DAYS: i64 = 30;

/// How long an automatic renewal waits after the previous one, matching the
/// server's one per day.
const AUTO_RENEW_RETRY: Duration = Duration::from_secs(24 * 3600);

/// Sends the automatic renewal if one is due and none was tried within
/// [`AUTO_RENEW_RETRY`]: catch-up after a rename on another device, or the
/// last 30 days before expiry. `stored` is the name the server holds. Silent:
/// no banner, no `name_error`, and `rename_pending` stays false — this is not
/// something the user asked for. `Err` only when the control stream broke.
async fn maybe_auto_renew(
    send: &mut quinn::SendStream,
    local: &mut LocalState,
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
    match send_renew(send, local, stored, reason).await {
        Ok(()) => Ok(()),
        Err(RenewSendError::Local(e)) => {
            tracing::warn!(error = %e, ?reason, "automatic renewal not possible");
            Ok(())
        }
        Err(RenewSendError::Transport) => Err(()),
    }
}

impl LocalState {
    /// Recomputes `UserFlags::VIEWING`: overlay on or main window visible.
    /// Returns `true` when the bit changed and `SetState` is due.
    fn update_viewing(&mut self) -> bool {
        let viewing = self.settings.overlay.enabled || self.window_visible;
        let changed = self.flags.viewing() != viewing;
        self.flags = self.flags.with_bit(UserFlags::VIEWING, viewing);
        changed
    }

    /// Commands that do not need a connection. Returns `true` if `SetState`
    /// should be sent.
    async fn handle_local(
        &mut self,
        cmd: Command,
        shared: &Arc<Shared>,
        send: Option<&mut quinn::SendStream>,
    ) -> bool {
        let mut set_state = false;
        match cmd {
            Command::SetMuted(m) => {
                self.flags = self.flags.with_bit(UserFlags::MUTED, m);
                shared.dsp.muted.store(m, Ordering::Relaxed);
                shared.state.send_modify(|s| s.muted = m);
                self.cue(shared, if m { Cue::Mute } else { Cue::Unmute });
                set_state = true;
            }
            Command::ToggleMute => {
                let m = !self.flags.muted();
                return Box::pin(self.handle_local(Command::SetMuted(m), shared, send)).await;
            }
            Command::SetDeafened(d) => {
                self.flags = self.flags.with_bit(UserFlags::DEAFENED, d);
                if d {
                    self.flags = self.flags.with_bit(UserFlags::MUTED, true);
                    shared.dsp.muted.store(true, Ordering::Relaxed);
                }
                shared.deafened.store(d, Ordering::Relaxed);
                shared.state.send_modify(|s| {
                    s.deafened = d;
                    if d {
                        s.muted = true;
                    }
                });
                self.cue(shared, if d { Cue::Mute } else { Cue::Unmute });
                set_state = true;
            }
            Command::ToggleDeafen => {
                let d = !self.flags.deafened();
                return Box::pin(self.handle_local(Command::SetDeafened(d), shared, send)).await;
            }
            Command::SetMode(mode) => {
                shared
                    .dsp
                    .vad_mode
                    .store(mode == Mode::Vad, Ordering::Relaxed);
                self.settings.audio.mode = match mode {
                    Mode::Vad => ModeSetting::Vad,
                    Mode::Ptt => ModeSetting::Ptt,
                };
                let settings = self.settings.clone();
                shared.state.send_modify(|s| {
                    s.mode = mode;
                    s.settings = settings;
                });
                let _ = self.settings.save(&self.paths);
            }
            Command::ToggleMode => {
                let mode = if shared.dsp.vad_mode.load(Ordering::Relaxed) {
                    Mode::Ptt
                } else {
                    Mode::Vad
                };
                return Box::pin(self.handle_local(Command::SetMode(mode), shared, send)).await;
            }
            Command::Ptt(down) => {
                tracing::debug!(down, "ptt");
                shared.dsp.ptt_down.store(down, Ordering::Relaxed);
                shared.state.send_modify(|s| s.ptt_down = down);
            }
            Command::SetVolume(user_id, percent) => {
                let percent = self.settings.set_volume(user_id, percent);
                shared.volumes.lock().unwrap().insert(user_id, percent);
                for sp in shared.speakers.lock().unwrap().values_mut() {
                    if sp.user_id == user_id {
                        sp.volume = percent;
                    }
                }
                let settings = self.settings.clone();
                shared.state.send_modify(|s| {
                    for p in s.peers.iter_mut() {
                        if p.info.user_id == user_id {
                            p.volume = percent;
                        }
                    }
                    s.settings = settings;
                });
                let _ = self.settings.save(&self.paths);
            }
            Command::UpdateSettings(new) => {
                let old = self.settings.clone();
                self.settings = *new;
                shared.dsp.apply_settings(&self.settings);
                shared.output_gain_bits.store(
                    mixer::volume_gain(self.settings.audio.output_volume).to_bits(),
                    Ordering::Relaxed,
                );
                *shared.cue_settings.lock().unwrap() = (
                    self.settings.audio.cue_join,
                    self.settings.audio.cue_tx,
                    self.settings.audio.cue_mute,
                );
                if old.audio.input_device != self.settings.audio.input_device
                    || old.audio.output_device != self.settings.audio.output_device
                    || old.audio.aec != self.settings.audio.aec
                {
                    self.reopen_streams(shared);
                }
                let settings = self.settings.clone();
                let mode = if settings.audio.mode == ModeSetting::Vad {
                    Mode::Vad
                } else {
                    Mode::Ptt
                };
                shared.state.send_modify(|s| {
                    s.settings = settings;
                    s.mode = mode;
                    s.overlay_on = s.settings.overlay.enabled;
                });
                let _ = self.settings.save(&self.paths);
                set_state |= self.update_viewing();
            }
            Command::SetCamera(on) => {
                self.flags = self.flags.with_bit(UserFlags::CAMERA_ON, on);
                shared.state.send_modify(|s| s.camera_on = on);
                set_state = true;
            }
            Command::SetOverlay(on) => {
                self.settings.overlay.enabled = on;
                let settings = self.settings.clone();
                shared.state.send_modify(|s| {
                    s.overlay_on = on;
                    s.settings = settings;
                });
                let _ = self.settings.save(&self.paths);
                set_state = self.update_viewing();
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
            Command::RequestRename(_) => {
                shared
                    .state
                    .send_modify(|s| s.name_error = Some("Nicht verbunden".into()));
            }
            Command::ReplaceIdentity(identity) => {
                self.replace_identity(*identity, shared);
            }
            Command::CreateInvite { .. }
            | Command::SetUserAdmin { .. }
            | Command::SetUserDisabled { .. }
            | Command::RevokeUser { .. }
            | Command::RequestUserList
            | Command::Quit => {}
        }
        set_state
    }

    /// Takes over an imported identity: memory first, then disk, so a failed
    /// write cannot leave the two disagreeing about which leaf is in force.
    fn replace_identity(&mut self, identity: Identity, shared: &Arc<Shared>) {
        self.identity = identity;
        self.pending_renew = None;
        self.renew_attempted_at = None;
        self.name = crate::conninfo::CertInfo::from_der(&self.identity.leaf_der).subject;
        let saved = self.identity.save(&self.paths);
        let server = format!(
            "{}:{}",
            self.identity.server.host, self.identity.server.port
        );
        let server_name = self.identity.server.server_name.clone();
        shared.state.send_modify(|s| {
            s.server = server;
            s.server_name = server_name;
            s.name_error = None;
            s.rename_pending = false;
            if let Err(e) = &saved {
                s.banner = Some(format!("Passport konnte nicht gespeichert werden: {e}"));
            }
        });
        tracing::info!(name = %self.name, "identity replaced");
    }

    /// Takes a freshly issued leaf into use.
    ///
    /// Checked before it is written: a leaf that does not match the request or
    /// the local key would make the next `client_config` fail, and the only way
    /// back from that is a passport from another device.
    fn apply_renewed_leaf(&mut self, leaf_der: Vec<u8>, shared: &Arc<Shared>) {
        let Some(pending) = self.pending_renew.take() else {
            tracing::warn!("unsolicited RenewResponse");
            return;
        };
        let info = crate::conninfo::CertInfo::from_der(&leaf_der);
        let mismatch = if info.subject != pending.cn {
            Some(format!(
                "Server hat ein Zertifikat für {:?} ausgestellt, erwartet war {:?}",
                info.subject, pending.cn
            ))
        } else if !crate::conninfo::leaf_matches_key(&leaf_der, &self.identity.key_der) {
            Some("Neues Zertifikat passt nicht zum eigenen Schlüssel".to_string())
        } else {
            None
        };
        if let Some(detail) = mismatch {
            tracing::warn!(%detail, "renewal rejected locally, keeping the old certificate");
            let user_asked = pending.reason == RenewReason::UserRename;
            shared.state.send_modify(|s| {
                s.rename_pending = false;
                if user_asked {
                    s.name_error = Some(detail);
                }
            });
            return;
        }

        // A server that moved says so in the certificates it issues; the next
        // reconnect goes to the new address.
        self.identity
            .server
            .update_from_certs(&leaf_der, &self.identity.ca_der);
        self.identity.leaf_der = leaf_der;
        self.identity.server.name = pending.cn.clone();
        let saved = self.identity.save(&self.paths);
        self.name = pending.cn.clone();
        tracing::info!(name = %pending.cn, reason = ?pending.reason, "certificate renewed");
        shared.state.send_modify(|s| {
            s.rename_pending = false;
            s.name_error = None;
            if let Err(e) = &saved {
                s.banner = Some(format!("Neues Zertifikat nicht gespeichert: {e}"));
            }
            // Otherwise the Verbindung window keeps showing the old subject
            // and fingerprint for the rest of the session.
            if let Some(old) = s.conn_info.as_ref() {
                let mut fresh = (**old).clone();
                fresh.client_cert = info.clone();
                s.conn_info = Some(Arc::new(fresh));
            }
        });
    }

    fn reopen_streams(&mut self, shared: &Arc<Shared>) {
        let mut backend = self.backend.lock().unwrap_or_else(|e| e.into_inner());
        shared.reference_open.store(false, Ordering::Relaxed);
        backend.close(StreamKind::Capture);
        backend.close(StreamKind::Reference);
        backend.close(StreamKind::Playback);
        shared.dsp.reinit_aec.store(true, Ordering::Relaxed);
        open_streams(&mut **backend, &self.settings, shared);
        let devices = backend.devices();
        shared.state.send_modify(|s| s.devices = devices);
    }

    fn cue(&self, shared: &Arc<Shared>, cue: Cue) {
        let (join, tx, mute) = *shared.cue_settings.lock().unwrap();
        let enabled = match cue {
            Cue::Join | Cue::Leave => join,
            Cue::TxStart | Cue::TxStop => tx,
            Cue::Mute | Cue::Unmute => mute,
        };
        if enabled {
            let samples = crate::cues::render(cue);
            shared.cues.lock().unwrap().extend(samples);
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
    let (client_config, cert_failure) = match conn::client_config_with_crl(
        &local.identity,
        crl.as_deref(),
    ) {
        Ok(c) => c,
        Err(e) => return SessionEnd::Fatal(e.to_string()),
    };
    let endpoint = match crate::net::bind_endpoint(addr) {
        Ok(e) => e,
        Err(e) => return SessionEnd::Retry(e.to_string()),
    };
    let sni = crate::net::server_name_for(&local.identity.server.server_name);
    let hello_flags = local.flags;
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
        // identity abandons the attempt made with the old one.
        end = drain_until_quit(cmd_rx, local, shared) => return end,
    };
    let session = match session {
        Ok(s) => s,
        Err(e) => {
            return if e.is_fatal() {
                SessionEnd::Fatal(e.to_string())
            } else {
                SessionEnd::Retry(e.to_string())
            };
        }
    };
    let conn::Session {
        conn: connection,
        mut send,
        mut recv,
        welcome,
    } = session;

    // A registration bundle imported into a running client: swap it for a
    // certificate of our own under the placeholder name, which the user can
    // change like any other name (`PROTOCOL.md` §2.3). The first-run window
    // does this itself, with the name the user typed.
    if welcome.registration {
        shared.state.send_modify(|s| {
            s.conn = ConnStatus::Connecting;
            s.banner = Some("Registrierung läuft…".into());
        });
        let name = welcome.you.name.clone();
        let bundle = local.identity.clone();
        let registered = tokio::select! {
            r = crate::net::register::complete(&mut send, &mut recv, &bundle, &name) => r,
            end = drain_until_quit(cmd_rx, local, shared) => return end,
        };
        // Stored first, then closed: the clean close is the receipt that
        // makes the server revoke the bundle (`PROTOCOL.md` §2.3).
        return match registered {
            // Saved before it is taken into use: an identity that only lives
            // in memory would consume the bundle on the next login and be
            // gone after a restart.
            Ok(identity) => match identity.save(&local.paths) {
                Ok(()) => {
                    connection.close(crate::net::register::receipt(true), b"");
                    local.replace_identity(identity, shared);
                    shared.state.send_modify(|s| s.banner = None);
                    SessionEnd::Retry("registriert".into())
                }
                Err(e) => {
                    connection.close(crate::net::register::receipt(false), b"");
                    shared.state.send_modify(|s| s.banner = None);
                    SessionEnd::Fatal(format!(
                        "Registrierung nicht gespeichert: {e} — die Einladung bleibt gültig"
                    ))
                }
            },
            // No answer: the bundle is still valid, so try again — unless the
            // server closed with a fatal code (replaced by another device,
            // revoked), where retrying would only burn the registration budget.
            Err(e @ crate::net::NetError::Connect(_)) => {
                shared.state.send_modify(|s| s.banner = None);
                if let Some(reason) = connection.close_reason() {
                    return close_end(ConnectError::from_connection_error(reason));
                }
                connection.close(crate::net::register::receipt(false), b"");
                SessionEnd::Retry(format!("Registrierung unterbrochen: {e}"))
            }
            Err(e) => {
                connection.close(crate::net::register::receipt(false), b"");
                shared.state.send_modify(|s| s.banner = None);
                SessionEnd::Fatal(format!("Registrierung fehlgeschlagen: {e}"))
            }
        };
    }

    // Connected: install the peer state, clear everything session scoped.
    let frame_samples = shared.dsp.frame_samples.load(Ordering::Relaxed);
    {
        let volumes = shared.volumes.lock().unwrap();
        let mut speakers = shared.speakers.lock().unwrap_or_else(|e| e.into_inner());
        speakers.clear();
        for u in &welcome.users {
            if let Ok(sp) = Speaker::new(
                u.user_id,
                volumes.get(&u.user_id).copied().unwrap_or(100.0),
                frame_samples,
            ) {
                speakers.insert(u.session, sp);
            }
        }
    }
    let update_required = match (
        icu_proto::Version::parse(&welcome.min_client),
        icu_proto::Version::parse(env!("CARGO_PKG_VERSION")),
    ) {
        (Ok(min), Ok(mine)) if min > mine => Some(welcome.min_client.clone()),
        _ => None,
    };
    local.name = welcome.you.name.clone();
    {
        let volumes = shared.volumes.lock().unwrap();
        let peers: Vec<Peer> = welcome
            .users
            .iter()
            .map(|u| Peer {
                info: u.clone(),
                speaking: u.flags.speaking(),
                volume: volumes.get(&u.user_id).copied().unwrap_or(100.0),
                rtt_ms: None,
                recv_loss_permille: None,
            })
            .collect();
        let me = welcome.you.clone();
        tracing::info!(peers = peers.len(), "welcome: initial peer list");
        // Parsed here, once: three X.509 certificates is far too much work
        // for a render thread that repaints at 30 fps.
        // Behind an `Arc`: `AppState` is cloned on every change — the
        // speaking tick alone fires ten times a second — and this is a
        // couple of dozen strings that are written exactly once.
        let info = Arc::new(crate::conninfo::ConnInfo::collect(
            &connection,
            &welcome,
            &local.identity,
            &cfg.client_string,
        ));
        shared.state.send_modify(|s| {
            s.conn = ConnStatus::Connected;
            s.me = Some(me);
            s.peers = peers;
            s.update_required = update_required;
            s.is_admin = welcome.is_admin;
            s.conn_info = Some(info);
        });
    }
    *shared.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(connection.clone());
    *was_connected = true;
    tracing::info!(
        session = welcome.you.session,
        user_id = welcome.you.user_id,
        "connected"
    );

    // Reader tasks.
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<ControlMsg>();
    let reader = tokio::spawn(control_reader(recv, ctrl_tx));
    let dg_reader = tokio::spawn(datagram_reader(connection.clone(), shared.clone()));
    let video_reader = tokio::spawn(crate::video::accept_loop(
        connection.clone(),
        shared.clone(),
    ));
    *shared.video_tx.lock().unwrap() = Some(crate::video::Sender::new(connection.clone()));

    // Flags may have changed while the handshake was in flight (a command
    // handled during `connect`); the server only knows `Hello.initial`.
    // Compare only the client-settable bits: `welcome.you.flags` may carry
    // server-authoritative `SPEAKING`/`ADMIN` bits `local.flags` never has,
    // which would otherwise trigger a spurious `SetState` on every connect.
    let settable = UserFlags::CLIENT_SETTABLE;
    if local.flags.sanitized().0 & settable != welcome.you.flags.0 & settable {
        let msg = ControlMsg::SetState {
            flags: local.flags.sanitized(),
        };
        if conn::write_msg(&mut send, &msg).await.is_err() {
            return SessionEnd::Retry("send failed".into());
        }
    }

    // Pick up a rename that happened on another device, or renew a
    // certificate that is about to expire (`PROTOCOL.md` §2.2); again every
    // hour, since a session can outlive the window.
    if maybe_auto_renew(&mut send, local, &welcome.you.name).await.is_err() {
        return SessionEnd::Retry("send failed".into());
    }
    let mut renew_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(3600),
        Duration::from_secs(3600),
    );
    renew_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut ping = tokio::time::interval(Duration::from_secs(5));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
    let mut set_state_budget = RateBudget::new(pl::SET_STATE_PER_S, Duration::from_secs(1));

    let end = loop {
        tokio::select! {
            msg = ctrl_rx.recv() => {
                let Some(msg) = msg else {
                    break match connection.close_reason() {
                        Some(e) => close_end(ConnectError::from_connection_error(e)),
                        None => SessionEnd::Retry("control stream ended".into()),
                    };
                };
                if let Some(end) = handle_control(msg, shared, local, &mut pings, &mut send).await {
                    break end;
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break SessionEnd::Quit };
                match cmd {
                    Command::Quit => break SessionEnd::Quit,
                    Command::RequestRename(name) => {
                        let name = icu_proto::normalize_name(&name);
                        if local.pending_renew.is_some() {
                            shared.state.send_modify(|s| {
                                s.name_error = Some("Eine Änderung läuft noch".into())
                            });
                        } else {
                            match send_renew(&mut send, local, &name, RenewReason::UserRename).await {
                                Ok(()) => shared.state.send_modify(|s| {
                                    s.rename_pending = true;
                                    s.name_error = None;
                                }),
                                Err(RenewSendError::Local(e)) => {
                                    shared.state.send_modify(|s| s.name_error = Some(e));
                                }
                                Err(RenewSendError::Transport) => {
                                    break SessionEnd::Retry("send failed".into());
                                }
                            }
                        }
                    }
                    Command::ReplaceIdentity(identity) => {
                        local.replace_identity(*identity, shared);
                        break SessionEnd::Retry("Identität ersetzt".into());
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
                            shared.state.send_modify(|s| s.invite_error = Some(e.to_string()));
                        } else if conn::write_msg(&mut send, &ControlMsg::CreateInviteRequest { name, ttl_s }).await.is_err() {
                            break SessionEnd::Retry("send failed".into());
                        }
                    }
                    Command::SetUserAdmin { user_id, is_admin } => {
                        if conn::write_msg(&mut send, &ControlMsg::AdminSetRole { user_id, is_admin }).await.is_err() {
                            break SessionEnd::Retry("send failed".into());
                        }
                    }
                    Command::SetUserDisabled { user_id, disabled } => {
                        if conn::write_msg(&mut send, &ControlMsg::AdminSetDisabled { user_id, disabled }).await.is_err() {
                            break SessionEnd::Retry("send failed".into());
                        }
                    }
                    Command::RevokeUser { user_id } => {
                        if conn::write_msg(&mut send, &ControlMsg::AdminRevokeUser { user_id }).await.is_err() {
                            break SessionEnd::Retry("send failed".into());
                        }
                    }
                    Command::RequestUserList => {
                        if conn::write_msg(&mut send, &ControlMsg::UserListRequest).await.is_err() {
                            break SessionEnd::Retry("send failed".into());
                        }
                    }
                    cmd => {
                        let send_state = local.handle_local(cmd, shared, Some(&mut send)).await;
                        if send_state && set_state_budget.take() {
                            let msg = ControlMsg::SetState { flags: local.flags.sanitized() };
                            if conn::write_msg(&mut send, &msg).await.is_err() {
                                break SessionEnd::Retry("send failed".into());
                            }
                        }
                    }
                }
            }
            _ = renew_tick.tick() => {
                let stored = local.name.clone();
                if maybe_auto_renew(&mut send, local, &stored).await.is_err() {
                    break SessionEnd::Retry("send failed".into());
                }
            }
            _ = ping.tick() => {
                let nonce = next_nonce;
                next_nonce += 1;
                pings.insert(nonce, Instant::now());
                pings.retain(|_, t| t.elapsed() < Duration::from_secs(30));
                if conn::write_msg(&mut send, &ControlMsg::Ping { nonce }).await.is_err() {
                    break SessionEnd::Retry("send failed".into());
                }
            }
            _ = loss_tick.tick() => {
                let (reports, measured) = collect_loss_reports(shared);
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
                    datagram_queue_bytes: icu_proto::limits::DATAGRAM_BUFFER_BYTES
                        .saturating_sub(connection.datagram_send_buffer_space())
                        as u64,
                };
                let cond = shared
                    .net
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
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
                    tracing::info!(?tier, "video quality tier changed");
                }
                shared.stats.video_tier.store(tier.as_u8() as i64, Ordering::Relaxed);
            }
            reason = connection.closed() => {
                break close_end(ConnectError::from_connection_error(reason));
            }
        }
    };

    reader.abort();
    dg_reader.abort();
    video_reader.abort();
    *shared.video_tx.lock().unwrap() = None;
    *shared.conn.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if matches!(end, SessionEnd::Quit) {
        connection.close(quinn::VarInt::from_u32(codes::NO_ERROR), b"bye");
    }
    endpoint.close(quinn::VarInt::from_u32(codes::NO_ERROR), b"");
    tokio::time::timeout(Duration::from_millis(500), endpoint.wait_idle())
        .await
        .ok();
    end
}

impl crate::video::VideoSink for Arc<Shared> {
    fn deliver(&self, session: Session, frame: crate::video::EncodedFrame) {
        DebugStats::inc(&self.stats.video_received);
        DebugStats::add(
            &self.stats.video_bytes_received,
            (icu_proto::header::VIDEO_HEADER_LEN + frame.data.len()) as u64,
        );
        self.video.lock().unwrap().push(session, frame);
    }
}

fn close_end(e: ConnectError) -> SessionEnd {
    if e.is_fatal() {
        SessionEnd::Fatal(e.to_string())
    } else {
        SessionEnd::Retry(e.to_string())
    }
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
            Some(cmd) => {
                let replaced = matches!(cmd, Command::ReplaceIdentity(_));
                local.handle_local(cmd, shared, None).await;
                if replaced {
                    return SessionEnd::Retry("Identität ersetzt".into());
                }
            }
        }
    }
}

async fn control_reader(mut recv: quinn::RecvStream, tx: mpsc::UnboundedSender<ControlMsg>) {
    loop {
        match crate::net::read_frame(&mut recv, icu_proto::MAX_CONTROL_FRAME).await {
            Ok(body) => match ControlMsg::decode(&body) {
                Ok(msg) => {
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!(%e, "control decode");
                    return;
                }
            },
            Err(_) => return,
        }
    }
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
                let mut speakers = shared.speakers.lock().unwrap_or_else(|e| e.into_inner());
                let sp = match speakers.get_mut(&header.speaker) {
                    Some(sp) => sp,
                    None => {
                        // Unknown session (UserJoined may still be in flight).
                        match Speaker::new(0, 100.0, frame_samples) {
                            Ok(sp) => speakers.entry(header.speaker).or_insert(sp),
                            Err(_) => continue,
                        }
                    }
                };
                sp.last_packet = Some(now);
                sp.jitter.push(
                    Packet {
                        seq: header.seq,
                        ts: header.ts,
                        end: header.end(),
                        payload: payload.to_vec(),
                    },
                    now,
                );
            }
            Err(_) => DebugStats::inc(&shared.stats.datagrams_bad),
        }
    }
}

/// `(session, loss per mille)` for every speaker with traffic this tick.
type LossPerSpeaker = Vec<(Session, u16)>;

/// Reads each speaker's jitter buffer and returns `(to_report, measured)`:
/// the first is what goes on the wire as `LossReport`, still gated on a
/// ≥10 ‰ change so idle jitter does not chatter at the server; the second is
/// every speaker's current figure, for the Verbindung window. The gate
/// mutates `last_reported_permille`, so the caller cannot re-derive one from
/// the other — hence both.
fn collect_loss_reports(shared: &Arc<Shared>) -> (LossPerSpeaker, LossPerSpeaker) {
    let mut report = Vec::new();
    let mut all = Vec::new();
    let mut speakers = shared.speakers.lock().unwrap_or_else(|e| e.into_inner());
    for (session, sp) in speakers.iter_mut() {
        let st = sp.jitter.stats();
        let played = st.played - sp.report_base.0;
        let lost = st.lost - sp.report_base.1;
        sp.report_base = (st.played, st.lost);
        let total = played + lost;
        if total == 0 {
            continue;
        }
        let permille = ((lost * 1000) / total) as u16;
        all.push((*session, permille));
        // The ≥10 ‰ gate is what keeps `LossReport` off the wire for noise;
        // it must stay exactly as it was. `all` is display only.
        if permille.abs_diff(sp.last_reported_permille) >= 10 {
            sp.last_reported_permille = permille;
            report.push((*session, permille));
        }
    }
    (report, all)
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
    _send: &mut quinn::SendStream,
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
        }
        ControlMsg::UserJoined { user } => {
            let frame_samples = shared.dsp.frame_samples.load(Ordering::Relaxed);
            let volume = shared
                .volumes
                .lock()
                .unwrap()
                .get(&user.user_id)
                .copied()
                .unwrap_or(100.0);
            {
                let mut speakers = shared.speakers.lock().unwrap_or_else(|e| e.into_inner());
                match speakers.get_mut(&user.session) {
                    Some(sp) => {
                        sp.user_id = user.user_id;
                        sp.volume = volume;
                    }
                    None => {
                        if let Ok(sp) = Speaker::new(user.user_id, volume, frame_samples) {
                            speakers.insert(user.session, sp);
                        }
                    }
                }
            }
            local.cue(shared, Cue::Join);
            shared.state.send_modify(|s| {
                s.peers.retain(|p| p.info.session != user.session);
                s.peers.push(Peer {
                    speaking: user.flags.speaking(),
                    info: user,
                    volume,
                    rtt_ms: None,
                    recv_loss_permille: None,
                });
                tracing::info!(peers = s.peers.len(), "user joined: peer list updated");
            });
        }
        ControlMsg::UserLeft { session, .. } => {
            shared
                .speakers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&session);
            shared.video.lock().unwrap().remove(session);
            local.cue(shared, Cue::Leave);
            shared
                .state
                .send_modify(|s| s.peers.retain(|p| p.info.session != session));
        }
        ControlMsg::UserState { session, flags } => {
            shared.state.send_modify(|s| {
                if let Some(me) = s.me.as_mut() {
                    if me.session == session {
                        me.flags = flags;
                    }
                }
                for p in s.peers.iter_mut() {
                    if p.info.session == session {
                        p.info.flags = flags;
                        p.speaking = p.speaking || flags.speaking();
                        if !flags.speaking() {
                            p.speaking = false;
                        }
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
        ControlMsg::Error { code, detail } => {
            tracing::warn!(code, %detail, "server error");
            if code == codes::NAME_TAKEN || code == codes::RENEW_REJECTED {
                // A background renewal must not paint a red error under the
                // name field: only a rename the user asked for lands there.
                let pending = local.pending_renew.take();
                let asked_for_it = pending
                    .map(|p| p.reason == RenewReason::UserRename)
                    .unwrap_or(code == codes::NAME_TAKEN);
                shared.state.send_modify(|s| {
                    s.rename_pending = false;
                    if asked_for_it {
                        s.name_error = Some(detail.clone());
                    }
                });
                if !asked_for_it {
                    tracing::warn!(code, %detail, "background renewal rejected");
                }
            } else if code == codes::NOT_ADMIN {
                shared.state.send_modify(|s| s.invite_error = Some(detail));
            } else {
                shared
                    .state
                    .send_modify(|s| s.banner = Some(format!("{}: {detail}", codes::name(code))));
            }
        }
        ControlMsg::LossHint { loss_permille } => {
            shared
                .dsp
                .loss_permille
                .store(loss_permille as u32, Ordering::Relaxed);
            shared
                .net
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_audio_loss_hint(loss_permille);
            let hinted = loss_permille > 0;
            for sp in shared
                .speakers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values_mut()
            {
                if sp.loss_hinted != hinted {
                    sp.loss_hinted = hinted;
                    sp.jitter.set_loss_hinted(hinted);
                }
            }
        }
        ControlMsg::KeyframeRequest { .. } => {
            shared.keyframe_request.store(true, Ordering::Relaxed);
        }
        ControlMsg::VideoCongestionHint { drop_permille } => {
            shared
                .net
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .set_video_drop_hint(drop_permille);
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
            shared.state.send_modify(|s| {
                s.last_invite = Some((bundle, expires));
                s.invite_error = None;
            });
        }
        ControlMsg::AdminSetRoleOk { .. }
        | ControlMsg::AdminSetDisabledOk { .. }
        | ControlMsg::AdminRevokeUserOk { .. } => {
            // Acks don't carry the full row; the Nutzer tab re-issues
            // RequestUserList right after sending an action, so this just
            // confirms no error banner is needed while that refresh lands.
            shared.state.send_modify(|s| s.user_action_error = None);
        }
        ControlMsg::UserListResponse { users } => {
            shared.state.send_modify(|s| s.user_list = users);
        }
        ControlMsg::AdminActionError { detail, .. } => {
            shared
                .state
                .send_modify(|s| s.user_action_error = Some(detail));
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
        ControlMsg::Welcome { .. } => {
            return Some(SessionEnd::Fatal("second Welcome".into()));
        }
        ControlMsg::Hello { .. }
        | ControlMsg::SetState { .. }
        | ControlMsg::Ping { .. }
        | ControlMsg::RenewRequest { .. }
        | ControlMsg::LossReport { .. }
        | ControlMsg::SetName { .. }
        | ControlMsg::CreateInviteRequest { .. }
        | ControlMsg::AdminSetRole { .. }
        | ControlMsg::AdminSetDisabled { .. }
        | ControlMsg::AdminRevokeUser { .. }
        | ControlMsg::UserListRequest => {
            return Some(SessionEnd::Fatal(format!(
                "server sent client message {}",
                msg.variant()
            )));
        }
    }
    None
}

/// A token bucket for `SetState` (≤ 20/s).
struct RateBudget {
    max: u32,
    window: Duration,
    count: u32,
    start: Instant,
}

impl RateBudget {
    fn new(max: u32, window: Duration) -> RateBudget {
        RateBudget {
            max,
            window,
            count: 0,
            start: Instant::now(),
        }
    }
    fn take(&mut self) -> bool {
        if self.start.elapsed() > self.window {
            self.start = Instant::now();
            self.count = 0;
        }
        if self.count < self.max {
            self.count += 1;
            true
        } else {
            false
        }
    }
}

impl From<NetError> for EngineError {
    fn from(e: NetError) -> Self {
        EngineError(e.to_string())
    }
}

/// What the camera thread needs from the engine (M4).
#[derive(Clone)]
pub struct CameraLink {
    shared: Arc<Shared>,
    runtime: tokio::runtime::Handle,
}

impl CameraLink {
    /// Sends one encoded frame on the live connection; `Err` when not
    /// connected or when flow control dropped it.
    pub fn send_frame(&self, keyframe: bool, ts: u32, data: &[u8]) -> Result<(), String> {
        let mut guard = self
            .shared
            .video_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(sender) = guard.as_mut() else {
            return Err("not connected".into());
        };
        let r = self.runtime.block_on(sender.send(keyframe, ts, data));
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
    /// server would relay the frames to nobody. Resuming builds a fresh
    /// encoder, whose first frame is an IDR.
    pub fn send_tier(&self) -> VideoTier {
        send_tier(self.video_tier(), &self.shared.state.borrow().peers)
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

/// [`CameraLink::send_tier`], without the engine.
fn send_tier(tier: VideoTier, peers: &[Peer]) -> VideoTier {
    if peers.iter().any(|p| p.info.flags.viewing()) {
        tier
    } else {
        VideoTier::T4
    }
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
}

impl Engine {
    /// Link for the camera thread.
    pub fn camera_link(&self) -> CameraLink {
        CameraLink {
            shared: self.shared.clone(),
            runtime: self.runtime(),
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
    use super::*;

    fn peer(session: Session, flags: u8) -> Peer {
        Peer {
            info: UserInfo {
                user_id: u32::from(session),
                session,
                name: format!("p{session}"),
                flags: UserFlags(flags),
            },
            speaking: false,
            volume: 100.0,
            rtt_ms: None,
            recv_loss_permille: None,
        }
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
            ..Default::default()
        };
        // 30 floored days are up to 31 left: the server would refuse.
        assert_eq!(renewal_due(&cert(30), "alice"), None);
        assert_eq!(renewal_due(&cert(29), "alice"), Some(RenewReason::Expiry));
        assert_eq!(renewal_due(&cert(200), "bob"), Some(RenewReason::CatchUp));
    }
}
