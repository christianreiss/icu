//! Offline probe: how much near-end speech survives AEC+RNNoise under
//! different reference conditions. Not a test — a diagnostic for the
//! "little arrives at the receiver" report (2026-09-21).
//!
//! `cargo run --release -p icu-client --features aec --example aec_probe`

use icu_client::dsp::{
    aec::Aec,
    denoise::Denoiser,
    testsignal::*,
    voice::{VoiceChain, VoiceStrength},
};

const SECS: usize = 8;
const N: usize = SR * SECS;
const FRAME: usize = 480;

fn run_case(name: &str, near: &[f32], reference: &[f32], use_aec: bool, use_denoise: bool) {
    run_case_voice(name, near, reference, use_aec, use_denoise, None);
}

/// Like `run_case`, with the voice optimizer after AEC and denoise, as in
/// the engine. `mean_prob` stays the pre-chain RNNoise probability.
fn run_case_voice(
    name: &str,
    near: &[f32],
    reference: &[f32],
    use_aec: bool,
    use_denoise: bool,
    voice: Option<VoiceStrength>,
) {
    let mut chain = voice.map(VoiceChain::new);
    let mut aec = Aec::new().expect("apm (build with --features aec)");
    let mut dn = Denoiser::new();
    let mut out = vec![0.0f32; N];
    let mut probs = Vec::with_capacity(N / FRAME);
    for k in 0..N / FRAME {
        let a = k * FRAME;
        let mut cap = near[a..a + FRAME].to_vec();
        if use_aec {
            aec.process(&reference[a..a + FRAME], &mut cap);
        }
        let p = if use_denoise { dn.process(&mut cap) } else { 0.0 };
        if let Some(c) = chain.as_mut() {
            c.process(&mut cap);
        }
        probs.push(p);
        out[a..a + FRAME].copy_from_slice(&cap);
    }
    // Skip the first 1s (filter warm-up / RNNoise warm block).
    let skip = SR;
    let rms_in = rms(&near[skip..]);
    let rms_out = rms(&out[skip..]);
    let db = 20.0 * (rms_out.max(1e-9) / rms_in.max(1e-9)).log10();
    let mean_prob: f32 = probs[skip / FRAME..].iter().sum::<f32>() / probs[skip / FRAME..].len() as f32;
    let frac_above_thr = probs[skip / FRAME..].iter().filter(|&&p| p >= 0.5).count() as f32
        / probs[skip / FRAME..].len() as f32;
    println!(
        "{name:<38} near_rms_in={rms_in:.4} out={rms_out:.4} gain={db:+.1} dB  mean_prob={mean_prob:.3}  frac(prob>=0.5)={frac_above_thr:.2}"
    );
}

fn main() {
    // Near-end: this user talking, no far-end voice active (single talk —
    // the common "I press PTT/VAD and talk" case).
    // `speechlike()` is filtered noise (no pitch), which RNNoise correctly
    // scores as low-probability voice — not representative of a human
    // talker. `harmonic_voice()` has real pitch/formant structure and is
    // what the project itself uses to stand in for a human voice
    // (`--test-tone`), so use it here as the near-end talker.
    let near = harmonic_voice(N, 0.05);
    let zeros = vec![0.0f32; N];

    run_case("(0) raw near-end, no AEC, no denoise", &near, &zeros, false, false);
    run_case("(1) near-end through DENOISE only", &near, &zeros, false, true);
    run_case("(a) near-end through AEC only, zero reference", &near, &zeros, true, false);
    run_case(
        "(a2) near-end through AEC+denoise, zero reference",
        &near,
        &zeros,
        true,
        true,
    );

    // (b) Reference = unrelated loud audio on the *wrong* sink (the bug:
    // AEC reference followed the default sink while playback — and thus
    // the real acoustic echo path — was on the explicitly chosen output
    // device). The reference is real audio but has nothing to do with what
    // is actually coming out of the user's speakers/headset.
    let wrong_sink_audio = speechlike(N, 0xBADC0DE, 5.1, 0.4, 0.2);
    run_case(
        "(b) loud UNCORRELATED reference (wrong sink)",
        &near,
        &wrong_sink_audio,
        true,
        true,
    );

    // (c) Correlated echo, correctly aligned — the case the existing unit
    // tests cover, included here as a sanity baseline.
    let far = speechlike(N, 0xC0FFEE, 3.3, 0.0, 0.1);
    let ir = room_ir(80.0, 0.5, 80.0, 0xBEEF);
    let echo = convolve(&far, &ir);
    let mic_with_echo: Vec<f32> = near.iter().zip(echo.iter()).map(|(n, e)| n + e).collect();
    run_case(
        "(c) correlated echo, correctly aligned",
        &mic_with_echo,
        &far,
        true,
        true,
    );

    // (d) Voice optimizer after AEC+denoise. Near-end level rises by the
    // makeup/compression; mean_prob must match (c) because the chain runs
    // after the probability is taken.
    for strength in [VoiceStrength::Medium, VoiceStrength::Strong] {
        run_case_voice(
            &format!("(d) (c) + voice optimizer {strength:?}"),
            &mic_with_echo,
            &far,
            true,
            true,
            Some(strength),
        );
    }

    // (e) Far-end only: residual echo after AEC+denoise, without and with
    // the optimizer. The optimizer may lift it by at most its makeup gain.
    run_case("(e) far-end only (echo), AEC+denoise", &echo, &far, true, true);
    for strength in [VoiceStrength::Medium, VoiceStrength::Strong] {
        run_case_voice(
            &format!("(e) far-end only + voice optimizer {strength:?}"),
            &echo,
            &far,
            true,
            true,
            Some(strength),
        );
    }
}
