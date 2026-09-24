//! Test double: a timer thread that feeds a synthetic signal into the capture
//! callback and collects whatever the playback callback produces.
//!
//! Used by the integration tests (no hardware) and by `--test-tone` style
//! checks. Blocks are 240 samples every 5 ms, like the PipeWire backend, unless
//! a test picks another quantum.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use super::backend::{
    AudioBackend, BackendError, BackendEvent, CaptureCallback, DeviceInfo, DeviceKind,
    PlaybackCallback, StreamKind, BLOCK_SAMPLES, SAMPLE_RATE,
};

/// A source of capture samples. Called once per block with the running sample
/// index and a slice to fill.
pub type Generator = Box<dyn FnMut(u64, &mut [f32]) + Send + 'static>;

/// Shared handle to inspect what the fake backend saw and produced.
#[derive(Clone, Default)]
pub struct FakeProbe {
    inner: Arc<Mutex<ProbeInner>>,
}

#[derive(Default)]
struct ProbeInner {
    played: Vec<f32>,
}

impl FakeProbe {
    /// Everything the playback callback produced so far.
    pub fn played(&self) -> Vec<f32> {
        self.inner.lock().unwrap().played.clone()
    }
    /// Drops collected playback samples.
    pub fn clear(&self) {
        self.inner.lock().unwrap().played.clear();
    }
}

struct Shared {
    capture: Mutex<Option<CaptureCallback>>,
    reference: Mutex<Option<CaptureCallback>>,
    playback: Mutex<Option<PlaybackCallback>>,
    generator: Mutex<Option<Generator>>,
    running: AtomicBool,
    probe: FakeProbe,
    /// Whether the playback output is fed back into the reference stream,
    /// like a sink monitor would.
    loop_reference: bool,
    /// Samples per callback: the graph quantum.
    block: usize,
}

/// The fake backend.
pub struct FakeBackend {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    events: Option<mpsc::Receiver<BackendEvent>>,
    _events_tx: mpsc::Sender<BackendEvent>,
}

impl FakeBackend {
    /// Creates a backend that captures `generator`'s output.
    pub fn new(generator: Generator) -> (FakeBackend, FakeProbe) {
        Self::with_options(generator, false)
    }

    /// `loop_reference = true` feeds the playback output into the reference
    /// stream, like a sink monitor would.
    pub fn with_options(generator: Generator, loop_reference: bool) -> (FakeBackend, FakeProbe) {
        Self::with_block(generator, loop_reference, BLOCK_SAMPLES)
    }

    /// Like [`FakeBackend::with_options`], with `block` samples per callback
    /// — a graph quantum other than the requested 240, as PipeWire runs
    /// when another client asks for more.
    pub fn with_block(
        generator: Generator,
        loop_reference: bool,
        block: usize,
    ) -> (FakeBackend, FakeProbe) {
        let probe = FakeProbe::default();
        let shared = Arc::new(Shared {
            capture: Mutex::new(None),
            reference: Mutex::new(None),
            playback: Mutex::new(None),
            generator: Mutex::new(Some(generator)),
            running: AtomicBool::new(true),
            probe: probe.clone(),
            loop_reference,
            block,
        });
        let (tx, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("icu-fake-audio".into())
            .spawn(move || run(thread_shared))
            .expect("spawn fake audio thread");
        (
            FakeBackend {
                shared,
                thread: Some(thread),
                events: Some(rx),
                _events_tx: tx,
            },
            probe,
        )
    }

    /// A silent backend.
    pub fn silent() -> (FakeBackend, FakeProbe) {
        Self::new(Box::new(|_, out| out.fill(0.0)))
    }
}

impl Drop for FakeBackend {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(shared: Arc<Shared>) {
    let period = Duration::from_micros(shared.block as u64 * 1_000_000 / SAMPLE_RATE as u64);
    let start = Instant::now();
    let mut tick: u64 = 0;
    let mut block = vec![0.0f32; shared.block];
    let mut out = vec![0.0f32; shared.block];
    while shared.running.load(Ordering::Relaxed) {
        let deadline = start + period * (tick as u32 + 1);
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
        // Playback first, so the reference (monitor) sees this block.
        out.fill(0.0);
        if let Some(cb) = shared.playback.lock().unwrap().as_mut() {
            cb(&mut out);
            shared
                .probe
                .inner
                .lock()
                .unwrap()
                .played
                .extend_from_slice(&out);
        }
        if let Some(cb) = shared.reference.lock().unwrap().as_mut() {
            if shared.loop_reference {
                cb(&out);
            } else {
                block.fill(0.0);
                cb(&block);
            }
        }
        if let Some(cb) = shared.capture.lock().unwrap().as_mut() {
            if let Some(g) = shared.generator.lock().unwrap().as_mut() {
                g(tick * shared.block as u64, &mut block);
            } else {
                block.fill(0.0);
            }
            cb(&block);
        }
        tick += 1;
    }
}

impl AudioBackend for FakeBackend {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        vec![
            DeviceInfo {
                id: "fake-mic".into(),
                description: "Fake microphone".into(),
                kind: DeviceKind::Input,
            },
            DeviceInfo {
                id: "fake-speaker".into(),
                description: "Fake speaker".into(),
                kind: DeviceKind::Output,
            },
        ]
    }

    fn open_capture(
        &mut self,
        _device: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        *self.shared.capture.lock().unwrap() = Some(cb);
        Ok(())
    }

    fn open_reference(
        &mut self,
        _sink: Option<&str>,
        cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        *self.shared.reference.lock().unwrap() = Some(cb);
        Ok(())
    }

    fn open_playback(
        &mut self,
        _device: Option<&str>,
        cb: PlaybackCallback,
    ) -> Result<(), BackendError> {
        *self.shared.playback.lock().unwrap() = Some(cb);
        Ok(())
    }

    fn close(&mut self, stream: StreamKind) {
        match stream {
            StreamKind::Capture => *self.shared.capture.lock().unwrap() = None,
            StreamKind::Reference => *self.shared.reference.lock().unwrap() = None,
            StreamKind::Playback => *self.shared.playback.lock().unwrap() = None,
        }
    }

    fn take_events(&mut self) -> Option<mpsc::Receiver<BackendEvent>> {
        self.events.take()
    }
}
