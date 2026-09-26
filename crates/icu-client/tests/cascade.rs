//! The default audio chain end to end, offline and deterministic
//! (`Client/IMPLEMENTATION.md` §6): microphone → `dsp::chain` (mic auto
//! level, RNNoise, VAD with the auto threshold, outbound normalizer and
//! limiter) → VAD gate with pre-roll → Opus → decoder (PLC/FEC where a test
//! loses packets) → receive normalizer → per-user volume → mixer and soft
//! limiter. What the listener hears is measured against the speaker's
//! microphone. No hardware, no threads, a synthetic clock; the voice is
//! `testsignal::voice`, which RNNoise treats like speech.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use icu_client::dsp::{
    chain::{SendChain, SendSettings},
    gate::{Decision, Gate, Mode},
    meter, mixer,
    normalize::Normalizer,
    opus,
    testsignal::{rms, voice, Rng, SR},
    vad,
    vad_auto::AutoThreshold,
};

const BLOCK: usize = 480;
const FRAME: usize = 960;
/// `AudioSettings::default()`: hangover and release.
const HANGOVER: Duration = Duration::from_millis(1000);
const RELEASE: Duration = Duration::from_millis(150);
const BLOCK_DT: Duration = Duration::from_millis(10);

/// A microphone signal and the truth about it.
struct Mic {
    samples: Vec<f32>,
    /// Per block: the speaker is talking and the block is a loud vowel —
    /// where the speech level is measured.
    loud: Vec<bool>,
}

/// A microphone signal, block by block: the synthetic voice
/// (`testsignal::voice`) at the level `speech_db(t)` returns for the
/// block's time (RMS in dBFS over loud vowel blocks; `None` = the speaker
/// is quiet) over uniform room noise at `noise_db`. `seed` picks the voice
/// and the noise.
fn microphone(secs: f32, seed: u64, noise_db: f32, speech_db: impl Fn(f32) -> Option<f32>) -> Mic {
    let n = (secs * SR as f32) as usize;
    let src = voice(n, seed, 0.1);
    // Level reference: the RMS over the loud vowel blocks of the source.
    let voiced_ref = {
        let mut acc = 0.0f64;
        let mut cnt = 0usize;
        for b in 0..n / BLOCK {
            if src.loud[b] {
                let r = rms(&src.samples[b * BLOCK..(b + 1) * BLOCK]);
                acc += (r as f64) * (r as f64);
                cnt += 1;
            }
        }
        (acc / cnt as f64).sqrt() as f32
    };
    let mut rng = Rng::new(seed);
    let noise_g = meter::from_db(noise_db) * 3f32.sqrt();
    let mut out = vec![0.0f32; n];
    let mut talking = Vec::with_capacity(n / BLOCK);
    for b in 0..n / BLOCK {
        let t = b as f32 * BLOCK as f32 / SR as f32;
        let level = speech_db(t);
        let g = level.map_or(0.0, |db| meter::from_db(db) / voiced_ref);
        let range = b * BLOCK..(b + 1) * BLOCK;
        for (o, s) in out[range.clone()].iter_mut().zip(&src.samples[range]) {
            *o = s * g + rng.uniform() * noise_g;
        }
        talking.push(level.is_some() && src.loud[b]);
    }
    Mic {
        samples: out,
        loud: talking,
    }
}

/// What a sender put on the wire: the packets per 20 ms frame slot (none
/// while the gate is closed, two when a spurt opens with its pre-roll),
/// and the gains of its two normalizers per block.
struct Stream {
    slots: Vec<Vec<Vec<u8>>>,
    /// (leveler, outbound) gain in dB after every block.
    gains: Vec<(f32, f32)>,
    /// Blocks on which the VAD heard voice.
    voice: Vec<bool>,
    /// Per block: level after the first half (post denoise, what the meter
    /// shows) and after the second half (what goes to the encoder), dBFS.
    levels: Vec<(f32, f32)>,
}

