//! Linux audio backend on pipewire-rs 0.10 (`docs/reference/client.md` §6).
//!
//! One dedicated thread runs the PipeWire main loop and owns the core, the
//! registry and up to three streams (capture, AEC reference, playback). The
//! engine talks to it through a [`pipewire::channel`]; every stream's
//! `process` callback runs on PipeWire's real-time data thread and only
//! forwards the block to the engine supplied closure.
//!
//! Stream properties (Spike 8): `node.latency = 240/48000`,
//! `node.rate = 1/48000`, `node.group = icu`, `media.role = Communication`,
//! format F32LE 48 kHz. Capture and reference negotiate native channels;
//! playback requests mono voice. The reference stream sets
//! `stream.capture.sink = true`; with no target device it connects to the
//! monitor ports of the default sink and follows default-sink changes
//! itself, and with an explicit output device (same one playback uses) it
//! follows that sink's monitor instead, so the reference always matches
//! what is actually being played back.
//!
//! The registry's "default" metadata object (`default.audio.sink` /
//! `default.audio.source`) is watched separately: when it changes, a
//! `BackendEvent::DefaultDeviceChanged` is sent so the engine reopens any
//! "system" (no explicit device) stream against the new default, instead of
//! relying solely on the session manager to relink an already-open stream.

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
        atomic::{AtomicU16, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};

use pipewire as pw;
use pw::{properties::properties, spa};

use super::backend::{
    AudioBackend, BackendError, BackendEvent, CaptureCallback, DeviceInfo, DeviceKind,
    PlaybackCallback, ReferenceCallback, ReferenceFrame, StreamKind, BLOCK_SAMPLES, SAMPLE_RATE,
};
use crate::sync::lock;

/// What the engine thread asks the PipeWire thread to do.
enum Cmd {
    Open {
        kind: StreamKind,
        device: Option<String>,
        handler: Handler,
        capture_channel: Option<u16>,
        selected_channel: Arc<AtomicU16>,
        reply: Sender<Result<(), BackendError>>,
    },
    Close(StreamKind),
    ReadLevel(Sender<Option<super::level::CaptureLevel>>),
    WriteLevel {
        expected: super::level::CaptureLevel,
        channels: Vec<f32>,
        reply: Sender<Result<super::level::CaptureLevel, BackendError>>,
        deadline: std::time::Instant,
    },
    Quit,
}

enum Handler {
    Capture(CaptureCallback),
    Reference(ReferenceCallback),
    Playback(PlaybackCallback),
}

/// Per stream user data living inside the PipeWire callbacks.
struct StreamData {
    handler: Handler,
    scratch: Vec<f32>,
    mono: Vec<f32>,
    reference: Vec<ReferenceFrame>,
    capture_channel: Option<u16>,
    selected_channel: Arc<AtomicU16>,
    selector: Option<super::channels::CaptureSelector>,
    channels: usize,
    kind: StreamKind,
    events: Sender<BackendEvent>,
}

/// Keeps the bound "default" metadata proxy and its property listener
/// alive; dropped only when the pipewire thread ends.
struct DefaultMetadataState {
    id: u32,
    _metadata: pw::metadata::Metadata,
    _listener: pw::metadata::MetadataListener,
}

/// The PipeWire backend handle owned by the engine.
pub struct PipeWireBackend {
    tx: pw::channel::Sender<Cmd>,
    devices: Arc<Mutex<HashMap<u32, DeviceInfo>>>,
    events: Option<Receiver<BackendEvent>>,
    thread: Option<JoinHandle<()>>,
    capture_channel: Option<u16>,
    selected_channel: Arc<AtomicU16>,
}

