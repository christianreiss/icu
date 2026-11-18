//! Reproducible offline production-DSP/Opus comparison. No microphone capture.
//! Run with an output directory; --cfg icu_legacy_audio builds the same probe
//! against the unmodified baseline rlib (see experiments/audio-quality.py).
#![allow(unexpected_cfgs)]
use icu_client::dsp::{
    chain::{SendChain, SendSettings},
    gate::{Decision, Gate, Mode},
    meter, mixer,
    normalize::Normalizer,
    opus,
    preroll::PreRoll,
    vad_auto::AutoThreshold,
};
use std::{
    collections::VecDeque,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

const BLOCK: usize = 480;
const RATE: usize = 48_000;

fn read_wav(bytes: &[u8]) -> Vec<f32> {
    assert_eq!(&bytes[..4], b"RIFF");
    let mut cursor = 12;
    while cursor + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let start = cursor + 8;
        assert!(start + size <= bytes.len());
        if &bytes[cursor..cursor + 4] == b"fmt " {
            assert_eq!(
                u16::from_le_bytes(bytes[start..start + 2].try_into().unwrap()),
                1
            );
            assert_eq!(
                u16::from_le_bytes(bytes[start + 2..start + 4].try_into().unwrap()),
                1
            );
            assert_eq!(
                u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap()),
                48_000
            );
            assert_eq!(
                u16::from_le_bytes(bytes[start + 14..start + 16].try_into().unwrap()),
                16
            );
        }
        if &bytes[cursor..cursor + 4] == b"data" {
            return bytes[start..start + size]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect();
        }
        cursor = start + size + size % 2;
    }
    panic!("WAV data missing")
}

fn wav(path: &Path, audio: &[f32]) {
    let mut file = std::fs::File::create(path).unwrap();
    let size = (audio.len() * 4) as u32;
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(size + 36).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt \x10\0\0\0\x03\0\x01\0").unwrap();
    file.write_all(&48_000u32.to_le_bytes()).unwrap();
    file.write_all(&192_000u32.to_le_bytes()).unwrap();
    file.write_all(b"\x04\0\x20\0data").unwrap();
    file.write_all(&size.to_le_bytes()).unwrap();
    for sample in audio {
        file.write_all(&sample.to_le_bytes()).unwrap();
    }
}

fn rms_db(samples: &[f32]) -> f32 {
    let e: f64 = samples.iter().map(|s| (*s as f64).powi(2)).sum();
    (10.0 * (e / samples.len().max(1) as f64).max(1e-12).log10()) as f32
}

fn scale(samples: &mut [f32], db: f32) {
    let gain = meter::from_db(db - rms_db(samples));
    for s in samples {
        *s *= gain;
    }
}

