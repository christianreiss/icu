//! Deterministic synthetic signals and metrics (lifted from
//! `experiments/spike-aec/src/signal.rs`). Used by the AEC tests, the
//! integration tests and `--test-tone`.

/// Sample rate.
pub const SR: usize = 48_000;

/// xorshift64* PRNG, no crate needed.
pub struct Rng(u64);

impl Rng {
    /// Seeded generator.
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    /// Next raw value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [-1, 1).
    pub fn uniform(&mut self) -> f32 {
        // 53 random bits over 2^53 → [0, 1). (Until 2026-09-25 this divided
        // by 2^52 and returned [-1, 3): every noise signal carried a DC
        // offset of 1.0 and every `(uniform() + 1) / 2` probability ran at
        // half its nominal rate.)
        ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) as f32
    }
}

/// Speech-like signal: coloured noise with two formant resonators and a
/// syllable envelope with pauses.
pub fn speechlike(n: usize, seed: u64, syllable_hz: f32, phase: f32, rms_target: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut out = vec![0.0f32; n];
    let mut lp = 0.0f32;
    let mut r1 = [0.0f32; 2];
    let mut r2 = [0.0f32; 2];
    let mk = |f: f32, q: f32| -> (f32, f32) {
        let w = 2.0 * std::f32::consts::PI * f / SR as f32;
        let r = (-w / (2.0 * q)).exp();
        (2.0 * r * w.cos(), -r * r)
    };
    let (a1a, a1b) = mk(700.0, 6.0);
    let (a2a, a2b) = mk(1800.0, 8.0);
    for (i, s) in out.iter_mut().enumerate() {
        let w = rng.uniform();
        lp = 0.85 * lp + 0.15 * w;
        let f1 = lp + a1a * r1[0] + a1b * r1[1];
        r1[1] = r1[0];
        r1[0] = f1;
        let f2 = lp + a2a * r2[0] + a2b * r2[1];
        r2[1] = r2[0];
        r2[0] = f2;
        let t = i as f32 / SR as f32;
        let m = (2.0 * std::f32::consts::PI * syllable_hz * t + phase).sin();
        let env = if m > 0.05 { m.powf(0.6) } else { 0.0 };
        let sentence = (2.0 * std::f32::consts::PI * 0.4 * t + phase * 0.7).sin();
        let gate = if sentence > -0.75 { 1.0 } else { 0.0 };
        *s = (0.55 * f1 + 0.35 * f2 + 0.3 * lp) * env * gate;
    }
    let r = rms(&out);
    if r > 0.0 {
        let g = rms_target / r;
        for s in out.iter_mut() {
            *s *= g;
        }
    }
    out
}

/// Room impulse response: direct sound after `delay_ms` plus decaying taps.
pub fn room_ir(delay_ms: f32, direct_gain: f32, rt_ms: f32, seed: u64) -> Vec<f32> {
    let d = (delay_ms * SR as f32 / 1000.0).round() as usize;
    let tail = (rt_ms * SR as f32 / 1000.0).round() as usize;
    let mut ir = vec![0.0f32; d + tail + 1];
    ir[d] = direct_gain;
    let mut rng = Rng::new(seed);
    for k in 0..60 {
        let frac = (k as f32 + 1.0) / 60.0;
        let pos = d + ((frac.powf(1.3) * tail as f32) as usize).min(tail.saturating_sub(1));
        let decay = (-6.0 * frac).exp();
        ir[pos] += direct_gain * 0.6 * decay * rng.uniform();
    }
    ir
}

/// Direct FIR convolution.
pub fn convolve(x: &[f32], ir: &[f32]) -> Vec<f32> {
    let mut y = vec![0.0f32; x.len()];
    for (k, &h) in ir.iter().enumerate() {
        if h == 0.0 {
            continue;
        }
        for i in k..x.len() {
            y[i] += h * x[i - k];
        }
    }
    y
}

/// RMS.
pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

/// Energy.
pub fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| (*v as f64) * (*v as f64)).sum()
}

/// ERLE in dB.
pub fn erle_db(mic: &[f32], out: &[f32]) -> f64 {
    let em = energy(mic);
    let eo = energy(out);
    if eo <= 0.0 || em <= 0.0 {
        return f64::NAN;
    }
    10.0 * (em / eo).log10()
}

