//! Peak/RMS metering for the UI.

/// Level of one block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    /// Peak in dBFS.
    pub peak_db: f32,
    /// RMS in dBFS.
    pub rms_db: f32,
}

impl Default for Level {
    fn default() -> Self {
        Level {
            peak_db: -100.0,
            rms_db: -100.0,
        }
    }
}

/// Measures a block.
pub fn measure(block: &[f32]) -> Level {
    if block.is_empty() {
        return Level::default();
    }
    let mut peak = 0.0f32;
    let mut energy = 0.0f64;
    for s in block {
        peak = peak.max(s.abs());
        energy += (*s as f64) * (*s as f64);
    }
    let rms = (energy / block.len() as f64).sqrt() as f32;
    Level {
        peak_db: to_db(peak),
        rms_db: to_db(rms),
    }
}

/// Linear amplitude to dBFS, floored at −100 dB.
pub fn to_db(x: f32) -> f32 {
    if x <= 1e-5 {
        -100.0
    } else {
        20.0 * x.log10()
    }
}

/// dB to linear gain.
pub fn from_db(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_sine_is_minus_3_db_rms() {
        let block: Vec<f32> = (0..480)
            .map(|i| (i as f32 / 480.0 * 20.0 * std::f32::consts::TAU).sin())
            .collect();
        let l = measure(&block);
        assert!((l.peak_db - 0.0).abs() < 0.1);
        assert!((l.rms_db + 3.01).abs() < 0.1);
        assert_eq!(measure(&[0.0; 10]).peak_db, -100.0);
        assert!((from_db(-6.02) - 0.5).abs() < 0.01);
    }
}
