//! Peer-assisted target tuning. Measurements never depend on playback volume.
use super::{meter, normalize::Normalizer};
use icu_proto::AudioLevel;
use std::time::{Duration, Instant};

pub struct Measurement {
    estimator: Normalizer,
    samples: usize,
    near_full: usize,
    peak: f32,
}
impl Default for Measurement {
    fn default() -> Self {
        Self {
            estimator: Normalizer::new(),
            samples: 0,
            near_full: 0,
            peak: 0.0,
        }
    }
}
impl Measurement {
    pub fn observe(&mut self, samples: &[f32], speech: Option<bool>) {
        if self.estimator.observe(samples, speech) {
            self.samples += samples.len();
            self.near_full += samples.iter().filter(|s| s.abs() >= 0.99).count();
            self.peak = samples.iter().fold(self.peak, |p, s| p.max(s.abs()));
        }
    }
    pub fn take(&mut self) -> Option<AudioLevel> {
        let n = std::mem::take(&mut self.samples);
        let full = std::mem::take(&mut self.near_full);
        let peak = std::mem::take(&mut self.peak);
        if !(48_000..=105_600).contains(&n) {
            return None;
        }
        Some(AudioLevel {
            level_cdb: (self.estimator.level_db() * 100.0).round() as i16,
            correction_cdb: (self.estimator.correction_db() * 100.0).round() as i16,
            peak_cdb: (meter::to_db(peak) * 100.0).round() as i16,
            near_full_permille: (full * 1000 / n) as u16,
            speech_ms: (n / 48) as u16,
        })
    }
}

#[derive(Default)]
pub struct PeerTuner {
    pub offset_db: f32,
    hint: Option<(f32, Instant)>,
    streak: u8,
    direction: f32,
    last_used: Option<Instant>,
}
impl PeerTuner {
    pub fn status(&self, now: Instant) -> &'static str {
        if self
            .hint
            .is_none_or(|(_, t)| now.saturating_duration_since(t) >= Duration::from_secs(6))
        {
            "kein gültiges Empfangsfeedback"
        } else if self.streak < 3 {
            "prüft"
        } else {
            "aktiv"
        }
    }

    pub fn hint(&mut self, correction_cdb: Option<i16>, now: Instant) {
        self.hint = correction_cdb
            .filter(|c| (-1200..=1200).contains(c))
            .map(|c| (c as f32 / 100.0, now));
    }
    pub fn tick(&mut self, local: Option<AudioLevel>, enabled: bool, now: Instant) -> f32 {
        let hint = self
            .hint
            .filter(|(_, t)| now.saturating_duration_since(*t) < Duration::from_secs(6));
        if !enabled || hint.is_none() {
            self.streak = 0;
            self.offset_db -= self.offset_db.clamp(-0.5, 0.5);
            return self.offset_db;
        }
        let Some(l) = local.filter(|l| l.valid()) else {
            return self.offset_db;
        };
        let (remote, at) = hint.unwrap();
        if self.last_used == Some(at) {
            return self.offset_db;
        }
        self.last_used = Some(at);
        let own = l.correction_cdb as f32 / 100.0;
        if remote.abs() <= 2.0
            || own.abs() <= 2.0
            || (own - remote).abs() > 2.0
            || own.signum() != remote.signum()
            || (remote > 0.0 && (l.peak_cdb > -300 || l.near_full_permille > 0))
        {
            self.streak = 0;
            return self.offset_db;
        }
        if self.direction != remote.signum() {
            self.streak = 0;
            self.direction = remote.signum();
        }
        self.streak = self.streak.saturating_add(1);
        if self.streak >= 3 {
            self.offset_db = (self.offset_db + remote.signum() * 0.5).clamp(-3.0, 3.0);
        }
        self.offset_db
    }
}

/// Capture measurements, accumulated only while actual speech may be transmitted.
#[derive(Default, Clone, Copy)]
pub struct CaptureWindow {
    pub samples: usize,
    pub speech_samples: usize,
    pub near_full: usize,
    pub peak: f32,
    pub speech_energy: f64,
    pub gain_sum: f64,
    pub noise_samples: usize,
    pub noise_energy: f64,
}
impl CaptureWindow {
    pub fn observe(&mut self, raw: &[f32], speech: bool, gain_db: f32) {
        if raw.iter().any(|s| !s.is_finite()) {
            return;
        }
        self.samples += raw.len();
        self.near_full += raw.iter().filter(|s| s.abs() >= 0.99).count();
        self.peak = raw.iter().fold(self.peak, |p, s| p.max(s.abs()));
        if speech {
            self.speech_samples += raw.len();
            self.speech_energy += raw.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
            self.gain_sum += gain_db as f64 * raw.len() as f64;
        } else {
            self.noise_samples += raw.len();
            self.noise_energy += raw.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
        }
    }
    pub fn gain_db(self) -> f32 {
        if self.speech_samples == 0 {
            0.0
        } else {
            (self.gain_sum / self.speech_samples as f64) as f32
        }
    }
    pub fn rms_db(self) -> f32 {
        meter::to_db((self.speech_energy / self.speech_samples.max(1) as f64).sqrt() as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn level(c: i16) -> AudioLevel {
        AudioLevel {
            level_cdb: -2800,
            correction_cdb: c,
            peak_cdb: -600,
            near_full_permille: 0,
            speech_ms: 1500,
        }
    }
    #[test]
    fn bounded_corroborated_and_expires() {
        let mut p = PeerTuner::default();
        let t = Instant::now();
        for i in 0..20 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(400), now);
            p.tick(Some(level(400)), true, now);
        }
        assert_eq!(p.offset_db, 3.0);
        for i in 0..6 {
            p.tick(None, true, t + Duration::from_secs(50 + i * 2));
        }
        assert_eq!(p.offset_db, 0.0);
    }
    #[test]
    fn uncorroborated_clipped_or_repeated_hints_do_not_tune() {
        let mut p = PeerTuner::default();
        let t = Instant::now();
        for i in 0..8 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(400), now);
            p.tick(Some(level(-400)), true, now);
        }
        assert_eq!(p.offset_db, 0.0);
        p = PeerTuner::default();
        p.hint(Some(400), t);
        for _ in 0..10 {
            p.tick(Some(level(400)), true, t);
        }
        assert_eq!(p.offset_db, 0.0);
    }
    #[test]
    fn silence_and_invalid_samples_are_not_speech() {
        let mut m = Measurement::default();
        for _ in 0..200 {
            m.observe(&[0.0; 480], None);
        }
        assert!(m.take().is_none());
        m.observe(&[f32::NAN; 480], Some(true));
        assert!(m.take().is_none());
    }
}
