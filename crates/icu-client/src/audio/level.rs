//! Exclusive, temporary capture-volume ownership. No device calls on audio callbacks.
use super::backend::{AudioBackend, BackendError};
use crate::dsp::feedback::CaptureWindow;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct CaptureLevel {
    pub device: String,
    /// Changes made outside this controller, including away-and-back changes.
    pub revision: u64,
    pub db: f32,
    pub min_db: f32,
    pub max_db: f32,
    pub step_db: f32,
    pub muted: bool,
    /// Native values retained for exact conditional restoration/channel balance.
    pub channels: Vec<f32>,
    pub kind: &'static str,
}
impl CaptureLevel {
    pub fn same(&self, other: &Self) -> bool {
        self.device == other.device
            && self.revision == other.revision
            && self.muted == other.muted
            && (self.db - other.db).abs() < 0.05
            && self.channels.len() == other.channels.len()
            && self
                .channels
                .iter()
                .zip(&other.channels)
                .all(|(a, b)| (a - b).abs() < 0.0001)
    }
    pub fn valid(&self) -> bool {
        [self.db, self.min_db, self.max_db, self.step_db]
            .iter()
            .all(|v| v.is_finite())
            && self.min_db <= self.db
            && self.db <= self.max_db
            && (0.0..=3.0).contains(&self.step_db)
            && self.channels.iter().all(|v| v.is_finite())
    }
}

