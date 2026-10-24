//! Mixer and soft limiter (`docs/reference/client.md` §6).

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
pub fn mix<'a>(inputs: impl IntoIterator<Item = (&'a [f32], f32)>, out: &mut [f32]) {
    out.fill(0.0);
    for (samples, gain) in inputs {
        if gain == 0.0 {
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

/// Smooth user/master volume changes over one mixer block, independently
/// for each speaker. Zero gain is a hard mute and never releases a tail.
#[derive(Default)]
pub struct GainRamp {
    gain: Option<f32>,
}

impl GainRamp {
    /// Applies a linear gain (up to 2× user volume and 2× master volume).
    /// The initial block starts at its configured level; later changes
    /// ramp over the 10-ms block instead of stepping at its first sample.
    pub fn apply(&mut self, block: &mut [f32], target: f32) {
        if block.is_empty() {
            return;
        }
        let target = if target.is_finite() {
            target.clamp(0.0, 4.0)
        } else {
            0.0
        };
        if target == 0.0 {
            block.fill(0.0);
        } else {
            let start = self.gain.unwrap_or(target);
            let step = (target - start) / block.len() as f32;
            for (i, sample) in block.iter_mut().enumerate() {
                *sample *= start + step * (i + 1) as f32;
            }
        }
        self.gain = Some(target);
    }
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
        mix([(&a[..], 1.0), (&b[..], 2.0)], &mut out);
        assert!((out[0] - 0.3).abs() < 1e-6);
        assert!((out[2] - 0.5).abs() < 1e-6);
        mix([(&a[..], 0.0), (&b[..], 0.0)], &mut out);
        assert_eq!(out, [0.0; 3]);
    }

    #[test]
    fn mix_never_clips() {
        let loud = [0.9f32; 480];
        let mut out = [0.0f32; 480];
        mix(
            [(&loud[..], 2.0), (&loud[..], 2.0), (&loud[..], 2.0)],
            &mut out,
        );
        assert!(out.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn volume_percent_to_gain() {
        assert_eq!(volume_gain(100.0), 1.0);
        assert_eq!(volume_gain(0.0), 0.0);
        assert_eq!(volume_gain(200.0), 2.0);
        assert_eq!(volume_gain(500.0), 2.0);
    }

    #[test]
    fn volume_changes_ramp_without_a_block_edge_step() {
        let mut gain = GainRamp::default();
        let mut block = [0.25; 480];
        gain.apply(&mut block, 1.0);
        assert_eq!(block, [0.25; 480]);
        for target in [2.0, 0.5, 4.0, 1.0] {
            let previous = *block.last().unwrap();
            block.fill(0.25);
            gain.apply(&mut block, target);
            let expected_step = (0.25 * target - previous) / 480.0;
            assert!((block[0] - previous - expected_step).abs() < 1e-6);
            assert!((block[479] - 0.25 * target).abs() < 1e-6);
            assert!(block
                .windows(2)
                .all(|s| (s[1] - s[0] - expected_step).abs() < 1e-6));
        }
    }

    #[test]
    fn mute_is_immediate_and_unmute_ramps_from_silence() {
        let mut gain = GainRamp::default();
        let mut block = [0.25; 480];
        gain.apply(&mut block, 2.0);
        gain.apply(&mut block, 0.0);
        assert_eq!(block, [0.0; 480]);
        block.fill(0.25);
        gain.apply(&mut block, 1.0);
        assert!((block[0] - 0.25 / 480.0).abs() < 1e-6);
        assert_eq!(block[479], 0.25);
    }

    #[test]
    fn volume_ramps_keep_independent_state_and_reject_invalid_targets() {
        let mut alice = GainRamp::default();
        let mut bob = GainRamp::default();
        alice.apply(&mut [0.1; 480], 2.0);
        bob.apply(&mut [0.1; 480], 0.5);
        alice.apply(&mut [], 4.0);
        let mut block = [0.1; 480];
        bob.apply(&mut block, 1.0);
        assert!((block[0] - 0.05).abs() < 0.001);
        block.fill(0.1);
        alice.apply(&mut block, 1.0);
        assert!((block[0] - 0.2).abs() < 0.001);
        for target in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0] {
            block.fill(0.1);
            alice.apply(&mut block, target);
            assert_eq!(block, [0.0; 480]);
        }
        let mut fresh = GainRamp::default();
        block.fill(0.1);
        fresh.apply(&mut block, 100.0);
        assert_eq!(block, [0.4; 480]);
    }
}