/// Pearson correlation.
pub fn pearson(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f64::NAN;
    }
    let ma = a[..n].iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    let mb = b[..n].iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let x = a[i] as f64 - ma;
        let y = b[i] as f64 - mb;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 0.0 || db <= 0.0 {
        return f64::NAN;
    }
    num / (da * db).sqrt()
}

/// Lag (in samples) at which `y` best matches `x`, searched over
/// `0..=max_lag` with the given step.
pub fn best_lag(x: &[f32], y: &[f32], max_lag: usize, step: usize) -> usize {
    let mut best = 0usize;
    let mut best_v = f64::NEG_INFINITY;
    let mut lag = 0;
    while lag <= max_lag {
        let n = x.len().min(y.len().saturating_sub(lag));
        let mut s = 0.0f64;
        for i in 0..n {
            s += x[i] as f64 * y[i + lag] as f64;
        }
        if s > best_v {
            best_v = s;
            best = lag;
        }
        lag += step;
    }
    best
}

/// How cleanly a received copy of a looped source plays: the received
/// signal is followed window by window at a running alignment; a window
/// that no longer matches there is a glitch (gap, concealment, skipped or
/// inserted frame).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Continuity {
    /// Windows judged (the source is voiced there).
    pub windows: usize,
    /// Windows that did not match the source at the running alignment.
    pub glitches: usize,
    /// Of those, windows nearly silent where the source is not (dropouts).
    pub dropouts: usize,
    /// Times the alignment moved: frames were skipped or inserted.
    pub realigned: usize,
    /// Lowest correlation of a judged window at the running alignment.
    pub worst: f64,
}

/// Compares `received` against the loop `source` in windows of `window`
/// samples. `received` must start after the stream has settled.
pub fn continuity(received: &[f32], source: &[f32], window: usize) -> Continuity {
    const MATCH: f64 = 0.95;
    let len = source.len();
    let looped: Vec<f32> = source.iter().chain(source.iter()).copied().collect();
    let at = |pos: usize, n: usize| -> &[f32] { &looped[pos % len..pos % len + n] };
    let voiced_rms = {
        let active: Vec<f32> = source.iter().copied().filter(|v| v.abs() > 1e-4).collect();
        rms(&active)
    };
    let mut out = Continuity::default();
    let anchor = SR / 10;
    if received.len() < SR / 2 + anchor || len < anchor {
        return out;
    }
    // Anchor on the loudest 100 ms of the first half second, searched over
    // the whole loop.
    let start = (0..SR / 2)
        .step_by(SR / 100)
        .max_by(|&a, &b| rms(&received[a..a + anchor]).total_cmp(&rms(&received[b..b + anchor])))
        .unwrap_or(0);
    let seg = &received[start..start + anchor];
    let coarse = best_lag(seg, &looped, len - 1, 4);
    let lo = coarse.saturating_sub(4);
    let found = lo + best_lag(seg, &looped[lo..], 8, 1);
    let mut lag = (found + len - start % len) % len;
    let search = 3 * window;
    let mut i = 0;
    while (i + 1) * window <= received.len() {
        let got = &received[i * window..(i + 1) * window];
        let pos = lag + i * window;
        let want = at(pos, window);
        i += 1;
        if rms(want) < 0.5 * voiced_rms {
            continue;
        }
        out.windows += 1;
        let r = pearson(got, want);
        out.worst = if out.windows == 1 {
            r
        } else {
            out.worst.min(r)
        };
        if r >= MATCH {
            continue;
        }
        out.glitches += 1;
        if rms(got) < 0.1 * rms(want) {
            out.dropouts += 1;
        }
        // Look for where the stream went: ± three windows.
        let base = (pos + len - search) % len;
        let span = &looped[base..base + 2 * search + window];
        let coarse = best_lag(got, span, 2 * search, 4);
        let lo = coarse.saturating_sub(4);
        let fine = lo + best_lag(got, &span[lo..], 8.min(2 * search - lo), 1);
        if pearson(got, &span[fine..fine + window]) >= MATCH && fine != search {
            lag = (lag + len + fine - search) % len;
            out.realigned += 1;
        }
    }
    out
}

