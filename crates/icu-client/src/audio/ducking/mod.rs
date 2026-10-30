//! Speech-driven attenuation of other applications. No microphone/profile gain
//! is changed here. Native control and recovery run in an independent process.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;
mod worker;

use serde::{Deserialize, Serialize};
use std::{
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub use worker::{run_worker, Service};

/// The saved user choice. Missing values migrate to Auto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Light,
    Medium,
    Strong,
    #[default]
    Auto,
}

impl Mode {
    pub const ALL: [(Self, &'static str); 5] = [
        (Self::Off, "Aus"),
        (Self::Light, "Leicht"),
        (Self::Medium, "Mittel"),
        (Self::Strong, "Stark"),
        (Self::Auto, "Auto"),
    ];
    pub fn label(self) -> &'static str {
        Self::ALL.iter().find(|(m, _)| *m == self).unwrap().1
    }
}

/// Only scalar evidence crosses the control boundary. Never raw audio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Input {
    pub mode: Mode,
    /// Actual backend output id, or the current system default.
    pub route: Option<String>,
    pub online: bool,
    pub transmitting_voice: bool,
    /// Audible, speech-selected playback RMS after personal/master gains.
    pub speech_db: Option<f32>,
    /// Fresh trusted ambient context; deliberately not treated as acoustic SPL.
    pub noisy_room: bool,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            route: None,
            online: false,
            transmitting_voice: false,
            speech_db: None,
            noisy_room: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub attenuation_db: f32,
    pub sessions: usize,
    pub detail: String,
    pub transmitting: bool,
    pub receiving: bool,
    pub route: Option<String>,
}

/// Lock-free handoff from DSP/mixer to the slow control thread. The timestamp
/// is process-monotonic, and stale evidence never keeps ducking active.
pub struct Signals {
    origin: Instant,
    tx_at: AtomicU64,
    rx_at: AtomicU64,
    rx_energy: AtomicU32,
}

impl Default for Signals {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            tx_at: AtomicU64::new(0),
            rx_at: AtomicU64::new(0),
            rx_energy: AtomicU32::new(0),
        }
    }
}

impl Signals {
    fn stamp(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64 + 1
    }
    pub fn transmitted(&self) {
        self.tx_at.store(self.stamp(), Ordering::Release);
    }
    pub fn played(&self, energy: f32) {
        if energy.is_finite() && energy > 1e-12 {
            self.rx_energy.store(energy.to_bits(), Ordering::Relaxed);
            self.rx_at.store(self.stamp(), Ordering::Release);
        }
    }
    pub fn snapshot(&self) -> (bool, Option<f32>) {
        let now = self.stamp();
        let recent = |at: u64| at > 0 && now.saturating_sub(at) <= 80;
        (
            recent(self.tx_at.load(Ordering::Acquire)),
            recent(self.rx_at.load(Ordering::Acquire)).then(|| {
                10.0 * f32::from_bits(self.rx_energy.load(Ordering::Relaxed))
                    .max(1e-12)
                    .log10()
            }),
        )
    }
    pub fn clear(&self) {
        self.tx_at.store(0, Ordering::Release);
        self.rx_at.store(0, Ordering::Release);
    }
}

/// Comparable pre-duck RMS energy from the affected applications. Unsupported
/// meters use a named fixed fallback; peak values are never passed as RMS.
#[derive(Debug, Clone, Copy, Default)]
pub struct Background {
    pub rms_db: Option<f32>,
    pub audible: Option<bool>,
}

/// One envelope for the complete room: adding speakers cannot multiply gains.
pub struct Envelope {
    db: f32,
    held_target: f32,
    last_voice: Option<Instant>,
    last: Instant,
    speech_energy: Option<f32>,
    background_energy: Option<f32>,
}

impl Envelope {
    pub fn new(now: Instant) -> Self {
        Self {
            db: 0.0,
            held_target: 0.0,
            last_voice: None,
            last: now,
            speech_energy: None,
            background_energy: None,
        }
    }
    pub fn reset(&mut self, now: Instant) {
        *self = Self::new(now);
    }
    pub fn update(
        &mut self,
        input: &Input,
        background: Background,
        now: Instant,
    ) -> (f32, &'static str) {
        let elapsed = now.saturating_duration_since(self.last);
        self.last = now;
        // A suspend or lost heartbeat expires activity; do not replay an envelope.
        if elapsed > Duration::from_secs(1) || !input.online || input.mode == Mode::Off {
            self.reset(now);
            return (0.0, "Aus");
        }
        let dt = elapsed.as_secs_f32().min(0.1);
        let speech = input.speech_db.filter(|x| x.is_finite() && *x > -70.0);
        let talking = input.transmitting_voice || speech.is_some();
        let mut reason = "Feste Absenkung";
        let target = match input.mode {
            Mode::Off => 0.0,
            Mode::Light => 6.0,
            Mode::Medium => 12.0,
            Mode::Strong => 18.0,
            Mode::Auto => {
                let bg = background.rms_db.filter(|x| x.is_finite());
                if background.audible == Some(false) {
                    reason = "Kein hörbarer Hintergrund";
                    0.0
                } else if let (Some(fg), Some(bg)) = (speech, bg) {
                    let smooth = |slot: &mut Option<f32>, db: f32| {
                        let energy = 10.0f32.powf(db.clamp(-120.0, 12.0) / 10.0);
                        let previous = slot.unwrap_or(energy);
                        let tau = if energy > previous { 0.25 } else { 0.8 };
                        let value = previous + (energy - previous) * (1.0 - (-dt / tau).exp());
                        *slot = Some(value);
                        10.0 * value.max(1e-12).log10()
                    };
                    let fg = smooth(&mut self.speech_energy, fg);
                    let bg = smooth(&mut self.background_energy, bg);
                    reason = "Auto · Sprach-/Hintergrundenergie";
                    let margin = if input.noisy_room { 12.0 } else { 10.0 };
                    (bg - fg + margin).clamp(0.0, 24.0)
                } else {
                    reason = if speech.is_none() && input.transmitting_voice {
                        "Auto · eigene Sprache, Mittel"
                    } else {
                        "Auto · keine vergleichbare Hintergrundmessung, Mittel"
                    };
                    12.0
                }
            }
        };
        if talking {
            self.last_voice = Some(now);
            self.held_target = target;
        }
        let holding = self
            .last_voice
            .is_some_and(|t| now.duration_since(t) <= Duration::from_millis(300));
        let target = if holding { self.held_target } else { 0.0 };
        // 50 ms attack, 750 ms release at the selected depth. Slow auto target changes
        // once established; the input meter is reconstructed before our gain.
        let speed = if target > self.db {
            target.max(1.0) / 0.05
        } else {
            self.held_target.max(self.db).max(1.0) / 0.75
        };
        self.db += (target - self.db).clamp(-speed * dt, speed * dt);
        self.db = self.db.clamp(0.0, 24.0);
        if self.db < 0.01 {
            self.db = 0.0;
        }
        (self.db, reason)
    }
}

