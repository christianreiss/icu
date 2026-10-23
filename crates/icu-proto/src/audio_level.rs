//! Fixed-point audio measurements: centi-dBFS/centi-dB, per mille, milliseconds.
use serde::{Deserialize, Serialize};

/// A bounded speech-only observation over two seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioLevel {
    /// Speech envelope in centi-dBFS.
    pub level_cdb: i16,
    /// Suggested receive correction in centi-dB.
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
            && (1000..=2200).contains(&self.speech_ms)
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
    /// Monotonic observation identity per reporter and source.
    pub report_id: u32,
}
impl AudioWindow {
    /// Window spans one to 2.2 seconds on the sender clock.
    pub fn valid(self) -> bool {
        (48_000..=105_600).contains(&self.end_ts.wrapping_sub(self.first_ts))
    }
    /// Distinct, non-overlapping observation in the same generation.
    pub fn newer_than(self, previous: Self) -> bool {
        self.gain_epoch == previous.gain_epoch
            && self.report_id.wrapping_sub(previous.report_id) as i32 > 0
            && self.first_ts.wrapping_sub(previous.end_ts) as i32 >= 0
    }
}
