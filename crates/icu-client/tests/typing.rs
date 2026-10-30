//! Typing next to the microphone (`docs/reference/client.md` §6): key clicks
//! must not open the VAD gate, voice must, and the start of a word must
//! survive the attack time. Offline and deterministic, like `cascade.rs`:
//! the real chain, auto threshold, gate and pre-roll ring on a synthetic
//! microphone. The clicks are decaying noise bursts, a stand-in for a
//! keyboard; how the real thing scores with RNNoise is for a live test
//! (`docs/development/testing.md`).

use std::time::{Duration, Instant};

use icu_client::dsp::{
    chain::{SendChain, SendSettings},
    gate::{Decision, Gate, Mode},
    meter,
    opus::{Decoder, Encoder},
    preroll::{self, PreRoll},
    testsignal::{voice, Rng, SR},
    vad_auto::AutoThreshold,
};

const BLOCK: usize = 480;
const FRAME: usize = 960;
const BLOCK_DT: Duration = Duration::from_millis(10);

/// What the gate did with a microphone signal.
struct Run {
    /// Frames sent, pre-roll included.
    sent: usize,
    /// Index of the first frame of each spurt, pre-roll included.
    starts: Vec<usize>,
    decoded: Vec<f32>,
    sent_at: Vec<bool>,
}

/// The sender's gate path: chain → VAD → gate (attack `attack_ms`) → pre-roll.
fn run(samples: &[f32], attack_ms: u32) -> Run {
    run_with(samples, attack_ms, true)
}
fn run_with(samples: &[f32], attack_ms: u32, attenuate: bool) -> Run {
    let t0 = Instant::now();
    let settings = SendSettings::default();
    let mut chain = SendChain::new();
    let mut auto = AutoThreshold::new(t0);
    let mut gate = Gate::new(
        Mode::Vad,
        Duration::from_millis(150),
        Duration::from_millis(1000),
    );
    gate.set_attack(Duration::from_millis(attack_ms as u64));
    let mut ring = PreRoll::default();
    let mut block = vec![0.0f32; BLOCK];
    let mut acc: Vec<f32> = Vec::with_capacity(FRAME);
    let mut voice_in_frame = false;
    let mut prev_sent = false;
    let mut frame = 0usize;
    let mut encoder = Encoder::new().unwrap();
    let mut decoder = Decoder::new().unwrap();
    let mut run = Run {
        sent: 0,
        starts: Vec::new(),
        decoded: vec![0.0; samples.len()],
        sent_at: vec![false; samples.len() / FRAME],
    };
    for b in 0..samples.len() / BLOCK {
        block.copy_from_slice(&samples[b * BLOCK..(b + 1) * BLOCK]);
        let now = t0 + BLOCK_DT * b as u32;
        let a = chain.analyze(&mut block, &settings);
        let (_, voice) = a.classify(&mut auto, now, gate.is_open(), None);
        voice_in_frame |= voice;
        if attenuate {
            chain.finish_vad(&mut block, voice, &settings);
        } else {
            chain.finish(&mut block, voice, &settings);
        }
        acc.extend_from_slice(&block);
        if acc.len() >= FRAME {
            let was_open = gate.is_open();
            let decision = gate.update_audio(now, voice_in_frame, Duration::from_millis(20));
            voice_in_frame = false;
            let sending = decision != Decision::Silent;
            if sending {
                let held = if !was_open && !prev_sent {
                    ring.take(gate.withheld() as usize + 1)
                } else {
                    Vec::new()
                };
                if !was_open && !prev_sent {
                    run.starts.push(frame - held.len());
                }
                run.sent += held.len() + 1;
                for (index, pcm) in held
                    .iter()
                    .map(Vec::as_slice)
                    .chain(std::iter::once(&acc[..FRAME]))
                    .enumerate()
                {
                    let at = frame - held.len() + index;
                    let packet = encoder.encode(pcm).unwrap();
                    decoder
                        .decode(packet, &mut run.decoded[at * FRAME..(at + 1) * FRAME])
                        .unwrap();
                    run.sent_at[at] = true;
                }
                ring.clear();
            } else {
                ring.push(&acc[..FRAME], preroll::depth(attack_ms * 2, 20));
            }
            prev_sent = sending;
            frame += 1;
            acc.drain(..FRAME);
        }
    }
    run
}

