//! WASAPI backend (Windows, `Client/IMPLEMENTATION.md` §3): shared mode,
//! one thread per stream, 48 kHz mono f32 in 240-sample blocks like every
//! backend.
//!
//! - Capture and playback are event driven. Where the device's mix format
//!   runs at 48 kHz, `IAudioClient3::InitializeSharedAudioStream` asks the
//!   engine for the smallest period at or above 5 ms; otherwise (44.1 kHz
//!   devices, no `IAudioClient3`) `IAudioClient::Initialize` with
//!   `AUTOCONVERTPCM` lets Windows resample, at its 10 ms period.
//! - Capture runs as category Other with the RAW option (no system effects
//!   in front of ICU's own processing), playback as GameChat.
//! - The AEC reference is loopback capture of the output device
//!   (`AUDCLNT_STREAMFLAGS_LOOPBACK`), read every 2 ms: loopback has no
//!   event of its own. ICU's own playback on the same device keeps it
//!   delivering while nothing else plays.
//! - `IMMNotificationClient` reports hot-plug as `DevicesChanged`; a stream
//!   on the default device („Systemstandard“, the console role) moves along
//!   when the default changes, or when its device goes away.
//!
//! COM objects never leave the thread that made them: every stream thread,
//! the watcher and each device listing join the multithreaded apartment on
//! their own.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use windows::{
    core::{implement, Interface, HSTRING, PCWSTR, PWSTR},
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{CloseHandle, HANDLE, PROPERTYKEY, WAIT_OBJECT_0},
        Media::{
            Audio::{
                eCapture, eConsole, eRender, AudioCategory_GameChat, AudioCategory_Other,
                AudioClientProperties, EDataFlow, ERole, IAudioCaptureClient, IAudioClient,
                IAudioClient2, IAudioClient3, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
                IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
                AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                AUDCLNT_STREAMOPTIONS_NONE, AUDCLNT_STREAMOPTIONS_RAW, DEVICE_STATE,
                DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
            },
            KernelStreaming::WAVE_FORMAT_EXTENSIBLE,
            Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT},
        },
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
                StructuredStorage::PropVariantToStringAlloc, CLSCTX_ALL, COINIT_MULTITHREADED,
                STGM_READ,
            },
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
};

use super::backend::{
    AudioBackend, BackendError, BackendEvent, CaptureCallback, DeviceInfo, DeviceKind,
    PlaybackCallback, StreamKind, BLOCK_SAMPLES, SAMPLE_RATE,
};

/// How long a stream thread waits for its event before looking at the stop
/// flag and the default device again.
const WAIT_MS: u32 = 100;
/// How often loopback is read: well inside the microphone's period, so a
/// reference packet reaches the engine soon after the matching capture.
const LOOPBACK_POLL: Duration = Duration::from_millis(2);
/// Hot-plug notifications arrive in bursts; the list is sent once they
/// settle.
const SETTLE: Duration = Duration::from_millis(250);

/// Joins the calling thread to the multithreaded apartment for as long as
/// it lives.
struct Com;

impl Com {
    fn init() -> Result<Com, String> {
        // SAFETY: balanced by `CoUninitialize` in `drop`.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| format!("CoInitializeEx: {e}"))?;
        Ok(Com)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: after every COM object of this thread is gone (declared
        // first, dropped last).
        unsafe { CoUninitialize() };
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    // SAFETY: COM is initialized on the calling thread.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|e| format!("MMDeviceEnumerator: {e}"))
}

/// Takes a string COM allocated.
fn take_pwstr(p: PWSTR) -> String {
    // SAFETY: a NUL-terminated string from COM, freed once read.
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn device_id(device: &IMMDevice) -> Result<String, String> {
    // SAFETY: a live endpoint.
    unsafe { device.GetId() }
        .map(take_pwstr)
        .map_err(|e| e.to_string())
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    // SAFETY: a live endpoint; the PROPVARIANT is read, not kept.
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        PropVariantToStringAlloc(&value).ok().map(take_pwstr)
    }
}

/// Every active endpoint, inputs first. Needs COM on the calling thread.
fn list(enumerator: &IMMDeviceEnumerator) -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    for (flow, kind) in [(eCapture, DeviceKind::Input), (eRender, DeviceKind::Output)] {
        // SAFETY: a live enumerator.
        let Ok(collection) = (unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) })
        else {
            continue;
        };
        let n = unsafe { collection.GetCount() }.unwrap_or(0);
        for i in 0..n {
            let Ok(device) = (unsafe { collection.Item(i) }) else {
                continue;
            };
            let Ok(id) = device_id(&device) else { continue };
            let description = friendly_name(&device).unwrap_or_else(|| id.clone());
            out.push(DeviceInfo {
                id,
                description,
                kind,
            });
        }
    }
    out
}