impl PipeWireBackend {
    /// Connects to the PipeWire daemon. Fails when no daemon is reachable.
    pub fn new() -> Result<PipeWireBackend, BackendError> {
        let (tx, rx) = pw::channel::channel::<Cmd>();
        let (ev_tx, ev_rx) = mpsc::channel();
        let devices: Arc<Mutex<HashMap<u32, DeviceInfo>>> = Arc::new(Mutex::new(HashMap::new()));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), BackendError>>();
        let devices_thread = devices.clone();
        let thread = std::thread::Builder::new()
            .name("icu-pipewire".into())
            .spawn(move || pw_thread(rx, ev_tx, devices_thread, ready_tx))
            .map_err(|e| BackendError(format!("cannot spawn pipewire thread: {e}")))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(BackendError("pipewire thread did not start".into())),
        }
        Ok(PipeWireBackend {
            tx,
            devices,
            events: Some(ev_rx),
            thread: Some(thread),
            capture_channel: None,
            selected_channel: Arc::new(AtomicU16::new(u16::MAX)),
        })
    }

    fn write_level(
        &self,
        expected: &super::level::CaptureLevel,
        channels: Vec<f32>,
    ) -> Result<super::level::CaptureLevel, BackendError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Cmd::WriteLevel {
                expected: expected.clone(),
                channels,
                reply,
                deadline: std::time::Instant::now() + Duration::from_millis(400),
            })
            .map_err(|_| super::level::unsupported())?;
        rx.recv_timeout(Duration::from_millis(500))
            .map_err(|_| super::level::unsupported())?
    }

    fn open(
        &mut self,
        kind: StreamKind,
        device: Option<&str>,
        handler: Handler,
    ) -> Result<(), BackendError> {
        let (reply, reply_rx) = mpsc::channel();
        self.tx
            .send(Cmd::Open {
                kind,
                device: device.map(str::to_string),
                handler,
                capture_channel: self.capture_channel,
                selected_channel: self.selected_channel.clone(),
                reply,
            })
            .map_err(|_| BackendError("pipewire thread gone".into()))?;
        reply_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| BackendError("pipewire thread did not answer".into()))?
    }
}

impl Drop for PipeWireBackend {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl AudioBackend for PipeWireBackend {
    fn name(&self) -> &'static str {
        "pipewire"
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        let map = lock(&self.devices);
        let mut list: Vec<DeviceInfo> = map.values().cloned().collect();
        list.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.description.cmp(&b.description)));
        list
    }

    fn set_capture_channel(&mut self, channel: Option<u16>) -> Result<(), BackendError> {
        self.capture_channel = channel;
        Ok(())
    }
    fn capture_channel_index(&self) -> Option<u16> {
        let channel = self.selected_channel.load(Ordering::Relaxed);
        (channel != u16::MAX).then_some(channel)
    }
    fn open_capture(
        &mut self,
        device: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        self.selected_channel.store(u16::MAX, Ordering::Relaxed);
        self.open(StreamKind::Capture, device, Handler::Capture(cb))
    }

    fn open_reference(
        &mut self,
        sink: Option<&str>,
        cb: ReferenceCallback,
    ) -> Result<(), BackendError> {
        self.open(StreamKind::Reference, sink, Handler::Reference(cb))
    }

    fn open_playback(
        &mut self,
        device: Option<&str>,
        cb: PlaybackCallback,
    ) -> Result<(), BackendError> {
        self.open(StreamKind::Playback, device, Handler::Playback(cb))
    }

    fn close(&mut self, stream: StreamKind) {
        let _ = self.tx.send(Cmd::Close(stream));
    }

    fn capture_level(&mut self) -> Result<Option<super::level::CaptureLevel>, BackendError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Cmd::ReadLevel(tx))
            .map_err(|_| super::level::unsupported())?;
        rx.recv_timeout(Duration::from_millis(500))
            .map_err(|_| super::level::unsupported())
    }
    fn set_capture_level(
        &mut self,
        expected: &super::level::CaptureLevel,
        db: f32,
    ) -> Result<super::level::CaptureLevel, BackendError> {
        let factor = 10.0f32.powf((db - expected.db) / 20.0);
        self.write_level(
            expected,
            expected.channels.iter().map(|v| v * factor).collect(),
        )
    }
    fn restore_capture_level(
        &mut self,
        expected: &super::level::CaptureLevel,
        original: &super::level::CaptureLevel,
    ) -> Result<(), BackendError> {
        self.write_level(expected, original.channels.clone())
            .map(|_| ())
    }

    fn take_events(&mut self) -> Option<Receiver<BackendEvent>> {
        self.events.take()
    }
}

/// A live stream plus its listener; dropping both disconnects it.
struct LiveStream {
    _listener: pw::stream::StreamListener<StreamData>,
    stream: pw::stream::StreamRc,
}

impl Drop for LiveStream {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
    }
}

