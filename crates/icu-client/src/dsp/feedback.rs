//! Slow peer-assisted output trim. Measurements never depend on playback volume.
use super::{meter, normalize::Normalizer};
use icu_proto::{AudioLevel, AudioWindow};
use std::time::{Duration, Instant};

/// Enough to correct a moderate downstream level error without chasing
/// arbitrary peer requests. Peak headroom and the final limiter still apply.
pub const MAX_PEER_TRIM_DB: f32 = 6.0;

pub struct Measurement {
    estimator: Normalizer,
    samples: usize,
    near_full: usize,
    peak: f32,
    epoch: Option<u32>,
    first_ts: Option<u32>,
    end_ts: u32,
    settle_left: usize,
    report_id: u32,
}
impl Default for Measurement {
    fn default() -> Self {
        Self {
            estimator: Normalizer::new(),
            samples: 0,
            near_full: 0,
            peak: 0.0,
            epoch: None,
            first_ts: None,
            end_ts: 0,
            settle_left: 0,
            report_id: 0,
        }
    }
}
impl Measurement {
    pub fn observe(&mut self, samples: &[f32], speech: Option<bool>) {
        self.observe_at(samples, speech, 0, self.end_ts);
    }
    /// Observe only regular decoded audio, retaining its sender clock and gain.
    pub fn observe_at(&mut self, samples: &[f32], speech: Option<bool>, epoch: u32, ts: u32) {
        if self.epoch != Some(epoch) {
            if self
                .epoch
                .is_some_and(|old| epoch.wrapping_sub(old) as i32 <= 0)
            {
                return;
            }
            self.estimator = Normalizer::new();
            self.samples = 0;
            self.near_full = 0;
            self.peak = 0.0;
            self.first_ts = None;
            self.epoch = Some(epoch);
            self.settle_left = 9600;
        }
        self.end_ts = ts.wrapping_add(samples.len() as u32);
        if self.settle_left > 0 {
            self.settle_left = self.settle_left.saturating_sub(samples.len());
            return;
        }
        if self.estimator.observe(samples, speech) {
            self.first_ts.get_or_insert(ts);
            self.samples += samples.len();
            self.near_full += samples.iter().filter(|s| s.abs() >= 0.99).count();
            self.peak = samples.iter().fold(self.peak, |p, s| p.max(s.abs()));
        }
    }
    pub fn take_window(&mut self) -> Option<(AudioLevel, AudioWindow)> {
        let first_ts = self.first_ts;
        let window = AudioWindow {
            gain_epoch: self.epoch.unwrap_or(0),
            first_ts: first_ts.unwrap_or(self.end_ts),
            end_ts: self.end_ts,
            report_id: self.report_id,
        };
        self.report_id = self.report_id.wrapping_add(1);
        let level = self.take()?;
        (first_ts.is_some() && window.valid()).then_some((level, window))
    }
    pub fn take(&mut self) -> Option<AudioLevel> {
        self.first_ts = None;
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
    decision: Option<&'static str>,
    epoch: u32,
    last_window: Option<AudioWindow>,
    last_action: Option<Instant>,
    tracking: bool,
    reporters: u16,
    boost_blocked: bool,
    candidate_trim: Option<f32>,
    received_at: Option<Instant>,
}
impl PeerTuner {
    pub fn status(&self, now: Instant) -> &'static str {
        if self
            .hint
            .is_none_or(|(_, t)| now.saturating_duration_since(t) >= Duration::from_secs(6))
        {
            if self.offset_db != 0.0 {
                "Guten Pegel während der Pause gehalten"
            } else {
                "warte auf Empfangsfeedback"
            }
        } else {
            self.decision.unwrap_or("prüft")
        }
    }

