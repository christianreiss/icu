//! The send-side chain behind the AEC (`docs/reference/client.md` §6):
//! manual gain → mic auto level → denoise → meter → (the caller's VAD
//! verdict) → voice optimizer → peer trim → outbound normalizer and limiter. One struct,
//! so the engine's DSP thread and the cascade test (`tests/cascade.rs`) run
//! the same code with the same switches.
//!
//! The VAD verdict is decided between the two halves because it needs the
//! measured block (RNNoise probability, level) and the gate's state, which
//! belong to the caller; the outbound normalizer needs the verdict for this
//! block and the leveler for the previous one.

use super::{
    denoise::Denoiser,
    meter::{self, Level},
    normalize::{Normalizer, Profile},
    voice::{Limiter, VoiceChain, VoiceStrength},
};
use crate::config::AudioSettings;

/// The switches of the chain, read from the settings every block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SendSettings {
    /// Manual microphone gain, linear.
    pub gain: f32,
    /// Mic auto leveling ([`Profile::MIC_LEVEL`]).
    pub auto_level: bool,
    /// RNNoise.
    pub denoise: bool,
    /// The voice optimizer.
    pub voice_opt: bool,
    pub voice_strength: VoiceStrength,
    /// Outbound normalization: the peak limiter, plus [`Profile::OUTBOUND`]
    /// when `auto_level` is off (one leveler per direction).
    pub normalize_outgoing: bool,
}

impl SendSettings {
    /// The switches as `[audio]` sets them: auto leveling only together with
    /// the denoiser (the energy VAD would open on boosted room noise).
    pub fn from_audio(a: &AudioSettings) -> SendSettings {
        SendSettings {
            gain: meter::from_db(a.gain_db),
            auto_level: a.auto_level && a.denoise,
            denoise: a.denoise,
            voice_opt: a.voice_opt,
            voice_strength: a.voice_strength,
            normalize_outgoing: a.normalize_outgoing,
        }
    }
}

impl Default for SendSettings {
    /// The defaults of a fresh installation.
    fn default() -> Self {
        SendSettings::from_audio(&AudioSettings::default())
    }
}

/// What the first half of the chain measured on a block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Analysis {
    /// RNNoise voice probability, if the denoiser ran.
    pub prob: Option<f32>,
    /// Level after gain, leveling and denoise: what the meter shows.
    pub level: Level,
}

/// The chain's state.
pub struct SendChain {
    leveler: Normalizer,
    level_on: bool,
    /// The previous block's VAD verdict steers the leveler, which runs ahead
    /// of the VAD.
    prev_voice: bool,
    denoiser: Denoiser,
    voice_chain: VoiceChain,
    voice_on: bool,
    strength: VoiceStrength,
    out_norm: Normalizer,
    out_limiter: Limiter,
    out_norm_on: bool,
    limiter_on: bool,
    /// Slow feedback trim belongs after the fixed compressor, which would
    /// otherwise compress away corrections made to the microphone target.
    peer_gain: f32,
    applied_peer_gain: f32,
}

impl Default for SendChain {
    fn default() -> Self {
        SendChain::new()
    }
}

impl SendChain {
    /// Fresh state, everything at unity.
    pub fn new() -> SendChain {
        SendChain {
            leveler: Normalizer::with(Profile::MIC_LEVEL),
            level_on: false,
            prev_voice: false,
            denoiser: Denoiser::new(),
            voice_chain: VoiceChain::new(VoiceStrength::default()),
            voice_on: false,
            strength: VoiceStrength::default(),
            out_norm: Normalizer::with(Profile::OUTBOUND),
            out_limiter: Limiter::new(),
            out_norm_on: false,
            limiter_on: false,
            peer_gain: 1.0,
            applied_peer_gain: 1.0,
        }
    }

    /// Clears the adaptive state after the streams were reopened: the
    /// device may have changed, and a held gain for the old microphone
    /// would hit the new one. The denoiser starts over too.
    pub fn restart(&mut self) {
        self.denoiser = Denoiser::new();
        self.leveler.reset();
        self.prev_voice = false;
        self.voice_chain.reset();
        self.out_norm.reset();
        self.out_limiter.reset();
        self.peer_gain = 1.0;
        self.applied_peer_gain = 1.0;
    }

