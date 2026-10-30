//! Slow peer-assisted output trim. Measurements never depend on playback volume.
use super::{meter, normalize::Normalizer};
use icu_proto::{AudioLevel, AudioWindow};
use std::time::{Duration, Instant};

/// Enough to correct a moderate downstream level error without chasing
/// arbitrary peer requests. Peak headroom and the final limiter still apply.
pub const MAX_PEER_TRIM_DB: f32 = 6.0;

/// Scalar evidence only: bounded to six seconds, never retains microphone PCM.
#[derive(Clone, Copy)]
struct Evidence {
    first: u32,
    end: u32,
    speech: usize,
    energy: f64,
    near_full: usize,
    peak: f32,
}

pub struct Measurement {
    estimator: Normalizer,
    samples: usize,
    energy: f64,
    near_full: usize,
    peak: f32,
    epoch: Option<u32>,
    first_ts: Option<u32>,
    end_ts: u32,
    settle_left: usize,
    report_id: u32,
    history: std::collections::VecDeque<Evidence>,
}
impl Default for Measurement {
    fn default() -> Self {
        Self {
            estimator: Normalizer::with(super::normalize::Profile::FEEDBACK),
            samples: 0,
            energy: 0.0,
            near_full: 0,
            peak: 0.0,
            epoch: None,
            first_ts: None,
            end_ts: 0,
            settle_left: 0,
            report_id: 0,
            // Six seconds at the production 10-ms block size. Capacity never
            // grows in observe_at, including hostile/changing packet sizes.
            history: std::collections::VecDeque::with_capacity(600),
        }
    }
}
impl Measurement {
    pub fn observe(&mut self, samples: &[f32], speech: Option<bool>) {
        self.observe_at(samples, speech, 0, self.end_ts);
    }
    fn clear_window(&mut self) {
        self.samples = 0;
        self.energy = 0.0;
        self.near_full = 0;
        self.peak = 0.0;
        self.first_ts = None;
    }
    /// Observe regular source audio only. Split packets into the same 10-ms
    /// measurement blocks as the sender, independently of Opus frame duration.
    pub fn observe_at(&mut self, samples: &[f32], speech: Option<bool>, epoch: u32, ts: u32) {
        if samples.is_empty() {
            return;
        }
        if self.epoch != Some(epoch) {
            if self
                .epoch
                .is_some_and(|old| epoch.wrapping_sub(old) as i32 <= 0)
            {
                return;
            }
            // A fresh listener has no applied gain transition to settle. A
            // real transition ramps in one 20-ms frame, not 200 ms of speech.
            self.settle_left = if self.epoch.is_some() { 960 } else { 0 };
            self.estimator = Normalizer::with(super::normalize::Profile::FEEDBACK);
            self.clear_window();
            self.history.clear();
            self.epoch = Some(epoch);
            self.end_ts = ts;
        }
        if (ts.wrapping_sub(self.end_ts) as i32) < 0 {
            return;
        }
        if ts != self.end_ts {
            // Missing/PLC/FEC audio cannot establish a continuous pitch contour.
            self.estimator.reset_voice_history();
        }
        for (i, block) in samples.chunks(480).enumerate() {
            let first = ts.wrapping_add((i * 480) as u32);
            self.end_ts = first.wrapping_add(block.len() as u32);
            if self.first_ts.is_some_and(|start| {
                self.end_ts.wrapping_sub(start) > icu_proto::audio_level::MAX_WINDOW_SAMPLES
            }) {
                self.clear_window();
            }
            let settling = self.settle_left > 0;
            self.settle_left = self.settle_left.saturating_sub(block.len());
            let eligible = self.estimator.observe(block, speech) && !settling;
            let mut evidence = Evidence {
                first,
                end: self.end_ts,
                speech: 0,
                energy: 0.0,
                near_full: 0,
                peak: 0.0,
            };
            if eligible {
                evidence.speech = block.len();
                evidence.energy = block.iter().map(|&s| f64::from(s).powi(2)).sum();
                evidence.near_full = block.iter().filter(|s| s.abs() >= 0.99).count();
                evidence.peak = block.iter().fold(0.0f32, |p, s| p.max(s.abs()));
                self.first_ts.get_or_insert(first);
                self.samples += evidence.speech;
                self.energy += evidence.energy;
                self.near_full += evidence.near_full;
                self.peak = self.peak.max(evidence.peak);
            }
            while self.history.front().is_some_and(|old| {
                self.end_ts.wrapping_sub(old.first) > icu_proto::audio_level::MAX_AGE_SAMPLES
            }) || self.history.len() >= 600
            {
                self.history.pop_front();
            }
            self.history.push_back(evidence);
        }
    }
    fn level(samples: usize, energy: f64, full: usize, peak: f32) -> Option<AudioLevel> {
        let min = usize::from(icu_proto::audio_level::MIN_SPEECH_MS) * 48;
        let max = usize::from(icu_proto::audio_level::MAX_SPEECH_MS) * 48;
        if !(min..=max).contains(&samples) {
            return None;
        }
        let level_cdb =
            (meter::to_db((energy / samples as f64).sqrt() as f32) * 100.0).round() as i16;
        Some(AudioLevel {
            level_cdb,
            correction_cdb: (i32::from(icu_proto::audio_level::TARGET_CDB) - i32::from(level_cdb))
                .clamp(-1200, 1200) as i16,
            peak_cdb: (meter::to_db(peak) * 100.0).round() as i16,
            near_full_permille: (full * 1000 / samples) as u16,
            speech_ms: (samples / 48) as u16,
        })
    }
    pub fn take_window(&mut self) -> Option<(AudioLevel, AudioWindow)> {
        let window = AudioWindow {
            gain_epoch: self.epoch?,
            first_ts: self.first_ts?,
            end_ts: self.end_ts,
            report_id: self.report_id,
        };
        let level = Self::level(self.samples, self.energy, self.near_full, self.peak)?;
        self.clear_window();
        if !window.valid() {
            return None;
        }
        self.report_id = self.report_id.wrapping_add(1);
        Some((level, window))
    }
    pub fn take(&mut self) -> Option<AudioLevel> {
        self.take_window().map(|(level, _)| level)
    }
    /// Reconstruct local scalar evidence for precisely the remote window. The
    /// returned identity is the hint identity, not an unrelated timer window.
    /// Missing history, old gains and windows beyond the source clock fail closed.
    pub fn measure_window(&self, window: AudioWindow) -> Option<AudioLevel> {
        if !window.valid()
            || self.epoch != Some(window.gain_epoch)
            || !(0..=icu_proto::audio_level::MAX_AGE_SAMPLES as i32)
                .contains(&(self.end_ts.wrapping_sub(window.end_ts) as i32))
        {
            return None;
        }
        let first = self.history.front()?;
        if (window.first_ts.wrapping_sub(first.first) as i32) < 0 {
            return None;
        }
        let mut samples = 0;
        let mut energy = 0.0;
        let mut full = 0;
        let mut peak = 0.0f32;
        for e in &self.history {
            if e.first.wrapping_sub(window.first_ts) as i32 >= 0
                && window.end_ts.wrapping_sub(e.end) as i32 >= 0
            {
                samples += e.speech;
                energy += e.energy;
                full += e.near_full;
                peak = peak.max(e.peak);
            }
        }
        Self::level(samples, energy, full, peak)
    }
}