/// Harmonic voice-like tone: a 180 Hz fundamental with decaying harmonics and
/// a syllable envelope. Opus preserves it waveform-exactly enough for
/// correlation tests, unlike noise.
pub fn harmonic_voice(n: usize, rms_target: f32) -> Vec<f32> {
    let mut out = vec![0.0f32; n];
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let f0 = 180.0 + 20.0 * (2.0 * std::f32::consts::PI * 0.7 * t).sin();
        let mut v = 0.0;
        for h in 1..=8 {
            let a = 1.0 / h as f32;
            v += a * (2.0 * std::f32::consts::PI * f0 * h as f32 * t).sin();
        }
        let m = (2.0 * std::f32::consts::PI * 2.5 * t).sin();
        let env = if m > 0.0 { m.powf(0.5) } else { 0.0 };
        *s = v * env;
    }
    let r = rms(&out);
    if r > 0.0 {
        let g = rms_target / r;
        for s in out.iter_mut() {
            *s *= g;
        }
    }
    out
}

/// A synthetic voice, as far as RNNoise is concerned: its voice probability
/// is ≥ 0.5 on more than 98 % of the loud vowel blocks, with at most a few
/// misses in a row, and it attenuates the voice by under 1 dB — like real
/// speech, unlike [`harmonic_voice`], which it drops for whole seconds.
/// Glottal pulses (f0 around 120 Hz, a contour per syllable, 1 % jitter)
/// through three gliding formant resonators; syllables of 160–280 ms with
/// 20/40 ms attack/release, 50–130 ms gaps, and on most syllables a 30–60 ms
/// fricative burst. No sentence pauses: a test imposes its own.
#[derive(Debug, Clone)]
pub struct Voice {
    /// Samples, RMS `rms_target` over the whole signal.
    pub samples: Vec<f32>,
    /// Per 480-sample block: the block lies in the loud part of a vowel (its
    /// RMS within 6 dB of the 90th percentile of all blocks).
    pub loud: Vec<bool>,
}

/// Two-pole resonator (a formant).
struct Resonator {
    a1: f32,
    a2: f32,
    gain: f32,
    y1: f32,
    y2: f32,
}

impl Resonator {
    fn new(hz: f32, bandwidth_hz: f32) -> Resonator {
        let mut r = Resonator {
            a1: 0.0,
            a2: 0.0,
            gain: 0.0,
            y1: 0.0,
            y2: 0.0,
        };
        r.tune(hz, bandwidth_hz);
        r
    }

