//! Native application gate: WASAPI reports timeline gaps; the production engine
//! owns the coordinated restart. A raw backend probe cannot exercise that path.

use super::*;
use std::sync::{atomic::AtomicUsize, Mutex, OnceLock};

/// Model a slow synchronous log file, as used by the application. Stream
/// metadata must be written before Start, when no audio deadline is running.
pub(super) fn slow_log() -> Arc<AtomicUsize> {
    static WRITES: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
    WRITES
        .get_or_init(|| {
            struct Writer(Arc<AtomicUsize>);
            impl std::io::Write for Writer {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    std::thread::sleep(Duration::from_millis(40));
                    self.0.fetch_add(1, Ordering::Relaxed);
                    Ok(bytes.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let writes = Arc::new(AtomicUsize::new(0));
            let writer_count = writes.clone();
            tracing_subscriber::fmt()
                .with_env_filter("off,icu_client::audio::wasapi=info")
                .with_ansi(false)
                .with_writer(move || Writer(writer_count.clone()))
                .try_init()
                .expect("native audio gates need their controlled log subscriber");
            writes
        })
        .clone()
}

#[derive(Default)]
struct Probe {
    capture: AtomicUsize,
    playback: AtomicUsize,
    reference: AtomicUsize,
    audible: AtomicUsize,
    opens: [AtomicUsize; 3],
}

impl Probe {
    fn counts(&self) -> [usize; 4] {
        [
            &self.capture,
            &self.playback,
            &self.reference,
            &self.audible,
        ]
        .map(|counter| counter.load(Ordering::Relaxed))
    }

    fn opened(&self) -> [usize; 3] {
        std::array::from_fn(|i| self.opens[i].load(Ordering::Relaxed))
    }
}

struct Observed {
    backend: Arc<Mutex<WasapiBackend>>,
    probe: Arc<Probe>,
}

impl AudioBackend for Observed {
    fn name(&self) -> &'static str {
        "wasapi"
    }
    fn devices(&self) -> Vec<DeviceInfo> {
        self.backend.lock().unwrap().devices()
    }
    fn set_capture_channel(&mut self, channel: Option<u16>) -> Result<(), BackendError> {
        self.backend.lock().unwrap().set_capture_channel(channel)
    }
    fn capture_channel_index(&self) -> Option<u16> {
        self.backend.lock().unwrap().capture_channel_index()
    }
    fn reference_slack(&self) -> Duration {
        self.backend.lock().unwrap().reference_slack()
    }
    fn open_capture(
        &mut self,
        device: Option<&str>,
        mut cb: CaptureCallback,
    ) -> Result<(), BackendError> {
        self.probe.opens[0].fetch_add(1, Ordering::Relaxed);
        let probe = self.probe.clone();
        self.backend.lock().unwrap().open_capture(
            device,
            Box::new(move |samples| {
                assert!(samples.iter().all(|x| x.is_finite()));
                probe.capture.fetch_add(samples.len(), Ordering::Relaxed);
                cb(samples);
            }),
        )
    }
    fn open_playback(
        &mut self,
        device: Option<&str>,
        mut cb: PlaybackCallback,
    ) -> Result<(), BackendError> {
        self.probe.opens[1].fetch_add(1, Ordering::Relaxed);
        let probe = self.probe.clone();
        let mut phase = 0.0f32;
        self.backend.lock().unwrap().open_playback(
            device,
            Box::new(move |samples| {
                cb(samples);
                probe.playback.fetch_add(samples.len(), Ordering::Relaxed);
                // A quiet, known local stimulus makes real loopback observable.
                for sample in samples {
                    *sample = phase.sin() * 0.02;
                    phase = (phase + std::f32::consts::TAU * 997.0 / SAMPLE_RATE as f32)
                        % std::f32::consts::TAU;
                }
            }),
        )
    }
    fn open_reference(
        &mut self,
        device: Option<&str>,
        mut cb: ReferenceCallback,
    ) -> Result<(), BackendError> {
        self.probe.opens[2].fetch_add(1, Ordering::Relaxed);
        let probe = self.probe.clone();
        self.backend.lock().unwrap().open_reference(
            device,
            Box::new(move |frames, channels| {
                assert!(frames
                    .iter()
                    .all(|frame| frame[..channels].iter().all(|x| x.is_finite())));
                probe.reference.fetch_add(frames.len(), Ordering::Relaxed);
                probe.audible.fetch_add(
                    frames
                        .iter()
                        .filter(|frame| frame[..channels].iter().any(|x| x.abs() > 0.00001))
                        .count(),
                    Ordering::Relaxed,
                );
                cb(frames, channels);
            }),
        )
    }
    fn close(&mut self, stream: StreamKind) {
        self.backend.lock().unwrap().close(stream);
    }
    fn take_events(&mut self) -> Option<mpsc::Receiver<BackendEvent>> {
        self.backend.lock().unwrap().take_events()
    }
}

/// Require a single, stable post-recovery epoch with at least one second of
/// fresh capture, playback and real loopback. Counts from old epochs cannot
/// satisfy this check. Persistent restart loops fail the bounded deadline.
fn healthy_epoch(engine: &crate::engine::Engine, probe: &Probe, previous: [usize; 3]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut opened = probe.opened();
    let mut base = probe.counts();
    loop {
        std::thread::sleep(Duration::from_millis(20));
        let now_opened = probe.opened();
        if now_opened != opened {
            opened = now_opened;
            base = probe.counts();
        }
        let counts = probe.counts();
        let fresh: [usize; 4] = std::array::from_fn(|i| counts[i] - base[i]);
        if opened
            .iter()
            .zip(previous)
            .all(|(after, before)| *after > before)
            && fresh[..3].iter().all(|frames| *frames >= 48_000)
            && fresh[3] >= 24_000
            && probe.opened() == opened
        {
            let state = engine.snapshot();
            assert!(
                state.aec_reference,
                "the recovered reference must reach the engine"
            );
            assert!(state.muted, "recovery must preserve the user's mute");
            assert!(
                state.banner.is_none(),
                "recovery left a device error: {:?}",
                state.banner
            );
            eprintln!("native engine: opens={opened:?} fresh_capture_playback_reference_audible={fresh:?}");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native recovery did not stabilize: opens={opened:?}, fresh={fresh:?}"
        );
    }
}

#[test]
#[ignore = "needs interactive Windows audio; plays a quiet tone and interrupts test streams"]
fn live_wasapi_engine_recovers_native_discontinuities() {
    use crate::config::{Identity, Paths, ServerInfo, Settings};
    use crate::engine::{Engine, EngineConfig};
    let writes = slow_log();
    let log_start = writes.load(Ordering::Relaxed);
    let dir = tempfile::tempdir().unwrap();
    // Reserve an inert local endpoint: no production identity or remote server.
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = socket.local_addr().unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let identity = Identity {
        key_der: key.serialize_der(),
        leaf_der: cert.der().to_vec(),
        ca_der: cert.der().to_vec(),
        com_key: icu_proto::com::ComKey::from_bytes([1; 32]),
        server: ServerInfo {
            host: "localhost".into(),
            port: addr.port(),
            server_name: "localhost".into(),
            name: "wasapi-native-test".into(),
        },
    };
    let mut settings = Settings::default();
    settings.audio.aec = true;
    settings.audio.muted = true;
    settings.audio.tune_system_mic = false;
    settings.audio.ducking = crate::audio::ducking::Mode::Off;
    let mut cfg = EngineConfig::new(identity, settings, Paths::new(Some(dir.path().to_owned())));
    cfg.server_addr = Some(addr);
    let backend = Arc::new(Mutex::new(WasapiBackend::new().unwrap()));
    let probe = Arc::new(Probe::default());
    let engine = Engine::start(
        cfg,
        Box::new(Observed {
            backend: backend.clone(),
            probe: probe.clone(),
        }),
    )
    .unwrap();
    healthy_epoch(&engine, &probe, [0; 3]);
    for kind in [StreamKind::Capture, StreamKind::Reference] {
        let before = probe.opened();
        {
            let mut backend = backend.lock().unwrap();
            backend.close(kind);
            backend
                .events_tx
                .send(BackendEvent::StreamDiscontinuity { stream: kind })
                .unwrap();
        }
        healthy_epoch(&engine, &probe, before);
    }
    engine.shutdown();
    assert!(writes.load(Ordering::Relaxed) - log_start >= 9);
}
