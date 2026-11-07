//! Conservative automatic camera correction. No future-frame buffering, device
//! writes or user tuning. State consists only of slowly changing parameters.
use crate::camera::RgbFrame;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Enhancer {
    scratch: Vec<u8>,
    lift: f32,
    balance: [f32; 3],
    over_budget: u8,
    resume_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Report {
    pub elapsed: Duration,
    pub bypassed: bool,
    pub corrected: bool,
}

impl Enhancer {
    pub fn process(&mut self, frame: &mut RgbFrame, now: Instant) -> Report {
        let start = Instant::now();
        if !frame.is_valid() || self.resume_at.is_some_and(|at| at > now) {
            return Report {
                bypassed: true,
                ..Report::default()
            };
        }
        self.resume_at = None;
        let (w, h) = (frame.width, frame.height);
        let mut hist = [0usize; 256];
        let mut neutral = [0u64; 3];
        let mut neutrals = 0;
        for p in frame.rgb.as_chunks::<3>().0.iter().step_by(4) {
            let y = (u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) >> 8;
            hist[y as usize] += 1;
            let min = *p.iter().min().unwrap();
            let max = *p.iter().max().unwrap();
            if min > 40 && max < 220 && max - min < 25 {
                for c in 0..3 {
                    neutral[c] += u64::from(p[c]);
                }
                neutrals += 1;
            }
        }
        let total: usize = hist.iter().sum();
        let mut n = 0;
        let median = hist
            .iter()
            .position(|v| {
                n += v;
                n >= total.div_ceil(2)
            })
            .unwrap_or(128) as f32;
        // Lift shadows only. Never darken a good scene or chase highlights.
        let wanted = ((80.0 - median) / 80.0).clamp(0.0, 0.5);
        self.lift += (wanted - self.lift) * 0.08;
        let mut target = [1.0; 3];
        if neutrals > total / 10 && neutrals > 32 {
            let mean = neutral.iter().sum::<u64>() as f32 / 3.0;
            for c in 0..3 {
                target[c] = (mean / neutral[c].max(1) as f32).clamp(0.96, 1.04);
            }
        }
        for (balance, target) in self.balance.iter_mut().zip(target) {
            if *balance == 0.0 {
                *balance = 1.0;
            }
            *balance += (target - *balance) * 0.04;
        }
        // Noise evidence from small spatial second differences; strong edges
        // are excluded. Only a weak edge-preserving spatial blend is allowed.
        let mut rough = 0u64;
        let mut count = 0;
        for y in (1..h - 1).step_by(3) {
            for x in (1..w - 1).step_by(3) {
                let i = (y * w + x) * 3;
                let l = i32::from(frame.rgb[i - 3]);
                let r = i32::from(frame.rgb[i + 3]);
                let m = i32::from(frame.rgb[i]);
                if (l - r).abs() < 12 {
                    rough += (2 * m - l - r).unsigned_abs() as u64;
                    count += 1;
                }
            }
        }
        let noise = count > 0 && rough / count > 5;
        let correct =
            self.lift > 0.005 || noise || self.balance.iter().any(|v| (*v - 1.0).abs() > 0.002);
        if correct {
            self.scratch.resize(frame.rgb.len(), 0);
            self.scratch.copy_from_slice(&frame.rgb);
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * 3;
                    for c in 0..3 {
                        let center = f32::from(self.scratch[i + c]);
                        let mut value = center;
                        if noise && x > 0 && x + 1 < w && y > 0 && y + 1 < h {
                            let mut sum = center;
                            let mut weight = 1.0;
                            for j in [i - 3, i + 3, i - w * 3, i + w * 3] {
                                let v = f32::from(self.scratch[j + c]);
                                if (v - center).abs() < 16.0 {
                                    sum += v;
                                    weight += 1.0;
                                }
                            }
                            value = center * 0.75 + sum / weight * 0.25;
                        }
                        // Smooth highlight-preserving curve, bounded to +32 levels.
                        let v = value / 255.0;
                        value += (self.lift * 128.0 * v * (1.0 - v)).min(32.0);
                        frame.rgb[i + c] =
                            (value * self.balance[c]).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
        let elapsed = start.elapsed();
        self.record_cost(elapsed, now);
        Report {
            elapsed,
            bypassed: false,
            corrected: correct,
        }
    }
    fn record_cost(&mut self, cost: Duration, now: Instant) {
        if cost > Duration::from_millis(5) {
            self.over_budget = self.over_budget.saturating_add(1);
        } else {
            self.over_budget = self.over_budget.saturating_sub(1);
        }
        if self.over_budget >= 3 {
            self.resume_at = Some(now + Duration::from_secs(5));
            self.over_budget = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn solid(rgb: [u8; 3]) -> RgbFrame {
        RgbFrame::checked(32, 24, rgb.repeat(32 * 24)).unwrap()
    }
    #[test]
    fn well_lit_neutral_image_is_unchanged() {
        let mut e = Enhancer::default();
        let mut f = solid([128; 3]);
        let old = f.rgb.clone();
        for _ in 0..60 {
            e.process(&mut f, Instant::now());
        }
        assert_eq!(f.rgb, old);
    }
    #[test]
    fn dark_image_lifts_gently_and_preserves_black_and_white() {
        let mut e = Enhancer::default();
        let mut f = solid([35; 3]);
        f.rgb[..3].fill(0);
        f.rgb[3..6].fill(255);
        for _ in 0..60 {
            let mut copy = f.clone();
            e.process(&mut copy, Instant::now());
            assert_eq!(&copy.rgb[..3], &[0; 3]);
            assert_eq!(&copy.rgb[3..6], &[255; 3]);
            assert!(copy.rgb[6] >= 35 && copy.rgb[6] <= 67);
        }
    }
    #[test]
    fn saturated_scene_does_not_teach_white_balance() {
        let mut e = Enhancer::default();
        let mut f = solid([200, 20, 20]);
        for _ in 0..90 {
            e.process(&mut f, Instant::now());
        }
        assert_eq!(e.balance, [1.0; 3]);
    }
    #[test]
    fn sustained_cpu_cost_bypasses_and_recovers_without_queueing() {
        let mut e = Enhancer::default();
        let now = Instant::now();
        for _ in 0..3 {
            e.record_cost(Duration::from_millis(8), now);
        }
        assert!(e.process(&mut solid([80; 3]), now).bypassed);
        assert!(
            !e.process(&mut solid([80; 3]), now + Duration::from_secs(6))
                .bypassed
        );
    }
}

#[cfg(test)]
mod codec_tests {
    use super::*;
    use crate::video::{decode, Decoder, VideoTier};
    use openh264::formats::{RgbSliceU8, YUVBuffer};
    #[test]
    fn denoising_survives_real_h264_at_every_active_tier() {
        for tier in [VideoTier::T0, VideoTier::T1, VideoTier::T2, VideoTier::T3] {
            let mut before = crate::camera::encoder(tier).unwrap();
            let mut after = crate::camera::encoder(tier).unwrap();
            let mut db = Decoder::new().unwrap();
            let mut da = Decoder::new().unwrap();
            let mut enhancer = Enhancer::default();
            let mut baseline_error = 0.0f64;
            let mut processed_error = 0.0f64;
            let mut bytes = [0usize; 2];
            let mut frames = 0;
            for tick in 0..30usize {
                let rgb: Vec<u8> = (0..64 * 48)
                    .flat_map(|i| {
                        let noise = (((i * 13 + tick * 17) % 23) as i32 - 11) as i16;
                        [(128 + noise) as u8; 3]
                    })
                    .collect();
                let mut frame = RgbFrame::checked(64, 48, rgb.clone()).unwrap();
                enhancer.process(&mut frame, Instant::now());
                let baseline = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (64, 48)));
                let processed = YUVBuffer::from_rgb_source(RgbSliceU8::new(&frame.rgb, (64, 48)));
                let b = before.encode(&baseline).unwrap().to_vec();
                let a = after.encode(&processed).unwrap().to_vec();
                bytes[0] += b.len();
                bytes[1] += a.len();
                if let (Some(b), Some(a)) = (decode(&mut db, &b), decode(&mut da, &a)) {
                    for (bp, ap) in b
                        .rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(a.rgba.as_chunks::<4>().0.iter())
                    {
                        baseline_error += (f64::from(bp[1]) - 128.0).powi(2);
                        processed_error += (f64::from(ap[1]) - 128.0).powi(2);
                    }
                    frames += 1;
                }
            }
            eprintln!(
                "{tier:?}: decoded={frames} noise_mse_before={} after={} encoded_bytes={bytes:?}",
                baseline_error / (frames * 64 * 48) as f64,
                processed_error / (frames * 64 * 48) as f64
            );
            assert!(frames >= 20);
            assert!(
                processed_error <= baseline_error * 1.05 + 64.0 * 48.0 * frames as f64,
                "denoising must not worsen coded noise"
            );
        }
    }
}