struct Run {
    heard: Vec<f32>,
    decoded: Vec<f32>,
    opens: usize,
    packets: usize,
    bytes: usize,
    cpu: Vec<f64>,
}
fn run(input: &[f32], frame: usize) -> Run {
    let settings = SendSettings::default();
    let mut chain = SendChain::new();
    let start = Instant::now();
    let mut auto = AutoThreshold::new(start);
    let mut gate = Gate::new(
        Mode::Vad,
        Duration::from_millis(150),
        Duration::from_millis(1000),
    );
    gate.set_attack(Duration::from_millis(20));
    let mut pre = PreRoll::default();
    let mut encoder = opus::Encoder::new().unwrap();
    let mut decoder = opus::Decoder::new().unwrap();
    let mut normalizer = Normalizer::new();
    let mut pending = VecDeque::new();
    let mut acc = Vec::with_capacity(frame);
    let mut voice_in_frame = false;
    let mut scratch = vec![0.0; 5760];
    let mut result = Run {
        heard: Vec::new(),
        decoded: Vec::new(),
        opens: 0,
        packets: 0,
        bytes: 0,
        cpu: Vec::new(),
    };
    for (i, raw) in input.as_chunks::<BLOCK>().0.iter().enumerate() {
        let mut block = [0.0; BLOCK];
        block.copy_from_slice(raw);
        let now = start + Duration::from_millis(i as u64 * 10);
        let cpu = Instant::now();
        let a = chain.analyze(&mut block, &settings);
        #[cfg(icu_legacy_audio)]
        let voice = {
            if let Some(p) = a.prob {
                auto.update(now, gate.is_open() || p >= auto.threshold(), p);
            }
            icu_client::dsp::vad::is_voice(auto.threshold(), a.prob, a.level)
        };
        #[cfg(not(icu_legacy_audio))]
        let (_, voice) = a.classify(&mut auto, now, gate.is_open(), None);
        #[cfg(icu_legacy_audio)]
        chain.finish(&mut block, voice, &settings);
        #[cfg(not(icu_legacy_audio))]
        chain.finish_vad(&mut block, voice, gate.is_open(), &settings);
        voice_in_frame |= voice;
        acc.extend_from_slice(&block);
        if acc.len() == frame {
            let open = gate.is_open();
            let decision = gate.update_audio(
                now,
                voice_in_frame,
                Duration::from_micros(frame as u64 * 1_000_000 / 48_000),
            );
            voice_in_frame = false;
            let mut encode = |samples: &[f32]| {
                let packet = encoder.encode(samples).unwrap();
                result.packets += 1;
                result.bytes += packet.len();
                let n = decoder.decode(packet, &mut scratch).unwrap();
                pending.extend(scratch[..n].iter().copied());
            };
            if decision != Decision::Silent {
                if !open {
                    result.opens += 1;
                    for samples in
                        pre.take(icu_client::dsp::preroll::depth(20, (frame / 48) as u32))
                    {
                        encode(&samples);
                    }
                }
                encode(&acc);
                pre.clear();
            } else {
                pre.push(
                    &acc,
                    icu_client::dsp::preroll::depth(40, (frame / 48) as u32),
                );
            }
            acc.clear();
        }
        result.cpu.push(cpu.elapsed().as_secs_f64() * 1000.0);
        let mut out = [0.0; BLOCK];
        for sample in &mut out {
            *sample = pending.pop_front().unwrap_or(0.0);
        }
        result.decoded.extend_from_slice(&out);
        normalizer.process(&mut out);
        for sample in &mut out {
            *sample = mixer::limit(*sample);
        }
        result.heard.extend_from_slice(&out);
    }
    result
}

// Independent source-selected energy: the clean input decides the mask, never
// VAD, peer correction or the controller's envelope. Exclude first12s for steady
// level. This is masked RMS, explicitly NOT P.56/LUFS or a perceptual score.
fn metrics(clean: &[f32], run: &Run) -> (usize, f32, f32, f32) {
    let reference: Vec<_> = clean
        .as_chunks::<BLOCK>()
        .0
        .iter()
        .map(|block| rms_db(block))
        .collect();
    let output: Vec<_> = run
        .heard
        .as_chunks::<BLOCK>()
        .0
        .iter()
        .map(|block| rms_db(block))
        .collect();
    let max = reference.iter().copied().fold(-120.0f32, f32::max);
    let mask: Vec<_> = reference
        .iter()
        .map(|l| *l > max - 25.0 && *l > -100.0)
        .collect();
    let mut best = (f64::NEG_INFINITY, 0usize);
    for lag in 0..=12 {
        let score: f64 = reference
            .iter()
            .zip(output.iter().skip(lag))
            .filter(|(a, _)| **a > -100.0)
            .map(|(a, b)| ((*a as f64).max(-80.0) + 80.0) * ((*b as f64).max(-80.0) + 80.0))
            .sum();
        if score > best.0 {
            best = (score, lag);
        }
    }
    let lag = best.1;
    let mut selected = Vec::new();
    let mut eligible = 0;
    let mut retained = 0;
    for (i, (&active, block)) in mask
        .iter()
        .zip(run.heard.as_chunks::<BLOCK>().0.iter().skip(lag))
        .enumerate()
    {
        if active {
            eligible += 1;
            if rms_db(block) > -65.0 {
                retained += 1;
            }
            if i >= 1200 {
                selected.extend_from_slice(block);
            }
        }
    }
    let peak = run
        .heard
        .iter()
        .copied()
        .map(f32::abs)
        .fold(0.0f32, f32::max);
    (
        lag,
        rms_db(&selected),
        retained as f32 / eligible.max(1) as f32 * 100.0,
        meter::to_db(peak),
    )
}