/// Main loop, context, core and registry, each step naming itself in the
/// error. The context stays alive as long as the core; the caller keeps it.
type Connected = (
    pw::main_loop::MainLoopRc,
    pw::context::ContextRc,
    pw::core::CoreRc,
    pw::registry::RegistryRc,
);

fn connect() -> Result<Connected, BackendError> {
    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| BackendError(format!("main loop: {e}")))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| BackendError(format!("context: {e}")))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| BackendError(format!("connect: {e}")))?;
    let registry = core
        .get_registry_rc()
        .map_err(|e| BackendError(format!("registry: {e}")))?;
    Ok((mainloop, context, core, registry))
}

fn pw_thread(
    rx: pw::channel::Receiver<Cmd>,
    events: Sender<BackendEvent>,
    devices: Arc<Mutex<HashMap<u32, DeviceInfo>>>,
    ready: Sender<Result<(), BackendError>>,
) {
    pw::init();
    let (mainloop, context, core, registry) = match connect() {
        Ok(x) => x,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // module-rt, loaded by the client context, is what can lift the engine's
    // audio threads to real-time priority (`super::rt`). Registered only now
    // that the context is connected; declared after it, so the registration
    // is dropped first whenever this thread ends.
    let _rt = super::rt::register_context(context.as_raw_ptr() as *mut std::ffi::c_void);

    // Device list from the registry: every Audio/Source and Audio/Sink node.
    // The "default" metadata object separately tracks which node id is the
    // OS default input/output; watched here so a "system" stream can be
    // reopened when that changes underneath it (mirrors the WASAPI
    // backend's IMMNotificationClient default-endpoint watcher).
    let sources: Rc<RefCell<HashMap<u32, super::pipewire_level::Source>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let sources_add = sources.clone();
    let sources_rm = sources.clone();
    let links: Rc<RefCell<HashMap<u32, (u32, u32)>>> = Rc::new(RefCell::new(HashMap::new()));
    let links_add = links.clone();
    let links_rm = links.clone();
    let registry_sources = registry.clone();
    let dev_add = devices.clone();
    let ev_add = events.clone();
    let dev_rm = devices.clone();
    let ev_rm = events.clone();
    let registry_meta = registry.clone();
    let default_meta: Rc<RefCell<Option<DefaultMetadataState>>> = Rc::new(RefCell::new(None));
    let default_meta_rm = default_meta.clone();
    let last_default_sink: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let last_default_source: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let ev_default = events.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| {
            if obj.type_ == pw::types::ObjectType::Link {
                if let Some(p) = obj.props {
                    if let (Some(out), Some(input)) = (
                        p.get("link.output.node").and_then(|v| v.parse().ok()),
                        p.get("link.input.node").and_then(|v| v.parse().ok()),
                    ) {
                        links_add.borrow_mut().insert(obj.id, (out, input));
                    }
                }
                return;
            }
            if obj.type_ == pw::types::ObjectType::Metadata {
                let Some(props) = obj.props else { return };
                if props.get("metadata.name") != Some("default") || default_meta.borrow().is_some()
                {
                    return;
                }
                let Ok(metadata) = registry_meta.bind::<pw::metadata::Metadata, _>(obj) else {
                    return;
                };
                let last_sink = last_default_sink.clone();
                let last_source = last_default_source.clone();
                let ev_default = ev_default.clone();
                let listener = metadata
                    .add_listener_local()
                    .property(move |_subject, key, _type, value| {
                        let slot = match key {
                            Some("default.audio.sink") => &last_sink,
                            Some("default.audio.source") => &last_source,
                            _ => return 0,
                        };
                        let value = value.map(str::to_string);
                        let mut last = slot.borrow_mut();
                        let changed = last.is_some() && *last != value;
                        *last = value;
                        drop(last);
                        if changed {
                            let _ = ev_default.send(BackendEvent::DefaultDeviceChanged);
                        }
                        0
                    })
                    .register();
                *default_meta.borrow_mut() = Some(DefaultMetadataState {
                    id: obj.id,
                    _metadata: metadata,
                    _listener: listener,
                });
                return;
            }
            if obj.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = obj.props else { return };
            let kind = match props.get("media.class") {
                Some("Audio/Source" | "Audio/Source/Virtual") => DeviceKind::Input,
                Some("Audio/Sink") => DeviceKind::Output,
                _ => return,
            };
            let Some(name) = props.get("node.name") else {
                return;
            };
            if kind == DeviceKind::Input {
                if let Ok(node) = registry_sources.bind::<pw::node::Node, _>(obj) {
                    sources_add.borrow_mut().insert(
                        obj.id,
                        super::pipewire_level::Source::new(node, name.to_string()),
                    );
                }
            }
            let description = props
                .get("node.description")
                .or_else(|| props.get("node.nick"))
                .unwrap_or(name)
                .to_string();
            let info = DeviceInfo {
                id: name.to_string(),
                description,
                kind,
            };
            let list = {
                let mut map = lock(&dev_add);
                map.insert(obj.id, info);
                map.values().cloned().collect()
            };
            let _ = ev_add.send(BackendEvent::DevicesChanged(list));
        })
        .global_remove(move |id| {
            sources_rm.borrow_mut().remove(&id);
            links_rm.borrow_mut().remove(&id);
            let removed = {
                let mut map = lock(&dev_rm);
                map.remove(&id).map(|_| map.values().cloned().collect())
            };
            if let Some(list) = removed {
                let _ = ev_rm.send(BackendEvent::DevicesChanged(list));
            }
            // WirePlumber restarting removes and re-adds the "default"
            // metadata global under a new id; drop the stale binding so the
            // `global` handler above rebinds it when it reappears.
            let mut slot = default_meta_rm.borrow_mut();
            if slot.as_ref().is_some_and(|s| s.id == id) {
                *slot = None;
            }
        })
        .register();

    // Wait for the initial registry roundtrip so `devices()` is populated
    // before `new()` returns.
    let pending = Rc::new(RefCell::new(true));
    let pending_done = pending.clone();
    let loop_weak = mainloop.downgrade();
    let _core_listener = core
        .add_listener_local()
        .done(move |_id, _seq| {
            *pending_done.borrow_mut() = false;
            if let Some(l) = loop_weak.upgrade() {
                l.quit();
            }
        })
        .register();
    if let Err(e) = core.sync(0) {
        let _ = ready.send(Err(BackendError(format!("sync: {e}"))));
        return;
    }
    mainloop.run();
    drop(_core_listener);

    let streams: Rc<RefCell<HashMap<StreamKind, LiveStream>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let loop_weak = mainloop.downgrade();
    let streams_cmd = streams.clone();
    let core_cmd = core.clone();
    let events_cmd = events.clone();
    type PendingLevel = (
        i32,
        String,
        Sender<Result<super::level::CaptureLevel, BackendError>>,
    );
    let pending_levels: Rc<RefCell<Vec<PendingLevel>>> = Rc::new(RefCell::new(Vec::new()));
    let pending_done = pending_levels.clone();
    let sources_done = sources.clone();
    let _level_sync = core
        .add_listener_local()
        .done(move |_, seq| {
            let mut pending = pending_done.borrow_mut();
            let Some(index) = pending.iter().position(|(id, _, _)| *id == seq.seq()) else {
                return;
            };
            let (_, name, reply) = pending.swap_remove(index);
            {
                let level = sources_done
                    .borrow()
                    .values()
                    .filter_map(|s| s.level.borrow().clone())
                    .find(|l| l.device == name);
                for source in sources_done.borrow().values() {
                    source.finish_write();
                }
                let _ = reply.send(level.ok_or_else(super::level::unsupported));
            }
        })
        .register();
    let _receiver = rx.attach(mainloop.loop_(), move |cmd| match cmd {
        Cmd::Open {
            kind,
            device,
            handler,
            capture_channel,
            selected_channel,
            reply,
        } => {
            let mut map = streams_cmd.borrow_mut();
            map.remove(&kind);
            let result = open_stream(
                core_cmd.clone(),
                kind,
                device.as_deref(),
                handler,
                capture_channel,
                selected_channel,
                events_cmd.clone(),
            );
            let _ = match result {
                Ok(live) => {
                    map.insert(kind, live);
                    reply.send(Ok(()))
                }
                Err(e) => reply.send(Err(e)),
            };
        }
        Cmd::ReadLevel(reply) => {
            let id = streams_cmd
                .borrow()
                .get(&StreamKind::Capture)
                .map(|s| s.stream.node_id());
            let source = id.and_then(|id| {
                links
                    .borrow()
                    .values()
                    .find(|(_, input)| *input == id)
                    .map(|(out, _)| *out)
            });
            let level = source.and_then(|id| {
                sources
                    .borrow()
                    .get(&id)
                    .and_then(|s| s.level.borrow().clone())
            });
            let _ = reply.send(level);
        }
        Cmd::WriteLevel {
            expected,
            channels,
            reply,
            deadline,
        } => {
            if std::time::Instant::now() > deadline {
                let _ = reply.send(Err(super::level::unsupported()));
                return;
            }
            let sources = sources.borrow();
            let source = sources.values().find(|s| {
                s.level
                    .borrow()
                    .as_ref()
                    .is_some_and(|l| l.device == expected.device)
            });
            let result = source
                .ok_or_else(super::level::unsupported)
                .and_then(|s| s.set(&expected, channels));
            match result {
                Ok(()) => match core_cmd.sync(0) {
                    Ok(seq) => {
                        pending_levels
                            .borrow_mut()
                            .push((seq.seq(), expected.device, reply))
                    }
                    Err(_) => {
                        let _ = reply.send(Err(super::level::unsupported()));
                    }
                },
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
            }
        }
        Cmd::Close(kind) => {
            streams_cmd.borrow_mut().remove(&kind);
        }
        Cmd::Quit => {
            streams_cmd.borrow_mut().clear();
            if let Some(l) = loop_weak.upgrade() {
                l.quit();
            }
        }
    });

    let _ = ready.send(Ok(()));
    mainloop.run();
    streams.borrow_mut().clear();
}

