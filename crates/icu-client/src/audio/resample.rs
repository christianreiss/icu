//! Bounded streaming sample-rate conversion for the CPAL emergency path.
//! A 32-tap, 256-phase windowed-sinc filter keeps state across callbacks.
//! Coefficients are calculated once before the real-time stream starts.
const TAPS: usize = 32;
const PHASES: usize = 256;

pub(super) struct Resampler {
    input_rate: u32,
    output_rate: u32,
    history: [f32; TAPS],
    cursor: usize,
    input_position: u64,
    next_output: u64,
    coefficients: Box<[[f32; TAPS]; PHASES]>,
}
impl Resampler {
    pub(super) fn new(input_rate: u32, output_rate: u32) -> Self {
        let cutoff = (output_rate as f64 / input_rate as f64).min(1.0) * 0.94;
        let mut coefficients = Box::new([[0.0; TAPS]; PHASES]);
        for (phase, row) in coefficients.iter_mut().enumerate() {
            let fraction = phase as f64 / PHASES as f64;
            let mut sum = 0.0;
            for (age, value) in row.iter_mut().enumerate() {
                let x = age as f64 - fraction - 15.0;
                let sinc = if x.abs() < 1e-12 {
                    cutoff
                } else {
                    (std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                };
                let window = 0.5 + 0.5 * (std::f64::consts::PI * x / 16.0).cos();
                *value = (sinc * window) as f32;
                sum += *value;
            }
            for value in row {
                *value /= sum;
            }
        }
        Self {
            input_rate,
            output_rate,
            history: [0.0; TAPS],
            cursor: 0,
            input_position: 0,
            next_output: 0,
            coefficients,
        }
    }
    pub(super) fn reset_history(&mut self) {
        self.history.fill(0.0);
        self.cursor = 0;
        self.input_position = 0;
        self.next_output = 0;
    }
    pub(super) fn push(&mut self, sample: f32, mut output: impl FnMut(f32)) {
        let sample = if sample.is_finite() { sample } else { 0.0 };
        if self.input_rate == self.output_rate {
            output(sample);
            return;
        }
        self.history[self.cursor] = sample;
        let now = self.input_position * u64::from(self.output_rate);
        while self.next_output <= now {
            let phase =
                ((now - self.next_output) * PHASES as u64 / u64::from(self.output_rate)) as usize;
            let row = &self.coefficients[phase.min(PHASES - 1)];
            let value = row
                .iter()
                .enumerate()
                .map(|(age, weight)| self.history[(self.cursor + TAPS - age) % TAPS] * weight)
                .sum();
            output(value);
            self.next_output += u64::from(self.input_rate);
        }
        self.cursor = (self.cursor + 1) % TAPS;
        self.input_position += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn microphone_channel_change_discards_the_previous_input_history() {
        let mut converter = Resampler::new(44_100, 48_000);
        for _ in 0..480 {
            converter.push(0.8, |_| {});
        }
        converter.reset_history();
        let mut samples = Vec::new();
        for _ in 0..480 {
            converter.push(0.0, |sample| samples.push(sample));
        }
        assert!(samples.len() > 480);
        assert!(samples.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn rate_and_history_are_independent_of_device_callback_size() {
        for (input, output) in [
            (44_100, 48_000),
            (48_000, 44_100),
            (96_000, 48_000),
            (48_000, 192_000),
        ] {
            let wave: Vec<f32> = (0..input)
                .map(|i| (std::f32::consts::TAU * 1_000.0 * i as f32 / input as f32).sin() * 0.2)
                .collect();
            let run = |chunk| {
                let mut converter = Resampler::new(input, output);
                let mut samples = Vec::new();
                for block in wave.chunks(chunk) {
                    for sample in block {
                        converter.push(*sample, |x| samples.push(x));
                    }
                }
                samples
            };
            let one = run(1);
            let many = run(8192);
            assert_eq!(one, many);
            assert!((one.len() as i64 - i64::from(output)).abs() <= 4);
            let rms =
                (one[100..].iter().map(|x| x * x).sum::<f32>() / (one.len() - 100) as f32).sqrt();
            assert!(
                (rms - 0.2 / 2f32.sqrt()).abs() < 0.002,
                "{input}->{output}: {rms}"
            );
        }
    }
    #[test]
    fn downsampling_rejects_aliases() {
        let mut converter = Resampler::new(96_000, 48_000);
        let mut out = Vec::new();
        for i in 0..96_000 {
            converter.push(
                (std::f32::consts::TAU * 35_000.0 * i as f32 / 96_000.0).sin(),
                |x| out.push(x),
            );
        }
        let rms = (out[100..].iter().map(|x| x * x).sum::<f32>() / (out.len() - 100) as f32).sqrt();
        assert!(rms < 0.01, "out-of-band alias RMS {rms}");
    }
}