fn main() {
    let dir = std::env::args().nth(1).expect("audio_quality OUTPUT_DIR");
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    let speech = read_wav(include_bytes!("../tests/fixtures/audio/voices-sp0307.wav"));
    let mut typing = read_wav(include_bytes!("../tests/fixtures/audio/keychron-v1.wav"));
    scale(&mut typing, -32.0);
    let mut csv = std::fs::File::create(dir.join("metrics.csv")).unwrap();
    writeln!(csv,"scenario,frame_ms,source_rms_db,source_masked_output_db,retained_mask_percent,envelope_lag_ms,sample_peak_db,gate_opens,packets,payload_bytes,estimated_ipv4_quic_kbps,dsp_encode_p99_ms,late_typing_rms_db").unwrap();
    for frame in [480, 960] {
        for (name, db) in [
            ("speech", -55.0),
            ("speech", -40.0),
            ("speech", -30.0),
            ("speech", -20.0),
            ("speech", -12.0),
            ("typing", -30.0),
            ("speech-typing", -30.0),
            ("typing-speech", -40.0),
            ("overlap", -30.0),
        ] {
            let n = RATE * 24;
            let mut clean = vec![0.0; n];
            let mut input = vec![0.0; n];
            let mut voice = speech.clone();
            scale(&mut voice, db);
            for i in 0..n {
                let t = i as f32 / RATE as f32;
                let pos = i % (RATE * 4);
                let speaking = match name {
                    "typing" => false,
                    "speech-typing" => t < 16.0,
                    "typing-speech" => t >= 8.0,
                    _ => true,
                };
                clean[i] = if speaking && pos < voice.len() {
                    voice[pos]
                } else {
                    0.0
                };
                let keys = match name {
                    "typing" | "overlap" => true,
                    "speech-typing" => t >= 16.0,
                    "typing-speech" => t < 8.0,
                    _ => false,
                };
                input[i] = clean[i] + if keys { typing[i % typing.len()] } else { 0.0 };
            }
            let mut result = run(&input, frame);
            assert!(result.heard.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
            let (lag, level, retained, peak) = metrics(&clean, &result);
            result.cpu.sort_by(f64::total_cmp);
            let p99 = result.cpu[result.cpu.len() * 99 / 100];
            // IPv4+UDP28, typical1RTT QUIC header13+AEAD16+DATAGRAM3,
            // ICU header14 + com authentication tag16. Actual transport,
            // acknowledgments/coalescing belong to separate network tests.
            let kbps = (result.bytes + result.packets * 90) as f64 * 8.0 / 24_000.0;
            writeln!(csv,"{name},{},{db},{level:.3},{retained:.2},{},{peak:.3},{},{},{},{kbps:.3},{p99:.3},{:.3}",frame/48,lag*10,result.opens,result.packets,result.bytes,rms_db(&result.heard[17*RATE..])).unwrap();
            let prefix = format!("{name}-{}-{}ms", -db as i32, frame / 48);
            wav(&dir.join(format!("{prefix}-input.wav")), &input);
            wav(&dir.join(format!("{prefix}-decoded.wav")), &result.decoded);
            wav(&dir.join(format!("{prefix}-heard.wav")), &result.heard);
        }
    }
}