    /// First half: gain, auto level, denoise, meter.
    pub fn analyze(&mut self, block: &mut [f32], s: &SendSettings) -> Analysis {
        if s.gain != 1.0 {
            for x in block.iter_mut() {
                *x *= s.gain;
            }
        }
        if s.auto_level && !self.level_on {
            self.leveler.reset();
        }
        self.level_on = s.auto_level;
        if s.auto_level {
            self.leveler.process_hinted(block, Some(self.prev_voice));
        }
        let prob = if s.denoise {
            let p = self.denoiser.process(block);
            if p.is_finite() {
                Some(p)
            } else {
                // Its state is no number: start over rather than leave the
                // VAD deaf until the next restart.
                self.denoiser = Denoiser::new();
                block.fill(0.0);
                None
            }
        } else {
            None
        };
        Analysis {
            prob,
            level: meter::measure(block),
        }
    }

    /// Second half: voice optimizer, outbound normalizer and limiter.
    /// `voice` is this block's VAD verdict.
    pub fn finish(&mut self, block: &mut [f32], voice: bool, s: &SendSettings) {
        self.prev_voice = voice;
        // Nonlinear, so after AEC; after the meter and VAD, so its makeup
        // gain does not move the thresholds.
        if s.voice_strength != self.strength {
            self.strength = s.voice_strength;
            self.voice_chain.set_strength(s.voice_strength);
        }
        if s.voice_opt && !self.voice_on {
            self.voice_chain.reset();
        }
        self.voice_on = s.voice_opt;
        if s.voice_opt {
            self.voice_chain.process(block);
        }
        // One local AGC and one slow remote trim, not two independent AGCs.
        // Ramp the trim over a block to avoid a click on a new hint. It must
        // not affect the denoiser, VAD, or system-gain transfer estimator.
        let peer_target = if s.auto_level && s.denoise {
            self.peer_gain
        } else {
            1.0
        };
        let peer_active = peer_target != 1.0 || self.applied_peer_gain != 1.0;
        if peer_active && !block.is_empty() {
            let start = self.applied_peer_gain;
            let step = (peer_target - start) / block.len() as f32;
            for (i, sample) in block.iter_mut().enumerate() {
                *sample *= start + step * (i + 1) as f32;
            }
        }
        self.applied_peer_gain = peer_target;
        // One leveler per direction (`dsp::normalize`): while the mic
        // leveler runs, outbound normalization is the limiter alone; the
        // normalizer takes over only when the leveler is off.
        let out_level = s.normalize_outgoing && !s.auto_level;
        if out_level && !self.out_norm_on {
            self.out_norm.reset();
        }
        self.out_norm_on = out_level;
        if out_level {
            self.out_norm.process_hinted(block, Some(voice));
        }
        // A peer boost always retains peak protection, including when the
        // user has switched off ordinary outbound normalization.
        let limit = s.normalize_outgoing || peer_active;
        if limit && !self.limiter_on {
            self.out_limiter.reset();
        }
        self.limiter_on = limit;
        if limit {
            for x in block.iter_mut() {
                *x = self.out_limiter.process(*x);
            }
        }
    }

    /// Apply the corroborated peer trim; compensate confirmed native gain
    /// changes in the microphone AGC independently.
    pub fn tune(&mut self, peer_offset_db: f32, capture_delta_db: f32) {
        self.peer_gain = if peer_offset_db.is_finite() {
            let bound = super::feedback::MAX_PEER_TRIM_DB;
            meter::from_db(peer_offset_db.clamp(-bound, bound))
        } else {
            1.0
        };
        if capture_delta_db != 0.0 {
            self.leveler.capture_changed(capture_delta_db);
        }
    }

