//! Conservative, level-independent evidence for receive-side voiced speech.
//!
//! This supplements the normalizer's energy-floor detector; it is not a
//! transmission VAD. A periodic signal alone is insufficient: mains hum,
//! fixed tones and fans must not establish a gain increase. A coherent pitch
//! that moves over several frames can confirm vowels whose compressed RMS
//! never rises above their own minimum. Unvoiced speech retains the existing
//! energy-floor path. No samples are modified and no heap work occurs.

const DECIMATE: usize = 6;
const HISTORY: usize = 320; // 40 ms at 8 kHz; >200 pairs at the lowest pitch.
const STEP: usize = 160; // Analyze every 20 ms, independent of input chunks.
const MIN_LAG: usize = 18; // 444 Hz.
const MAX_LAG: usize = 114; // 70 Hz.
const TRACK: usize = 25; // Last half second of pitch observations.

#[derive(Debug, Clone)]
pub(super) struct Voicing {
    history: [f32; HISTORY],
    input_energy: [f32; HISTORY],
    low_pass: [f32; 4],
    cursor: usize,
    filled: usize,
    sum: f32,
    energy: f32,
    count: usize,
    step: usize,
    pitches: [f32; TRACK],
    pitch_cursor: usize,
    voiced: bool,
}

impl Default for Voicing {
    fn default() -> Self {
        Self {
            history: [0.0; HISTORY],
            input_energy: [0.0; HISTORY],
            low_pass: [0.0; 4],
            cursor: 0,
            filled: 0,
            sum: 0.0,
            energy: 0.0,
            count: 0,
            step: 0,
            pitches: [0.0; TRACK],
            pitch_cursor: 0,
            voiced: false,
        }
    }
}

impl Voicing {
    pub(super) fn observe(&mut self, samples: &[f32]) -> bool {
        for &s in samples {
            self.energy += s * s;
            // Low-pass before decimation; the later energy comparison also
            // prevents weak aliases of swept ultrasonic tones becoming voice.
            let mut filtered = s;
            for pole in &mut self.low_pass {
                *pole += 0.145 * (filtered - *pole);
                filtered = *pole;
            }
            self.sum += filtered;
            self.count += 1;
            if self.count != DECIMATE {
                continue;
            }
            self.history[self.cursor] = self.sum / DECIMATE as f32;
            self.input_energy[self.cursor] = self.energy / DECIMATE as f32;
            self.cursor = (self.cursor + 1) % HISTORY;
            self.filled = (self.filled + 1).min(HISTORY);
            self.sum = 0.0;
            self.energy = 0.0;
            self.count = 0;
            self.step += 1;
            if self.step == STEP {
                self.step = 0;
                self.analyze();
            }
        }
        self.voiced
    }

    fn analyze(&mut self) {
        let pitch = if self.filled == HISTORY {
            self.pitch()
        } else {
            0.0
        };
        self.track(pitch);
    }

    fn track(&mut self, pitch: f32) {
        let previous = self.pitches[(self.pitch_cursor + TRACK - 1) % TRACK];
        if pitch == 0.0 || (previous > 0.0 && (pitch / previous - 1.0).abs() > 0.1) {
            // Missing evidence and pitch jumps start a new run, rather than
            // joining stationary tones on either side into apparent movement.
            self.pitches.fill(0.0);
            self.pitch_cursor = 0;
        }
        self.pitches[self.pitch_cursor] = pitch;
        self.pitch_cursor = (self.pitch_cursor + 1) % TRACK;
        let mut count = 0;
        let mut lo = f32::INFINITY;
        let mut hi = 0.0f32;
        for p in self.pitches.into_iter().filter(|p| *p > 0.0) {
            count += 1;
            lo = lo.min(p);
            hi = hi.max(p);
        }
        // Integer-lag jitter of a fixed high tone is at most one sample
        // (~5.6% at MIN_LAG). Require real movement, but reject octave jumps
        // and unrelated periodic sources as corroboration.
        let movement = (1..count)
            .filter(|&i| {
                let a = self.pitches[(self.pitch_cursor + TRACK - i) % TRACK];
                let b = self.pitches[(self.pitch_cursor + TRACK - i - 1) % TRACK];
                b > 0.0 && (a / b - 1.0).abs() >= 0.002
            })
            .count();
        self.voiced =
            pitch > 0.0 && count >= 5 && movement >= 4 && hi >= lo * 1.06 && hi < lo * 1.35;
    }

