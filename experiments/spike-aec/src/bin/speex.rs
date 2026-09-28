//! Spike 6b: Speex-AEC (speexdsp `speex_echo_*`, MDF-Algorithmus) als Vergleichskandidat.
//!
//! Kein gepflegtes Rust-Binding vorhanden (speexdsp-sys 0.1.2 ist von 2018, speexdsp 0.1.2
//! von 2022), deshalb hier vier handgeschriebene `extern "C"`-Deklarationen gegen die
//! System-libspeexdsp 1.2.1 (Fedora-Paket `speexdsp-devel`).
//! Dieselben Signale und dieselben Metriken wie in `main.rs`, damit die Zahlen
//! nebeneinander stehen koennen.
//!
//! Wegwerf-Code. Kein Produktcode.

#[path = "../signal.rs"]
mod signal;

use signal::*;
use std::os::raw::{c_int, c_void};
use std::time::Instant;

#[repr(C)]
struct SpeexEchoState {
    _private: [u8; 0],
}

#[link(name = "speexdsp")]
extern "C" {
    fn speex_echo_state_init(frame_size: c_int, filter_length: c_int) -> *mut SpeexEchoState;
    fn speex_echo_state_destroy(st: *mut SpeexEchoState);
    fn speex_echo_cancellation(
        st: *mut SpeexEchoState,
        rec: *const i16,
        play: *const i16,
        out: *mut i16,
    );
    fn speex_echo_ctl(st: *mut SpeexEchoState, request: c_int, ptr: *mut c_void) -> c_int;
}

const SPEEX_ECHO_SET_SAMPLING_RATE: c_int = 24;

const SECS: usize = 10;
const N: usize = SR * SECS;
const DT_START: usize = SR * 4;
const DT_END: usize = SR * 7;
const MIC_NOISE_RMS: f32 = 0.00316; // -50 dBFS

fn to_i16(x: &[f32]) -> Vec<i16> {
    x.iter().map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16).collect()
}

fn from_i16(x: &[i16]) -> Vec<f32> {
    x.iter().map(|v| *v as f32 / 32767.0).collect()
}

fn soft_clip(x: &[f32], drive: f32) -> Vec<f32> {
    if drive <= 0.0 {
        return x.to_vec();
    }
    x.iter().map(|v| (v * drive).tanh() / drive).collect()
}

struct Scene {
    far: Vec<f32>,
    mic: Vec<f32>,
    near: Vec<f32>,
}

fn build_scene(delay_ms: f32, drive: f32, drift_ppm: f32) -> Scene {
    let far = speechlike(N, 0xC0FFEE, 3.3, 0.0, 0.1);
    let played = soft_clip(&far, drive);
    let ir = room_ir(delay_ms, 0.5, 80.0, 0xBEEF);
    let mut echo = convolve(&played, &ir);
    if drift_ppm != 0.0 {
        echo = drift_resample(&echo, drift_ppm);
    }

    let echo_rms_dt = rms(&echo[DT_START..DT_END]).max(1e-6);
    let near_full = speechlike(N, 0x5EED_1234, 2.7, 1.1, echo_rms_dt);
    let mut near = vec![0.0f32; N];
    let ramp = SR / 50;
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
    let g = MIC_NOISE_RMS * 3f32.sqrt();
    let mic: Vec<f32> = (0..N).map(|i| echo[i] + near[i] + rng.uniform() * g).collect();

    Scene { far, mic, near }
}

/// `filter_ms` = Laenge des adaptiven Filters (Nachhallzeit, die abgedeckt werden soll).
/// Speex-Doku empfiehlt "1/3 des Nachhalls" bzw. 100-500 ms; der Filter muss das gesamte
/// Echo-Delay PLUS den Nachhall abdecken, sonst ist das Echo ausserhalb des Filterfensters.
fn run_speex(scene: &Scene, frame: usize, filter_ms: f32) -> (Vec<f32>, Vec<f64>) {
    let filter_len = (filter_ms * SR as f32 / 1000.0) as c_int;
    let mic_i = to_i16(&scene.mic);
    let far_i = to_i16(&scene.far);
    let mut out_i = vec![0i16; N];
    let mut times = Vec::new();

    unsafe {
        let st = speex_echo_state_init(frame as c_int, filter_len);
        assert!(!st.is_null());
        let mut rate: c_int = SR as c_int;
        speex_echo_ctl(st, SPEEX_ECHO_SET_SAMPLING_RATE, &mut rate as *mut c_int as *mut c_void);

        let mut k = 0;
        while (k + 1) * frame <= N {
            let a = k * frame;
            let t = Instant::now();
            speex_echo_cancellation(
                st,
                mic_i[a..].as_ptr(),
                far_i[a..].as_ptr(),
                out_i[a..].as_mut_ptr(),
            );
            times.push(t.elapsed().as_secs_f64() * 1e6);
            k += 1;
        }
        speex_echo_state_destroy(st);
    }

    (from_i16(&out_i), times)
}

fn win(x: &[f32], from_s: f64, to_s: f64) -> &[f32] {
    let a = (from_s * SR as f64) as usize;
    let b = ((to_s * SR as f64) as usize).min(x.len());
    &x[a..b]
}

fn steady_erle(mic: &[f32], out: &[f32]) -> (f64, f64) {
    (erle_db(win(mic, 2.0, 4.0), win(out, 2.0, 4.0)), erle_db(win(mic, 8.0, 10.0), win(out, 8.0, 10.0)))
}