/// Compare-and-restore ownership of a native channel vector. Muting and channel
/// balance are separate user controls and are never written by this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Gain {
    pub baseline: Vec<f32>,
    pub last: Vec<f32>,
    pub previous: Vec<f32>,
    pub yielded: bool,
}

pub(crate) fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.is_finite() && b.is_finite() && (a - b).abs() <= 0.00005)
}

impl Gain {
    pub fn new(baseline: Vec<f32>) -> Option<Self> {
        (!baseline.is_empty()
            && baseline.len() <= 64
            && baseline
                .iter()
                .all(|v| v.is_finite() && *v >= 0.0 && *v <= 4.0))
        .then(|| Self {
            last: baseline.clone(),
            previous: baseline.clone(),
            baseline,
            yielded: false,
        })
    }
    pub fn target(&self, db: f32) -> Vec<f32> {
        let factor = 10.0f32.powf(-db.clamp(0.0, 24.0) / 20.0);
        self.baseline.iter().map(|v| v * factor).collect()
    }
    pub fn owns(&self, current: &[f32]) -> bool {
        !self.yielded && same(current, &self.last)
    }
    pub fn written(&mut self, next: Vec<f32>) {
        self.previous = std::mem::replace(&mut self.last, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_setting_migrates_to_auto() {
        let s: crate::config::Settings = toml::from_str("[audio]\n").unwrap();
        assert_eq!(s.audio.ducking, Mode::Auto);
        for (mode, _) in Mode::ALL {
            let mut s = s.clone();
            s.audio.ducking = mode;
            let round: crate::config::Settings =
                toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
            assert_eq!(round.audio.ducking, mode);
        }
    }
    #[test]
    fn one_envelope_holds_words_releases_and_never_pumps_on_constant_input() {
        let start = Instant::now();
        let mut env = Envelope::new(start);
        let mut i = Input {
            mode: Mode::Auto,
            online: true,
            speech_db: Some(-22.0),
            ..Input::default()
        };
        let bg = Background {
            rms_db: Some(-18.0),
            audible: Some(true),
        };
        for tick in 1..=300 {
            let (db, _) = env.update(&i, bg, start + Duration::from_millis(tick * 10));
            if tick > 10 {
                assert!((db - 14.0).abs() < 0.001);
            }
        }
        i.speech_db = None;
        for tick in 301..=325 {
            assert!(
                (env.update(&i, bg, start + Duration::from_millis(tick * 10))
                    .0
                    - 14.0)
                    .abs()
                    < 0.001
            );
        }
        for tick in 326..=410 {
            env.update(&i, bg, start + Duration::from_millis(tick * 10));
        }
        assert_eq!(env.db, 0.0);
    }
    #[test]
    fn noise_alone_and_off_do_not_duck_and_local_voice_has_bounded_fallback() {
        let now = Instant::now();
        let mut e = Envelope::new(now);
        let mut i = Input {
            mode: Mode::Auto,
            online: true,
            noisy_room: true,
            ..Input::default()
        };
        let bg = Background {
            rms_db: Some(-5.0),
            audible: Some(true),
        };
        assert_eq!(e.update(&i, bg, now + Duration::from_millis(20)).0, 0.0);
        i.transmitting_voice = true;
        for tick in 2..10 {
            e.update(&i, bg, now + Duration::from_millis(tick * 20));
        }
        assert_eq!(e.db, 12.0);
        i.mode = Mode::Off;
        assert_eq!(e.update(&i, bg, now + Duration::from_millis(200)).0, 0.0);
    }
    #[test]
    fn gain_is_relative_to_baseline_and_manual_changes_win() {
        let mut g = Gain::new(vec![0.8, 0.4]).unwrap();
        let target = g.target(12.0);
        g.written(target.clone());
        assert_eq!(g.target(12.0), target);
        assert!((target[0] / target[1] - 2.0).abs() < 1e-6);
        assert!(g.owns(&target));
        assert!(!g.owns(&[0.3, 0.15]));
        assert_eq!(g.target(0.0), [0.8, 0.4]);
        assert!(Gain::new(vec![f32::NAN]).is_none());
    }
}
