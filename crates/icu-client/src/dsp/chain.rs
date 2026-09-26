//! The send-side chain behind the AEC (`Client/IMPLEMENTATION.md` §6):
//! manual gain → mic auto level → denoise → meter → (the caller's VAD
//! verdict) → voice optimizer → outbound normalizer and limiter. One struct,
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
        }
    }

    /// Clears the adaptive state after the streams were reopened: the
    /// device may have changed, and a held gain for the old microphone
    /// would hit the new one.
    pub fn restart(&mut self) {
        self.leveler.reset();
        self.prev_voice = false;
        self.voice_chain.reset();
        self.out_norm.reset();
        self.out_limiter.reset();
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
            Some(self.denoiser.process(block))
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
        if s.normalize_outgoing && !self.limiter_on {
            self.out_limiter.reset();
        }
        self.limiter_on = s.normalize_outgoing;
        if s.normalize_outgoing {
            for x in block.iter_mut() {
                *x = self.out_limiter.process(*x);
            }
        }
    }

    /// Gains of the leveler and the outbound normalizer in dB (tests and
    /// diagnostics).
    pub fn gains_db(&self) -> (f32, f32) {
        (self.leveler.gain_db(), self.out_norm.gain_db())
    }
}
