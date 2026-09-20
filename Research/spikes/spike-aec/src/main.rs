//! Spike 6: Echo Cancellation mit dem WebRTC AudioProcessing Module (AEC3)
//! ueber das Crate `webrtc-audio-processing` 2.1.0 (Feature `bundled`).
//!
//! Offline, ohne Mikrofon: Far-End wird synthetisch erzeugt, das Near-End-Mikrofonsignal
//! ist eine verzoegerte, gedaempfte, verhallte Kopie plus optionalem Near-End-Sprecher.
//! Gemessen werden ERLE (Single-Talk), Near-End-Erhalt (Double-Talk), CPU je Frame,
//! algorithmische Verzoegerung, Delay-Robustheit und Clock-Drift-Robustheit.
//!
//! Wegwerf-Code. Kein Produktcode.

mod signal;

use signal::*;
use std::time::Instant;
use webrtc_audio_processing::{Config, Processor};
use webrtc_audio_processing_config::{
    EchoCanceller, GainController, HighPassFilter, NoiseSuppression,
};

const SECS: usize = 10;
const N: usize = SR * SECS;

/// Zeitplan: 0-4 s nur Far-End (Konvergenz), 4-7 s Double-Talk, 7-10 s nur Far-End.
const DT_START: usize = SR * 4;
const DT_END: usize = SR * 7;

/// Mikrofon-Grundrauschen. -50 dBFS ist ein realistischer Wert fuer ein Consumer-Mikrofon
/// in einem ruhigen Raum; der Wert deckelt die messbare ERLE nach oben.
const MIC_NOISE_RMS: f32 = 0.00316; // -50 dBFS

#[derive(Clone, Copy)]
struct SceneOpts {
    delay_ms: f32,
    drift_ppm: f32,
    /// (Zeitpunkt in Samples, neues Delay in ms)
    delay_step: Option<(usize, f32)>,
    /// Lautsprecher-Nichtlinearitaet (Soft-Clipping). 0.0 = linear.
    speaker_drive: f32,
}

impl Default for SceneOpts {
    fn default() -> Self {
        SceneOpts { delay_ms: 80.0, drift_ppm: 0.0, delay_step: None, speaker_drive: 0.0 }
    }
}

struct Scene {
    /// Was aus dem Lautsprecher kommen soll (Referenz / Reverse Stream).
    far: Vec<f32>,
    /// Was das Mikrofon aufnimmt: Echo + Near-End + Rauschteppich.
    mic: Vec<f32>,
    /// Der reine Near-End-Anteil im Mikrofon (Ground Truth fuer Double-Talk).
    near: Vec<f32>,
    /// Nur der Echo-Anteil.
    echo: Vec<f32>,
}

/// Soft-Clipping als grobe Nachbildung eines uebersteuerten Kleinlautsprechers.
fn soft_clip(x: &[f32], drive: f32) -> Vec<f32> {
    if drive <= 0.0 {
        return x.to_vec();
    }
    x.iter().map(|v| (v * drive).tanh() / drive).collect()
}

fn build_scene(o: SceneOpts) -> Scene {
    // Far-End: der entfernte Sprecher auf dem Lautsprecher, RMS -20 dBFS.
    let far = speechlike(N, 0xC0FFEE, 3.3, 0.0, 0.1);

    // Der Lautsprecher verzerrt (optional), erst danach wirkt der Raum.
    let played = soft_clip(&far, o.speaker_drive);

    // Raum: Direktschall bei delay_ms (ERL rund 6 dB), 80 ms Nachhall.
    let ir = room_ir(o.delay_ms, 0.5, 80.0, 0xBEEF);
    let mut echo = convolve(&played, &ir);

    // Optionaler Delay-Sprung zur Laufzeit.
    if let Some((at, new_delay_ms)) = o.delay_step {
        let ir2 = room_ir(new_delay_ms, 0.5, 80.0, 0xBEEF);
        let echo2 = convolve(&played, &ir2);
        echo[at..].copy_from_slice(&echo2[at..]);
    }

    // Clock-Drift zwischen Capture- und Playback-Uhr.
    if o.drift_ppm != 0.0 {
        echo = drift_resample(&echo, o.drift_ppm);
    }

    // Near-End-Sprecher, nur im Double-Talk-Fenster, RMS auf Echo-Niveau.
    let echo_rms_dt = rms(&echo[DT_START..DT_END]).max(1e-6);
    let near_full = speechlike(N, 0x5EED_1234, 2.7, 1.1, echo_rms_dt);
    let mut near = vec![0.0f32; N];
    let ramp = SR / 50; // 20 ms Ein-/Ausblendung
    for i in DT_START..DT_END {
        let w = if i < DT_START + ramp {
            (i - DT_START) as f32 / ramp as f32
        } else if i > DT_END - ramp {
            (DT_END - i) as f32 / ramp as f32
        } else {
            1.0
        };
        near[i] = near_full[i] * w;
    }

    let mut rng = Rng::new(0xA5A5);
    let g = MIC_NOISE_RMS * 3f32.sqrt(); // Gleichverteilung [-g,g] hat RMS g/sqrt(3)
    let mic: Vec<f32> = (0..N).map(|i| echo[i] + near[i] + rng.uniform() * g).collect();

    Scene { far, mic, near, echo }
}