/// Counts changes of the default devices, one counter per direction, for
/// the streams that follow them.
#[derive(Default)]
struct Defaults {
    render: AtomicU64,
    capture: AtomicU64,
}

impl Defaults {
    fn of(&self, flow: EDataFlow) -> &AtomicU64 {
        if flow == eCapture {
            &self.capture
        } else {
            &self.render
        }
    }
}

/// Hears Windows about endpoints; does no work in the callbacks (COM
/// forbids waiting there) but bumps a counter and wakes the watcher.
#[implement(IMMNotificationClient)]
struct Notifications {
    defaults: Arc<Defaults>,
    wake: mpsc::Sender<()>,
}

impl IMMNotificationClient_Impl for Notifications_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        let _ = self.wake.send(());
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        let _ = self.wake.send(());
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        let _ = self.wake.send(());
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        if role == eConsole {
            self.defaults.of(flow).fetch_add(1, Ordering::SeqCst);
            let _ = self.wake.send(());
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// The thread that holds the notification registration and sends the
/// device list after every burst of changes. Ends when dropped.
struct Watcher {
    stopping: Arc<AtomicBool>,
    wake: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    fn start(
        defaults: Arc<Defaults>,
        events: mpsc::Sender<BackendEvent>,
    ) -> Result<Watcher, String> {
        let (wake_tx, wake_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let stop = wake_tx.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let stopping_t = stopping.clone();
        let thread = std::thread::Builder::new()
            .name("icu-wasapi-watch".into())
            .spawn(move || {
                let _com = match Com::init() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let enumerator = match enumerator() {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let client: IMMNotificationClient = Notifications {
                    defaults,
                    wake: wake_tx,
                }
                .into();
                // SAFETY: the client lives until it is unregistered below.
                if let Err(e) = unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }
                {
                    tracing::warn!(%e, "no audio hot-plug notifications");
                }
                let _ = ready_tx.send(Ok(()));
                while wake_rx.recv().is_ok() {
                    if stopping_t.load(Ordering::SeqCst) {
                        break;
                    }
                    // Let the burst settle, then send one list.
                    while wake_rx.recv_timeout(SETTLE).is_ok() {}
                    if stopping_t.load(Ordering::SeqCst) {
                        break;
                    }
                    let _ = events.send(BackendEvent::DevicesChanged(list(&enumerator)));
                }
                unsafe {
                    let _ = enumerator.UnregisterEndpointNotificationCallback(&client);
                }
            })
            .map_err(|e| e.to_string())?;
        let watcher = Watcher {
            stopping,
            wake: stop,
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(watcher),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("WASAPI watcher did not start".into()),
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // Set before the wake, read after each wake.
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// One running stream: its thread and its stop flag.
struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// WASAPI backend.
pub struct WasapiBackend {
    events_tx: mpsc::Sender<BackendEvent>,
    events: Option<mpsc::Receiver<BackendEvent>>,
    defaults: Arc<Defaults>,
    streams: HashMap<StreamKind, Running>,
    /// Dropped after the streams.
    _watcher: Watcher,
}

impl WasapiBackend {
    /// Starts the device watcher; fails when there is no audio service.
    pub fn new() -> Result<WasapiBackend, BackendError> {
        let (tx, rx) = mpsc::channel();
        let defaults = Arc::new(Defaults::default());
        let watcher = Watcher::start(defaults.clone(), tx.clone())?;
        Ok(WasapiBackend {
            events_tx: tx,
            events: Some(rx),
            defaults,
            streams: HashMap::new(),
            _watcher: watcher,
        })
    }

    fn open(
        &mut self,
        kind: StreamKind,
        device: Option<&str>,
        callback: Callback,
    ) -> Result<(), BackendError> {
        self.streams.remove(&kind);
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let spec = Spec {
            kind,
            device: device.map(str::to_string),
        };
        let (stop_t, defaults, events) =
            (stop.clone(), self.defaults.clone(), self.events_tx.clone());
        let thread = std::thread::Builder::new()
            .name(format!("icu-wasapi-{}", spec.name()))
            .spawn(move || run_stream(spec, callback, stop_t, defaults, events, ready_tx))
            .map_err(|e| BackendError(e.to_string()))?;
        let running = Running {
            stop,
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {
                self.streams.insert(kind, running);
                Ok(())
            }
            Ok(Err(e)) => Err(BackendError(e)),
            Err(_) => Err(BackendError(format!("{kind:?}: device did not start"))),
        }
    }
}

impl AudioBackend for WasapiBackend {
    fn name(&self) -> &'static str {
        "wasapi"
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        // On a thread of its own: the caller's may be in another apartment
        // (the UI thread is STA).
        std::thread::scope(|s| {
            s.spawn(|| {
                let Ok(_com) = Com::init() else {
                    return Vec::new();
                };
                enumerator().map(|e| list(&e)).unwrap_or_default()
            })
            .join()
            .unwrap_or_default()
        })
    }

    fn open_capture(
        &mut self,
        device: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        self.open(StreamKind::Capture, device, Callback::Capture(cb))
    }

    fn open_reference(
        &mut self,
        sink: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        self.open(StreamKind::Reference, sink, Callback::Capture(cb))
    }

    fn open_playback(
        &mut self,
        device: Option<&str>,
        cb: PlaybackCallback,
    ) -> Result<(), BackendError> {
        self.open(StreamKind::Playback, device, Callback::Playback(cb))
    }

    fn close(&mut self, stream: StreamKind) {
        self.streams.remove(&stream);
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<BackendEvent>> {
        self.events.take()
    }

    /// Microphone and loopback are two free-running streams. Measured in
    /// the VM (QEMU, HDA): loopback stalls up to ~35 ms, then delivers the
    /// backlog in one go; without this slack the engine counted each stall
    /// as a dropout and reset the AEC every 10 s.
    fn reference_slack(&self) -> Duration {
        Duration::from_millis(40)
    }
}

enum Callback {
    Capture(CaptureCallback),
    Playback(PlaybackCallback),
}

/// What a stream thread opens.
struct Spec {
    kind: StreamKind,
    /// Endpoint id; `None` follows the default device.
    device: Option<String>,
}

impl Spec {
    fn name(&self) -> &'static str {
        match self.kind {
            StreamKind::Capture => "capture",
            StreamKind::Reference => "loopback",
            StreamKind::Playback => "playback",
        }
    }

    fn flow(&self) -> EDataFlow {
        match self.kind {
            StreamKind::Capture => eCapture,
            StreamKind::Reference | StreamKind::Playback => eRender,
        }
    }

    fn loopback(&self) -> bool {
        self.kind == StreamKind::Reference
    }
}

/// Why a stream's pump stopped.
enum Ended {
    /// Closed by the backend.
    Stopped,
    /// The default device changed under a stream that follows it.
    DefaultChanged,
    /// The device is gone (unplugged, disabled).
    DeviceLost,
    /// Anything else.
    Failed(String),
}

fn run_stream(
    spec: Spec,
    mut callback: Callback,
    stop: Arc<AtomicBool>,
    defaults: Arc<Defaults>,
    events: mpsc::Sender<BackendEvent>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let _com = match Com::init() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let name = format!("icu-wasapi-{}", spec.name());
    if spec.kind != StreamKind::Reference {
        super::rt::promote_current_thread(&name);
    }
    let generation = defaults.of(spec.flow()).load(Ordering::SeqCst);
    let mut stream = match Stream::open(&spec) {
        Ok(stream) => stream,
        Err(e) => {
            let _ = ready.send(Err(e.clone()));
            // Initial failure also needs recovery (audio service restarting,
            // or no endpoint yet). The engine applies its bounded backoff.
            let _ = events.send(BackendEvent::StreamError {
                stream: spec.kind,
                message: e,
            });
            return;
        }
    };
    let _ = ready.send(Ok(()));
    tracing::info!(stream = spec.name(), device = %stream.device_name,
        channels = stream.channels, period = stream.period, mode = stream.mode,
        "wasapi stream open");
    let ended = stream.pump(&mut callback, &stop, || {
        spec.device.is_none() && defaults.of(spec.flow()).load(Ordering::SeqCst) != generation
    });
    // The engine owns recovery, including clearing the audio rings and resetting
    // AEC. Reopening just this thread kept samples from unrelated device clocks.
    match ended {
        Ended::Stopped => {}
        Ended::DefaultChanged => {
            let _ = events.send(BackendEvent::DefaultDeviceChanged);
        }
        Ended::DeviceLost | Ended::Failed(_) => {
            let message = match ended {
                Ended::Failed(message) => message,
                _ => "Gerät nicht mehr verfügbar".into(),
            };
            let _ = events.send(BackendEvent::StreamError {
                stream: spec.kind,
                message,
            });
        }
    }
}

/// An open, started WASAPI stream.
struct Stream {
    // Declaration order is drop order: clients before the audio client.
    capture: Option<IAudioCaptureClient>,
    render: Option<IAudioRenderClient>,
    client: IAudioClient,
    event: Option<HANDLE>,
    channels: usize,
    /// Frames in the endpoint buffer (render).
    buffer_frames: u32,
    /// Engine period in frames, for the log.
    period: u32,
    /// Which initialization took: "IAudioClient3" or "autoconvert".
    mode: &'static str,
    loopback: bool,
    device_name: String,
    /// Captured samples not yet handed on as a whole block.
    pending: Vec<f32>,
    /// One block for the playback callback.
    block: Vec<f32>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: stopping a started client; the event is ours.
        unsafe {
            let _ = self.client.Stop();
            if let Some(event) = self.event {
                let _ = CloseHandle(event);
            }
        }
    }
}

/// The device's shared-mode mix format, copied out of COM's allocation.
struct MixFormat {
    format: WAVEFORMATEX,
    extensible: Option<WAVEFORMATEXTENSIBLE>,
}

impl MixFormat {
    fn is_float32(&self) -> bool {
        let tag = u32::from(self.format.wFormatTag);
        let float = tag == WAVE_FORMAT_IEEE_FLOAT
            || (tag == WAVE_FORMAT_EXTENSIBLE
                && self.extensible.is_some_and(|x| {
                    let sub = x.SubFormat;
                    sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
                }));
        float && self.format.wBitsPerSample == 32
    }

    fn ptr(&self) -> *const WAVEFORMATEX {
        match &self.extensible {
            Some(x) => (x as *const WAVEFORMATEXTENSIBLE).cast(),
            None => &self.format,
        }
    }
}

fn mix_format(client: &IAudioClient) -> Result<MixFormat, String> {
    // SAFETY: GetMixFormat hands back a CoTaskMemAlloc'ed WAVEFORMATEX,
    // extended when its tag says so; copied, then freed.
    unsafe {
        let p = client
            .GetMixFormat()
            .map_err(|e| format!("GetMixFormat: {e}"))?;
        let format = *p;
        let extensible = (u32::from(format.wFormatTag) == WAVE_FORMAT_EXTENSIBLE
            && format.cbSize >= 22)
            .then(|| *(p as *const WAVEFORMATEXTENSIBLE));
        CoTaskMemFree(Some(p as *const _));
        Ok(MixFormat { format, extensible })
    }
}

/// 48 kHz float32 with the mix format's channels, for `AUTOCONVERTPCM`.
fn float48(channels: u16, mask: u32) -> WAVEFORMATEXTENSIBLE {
    let block = channels * 4;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: mask,
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

/// The smallest multiple of `fundamental` at or above one block, within
/// the engine's limits.
fn pick_period(fundamental: u32, min: u32, max: u32) -> u32 {
    let fundamental = fundamental.max(1);
    let want = (BLOCK_SAMPLES as u32).div_ceil(fundamental) * fundamental;
    want.clamp(min.max(fundamental), max.max(min))
}

impl Stream {
    fn open(spec: &Spec) -> Result<Stream, String> {
        let enumerator = enumerator()?;
        // SAFETY (whole block): COM calls on live objects of this thread;
        // format pointers point at locals that outlive the calls.
        unsafe {
            let device = match &spec.device {
                Some(id) => enumerator
                    .GetDevice(&HSTRING::from(id.as_str()))
                    .map_err(|e| format!("{id}: {e}"))?,
                None => enumerator
                    .GetDefaultAudioEndpoint(spec.flow(), eConsole)
                    .map_err(|e| format!("kein Standardgerät: {e}"))?,
            };
            let device_name = friendly_name(&device).unwrap_or_else(|| "?".into());
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| format!("Activate: {e}"))?;
            if !spec.loopback() {
                set_properties(&client, spec.kind);
            }
            let mix = mix_format(&client)?;
            let channels = mix.format.nChannels.max(1);
            let mut mode = "autoconvert";
            let mut period = 0u32;
            let mut initialized = false;
            if !spec.loopback() && mix.is_float32() && mix.format.nSamplesPerSec == SAMPLE_RATE {
                if let Ok(client3) = client.cast::<IAudioClient3>() {
                    let (mut default, mut fundamental, mut min, mut max) = (0, 0, 0, 0);
                    if client3
                        .GetSharedModeEnginePeriod(
                            mix.ptr(),
                            &mut default,
                            &mut fundamental,
                            &mut min,
                            &mut max,
                        )
                        .is_ok()
                    {
                        let p = pick_period(fundamental, min, max);
                        match client3.InitializeSharedAudioStream(
                            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                            p,
                            mix.ptr(),
                            None,
                        ) {
                            Ok(()) => {
                                initialized = true;
                                period = p;
                                mode = "IAudioClient3";
                            }
                            Err(e) => {
                                tracing::debug!(%e, "InitializeSharedAudioStream, using Initialize")
                            }
                        }
                    }
                }
            }
            if !initialized {
                let mask = mix.extensible.map(|x| x.dwChannelMask).unwrap_or(0);
                let format = float48(channels, mask);
                let (flags, buffer) = if spec.loopback() {
                    // Read by polling; a generous buffer rides out a late
                    // wake-up.
                    (AUDCLNT_STREAMFLAGS_LOOPBACK, 1_000_000)
                } else {
                    (AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 200_000)
                };
                client
                    .Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        flags
                            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                        buffer,
                        0,
                        (&format as *const WAVEFORMATEXTENSIBLE).cast(),
                        None,
                    )
                    .map_err(|e| format!("Initialize: {e}"))?;
                period = SAMPLE_RATE / 100;
            }
            let event = if spec.loopback() {
                None
            } else {
                let event = CreateEventW(None, false, false, PCWSTR::null())
                    .map_err(|e| format!("CreateEvent: {e}"))?;
                if let Err(e) = client.SetEventHandle(event) {
                    let _ = CloseHandle(event);
                    return Err(format!("SetEventHandle: {e}"));
                }
                Some(event)
            };
            let buffer_frames = client.GetBufferSize().unwrap_or(0);
            let (capture, render) = if spec.kind == StreamKind::Playback {
                let render: IAudioRenderClient = client
                    .GetService()
                    .map_err(|e| format!("IAudioRenderClient: {e}"))?;
                (None, Some(render))
            } else {
                let capture: IAudioCaptureClient = client
                    .GetService()
                    .map_err(|e| format!("IAudioCaptureClient: {e}"))?;
                (Some(capture), None)
            };
            let stream = Stream {
                capture,
                render,
                client,
                event,
                channels: usize::from(channels),
                buffer_frames,
                period,
                mode,
                loopback: spec.loopback(),
                device_name,
                pending: Vec::with_capacity(BLOCK_SAMPLES * 8),
                block: vec![0.0; BLOCK_SAMPLES],
            };
            if let Some(render) = &stream.render {
                // Start on silence so the first period is not a gap.
                if let Ok(data) = render.GetBuffer(buffer_frames) {
                    std::ptr::write_bytes(data, 0, buffer_frames as usize * stream.channels * 4);
                    let _ = render.ReleaseBuffer(buffer_frames, 0);
                }
            }
            stream.client.Start().map_err(|e| format!("Start: {e}"))?;
            Ok(stream)
        }
    }

    /// Moves audio until stopped, the device fails, or `moved()` says the
    /// default changed under it.
    fn pump(
        &mut self,
        callback: &mut Callback,
        stop: &AtomicBool,
        moved: impl Fn() -> bool,
    ) -> Ended {
        let mut last_check = Instant::now();
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ended::Stopped;
            }
            if last_check.elapsed() > Duration::from_millis(100) {
                last_check = Instant::now();
                if moved() {
                    return Ended::DefaultChanged;
                }
            }
            match self.event {
                // SAFETY: our own event handle.
                Some(event) => {
                    if unsafe { WaitForSingleObject(event, WAIT_MS) } != WAIT_OBJECT_0 {
                        continue;
                    }
                }
                None => std::thread::sleep(LOOPBACK_POLL),
            }
            let moved_audio = match callback {
                Callback::Capture(cb) => self.read(cb),
                Callback::Playback(cb) => self.write(cb),
            };
            if let Err(e) = moved_audio {
                return if e.code() == AUDCLNT_E_DEVICE_INVALIDATED {
                    Ended::DeviceLost
                } else {
                    Ended::Failed(e.to_string())
                };
            }
        }
    }

    /// Takes every packet waiting and hands on whole blocks: channel 0 of
    /// a microphone, the average of all channels of the loopback mix.
    fn read(&mut self, cb: &mut CaptureCallback) -> windows::core::Result<()> {
        let Some(capture) = &self.capture else {
            return Ok(());
        };
        let channels = self.channels;
        // SAFETY: GetBuffer/ReleaseBuffer pairs on a started client; the
        // buffer holds `frames × channels` f32 (the format asked for).
        unsafe {
            while capture.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let n = frames as usize;
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    self.pending.extend(std::iter::repeat_n(0.0, n));
                } else {
                    let samples = std::slice::from_raw_parts(data as *const f32, n * channels);
                    if self.loopback {
                        let scale = 1.0 / channels as f32;
                        self.pending.extend(
                            samples
                                .chunks_exact(channels)
                                .map(|f| f.iter().sum::<f32>() * scale),
                        );
                    } else {
                        self.pending
                            .extend(samples.chunks_exact(channels).map(|f| f[0]));
                    }
                }
                capture.ReleaseBuffer(frames)?;
            }
        }
        // The engine pairs microphone and reference per push
        // (`dsp::refalign`) and waits up to 1.5 microphone pushes for the
        // reference. The two WASAPI threads run on their own phases, so a
        // microphone push spans at least the device period (resampled
        // packets vary around it), never a single block.
        let min = if self.loopback {
            BLOCK_SAMPLES
        } else {
            (self.period as usize).div_ceil(BLOCK_SAMPLES).max(2) * BLOCK_SAMPLES
        };
        let whole = self.pending.len() / BLOCK_SAMPLES * BLOCK_SAMPLES;
        if whole >= min {
            cb(&self.pending[..whole]);
            self.pending.drain(..whole);
        }
        Ok(())
    }

    /// Fills the free part of the endpoint buffer, whole blocks only, the
    /// mono voice on every channel.
    fn write(&mut self, cb: &mut PlaybackCallback) -> windows::core::Result<()> {
        let Some(render) = &self.render else {
            return Ok(());
        };
        // SAFETY: GetBuffer/ReleaseBuffer on a started client, writing
        // exactly the `frames × channels` f32 asked for.
        unsafe {
            let padding = self.client.GetCurrentPadding()?;
            let free = self.buffer_frames.saturating_sub(padding) as usize;
            let frames = free / BLOCK_SAMPLES * BLOCK_SAMPLES;
            if frames == 0 {
                return Ok(());
            }
            let data = render.GetBuffer(frames as u32)?;
            let out = std::slice::from_raw_parts_mut(data as *mut f32, frames * self.channels);
            for chunk in out.chunks_exact_mut(BLOCK_SAMPLES * self.channels) {
                self.block.fill(0.0);
                cb(&mut self.block);
                for (frame, s) in chunk.chunks_exact_mut(self.channels).zip(&self.block) {
                    frame.fill(*s);
                }
            }
            render.ReleaseBuffer(frames as u32, 0)?;
        }
        Ok(())
    }
}

