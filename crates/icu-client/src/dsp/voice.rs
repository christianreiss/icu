//! Optional voice optimizer: 80 Hz high-pass → compressor → peak limiter.
//!
//! Fixed, non-adaptive dynamics (no AGC, `Research/feature-freeze.md`).
//! Nonlinear, therefore strictly **after** AEC: in front of it the adaptive
//! filter would lose the linear relation to the reference
//! (`Research/aec.md`). No lookahead, so no added latency. Works on any
//! block length at 48 kHz, allocates nothing per block.

use serde::{Deserialize, Serialize};

const SR: f32 = 48_000.0;
/// High-pass corner in Hz.
const HPF_HZ: f32 = 80.0;
/// Soft-knee width in dB.
const KNEE_DB: f32 = 6.0;
const ATTACK_S: f32 = 0.005;
const RELEASE_S: f32 = 0.120;
/// Limiter ceiling, −1 dBFS.
const CEILING: f32 = 0.891_250_9;
const LIMITER_RELEASE_S: f32 = 0.050;

/// Compressor preset as written to the settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VoiceStrength {
    /// −18 dBFS, 2:1, +3 dB.
    Light,
    /// −24 dBFS, 3:1, +6 dB.
    #[default]
    Medium,
    /// −30 dBFS, 4:1, +9 dB.
    Strong,
}

impl VoiceStrength {
    /// `(threshold_db, ratio, makeup_db)`.
    fn params(self) -> (f32, f32, f32) {
        match self {
            VoiceStrength::Light => (-18.0, 2.0, 3.0),
            VoiceStrength::Medium => (-24.0, 3.0, 6.0),
            VoiceStrength::Strong => (-30.0, 4.0, 9.0),
        }
    }

    /// Stable index for an atomic.
    pub fn to_u32(self) -> u32 {
        match self {
            VoiceStrength::Light => 0,
            VoiceStrength::Medium => 1,
            VoiceStrength::Strong => 2,
        }
    }

    /// Inverse of [`VoiceStrength::to_u32`]; unknown values → `Medium`.
    pub fn from_u32(v: u32) -> VoiceStrength {
        match v {
            0 => VoiceStrength::Light,
            2 => VoiceStrength::Strong,
            _ => VoiceStrength::Medium,
        }
    }
}

fn coef(seconds: f32) -> f32 {
    (-1.0 / (seconds * SR)).exp()
}

/// Flushes values that would decay into denormals.
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 {
        0.0
    } else {
        x
    }
}

/// Second-order Butterworth high-pass (RBJ), direct form I.
#[derive(Debug, Clone)]
struct Hpf {
    b: [f32; 3],
    a: [f32; 2],
    x: [f32; 2],
    y: [f32; 2],
}

