//! Voice activity decision: RNNoise probability above an absolute level
//! floor, or an energy gate while the denoiser is off.

use super::meter;

/// RMS level in dBFS a block must reach to count as voice while the
/// denoiser (and with it the RNNoise probability) is off.
pub const ENERGY_THRESHOLD_DB: f32 = -40.0;
/// RMS level in dBFS below which the RNNoise probability is not trusted.
/// RNNoise does not look at the level: on a digitally silent microphone the
/// AEC's comfort-noise transient when the far end starts (≈ −96 dBFS; right
/// after a reinitialization up to −63 dBFS) scored 0.25–0.6, above the auto
/// threshold, and one such block held the gate open for the whole hangover
/// (live test 2026-09-26: 55 packets of silence per far-end onset). Speech at
/// −40 dBFS without the leveler loses ~4 % of its voiced blocks here, all at
/// word edges the hangover covers.
const PROB_FLOOR_DB: f32 = -60.0;

/// Detector and threshold on the same 0–1 display axis. Without RNNoise,
/// the detector uses energy, mapped from −60 to 0 dBFS like the level meter.
pub fn meter_values(denoise: bool, probability: f32, threshold: f32, rms_db: f32) -> (f32, f32) {
    if denoise {
        (probability, threshold)
    } else {
        let fraction = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        (fraction(rms_db), fraction(ENERGY_THRESHOLD_DB))
    }
}

/// Decides for one 10 ms block. `prob` is the RNNoise probability if the
/// denoiser ran, compared against `threshold` on blocks at least
/// [`PROB_FLOOR_DB`] loud; without it the block's `level` decides.
pub fn is_voice(threshold: f32, prob: Option<f32>, level: meter::Level) -> bool {
    match prob {
        Some(p) => p >= threshold && level.rms_db >= PROB_FLOOR_DB,
        None => level.rms_db >= ENERGY_THRESHOLD_DB,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_meter_matches_the_actual_energy_gate() {
        assert_eq!(meter_values(true, 0.7, 0.4, -50.0), (0.7, 0.4));
        for rms_db in [-55.0, -40.0, -25.0] {
            let (value, threshold) = meter_values(false, 0.0, 0.9, rms_db);
            assert_eq!(
                value >= threshold,
                is_voice(
                    0.9,
                    None,
                    meter::Level {
                        rms_db,
                        peak_db: rms_db
                    }
                )
            );
        }
    }

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
        let soft = meter::Level {
            peak_db: -45.0,
            rms_db: -55.0,
        };
        assert!(is_voice(0.5, Some(0.9), soft));
        // Near-silence is not voice, whatever RNNoise says.
        assert!(!is_voice(0.5, Some(0.9), quiet));
        assert!(!is_voice(0.5, Some(0.1), loud));
        assert!(is_voice(0.5, None, loud));
        assert!(!is_voice(0.5, None, quiet));
    }
}
