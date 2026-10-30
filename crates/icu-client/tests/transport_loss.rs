//! Deterministic application-loss matrix through production Opus and jitter.
//! This is codec/playout coverage, not a 64-client live load benchmark.
use icu_client::dsp::{
    jitter::{JitterBuffer, JitterConfig, Packet, Pop},
    opus::{Decoder, Encoder},
    testsignal::{pearson, speechlike},
};
use std::time::{Duration, Instant};

#[test]
fn ten_and_twenty_ms_frames_survive_zero_to_ten_percent_loss() {
    const FRAMES: usize = 400;
    for samples in [480usize, 960] {
        let frame_ms = samples as u64 / 48;
        let signal = speechlike(FRAMES * samples, 913, 3.0, 0.0, 0.12);
        for loss in [0usize, 1, 3, 5, 10] {
            let mut encoder = Encoder::new().unwrap();
            encoder.set_loss_permille((loss * 10) as u16).unwrap();
            let packets: Vec<_> = signal
                .chunks_exact(samples)
                .map(|pcm| encoder.encode(pcm).unwrap().to_vec())
                .collect();
            let mut clean_decoder = Decoder::new().unwrap();
            let mut scratch = [0.0f32; 5760];
            let mut reference = Vec::new();
            for packet in &packets {
                let n = clean_decoder.decode(packet, &mut scratch).unwrap();
                reference.extend_from_slice(&scratch[..n]);
            }
            let mut arrivals: Vec<_> = packets
                .into_iter()
                .enumerate()
                .filter(|(seq, _)| (seq * 37 + 50) % 100 >= loss)
                .map(|(seq, payload)| {
                    (
                        seq as u64 * frame_ms + 25 + (seq * 29 % 7) as u64,
                        Packet {
                            gain_epoch: 0,
                            seq: seq as u16,
                            ts: (seq * samples) as u32,
                            end: seq == FRAMES - 1,
                            payload,
                        },
                    )
                })
                .collect();
            arrivals.sort_by_key(|(at, _)| *at);
            let expected_loss = FRAMES - arrivals.len();
            assert_eq!(expected_loss, FRAMES * loss / 100);
            let mut arrivals = arrivals.into_iter().peekable();
            let mut jitter = JitterBuffer::new(JitterConfig {
                frame_samples: samples as u32,
                ..JitterConfig::default()
            });
            let mut decoder = Decoder::new().unwrap();
            let t0 = Instant::now();
            let mut played = Vec::new();
            let (mut recovered, mut fallback) = (0usize, 0usize);
            for tick in 0..FRAMES + 30 {
                let ms = tick as u64 * frame_ms;
                while arrivals.peek().is_some_and(|(at, _)| *at <= ms) {
                    let (at, packet) = arrivals.next().unwrap();
                    jitter.push_with_duration(
                        packet,
                        samples as u32,
                        t0 + Duration::from_millis(at),
                    );
                }
                let n = match jitter.pop(t0 + Duration::from_millis(ms)) {
                    Pop::Packet(packet) => decoder.decode(&packet.payload, &mut scratch).unwrap(),
                    Pop::Lost {
                        fec_source,
                        samples,
                        ..
                    } => {
                        let n = if let Some(next) = fec_source {
                            decoder
                                .decode_fec_for(&next.payload, &mut scratch, samples as usize)
                                .unwrap()
                        } else {
                            decoder.conceal_for(&mut scratch, samples as usize).unwrap()
                        };
                        if decoder.last_fec_samples() > 0 {
                            recovered += 1;
                        } else {
                            fallback += 1;
                        }
                        n
                    }
                    Pop::Silence => 0,
                };
                assert!(scratch[..n].iter().all(|x| x.is_finite() && x.abs() < 1.2));
                played.extend_from_slice(&scratch[..n]);
            }
            let stats = jitter.stats();
            assert_eq!(
                played.len(),
                reference.len(),
                "frame={frame_ms} loss={loss}% {stats:?}"
            );
            assert_eq!(stats.lost as usize, expected_loss);
            assert_eq!(recovered + fallback, expected_loss);
            assert_eq!(
                (
                    stats.underruns,
                    stats.stretched,
                    stats.skipped,
                    stats.dropped
                ),
                (0, 0, 0, 0)
            );
            let correlation = pearson(&played, &reference);
            assert!(
                correlation > 0.65,
                "frame={frame_ms} loss={loss}% correlation={correlation}"
            );
            eprintln!("frame={frame_ms}ms loss={loss}% lost={expected_loss} FEC={recovered} PLC={fallback} correlation={correlation:.4}");
        }
    }
}