/// Basiskonfiguration: NUR der Echo Canceller, damit ERLE nicht von NS/AGC verfaelscht wird.
/// Hinweis: AEC3 erzwingt intern `enforce_high_pass_filtering = true` (config-Doku),
/// das separate High-Pass-Filter-Submodul bleibt hier aus.
fn aec_only(stream_delay_ms: Option<u16>) -> Config {
    Config {
        echo_canceller: Some(EchoCanceller::Full { stream_delay_ms }),
        high_pass_filter: None,
        noise_suppression: None,
        gain_controller: None,
        ..Default::default()
    }
}

struct RunResult {
    out: Vec<f32>,
    capture_us: Vec<f64>,
    render_us: Vec<f64>,
}

/// Faehrt eine Szene durch das APM. Reihenfolge je 10-ms-Tick: erst Render-Frame,
/// dann Capture-Frame (das Echo in Capture-Frame k stammt aus Render-Frames <= k).
fn run(scene: &Scene, cfg: Config) -> RunResult {
    let ap = Processor::new(SR as u32).expect("Processor::new");
    ap.set_config(cfg);

    let frames = N / FRAME;
    let mut out = vec![0.0f32; N];
    let mut capture_us = Vec::with_capacity(frames);
    let mut render_us = Vec::with_capacity(frames);

    let mut rbuf = vec![vec![0.0f32; FRAME]];
    let mut cbuf = vec![vec![0.0f32; FRAME]];

    for k in 0..frames {
        let a = k * FRAME;
        let b = a + FRAME;

        rbuf[0].copy_from_slice(&scene.far[a..b]);
        let t0 = Instant::now();
        ap.process_render_frame(&mut rbuf).expect("render");
        render_us.push(t0.elapsed().as_secs_f64() * 1e6);

        cbuf[0].copy_from_slice(&scene.mic[a..b]);
        let t1 = Instant::now();
        ap.process_capture_frame(&mut cbuf).expect("capture");
        capture_us.push(t1.elapsed().as_secs_f64() * 1e6);

        out[a..b].copy_from_slice(&cbuf[0]);
    }

    RunResult { out, capture_us, render_us }
}

fn win(x: &[f32], from_s: f64, to_s: f64) -> &[f32] {
    let a = (from_s * SR as f64) as usize;
    let b = ((to_s * SR as f64) as usize).min(x.len());
    &x[a..b]
}

fn erle_per_second(mic: &[f32], out: &[f32]) -> Vec<f64> {
    (0..SECS)
        .map(|s| erle_db(win(mic, s as f64, s as f64 + 1.0), win(out, s as f64, s as f64 + 1.0)))
        .collect()
}

/// Eingeschwungene ERLE: letzte 2 s des jeweiligen Single-Talk-Fensters.
fn steady_erle(mic: &[f32], out: &[f32]) -> (f64, f64) {
    (erle_db(win(mic, 2.0, 4.0), win(out, 2.0, 4.0)), erle_db(win(mic, 8.0, 10.0), win(out, 8.0, 10.0)))
}

