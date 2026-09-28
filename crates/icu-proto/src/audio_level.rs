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