    fn pitch(&self) -> f32 {
        let mut x = [0.0; HISTORY];
        let mean = self.history.iter().sum::<f32>() / HISTORY as f32;
        for (i, s) in x.iter_mut().enumerate() {
            *s = self.history[(self.cursor + i) % HISTORY] - mean;
        }
        let energy = x.iter().map(|s| s * s).sum::<f32>();
        if energy < 0.01 * self.input_energy.iter().sum::<f32>() {
            return 0.0;
        }
        let mut corr = [0.0; MAX_LAG + 2];
        for (lag, value) in corr.iter_mut().enumerate().skip(MIN_LAG - 1) {
            let (mut xy, mut xx, mut yy) = (0.0f32, 0.0f32, 0.0f32);
            for (&a, &b) in x[lag..].iter().zip(&x[..HISTORY - lag]) {
                xy += a * b;
                xx += a * a;
                yy += b * b;
            }
            if xx > 1e-12 && yy > 1e-12 {
                *value = xy / (xx * yy).sqrt();
            }
        }
        // A local maximum avoids mistaking the high short-lag correlation
        // of a slow hum or DC drift for a voice-range fundamental.
        let peak = |lag: usize| corr[lag] > corr[lag - 1] && corr[lag] >= corr[lag + 1];
        let best = (MIN_LAG..=MAX_LAG)
            .filter(|&lag| peak(lag))
            .map(|lag| corr[lag])
            .fold(0.0f32, f32::max);
        let Some(lag) =
            (MIN_LAG..=MAX_LAG).find(|&lag| peak(lag) && corr[lag] >= 0.75f32.max(best * 0.9))
        else {
            return 0.0;
        };
        // Interpolation prevents fixed-tone integer-bin jitter from looking
        // like a moving fundamental, especially at the top of the range.
        let curve = corr[lag - 1] - 2.0 * corr[lag] + corr[lag + 1];
        let offset = if curve < -1e-6 {
            ((corr[lag - 1] - corr[lag + 1]) / (2.0 * curve)).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        lag as f32 + offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BLOCK: usize = 480;

    fn voice(phase: &mut f32, sample: usize, pitch: f32, contour: f32) -> f32 {
        let t = sample as f32 / 48_000.0;
        let hz = pitch * (1.0 + 0.11 * (t * std::f32::consts::TAU * contour).sin());
        *phase = (*phase + hz / 48_000.0).fract();
        let p = *phase * std::f32::consts::TAU;
        p.sin() + 0.4 * (2.0 * p).sin() + 0.2 * (3.0 * p).sin()
    }

    #[test]
    fn voiced_contours_work_at_low_middle_and_high_pitches() {
        for pitch in [80.0, 180.0, 350.0] {
            let mut detector = Voicing::default();
            let mut phase = 0.0;
            let mut voiced = 0;
            for b in 0..1000 {
                let samples: Vec<f32> = (0..BLOCK)
                    .map(|i| voice(&mut phase, b * BLOCK + i, pitch, 1.3) * 0.05)
                    .collect();
                voiced += usize::from(detector.observe(&samples));
            }
            assert!(voiced > 700, "{pitch} Hz: {voiced} voiced blocks");
        }
    }

    #[test]
    fn stationary_hiss_hum_tones_and_slow_fan_do_not_confirm_voice() {
        for kind in 0..15 {
            let mut detector = Voicing::default();
            let mut rng = 7u32;
            let mut low = 0.0;
            let mut phase = 0.0;
            let mut voiced = 0;
            for b in 0..1000 {
                let samples: Vec<f32> = (0..BLOCK)
                    .map(|i| {
                        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                        let noise = rng as f32 / u32::MAX as f32 * 2.0 - 1.0;
                        low = 0.98 * low + 0.02 * noise;
                        let n = b * BLOCK + i;
                        let t = n as f32 / 48_000.0;
                        match kind {
                            0 => noise * 0.05,
                            1 => low,
                            2 => (t * std::f32::consts::TAU * 50.0).sin() * 0.05,
                            3 => (t * std::f32::consts::TAU * 60.0).sin() * 0.05,
                            4 => voice(&mut phase, n, 100.0, 0.0) * 0.05,
                            5 => voice(&mut phase, n, 120.0, 0.0) * 0.05,
                            6 => voice(&mut phase, n, 220.0, 0.0) * 0.05,
                            7 => voice(&mut phase, n, 440.0, 0.0) * 0.05,
                            8 => voice(&mut phase, n, 160.0, 0.05) * 0.05 + noise * 0.001,
                            9 => 0.1 + low * 0.01,
                            10 => {
                                voice(&mut phase, n, if b < 500 { 180.0 } else { 160.0 }, 0.0)
                                    * 0.05
                            }
                            11 => {
                                voice(&mut phase, n, if b < 500 { 180.0 } else { 195.0 }, 0.0)
                                    * 0.05
                            }
                            12 => {
                                if (400..600).contains(&b) {
                                    0.0
                                } else {
                                    voice(&mut phase, n, if b < 500 { 180.0 } else { 200.0 }, 0.0)
                                        * 0.05
                                }
                            }
                            13 | 14 => {
                                let hz = if kind == 13 { 8000.0 } else { 16000.0 }
                                    + 180.0
                                    + 20.0 * (t * std::f32::consts::TAU * 1.3).sin();
                                phase = (phase + hz / 48_000.0).fract();
                                (phase * std::f32::consts::TAU).sin() * 0.05
                            }
                            _ => unreachable!(),
                        }
                    })
                    .collect();
                voiced += usize::from(detector.observe(&samples));
            }
            assert_eq!(voiced, 0, "noise kind {kind} established voice");
        }
    }

    #[test]
    fn a_gap_or_rejected_jump_cannot_join_stationary_pitch_runs() {
        for gap in [0.0, 45.0] {
            let mut detector = Voicing::default();
            for pitch in [40.0; 5].into_iter().chain([gap]).chain([45.0; 6]) {
                detector.track(pitch);
                assert!(
                    !detector.voiced,
                    "separate constant pitches are not a contour"
                );
            }
        }
    }

    #[test]
    fn analysis_clock_is_independent_of_callback_fragmentation() {
        let mut whole = Voicing::default();
        let mut fragments = Voicing::default();
        let mut phase = 0.0;
        for b in 0..200 {
            let samples: Vec<f32> = (0..960)
                .map(|i| voice(&mut phase, b * 960 + i, 180.0, 1.3) * 0.05)
                .collect();
            let expected = whole.observe(&samples);
            let mut got = false;
            for fragment in samples.chunks(137) {
                got = fragments.observe(fragment);
            }
            assert_eq!(got, expected);
            assert_eq!(whole.pitches, fragments.pitches);
        }
    }

    /// Release-only timing evidence for the maximum room: each receiver has
    /// a normalization and feedback estimator. No hardware-specific limit.
    #[test]
    #[ignore = "run optimized to record the 64-speaker processing budget"]
    fn sixty_four_speaker_cpu_budget() {
        let mut detectors = vec![Voicing::default(); 128];
        let mut phase = 0.0;
        let start = std::time::Instant::now();
        let mut total = 0;
        for b in 0..100 {
            let samples: Vec<f32> = (0..BLOCK)
                .map(|i| voice(&mut phase, b * BLOCK + i, 180.0, 1.3) * 0.05)
                .collect();
            for detector in &mut detectors {
                total += usize::from(std::hint::black_box(detector).observe(&samples));
            }
        }
        eprintln!(
            "64-speaker normalization+feedback voicing / 1 s: {:?}; {total} detections",
            start.elapsed()
        );
    }
}