/// Baseline: dieselbe Konfiguration, aber ohne jedes Far-End-Signal. Das ist das Near-End,
/// wie das APM es ausgibt, wenn es nichts wegzurechnen gibt. Nur gegen diese Baseline ist
/// der Double-Talk-Schaden messbar; gegen das rohe Near-End wuerde man auch den
/// (von AEC3 erzwungenen) Hochpass und die NS-Faerbung als "Schaden" zaehlen.
fn near_only_baseline(scene: &Scene, cfg: Config) -> Vec<f32> {
    let mut rng = Rng::new(0xA5A5);
    let g = MIC_NOISE_RMS * 3f32.sqrt();
    let mic: Vec<f32> = (0..N).map(|i| scene.near[i] + rng.uniform() * g).collect();
    let s = Scene {
        far: vec![0.0; N],
        mic,
        near: scene.near.clone(),
        echo: vec![0.0; N],
    };
    run(&s, cfg).out
}

/// Double-Talk-Auswertung. `out` wird um `lag` Samples ausgerichtet und gegen `refsig`
/// verglichen, das um `ref_lag` ausgerichtet wird.
/// Liefert (SI-SDR Near-End-aktiv, Korrelation Near-End-aktiv, ERLE in den Near-End-Pausen).
fn double_talk_vs(
    scene: &Scene,
    out: &[f32],
    lag: usize,
    refsig: &[f32],
    ref_lag: usize,
) -> (f64, f64, f64) {
    let blocks: Vec<f32> = (DT_START..DT_END)
        .step_by(FRAME)
        .map(|a| rms(&scene.near[a..(a + FRAME).min(N)]))
        .collect();
    let peak = blocks.iter().cloned().fold(0.0f32, f32::max);
    let thr = peak * 0.10;

    let (mut a_out, mut a_ref) = (Vec::new(), Vec::new());
    let (mut p_mic, mut p_out) = (Vec::new(), Vec::new());

    for (bi, a) in (DT_START..DT_END).step_by(FRAME).enumerate() {
        let b = (a + FRAME).min(N);
        if a < DT_START + SR / 5 || b > DT_END - SR / 5 {
            continue;
        }
        if blocks[bi] > thr {
            for i in a..b {
                if i + lag < N && i + ref_lag < refsig.len() {
                    a_out.push(out[i + lag]);
                    a_ref.push(refsig[i + ref_lag]);
                }
            }
        } else {
            for i in a..b {
                if i + lag < N {
                    p_mic.push(scene.mic[i]);
                    p_out.push(out[i + lag]);
                }
            }
        }
    }

    (si_sdr_db(&a_out, &a_ref), pearson(&a_out, &a_ref), erle_db(&p_mic, &p_out))
}

/// Wie [`double_talk_vs`], aber gegen das rohe (unverarbeitete) Near-End.
fn double_talk_metrics(scene: &Scene, out: &[f32], lag: usize) -> (f64, f64, f64) {
    let near = scene.near.clone();
    double_talk_vs(scene, out, lag, &near, 0)
}

fn stats_line(mut v: Vec<f64>) -> (f64, f64, f64) {
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (mean, percentile(&v, 0.50), percentile(&v, 0.99))
}

/// Algorithmische Verzoegerung: Impuls durch den Capture-Pfad, Render-Stream still.
fn measure_algorithmic_delay() -> (i64, f64) {
    let ap = Processor::new(SR as u32).expect("Processor::new");
    ap.set_config(aec_only(None));

    let frames = 100;
    let impulse_frame = 20;
    let impulse_pos = 100usize;
    let mut rbuf = vec![vec![0.0f32; FRAME]];
    let mut cbuf = vec![vec![0.0f32; FRAME]];
    let mut out = Vec::with_capacity(frames * FRAME);

    for k in 0..frames {
        rbuf[0].iter_mut().for_each(|s| *s = 0.0);
        ap.process_render_frame(&mut rbuf).unwrap();
        cbuf[0].iter_mut().for_each(|s| *s = 0.0);
        if k == impulse_frame {
            cbuf[0][impulse_pos] = 0.5;
        }
        ap.process_capture_frame(&mut cbuf).unwrap();
        out.extend_from_slice(&cbuf[0]);
    }

    let in_pos = impulse_frame * FRAME + impulse_pos;
    let (mut peak, mut peak_v) = (0usize, 0.0f32);
    for (i, v) in out.iter().enumerate() {
        if v.abs() > peak_v {
            peak_v = v.abs();
            peak = i;
        }
    }
    (peak as i64 - in_pos as i64, peak_v as f64)
}

