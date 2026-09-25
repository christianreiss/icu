//! Loudness normalization: brings a voice to a common level.
//!
//! Three uses, one per [`Profile`]: receive side (each remote speaker before
//! their volume slider and the mix), mic auto leveling (a slow automatic
//! input gain ahead of denoise and VAD) and outbound normalization (right
//! before the encoder). The level estimate only moves on blocks clearly
//! above the voice's own noise floor (the minimum block level of the last
//! 1.5–3 s, so steady hiss never counts as speech) and, on the send side,
//! only while the VAD hears voice; in pauses the gain holds, so nothing
//! pumps or lifts room noise. The estimate follows an upper envelope — fast
//! up so a loud onset is caught early, slow down so the gain does not
//! breathe between words. 48 kHz, any block length, allocates nothing.

use super::meter::{from_db, measure, to_db};

/// Target, bounds and speed of one normalizer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Profile {
    /// Target level of the speech envelope in dBFS RMS.
    pub target_db: f32,
    /// Gain bounds in dB.
    pub max_boost_db: f32,
    pub max_cut_db: f32,
    /// Envelope time constants in seconds.
    pub rise_s: f32,
    pub fall_s: f32,
}

impl Profile {
    /// Each remote speaker, in the mixer.
    pub const RECEIVE: Profile = Profile {
        target_db: -22.0,
        max_boost_db: 12.0,
        max_cut_db: -12.0,
        rise_s: 0.08,
        fall_s: 2.0,
    };
    /// Mic auto leveling: a slow hand on the input fader.
    pub const MIC_LEVEL: Profile = Profile {
        target_db: -22.0,
        max_boost_db: 15.0,
        max_cut_db: -10.0,
        rise_s: 0.5,
        fall_s: 4.0,
    };
    /// Own voice right before the encoder.
    pub const OUTBOUND: Profile = Profile::RECEIVE;
}

/// A block counts as speech this far above the noise floor…
const SPEECH_MARGIN_DB: f32 = 10.0;
/// …and never below this absolute level.
const SPEECH_MIN_DB: f32 = -50.0;
/// Floor = minimum over the current and the previous window of this length.
const FLOOR_WINDOW_S: f32 = 1.5;
const SR: f32 = 48_000.0;

/// Per-block smoothing factor for a time constant, given the block length.
fn alpha(seconds: f32, samples: usize) -> f32 {
    1.0 - (-(samples as f32) / (seconds * SR)).exp()
}

/// Loudness normalizer for one voice.
#[derive(Debug, Clone)]
pub struct Normalizer {
    profile: Profile,
    /// Speech envelope in dBFS; starts at the target (unity gain).
    level_db: f32,
    /// Linear gain applied at the end of the last block.
    gain: f32,
    win_min: f32,
    prev_win_min: f32,
    /// Samples seen in the current floor window.
    win_samples: usize,
}

impl Default for Normalizer {
    fn default() -> Self {
        Normalizer::new()
    }
}

impl Normalizer {
    /// Receive-side normalizer ([`Profile::RECEIVE`]).
    pub fn new() -> Normalizer {
        Normalizer::with(Profile::RECEIVE)
    }

    /// Fresh state: unity gain until speech has been heard.
    pub fn with(profile: Profile) -> Normalizer {
        Normalizer {
            profile,
            level_db: profile.target_db,
            gain: 1.0,
            win_min: f32::INFINITY,
            prev_win_min: f32::INFINITY,
            win_samples: 0,
        }
    }

    /// Clears all state (when switched off, so re-enabling starts clean).
    pub fn reset(&mut self) {
        *self = Normalizer::with(self.profile);
    }

    /// Current gain in dB (for tests and diagnostics).
    pub fn gain_db(&self) -> f32 {
        to_db(self.gain)
    }

    /// Processes one block in place.
    pub fn process(&mut self, block: &mut [f32]) {
        self.process_hinted(block, None);
    }