/// The sender: everything the DSP thread does after the AEC, with the
/// default settings, in VAD mode with the auto-calibrated threshold; the
/// encoder carries `loss_permille` as a `LossHint` would set it.
fn send(mic: &Mic, settings: &SendSettings, loss_permille: u16) -> Stream {
    let t0 = Instant::now();
    let mut chain = SendChain::new();
    let mut auto = AutoThreshold::new(t0);
    let mut gate = Gate::new(Mode::Vad, RELEASE, HANGOVER);
    let mut encoder = opus::Encoder::new().unwrap();
    encoder.set_loss_permille(loss_permille).unwrap();
    let mut acc: Vec<f32> = Vec::with_capacity(FRAME);
    let mut prev_frame: Option<Vec<f32>> = None;
    let mut prev_sent = false;
    let mut voice_in_frame = false;
    let mut stream = Stream {
        slots: Vec::new(),
        gains: Vec::new(),
        voice: Vec::new(),
        levels: Vec::new(),
    };
    let mut block = vec![0.0f32; BLOCK];
    for b in 0..mic.samples.len() / BLOCK {
        block.copy_from_slice(&mic.samples[b * BLOCK..(b + 1) * BLOCK]);
        let now = t0 + BLOCK_DT * b as u32;
        let a = chain.analyze(&mut block, settings);
        if let Some(p) = a.prob {
            auto.update(now, gate.is_open(), p);
        }
        let voice = vad::is_voice(auto.threshold(), a.prob, a.level);
        voice_in_frame |= voice;
        chain.finish(&mut block, voice, settings);
        stream.gains.push(chain.gains_db());
        stream.voice.push(voice);
        stream
            .levels
            .push((a.level.rms_db, meter::to_db(rms(&block))));
        acc.extend_from_slice(&block);
        if acc.len() >= FRAME {
            let was_open = gate.is_open();
            let decision = gate.update(now, voice_in_frame);
            voice_in_frame = false;
            let sending = decision != Decision::Silent;
            let mut slot = Vec::new();
            if sending {
                if !was_open && !prev_sent {
                    if let Some(prev) = prev_frame.as_ref() {
                        slot.push(encoder.encode(prev).unwrap().to_vec());
                    }
                }
                slot.push(encoder.encode(&acc[..FRAME]).unwrap().to_vec());
            }
            stream.slots.push(slot);
            prev_frame = Some(acc[..FRAME].to_vec());
            prev_sent = sending;
            acc.drain(..FRAME);
        }
    }
    stream
}

/// What happens to a frame slot on the way.
#[derive(Clone, Copy, PartialEq)]
enum Loss {
    /// Arrives.
    None,
    /// Lost; the next packet is there, so its in-band FEC recovers it.
    Fec,
    /// Lost without a successor in time: packet loss concealment.
    Plc,
}