/// Langer Drift-Lauf ohne Double-Talk. Noetig, weil 100 ppm ueber 10 s nur 48 Samples
/// Schlupf ergeben — weniger als einen AEC3-Block (64 Samples bei 16 kHz Bandrate,
/// 192 bei 48 kHz). Ein 10-s-Test bei 100 ppm erzwingt also gar keine Neuausrichtung
/// und sagt nichts ueber eine lange Session. Gemessen wird die ERLE in den letzten 10 s.
fn drift_long(secs: usize, ppm: f32, drive: f32) -> (f64, f64) {
    let n = SR * secs;
    let far = speechlike(n, 0xC0FFEE, 3.3, 0.0, 0.1);
    let played = soft_clip(&far, drive);
    let ir = room_ir(80.0, 0.5, 80.0, 0xBEEF);
    let mut echo = convolve(&played, &ir);
    if ppm != 0.0 {
        echo = drift_resample(&echo, ppm);
    }
    let mut rng = Rng::new(0xA5A5);
    let g = MIC_NOISE_RMS * 3f32.sqrt();
    let mic: Vec<f32> = (0..n).map(|i| echo[i] + rng.uniform() * g).collect();

    let ap = Processor::new(SR as u32).expect("Processor::new");
    ap.set_config(aec_only(None));
    let mut out = vec![0.0f32; n];
    let mut rbuf = vec![vec![0.0f32; FRAME]];
    let mut cbuf = vec![vec![0.0f32; FRAME]];
    for k in 0..(n / FRAME) {
        let (a, b) = (k * FRAME, k * FRAME + FRAME);
        rbuf[0].copy_from_slice(&far[a..b]);
        ap.process_render_frame(&mut rbuf).unwrap();
        cbuf[0].copy_from_slice(&mic[a..b]);
        ap.process_capture_frame(&mut cbuf).unwrap();
        out[a..b].copy_from_slice(&cbuf[0]);
    }

    // Erste 10 s (frisch konvergiert) gegen letzte 10 s (nach kumuliertem Schlupf).
    let first = erle_db(&mic[SR * 2..SR * 10], &out[SR * 2..SR * 10]);
    let last = erle_db(&mic[n - SR * 10..], &out[n - SR * 10..]);
    (first, last)
}