    /// Like [`Normalizer::process`]; `Some(false)` (the VAD hears no voice)
    /// holds the level estimate. The noise floor keeps tracking either way.
    pub fn process_hinted(&mut self, block: &mut [f32], voice: Option<bool>) {
        if block.is_empty() {
            return;
        }
        for s in block.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        let rms_db = measure(block).rms_db;

        // Exact silence (jitter-buffer gaps, DTX) says nothing about the
        // speaker's noise floor.
        if rms_db > -99.0 {
            self.win_min = self.win_min.min(rms_db);
        }
        self.win_samples += block.len();
        if self.win_samples as f32 >= FLOOR_WINDOW_S * SR {
            self.prev_win_min = self.win_min;
            self.win_min = f32::INFINITY;
            self.win_samples = 0;
        }
        let floor = self.win_min.min(self.prev_win_min);

        let p = self.profile;
        if voice != Some(false) && rms_db > SPEECH_MIN_DB.max(floor + SPEECH_MARGIN_DB) {
            let a = if rms_db > self.level_db {
                alpha(p.rise_s, block.len())
            } else {
                alpha(p.fall_s, block.len())
            };
            self.level_db += a * (rms_db - self.level_db);
        }

        let target = from_db((p.target_db - self.level_db).clamp(p.max_cut_db, p.max_boost_db));
        let start = self.gain;
        let step = (target - start) / block.len() as f32;
        for (i, s) in block.iter_mut().enumerate() {
            *s *= start + step * (i + 1) as f32;
        }
        self.gain = target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 480;

    /// Speech-like test signal: 300 ms of 220 Hz tone at `db` dBFS RMS, then
    /// 100 ms of `noise_db` noise, repeated for `seconds`. Yields blocks.
    fn talk(db: f32, noise_db: f32, seconds: f32) -> Vec<Vec<f32>> {
        let amp = from_db(db) * std::f32::consts::SQRT_2;
        let noise = from_db(noise_db) * 3f32.sqrt();
        let mut rng = 0x1234_5678u32;
        let n = (seconds * SR) as usize / BLOCK;
        (0..n)
            .map(|b| {
                let speaking = (b % 40) < 30;
                (0..BLOCK)
                    .map(|i| {
                        rng ^= rng << 13;
                        rng ^= rng >> 17;
                        rng ^= rng << 5;
                        let r = (rng as f32 / u32::MAX as f32) * 2.0 - 1.0;
                        let t = (b * BLOCK + i) as f32 / SR;
                        let tone = if speaking {
                            amp * (t * 220.0 * std::f32::consts::TAU).sin()
                        } else {
                            0.0
                        };
                        tone + noise * r
                    })
                    .collect()
            })
            .collect()
    }

    fn run(n: &mut Normalizer, blocks: Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        blocks
            .into_iter()
            .map(|mut b| {
                n.process(&mut b);
                b
            })
            .collect()
    }

    #[test]
    fn quiet_speaker_is_raised() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-34.0, -70.0, 10.0));
        assert!(n.gain_db() > 9.0, "gain {}", n.gain_db());
    }

    #[test]
    fn loud_speaker_is_lowered_quickly() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-8.0, -70.0, 1.0));
        assert!(n.gain_db() < -9.0, "gain {}", n.gain_db());
    }

    #[test]
    fn gain_is_bounded() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-48.0, -90.0, 20.0));
        assert!(n.gain_db() <= Profile::RECEIVE.max_boost_db + 0.01);
        let mut n = Normalizer::new();
        run(&mut n, talk(0.0, -90.0, 5.0));
        assert!(n.gain_db() >= Profile::RECEIVE.max_cut_db - 0.01);
    }

    #[test]
    fn silence_holds_gain() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-34.0, -70.0, 10.0));
        let before = n.gain_db();
        run(&mut n, vec![vec![0.0; BLOCK]; 500]);
        assert!((n.gain_db() - before).abs() < 0.01);
    }

    #[test]
    fn steady_hiss_is_not_boosted() {
        // Hiss above the absolute gate, no speech at all.
        let mut n = Normalizer::new();
        run(&mut n, talk(-100.0, -40.0, 10.0));
        assert!(n.gain_db().abs() < 1.0, "gain {}", n.gain_db());
    }

    #[test]
    fn speech_over_hiss_is_normalized() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-30.0, -45.0, 10.0));
        assert!(n.gain_db() > 5.0, "gain {}", n.gain_db());
    }

    #[test]
    fn output_is_finite() {
        let mut n = Normalizer::new();
        let mut b = vec![f32::NAN, f32::INFINITY, 0.5, -0.5];
        n.process(&mut b);
        assert!(b.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn reset_restores_unity() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-34.0, -70.0, 10.0));
        n.reset();
        assert_eq!(n.gain_db(), 0.0);
    }

    #[test]
    fn mic_level_is_slow() {
        let mut n = Normalizer::with(Profile::MIC_LEVEL);
        run(&mut n, talk(-8.0, -70.0, 0.3));
        assert!(n.gain_db() > -9.0, "gain {}", n.gain_db());
        run(&mut n, talk(-8.0, -70.0, 10.0));
        assert!(n.gain_db() < -9.5, "gain {}", n.gain_db());
    }

    #[test]
    fn mic_level_boost_is_bounded() {
        let mut n = Normalizer::with(Profile::MIC_LEVEL);
        run(&mut n, talk(-48.0, -90.0, 30.0));
        assert!(
            n.gain_db() > 12.0 && n.gain_db() <= 15.01,
            "gain {}",
            n.gain_db()
        );
    }

    #[test]
    fn no_voice_hint_holds_gain() {
        let mut n = Normalizer::new();
        for mut b in talk(-34.0, -70.0, 10.0) {
            n.process_hinted(&mut b, Some(false));
        }
        assert_eq!(n.gain_db(), 0.0);
        for mut b in talk(-34.0, -70.0, 10.0) {
            n.process_hinted(&mut b, Some(true));
        }
        assert!(n.gain_db() > 9.0, "gain {}", n.gain_db());
    }

    #[test]
    fn reset_keeps_profile() {
        let mut n = Normalizer::with(Profile::MIC_LEVEL);
        n.reset();
        assert_eq!(n.profile, Profile::MIC_LEVEL);
    }
}
