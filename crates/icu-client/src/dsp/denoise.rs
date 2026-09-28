//! RNNoise through `nnnoiseless`. Works on 480-sample blocks scaled to
//! ±32768 (`docs/research/aec.md`: the APM uses ±1.0, RNNoise ±32768).

use nnnoiseless::DenoiseState;

/// Block size RNNoise requires.
pub const FRAME: usize = DenoiseState::FRAME_SIZE;

/// A denoiser instance.
pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    scratch_in: Vec<f32>,
    scratch_out: Vec<f32>,
    /// The first output block after creation is the filter warm-up; it is
    /// replaced by silence (`docs/research/audio-io.md` I4).
    warm: bool,
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl Denoiser {
    /// New instance.
    pub fn new() -> Denoiser {
        Denoiser {
            state: DenoiseState::new(),
            scratch_in: vec![0.0; FRAME],
            scratch_out: vec![0.0; FRAME],
            warm: false,
        }
    }

    /// Denoises `block` (±1.0, 480 samples) in place and returns the voice
    /// probability. Samples that are no number go in as silence
    /// ([`super::sanitized`]): RNNoise has no guard of its own, and a NaN
    /// reaching its inverse FFT is undefined behaviour.
    pub fn process(&mut self, block: &mut [f32]) -> f32 {
        debug_assert_eq!(block.len(), FRAME);
        for (d, s) in self.scratch_in.iter_mut().zip(block.iter()) {
            *d = super::sanitized(*s) * 32768.0;
        }
        let prob = self
            .state
            .process_frame(&mut self.scratch_out, &self.scratch_in);
        if !self.warm {
            self.warm = true;
            block.fill(0.0);
            return 0.0;
        }
        for (d, s) in block.iter_mut().zip(self.scratch_out.iter()) {
            *d = s / 32768.0;
        }
        prob
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scale test (`docs/research/aec.md` A8): a sine survives the ±1 ↔ ±32768 conversion.
    #[test]
    fn amplitude_survives_the_scaling() {
        let mut d = Denoiser::new();
        let mut max_out = 0.0f32;
        for k in 0..30 {
            let mut block: Vec<f32> = (0..FRAME)
                .map(|i| {
                    let n = (k * FRAME + i) as f32;
                    (n / 48_000.0 * 1000.0 * std::f32::consts::TAU).sin() * 0.5
                })
                .collect();
            d.process(&mut block);
            if k > 5 {
                max_out = max_out.max(block.iter().fold(0.0f32, |m, s| m.max(s.abs())));
            }
        }
        assert!(
            (0.2..=0.6).contains(&max_out),
            "peak after denoise {max_out}, expected around 0.5"
        );
    }
}
