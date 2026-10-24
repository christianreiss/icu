//! Signal processing: AEC, denoise, VAD, gate, voice optimizer, codec, jitter
//! buffer, receive-side normalization, mixer.

pub mod aec;
pub mod chain;
pub mod denoise;
pub mod gate;
pub mod jitter;
pub mod meter;
pub mod mixer;
pub mod normalize;
pub mod opus;
pub mod preroll;
pub mod refalign;
pub mod testsignal;
pub mod vad;
pub mod vad_auto;
pub mod voice;
mod voicing;

/// Largest sample magnitude the send side passes on: +12 dBFS, far above
/// what a microphone delivers and far below where a filter overflows.
pub const SAMPLE_LIMIT: f32 = 4.0;

/// One capture sample made safe for the send side: no number becomes
/// silence, anything beyond ±[`SAMPLE_LIMIT`] is clamped.
pub fn sanitized(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-SAMPLE_LIMIT, SAMPLE_LIMIT)
    } else {
        0.0
    }
}

/// [`sanitized`] for a whole block, in place. A float capture path can
/// deliver NaN or ±Inf; one such sample left AEC3's and RNNoise's state no
/// number for the rest of the run — and in RNNoise's inverse FFT it is
/// undefined behaviour (`easyfft` unwraps the error it causes unchecked).
pub fn sanitize(block: &mut [f32]) {
    for s in block {
        *s = sanitized(*s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_that_are_no_number_become_silence() {
        let mut block = [
            0.5,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            -9.0,
            5.0,
            -0.25,
        ];
        sanitize(&mut block);
        assert_eq!(block, [0.5, 0.0, 0.0, 0.0, -4.0, 4.0, -0.25]);
    }
}

pub mod feedback;