fn main() {
    println!("# Spike 6: WebRTC AEC3 ueber webrtc-audio-processing 2.1.0 (bundled)");
    println!("# 48000 Hz mono, 10-ms-Frames (480 Samples), 10 s Material");
    println!("# Zeitplan: 0-4 s Far-End only | 4-7 s Double-Talk | 7-10 s Far-End only");
    println!("# Mikrofon-Grundrauschen {:.1} dBFS -> messbare ERLE nach oben gedeckelt\n", 20.0 * MIC_NOISE_RMS.log10());

    // ---------------------------------------------- 0) Algorithmische Verzoegerung
    let (lag_i, peak) = measure_algorithmic_delay();
    let lag = lag_i.max(0) as usize;
    println!("## 0) Algorithmische Verzoegerung (Impuls durch den Capture-Pfad, Render still)");
    println!(
        "Verschiebung Ausgabespitze gegenueber Eingabe: {} Samples = {:.2} ms (Spitze {:.3}, Eingang 0.500)",
        lag_i,
        lag_i as f64 * 1000.0 / SR as f64,
        peak
    );
    println!("Alle Double-Talk-Metriken unten sind um diese Verzoegerung ausgerichtet.\n");

    // ---------------------------------------------------------------- Basisszene
    let scene = build_scene(SceneOpts::default());
    println!(
        "Szene: Echopfad-Delay 80 ms, ERL(direct) ~6 dB, Echo-RMS {:.1} dBFS, Far-RMS {:.1} dBFS, Near-RMS(DT) {:.1} dBFS",
        20.0 * rms(&scene.echo).log10(),
        20.0 * rms(&scene.far).log10(),
        20.0 * rms(&scene.near[DT_START..DT_END]).log10()
    );
    let l = best_lag(&scene.far[..SR], &scene.echo, SR / 2);
    println!("Gegenprobe Kreuzkorrelation far->echo: {} Samples = {:.1} ms\n", l, l as f64 * 1000.0 / SR as f64);

    // ------------------------------------------------- 1) Auto-Delay (Referenzlauf)
    let r = run(&scene, aec_only(None));
    println!("## 1) Referenzlauf, stream_delay_ms = None (AEC3-eigene Delay-Schaetzung), linearer Lautsprecher");
    print!("ERLE je Sekunde [dB]:");
    for (s, v) in erle_per_second(&scene.mic, &r.out).iter().enumerate() {
        print!(" {}s={:.1}", s, v);
    }
    println!();
    let (e1, e2) = steady_erle(&scene.mic, &r.out);
    println!("Eingeschwungene ERLE Single-Talk: 2-4 s = {:.1} dB, 8-10 s = {:.1} dB", e1, e2);

    let (sdr, corr, dt_erle) = double_talk_metrics(&scene, &r.out, lag);
    let (sdr0, corr0, dt_erle0) = double_talk_metrics(&scene, &scene.mic, 0);
    let base = near_only_baseline(&scene, aec_only(None));
    let (sdrb, corrb, _) = double_talk_vs(&scene, &r.out, lag, &base, lag);
    println!(
        "Double-Talk, Near-End aktiv, gegen ROHES Near-End: SI-SDR {:.1} dB (Mikrofon unbearbeitet {:.1} dB), Korr {:.3} (roh {:.3})",
        sdr, sdr0, corr, corr0
    );
    println!(
        "Double-Talk, Near-End aktiv, gegen BASELINE (dasselbe APM ohne Far-End): SI-SDR {:.1} dB, Korr {:.3}",
        sdrb, corrb
    );
    println!("Double-Talk, Near-End-Pausen: ERLE {:.1} dB (roh {:.1} dB)", dt_erle, dt_erle0);

    let (cm, cp50, cp99) = stats_line(r.capture_us.clone());
    let (rm, rp50, rp99) = stats_line(r.render_us.clone());
    println!(
        "CPU je 10-ms-Frame: capture mean {:.0} us / p50 {:.0} / p99 {:.0} | render mean {:.0} us / p50 {:.0} / p99 {:.0}",
        cm, cp50, cp99, rm, rp50, rp99
    );
    println!("  Summe mean {:.0} us je 10 ms = {:.2} % eines Kerns\n", cm + rm, (cm + rm) / 10_000.0 * 100.0);

    // ------------------------------------------ 2) Nichtlinearer Lautsprecher
    println!("## 2) Lautsprecher-Nichtlinearitaet (Soft-Clipping), sonst wie 1)");
    println!("| drive | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] | DT SI-SDR [dB] | DT-Pausen-ERLE [dB] |");
    println!("| --- | --- | --- | --- | --- |");
    for drive in [0.0f32, 2.0, 5.0, 10.0] {
        let sc = build_scene(SceneOpts { speaker_drive: drive, ..Default::default() });
        let rr = run(&sc, aec_only(None));
        let (a, b) = steady_erle(&sc.mic, &rr.out);
        let bl = near_only_baseline(&sc, aec_only(None));
        let (s, _, p) = double_talk_vs(&sc, &rr.out, lag, &bl, lag);
        println!("| {:.0} | {:.1} | {:.1} | {:.1} | {:.1} |", drive, a, b, s, p);
    }
    println!();

    // ------------------------------------------------------ 3) Delay-Robustheit
    println!("## 3) set_stream_delay_ms falsch angegeben (wahres Echopfad-Delay 80 ms), AEC3 Full");
    println!("| stream_delay_ms | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] | DT SI-SDR [dB] |");
    println!("| --- | --- | --- | --- |");
    for sd in [None, Some(50u16), Some(80), Some(110), Some(0), Some(200)] {
        let rr = run(&scene, aec_only(sd));
        let (a, b) = steady_erle(&scene.mic, &rr.out);
        let (s, _, _) = double_talk_metrics(&scene, &rr.out, lag);
        println!(
            "| {} | {:.1} | {:.1} | {:.1} |",
            sd.map(|v| v.to_string()).unwrap_or_else(|| "None (auto)".into()),
            a,
            b,
            s
        );
    }
    // Gegenprobe mit AECM: dort ist stream_delay_ms laut Crate-Doku Pflicht.
    println!("\nGegenprobe mit EchoCanceller::Mobile (AECM), wo stream_delay_ms Pflicht ist:");
    println!("| stream_delay_ms | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] |");
    println!("| --- | --- | --- |");
    for sd in [0u16, 50, 80, 110, 200] {
        let cfg = Config {
            echo_canceller: Some(EchoCanceller::Mobile { stream_delay_ms: sd }),
            high_pass_filter: None,
            noise_suppression: None,
            gain_controller: None,
            ..Default::default()
        };
        let rr = run(&scene, cfg);
        let (a, b) = steady_erle(&scene.mic, &rr.out);
        println!("| {} | {:.1} | {:.1} |", sd, a, b);
    }
    println!();

    // ------------------------------------------------------- 4) Wahres Delay variieren
    println!("## 4) Wahres Echopfad-Delay variieren (stream_delay_ms = None)");
    println!("| wahres Delay [ms] | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] |");
    println!("| --- | --- | --- |");
    for d_ms in [40.0f32, 80.0, 120.0, 200.0, 400.0] {
        let sc = build_scene(SceneOpts { delay_ms: d_ms, ..Default::default() });
        let rr = run(&sc, aec_only(None));
        let (a, b) = steady_erle(&sc.mic, &rr.out);
        println!("| {:.0} | {:.1} | {:.1} |", d_ms, a, b);
    }
    println!();

    // ------------------------------------------------------------ 5) Delay-Sprung
    println!("## 5) Delay-Sprung bei t = 5 s (80 ms -> 140 ms), stream_delay_ms = None");
    let sc = build_scene(SceneOpts { delay_step: Some((SR * 5, 140.0)), ..Default::default() });
    let rr = run(&sc, aec_only(None));
    print!("ERLE je Sekunde [dB]:");
    for (s, v) in erle_per_second(&sc.mic, &rr.out).iter().enumerate() {
        print!(" {}s={:.1}", s, v);
    }
    println!("\n");

    // -------------------------------------------------------------- 6) Clock-Drift
    println!("## 6) Clock-Drift zwischen Capture- und Playback-Uhr (stream_delay_ms = None)");
    println!("| Drift [ppm] | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] | mit drive=5 8-10 s [dB] |");
    println!("| --- | --- | --- | --- |");
    for ppm in [0.0f32, 10.0, 100.0, 1000.0, 5000.0] {
        let sc = build_scene(SceneOpts { drift_ppm: ppm, ..Default::default() });
        let rr = run(&sc, aec_only(None));
        let (a, b) = steady_erle(&sc.mic, &rr.out);
        let sc2 = build_scene(SceneOpts { drift_ppm: ppm, speaker_drive: 5.0, ..Default::default() });
        let rr2 = run(&sc2, aec_only(None));
        let (_, b2) = steady_erle(&sc2.mic, &rr2.out);
        println!("| {:.0} | {:.1} | {:.1} | {:.1} |", ppm, a, b, b2);
    }
    println!();

    // ---------------------------------------- 6b) Drift ueber lange Sessions
    println!("## 6b) Clock-Drift ueber 60 s (kumulierter Schlupf statt Momentaufnahme)");
    println!("Schlupf = ppm * 1e-6 * Dauer * 48000 Samples; ein AEC3-Block sind 192 Samples bei 48 kHz.");
    println!("| Drift [ppm] | Dauer [s] | Schlupf am Ende [Samples / ms] | ERLE 2-10 s [dB] | ERLE letzte 10 s [dB] |");
    println!("| --- | --- | --- | --- | --- |");
    for (ppm, secs) in [
        (0.0f32, 60usize),
        (100.0, 60),
        (500.0, 60),
        (1000.0, 60),
        // Gleiche Frage noch einmal, aber mit realistischem Drift ueber eine lange Session:
        // Ist der akkumulierte Schlupf entscheidend oder die Driftrate?
        (100.0, 300),
        (100.0, 600),
        (20.0, 600),
    ] {
        let (first, last) = drift_long(secs, ppm, 5.0);
        let slip = ppm * 1e-6 * secs as f32 * SR as f32;
        println!(
            "| {:.0} | {} | {:.0} / {:.1} ms | {:.1} | {:.1} |",
            ppm,
            secs,
            slip,
            slip * 1000.0 / SR as f32,
            first,
            last
        );
    }
    println!("(AEC3-Filterlaenge: 13 Bloecke x 64 Samples bei 16 kHz Bandrate = 52 ms)");
    println!();

    // --------------------------------------------- 7) AEC + NS (Reihenfolge im APM)
    println!("## 7) AEC allein vs. AEC + HPF + WebRTC-NS im selben APM (intern AEC -> NS)");
    let sc = build_scene(SceneOpts { speaker_drive: 5.0, ..Default::default() });
    for (name, cfg) in [
        ("AEC allein", aec_only(None)),
        (
            "AEC+HPF+NS",
            Config {
                echo_canceller: Some(EchoCanceller::Full { stream_delay_ms: None }),
                high_pass_filter: Some(HighPassFilter::default()),
                noise_suppression: Some(NoiseSuppression::default()),
                gain_controller: None,
                ..Default::default()
            },
        ),
        (
            "AEC+HPF+NS+AGC2",
            Config {
                echo_canceller: Some(EchoCanceller::Full { stream_delay_ms: None }),
                high_pass_filter: Some(HighPassFilter::default()),
                noise_suppression: Some(NoiseSuppression::default()),
                gain_controller: Some(GainController::GainController2(Default::default())),
                ..Default::default()
            },
        ),
    ] {
        let rr = run(&sc, cfg);
        let (_, b) = steady_erle(&sc.mic, &rr.out);
        let bl = near_only_baseline(&sc, cfg);
        let (s, c, p) = double_talk_vs(&sc, &rr.out, lag, &bl, lag);
        let (cm, _, cp99) = stats_line(rr.capture_us.clone());
        let (rm, _, _) = stats_line(rr.render_us.clone());
        println!(
            "{:<16} ERLE 8-10 s {:>5.1} dB | DT SI-SDR vs. Baseline {:>5.1} dB | DT-Korr {:.3} | DT-Pausen-ERLE {:>5.1} dB | CPU {:.0} us/10 ms (capture p99 {:.0}) = {:.2} % Kern",
            name,
            b,
            s,
            c,
            p,
            cm + rm,
            cp99,
            (cm + rm) / 10_000.0 * 100.0
        );
    }
    println!();

    // -------------------------------- 8) Gegenprobe: unvollstaendige Referenz
    println!("## 8) Zentrale Frage: welche Referenz sieht ein In-Process-AEC?");
    println!("Mikrofon hoert Voice-Echo UND Spielton; Referenz kennt einmal nur die Voice,");
    println!("einmal den kompletten Systemmix (Loopback / Monitor-Source).");
    let game = speechlike(N, 0x1234_5678, 1.9, 2.2, 0.1);
    let ir = room_ir(80.0, 0.5, 80.0, 0xBEEF);
    let game_echo = convolve(&soft_clip(&game, 5.0), &ir);
    let base = build_scene(SceneOpts { speaker_drive: 5.0, ..Default::default() });
    let mut rng = Rng::new(0xA5A5);
    let g = MIC_NOISE_RMS * 3f32.sqrt();
    let mic_mixed: Vec<f32> =
        (0..N).map(|i| base.echo[i] + game_echo[i] + base.near[i] + rng.uniform() * g).collect();

    println!("| Referenzsignal | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] |");
    println!("| --- | --- | --- |");
    let only_voice = Scene {
        far: base.far.clone(),
        mic: mic_mixed.clone(),
        near: base.near.clone(),
        echo: base.echo.clone(),
    };
    let rp = run(&only_voice, aec_only(None));
    let (a, b) = steady_erle(&only_voice.mic, &rp.out);
    println!("| nur eigener Voice-Playback | {:.1} | {:.1} |", a, b);

    let sysmix: Vec<f32> = (0..N).map(|i| base.far[i] + game[i]).collect();
    let full = Scene { far: sysmix, mic: mic_mixed, near: base.near.clone(), echo: base.echo.clone() };
    let rs = run(&full, aec_only(None));
    let (a2, b2) = steady_erle(&full.mic, &rs.out);
    println!("| Systemmix (Voice + Spielton) | {:.1} | {:.1} |", a2, b2);
}