/// Room noise at about −60 dBFS with a key click every ~110 ms: 8 ms of
/// exponentially decaying noise.
fn clicks(secs: f32, seed: u64) -> Vec<f32> {
    let n = (secs * SR as f32) as usize;
    let mut rng = Rng::new(seed);
    let noise = meter::from_db(-60.0) * 3f32.sqrt();
    let mut out: Vec<f32> = (0..n).map(|_| rng.uniform() * noise).collect();
    let mut at = SR / 2;
    while at + 600 < n {
        for i in 0..384 {
            let env = (-(i as f32) / 70.0).exp();
            out[at + i] += rng.uniform() * 0.4 * env;
        }
        at += 4800 + (rng.next_u64() % 1600) as usize;
    }
    out
}

#[test]
fn a_typing_burst_does_not_open_the_gate() {
    let mic = clicks(6.0, 7);
    let plain = run(&mic, 0);
    let filtered = run(&mic, 40);
    eprintln!(
        "frames sent: {} without the attack, {} with it",
        plain.sent, filtered.sent
    );
    assert_eq!(filtered.sent, 0, "clicks sent with the 40 ms attack");
}

#[test]
fn voice_still_opens_the_gate_and_its_start_is_kept() {
    let v = voice(SR * 8, 3, 0.1);
    let plain = run(&v.samples, 0);
    let filtered = run(&v.samples, 40);
    assert!(!plain.starts.is_empty(), "the voice never opened the gate");
    assert_eq!(
        filtered.starts.len(),
        plain.starts.len(),
        "same spurts with and without the attack"
    );
    for (f, p) in filtered.starts.iter().zip(&plain.starts) {
        assert!(
            f <= p,
            "the attack cost the start of a spurt: frame {f} vs {p}"
        );
    }
}

#[test]
fn voice_over_typing_is_sent() {
    let mut mic = clicks(8.0, 11);
    let v = voice(SR * 8, 5, 0.1);
    for (m, s) in mic.iter_mut().zip(&v.samples) {
        *m += s;
    }
    let received = run(&mic, 40);
    let expected = run(&v.samples, 40);
    let mut covered = 0;
    let mut judged = 0;
    for (i, loud) in v.loud.iter().enumerate() {
        if *loud && i >= 100 {
            judged += 1;
            let frame = i / 2;
            if received.sent_at[frame]
                && meter::measure(&received.decoded[i * BLOCK..(i + 1) * BLOCK]).rms_db > -45.0
            {
                covered += 1;
            }
        }
    }
    assert!(judged > 100);
    assert!(
        covered as f32 / judged as f32 >= 0.90,
        "speech coverage {covered}/{judged}"
    );
    let source_level = meter::measure(&expected.decoded[SR..]).rms_db;
    let mixed_level = meter::measure(&received.decoded[SR..]).rms_db;
    assert!(
        (source_level - mixed_level).abs() < 6.0,
        "speech+typing level {mixed_level}, clean {source_level}"
    );
}

#[test]
fn typing_after_speech_is_quieter_in_the_encoded_hangover() {
    let speech = voice(SR * 3, 23, 0.05);
    let tail = clicks(2.0, 31);
    let mut microphone = speech.samples;
    microphone.extend_from_slice(&tail);
    let baseline = run_with(&microphone, 40, false);
    let protected = run_with(&microphone, 40, true);
    let start = SR * 3 + SR / 3;
    let end = SR * 4;
    let before = meter::measure(&baseline.decoded[start..end]).rms_db;
    let after = meter::measure(&protected.decoded[start..end]).rms_db;
    eprintln!("post-speech typing decoded RMS: {before:.1} -> {after:.1}dBFS");
    assert!(
        after <= before - 4.0 || after <= -70.0,
        "residual typing not attenuated"
    );
    let voice_before = meter::measure(&baseline.decoded[SR..SR * 3]).rms_db;
    let voice_after = meter::measure(&protected.decoded[SR..SR * 3]).rms_db;
    assert!(
        (voice_before - voice_after).abs() < 1.0,
        "speech changed by suppression"
    );
}