/// One speaker on the listener's side: decoder, receive normalizer,
/// volume — what the mixer thread does per speaker, per 10 ms block.
/// Returns what this speaker contributes to the mix (before the mixer's
/// limiter), block-aligned with the stream, and the receive gain per block.
fn receive(stream: &Stream, loss: impl Fn(usize) -> Loss, volume: f32) -> (Vec<f32>, Vec<f32>) {
    let mut decoder = opus::Decoder::new().unwrap();
    let mut norm = Normalizer::new();
    let mut pending: VecDeque<f32> = VecDeque::new();
    let mut scratch = vec![0.0f32; 5760];
    let mut heard = Vec::with_capacity(stream.slots.len() * FRAME);
    let mut gains = Vec::with_capacity(stream.slots.len() * 2);
    let mut out = vec![0.0f32; BLOCK];
    let gain = mixer::volume_gain(volume);
    // Samples queued from a concealed or recovered frame: the mixer holds
    // the normalizer on those (`engine::mixer_thread`).
    let mut concealed = 0usize;
    for (k, slot) in stream.slots.iter().enumerate() {
        for half in 0..2 {
            if half == 1 {
                // The slot closes on the sender: what arrives.
                match loss(k) {
                    Loss::None if slot.is_empty() => {
                        // Nothing arrived: the jitter buffer plays what it
                        // still holds (the pre-roll's extra frame), then
                        // silence.
                        let short = FRAME.saturating_sub(pending.len());
                        pending.extend(std::iter::repeat_n(0.0, short));
                    }
                    Loss::None => {
                        for p in slot {
                            let n = decoder.decode(p, &mut scratch).unwrap();
                            pending.extend(scratch[..n].iter().copied());
                        }
                    }
                    Loss::Fec => {
                        let next = stream.slots[k + 1].first().expect("successor");
                        let n = decoder.decode_fec(next, &mut scratch).unwrap();
                        pending.extend(scratch[..n].iter().copied());
                        concealed = pending.len();
                    }
                    Loss::Plc => {
                        let n = decoder.conceal(&mut scratch).unwrap();
                        pending.extend(scratch[..n].iter().copied());
                        concealed = pending.len();
                    }
                }
            }
            if pending.len() >= BLOCK {
                for (dst, src) in out.iter_mut().zip(pending.drain(..BLOCK)) {
                    *dst = src;
                }
                let hint = if concealed > 0 { Some(false) } else { None };
                concealed = concealed.saturating_sub(BLOCK);
                norm.process_hinted(&mut out, hint);
                for s in out.iter_mut() {
                    *s *= gain;
                }
            } else {
                out.fill(0.0);
            }
            gains.push(norm.gain_db());
            heard.extend_from_slice(&out);
        }
    }
    (heard, gains)
}

/// Mixes the speakers' contributions through the mixer's soft limiter.
fn mix(speakers: &[&[f32]]) -> Vec<f32> {
    let n = speakers.iter().map(|s| s.len()).min().unwrap_or(0);
    let mut out = vec![0.0f32; n];
    let mut block = vec![0.0f32; BLOCK];
    for b in 0..n / BLOCK {
        mixer::mix(
            speakers
                .iter()
                .map(|s| (&s[b * BLOCK..(b + 1) * BLOCK], 1.0)),
            &mut block,
        );
        out[b * BLOCK..(b + 1) * BLOCK].copy_from_slice(&block);
    }
    out
}

/// The whole path for one speaker at `volume` percent, nothing lost.
fn one_speaker(mic: &Mic, volume: f32) -> (Vec<f32>, Stream, Vec<f32>) {
    let stream = send(mic, &SendSettings::default(), 0);
    let (part, gains) = receive(&stream, |_| Loss::None, volume);
    let heard = mix(&[&part]);
    (heard, stream, gains)
}

/// RMS in dB per block.
fn block_levels(x: &[f32]) -> Vec<f32> {
    x.as_chunks::<BLOCK>()
        .0
        .iter()
        .map(|b| meter::to_db(rms(b)))
        .collect()
}

/// The chain's delay in blocks: the lag at which the heard block levels
/// follow the microphone's best.
fn lag_blocks(mic: &[f32], heard: &[f32]) -> usize {
    let a = block_levels(mic);
    let b = block_levels(heard);
    let score = |lag: usize| -> f64 {
        a.iter()
            .zip(b[lag..].iter())
            .map(|(p, q)| (*p as f64) * (*q as f64))
            .sum::<f64>()
    };
    (0..8)
        .max_by(|&x, &y| score(x).total_cmp(&score(y)))
        .unwrap()
}

/// RMS in dB of `x[from_s..to_s]`.
fn level_db(x: &[f32], from_s: f32, to_s: f32) -> f32 {
    let a = (from_s * SR as f32) as usize;
    let b = ((to_s * SR as f32) as usize).min(x.len());
    meter::to_db(rms(&x[a..b]))
}

