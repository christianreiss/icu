//! Signalerzeugung und Metriken fuer Spike 6 (AEC).
//! Wegwerf-Code. Kein Produktcode.

pub const SR: usize = 48_000;
pub const FRAME: usize = SR / 100; // 10 ms = 480 Samples

/// Kleiner deterministischer PRNG (xorshift64*), damit keine Crate-Abhaengigkeit noetig ist.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Gleichverteilt in [-1, 1).
    pub fn uniform(&mut self) -> f32 {
        (self.next_u64() >> 11) as f32 / (1u64 << 52) as f32 * 2.0 - 1.0
    }
}

/// Sprachaehnliches Signal: gefaerbtes Rauschen (AR-Tiefpass + zwei Resonanzen als grobe
/// Formantnachbildung), moduliert mit einer silbenartigen Huellkurve inklusive Pausen.
/// Absichtlich *kein* weisses Rauschen: weisses Rauschen laesst einen adaptiven Filter
/// unrealistisch schnell konvergieren.
pub fn speechlike(n: usize, seed: u64, syllable_hz: f32, phase: f32, rms_target: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut out = vec![0.0f32; n];

    // AR(1)-Tiefpass fuer die Spektralneigung.
    let mut lp = 0.0f32;
    // Zwei Resonatoren (Biquad-Bandpass, grob bei 700 Hz und 1800 Hz).
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
        // Silbenhuellkurve mit Pausen: nur die positive Halbwelle, mit Wurzel angehoben.
        let m = (2.0 * std::f32::consts::PI * syllable_hz * t + phase).sin();
        let env = if m > 0.05 { m.powf(0.6) } else { 0.0 };
        // Langsamere Atem-/Satzpausen (alle ~2,5 s eine kurze Pause).
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

/// Simulierter Raum: Direktschall nach `delay_ms` plus ein paar abklingende Reflexionen
/// (kurze, exponentiell abfallende Nachhall-IR mit unregelmaessigen Taps).
pub fn room_ir(delay_ms: f32, direct_gain: f32, rt_ms: f32, seed: u64) -> Vec<f32> {
    let d = (delay_ms * SR as f32 / 1000.0).round() as usize;
    let tail = (rt_ms * SR as f32 / 1000.0).round() as usize;
    let mut ir = vec![0.0f32; d + tail + 1];
    ir[d] = direct_gain;
    let mut rng = Rng::new(seed);
    // ~60 Reflexionen, exponentiell abklingend.
    for k in 0..60 {
        let frac = (k as f32 + 1.0) / 60.0;
        let pos = d + ((frac.powf(1.3) * tail as f32) as usize).min(tail.saturating_sub(1));
        let decay = (-6.0 * frac).exp();
        ir[pos] += direct_gain * 0.6 * decay * rng.uniform();
    }
    ir
}

/// FIR-Faltung (direkt; fuer 10 s und wenige tausend Taps schnell genug).
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

/// Resampling per linearer Interpolation, um Clock-Drift zu simulieren:
/// `ppm` > 0 heisst, die Capture-Uhr laeuft schneller als die Playback-Uhr.
pub fn drift_resample(x: &[f32], ppm: f32) -> Vec<f32> {
    let ratio = 1.0 + ppm * 1e-6;
    let n = x.len();
    let mut y = vec![0.0f32; n];
    for (i, s) in y.iter_mut().enumerate() {
        let pos = i as f32 * ratio;
        let i0 = pos.floor() as usize;
        if i0 + 1 >= n {
            break;
        }
        let f = pos - i0 as f32;
        *s = x[i0] * (1.0 - f) + x[i0 + 1] * f;
    }
    y
}

pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

pub fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| (*v as f64) * (*v as f64)).sum()
}

/// ERLE in dB ueber ein Fenster: 10*log10(E_mic / E_out).
pub fn erle_db(mic: &[f32], out: &[f32]) -> f64 {
    let em = energy(mic);
    let eo = energy(out);
    if eo <= 0.0 || em <= 0.0 {
        return f64::NAN;
    }
    10.0 * (em / eo).log10()
}

/// Pearson-Korrelation.
pub fn pearson(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f64::NAN;
    }
    let ma = a[..n].iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    let mb = b[..n].iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    let mut num = 0.0;
    let mut da = 0.0;
    let mut db = 0.0;
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

/// Skaleninvariantes SDR (dB) von `est` gegenueber dem sauberen Referenzsignal `refs`.
/// Ein globaler Pegelfaktor wird herausgerechnet, ein Zeitversatz nicht.
pub fn si_sdr_db(est: &[f32], refs: &[f32]) -> f64 {
    let n = est.len().min(refs.len());
    if n == 0 {
        return f64::NAN;
    }
    let mut dot = 0.0f64;
    let mut er = 0.0f64;
    for i in 0..n {
        dot += est[i] as f64 * refs[i] as f64;
        er += (refs[i] as f64) * (refs[i] as f64);
    }
    if er <= 0.0 {
        return f64::NAN;
    }
    let a = dot / er;
    let mut es = 0.0f64;
    let mut en = 0.0f64;
    for i in 0..n {
        let s = a * refs[i] as f64;
        let e = est[i] as f64 - s;
        es += s * s;
        en += e * e;
    }
    if en <= 0.0 {
        return f64::INFINITY;
    }
    10.0 * (es / en).log10()
}

/// Verzoegerung in Samples, die `y` gegenueber `x` hat (Maximum der Kreuzkorrelation).
pub fn best_lag(x: &[f32], y: &[f32], max_lag: usize) -> usize {
    let mut best = 0usize;
    let mut best_v = f64::NEG_INFINITY;
    for lag in 0..=max_lag {
        let mut s = 0.0f64;
        let n = x.len().min(y.len().saturating_sub(lag));
        let step = 1;
        let mut i = 0;
        while i < n {
            s += x[i] as f64 * y[i + lag] as f64;
            i += step;
        }
        if s > best_v {
            best_v = s;
            best = lag;
        }
    }
    best
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}