struct Owned {
    original: CaptureLevel,
    last: CaptureLevel,
    _lease: File,
}
struct Response {
    before_db: f32,
    change_db: f32,
    at: Instant,
}
const EVIDENCE_AGE: Duration = Duration::from_secs(5);
const RESPONSE_AGE: Duration = Duration::from_secs(20);
const LEARNING_AGE: Duration = Duration::from_secs(30);
#[derive(Default)]
pub struct SystemTuner {
    owned: Option<Owned>,
    suspended: bool,
    response_suspended: bool,
    speech_samples: usize,
    last_write: Option<Instant>,
    response: Option<Response>,
    noise_db: Option<(f32, Instant)>,
    last_speech: Option<Instant>,
    learning_since: Option<Instant>,
    pub status: String,
}
impl SystemTuner {
    /// Drop measurements across mute, capture gaps or a changed device timeline.
    /// Ownership is retained, so a later restore still respects user changes.
    pub fn invalidate_evidence(&mut self) {
        self.speech_samples = 0;
        self.noise_db = None;
        self.response = None;
        self.last_speech = None;
        self.learning_since = None;
    }
    /// Restores only our own last value, never overwriting another application's change.
    pub fn restore(&mut self, backend: &mut dyn AudioBackend) -> f32 {
        let mut delta = 0.0;
        if let Some(o) = self.owned.take() {
            match backend.restore_capture_level(&o.last, &o.original) {
                Ok(()) => delta = o.original.db - o.last.db,
                Err(e) => tracing::warn!(error=%e, "microphone restoration skipped or failed"),
            }
        }
        *self = Self::default();
        delta
    }
    pub fn delta_db(&self) -> f32 {
        self.owned
            .as_ref()
            .map_or(0.0, |o| o.last.db - o.original.db)
    }
    pub fn tick(&mut self, backend: &mut dyn AudioBackend, w: CaptureWindow, now: Instant) -> f32 {
        if self
            .last_speech
            .is_some_and(|at| now.saturating_duration_since(at) > EVIDENCE_AGE)
            || self
                .learning_since
                .is_some_and(|at| now.saturating_duration_since(at) > LEARNING_AGE)
        {
            self.speech_samples = 0;
            self.last_speech = None;
            self.learning_since = None;
        }
        if self
            .noise_db
            .is_some_and(|(_, at)| now.saturating_duration_since(at) > EVIDENCE_AGE)
        {
            self.noise_db = None;
        }
        if self
            .response
            .as_ref()
            .is_some_and(|r| now.saturating_duration_since(r.at) > RESPONSE_AGE)
        {
            self.response = None;
        }
        // A one-sample table/key impulse is a peak, not evidence for a
        // hardware correction. Sustained clipping can still be corrected
        // if severe distortion has made the VAD stop recognizing speech.
        let clipped = w.sustained_clipping();
        // A response-validation pause must not defeat later clipping
        // protection. Manual changes and device/write faults still suspend.
        if self.suspended && !(self.response_suspended && clipped) {
            return 0.0;
        }
        let current = match backend.capture_level() {
            Ok(Some(c)) if c.valid() => c,
            Ok(_) => {
                if self.owned.is_some() {
                    self.suspended = true;
                    self.response_suspended = false;
                }
                self.status = "Nur Software: kein geeigneter Mikrofonregler".into();
                return 0.0;
            }
            Err(e) => {
                self.status = format!("Mikrofonregler nicht verfügbar: {e}");
                self.suspended = true;
                self.response_suspended = false;
                return 0.0;
            }
        };
        if let Some(o) = self.owned.as_ref() {
            if !current.same(&o.last) {
                self.owned = None;
                self.suspended = true;
                self.response_suspended = false;
                self.status = "Manuell geändert – Mikrofonregelung pausiert".into();
                return 0.0;
            }
        }
        if current.muted {
            self.status = "Mikrofon am System stumm".into();
            return 0.0;
        }
        self.status = format!("Lernt · {}", current.kind);
        // Windows of wall time, not an unlimited accumulated backlog.
        if w.samples > 105_600 {
            self.invalidate_evidence();
            return 0.0;
        }
        if w.noise_samples >= 12_000 {
            self.noise_db = Some((
                crate::dsp::meter::to_db((w.noise_energy / w.noise_samples as f64).sqrt() as f32),
                now,
            ));
        }
        if w.speech_samples < 12_000 && !clipped {
            return 0.0;
        }
        self.speech_samples = self.speech_samples.saturating_add(w.speech_samples);
        if w.speech_samples > 0 {
            self.last_speech = Some(now);
            self.learning_since.get_or_insert(now);
        }
        if self.last_write.is_some_and(|t| {
            now.saturating_duration_since(t) < Duration::from_secs(if clipped { 1 } else { 5 })
        }) {
            return 0.0;
        }
        if let Some(response) = self.response.as_ref() {
            if !clipped && w.speech_samples < 24_000 {
                self.status = "Warte auf passende Sprache zur Pegelprüfung".into();
                return 0.0;
            }
            // Natural speech varies: suspend only a large, contradictory response.
            // A newly loud/clipped phrase is not evidence that the device
            // ignored our previous write. Clipping must still lower capture
            // gain, rather than suspend while the microphone is overdriven.
            if !clipped && (w.rms_db() - response.before_db - response.change_db).abs() > 9.0 {
                self.suspended = true;
                self.response_suspended = true;
                self.status = "Mikrofon prüfen: Pegel reagiert unerwartet".into();
                return 0.0;
            }
            self.response = None;
        }
        let correction = w.gain_db();
        if !clipped && correction.abs() <= 3.0 {
            self.status = format!("Eingeregelt · {}", current.kind);
            return 0.0;
        }
        let up = !clipped && correction > 0.0;
        if up
            && (self.speech_samples < 480_000
                || w.peak > 0.5
                || w.rms_db() < -50.0
                || self
                    .noise_db
                    .is_none_or(|(noise, _)| w.rms_db() - noise < 10.0))
        {
            return 0.0;
        }
        let step = current.step_db.max(1.0);
        let start = self.owned.as_ref().map_or(current.db, |o| o.original.db);
        let target = (current.db + if up { step } else { -step }).clamp(
            current.min_db.max(start - 12.0),
            current.max_db.min(start + 12.0),
        );
        if (target - current.db).abs() < 0.05 {
            self.status = "Regelgrenze – Mikrofonabstand/Pegel prüfen".into();
            return 0.0;
        }
        if self.owned.is_none() {
            let lease = match lease(&current.device) {
                Ok(f) => f,
                Err(_) => {
                    self.status = "Mikrofonregelung in anderem ICU aktiv".into();
                    return 0.0;
                }
            };
            self.owned = Some(Owned {
                original: current.clone(),
                last: current.clone(),
                _lease: lease,
            });
        }
        match backend.set_capture_level(&current, target) {
            Ok(actual)
                if actual.valid()
                    && actual.device == current.device
                    && actual.revision == current.revision
                    && !actual.muted
                    && (actual.db - current.db).abs() <= 3.05
                    && (actual.db - target).abs() <= current.step_db.max(0.1)
                    && (actual.db - start).abs() <= 12.05 =>
            {
                let delta = actual.db - current.db;
                self.owned.as_mut().unwrap().last = actual;
                self.last_write = Some(now);
                self.suspended = false;
                self.response_suspended = false;
                self.response = Some(Response {
                    before_db: w.rms_db(),
                    change_db: delta,
                    at: now,
                });
                self.speech_samples = 0;
                self.learning_since = None;
                self.noise_db = self.noise_db.map(|(n, at)| (n + delta, at));
                delta
            }
            Ok(actual)
                if actual.device == current.device
                    && actual.revision == current.revision
                    && actual.db.is_finite() =>
            {
                // Even an unexpectedly quantized write is ours; retain readback so it can be restored.
                let delta = actual.db - current.db;
                self.owned.as_mut().unwrap().last = actual;
                self.suspended = true;
                self.response_suspended = false;
                self.status = "Mikrofonregler reagiert unerwartet – Software bleibt aktiv".into();
                delta
            }
            _ => {
                self.suspended = true;
                self.response_suspended = false;
                self.status = "Mikrofonregelung fehlgeschlagen – Software bleibt aktiv".into();
                0.0
            }
        }
    }
}