/// RMS in dB over the loud vowel blocks in `from_s..to_s`; `lag` shifts
/// the signal against the mask.
fn voiced_level_db(x: &[f32], loud: &[bool], lag: usize, from_s: f32, to_s: f32) -> f32 {
    let a = (from_s * SR as f32) as usize / BLOCK;
    let b = ((to_s * SR as f32) as usize / BLOCK).min(x.len() / BLOCK - lag);
    let mut acc = 0.0f64;
    let mut cnt = 0usize;
    for blk in a..b {
        if loud[blk] {
            let r = rms(&x[(blk + lag) * BLOCK..(blk + lag + 1) * BLOCK]);
            acc += (r as f64) * (r as f64);
            cnt += 1;
        }
    }
    meter::to_db((acc / cnt.max(1) as f64).sqrt() as f32)
}

/// Peak in dBFS.
fn peak_db(x: &[f32]) -> f32 {
    meter::to_db(x.iter().fold(0.0f32, |m, s| m.max(s.abs())))
}

/// The largest rise of a gain trajectory (one value per block) within
/// any window of `blocks`.
fn fastest_rise_db(gain: &[f32], blocks: usize) -> f32 {
    gain.windows(blocks + 1)
        .map(|w| w[blocks] - w.iter().cloned().fold(f32::MAX, f32::min))
        .fold(0.0f32, f32::max)
}

/// Direction changes per second of a gain trajectory (one value per
/// block), ignoring moves under `dead_band` dB.
fn direction_changes_per_s(gain: &[f32], dead_band: f32) -> f32 {
    let mut last = gain[0];
    let mut dir = 0i8;
    let mut changes = 0usize;
    for &g in &gain[1..] {
        let d = g - last;
        if d.abs() < dead_band {
            continue;
        }
        let now = d.signum() as i8;
        if dir != 0 && now != dir {
            changes += 1;
        }
        dir = now;
        last = g;
    }
    changes as f32 / (gain.len() as f32 / 100.0)
}

/// Total gain per block: sender stages plus the receive stage, the latter
/// shifted back by the chain's lag.
fn total_gain(stream: &Stream, rx_gains: &[f32], lag: usize) -> Vec<f32> {
    stream
        .gains
        .iter()
        .zip(rx_gains[lag..].iter())
        .map(|((a, b), c)| a + b + c)
        .collect()
}

/// (a) A quiet and a loud speaker land within 3 dB of each other, both
/// near the −22 dBFS target; (c) neither one's gain hunts.
#[test]
fn quiet_and_loud_speakers_arrive_at_the_same_level() {
    let quiet = microphone(20.0, 11, -55.0, |_| Some(-40.0));
    let loud = microphone(20.0, 12, -55.0, |_| Some(-10.0));
    let mut levels = Vec::new();
    for (name, mic) in [("−40 dBFS", &quiet), ("−10 dBFS", &loud)] {
        let (heard, stream, rx_gains) = one_speaker(mic, 100.0);
        let lag = lag_blocks(&mic.samples, &heard);
        let early = voiced_level_db(&heard, &mic.loud, lag, 2.0, 4.0);
        let steady = voiced_level_db(&heard, &mic.loud, lag, 12.0, 20.0);
        let gain = total_gain(&stream, &rx_gains, lag);
        let settle = &gain[1200..];
        let changes = direction_changes_per_s(settle, 0.1);
        let swing = settle.iter().cloned().fold(f32::MIN, f32::max)
            - settle.iter().cloned().fold(f32::MAX, f32::min);
        let rise = fastest_rise_db(settle, 50);
        let (mic_g, out_g) = stream.gains[stream.gains.len() - 1];
        eprintln!(
            "{name}: heard {steady:.1} dBFS (voiced, 12–20 s; {early:.1} at 2–4 s), lag {lag} blocks, \
             gains mic {mic_g:.1} out {out_g:.1} rx {:.1} dB, {changes:.1} direction changes/s, \
             swing {swing:.1} dB, fastest rise {rise:.1} dB per 0.5 s",
            rx_gains[rx_gains.len() - 1]
        );
        // Speech has a few syllables per second; a normalizer that follows
        // them turns at most twice per syllable. Anything faster is the
        // stages hunting each other. And the gain only ever comes back at
        // the slow release: a rise of more than 2 dB within half a second
        // in a steady spurt is pumping.
        assert!(changes <= 8.0, "{name}: gain hunts, {changes:.1} changes/s");
        assert!(
            rise <= 2.0,
            "{name}: gain pumps, rises {rise:.1} dB within 0.5 s in steady state"
        );
        levels.push(steady);
    }
    let diff = (levels[0] - levels[1]).abs();
    assert!(diff <= 3.0, "levels differ by {diff:.1} dB: {levels:?}");
    for l in levels {
        assert!(
            (-28.0..=-18.0).contains(&l),
            "heard {l:.1} dBFS, target −22"
        );
    }
}

