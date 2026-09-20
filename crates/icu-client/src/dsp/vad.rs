//! Voice activity decision: RNNoise probability or an energy gate.

use super::meter;

/// How voice is detected.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VadSource {
    /// RNNoise voice probability (0–1) against a threshold.
    Rnnoise {
        /// Threshold, default 0.5.
        threshold: f32,
    },
    /// RMS energy in dBFS against a threshold.
    Energy {
        /// Threshold in dBFS, e.g. −40.
        threshold_db: f32,
    },
}

impl Default for VadSource {
    fn default() -> Self {
        VadSource::Rnnoise { threshold: 0.5 }
    }
}

/// Decides for one 10 ms block. `prob` is the RNNoise probability if the
/// denoiser ran, `level` the block's level.
pub fn is_voice(source: VadSource, prob: Option<f32>, level: meter::Level) -> bool {
    match (source, prob) {
        (VadSource::Rnnoise { threshold }, Some(p)) => p >= threshold,
        // Denoiser off: fall back to energy at a fixed −40 dBFS.
        (VadSource::Rnnoise { .. }, None) => level.rms_db >= -40.0,
        (VadSource::Energy { threshold_db }, _) => level.rms_db >= threshold_db,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions() {
        let loud = meter::Level {
            peak_db: -3.0,
            rms_db: -10.0,
        };
        let quiet = meter::Level {
            peak_db: -60.0,
            rms_db: -70.0,
        };
        assert!(is_voice(VadSource::default(), Some(0.9), quiet));
        assert!(!is_voice(VadSource::default(), Some(0.1), loud));
        assert!(is_voice(VadSource::default(), None, loud));
        assert!(!is_voice(VadSource::default(), None, quiet));
        assert!(is_voice(
            VadSource::Energy {
                threshold_db: -20.0
            },
            Some(0.0),
            loud
        ));
    }
}
