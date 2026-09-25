//! Linux audio backend on pipewire-rs 0.10 (`Client/IMPLEMENTATION.md` §6).
//!
//! One dedicated thread runs the PipeWire main loop and owns the core, the
//! registry and up to three streams (capture, AEC reference, playback). The
//! engine talks to it through a [`pipewire::channel`]; every stream's
//! `process` callback runs on PipeWire's real-time data thread and only
//! forwards the block to the engine supplied closure.
//!
//! Stream properties (Spike 8): `node.latency = 240/48000`,
//! `node.rate = 1/48000`, `node.group = icu`, `media.role = Communication`,
//! format F32LE mono 48 kHz. The reference stream sets
//! `stream.capture.sink = true`; with no target device it connects to the
//! monitor ports of the default sink and follows default-sink changes
//! itself, and with an explicit output device (same one playback uses) it
//! follows that sink's monitor instead, so the reference always matches
//! what is actually being played back.

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
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
    PlaybackCallback, StreamKind, BLOCK_SAMPLES, SAMPLE_RATE,
};
use crate::sync::lock;

/// What the engine thread asks the PipeWire thread to do.
enum Cmd {
    Open {
        kind: StreamKind,
        device: Option<String>,
        handler: Handler,
        reply: Sender<Result<(), BackendError>>,
    },
    Close(StreamKind),
    Quit,
}

enum Handler {
    Capture(CaptureCallback),
    Playback(PlaybackCallback),
}

/// Per stream user data living inside the PipeWire callbacks.
struct StreamData {
    handler: Handler,
    scratch: Vec<f32>,
    kind: StreamKind,
    events: Sender<BackendEvent>,
}

/// The PipeWire backend handle owned by the engine.
pub struct PipeWireBackend {
    tx: pw::channel::Sender<Cmd>,
    devices: Arc<Mutex<HashMap<u32, DeviceInfo>>>,
    events: Option<Receiver<BackendEvent>>,
    thread: Option<JoinHandle<()>>,
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
        })
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

    fn open_capture(
        &mut self,
        device: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        self.open(StreamKind::Capture, device, Handler::Capture(cb))
    }

    fn open_reference(
        &mut self,
        sink: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        self.open(StreamKind::Reference, sink, Handler::Capture(cb))
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

fn pw_thread(
    rx: pw::channel::Receiver<Cmd>,
    events: Sender<BackendEvent>,
    devices: Arc<Mutex<HashMap<u32, DeviceInfo>>>,
    ready: Sender<Result<(), BackendError>>,
) {
    pw::init();
    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(l) => l,
        Err(e) => {
            let _ = ready.send(Err(BackendError(format!("main loop: {e}"))));
            return;
        }
    };
    let context = match pw::context::ContextRc::new(&mainloop, None) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(BackendError(format!("context: {e}"))));
            return;
        }
    };
    // module-rt, loaded by the client context, is what can lift the engine's
    // audio threads to real-time priority (`super::rt`).
    super::rt::register_context(context.as_raw_ptr() as *mut std::ffi::c_void);
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(BackendError(format!("connect: {e}"))));
            return;
        }
    };
    let registry = match core.get_registry_rc() {
        Ok(r) => r,
        Err(e) => {
            let _ = ready.send(Err(BackendError(format!("registry: {e}"))));
            return;
        }
    };

    // Device list from the registry: every Audio/Source and Audio/Sink node.
    let dev_add = devices.clone();
    let ev_add = events.clone();
    let dev_rm = devices.clone();
    let ev_rm = events.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| {
            if obj.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = obj.props else { return };
            let kind = match props.get("media.class") {
                Some("Audio/Source") => DeviceKind::Input,
                Some("Audio/Sink") => DeviceKind::Output,
                _ => return,
            };
            let Some(name) = props.get("node.name") else {
                return;
            };
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
            let removed = {
                let mut map = lock(&dev_rm);
                map.remove(&id).map(|_| map.values().cloned().collect())
            };
            if let Some(list) = removed {
                let _ = ev_rm.send(BackendEvent::DevicesChanged(list));
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
    let _receiver = rx.attach(mainloop.loop_(), move |cmd| match cmd {
        Cmd::Open {
            kind,
            device,
            handler,
            reply,
        } => {
            let mut map = streams_cmd.borrow_mut();
            map.remove(&kind);
            let result = open_stream(
                core_cmd.clone(),
                kind,
                device.as_deref(),
                handler,
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
        *pw::keys::AUDIO_CHANNELS => "1",
    };
    if kind == StreamKind::Reference {
        props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    }
    if let Some(target) = device {
        props.insert(*pw::keys::TARGET_OBJECT, target);
    }

    let stream = pw::stream::StreamRc::new(core, name, props)
        .map_err(|e| BackendError(format!("{name}: {e}")))?;

    let data = StreamData {
        handler,
        // Sized for PipeWire's largest default quantum (8192) so the
        // real-time callback never reallocates, whatever the graph runs.
        scratch: Vec::with_capacity(8192),
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
            match &mut data.handler {
                Handler::Capture(cb) => {
                    let n = d.chunk().size() as usize / 4;
                    let offset = d.chunk().offset() as usize;
                    let Some(bytes) = d.data() else { return };
                    let n = n.min(bytes.len().saturating_sub(offset) / 4);
                    data.scratch.clear();
                    data.scratch.extend(
                        bytes[offset..offset + n * 4]
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|c| f32::from_le_bytes(*c)),
                    );
                    cb(&data.scratch);
                }
                Handler::Playback(cb) => {
                    let Some(bytes) = d.data() else { return };
                    let capacity = bytes.len() / 4;
                    let n = if requested == 0 {
                        BLOCK_SAMPLES.min(capacity)
                    } else {
                        requested.min(capacity)
                    };
                    data.scratch.clear();
                    data.scratch.resize(n, 0.0);
                    cb(&mut data.scratch);
                    for (dst, s) in bytes
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(data.scratch.iter())
                    {
                        *dst = s.to_le_bytes();
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
    audio_info.set_channels(1);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    audio_info.set_position(position);
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