    fn tune(&mut self, hz: f32, bandwidth_hz: f32) {
        let r = (-std::f32::consts::PI * bandwidth_hz / SR as f32).exp();
        let w = 2.0 * std::f32::consts::PI * hz / SR as f32;
        self.a1 = 2.0 * r * w.cos();
        self.a2 = -r * r;
        self.gain = (1.0 - r) * (1.0 - 2.0 * r * w.cos() + r * r).sqrt();
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.gain * x + self.a1 * self.y1 + self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// See [`Voice`].
pub fn voice(n: usize, seed: u64, rms_target: f32) -> Voice {
    // (F1, F2, F3) of a, i, u, e, o, æ.
    const VOWELS: [(f32, f32, f32); 6] = [
        (730.0, 1090.0, 2440.0),
        (270.0, 2290.0, 3010.0),
        (300.0, 870.0, 2240.0),
        (530.0, 1840.0, 2480.0),
        (570.0, 840.0, 2410.0),
        (660.0, 1720.0, 2410.0),
    ];
    const BLOCK: usize = 480;
    let sr = SR as f32;
    let mut rng = Rng::new(seed);
    let mut u01 = move || (rng.uniform() + 1.0) / 2.0;
    let mut noise = Rng::new(seed ^ 0x5EED);
    let mut samples = vec![0.0f32; n];
    let mut f1 = Resonator::new(500.0, 80.0);
    let mut f2 = Resonator::new(1500.0, 100.0);
    let mut f3 = Resonator::new(2500.0, 140.0);
    let mut fricative = Resonator::new(3500.0, 1500.0);
    let mut glottis = 0.0f32;
    let mut phase = 0.0f32;
    let mut f0_base = 120.0f32;
    let mut vowel = VOWELS[0];
    let mut i = 0usize;
    while i < n {
        let next = VOWELS[(u01() * VOWELS.len() as f32) as usize % VOWELS.len()];
        let vowel_len = (sr * (0.16 + 0.12 * u01())) as usize;
        let gap_len = (sr * (0.05 + 0.08 * u01())) as usize;
        let burst_len = if u01() < 0.6 {
            (sr * (0.03 + 0.03 * u01())) as usize
        } else {
            0
        };
        let f0_start = f0_base * (0.9 + 0.2 * u01());
        let f0_end = f0_start * (0.85 + 0.25 * u01());
        f0_base = (f0_base + 0.3 * (f0_end - f0_base)).clamp(95.0, 150.0);
        for k in 0..burst_len.min(n - i) {
            let env = ((k as f32 / burst_len as f32) * std::f32::consts::PI).sin();
            // Fricatives sit well below the vowels, as in speech.
            samples[i] = 0.04 * env * fricative.run(2.0 * noise.uniform());
            i += 1;
        }
        for k in 0..vowel_len.min(n - i) {
            // Formants glide from the previous vowel over the first 40 ms.
            let glide = (k as f32 / (0.04 * sr)).min(1.0);
            if k % 48 == 0 {
                f1.tune(vowel.0 + (next.0 - vowel.0) * glide, 80.0);
                f2.tune(vowel.1 + (next.1 - vowel.1) * glide, 100.0);
                f3.tune(vowel.2 + (next.2 - vowel.2) * glide, 140.0);
            }
            let f0 = f0_start + (f0_end - f0_start) * k as f32 / vowel_len as f32;
            phase += f0 * (1.0 + 0.01 * noise.uniform()) / sr;
            let pulse = if phase >= 1.0 {
                phase -= 1.0;
                1.0
            } else {
                0.0
            };
            // Leaky integration gives the glottal −12 dB/oct rolloff.
            glottis = 0.97 * glottis + pulse;
            let source = glottis + 0.02 * noise.uniform();
            let v = f1.run(source) + 0.6 * f2.run(source) + 0.3 * f3.run(source);
            let attack = (k as f32 / (0.02 * sr)).min(1.0);
            let release = ((vowel_len - k) as f32 / (0.04 * sr)).min(1.0);
            samples[i] = 0.4 * v * attack * release;
            i += 1;
        }
        vowel = next;
        i += gap_len.min(n - i);
    }
    let r = rms(&samples);
    if r > 0.0 {
        let g = rms_target / r;
        for s in samples.iter_mut() {
            *s *= g;
        }
    }
    let block_rms: Vec<f32> = samples
        .as_chunks::<BLOCK>()
        .0
        .iter()
        .map(|b| rms(b))
        .collect();
    let mut sorted = block_rms.clone();
    sorted.sort_by(f32::total_cmp);
    let p90 = sorted.get(sorted.len() * 9 / 10).copied().unwrap_or(0.0);
    let loud = block_rms.iter().map(|&r| r > 0.5 * p90).collect();
    Voice { samples, loud }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_covers_minus_one_to_one_with_zero_mean() {
        let mut rng = Rng::new(42);
        let (mut lo, mut hi, mut sum) = (f32::MAX, f32::MIN, 0.0f64);
        let n = 200_000;
        for _ in 0..n {
            let u = rng.uniform();
            lo = lo.min(u);
            hi = hi.max(u);
            sum += u as f64;
        }
        assert!((-1.0..-0.99).contains(&lo), "min {lo}");
        assert!(hi < 1.0 && hi > 0.99, "max {hi}");
        assert!((sum / n as f64).abs() < 0.01, "mean {}", sum / n as f64);
    }

    fn window_at(source: &[f32], pos: usize) -> Vec<f32> {
        (0..960).map(|i| source[(pos + i) % source.len()]).collect()
    }

    #[test]
    fn continuity_counts_gaps_and_skips() {
        let source = harmonic_voice(SR * 2, 0.15);
        let phase = 12_345;
        let clean: Vec<f32> = source
            .iter()
            .cycle()
            .skip(phase)
            .take(SR * 3)
            .copied()
            .collect();
        let c = continuity(&clean, &source, 960);
        assert!(c.windows > 50, "{c:?}");
        assert_eq!((c.glitches, c.realigned), (0, 0), "{c:?}");

        // Zero 20 ms in a voiced stretch, and cut one 20 ms frame elsewhere.
        let voiced = |from: usize| {
            (from..)
                .step_by(960)
                .find(|&p| rms(&window_at(&source, phase + p)) > 0.1)
                .unwrap()
        };
        let mut glitched = clean.clone();
        let gap = voiced(SR);
        glitched[gap..gap + 960].fill(0.0);
        let cut = voiced(2 * SR);
        glitched.drain(cut..cut + 960);
        let c = continuity(&glitched, &source, 960);
        assert!(c.dropouts >= 1, "{c:?}");
        assert!(c.realigned >= 1, "{c:?}");
    }
}
