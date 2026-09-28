//! Source-node controls, including hardware volume where PipeWire exposes it.
//! Called exclusively from the PipeWire main loop, never the processing callback.
use super::{
    backend::BackendError,
    level::{unsupported, CaptureLevel},
};
use pipewire::{self as pw, spa};
use spa::pod::{Object, Property, Value, ValueArray};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub(super) struct Source {
    pub node: pw::node::Node,
    pub level: Rc<RefCell<Option<CaptureLevel>>>,
    expected_write: Rc<RefCell<Option<Vec<f32>>>>,
    pub _listener: pw::node::NodeListener,
}
impl Source {
    pub fn new(node: pw::node::Node, name: String) -> Self {
        let level = Rc::new(RefCell::new(None));
        let observed = level.clone();
        let expected_write = Rc::new(RefCell::new(None::<Vec<f32>>));
        let own_write = expected_write.clone();
        let revision_counter = Cell::new(0u64);
        let listener = node
            .add_listener_local()
            .param(move |_, _, _, _, pod| {
                let Some(pod) = pod else { return };
                let Ok((_, Value::Object(o))) =
                    spa::pod::deserialize::PodDeserializer::deserialize_from::<Value>(
                        pod.as_bytes(),
                    )
                else {
                    return;
                };
                let mut channels = None;
                let mut muted = None;
                for p in o.properties {
                    match (p.key, p.value) {
                        (
                            spa::sys::SPA_PROP_channelVolumes,
                            Value::ValueArray(ValueArray::Float(v)),
                        ) => channels = Some(v),
                        (spa::sys::SPA_PROP_mute, Value::Bool(v)) => muted = Some(v),
                        _ => {}
                    }
                }
                let mut slot = observed.borrow_mut();
                let channels =
                    channels.or_else(|| slot.as_ref().map(|l: &CaptureLevel| l.channels.clone()));
                if let Some(channels) = channels.filter(|v| {
                    !v.is_empty() && v.len() <= 64 && v.iter().all(|x| x.is_finite() && *x >= 0.0)
                }) {
                    let max = channels.iter().copied().fold(0.0f32, f32::max);
                    if max <= 0.0 {
                        revision_counter.set(revision_counter.get() + 1);
                        *slot = None;
                        return;
                    }
                    let mute = muted.unwrap_or_else(|| slot.as_ref().is_some_and(|l| l.muted));
                    let own = own_write.borrow().as_ref().is_some_and(|v| {
                        v.len() == channels.len()
                            && v.iter().zip(&channels).all(|(a, b)| (a - b).abs() < 0.0001)
                    });
                    let changed = slot
                        .as_ref()
                        .is_some_and(|l| l.channels != channels || l.muted != mute);
                    let revision = revision_counter.get()
                        + u64::from(
                            changed && (!own || slot.as_ref().is_some_and(|l| l.muted != mute)),
                        );
                    revision_counter.set(revision);
                    if own {
                        own_write.borrow_mut().take();
                    }
                    // PipeWire's channelVolumes are linear amplitude. Never boost above unity.
                    *slot = Some(CaptureLevel {
                        device: name.clone(),
                        revision,
                        db: 20.0 * max.log10(),
                        min_db: -60.0,
                        max_db: 0.0,
                        step_db: 1.0,
                        muted: muted.unwrap_or_else(|| slot.as_ref().is_some_and(|l| l.muted)),
                        channels,
                        kind: "Systemregler (Hardware unbekannt)",
                    });
                }
            })
            .register();
        node.subscribe_params(&[spa::param::ParamType::Props]);
        node.enum_params(0, Some(spa::param::ParamType::Props), 0, u32::MAX);
        Self {
            node,
            level,
            _listener: listener,
            expected_write,
        }
    }
    pub fn finish_write(&self) {
        self.expected_write.borrow_mut().take();
    }
    pub fn set(&self, expected: &CaptureLevel, channels: Vec<f32>) -> Result<(), BackendError> {
        if !self
            .level
            .borrow()
            .as_ref()
            .is_some_and(|l| l.same(expected))
        {
            return Err(unsupported());
        }
        if channels.len() != expected.channels.len()
            || channels
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        {
            return Err(unsupported());
        }
        *self.expected_write.borrow_mut() = Some(channels.clone());
        let value = Value::Object(Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Props,
            id: spa::sys::SPA_PARAM_Props,
            properties: vec![Property::new(
                spa::sys::SPA_PROP_channelVolumes,
                Value::ValueArray(ValueArray::Float(channels)),
            )],
        });
        let bytes =
            spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &value)
                .map_err(|_| unsupported())?
                .0
                .into_inner();
        let pod = spa::pod::Pod::from_bytes(&bytes).ok_or_else(unsupported)?;
        self.node.set_param(spa::param::ParamType::Props, 0, pod);
        self.node
            .enum_params(0, Some(spa::param::ParamType::Props), 0, u32::MAX);
        Ok(())
    }
}