fn open_stream(
    core: pw::core::CoreRc,
    kind: StreamKind,
    device: Option<&str>,
    handler: Handler,
    capture_channel: Option<u16>,
    selected_channel: Arc<AtomicU16>,
    events: Sender<BackendEvent>,
) -> Result<LiveStream, BackendError> {
    let (name, category, direction) = match kind {
        StreamKind::Capture => ("icu-capture", "Capture", spa::utils::Direction::Input),
        StreamKind::Reference => ("icu-aec-reference", "Capture", spa::utils::Direction::Input),
        StreamKind::Playback => ("icu-playback", "Playback", spa::utils::Direction::Output),
    };
    let latency = format!("{BLOCK_SAMPLES}/{SAMPLE_RATE}");
    let rate = format!("1/{SAMPLE_RATE}");
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => category,
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::APP_NAME => "ICU",
        *pw::keys::NODE_NAME => name,
        *pw::keys::NODE_GROUP => "icu",
        *pw::keys::NODE_LATENCY => latency.as_str(),
        *pw::keys::NODE_RATE => rate.as_str(),

    };
    if kind == StreamKind::Playback {
        props.insert(*pw::keys::AUDIO_CHANNELS, "1");
    }
    if kind == StreamKind::Reference {
        props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    }
    if let Some(target) = device {
        props.insert(*pw::keys::TARGET_OBJECT, target);
        props.insert("node.dont-fallback", "true");
    }

    let stream = pw::stream::StreamRc::new(core, name, props)
        .map_err(|e| BackendError(format!("{name}: {e}")))?;

    let data = StreamData {
        handler,
        // Fixed scratch is allocated before real-time processing. Larger
        // graph quanta are handled in bounded chunks, without Vec growth.
        scratch: vec![0.0; 1024 * 64],
        mono: vec![0.0; 8192],
        reference: vec![[0.0; 8]; 1024],
        capture_channel,
        selected_channel,
        selector: None,
        channels: 0,
        kind,
        events,
    };
    let listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_stream, data, _old, new| {
            if let pw::stream::StreamState::Error(msg) = new {
                let _ = data.events.send(BackendEvent::StreamError {
                    stream: data.kind,
                    message: msg,
                });
            }
        })
        .param_changed(|_, data, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                data.channels = 0;
                return;
            };
            let mut format = spa::param::audio::AudioInfoRaw::new();
            if format.parse(param).is_err() {
                return;
            }
            let channels = format.channels() as usize;
            tracing::debug!(stream = ?data.kind, channels, format = ?format.format(), positions = ?&format.position()[..channels.min(64)], "negotiated native audio layout");
            let max = if data.kind == StreamKind::Reference {
                8
            } else {
                64
            };
            if !(1..=max).contains(&channels) || format.rate() != SAMPLE_RATE {
                data.channels = 0;
                let _ = data.events.send(BackendEvent::StreamError {
                    stream: data.kind,
                    message: format!(
                        "Nicht unterstütztes Audioformat: {} Hz, {channels} Kanäle",
                        format.rate()
                    ),
                });
                return;
            }
            if data.kind == StreamKind::Capture {
                match super::channels::CaptureSelector::new(data.capture_channel, channels) {
                    Ok(selector) => data.selector = Some(selector),
                    Err(error) => {
                        data.channels = 0;
                        let _ = data.events.send(BackendEvent::StreamError {
                            stream: data.kind,
                            message: error.to_string(),
                        });
                        return;
                    }
                }
            }
            data.channels = channels;
        })
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = buffer.requested() as usize;
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let d = &mut datas[0];
            let channels = data.channels;
            if channels == 0 {
                return;
            }
            let size = d.chunk().size() as usize;
            let offset = d.chunk().offset() as usize;
            match &mut data.handler {
                Handler::Capture(cb) => {
                    let Some(bytes) = d.data() else {
                        return;
                    };
                    let offset = offset.min(bytes.len());
                    let size = size.min(bytes.len() - offset);
                    for raw in bytes[offset..offset + size].chunks(1024 * channels * 4) {
                        let frames = raw.len() / (channels * 4);
                        let count = frames * channels;
                        for (sample, raw) in
                            data.scratch[..count].iter_mut().zip(raw.as_chunks::<4>().0.iter())
                        {
                            *sample = f32::from_le_bytes(*raw);
                        }
                        let Some(selector) = &mut data.selector else {
                            return;
                        };
                        selector.update(&data.scratch[..count], &data.selected_channel, &data.events);
                        for (sample, frame) in data.mono[..frames]
                            .iter_mut()
                            .zip(data.scratch[..count].chunks_exact(channels))
                        {
                            *sample = frame[selector.channel()];
                        }
                        cb(&data.mono[..frames]);
                    }
                }
                Handler::Reference(cb) => {
                    let Some(bytes) = d.data() else {
                        return;
                    };
                    let offset = offset.min(bytes.len());
                    let size = size.min(bytes.len() - offset);
                    for raw in bytes[offset..offset + size].chunks(1024 * channels * 4) {
                        let frames = raw.len() / (channels * 4);
                        for (frame, raw) in data.reference[..frames]
                            .iter_mut()
                            .zip(raw.chunks_exact(channels * 4))
                        {
                            frame.fill(0.0);
                            for (sample, bytes) in
                                frame[..channels].iter_mut().zip(raw.as_chunks::<4>().0.iter())
                            {
                                *sample = f32::from_le_bytes(*bytes);
                            }
                        }
                        cb(&data.reference[..frames], channels);
                    }
                }
                Handler::Playback(cb) => {
                    let Some(bytes) = d.data() else {
                        return;
                    };
                    let capacity = bytes.len() / 4;
                    let n = if requested == 0 {
                        BLOCK_SAMPLES.min(capacity)
                    } else {
                        requested.min(capacity)
                    };
                    for raw in bytes[..n * 4].chunks_mut(data.mono.len() * 4) {
                        let frames = raw.len() / 4;
                        data.mono[..frames].fill(0.0);
                        cb(&mut data.mono[..frames]);
                        for (dst, sample) in raw.as_chunks_mut::<4>().0.iter_mut().zip(&data.mono[..frames]) {
                            dst.copy_from_slice(&sample.to_le_bytes());
                        }
                    }
                    let chunk = d.chunk_mut();
                    *chunk.offset_mut() = 0;
                    *chunk.stride_mut() = 4;
                    *chunk.size_mut() = (n * 4) as u32;
                }
            }
        })
        .register()
        .map_err(|e| BackendError(format!("{name} listener: {e}")))?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(SAMPLE_RATE);
    if kind == StreamKind::Playback {
        audio_info.set_channels(1);
        let mut position = [0; spa::param::audio::MAX_CHANNELS];
        position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
        audio_info.set_position(position);
    }
    // Capture/reference leave channels unspecified, negotiating the native
    // endpoint layout instead of asking PipeWire to downmix it to mono.
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| BackendError(format!("format pod: {e:?}")))?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values)
        .ok_or_else(|| BackendError("format pod invalid".into()))?];

    stream
        .connect(
            direction,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| BackendError(format!("{name} connect: {e}")))?;

    Ok(LiveStream {
        _listener: listener,
        stream,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a task-owned stereo loopback (L=-R) from pw-cat before this
    /// test. It exercises actual PipeWire format negotiation and callbacks,
    /// while no physical microphone or speaker is opened.
    #[test]
    #[ignore = "needs task-owned ICU_CAPTURE_TEST_SOURCE and ICU_REFERENCE_TEST_SINK with stereo antiphase audio"]
    fn native_stereo_capture_and_reference_keep_independent_channels() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .try_init();
        let source = std::env::var("ICU_CAPTURE_TEST_SOURCE").unwrap();
        let sink = std::env::var("ICU_REFERENCE_TEST_SINK").unwrap();
        assert!(source.starts_with("icu-voice-test-"));
        assert!(sink.starts_with("icu-voice-test-"));
        let capture = Arc::new(AtomicUsize::new(0));
        let reference = Arc::new(AtomicUsize::new(0));
        let reference_active = Arc::new(AtomicUsize::new(0));
        let reference_total = Arc::new(AtomicUsize::new(0));
        let reference_same = Arc::new(AtomicUsize::new(0));
        let reference_energy = Arc::new(Mutex::new([0.0f64; 3]));
        let c = capture.clone();
        let r = reference.clone();
        let active = reference_active.clone();
        let total = reference_total.clone();
        let same = reference_same.clone();
        let energy = reference_energy.clone();
        let mut backend = PipeWireBackend::new().unwrap();
        backend.set_capture_channel(Some(1)).unwrap();
        backend
            .open_capture(
                Some(&source),
                Box::new(move |samples| {
                    c.fetch_add(
                        samples.iter().filter(|sample| sample.abs() > 0.05).count(),
                        Ordering::Relaxed,
                    );
                }),
            )
            .unwrap();
        backend
            .open_reference(
                Some(&sink),
                Box::new(move |frames, channels| {
                    assert_eq!(channels, 2);
                    total.fetch_add(frames.len(), Ordering::Relaxed);
                    if let Ok(mut energy) = energy.try_lock() {
                        for frame in frames {
                            energy[0] += f64::from(frame[0]).powi(2);
                            energy[1] += f64::from(frame[1]).powi(2);
                            energy[2] += f64::from(frame[0]) * f64::from(frame[1]);
                        }
                    }
                    active.fetch_add(
                        frames
                            .iter()
                            .filter(|f| f[0].abs() > 0.05 || f[1].abs() > 0.05)
                            .count(),
                        Ordering::Relaxed,
                    );
                    same.fetch_add(
                        frames
                            .iter()
                            .filter(|f| f[0].abs() > 0.05 && (f[0] - f[1]).abs() < 0.001)
                            .count(),
                        Ordering::Relaxed,
                    );
                    r.fetch_add(
                        frames
                            .iter()
                            .filter(|frame| {
                                frame[0].abs() > 0.02
                                    && frame[1].abs() > 0.02
                                    && frame[0] * frame[1] < 0.0
                            })
                            .count(),
                        Ordering::Relaxed,
                    );
                }),
            )
            .unwrap();
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(4)
            && (capture.load(Ordering::Relaxed) < 4800 || reference.load(Ordering::Relaxed) < 4800)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(backend.capture_channel_index(), Some(1));
        eprintln!(
            "native capture active samples {}, antiphase reference frames {}",
            capture.load(Ordering::Relaxed),
            reference.load(Ordering::Relaxed)
        );
        eprintln!(
            "reference active frames {}, identical stereo {}",
            reference_active.load(Ordering::Relaxed),
            reference_same.load(Ordering::Relaxed)
        );
        eprintln!(
            "reference total frames {}",
            reference_total.load(Ordering::Relaxed)
        );
        let energy = *reference_energy.lock().unwrap();
        let correlation = energy[2] / (energy[0] * energy[1]).sqrt().max(1e-12);
        eprintln!("reference channel correlation {correlation:.6}");
        // Native per-channel volumes may differ; preserve those values
        // instead of rewriting the user's mixer balance for this test.
        assert!(correlation < -0.995, "independent antiphase channels lost");
        assert!(
            capture.load(Ordering::Relaxed) > 4800,
            "second input channel not captured"
        );
        assert!(
            reference.load(Ordering::Relaxed) > 4800,
            "antiphase reference erased"
        );
    }

    /// The runner first feeds the left input, then only the right input.
    #[test]
    #[ignore = "needs the temporary virtual endpoints from tests/native_channels.py"]
    fn native_automatic_channel_change_keeps_capture_running() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let source = std::env::var("ICU_CAPTURE_TEST_SOURCE").unwrap();
        assert!(source.starts_with("icu-voice-test-"));
        let samples = Arc::new(AtomicUsize::new(0));
        let count = samples.clone();
        let mut backend = PipeWireBackend::new().unwrap();
        let events = backend.take_events().unwrap();
        backend.set_capture_channel(None).unwrap();
        backend
            .open_capture(
                Some(&source),
                Box::new(move |input| {
                    count.fetch_add(
                        input.iter().filter(|sample| sample.abs() > 0.01).count(),
                        Ordering::Relaxed,
                    );
                }),
            )
            .unwrap();
        let start = std::time::Instant::now();
        let mut saw_left = false;
        let mut switched_at = None;
        while start.elapsed() < Duration::from_secs(8) {
            saw_left |= backend.capture_channel_index() == Some(0);
            for event in events.try_iter() {
                match event {
                    BackendEvent::CaptureChannelChanged { channel: 1 } if saw_left => {
                        switched_at = Some(samples.load(Ordering::Relaxed));
                    }
                    BackendEvent::StreamDiscontinuity { .. } | BackendEvent::StreamError { .. } => {
                        panic!("channel selection must preserve the native stream: {event:?}");
                    }
                    _ => {}
                }
            }
            if switched_at.is_some_and(|before| samples.load(Ordering::Relaxed) > before + 4800) {
                assert_eq!(backend.capture_channel_index(), Some(1));
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("capture did not continue after switching inputs: left={saw_left}, switch={switched_at:?}");
    }

    /// The case from the review (L6): PipeWire's library is there, its
    /// daemon is not. The backend fails, and no freed context stays behind
    /// for the silent fallback's threads to promote themselves through.
    /// Run as `PIPEWIRE_REMOTE=icu-no-such-daemon cargo test -p icu-client
    /// --lib a_failed_connect -- --ignored`.
    #[test]
    #[ignore = "needs PIPEWIRE_REMOTE pointing at no daemon"]
    fn a_failed_connect_leaves_no_rt_context() {
        assert_eq!(
            std::env::var("PIPEWIRE_REMOTE").as_deref(),
            Ok("icu-no-such-daemon"),
            "would connect to the real daemon"
        );
        assert!(PipeWireBackend::new().is_err());
        assert!(super::super::rt::registered_context().is_null());
    }
}

#[cfg(test)]
mod level_live_tests {
    use super::*;
    #[test]
    #[ignore = "needs a task-owned PipeWire source named icu-feedback-test-*; never changes a real microphone"]
    fn capture_level_roundtrip_on_virtual_source() {
        let source = std::env::var("ICU_LEVEL_TEST_SOURCE").expect("explicit virtual source");
        assert!(source.starts_with("icu-feedback-test-"));
        let mut backend = PipeWireBackend::new().unwrap();
        backend
            .open_capture(Some(&source), Box::new(|_| {}))
            .unwrap();
        let start = std::time::Instant::now();
        let original = loop {
            if let Some(level) = backend.capture_level().unwrap() {
                break level;
            }
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "source never linked or exposed volume"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(original.device, source);
        let changed = backend
            .set_capture_level(&original, original.db - 1.0)
            .unwrap();
        let restored = backend.restore_capture_level(&changed, &original);
        assert!(restored.is_ok(), "restore: {restored:?}");
        assert!(
            (changed.db - original.db + 1.0).abs() < 0.05,
            "acknowledged actual change: {changed:?}"
        );
        assert!(backend.capture_level().unwrap().unwrap().same(&original));
    }
}
