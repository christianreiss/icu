//! Receive-side loudness normalization: brings each remote speaker to a
//! common level before their volume slider and the mix.
//!
//! Not the capture-side AGC `Research/feature-freeze.md` rejects: that one
//! pumps and lifts room noise into pauses. Here the level estimate only moves
//! on blocks clearly above the speaker's own noise floor (the minimum block
//! level of the last 1.5–3 s, so steady hiss never counts as speech), and in
//! pauses the gain holds. The estimate follows an upper envelope — fast up so
//! a loud onset is caught before `mixer::limit` colours it, slow down so the
//! gain does not breathe between words. One instance per speaker, 48 kHz,
//! any block length, allocates nothing.

use super::meter::{from_db, measure, to_db};

/// Target level of the speech envelope in dBFS RMS.
const TARGET_DB: f32 = -22.0;
/// Gain bounds in dB.
const MAX_BOOST_DB: f32 = 12.0;
const MAX_CUT_DB: f32 = -12.0;
/// A block counts as speech this far above the noise floor…
const SPEECH_MARGIN_DB: f32 = 10.0;
/// …and never below this absolute level.
const SPEECH_MIN_DB: f32 = -50.0;
/// Envelope time constants in seconds.
const RISE_S: f32 = 0.08;
const FALL_S: f32 = 2.0;
/// Floor = minimum over the current and the previous window of this length.
const FLOOR_WINDOW_S: f32 = 1.5;
const SR: f32 = 48_000.0;

/// Per-block smoothing factor for a time constant, given the block length.
fn alpha(seconds: f32, samples: usize) -> f32 {
    1.0 - (-(samples as f32) / (seconds * SR)).exp()
}

/// Loudness normalizer for one speaker.
#[derive(Debug, Clone)]
pub struct Normalizer {
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
    /// Fresh state: unity gain until speech has been heard.
    pub fn new() -> Normalizer {
        Normalizer {
            level_db: TARGET_DB,
            gain: 1.0,
            win_min: f32::INFINITY,
            prev_win_min: f32::INFINITY,
            win_samples: 0,
        }
    }

    /// Clears all state (when switched off, so re-enabling starts clean).
    pub fn reset(&mut self) {
        *self = Normalizer::new();
    }

    /// Current gain in dB (for tests and diagnostics).
    pub fn gain_db(&self) -> f32 {
        to_db(self.gain)
    }

    /// Processes one block in place.
    pub fn process(&mut self, block: &mut [f32]) {
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

        if rms_db > SPEECH_MIN_DB.max(floor + SPEECH_MARGIN_DB) {
            let a = if rms_db > self.level_db {
                alpha(RISE_S, block.len())
            } else {
                alpha(FALL_S, block.len())
            };
            self.level_db += a * (rms_db - self.level_db);
        }

        let target = from_db((TARGET_DB - self.level_db).clamp(MAX_CUT_DB, MAX_BOOST_DB));
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
        assert!(n.gain_db() <= MAX_BOOST_DB + 0.01);
        let mut n = Normalizer::new();
        run(&mut n, talk(0.0, -90.0, 5.0));
        assert!(n.gain_db() >= MAX_CUT_DB - 0.01);
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
}