impl Hpf {
    fn new() -> Hpf {
        let w = 2.0 * std::f32::consts::PI * HPF_HZ / SR;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        Hpf {
            b: [
                (1.0 + cos) / 2.0 / a0,
                -(1.0 + cos) / a0,
                (1.0 + cos) / 2.0 / a0,
            ],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn process(&mut self, s: f32) -> f32 {
        let y = self.b[0] * s + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [s, self.x[0]];
        self.y = [flush(y), self.y[0]];
        self.y[0]
    }

    fn reset(&mut self) {
        self.x = [0.0; 2];
        self.y = [0.0; 2];
    }
}

/// Feed-forward compressor with soft knee; the gain reduction (dB) is
/// smoothed with separate attack and release.
#[derive(Debug, Clone)]
struct Compressor {
    threshold_db: f32,
    slope: f32,
    makeup_db: f32,
    attack: f32,
    release: f32,
    /// Smoothed gain reduction in dB (≤ 0).
    gr_db: f32,
}

impl Compressor {
    fn new(strength: VoiceStrength) -> Compressor {
        let mut c = Compressor {
            threshold_db: 0.0,
            slope: 0.0,
            makeup_db: 0.0,
            attack: coef(ATTACK_S),
            release: coef(RELEASE_S),
            gr_db: 0.0,
        };
        c.set_strength(strength);
        c
    }

    fn set_strength(&mut self, strength: VoiceStrength) {
        let (t, r, m) = strength.params();
        self.threshold_db = t;
        self.slope = 1.0 / r - 1.0;
        self.makeup_db = m;
    }

    /// Static gain reduction for an input level in dB.
    fn curve(&self, level_db: f32) -> f32 {
        let over = level_db - self.threshold_db;
        if 2.0 * over < -KNEE_DB {
            0.0
        } else if 2.0 * over.abs() <= KNEE_DB {
            self.slope * (over + KNEE_DB / 2.0).powi(2) / (2.0 * KNEE_DB)
        } else {
            self.slope * over
        }
    }

    fn process(&mut self, s: f32) -> f32 {
        let level_db = 20.0 * (s.abs() + 1e-9).log10();
        let target = self.curve(level_db);
        let a = if target < self.gr_db {
            self.attack
        } else {
            self.release
        };
        self.gr_db = a * self.gr_db + (1.0 - a) * target;
        s * 10f32.powf((self.gr_db + self.makeup_db) / 20.0)
    }

    fn reset(&mut self) {
        self.gr_db = 0.0;
    }
}

/// Peak limiter without lookahead: instant attack, exponential release,
/// hard clamp as a backstop.
#[derive(Debug, Clone)]
struct Limiter {
    gain: f32,
    release: f32,
}

impl Limiter {
    fn new() -> Limiter {
        Limiter {
            gain: 1.0,
            release: coef(LIMITER_RELEASE_S),
        }
    }

    fn process(&mut self, s: f32) -> f32 {
        let a = s.abs();
        let target = if a > CEILING { CEILING / a } else { 1.0 };
        if target < self.gain {
            self.gain = target;
        } else {
            self.gain = self.release * self.gain + (1.0 - self.release) * target;
        }
        (s * self.gain).clamp(-CEILING, CEILING)
    }

    fn reset(&mut self) {
        self.gain = 1.0;
    }
}

/// The whole chain.
#[derive(Debug, Clone)]
pub struct VoiceChain {
    hpf: Hpf,
    comp: Compressor,
    limiter: Limiter,
}

impl VoiceChain {
    /// A fresh chain with the given preset.
    pub fn new(strength: VoiceStrength) -> VoiceChain {
        VoiceChain {
            hpf: Hpf::new(),
            comp: Compressor::new(strength),
            limiter: Limiter::new(),
        }
    }

    /// Switches the preset; filter and envelope state are kept.
    pub fn set_strength(&mut self, strength: VoiceStrength) {
        self.comp.set_strength(strength);
    }

    /// Clears all state (after enabling, or when the streams restart).
    pub fn reset(&mut self) {
        self.hpf.reset();
        self.comp.reset();
        self.limiter.reset();
    }

    /// Processes one block in place.
    pub fn process(&mut self, block: &mut [f32]) {
        for s in block.iter_mut() {
            let x = if s.is_finite() { *s } else { 0.0 };
            *s = self.limiter.process(self.comp.process(self.hpf.process(x)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, dbfs: f32, secs: f32) -> Vec<f32> {
        let amp = 10f32.powf(dbfs / 20.0);
        (0..(secs * SR) as usize)
            .map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / SR).sin())
            .collect()
    }

    /// RMS in dB of the second half (after settling).
    fn rms_db(x: &[f32]) -> f32 {
        let tail = &x[x.len() / 2..];
        let ms = tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32;
        10.0 * ms.max(1e-20).log10()
    }

    fn run_hpf(x: &mut [f32]) {
        let mut h = Hpf::new();
        for s in x.iter_mut() {
            *s = h.process(*s);
        }
    }

    fn run_chain(strength: VoiceStrength, x: &mut [f32]) {
        let mut c = VoiceChain::new(strength);
        for block in x.chunks_mut(480) {
            c.process(block);
        }
    }

    #[test]
    fn hpf_cuts_rumble_keeps_voice() {
        let mut low = sine(30.0, -20.0, 1.0);
        let mut mid = sine(1000.0, -20.0, 1.0);
        let (l0, m0) = (rms_db(&low), rms_db(&mid));
        run_hpf(&mut low);
        run_hpf(&mut mid);
        assert!(
            l0 - rms_db(&low) > 12.0,
            "30 Hz only {:.1} dB down",
            l0 - rms_db(&low)
        );
        assert!((m0 - rms_db(&mid)).abs() < 0.5);
    }

    #[test]
    fn below_threshold_only_makeup() {
        let mut x = sine(1000.0, -40.0, 1.0);
        let before = rms_db(&x);
        run_chain(VoiceStrength::Medium, &mut x);
        let gain = rms_db(&x) - before;
        assert!((gain - 6.0).abs() < 0.5, "gain {gain:.2} dB");
    }

    #[test]
    fn loud_and_quiet_move_closer() {
        let mut loud = sine(1000.0, -6.0, 1.0);
        let mut quiet = sine(1000.0, -30.0, 1.0);
        let d_in = rms_db(&loud) - rms_db(&quiet);
        run_chain(VoiceStrength::Medium, &mut loud);
        run_chain(VoiceStrength::Medium, &mut quiet);
        let d_out = rms_db(&loud) - rms_db(&quiet);
        // 24 dB in; 18 dB over a 3:1 threshold → ~12 dB reduction.
        assert!(
            d_out > 9.0 && d_out < 15.0,
            "in {d_in:.1} dB, out {d_out:.1} dB"
        );
    }

    #[test]
    fn limiter_holds_ceiling() {
        let mut x = sine(1000.0, 6.0, 0.5);
        x[1000] = f32::NAN;
        run_chain(VoiceStrength::Strong, &mut x);
        assert!(x.iter().all(|s| s.is_finite()));
        let peak = x.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= CEILING + 1e-6, "peak {peak}");
    }

    #[test]
    fn silence_stays_silent() {
        let mut x = sine(1000.0, -10.0, 0.2);
        x.extend(std::iter::repeat_n(0.0, 48_000));
        run_chain(VoiceStrength::Strong, &mut x);
        let tail = &x[x.len() - 480..];
        assert!(tail.iter().all(|s| *s == 0.0 || s.is_normal()));
        assert!(tail.iter().fold(0.0f32, |m, s| m.max(s.abs())) < 1e-6);
    }

    #[test]
    fn strength_roundtrips_through_u32() {
        for s in [
            VoiceStrength::Light,
            VoiceStrength::Medium,
            VoiceStrength::Strong,
        ] {
            assert_eq!(VoiceStrength::from_u32(s.to_u32()), s);
        }
    }
}
