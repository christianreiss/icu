//! Voice activity decision: RNNoise probability, or an energy gate while the
//! denoiser is off.

use super::meter;

/// RMS level in dBFS a block must reach to count as voice while the
/// denoiser (and with it the RNNoise probability) is off.
const ENERGY_THRESHOLD_DB: f32 = -40.0;

/// Decides for one 10 ms block. `prob` is the RNNoise probability if the
/// denoiser ran, compared against `threshold`; without it the block's
/// `level` decides.
pub fn is_voice(threshold: f32, prob: Option<f32>, level: meter::Level) -> bool {
    match prob {
        Some(p) => p >= threshold,
        None => level.rms_db >= ENERGY_THRESHOLD_DB,
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
        assert!(is_voice(0.5, Some(0.9), quiet));
        assert!(!is_voice(0.5, Some(0.1), loud));
        assert!(is_voice(0.5, None, loud));
        assert!(!is_voice(0.5, None, quiet));
    }
}