    pub fn block_boost(&mut self, blocked: bool) {
        self.boost_blocked = blocked;
    }
    pub fn seed_trim(&mut self, db: f32) {
        self.candidate_trim = db.is_finite().then_some(db.clamp(-6.0, 6.0));
    }
    pub fn hint_age_secs(&self, now: Instant) -> Option<f32> {
        self.received_at
            .map(|at| now.saturating_duration_since(at).as_secs_f32())
    }
    pub fn conflicting(&self, now: Instant) -> bool {
        self.hint.is_none()
            && self.reporters > 0
            && self
                .received_at
                .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(6))
    }
    pub fn epoch(&self) -> u32 {
        self.epoch
    }
    pub fn reporters(&self) -> u16 {
        self.reporters
    }
    pub fn correction(&self, now: Instant) -> Option<f32> {
        self.hint
            .filter(|(_, at)| now.saturating_duration_since(*at) < Duration::from_secs(6))
            .map(|(c, _)| c)
    }
    pub fn set_epoch(&mut self, epoch: u32) {
        if epoch != self.epoch {
            self.epoch = epoch;
            self.hint = None;
            self.last_window = None;
            self.last_used = None;
            self.streak = 0;
            self.reporters = 0;
            self.received_at = None;
        }
    }
    pub fn hint_window(
        &mut self,
        window: AudioWindow,
        correction: Option<i16>,
        reporters: u16,
        now: Instant,
    ) {
        if window.gain_epoch != self.epoch {
            return;
        }
        if correction.is_some() {
            if !window.valid() || self.last_window.is_some_and(|old| !window.newer_than(old)) {
                return;
            }
            self.last_window = Some(window);
        }
        self.reporters = reporters;
        self.hint(correction, now);
    }
    pub fn hint(&mut self, correction_cdb: Option<i16>, now: Instant) {
        self.received_at = Some(now);
        self.hint = correction_cdb
            .filter(|c| (-1200..=1200).contains(c))
            .map(|c| (c as f32 / 100.0, now));
    }
    pub fn tick(&mut self, local: Option<AudioLevel>, enabled: bool, now: Instant) -> f32 {
        let previous_db = self.offset_db;
        let offset_db = self.update(local, enabled, now);
        tracing::debug!(
            ?local,
            remote_correction_db = ?self.hint.map(|(db, _)| db),
            enabled,
            previous_db,
            offset_db,
            status = self.status(now),
            "peer audio correction"
        );
        offset_db
    }

    fn update(&mut self, local: Option<AudioLevel>, enabled: bool, now: Instant) -> f32 {
        let hint = self
            .hint
            .filter(|(_, t)| now.saturating_duration_since(*t) < Duration::from_secs(6));
        if !enabled {
            self.streak = 0;
            self.tracking = false;
            self.decision = Some("aus");
            if self
                .last_action
                .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(2))
            {
                self.offset_db -= self.offset_db.clamp(-0.5, 0.5);
                self.last_action = Some(now);
            }
            return self.offset_db;
        }
        if hint.is_none() {
            self.streak = 0;
            self.decision = Some("Guten Pegel während der Pause gehalten");
            return self.offset_db;
        }
        let Some(l) = local.filter(|l| l.valid()) else {
            self.decision = Some("zu wenig lokale Sprache");
            return self.offset_db;
        };
        let (remote, at) = hint.unwrap();
        if self.last_used == Some(at) {
            return self.offset_db;
        }
        self.last_used = Some(at);
        let own = l.correction_cdb as f32 / 100.0;
        // The listener defines the residual error. Requiring the local
        // estimate to exceed the same dead band stops too early when Opus
        // and independent speech selection differ slightly; corroboration
        // is the bounded difference and common direction below.
        let rejected = if remote.abs() <= 2.0 {
            Some("im Zielbereich")
        } else if (own - remote).abs() > 2.0 || own.signum() != remote.signum() {
            Some("lokale Messung widerspricht")
        } else if remote > 0.0
            && (self.boost_blocked || l.peak_cdb > -300 || l.near_full_permille > 0)
        {
            Some("zu wenig Spitzenreserve")
        } else {
            None
        };
        if let Some(reason) = rejected {
            self.streak = 0;
            self.decision = Some(reason);
            return self.offset_db;
        }
        if self.direction != remote.signum() {
            self.streak = 0;
            self.direction = remote.signum();
            self.tracking = false;
        }
        if let Some(seed) = self.candidate_trim.take() {
            if seed.signum() == remote.signum() && seed.abs() >= 0.5 {
                self.tracking = true;
            }
        }
        self.streak = self.streak.saturating_add(1);
        self.decision = Some("prüft");
        if (self.streak >= 3 || self.tracking)
            && self
                .last_action
                .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(2))
        {
            let next =
                (self.offset_db + remote.signum() * 0.5).clamp(-MAX_PEER_TRIM_DB, MAX_PEER_TRIM_DB);
            self.decision = Some(if next == self.offset_db {
                "Regelgrenze"
            } else {
                "aktiv"
            });
            self.offset_db = next;
            self.last_action = Some(now);
            self.streak = 0;
            self.tracking = true;
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
    fn bounded_corroborated_and_held_during_silence() {
        let mut p = PeerTuner::default();
        let t = Instant::now();
        for i in 0..20 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(400), now);
            p.tick(Some(level(400)), true, now);
        }
        assert_eq!(p.offset_db, MAX_PEER_TRIM_DB);
        for i in 0..12 {
            p.tick(None, true, t + Duration::from_secs(50 + i * 2));
        }
        assert_eq!(p.offset_db, MAX_PEER_TRIM_DB);
        for i in 0..12 {
            p.tick(None, false, t + Duration::from_secs(100 + i * 2));
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

    #[test]
    fn sustained_local_speech_corroborates_peer_hints() {
        let mut m = Measurement::default();
        let mut p = PeerTuner::default();
        let t = Instant::now();
        for i in 0..12 {
            for _ in 0..200 {
                m.observe(&[meter::from_db(-30.0); 480], Some(true));
            }
            let now = t + Duration::from_secs(i * 2);
            let local = m.take().expect("confirmed speech must produce feedback");
            p.hint(Some(local.correction_cdb), now);
            p.tick(Some(local), true, now);
        }
        assert!(p.offset_db > 0.0, "peer correction must reach the leveler");
    }

    #[test]
    fn clean_receive_speech_produces_feedback_after_each_gain_epoch() {
        let mut m = Measurement::default();
        let mut phase = 0.0f32;
        let mut ts = 0;
        for epoch in 0..2 {
            let mut reports = 0;
            for b in 0..1200 {
                let mut block = [0.0; 480];
                for (i, s) in block.iter_mut().enumerate() {
                    let t = (b * 480 + i) as f32 / 48_000.0;
                    let hz = 180.0 + 20.0 * (t * std::f32::consts::TAU * 1.3).sin();
                    phase = (phase + hz / 48_000.0).fract();
                    let p = phase * std::f32::consts::TAU;
                    *s = p.sin() + 0.4 * (2.0 * p).sin() + 0.2 * (3.0 * p).sin();
                }
                let scale = meter::from_db(-34.0) / meter::from_db(meter::measure(&block).rms_db);
                for s in &mut block {
                    *s *= scale;
                }
                m.observe_at(&block, None, epoch, ts);
                ts += 480;
                if b % 200 == 199 {
                    if let Some((level, window)) = m.take_window() {
                        assert_eq!(window.gain_epoch, epoch);
                        assert!(level.valid());
                        if b > 600 {
                            assert!((850..=1050).contains(&level.correction_cdb), "{level:?}");
                        }
                        reports += 1;
                    }
                }
            }
            assert!(
                reports >= 4,
                "epoch {epoch} produced only {reports} windows"
            );
        }
    }

    #[test]
    fn clipping_blocks_boost_but_allows_cut_and_reports_the_reason() {
        let t = Instant::now();
        let mut p = PeerTuner::default();
        for i in 0..20 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(400), now);
            p.tick(
                Some(AudioLevel {
                    peak_cdb: -100,
                    ..level(400)
                }),
                true,
                now,
            );
            assert_eq!(p.offset_db, 0.0);
            assert_eq!(p.status(now), "zu wenig Spitzenreserve");
        }
        for i in 20..40 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(-400), now);
            p.tick(
                Some(AudioLevel {
                    peak_cdb: -100,
                    ..level(-400)
                }),
                true,
                now,
            );
        }
        assert_eq!(p.offset_db, -MAX_PEER_TRIM_DB);
        assert_eq!(p.status(t + Duration::from_secs(78)), "Regelgrenze");
        p.tick(None, false, t + Duration::from_secs(80));
        assert_eq!(p.offset_db, -MAX_PEER_TRIM_DB + 0.5);
    }

    #[test]
    fn missing_local_speech_is_visible_and_cannot_apply_a_hint() {
        let mut p = PeerTuner::default();
        let now = Instant::now();
        p.hint(Some(400), now);
        p.tick(None, true, now);
        assert_eq!(p.offset_db, 0.0);
        assert_eq!(p.status(now), "zu wenig lokale Sprache");
    }

    #[test]
    fn corroboration_near_local_dead_band_still_corrects_the_listener() {
        let mut p = PeerTuner::default();
        let t = Instant::now();
        for i in 0..3 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(-300), now);
            p.tick(Some(level(-150)), true, now);
        }
        assert_eq!(p.offset_db, -0.5);
    }

    #[test]
    fn short_pauses_do_not_erase_valid_corroboration() {
        let mut p = PeerTuner::default();
        let t = Instant::now();
        for i in 0..3 {
            let now = t + Duration::from_secs(i * 4);
            p.hint(Some(400), now);
            p.tick(Some(level(400)), true, now);
            let before_pause = p.offset_db;
            p.tick(None, true, now + Duration::from_secs(2));
            assert_eq!(p.offset_db, before_pause, "a pause must not change gain");
        }
        assert_eq!(p.offset_db, 0.5);
    }

    #[test]
    fn fresh_in_target_feedback_holds_the_learned_trim() {
        let mut p = PeerTuner::default();
        let t = Instant::now();
        for i in 0..6 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(400), now);
            p.tick(Some(level(400)), true, now);
        }
        let learned = p.offset_db;
        assert!(learned > 0.0);
        for i in 6..36 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(0), now);
            p.tick(Some(level(0)), true, now);
            assert_eq!(
                p.offset_db, learned,
                "successful feedback must not cause relearning"
            );
        }
    }
    #[test]
    fn sustained_limiter_block_prevents_boost_despite_apparent_peak_headroom() {
        let mut p = PeerTuner::default();
        p.block_boost(true);
        let t = Instant::now();
        for i in 0..10 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(400), now);
            p.tick(Some(level(400)), true, now);
        }
        assert_eq!(p.offset_db, 0.0);
        p.block_boost(false);
        for i in 10..13 {
            let now = t + Duration::from_secs(i * 2);
            p.hint(Some(400), now);
            p.tick(Some(level(400)), true, now);
        }
        assert_eq!(p.offset_db, 0.5);
    }
}