#[derive(Default)]
pub struct PeerTuner {
    pub offset_db: f32,
    hint: Option<(f32, Instant)>,
    streak: u8,
    evidence_ms: u32,
    direction: f32,
    last_used: Option<Instant>,
    decision: Option<&'static str>,
    epoch: u32,
    last_window: Option<AudioWindow>,
    last_action: Option<Instant>,
    tracking: bool,
    reporters: u16,
    agreeing: u16,
    hint_speech_ms: u16,
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
    pub fn agreeing(&self) -> u16 {
        self.agreeing
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
            self.evidence_ms = 0;
            self.reporters = 0;
            self.agreeing = 0;
            self.received_at = None;
        }
    }
    /// Versioned wire hint with explicit consensus and confidence metadata.
    pub fn hint_observation(
        &mut self,
        window: AudioWindow,
        correction: Option<i16>,
        reporters: u16,
        agreeing: u16,
        speech_ms: u16,
        now: Instant,
    ) {
        if agreeing > reporters
            || reporters > icu_proto::limits::MAX_MAX_USERS
            || (correction.is_some()
                && (agreeing <= reporters / 2
                    || !(icu_proto::audio_level::MIN_SPEECH_MS
                        ..=icu_proto::audio_level::MAX_SPEECH_MS)
                        .contains(&speech_ms)))
        {
            return;
        }
        if self.hint_window(window, correction, reporters, now) {
            self.agreeing = agreeing;
            self.hint_speech_ms = speech_ms;
        }
    }
    pub fn hint_window(
        &mut self,
        window: AudioWindow,
        correction: Option<i16>,
        reporters: u16,
        now: Instant,
    ) -> bool {
        if window.gain_epoch != self.epoch
            || reporters > icu_proto::limits::MAX_MAX_USERS
            || correction.is_some_and(|c| !(-1200..=1200).contains(&c))
        {
            return false;
        }
        if correction.is_some() || reporters > 0 {
            if !window.valid()
                || self
                    .last_window
                    .is_some_and(|old| !window.newer_hint_than(old))
            {
                return false;
            }
            self.last_window = Some(window);
        }
        self.reporters = reporters;
        self.agreeing = reporters;
        self.hint(correction, now);
        true
    }
    pub fn hint(&mut self, correction_cdb: Option<i16>, now: Instant) {
        self.received_at = Some(now);
        self.hint_speech_ms = 1000;
        self.hint = correction_cdb
            .filter(|c| (-1200..=1200).contains(c))
            .map(|c| (c as f32 / 100.0, now));
    }
    pub fn window(&self) -> Option<AudioWindow> {
        self.last_window
    }
    /// Production entry point: local evidence must describe exactly the
    /// accepted remote window/generation, never the latest unrelated phrase.
    pub fn tick_matched(
        &mut self,
        local: Option<(AudioLevel, AudioWindow)>,
        enabled: bool,
        now: Instant,
    ) -> f32 {
        let local = local.filter(|(_, window)| Some(*window) == self.last_window);
        self.tick(local.map(|(level, _)| level), enabled, now)
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
            self.evidence_ms = 0;
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
            self.evidence_ms = 0;
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
        let rejected = if remote.abs() <= f32::from(icu_proto::audio_level::ACCEPT_CDB) / 100.0 {
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
            self.evidence_ms = 0;
            self.decision = Some(reason);
            return self.offset_db;
        }
        if self.direction != remote.signum() {
            self.streak = 0;
            self.evidence_ms = 0;
            self.direction = remote.signum();
            self.tracking = false;
        }
        if let Some(seed) = self.candidate_trim.take() {
            if seed.signum() == remote.signum() && seed.abs() >= 0.5 {
                self.tracking = true;
            }
        }
        self.streak = self.streak.saturating_add(1);
        // A 100-ms answer contributes, but cannot carry the confidence of a
        // full phrase. Bound each vote and accumulate only consumed hints.
        self.evidence_ms = self
            .evidence_ms
            .saturating_add(u32::from(l.speech_ms.min(self.hint_speech_ms).min(1000)));
        self.decision = Some("prüft");
        if ((self.streak >= 3 && self.evidence_ms >= 600)
            || (self.tracking && self.evidence_ms >= 200))
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
            self.evidence_ms = 0;
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
    /// Duration of adjacent capture blocks with at least 1% clipped samples.
    pub clip_streak_samples: usize,
    pub max_clip_streak_samples: usize,
}
impl CaptureWindow {
    pub fn observe(&mut self, raw: &[f32], speech: bool, gain_db: f32) {
        if raw.iter().any(|s| !s.is_finite()) {
            self.clip_streak_samples = 0;
            return;
        }
        self.samples += raw.len();
        let clipped = raw.iter().filter(|s| s.abs() >= 0.99).count();
        self.near_full += clipped;
        if !raw.is_empty() && clipped >= raw.len().div_ceil(100) {
            self.clip_streak_samples += raw.len();
            self.max_clip_streak_samples =
                self.max_clip_streak_samples.max(self.clip_streak_samples);
        } else {
            self.clip_streak_samples = 0;
        }
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
    /// Short isolated impacts must not ratchet down the hardware gain.
    /// Without recognized speech, require a longer 100 ms distortion run.
    pub fn sustained_clipping(self) -> bool {
        self.near_full >= 24
            && self.max_clip_streak_samples >= 1_440
            && (self.speech_samples >= 4_800 || self.max_clip_streak_samples >= 4_800)
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
                            assert!((1150..=1200).contains(&level.correction_cdb), "{level:?}");
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
    fn lost_frames_cannot_join_separate_stationary_tones_into_speech() {
        let mut m = Measurement::default();
        let mut phase = 0.0f32;
        for b in 0..1000 {
            // Two packets of one stationary tone, then a missing packet.
            // The mixer excludes PLC/FEC from feedback, so only the sender
            // clock reveals that these are separate 40-ms observations.
            if b % 3 != 2 {
                let hz = 180.0 + 20.0 * ((b / 3) as f32 * 0.2).sin();
                let mut block = [0.0; 960];
                for s in &mut block {
                    phase = (phase + hz / 48_000.0).fract();
                    let p = phase * std::f32::consts::TAU;
                    *s = p.sin() + 0.4 * (2.0 * p).sin() + 0.2 * (3.0 * p).sin();
                }
                let scale = meter::from_db(-34.0) / meter::from_db(meter::measure(&block).rms_db);
                for s in &mut block {
                    *s *= scale;
                }
                m.observe_at(&block, None, 0, b * 960);
            }
            if b % 100 == 99 {
                assert!(
                    m.take_window().is_none(),
                    "a timestamp gap must break the pitch trajectory"
                );
            }
        }
    }

    #[test]
    fn timestamp_gaps_preserve_the_learned_level() {
        let mut m = Measurement::default();
        for b in 0..500 {
            m.observe_at(&[0.02; 960], Some(true), 7, b * 960);
        }
        let learned = m.estimator.level_db();
        assert!(learned < -30.0);
        let correction = m.estimator.correction_db();
        m.observe_at(&[0.0; 960], None, 7, 600 * 960);
        assert_eq!(m.estimator.level_db(), learned);
        assert_eq!(m.estimator.correction_db(), correction);
    }

    #[test]
    fn a_wrapping_sender_clock_keeps_contiguous_voice_evidence() {
        let mut plain = Measurement::default();
        let mut wrapped = Measurement::default();
        let mut phase = 0.0f32;
        let origin = u32::MAX - 48_000;
        for b in 0..400u32 {
            let mut block = [0.0; 960];
            for (i, s) in block.iter_mut().enumerate() {
                let t = (b * 960 + i as u32) as f32 / 48_000.0;
                let hz = 180.0 + 20.0 * (t * std::f32::consts::TAU * 1.3).sin();
                phase = (phase + hz / 48_000.0).fract();
                *s = (phase * std::f32::consts::TAU).sin() * 0.05;
            }
            plain.observe_at(&block, None, 0, b * 960);
            wrapped.observe_at(&block, None, 0, origin.wrapping_add(b * 960));
            if b % 100 == 99 {
                let expected = plain.take_window().expect("continuous voiced signal").0;
                let actual = wrapped.take_window().expect("wrapped voiced signal").0;
                assert_eq!(actual, expected);
            }
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
    #[test]
    fn short_answers_report_true_rms_without_a_cold_envelope_bias() {
        for speech_ms in [200u32, 400, 800] {
            let mut m = Measurement::default();
            for turn in 0..5u32 {
                for b in 0..200u32 {
                    let speech = b < speech_ms / 10;
                    let samples = [if speech { meter::from_db(-30.0) } else { 0.0 }; 480];
                    m.observe_at(&samples, Some(speech), 7, (turn * 200 + b) * 480);
                }
                let (level, window) = m.take_window().expect("short reply must contribute");
                assert_eq!(u32::from(level.speech_ms), speech_ms);
                assert!((level.level_cdb + 3000).abs() <= 1, "{level:?}");
                assert!((level.correction_cdb - 800).abs() <= 1, "{level:?}");
                assert!(window.valid());
            }
        }
    }
    #[test]
    fn matched_history_uses_the_old_phrase_not_the_new_loud_phrase() {
        let mut m = Measurement::default();
        let mut tuner = PeerTuner::default();
        tuner.set_epoch(1);
        let now = Instant::now();
        for turn in 0..3u32 {
            let first = turn * 192_000;
            for b in 0..400u32 {
                let db = if b < 200 { -30.0 } else { -12.0 };
                m.observe_at(&[meter::from_db(db); 480], Some(true), 1, first + b * 480);
            }
            let w = AudioWindow {
                gain_epoch: 1,
                first_ts: first,
                end_ts: first + 96_000,
                report_id: turn,
            };
            let local = m
                .measure_window(w)
                .expect("six-second history holds the phrase");
            assert_eq!(local.correction_cdb, 800);
            let at = now + Duration::from_secs(u64::from(turn) * 4);
            tuner.hint_observation(w, Some(800), 3, 3, 2000, at);
            tuner.tick_matched(Some((local, w)), true, at);
        }
        assert_eq!(tuner.offset_db, 0.5);
        let wrong = AudioWindow {
            gain_epoch: 1,
            first_ts: 0,
            end_ts: 96_000,
            report_id: 0,
        };
        assert!(
            m.measure_window(wrong).is_none(),
            "old scalar evidence expires"
        );
        m.observe_at(&[0.01; 480], Some(true), 2, 600_000);
        assert!(
            m.measure_window(tuner.window().unwrap()).is_none(),
            "old epoch cannot corroborate"
        );
    }
    #[test]
    fn unmatched_windows_do_not_consume_the_hint() {
        let mut p = PeerTuner::default();
        let now = Instant::now();
        for id in 0..3u32 {
            let w = AudioWindow {
                gain_epoch: 0,
                first_ts: id * 96_000,
                end_ts: (id + 1) * 96_000,
                report_id: id,
            };
            let at = now + Duration::from_secs(u64::from(id) * 2);
            p.hint_window(w, Some(400), 1, at);
            let wrong = AudioWindow { gain_epoch: 9, ..w };
            p.tick_matched(Some((level(400), wrong)), true, at);
            assert_eq!(p.offset_db, 0.0);
            p.tick_matched(Some((level(400), w)), true, at);
        }
        assert_eq!(p.offset_db, 0.5);
    }
    #[test]
    fn acceptance_band_is_applied_once_to_raw_target_error() {
        for (error, expected) in [
            (149, 0.0),
            (150, 0.0),
            (151, 0.5),
            (-150, 0.0),
            (-151, -0.5),
        ] {
            let mut p = PeerTuner::default();
            let now = Instant::now();
            for i in 0..3 {
                let at = now + Duration::from_secs(i * 2);
                p.hint(Some(error), at);
                p.tick(Some(level(error)), true, at);
            }
            assert_eq!(p.offset_db, expected, "raw error {error}");
        }
    }
    #[test]
    fn short_observations_are_weighted_and_silent_gaps_expire_them() {
        let mut p = PeerTuner::default();
        let now = Instant::now();
        for id in 0..6u32 {
            let w = AudioWindow {
                gain_epoch: 0,
                first_ts: id * 96_000,
                end_ts: id * 96_000 + 4800,
                report_id: id,
            };
            let at = now + Duration::from_secs(u64::from(id) * 2);
            p.hint_observation(w, Some(400), 3, 3, 100, at);
            p.tick_matched(
                Some((
                    AudioLevel {
                        speech_ms: 100,
                        ..level(400)
                    },
                    w,
                )),
                true,
                at,
            );
            assert_eq!(p.offset_db, if id < 5 { 0.0 } else { 0.5 });
        }
        let mut m = Measurement::default();
        m.observe_at(&[0.02; 3840], Some(true), 1, 0);
        assert!(m.take_window().is_none());
        m.observe_at(&[0.02; 3840], Some(true), 1, 144_000);
        assert!(
            m.take_window().is_none(),
            "old 80ms cannot combine across long pause"
        );
        assert!(m.history.len() <= 600);
    }
}