/// (b) In a pause the gain holds: the room noise the listener hears does
/// not creep up, and once the gate closes the output is digital silence.
/// (d) The spurt after the pause starts at the level it ends with.
#[test]
fn pauses_hold_the_gain_and_spurts_start_clean() {
    // Talk 0–6 s, quiet 6–13 s (noise only), talk 13–20 s.
    let mic = microphone(20.0, 21, -55.0, |t| {
        if (6.0..13.0).contains(&t) {
            None
        } else {
            Some(-30.0)
        }
    });
    let (heard, stream, rx_gains) = one_speaker(&mic, 100.0);
    let lag = lag_blocks(&mic.samples, &heard);
    let gain = total_gain(&stream, &rx_gains, lag);
    // The gate stays open for the hangover (1 s) after the last voice.
    let last_voice = stream.voice[..1300].iter().rposition(|&v| v).unwrap();
    let closed = last_voice + 100 + 2;
    let (g_start, g_end) = (gain[last_voice + 10], gain[closed - 1]);
    let secs = |b: usize| (b + lag) as f32 / 100.0;
    let noise_start = level_db(&heard, secs(last_voice + 10), secs(last_voice + 40));
    let noise_end = level_db(&heard, secs(closed - 30), secs(closed));
    eprintln!(
        "pause: gain {g_start:.1} → {g_end:.1} dB, heard noise {noise_start:.1} → {noise_end:.1} dBFS, \
         last voice block {last_voice}, gate closed at {closed}"
    );
    assert!(
        g_end <= g_start + 0.5,
        "gain rose in the pause: {g_start:.1} → {g_end:.1} dB"
    );
    assert!(
        noise_end <= noise_start + 3.0,
        "noise pumped up in the pause: {noise_start:.1} → {noise_end:.1} dBFS"
    );
    // Digital silence stays silent: from a little after the gate closed
    // until the next spurt, every sample is exactly zero.
    let silent = &heard[(closed + lag + 20) * BLOCK..(1300 - 5) * BLOCK];
    assert!(
        silent.iter().all(|&s| s == 0.0),
        "listener output during a closed gate is not silent: peak {:.1} dBFS",
        peak_db(silent)
    );
    // (d) The first 50 ms of the new spurt against 1–3 s into it: the gain
    // the listener's level shows over the microphone's, each an RMS over
    // the loud blocks of its window, so the syllables' own loudness cancels.
    let first_loud = (1300..).find(|&b| mic.loud[b]).unwrap();
    let secs_at = |b: usize| b as f32 / 100.0;
    let effective = |from: usize, to: usize| {
        voiced_level_db(&heard, &mic.loud, lag, secs_at(from), secs_at(to))
            - voiced_level_db(&mic.samples, &mic.loud, 0, secs_at(from), secs_at(to))
    };
    let onset = effective(first_loud, first_loud + 5);
    let steady = effective(first_loud + 100, first_loud + 300);
    for b in [
        last_voice,
        closed - 1,
        first_loud,
        first_loud + 100,
        first_loud + 300,
    ] {
        eprintln!(
            "  block {b}: gains mic {:.1} out {:.1} rx {:.1}",
            stream.gains[b].0,
            stream.gains[b].1,
            rx_gains[b + lag]
        );
    }
    eprintln!(
        "onset after the pause: gain {onset:.1} dB over the first 50 ms, {steady:.1} dB steady"
    );
    assert!(
        onset <= steady + 3.0,
        "spurt onset overshoots: {onset:.1} dB vs steady {steady:.1} dB"
    );
    // The level the listener gets after the pause is the one from before.
    let before = voiced_level_db(&heard, &mic.loud, lag, 3.0, 6.0);
    let after = voiced_level_db(&heard, &mic.loud, lag, 13.0, 16.0);
    eprintln!("before the pause {before:.1} dBFS, after {after:.1} dBFS");
    assert!(
        (before - after).abs() <= 3.0,
        "level changed over the pause: {before:.1} → {after:.1}"
    );
}

