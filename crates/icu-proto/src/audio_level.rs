//! Fixed-point audio measurements: centi-dBFS/centi-dB, per mille, milliseconds.
use serde::{Deserialize, Serialize};

/// Shared policy for speech-only, pre-playback RMS feedback. A policy change
/// requires a protocol-minor change; a file-format version is not compatibility.
pub const POLICY_VERSION: u16 = 2;
/// Eligible speech RMS target in centi-dBFS.
pub const TARGET_CDB: i16 = -2200;
/// Single final acceptance half-width in centi-dB (3 dB total).
pub const ACCEPT_CDB: i16 = 150;
/// Smallest independent observation, weighted by its speech duration.
pub const MIN_SPEECH_MS: u16 = 100;
/// Bounded suitable speech duration per observation.
pub const MAX_SPEECH_MS: u16 = 2200;
/// Maximum observation span on the 48-kHz sender clock.
pub const MAX_WINDOW_SAMPLES: u32 = 105_600;
/// Maximum source-time age of feedback and local scalar evidence.
pub const MAX_AGE_SAMPLES: u32 = 288_000;

/// A bounded speech-only observation reported at most once every two seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioLevel {
    /// RMS of eligible speech samples in centi-dBFS, not LUFS.
    pub level_cdb: i16,
    /// Raw target error in centi-dB, bounded to ±12 dB; no dead band applied.
    pub correction_cdb: i16,
    /// Maximum speech sample in centi-dBFS.
    pub peak_cdb: i16,
    /// Fraction of speech samples at or above 0.99 amplitude.
    pub near_full_permille: u16,
    /// Eligible speech duration, milliseconds.
    pub speech_ms: u16,
}

impl AudioLevel {
    /// Bound all untrusted wire values before storing or using them.
    pub fn valid(self) -> bool {
        (-10000..=1200).contains(&self.level_cdb)
            && (-1200..=1200).contains(&self.correction_cdb)
            && (-10000..=2400).contains(&self.peak_cdb)
            && self.near_full_permille <= 1000
            && (MIN_SPEECH_MS..=MAX_SPEECH_MS).contains(&self.speech_ms)
    }
}

/// Identity and sender sample-clock bounds of one independent observation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioWindow {
    /// Applied sender gain generation.
    pub gain_epoch: u32,
    /// First sender sample in this window.
    pub first_ts: u32,
    /// Exclusive last sample, including intervening non-speech samples.
    pub end_ts: u32,
    /// Monotonic identity per reporter/source for reports; server/source for hints.
    pub report_id: u32,
}
impl AudioWindow {
    /// Window spans 100 ms to 2.2 seconds on the sender clock.
    pub fn valid(self) -> bool {
        (u32::from(MIN_SPEECH_MS) * 48..=MAX_WINDOW_SAMPLES)
            .contains(&self.end_ts.wrapping_sub(self.first_ts))
    }
    /// Server hints may cover overlapping independently measured windows. Their
    /// own sequence and advancing end, not reporter phase, prevent replay.
    pub fn newer_hint_than(self, previous: Self) -> bool {
        self.gain_epoch == previous.gain_epoch
            && self.report_id.wrapping_sub(previous.report_id) as i32 > 0
            && self.end_ts.wrapping_sub(previous.end_ts) as i32 > 0
    }
    /// Distinct, non-overlapping observation from the SAME reporter/generation.
    pub fn newer_than(self, previous: Self) -> bool {
        self.gain_epoch == previous.gain_epoch
            && self.report_id.wrapping_sub(previous.report_id) as i32 > 0
            && self.first_ts.wrapping_sub(previous.end_ts) as i32 >= 0
    }
}