fn dt_metrics(scene: &Scene, out: &[f32]) -> (f64, f64, f64) {
    let blocks: Vec<f32> = (DT_START..DT_END)
        .step_by(FRAME)
        .map(|a| rms(&scene.near[a..(a + FRAME).min(N)]))
        .collect();
    let peak = blocks.iter().cloned().fold(0.0f32, f32::max);
    let thr = peak * 0.10;
    let (mut ao, mut an) = (Vec::new(), Vec::new());
    let (mut pm, mut po) = (Vec::new(), Vec::new());
    for (bi, a) in (DT_START..DT_END).step_by(FRAME).enumerate() {
        let b = (a + FRAME).min(N);
        if a < DT_START + SR / 5 || b > DT_END - SR / 5 {
            continue;
        }
        if blocks[bi] > thr {
            ao.extend_from_slice(&out[a..b]);
            an.extend_from_slice(&scene.near[a..b]);
        } else {
            pm.extend_from_slice(&scene.mic[a..b]);
            po.extend_from_slice(&out[a..b]);
        }
    }
    (si_sdr_db(&ao, &an), pearson(&ao, &an), erle_db(&pm, &po))
}

fn main() {
    println!("# Spike 6b: Speex-AEC (libspeexdsp 1.2.1, MDF) - gleiche Signale wie Spike 6");
    println!("# 48000 Hz mono, int16, Echopfad-Delay 80 ms, ERL ~6 dB, Mikrofonrauschen -50 dBFS\n");

    // Algorithmische Verzoegerung: Impuls durch den Capture-Pfad, Playback still.
    {
        let frame = FRAME;
        let mut mic = vec![0i16; N];
        mic[frame * 20 + 100] = 16384;
        let far = vec![0i16; N];
        let mut out = vec![0i16; N];
        unsafe {
            let st = speex_echo_state_init(frame as c_int, (SR / 4) as c_int);
            let mut rate: c_int = SR as c_int;
            speex_echo_ctl(st, SPEEX_ECHO_SET_SAMPLING_RATE, &mut rate as *mut c_int as *mut c_void);
            let mut k = 0;
            while (k + 1) * frame <= N {
                let a = k * frame;
                speex_echo_cancellation(st, mic[a..].as_ptr(), far[a..].as_ptr(), out[a..].as_mut_ptr());
                k += 1;
            }
            speex_echo_state_destroy(st);
        }
        let inp = frame * 20 + 100;
        let (mut p, mut pv) = (0usize, 0i16);
        for (i, v) in out.iter().enumerate() {
            if v.abs() > pv {
                pv = v.abs();
                p = i;
            }
        }
        println!(
            "## Algorithmische Verzoegerung: {} Samples = {:.2} ms (Spitze {} von 16384)\n",
            p as i64 - inp as i64,
            (p as i64 - inp as i64) as f64 * 1000.0 / SR as f64,
            pv
        );
    }

    println!("## Filterlaenge variieren (Frame 10 ms, linearer Lautsprecher)");
    println!("| filter_length [ms] | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] | DT SI-SDR [dB] | DT-Pausen-ERLE [dB] | CPU/Frame mean [us] |");
    println!("| --- | --- | --- | --- | --- | --- |");
    let sc = build_scene(80.0, 0.0, 0.0);
    for fl in [100.0f32, 200.0, 300.0, 500.0] {
        let (out, t) = run_speex(&sc, FRAME, fl);
        let (a, b) = steady_erle(&sc.mic, &out);
        let (s, _, p) = dt_metrics(&sc, &out);
        let mean = t.iter().sum::<f64>() / t.len() as f64;
        println!("| {:.0} | {:.1} | {:.1} | {:.1} | {:.1} | {:.0} |", fl, a, b, s, p, mean);
    }
    println!();

    println!("## Frame-Groesse variieren (filter_length 300 ms)");
    println!("| Frame [ms] | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] | CPU/Frame mean [us] | CPU-Anteil [% Kern] |");
    println!("| --- | --- | --- | --- | --- |");
    for (ms, fr) in [(10.0f64, FRAME), (20.0, FRAME * 2)] {
        let (out, t) = run_speex(&sc, fr, 300.0);
        let (a, b) = steady_erle(&sc.mic, &out);
        let mean = t.iter().sum::<f64>() / t.len() as f64;
        println!("| {:.0} | {:.1} | {:.1} | {:.0} | {:.2} |", ms, a, b, mean, mean / (ms * 1000.0) * 100.0);
    }
    println!();

    println!("## Lautsprecher-Nichtlinearitaet und Clock-Drift (Frame 10 ms, filter_length 300 ms)");
    println!("| Szenario | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] |");
    println!("| --- | --- | --- |");
    for (name, drive, ppm) in [
        ("linear, kein Drift", 0.0f32, 0.0f32),
        ("drive 5", 5.0, 0.0),
        ("drive 10", 10.0, 0.0),
        ("Drift 100 ppm", 0.0, 100.0),
        ("Drift 1000 ppm", 0.0, 1000.0),
        ("Drift 5000 ppm", 0.0, 5000.0),
    ] {
        let s = build_scene(80.0, drive, ppm);
        let (out, _) = run_speex(&s, FRAME, 300.0);
        let (a, b) = steady_erle(&s.mic, &out);
        println!("| {} | {:.1} | {:.1} |", name, a, b);
    }
    println!();

    println!("## Echopfad-Delay variieren (Frame 10 ms, filter_length 300 ms)");
    println!("| wahres Delay [ms] | ERLE 2-4 s [dB] | ERLE 8-10 s [dB] |");
    println!("| --- | --- | --- |");
    for d in [40.0f32, 80.0, 120.0, 200.0, 400.0] {
        let s = build_scene(d, 0.0, 0.0);
        let (out, _) = run_speex(&s, FRAME, 300.0);
        let (a, b) = steady_erle(&s.mic, &out);
        println!("| {:.0} | {:.1} | {:.1} |", d, a, b);
    }
}
