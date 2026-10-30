//! Native PipeWire control, on the guardian thread (never an audio callback).
//! Only transient softVolumes are attenuated. WirePlumber persists channelVolumes,
//! not softVolumes; the user's scalar volume, mute and channel balance survive.
use super::{same, Background, Gain};
#[path = "linux_meter.rs"]
mod meter;
use meter::Meter;
use pipewire::{self as pw, spa};
use spa::pod::{Object, Property, Value, ValueArray};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

const MAX_NODES: usize = 256;
const MAX_LINKS: usize = 2048;

#[derive(Clone, Default)]
struct Observed {
    channels: Vec<f32>,
    soft: Vec<f32>,
    muted: bool,
    running: bool,
}

struct Session {
    _listener: pw::node::NodeListener,
    node: pw::node::Node,
    observed: Rc<RefCell<Observed>>,
    gain: Option<Gain>,
    serial: u64,
    own: bool,
    known_soft: Vec<f32>,
    pending_until: Option<Instant>,
    previous_owned: bool,
    restoring: bool,
    expected_soft: Rc<RefCell<Option<Vec<f32>>>>,
    write_ack: Rc<Cell<bool>>,
    metered_factor: f32,
    baseline_energy: Option<(f32, Instant)>,
}

impl Session {
    fn new(node: pw::node::Node, serial: u64, own: bool) -> Self {
        let observed = Rc::new(RefCell::new(Observed::default()));
        let expected_soft = Rc::new(RefCell::new(None::<Vec<f32>>));
        let write_ack = Rc::new(Cell::new(false));
        let expected_event = expected_soft.clone();
        let ack_event = write_ack.clone();
        let props = observed.clone();
        let state = observed.clone();
        let listener = node
            .add_listener_local()
            .info(move |info| {
                state.borrow_mut().running = matches!(info.state(), pw::node::NodeState::Running)
            })
            .param(move |_, _, _, _, pod| {
                let Some(pod) = pod else { return };
                let Ok((_, Value::Object(object))) =
                    spa::pod::deserialize::PodDeserializer::deserialize_from::<Value>(
                        pod.as_bytes(),
                    )
                else {
                    return;
                };
                let mut current = props.borrow_mut();
                for property in object.properties {
                    match (property.key, property.value) {
                        (
                            spa::sys::SPA_PROP_channelVolumes,
                            Value::ValueArray(ValueArray::Float(value)),
                        ) if valid(&value) => current.channels = value,
                        (
                            spa::sys::SPA_PROP_softVolumes,
                            Value::ValueArray(ValueArray::Float(value)),
                        ) if value.is_empty() || valid(&value) => {
                            if expected_event
                                .borrow()
                                .as_ref()
                                .is_some_and(|expected| same(expected, &value))
                            {
                                ack_event.set(true);
                            }
                            current.soft = value;
                        }
                        (spa::sys::SPA_PROP_mute, Value::Bool(value)) => current.muted = value,
                        _ => {}
                    }
                }
            })
            .register();
        node.subscribe_params(&[spa::param::ParamType::Props]);
        node.enum_params(0, Some(spa::param::ParamType::Props), 0, 1);
        Self {
            node,
            _listener: listener,
            observed,
            gain: None,
            serial,
            own,
            known_soft: Vec::new(),
            pending_until: None,
            previous_owned: false,
            restoring: false,
            expected_soft,
            write_ack,
            metered_factor: 1.0,
            baseline_energy: None,
        }
    }