fn lease(device: &str) -> std::io::Result<File> {
    let dirs = directories::ProjectDirs::from("net", "alpha-labs", "icu")
        .ok_or_else(|| std::io::Error::other("no private cache directory"))?;
    let dir = dirs.cache_dir().join("capture-locks");
    std::fs::create_dir_all(&dir)?;
    let hash = format!("{:x}", Sha256::digest(device.as_bytes()));
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let f = options.open(dir.join(hash))?;
    f.try_lock().map_err(std::io::Error::other)?;
    Ok(f)
}

pub fn unsupported() -> BackendError {
    BackendError("capture level unavailable or changed externally".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::backend::*;
    struct Device {
        level: CaptureLevel,
        writes: usize,
        fail: bool,
    }
    impl Device {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Self {
                level: CaptureLevel {
                    device: format!("test-{}-{unique}", std::process::id()),
                    revision: 0,
                    db: -20.0,
                    min_db: -60.0,
                    max_db: 0.0,
                    step_db: 1.0,
                    muted: false,
                    channels: vec![0.1],
                    kind: "fake",
                },
                writes: 0,
                fail: false,
            }
        }
    }
    impl AudioBackend for Device {
        fn name(&self) -> &'static str {
            "fake-level"
        }
        fn devices(&self) -> Vec<DeviceInfo> {
            vec![]
        }
        fn open_capture(
            &mut self,
            _: Option<&str>,
            _: CaptureCallback,
        ) -> Result<(), BackendError> {
            Ok(())
        }
        fn open_reference(
            &mut self,
            _: Option<&str>,
            _: ReferenceCallback,
        ) -> Result<(), BackendError> {
            Ok(())
        }
        fn open_playback(
            &mut self,
            _: Option<&str>,
            _: PlaybackCallback,
        ) -> Result<(), BackendError> {
            Ok(())
        }
        fn close(&mut self, _: StreamKind) {}
        fn take_events(&mut self) -> Option<std::sync::mpsc::Receiver<BackendEvent>> {
            None
        }
        fn capture_level(&mut self) -> Result<Option<CaptureLevel>, BackendError> {
            Ok(Some(self.level.clone()))
        }
        fn set_capture_level(
            &mut self,
            expected: &CaptureLevel,
            db: f32,
        ) -> Result<CaptureLevel, BackendError> {
            if self.fail || !self.level.same(expected) {
                return Err(unsupported());
            }
            self.level.db = (db / self.level.step_db).round() * self.level.step_db;
            self.level.channels = vec![10.0f32.powf(self.level.db / 20.0)];
            self.writes += 1;
            Ok(self.level.clone())
        }
    }
    fn window(gain: f32, db: f32) -> CaptureWindow {
        CaptureWindow {
            samples: 48_000,
            speech_samples: 36_000,
            near_full: 0,
            peak: 0.2,
            speech_energy: 36_000.0 * 10f64.powf(db as f64 / 10.0),
            gain_sum: 36_000.0 * gain as f64,
            noise_samples: 12_000,
            noise_energy: 12_000.0 * 10f64.powf((db - 25.0) as f64 / 10.0),
            ..Default::default()
        }
    }
    #[test]
    fn transfers_gain_and_restores_own_changes() {
        let mut d = Device::new();
        let mut t = SystemTuner::default();
        let start = Instant::now();
        for i in 0..110 {
            let delta = d.level.db + 20.0;
            t.tick(
                &mut d,
                window(9.0 - delta, -36.0 + delta),
                start + Duration::from_secs(i),
            );
        }
        assert!((d.level.db + 14.0).abs() < 0.1, "{}", d.level.db);
        assert!(d.writes <= 6);
        assert!((t.restore(&mut d) + 6.0).abs() < 0.1);
        assert_eq!(d.level.db, -20.0);
    }
    #[test]
    fn manual_override_is_preserved_and_suspends() {
        let mut d = Device::new();
        let mut t = SystemTuner::default();
        let start = Instant::now();
        for i in 0..15 {
            t.tick(&mut d, window(8.0, -35.0), start + Duration::from_secs(i));
        }
        assert!(d.writes > 0);
        d.level.db = -25.0;
        t.tick(&mut d, window(8.0, -35.0), start + Duration::from_secs(30));
        assert!(t.suspended);
        t.restore(&mut d);
        assert_eq!(d.level.db, -25.0);
    }
    #[test]
    fn silence_noise_clipping_and_native_steps() {
        let mut d = Device::new();
        let mut t = SystemTuner::default();
        let start = Instant::now();
        for i in 0..20 {
            t.tick(
                &mut d,
                CaptureWindow::default(),
                start + Duration::from_secs(i),
            );
        }
        assert_eq!(d.writes, 0);
        for i in 20..40 {
            let mut w = window(10.0, -35.0);
            w.noise_energy = w.speech_energy / 3.0;
            t.tick(&mut d, w, start + Duration::from_secs(i));
        }
        assert_eq!(d.writes, 0, "no noise boost");
        let mut w = window(10.0, -5.0);
        w.near_full = 48;
        w.max_clip_streak_samples = 4_800;
        w.peak = 1.0;
        assert!(
            t.tick(&mut d, w, start + Duration::from_secs(50)) < 0.0,
            "clipping must reduce gain"
        );
        t.restore(&mut d);
        d.level.step_db = 4.0;
        t.tick(&mut d, w, start + Duration::from_secs(60));
        assert_eq!(d.level.db, -20.0);
    }
    #[test]
    fn one_instance_owns_a_device_and_errors_do_not_retry() {
        let mut d = Device::new();
        let mut a = SystemTuner::default();
        let mut b = SystemTuner::default();
        let now = Instant::now();
        let mut w = window(0.0, -2.0);
        w.near_full = 48;
        w.max_clip_streak_samples = 4_800;
        assert!(a.tick(&mut d, w, now) < 0.0);
        let n = d.writes;
        assert_eq!(b.tick(&mut d, w, now), 0.0);
        assert_eq!(d.writes, n);
        a.restore(&mut d);
        d.fail = true;
        b.tick(&mut d, w, now + Duration::from_secs(3));
        assert!(b.suspended);
    }

    #[test]
    fn clipping_after_a_quiet_phrase_overrides_response_validation() {
        let mut d = Device::new();
        let mut t = SystemTuner::default();
        let now = Instant::now();
        for i in 0..14 {
            t.tick(&mut d, window(9.0, -36.0), now + Duration::from_secs(i));
        }
        assert!(
            d.writes > 0,
            "the quiet phrase must have raised capture gain"
        );
        assert!(t.response.is_some());
        let mut loud = window(-12.0, -3.0);
        loud.near_full = 100;
        loud.max_clip_streak_samples = 4_800;
        loud.peak = 1.0;
        assert!(
            t.tick(&mut d, loud, now + Duration::from_secs(14)) < 0.0,
            "clipped capture must be reduced even after a large natural level jump"
        );
        assert!(!t.suspended);
        t.restore(&mut d);
    }
    #[test]
    fn later_clipping_recovers_a_response_pause_but_never_a_manual_override() {
        let mut d = Device::new();
        let mut t = SystemTuner::default();
        let now = Instant::now();
        for i in 0..14 {
            t.tick(&mut d, window(9.0, -36.0), now + Duration::from_secs(i));
        }
        t.tick(&mut d, window(-4.0, -18.0), now + Duration::from_secs(20));
        assert!(t.suspended && t.response_suspended);
        let mut clipped = window(-12.0, -3.0);
        clipped.near_full = 50;
        clipped.max_clip_streak_samples = 4_800;
        clipped.peak = 1.0;
        assert!(t.tick(&mut d, clipped, now + Duration::from_secs(21)) < 0.0);
        assert!(!t.suspended);
        d.level.db = -10.0;
        t.tick(&mut d, clipped, now + Duration::from_secs(23));
        assert!(t.suspended && !t.response_suspended);
        let writes = d.writes;
        t.tick(&mut d, clipped, now + Duration::from_secs(25));
        assert_eq!(d.writes, writes);
    }

    #[test]
    fn short_answer_does_not_consume_pending_hardware_validation() {
        let mut d = Device::new();
        let mut t = SystemTuner::default();
        let now = Instant::now();
        for i in 0..14 {
            t.tick(&mut d, window(9.0, -36.0), now + Duration::from_secs(i));
        }
        assert!(t.response.is_some());
        let mut short = window(-4.0, -18.0);
        short.speech_samples = 14_400;
        short.speech_energy = 14_400.0 * 10f64.powf(-1.8);
        t.tick(&mut d, short, now + Duration::from_secs(20));
        assert!(
            t.response.is_some(),
            "300 ms cannot discard the pending check"
        );
        assert!(!t.suspended);
        let mut enough = short;
        enough.speech_samples = 28_800;
        enough.speech_energy *= 2.0;
        t.tick(&mut d, enough, now + Duration::from_secs(21));
        assert!(t.suspended && t.response_suspended);
        t.restore(&mut d);
    }

    #[test]
    fn old_speech_and_quiet_room_cannot_authorize_a_later_boost() {
        let mut d = Device::new();
        let mut t = SystemTuner::default();
        let now = Instant::now();
        for i in 0..13 {
            t.tick(&mut d, window(9.0, -36.0), now + Duration::from_secs(i));
        }
        assert_eq!(d.writes, 0);
        let mut speech = window(9.0, -36.0);
        speech.noise_samples = 0;
        speech.noise_energy = 0.0;
        t.tick(&mut d, speech, now + Duration::from_secs(60));
        assert_eq!(d.writes, 0);
        assert_eq!(t.speech_samples, speech.speech_samples);
        assert!(t.noise_db.is_none());
    }

    #[test]
    fn isolated_clipped_impulses_do_not_rachet_microphone_down() {
        let mut d = Device::new();
        let mut t = SystemTuner::default();
        let now = Instant::now();
        for i in 0..30 {
            let impulse = CaptureWindow {
                samples: 48_000,
                near_full: 1,
                peak: 1.0,
                ..Default::default()
            };
            t.tick(&mut d, impulse, now + Duration::from_secs(i));
        }
        assert_eq!(d.writes, 0);
        let mut repeated = CaptureWindow::default();
        let mut click = [0.0; 480];
        click[..100].fill(1.0);
        for _ in 0..100 {
            repeated.observe(&click, false, 0.0);
            repeated.observe(&[0.0; 480], false, 0.0);
        }
        assert_eq!(repeated.near_full, 10_000);
        assert!(!repeated.sustained_clipping());
        assert_eq!(t.tick(&mut d, repeated, now + Duration::from_secs(30)), 0.0);
        let mut sustained = CaptureWindow::default();
        for _ in 0..10 {
            sustained.observe(&[1.0; 480], false, 0.0);
        }
        assert!(sustained.sustained_clipping());
        assert!(t.tick(&mut d, sustained, now + Duration::from_secs(31)) < 0.0);
        t.restore(&mut d);
    }
}