/// Stream category and, for the microphone, RAW (no system effects in
/// front of ICU's own processing). Best effort: older drivers refuse RAW,
/// then it is asked without.
fn set_properties(client: &IAudioClient, kind: StreamKind) {
    let Ok(client2) = client.cast::<IAudioClient2>() else {
        return;
    };
    let (category, raw) = match kind {
        StreamKind::Capture => (AudioCategory_Other, true),
        _ => (AudioCategory_GameChat, false),
    };
    let props = |options| AudioClientProperties {
        cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
        bIsOffload: false.into(),
        eCategory: category,
        Options: options,
    };
    // SAFETY: before Initialize, with a local struct.
    unsafe {
        if raw
            && client2
                .SetClientProperties(&props(AUDCLNT_STREAMOPTIONS_RAW))
                .is_ok()
        {
            return;
        }
        let _ = client2.SetClientProperties(&props(AUDCLNT_STREAMOPTIONS_NONE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run in an interactive Windows desktop session, with a default input
    /// and output. Plays a quiet tone for three seconds and measures loopback.
    #[test]
    #[ignore = "needs Windows audio endpoints; plays a quiet test tone"]
    fn live_wasapi_capture_playback_and_loopback() {
        use std::sync::atomic::AtomicUsize;
        let mut backend = WasapiBackend::new().unwrap();
        let devices = backend.devices();
        assert!(devices.iter().any(|d| d.kind == DeviceKind::Input));
        assert!(devices.iter().any(|d| d.kind == DeviceKind::Output));
        let cap = Arc::new(AtomicUsize::new(0));
        let reference = Arc::new(AtomicUsize::new(0));
        let audible = Arc::new(AtomicUsize::new(0));
        let counter = cap.clone();
        backend
            .open_capture(
                None,
                Box::new(move |samples| {
                    assert!(samples.iter().all(|x| x.is_finite()));
                    counter.fetch_add(samples.len(), Ordering::Relaxed);
                }),
            )
            .unwrap();
        let (counter, nonzero) = (reference.clone(), audible.clone());
        backend
            .open_reference(
                None,
                Box::new(move |samples| {
                    counter.fetch_add(samples.len(), Ordering::Relaxed);
                    nonzero.fetch_add(
                        samples.iter().filter(|x| x.abs() > 0.00001).count(),
                        Ordering::Relaxed,
                    );
                }),
            )
            .unwrap();
        let mut phase = 0.0f32;
        backend
            .open_playback(
                None,
                Box::new(move |samples| {
                    for sample in samples {
                        *sample = phase.sin() * 0.02;
                        phase = (phase + std::f32::consts::TAU * 997.0 / SAMPLE_RATE as f32)
                            % std::f32::consts::TAU;
                    }
                }),
            )
            .unwrap();
        std::thread::sleep(Duration::from_secs(3));
        drop(backend);
        let (cap, reference, audible) = (
            cap.load(Ordering::Relaxed),
            reference.load(Ordering::Relaxed),
            audible.load(Ordering::Relaxed),
        );
        eprintln!("WASAPI: capture={cap} reference={reference} nonzero_reference={audible}");
        assert!(cap > 48_000, "capture did not keep running");
        assert!(reference > 48_000, "loopback did not keep running");
        assert!(audible > 24_000, "rendered tone did not reach loopback");
    }

    #[test]
    fn the_period_is_the_first_fundamental_multiple_from_5_ms() {
        // Typical Windows 10 HD Audio: fundamental 48 frames (1 ms).
        assert_eq!(pick_period(48, 48, 480), 240);
        // Fundamental 10 ms only: stays at 10 ms.
        assert_eq!(pick_period(480, 480, 480), 480);
        // Odd fundamental: rounded up.
        assert_eq!(pick_period(100, 100, 1000), 300);
    }

    #[test]
    fn the_autoconvert_format_is_48k_float() {
        let f = float48(2, 3);
        assert_eq!({ f.Format.nSamplesPerSec }, 48_000);
        assert_eq!({ f.Format.nBlockAlign }, 8);
        assert_eq!({ f.Format.nAvgBytesPerSec }, 384_000);
    }
}