/// (e) Frames the decoder concealed (PLC) or recovered from in-band FEC do
/// not move the receive normalizer.
#[test]
fn concealed_frames_are_not_boosted() {
    let mic = microphone(10.0, 31, -55.0, |_| Some(-22.0));
    // A LossHint of 10 % makes the encoder add FEC.
    let stream = send(&mic, &SendSettings::default(), 100);
    let plc = 200..205; // 4.0–4.1 s: five frames concealed
    let fec = 300; // 6.0 s: one frame recovered from packet 301
    let (with_loss, gains) = receive(
        &stream,
        |k| {
            if plc.contains(&k) {
                Loss::Plc
            } else if k == fec {
                Loss::Fec
            } else {
                Loss::None
            }
        },
        100.0,
    );
    let (clean, clean_gains) = receive(&stream, |_| Loss::None, 100.0);
    // Against the clean run: the same stream without the loss, so what the
    // normalizer does anyway (still converging) is not blamed on the PLC.
    let after = plc.end * 2 + 1;
    let plc_level = level_db(&with_loss, 4.0, 4.1);
    let clean_level = level_db(&clean, 4.0, 4.1);
    eprintln!(
        "PLC burst: receive gain {:.2} dB after it, {:.2} dB without the loss; \
         heard {plc_level:.1} dBFS where the clean stream has {clean_level:.1}",
        gains[after], clean_gains[after]
    );
    assert!(
        gains[after] <= clean_gains[after] + 0.1,
        "PLC raised the receive gain to {:.2} dB ({:.2} without the loss)",
        gains[after],
        clean_gains[after]
    );
    assert!(
        plc_level <= clean_level + 1.0,
        "concealed frames louder than the real ones"
    );
    let after = fec * 2 + 2;
    let fec_level = level_db(&with_loss, 6.0, 6.02);
    let ref_level = level_db(&clean, 6.0, 6.02);
    eprintln!(
        "FEC frame: receive gain {:.2} dB after it, {:.2} dB without the loss; heard {fec_level:.1} dBFS vs {ref_level:.1} clean",
        gains[after], clean_gains[after]
    );
    assert!(
        (gains[after] - clean_gains[after]).abs() <= 0.1,
        "FEC moved the receive gain"
    );
    assert!(
        (fec_level - ref_level).abs() <= 3.0,
        "FEC frame level off by more than 3 dB"
    );
}

