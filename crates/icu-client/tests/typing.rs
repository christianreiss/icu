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
    preroll::{self, PreRoll},
    testsignal::{voice, Rng, SR},
    vad,
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
}

/// The sender's gate path: chain → VAD → gate (attack `attack_ms`) → pre-roll.
fn run(samples: &[f32], attack_ms: u32) -> Run {
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
    let mut run = Run {
        sent: 0,
        starts: Vec::new(),
    };
    for b in 0..samples.len() / BLOCK {
        block.copy_from_slice(&samples[b * BLOCK..(b + 1) * BLOCK]);
        let now = t0 + BLOCK_DT * b as u32;
        let a = chain.analyze(&mut block, &settings);
        if let Some(p) = a.prob {
            auto.update(now, gate.is_open(), p);
        }
        let voice = vad::is_voice(auto.threshold(), a.prob, a.level);
        voice_in_frame |= voice;
        chain.finish(&mut block, voice, &settings);
        acc.extend_from_slice(&block);
        if acc.len() >= FRAME {
            let was_open = gate.is_open();
            let decision = gate.update(now, voice_in_frame);
            voice_in_frame = false;
            let sending = decision != Decision::Silent;
            if sending {
                let held = if !was_open && !prev_sent {
                    ring.take(gate.withheld() as usize + 1).len()
                } else {
                    0
                };
                if !was_open && !prev_sent {
                    run.starts.push(frame - held);
                }
                run.sent += held + 1;
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
    assert!(!run(&mic, 40).starts.is_empty(), "voice while typing lost");
}