    fn write(&self, key: u32, channels: Vec<f32>) -> Result<(), String> {
        if !valid(&channels) {
            return Err("Ungültiger PipeWire-Kanalpegel".into());
        }
        let object = Value::Object(Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Props,
            id: spa::sys::SPA_PARAM_Props,
            properties: vec![Property::new(
                key,
                Value::ValueArray(ValueArray::Float(channels)),
            )],
        });
        let bytes = spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &object,
        )
        .map_err(|e| e.to_string())?
        .0
        .into_inner();
        let pod = spa::pod::Pod::from_bytes(&bytes).ok_or("Ungültiger PipeWire-Pegelparameter")?;
        self.node.set_param(spa::param::ParamType::Props, 0, pod);
        self.node
            .enum_params(0, Some(spa::param::ParamType::Props), 0, 1);
        Ok(())
    }

    // A concurrent user change wins. Channel writes clear have_soft_volume in
    // PipeWire; another soft-volume writer is detected by compare-and-restore.
    fn release(&mut self) -> Result<(), String> {
        let Some(gain) = self.gain.as_ref() else {
            return Ok(());
        };
        let current = self.observed.borrow().clone();
        if self.restoring
            && same(&current.soft, &gain.baseline)
            && same(&current.channels, &gain.baseline)
        {
            self.gain = None;
            self.restoring = false;
            self.known_soft.clear();
            return Ok(());
        }
        if gain.yielded
            || !same(&current.channels, &gain.baseline)
            || (!gain.owns(&current.soft)
                && !(self.pending_until.is_some_and(|at| Instant::now() < at)
                    && !self.write_ack.get()
                    && same(&current.soft, &gain.previous)))
        {
            // A competing user/controller owns this value now. Never overwrite it.
            self.gain = None;
            self.restoring = false;
            return Ok(());
        }
        let baseline = gain.baseline.clone();
        *self.expected_soft.borrow_mut() = Some(baseline.clone());
        self.write_ack.set(false);
        self.write(spa::sys::SPA_PROP_softVolumes, baseline.clone())?;
        self.write(spa::sys::SPA_PROP_channelVolumes, baseline)?;
        // Keep the journal/ownership until a Props readback confirms the restore.
        self.restoring = true;
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    serial: u64,
    gain: Gain,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Journal {
    cookie: u32,
    sessions: Vec<Saved>,
}

struct Link {
    _listener: pw::link::LinkListener,
    _proxy: pw::link::Link,
    state: Rc<Cell<(u32, u32, bool)>>,
}
struct Metadata {
    id: u32,
    _listener: pw::metadata::MetadataListener,
    _proxy: pw::metadata::Metadata,
}

#[derive(Default)]
struct Graph {
    sessions: HashMap<u32, Session>,
    sinks: HashMap<u32, String>,
    links: HashMap<u32, Link>,
    metadata: Option<Metadata>,
}

/// Native per-application control and passive RMS monitoring. Missing or stale
/// monitor data returns None, selecting the UI's named Medium fallback.
pub(super) struct Native {
    _registry_listener: pw::registry::Listener,
    _core_listener: pw::core::Listener,
    _registry: pw::registry::RegistryRc,
    core: pw::core::CoreRc,
    _context: pw::context::ContextRc,
    mainloop: pw::main_loop::MainLoopRc,
    graph: Rc<RefCell<Graph>>,
    default_sink: Rc<RefCell<Option<String>>>,
    error: Rc<RefCell<Option<String>>>,
    done: Rc<Cell<i32>>,
    selected: Option<u32>,
    cookie: Rc<Cell<u32>>,
    journal: PathBuf,
    last_journal: RefCell<Vec<u8>>,
    meters: HashMap<u32, Meter>,
    recovery_complete: bool,
}

fn valid(values: &[f32]) -> bool {
    !values.is_empty()
        && values.len() <= 64
        && values
            .iter()
            .all(|v| v.is_finite() && (0.0..=4.0).contains(v))
}

fn is_icu(props: &spa::utils::dict::DictRef) -> bool {
    [
        "node.group",
        "application.name",
        "application.id",
        "application.process.binary",
        "node.name",
    ]
    .into_iter()
    .filter_map(|key| props.get(key))
    .any(|value| {
        let name = value.to_ascii_lowercase();
        name == "icu"
            || name == "icu-client"
            || name.starts_with("icu-")
            || name.starts_with("icu.")
    })
}

/// Resolve actual active links, including intervening filter nodes, and reject
/// fan-out to several output devices. Never rely on a stream's requested target.
fn routed_sinks(start: u32, links: &[(u32, u32)], sinks: &HashSet<u32>) -> HashSet<u32> {
    let mut pending = vec![start];
    let mut visited = HashSet::new();
    let mut result = HashSet::new();
    while let Some(node) = pending.pop() {
        if !visited.insert(node) {
            continue;
        }
        if visited.len() > MAX_NODES {
            return HashSet::new();
        }
        if sinks.contains(&node) {
            result.insert(node);
            continue;
        }
        pending.extend(
            links
                .iter()
                .filter(|(out, _)| *out == node)
                .map(|(_, input)| *input),
        );
    }
    result
}

impl Native {
    pub fn new() -> Result<Self, String> {
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
        let core = context
            .connect_rc(None)
            .map_err(|e| format!("PipeWire: {e}"))?;
        let registry = core.get_registry_rc().map_err(|e| e.to_string())?;
        let graph = Rc::new(RefCell::new(Graph::default()));
        let default_sink = Rc::new(RefCell::new(None));
        let added = graph.clone();
        let removed = graph.clone();
        let registry_add = registry.clone();
        let default_add = default_sink.clone();
        let default_remove = default_sink.clone();
        let listener = registry
            .add_listener_local()
            .global(move |object| {
                let Some(props) = object.props else { return };
                let mut graph = added.borrow_mut();
                match object.type_ {
                    pw::types::ObjectType::Node => {
                        if graph.sessions.len() + graph.sinks.len() >= MAX_NODES {
                            return;
                        }
                        match props.get("media.class") {
                            Some("Audio/Sink" | "Audio/Sink/Virtual") => {
                                if let Some(name) = props.get("node.name") {
                                    graph.sinks.insert(object.id, name.to_string());
                                }
                            }
                            Some("Stream/Output/Audio") => {
                                let Some(serial) =
                                    props.get("object.serial").and_then(|v| v.parse().ok())
                                else {
                                    return;
                                };
                                if let Ok(node) = registry_add.bind::<pw::node::Node, _>(object) {
                                    graph.sessions.insert(
                                        object.id,
                                        Session::new(node, serial, is_icu(props)),
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                    pw::types::ObjectType::Link => {
                        if graph.links.len() >= MAX_LINKS {
                            return;
                        }
                        if let Ok(proxy) = registry_add.bind::<pw::link::Link, _>(object) {
                            let state = Rc::new(Cell::new((0, 0, false)));
                            let update = state.clone();
                            let listener = proxy
                                .add_listener_local()
                                .info(move |info| {
                                    update.set((
                                        info.output_node_id(),
                                        info.input_node_id(),
                                        matches!(info.state(), pw::link::LinkState::Active),
                                    ));
                                })
                                .register();
                            graph.links.insert(
                                object.id,
                                Link {
                                    _proxy: proxy,
                                    _listener: listener,
                                    state,
                                },
                            );
                        }
                    }
                    pw::types::ObjectType::Metadata
                        if props.get("metadata.name") == Some("default")
                            && graph.metadata.is_none() =>
                    {
                        if let Ok(proxy) = registry_add.bind::<pw::metadata::Metadata, _>(object) {
                            let current = default_add.clone();
                            let listener = proxy
                                .add_listener_local()
                                .property(move |subject, key, _, value| {
                                    if subject == 0 && key == Some("default.audio.sink") {
                                        *current.borrow_mut() = value
                                            .and_then(|v| {
                                                serde_json::from_str::<serde_json::Value>(v).ok()
                                            })
                                            .and_then(|v| {
                                                v.get("name")
                                                    .and_then(|n| n.as_str())
                                                    .map(str::to_string)
                                            });
                                    }
                                    0
                                })
                                .register();
                            graph.metadata = Some(Metadata {
                                id: object.id,
                                _proxy: proxy,
                                _listener: listener,
                            });
                        }
                    }
                    _ => {}
                }
            })
            .global_remove(move |id| {
                let mut graph = removed.borrow_mut();
                graph.sessions.remove(&id);
                graph.sinks.remove(&id);
                graph.links.remove(&id);
                if graph.metadata.as_ref().is_some_and(|m| m.id == id) {
                    graph.metadata = None;
                    *default_remove.borrow_mut() = None;
                }
            })
            .register();
        let done = Rc::new(Cell::new(-1));
        let done_event = done.clone();
        let error = Rc::new(RefCell::new(None));
        let error_event = error.clone();
        let cookie = Rc::new(Cell::new(0));
        let cookie_event = cookie.clone();
        let core_listener = core
            .add_listener_local()
            .info(move |info| cookie_event.set(info.cookie()))
            .done(move |_, seq| done_event.set(seq.seq()))
            .error(move |id, _, _, message| {
                if id == 0 {
                    *error_event.borrow_mut() = Some(format!("PipeWire-Verbindung: {message}"));
                }
            })
            .register();
        let mut native = Self {
            _registry_listener: listener,
            _core_listener: core_listener,
            _registry: registry,
            core,
            _context: context,
            mainloop,
            graph,
            default_sink,
            error,
            done,
            selected: None,
            cookie,
            journal: super::worker::state_dir()?.join("pipewire.json"),
            last_journal: RefCell::new(Vec::new()),
            meters: HashMap::new(),
            recovery_complete: false,
        };
        native.roundtrip(Duration::from_millis(500))?;
        native.roundtrip(Duration::from_millis(500))?;
        native.recover()?;
        native.recovery_complete = true;
        Ok(native)
    }

    fn persist(&self) -> Result<(), String> {
        let graph = self.graph.borrow();
        let journal = Journal {
            cookie: self.cookie.get(),
            sessions: graph
                .sessions
                .values()
                .filter_map(|s| {
                    s.gain.clone().map(|gain| Saved {
                        serial: s.serial,
                        gain,
                    })
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
        if *self.last_journal.borrow() == bytes {
            return Ok(());
        }
        save_journal(&self.journal, &bytes)?;
        *self.last_journal.borrow_mut() = bytes;
        Ok(())
    }

    /// Recover a previous guardian only on the same PipeWire daemon generation
    /// and immutable node serial. Reused numeric object ids never grant ownership.
    fn recover(&mut self) -> Result<(), String> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).custom_flags(libc::O_NOFOLLOW);
        use std::os::unix::fs::OpenOptionsExt;
        let file = match options.open(&self.journal) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(512 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let old: Journal = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Absenkungs-Wiederherstellung: {e}"))?;
        if old.sessions.len() > MAX_NODES
            || old.sessions.iter().any(|saved| {
                !valid(&saved.gain.baseline)
                    || !valid(&saved.gain.last)
                    || saved.gain.baseline.len() != saved.gain.last.len()
                    || (!saved.gain.previous.is_empty()
                        && (!valid(&saved.gain.previous)
                            || saved.gain.previous.len() != saved.gain.baseline.len()))
            })
        {
            return Err(
                "Ungültiger PipeWire-Wiederherstellungsdatensatz; unverändert aufbewahrt".into(),
            );
        }
        if old.cookie == self.cookie.get() {
            // A core sync does not wait for a foreign client's parameter reply.
            // Never discard a valid recovery record merely because its Props
            // enumeration is still in flight after registry discovery.
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                let pending = {
                    let graph = self.graph.borrow();
                    old.sessions.iter().any(|saved| {
                        graph.sessions.values().any(|s| {
                            s.serial == saved.serial && s.observed.borrow().channels.is_empty()
                        })
                    })
                };
                if !pending {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err("PipeWire-Pegel für Wiederherstellung noch unbekannt".into());
                }
                self.mainloop
                    .loop_()
                    .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(2)));
                self.pump()?;
            }
            let mut graph = self.graph.borrow_mut();
            for saved in old.sessions {
                if let Some(session) = graph
                    .sessions
                    .values_mut()
                    .find(|s| s.serial == saved.serial && !s.own)
                {
                    session.gain = Some(saved.gain);
                    session.pending_until = Some(Instant::now() + Duration::from_millis(150));
                    session.write_ack.set(false);
                    session.release()?;
                }
            }
        }
        self.roundtrip(Duration::from_millis(250))?;
        if self.restore() {
            Ok(())
        } else {
            Err("PipeWire-Wiederherstellung noch nicht bestätigt".into())
        }
    }

    fn pump(&self) -> Result<(), String> {
        // Bounded dispatch, including a busy registry. Never spin indefinitely.
        for _ in 0..32 {
            if self.mainloop.loop_().iterate(pw::loop_::Timeout::None) <= 0 {
                break;
            }
        }
        self.error.borrow().clone().map_or(Ok(()), Err)
    }

    fn roundtrip(&self, timeout: Duration) -> Result<(), String> {
        let seq = self.core.sync(0).map_err(|e| e.to_string())?.seq();
        let deadline = Instant::now() + timeout;
        while self.done.get() != seq {
            if Instant::now() >= deadline {
                return Err("PipeWire-Steuerung antwortet nicht".into());
            }
            self.mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(2)));
            if let Some(error) = self.error.borrow().clone() {
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn active_route(&self) -> Option<String> {
        self.selected
            .and_then(|id| self.graph.borrow().sinks.get(&id).cloned())
    }

    pub fn measure(&mut self, route: Option<&str>) -> Result<Background, String> {
        self.pump()?;
        let desired = route
            .map(str::to_string)
            .or_else(|| self.default_sink.borrow().clone());
        self.selected = desired.and_then(|name| {
            self.graph
                .borrow()
                .sinks
                .iter()
                .find(|(_, n)| **n == name)
                .map(|(id, _)| *id)
        });
        if self.selected.is_none() {
            return Err("Absenkung: Ausgabegerät noch nicht verfügbar".into());
        }
        let mut graph = self.graph.borrow_mut();
        let links: Vec<_> = graph
            .links
            .values()
            .map(|l| l.state.get())
            .filter(|(_, _, active)| *active)
            .map(|(o, i, _)| (o, i))
            .collect();
        let sinks: HashSet<_> = graph.sinks.keys().copied().collect();
        let selected = self.selected.unwrap();
        let eligible: Vec<_> = graph
            .sessions
            .iter()
            .filter(|(id, s)| {
                let current = s.observed.borrow();
                let destinations = routed_sinks(**id, &links, &sinks);
                !s.own
                    && current.running
                    && !current.muted
                    && destinations.len() == 1
                    && destinations.contains(&selected)
            })
            .map(|(id, s)| (*id, s.serial))
            .collect();
        self.meters
            .retain(|id, _| eligible.iter().any(|(key, _)| key == id));
        for (id, serial) in eligible.iter().take(32) {
            if !self.meters.contains_key(id) {
                if let Ok(meter) = Meter::new(self.core.clone(), *serial) {
                    self.meters.insert(*id, meter);
                }
            }
        }
        let now = Instant::now();
        let mut energy = 0.0;
        for (id, _) in &eligible {
            // PipeWire's output monitor is post stream volume (native fixture
            // verifies -12 dB at the tap). Undo only our own known scalar gain;
            // retain the application's current channel balance and user volume.
            let session = graph.sessions.get_mut(id).unwrap();
            let current = session.observed.borrow();
            let factor = session
                .gain
                .as_ref()
                .filter(|g| !g.yielded && same(&current.channels, &g.baseline))
                .and_then(|g| {
                    let owned = (same(&current.soft, &g.last) && !same(&g.last, &g.baseline))
                        || (session.previous_owned
                            && !session.write_ack.get()
                            && session.pending_until.is_some_and(|at| Instant::now() < at)
                            && same(&current.soft, &g.previous));
                    if !owned {
                        return None;
                    }
                    current
                        .soft
                        .iter()
                        .zip(&g.baseline)
                        .find_map(|(soft, base)| (*base > 1e-8).then_some(soft / base))
                })
                .unwrap_or(1.0)
                .clamp(10.0f32.powf(-24.0 / 20.0), 1.0);
            let Some(meter) = self.meters.get_mut(id) else {
                return Ok(Background::default());
            };
            if (factor - session.metered_factor).abs() > 0.0001 {
                session.metered_factor = factor;
                meter.invalidate(now);
            }
            if let Some(measured) = meter.energy(now) {
                session.baseline_energy = Some((measured / (factor * factor), now));
            }
            let Some((value, at)) = session.baseline_energy else {
                return Ok(Background::default());
            };
            if now.saturating_duration_since(at) > Duration::from_millis(500) {
                return Ok(Background::default());
            }
            energy += value;
        }
        Ok(Background {
            rms_db: Some(10.0 * energy.max(1e-16).log10()),
            audible: Some(energy > 1e-12),
        })
    }

    pub fn apply(&mut self, db: f32) -> Result<usize, String> {
        self.pump()?;
        if !db.is_finite() || db <= 0.01 {
            return if self.restore() {
                Ok(0)
            } else {
                Err("Absenkung wird wiederhergestellt".into())
            };
        }
        let selected = self.selected.ok_or("Kein aktives Ausgabegerät")?;
        let mut count = 0;
        let mut writes = Vec::new();
        // Preserve the last acknowledged state before releasing any session.
        self.persist()?;
        {
            let mut graph = self.graph.borrow_mut();
            let links: Vec<_> = graph
                .links
                .values()
                .map(|l| l.state.get())
                .filter(|(_, _, active)| *active)
                .map(|(o, i, _)| (o, i))
                .collect();
            let sinks: HashSet<_> = graph.sinks.keys().copied().collect();
            for (id, session) in graph.sessions.iter_mut() {
                let current = session.observed.borrow().clone();
                let destinations = routed_sinks(*id, &links, &sinks);
                let eligible = !session.own
                    && current.running
                    && !current.muted
                    && destinations.len() == 1
                    && destinations.contains(&selected);
                if !eligible || session.restoring {
                    session.release()?;
                    continue;
                }
                if session.gain.is_none() {
                    // Empty/unity is the usual inactive soft property. A foreign
                    // nontrivial override is left alone until that owner releases it.
                    if !current.soft.is_empty()
                        && !same(&current.soft, &current.channels)
                        && !same(&current.soft, &session.known_soft)
                        && !current.soft.iter().all(|v| (*v - 1.0).abs() < 0.00005)
                    {
                        continue;
                    }
                    session.gain = Gain::new(current.channels.clone());
                }
                let Some(gain) = session.gain.as_mut() else {
                    continue;
                };
                if gain.yielded {
                    continue;
                }
                if same(&current.soft, &gain.last) {
                    session.pending_until = None;
                } else if same(&current.channels, &gain.baseline)
                    && same(&current.soft, &gain.previous)
                    && !session.write_ack.get()
                    && session
                        .pending_until
                        .is_some_and(|deadline| Instant::now() < deadline)
                {
                    count += 1;
                    continue;
                }
                if !same(&current.channels, &gain.baseline)
                    || (!same(&gain.last, &gain.baseline) && !same(&current.soft, &gain.last))
                {
                    gain.yielded = true;
                    continue;
                }
                let next = gain.target(db);
                if !same(&next, &gain.last) {
                    session.previous_owned =
                        !same(&gain.last, &gain.baseline) && same(&current.soft, &gain.last);
                    gain.written(next.clone());
                    *session.expected_soft.borrow_mut() = Some(next.clone());
                    session.write_ack.set(false);
                    // The last command can still be in flight: journal what was
                    // actually observed, not the previous intended command.
                    gain.previous = current.soft.clone();
                    session.known_soft = next.clone();
                    session.pending_until = Some(Instant::now() + Duration::from_millis(150));
                    writes.push((*id, next));
                }
                count += 1;
            }
        }
        // Record intent first. `previous` covers a crash before the async write
        // is acknowledged; `last` covers one after it reached PipeWire.
        self.persist()?;
        {
            let graph = self.graph.borrow();
            for (id, next) in writes {
                if let Some(session) = graph.sessions.get(&id) {
                    session.write(spa::sys::SPA_PROP_softVolumes, next)?;
                }
            }
        }
        self.roundtrip(Duration::from_millis(100))?;
        Ok(count)
    }

    pub fn restore(&mut self) -> bool {
        if self.pump().is_err() {
            return false;
        }
        if self
            .graph
            .borrow()
            .sessions
            .values()
            .all(|s| s.gain.is_none())
        {
            return self.persist().is_ok();
        }
        let mut ok = true;
        for session in self.graph.borrow_mut().sessions.values_mut() {
            ok &= session.release().is_ok();
        }
        if !ok || self.roundtrip(Duration::from_millis(100)).is_err() {
            return false;
        }
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            if self.pump().is_err() {
                return false;
            }
            let mut graph = self.graph.borrow_mut();
            for session in graph.sessions.values_mut().filter(|s| s.restoring) {
                if session.release().is_err() {
                    return false;
                }
            }
            if graph.sessions.values().all(|s| s.gain.is_none()) {
                drop(graph);
                return self.persist().is_ok();
            }
            drop(graph);
            if Instant::now() >= deadline {
                return false;
            }
            self.mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(2)));
        }
    }
}

fn save_journal(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension("pending");
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.sync_data().map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

impl Drop for Native {
    fn drop(&mut self) {
        if self.recovery_complete {
            let _ = self.restore();
        }
        self.meters.clear();
        // Bound Node/Link proxies do not retain CoreRc. Destroy their listeners
        // and proxies while the context/mainloop are certainly still alive.
        *self.graph.borrow_mut() = Graph::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Run only with a dedicated null sink and a synthetic stream supplied by
    /// experiments/test-ducking-linux.py; never selects the desktop's default device.
    #[test]
    #[ignore = "requires an isolated native PipeWire null sink and fixture stream"]
    fn native_null_sink_attenuation_restore_and_manual_override() {
        let sink = std::env::var("ICU_DUCKING_TEST_SINK").expect("isolated sink name");
        let id: u32 = std::env::var("ICU_DUCKING_TEST_NODE")
            .unwrap()
            .parse()
            .unwrap();
        let mut native = Native::new().unwrap();
        for _ in 0..50 {
            native.pump().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        native.measure(Some(&sink)).unwrap();
        let before = native
            .graph
            .borrow()
            .sessions
            .get(&id)
            .expect("synthetic stream")
            .observed
            .borrow()
            .clone();
        assert!(before.running && valid(&before.channels));
        let mut baseline_energy = None;
        for _ in 0..100 {
            baseline_energy = native.measure(Some(&sink)).unwrap().rms_db;
            if baseline_energy.is_some_and(|db| db > -90.0) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            baseline_energy.is_some_and(|db| db > -90.0),
            "per-stream native monitor did not produce audio"
        );
        let raw_before = 10.0
            * native
                .meters
                .get_mut(&id)
                .unwrap()
                .energy(Instant::now())
                .unwrap()
                .log10();
        eprintln!("native monitor before={baseline_energy:?}, raw={raw_before}");
        // Exercise the public mode envelope and actual native PCM gain. Medium
        // and dynamic Auto are measured below; these cover Light, Strong, Off.
        for (mode, expected_db) in [
            (super::super::Mode::Light, 6.0f32),
            (super::super::Mode::Strong, 18.0),
            (super::super::Mode::Off, 0.0),
        ] {
            let now = Instant::now();
            let mut envelope = super::super::Envelope::new(now - Duration::from_millis(100));
            let input = super::super::Input {
                mode,
                online: true,
                transmitting_voice: true,
                ..Default::default()
            };
            let (db, _) = envelope.update(&input, Background::default(), now);
            assert!((db - expected_db).abs() < 0.001);
            assert_eq!(native.apply(db).unwrap(), usize::from(db > 0.0));
            for _ in 0..50 {
                native.measure(Some(&sink)).unwrap();
                std::thread::sleep(Duration::from_millis(5));
            }
            let measured = native.measure(Some(&sink)).unwrap().rms_db.unwrap();
            let raw = 10.0
                * native
                    .meters
                    .get_mut(&id)
                    .unwrap()
                    .energy(Instant::now())
                    .unwrap()
                    .log10();
            let observed = native.graph.borrow().sessions[&id]
                .observed
                .borrow()
                .clone();
            let expected: Vec<_> = before
                .channels
                .iter()
                .map(|v| v * 10.0f32.powf(-expected_db / 20.0))
                .collect();
            assert!(same(&before.channels, &observed.channels));
            assert_eq!(before.muted, observed.muted);
            assert!(same(&expected, &observed.soft));
            assert!(
                (raw_before - raw - expected_db).abs() < 0.5,
                "mode={mode:?}: raw {raw_before} → {raw}, expected {expected_db} dB"
            );
            assert!(
                (measured - baseline_energy.unwrap()).abs() < 0.5,
                "pre-duck reconstruction changed in {mode:?}"
            );
            eprintln!("native mode={mode:?}: requested={expected_db:.1}dB actual={:.3}dB reconstructed={measured:.3}dBFS", raw_before - raw);
        }
        assert_eq!(native.apply(12.0).unwrap(), 1);
        for _ in 0..50 {
            native.measure(Some(&sink)).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        let ducked_energy = native.measure(Some(&sink)).unwrap().rms_db;
        let raw_after = 10.0
            * native
                .meters
                .get_mut(&id)
                .unwrap()
                .energy(Instant::now())
                .unwrap()
                .log10();
        eprintln!("native monitor ducked/reconstructed={ducked_energy:?}, raw={raw_after}");
        assert!(
            (raw_before - raw_after - 12.0).abs() < 0.5,
            "monitor must be post-volume"
        );
        assert!(
            (baseline_energy.unwrap() - ducked_energy.unwrap()).abs() < 0.5,
            "Auto must measure unchanged pre-duck energy"
        );
        let ducked = native.graph.borrow().sessions[&id]
            .observed
            .borrow()
            .clone();
        assert!(
            same(&before.channels, &ducked.channels),
            "persistent user volume changed"
        );
        let expected: Vec<_> = before
            .channels
            .iter()
            .map(|v| v * 10.0f32.powf(-12.0 / 20.0))
            .collect();
        assert!(
            same(&expected, &ducked.soft),
            "expected {expected:?}, observed {:?}",
            ducked.soft
        );
        assert!(native.restore());
        for _ in 0..50 {
            native.pump().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        let restored = native.graph.borrow().sessions[&id]
            .observed
            .borrow()
            .clone();
        assert!(same(&before.channels, &restored.channels));
        assert!(same(&before.channels, &restored.soft));
        // Once a write was acknowledged, a foreign controller returning to
        // our previous value owns that value; it is not an in-flight ICU write.
        native.apply(12.0).unwrap();
        for _ in 0..50 {
            native.pump().unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        let old_soft = native.graph.borrow().sessions[&id]
            .observed
            .borrow()
            .soft
            .clone();
        native.apply(18.0).unwrap();
        for _ in 0..50 {
            native.pump().unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        native.graph.borrow().sessions[&id]
            .write(spa::sys::SPA_PROP_softVolumes, old_soft.clone())
            .unwrap();
        for _ in 0..50 {
            native.pump().unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            native.apply(18.0).unwrap(),
            0,
            "acknowledged previous soft value is now a manual override"
        );
        assert!(native.restore());
        assert!(same(
            &native.graph.borrow().sessions[&id].observed.borrow().soft,
            &old_soft
        ));
        native.graph.borrow().sessions[&id]
            .write(spa::sys::SPA_PROP_softVolumes, before.channels.clone())
            .unwrap();
        native.graph.borrow().sessions[&id]
            .write(spa::sys::SPA_PROP_channelVolumes, before.channels.clone())
            .unwrap();
        for _ in 0..50 {
            native.pump().unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        // Close the real feedback loop: steady foreground/background must not
        // pump when the monitor sees our own progressively attenuated samples.
        let mut envelope = super::super::Envelope::new(Instant::now());
        let input = super::super::Input {
            mode: super::super::Mode::Auto,
            online: true,
            speech_db: Some(-32.0),
            ..Default::default()
        };
        let mut stable = Vec::new();
        for tick in 0..200 {
            let background = native.measure(Some(&sink)).unwrap();
            let (db, reason) = envelope.update(&input, background, Instant::now());
            native.apply(db).unwrap();
            if tick > 100 {
                assert!(reason.contains("Sprach-/Hintergrundenergie"), "{reason}");
                stable.push(db);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let min = stable.iter().copied().fold(f32::INFINITY, f32::min);
        let max = stable.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        eprintln!("native Auto steady attenuation {min:.3}..{max:.3} dB");
        assert!(min > 12.0 && max < 15.0 && max - min < 0.5);
        assert!(native.restore());
        assert_eq!(native.apply(12.0).unwrap(), 1);
        for _ in 0..50 {
            native.pump().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        let manual: Vec<_> = before.channels.iter().map(|v| v * 0.7).collect();
        native.graph.borrow().sessions[&id]
            .write(spa::sys::SPA_PROP_channelVolumes, manual.clone())
            .unwrap();
        for _ in 0..50 {
            native.pump().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            native.apply(18.0).unwrap(),
            0,
            "manual override must win for this spurt"
        );
        assert!(native.restore());
        assert!(same(
            &manual,
            &native.graph.borrow().sessions[&id]
                .observed
                .borrow()
                .channels
        ));
        // Reset both the actual baseline and inactive soft property so a new
        // guardian cannot mistake our synthetic leftover for another controller.
        native.graph.borrow().sessions[&id]
            .write(spa::sys::SPA_PROP_softVolumes, before.channels.clone())
            .unwrap();
        native.graph.borrow().sessions[&id]
            .write(spa::sys::SPA_PROP_channelVolumes, before.channels)
            .unwrap();
        native.roundtrip(Duration::from_millis(100)).unwrap();
        let journal = native.journal.clone();
        drop(native);
        let oversized = Journal {
            cookie: 0,
            sessions: (0..=MAX_NODES)
                .map(|serial| Saved {
                    serial: serial as u64,
                    gain: Gain::new(vec![0.5]).unwrap(),
                })
                .collect(),
        };
        for invalid in [
            b"corrupt recovery record".to_vec(),
            serde_json::to_vec(&oversized).unwrap(),
        ] {
            std::fs::write(&journal, &invalid).unwrap();
            assert!(Native::new().is_err());
            assert_eq!(
                std::fs::read(&journal).unwrap(),
                invalid,
                "failed recovery must preserve evidence"
            );
        }
        std::fs::remove_file(journal).unwrap();
    }

    #[test]
    fn route_selection_handles_filters_cycles_and_multiple_sinks() {
        let sinks = HashSet::from([10, 20]);
        assert_eq!(
            routed_sinks(1, &[(1, 2), (2, 10)], &sinks),
            HashSet::from([10])
        );
        assert_eq!(
            routed_sinks(1, &[(1, 2), (2, 1), (2, 10)], &sinks),
            HashSet::from([10])
        );
        assert_eq!(routed_sinks(1, &[(1, 10), (1, 20)], &sinks), sinks);
        assert!(routed_sinks(3, &[(1, 10)], &sinks).is_empty());
    }
}
