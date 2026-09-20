//! Mixer and soft limiter (`Client/IMPLEMENTATION.md` §6).

/// Knee above which the limiter starts compressing.
pub const KNEE: f32 = 0.8;

/// Soft limiter: linear below [`KNEE`], `tanh` above, never exceeds ±1.0.
pub fn limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = (a - KNEE) / (1.0 - KNEE);
        let y = (KNEE + (1.0 - KNEE) * over.tanh()).min(1.0);
        y.copysign(x)
    }
}

/// Sums `inputs` (each scaled by its gain) into `out` and limits the result.
/// `out` is overwritten.
pub fn mix(inputs: &[(&[f32], f32)], out: &mut [f32]) {
    out.fill(0.0);
    for (samples, gain) in inputs {
        if *gain == 0.0 {
            continue;
        }
        for (o, s) in out.iter_mut().zip(samples.iter()) {
            *o += s * gain;
        }
    }
    for o in out.iter_mut() {
        *o = limit(*o);
    }
}

/// Converts a per-user volume in percent (0–200) into a linear gain.
pub fn volume_gain(percent: f32) -> f32 {
    (percent / 100.0).clamp(0.0, 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_is_linear_below_knee_and_bounded_above() {
        for x in [-0.8f32, -0.5, 0.0, 0.3, 0.8] {
            assert_eq!(limit(x), x);
        }
        for x in [0.9f32, 1.0, 2.0, 10.0, 100.0] {
            let y = limit(x);
            assert!(y > KNEE && y <= 1.0, "{x} → {y}");
            assert_eq!(limit(-x), -y);
        }
        assert!(limit(1.0) < limit(1.5), "monotonic");
    }

    #[test]
    fn mix_sums_with_gain_and_mutes_zero_gain() {
        let a = [0.1f32, 0.2, 0.3];
        let b = [0.1f32, 0.1, 0.1];
        let mut out = [9.0f32; 3];
        mix(&[(&a, 1.0), (&b, 2.0)], &mut out);
        assert!((out[0] - 0.3).abs() < 1e-6);
        assert!((out[2] - 0.5).abs() < 1e-6);
        mix(&[(&a, 0.0), (&b, 0.0)], &mut out);
        assert_eq!(out, [0.0; 3]);
    }

    #[test]
    fn mix_never_clips() {
        let loud = [0.9f32; 480];
        let mut out = [0.0f32; 480];
        mix(&[(&loud, 2.0), (&loud, 2.0), (&loud, 2.0)], &mut out);
        assert!(out.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn volume_percent_to_gain() {
        assert_eq!(volume_gain(100.0), 1.0);
        assert_eq!(volume_gain(0.0), 0.0);
        assert_eq!(volume_gain(200.0), 2.0);
        assert_eq!(volume_gain(500.0), 2.0);
    }
}