/// The checked-in fixtures are licensed, 48 kHz mono PCM16 RIFF files.
fn pcm16_wav(bytes: &[u8]) -> Vec<f32> {
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let count = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = &bytes[offset + 8..offset + 8 + count];
        if &bytes[offset..offset + 4] == b"fmt " {
            assert_eq!(u16::from_le_bytes(body[..2].try_into().unwrap()), 1);
            assert_eq!(u16::from_le_bytes(body[2..4].try_into().unwrap()), 1);
            assert_eq!(u32::from_le_bytes(body[4..8].try_into().unwrap()), 48_000);
            assert_eq!(u16::from_le_bytes(body[14..16].try_into().unwrap()), 16);
        }
        if &bytes[offset..offset + 4] == b"data" {
            return body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|sample| f32::from(i16::from_le_bytes(*sample)) / 32768.0)
                .collect();
        }
        offset += 8 + count + (count & 1);
    }
    panic!("missing WAV data")
}

#[test]
fn recorded_speech_with_a_recorded_keyboard_keeps_the_active_words() {
    let speech: Vec<f32> = pcm16_wav(include_bytes!("fixtures/audio/voices-sp0307.wav"))
        .into_iter()
        .map(|x| x * 0.4)
        .collect();
    let keyboard = pcm16_wav(include_bytes!("fixtures/audio/keychron-v1.wav"));
    let mixed: Vec<f32> = speech
        .iter()
        .zip(keyboard.iter().cycle())
        .map(|(voice, key)| voice + key)
        .collect();
    let clean = run_with(&speech, 40, false);
    let noisy = run(&mixed, 40);
    let levels: Vec<f32> = speech
        .as_chunks::<BLOCK>()
        .0
        .iter()
        .map(|block| meter::measure(block).rms_db)
        .collect();
    let loudest = levels.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let (mut judged, mut covered, mut gap, mut longest_gap) = (0, 0, 0, 0);
    let (mut clean_energy, mut mixed_energy) = (0.0f64, 0.0f64);
    for (block, level) in levels.iter().enumerate() {
        if *level < loudest - 18.0 {
            gap = 0;
            continue;
        }
        judged += 1;
        // Same codec/denoiser delay in both runs; permit a 20 ms alignment
        // range against the independent source activity mask.
        let start = block.saturating_sub(1) * BLOCK;
        let end = ((block + 3) * BLOCK).min(speech.len());
        if meter::measure(&noisy.decoded[start..end]).rms_db > -48.0 {
            covered += 1;
            gap = 0;
        } else {
            gap += 1;
            longest_gap = longest_gap.max(gap);
        }
        clean_energy += clean.decoded[start..end]
            .iter()
            .map(|x| f64::from(*x).powi(2))
            .sum::<f64>();
        mixed_energy += noisy.decoded[start..end]
            .iter()
            .map(|x| f64::from(*x).powi(2))
            .sum::<f64>();
    }
    let level_delta = 10.0 * (mixed_energy / clean_energy.max(1e-12)).log10();
    eprintln!("recorded speech+keyboard: {covered}/{judged} active blocks, longest missing {longest_gap}0ms, level delta {level_delta:.1}dB");
    assert!(judged > 100);
    assert!(
        covered as f32 / judged as f32 >= 0.95,
        "recorded words lost"
    );
    assert!(longest_gap <= 2, "recorded speech has >20ms dropout");
    assert!(
        level_delta.abs() < 4.0,
        "recorded voice level changed: {level_delta:.1}dB"
    );
}

#[test]
fn recorded_keyboard_after_speech_is_attenuated_through_opus() {
    let mut speech = pcm16_wav(include_bytes!("fixtures/audio/voices-sp0307.wav"));
    for sample in &mut speech {
        *sample *= 0.4;
    }
    let voice_end = speech.len();
    let keyboard = pcm16_wav(include_bytes!("fixtures/audio/keychron-v1.wav"));
    speech.extend_from_slice(&keyboard[..SR * 2]);
    let plain = run_with(&speech, 40, false);
    let protected = run(&speech, 40);
    let start = voice_end + SR / 4;
    let end = voice_end + SR;
    let before = meter::measure(&plain.decoded[start..end]).rms_db;
    let after = meter::measure(&protected.decoded[start..end]).rms_db;
    eprintln!("recorded post-speech keyboard decoded: {before:.1} -> {after:.1}dBFS");
    assert!(
        after <= before - 4.0 || after <= -70.0,
        "recorded keyboard not attenuated"
    );
    let voice_before = meter::measure(&plain.decoded[..voice_end]).rms_db;
    let voice_after = meter::measure(&protected.decoded[..voice_end]).rms_db;
    assert!(
        (voice_before - voice_after).abs() < 1.0,
        "recorded speech harmed"
    );
}