    /// Gains of the leveler and the outbound normalizer in dB (tests and
    /// diagnostics).
    pub fn gains_db(&self) -> (f32, f32) {
        (self.leveler.gain_db(), self.out_norm.gain_db())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testsignal;

    #[test]
    fn peer_trim_reaches_output_after_compression_without_moving_mic_agc() {
        let settings = SendSettings::default();
        for trim_db in [-6.0, 6.0] {
            let mut baseline = SendChain::new();
            let mut assisted = SendChain::new();
            assisted.tune(trim_db, 0.0);
            for b in 0..100 {
                let input: Vec<f32> = (0..480)
                    .map(|i| {
                        ((b * 480 + i) as f32 * 220.0 / 48_000.0 * std::f32::consts::TAU).sin()
                            * 0.05
                    })
                    .collect();
                let mut plain = input.clone();
                let mut tuned = input;
                baseline.finish(&mut plain, true, &settings);
                assisted.finish(&mut tuned, true, &settings);
                if b > 0 {
                    let delta = meter::measure(&tuned).rms_db - meter::measure(&plain).rms_db;
                    assert!((delta - trim_db).abs() < 0.01, "{delta} vs {trim_db}");
                }
                assert_eq!(baseline.gains_db(), assisted.gains_db());
            }
        }
    }

    #[test]
    fn peer_boost_keeps_limiter_and_disabled_leveling_removes_trim() {
        let mut chain = SendChain::new();
        let mut settings = SendSettings {
            voice_opt: false,
            normalize_outgoing: false,
            ..SendSettings::default()
        };
        chain.tune(99.0, 0.0);
        assert!((meter::to_db(chain.peer_gain) - 6.0).abs() < 0.01);
        let mut block = [0.9; 480];
        chain.finish(&mut block, true, &settings);
        assert!(block.iter().all(|s| s.abs() <= 0.891_251));
        settings.auto_level = false;
        chain.finish(&mut [0.1; 480], true, &settings);
        assert_eq!(chain.applied_peer_gain, 1.0);
        chain.restart();
        assert_eq!(chain.peer_gain, 1.0);
        chain.tune(f32::NAN, 0.0);
        assert_eq!(chain.peer_gain, 1.0);
    }

    /// One corrupt capture block (a float driver's NaN or ±Inf) reaches the
    /// denoiser with the leveler off. It must not poison it: afterwards the
    /// probability is a number again and the voice is recognised.
    #[test]
    fn a_non_finite_block_does_not_poison_the_voice_detector() {
        let s = SendSettings {
            auto_level: false,
            ..SendSettings::default()
        };
        let voice = testsignal::voice(48_000 * 3, 7, 0.05);
        let mut chain = SendChain::new();
        let run = |chain: &mut SendChain, range: std::ops::Range<usize>| {
            let mut probs = Vec::new();
            for (b, loud) in range.clone().zip(&voice.loud[range]) {
                let mut block = voice.samples[b * 480..(b + 1) * 480].to_vec();
                let a = chain.analyze(&mut block, &s);
                chain.finish(&mut block, false, &s);
                assert!(block.iter().all(|x| x.is_finite()), "block {b}");
                if *loud {
                    probs.push(a.prob);
                }
            }
            probs
        };
        let blocks = voice.loud.len();
        run(&mut chain, 0..blocks / 3);
        let mut bad = vec![0.1f32; 480];
        bad[10] = f32::NAN;
        bad[20] = f32::INFINITY;
        bad[30] = f32::NEG_INFINITY;
        let a = chain.analyze(&mut bad, &s);
        assert!(a.prob.is_none_or(f32::is_finite));
        assert!(bad.iter().all(|x| x.is_finite()));
        let after = run(&mut chain, blocks / 3 + 10..blocks);
        let heard = after.iter().filter(|p| p.is_some_and(|p| p >= 0.5)).count();
        assert!(
            after.iter().all(|p| p.is_some_and(f32::is_finite)),
            "a probability that is no number"
        );
        assert!(
            heard * 2 > after.len(),
            "voice heard in {heard} of {} loud blocks",
            after.len()
        );
    }
}