/// (f) Two and four loud speakers at once: the mix never exceeds 0 dBFS,
/// and the soft limiter's distortion stays small — at the default volume,
/// where it hardly ever engages, and with every speaker turned up to
/// 200 %, where it works on every loud syllable.
#[test]
fn simultaneous_loud_speakers_do_not_clip() {
    let streams: Vec<Stream> = (0..4)
        .map(|i| {
            let mic = microphone(8.0, 41 + i, -55.0, |_| Some(-10.0));
            send(&mic, &SendSettings::default(), 0)
        })
        .collect();
    for (n, volume) in [(2usize, 100.0f32), (4, 100.0), (2, 200.0), (4, 200.0)] {
        let parts: Vec<Vec<f32>> = streams[..n]
            .iter()
            .map(|s| receive(s, |_| Loss::None, volume).0)
            .collect();
        let inputs: Vec<&[f32]> = parts.iter().map(|p| p.as_slice()).collect();
        let out = mix(&inputs);
        // The linear sum the limiter worked on, and its best linear fit.
        let len = out.len();
        let sum: Vec<f32> = (0..len)
            .map(|i| inputs.iter().map(|p| p[i]).sum())
            .collect();
        let tail = 3 * SR..len;
        let dot: f64 = tail.clone().map(|i| out[i] as f64 * sum[i] as f64).sum();
        let ss: f64 = tail.clone().map(|i| (sum[i] as f64).powi(2)).sum();
        let k = dot / ss;
        let resid: f64 = tail
            .clone()
            .map(|i| (out[i] as f64 - k * sum[i] as f64).powi(2))
            .sum();
        let thd_n = 10.0 * (resid / (k * k * ss)).log10();
        let peak = peak_db(&out[3 * SR..]);
        let sum_peak = peak_db(&sum[3 * SR..]);
        let over = tail.clone().filter(|&i| sum[i].abs() > mixer::KNEE).count() as f32
            / (len - 3 * SR) as f32;
        eprintln!(
            "{n} speakers at {volume:.0} %: mix peak {peak:.1} dBFS (sum peak {sum_peak:.1}), \
             limiter active on {:.2} % of samples, distortion {thd_n:.1} dB, level {:.1} dBFS",
            100.0 * over,
            level_db(&out, 3.0, 8.0)
        );
        assert!(
            out.iter().all(|s| s.abs() <= 1.0),
            "{n} speakers at {volume:.0} % exceed 0 dBFS"
        );
        assert!(
            thd_n <= -20.0,
            "{n} speakers at {volume:.0} %: limiter distortion {thd_n:.1} dB (limit −20 dB)"
        );
    }
}

/// (g) A speaker who suddenly talks 15 dB louder: the stages must not cut
/// in series and then release one after the other — the listener would
/// hear the voice sag well below the target for seconds.
#[test]
fn a_level_jump_does_not_dip() {
    let mic = microphone(20.0, 51, -55.0, |t| {
        Some(if t < 10.0 { -37.0 } else { -22.0 })
    });
    let (heard, stream, rx_gains) = one_speaker(&mic, 100.0);
    let lag = lag_blocks(&mic.samples, &heard);
    let before = voiced_level_db(&heard, &mic.loud, lag, 6.0, 10.0);
    let after = voiced_level_db(&heard, &mic.loud, lag, 16.0, 20.0);
    // The chain's gain per window (listener over microphone, both over the
    // loud blocks), so the syllables' own loudness cancels.
    let effective = |from: f32, to: f32| {
        voiced_level_db(&heard, &mic.loud, lag, from, to)
            - voiced_level_db(&mic.samples, &mic.loud, 0, from, to)
    };
    let steady_gain = effective(16.0, 20.0);
    let mut worst_dip = 0.0f32;
    let mut worst_over = 0.0f32;
    for i in 0..12 {
        let from = 10.5 + 0.5 * i as f32;
        let g = effective(from, from + 0.5);
        let b = (from * 100.0) as usize;
        eprintln!(
            "{from:.1} s: gain {g:.1} dB (steady {steady_gain:.1}); stages mic {:.1} rx {:.1}",
            stream.gains[b].0,
            rx_gains[b + lag]
        );
        worst_dip = worst_dip.max(steady_gain - g);
        if i >= 1 {
            worst_over = worst_over.max(g - steady_gain);
        }
    }
    let onset = voiced_level_db(&heard, &mic.loud, lag, 10.0, 10.5);
    eprintln!(
        "jump: steady {before:.1} dBFS before, {after:.1} after; first 0.5 s {onset:.1} dBFS, \
         worst dip {worst_dip:.1} dB, worst overshoot after 1 s {worst_over:.1} dB"
    );
    assert!(
        (before - after).abs() <= 3.0,
        "the same voice lands at {before:.1} dBFS quiet and {after:.1} dBFS loud"
    );
    assert!(
        worst_dip <= 3.0,
        "voice sags {worst_dip:.1} dB below its steady level after a +15 dB jump"
    );
    assert!(
        worst_over <= 3.0,
        "voice stays {worst_over:.1} dB hot a second after a +15 dB jump"
    );
}
