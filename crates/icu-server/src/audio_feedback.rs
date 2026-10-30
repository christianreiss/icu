//! Bounded, epoch-attributed speech feedback. A receipt is not a measurement.
use icu_proto::{AudioHeader, AudioLevel, AudioWindow, ControlMsg, Session};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
#[derive(Clone, Copy)]
struct Source {
    epoch: u32,
    ts: u32,
    at: Instant,
}
#[derive(Clone, Copy)]
struct Report {
    level: AudioLevel,
    window: AudioWindow,
    at: Instant,
}
#[derive(Default)]
pub struct Feedback {
    caps: HashMap<Session, u32>,
    sources: HashMap<Session, Source>,
    reports: HashMap<(Session, Session), Report>,
    seen: HashMap<(Session, Session), AudioWindow>,
    last_hint: HashMap<Session, AudioWindow>,
    consumed: HashMap<(Session, Session), AudioWindow>,
    last_tick: Option<Instant>,
}
impl Feedback {
    pub fn join(&mut self, session: Session, caps: u32) {
        self.caps.insert(session, caps);
    }
    pub fn forget(&mut self, session: Session) {
        self.caps.remove(&session);
        self.sources.remove(&session);
        self.last_hint.remove(&session);
        self.reports
            .retain(|(s, r), _| *s != session && *r != session);
        self.seen.retain(|(s, r), _| *s != session && *r != session);
        self.consumed
            .retain(|(s, r), _| *s != session && *r != session);
    }
    fn capable(&self, s: Session) -> bool {
        self.caps
            .get(&s)
            .is_some_and(|c| c & icu_proto::CAP_AUDIO_FEEDBACK != 0)
    }
    /// Called only for authenticated, admitted audio datagrams.
    pub fn observe_audio(&mut self, speaker: Session, header: AudioHeader, now: Instant) {
        if !self.capable(speaker) {
            return;
        }
        if let Some(old) = self.sources.get(&speaker) {
            if header.gain_epoch != old.epoch {
                if header.gain_epoch.wrapping_sub(old.epoch) as i32 <= 0 {
                    return;
                }
                self.reports.retain(|(s, _), _| *s != speaker);
                self.seen.retain(|(s, _), _| *s != speaker);
                self.consumed.retain(|(s, _), _| *s != speaker);
                self.last_hint.remove(&speaker);
            } else if (header.ts.wrapping_sub(old.ts) as i32) < 0 {
                return;
            }
        }
        self.sources.insert(
            speaker,
            Source {
                epoch: header.gain_epoch,
                ts: header.ts,
                at: now,
            },
        );
    }
    pub fn report(
        &mut self,
        reporter: Session,
        speaker: Session,
        level: AudioLevel,
        window: AudioWindow,
        now: Instant,
    ) {
        if reporter == speaker
            || !self.capable(reporter)
            || !self.capable(speaker)
            || !level.valid()
            || !window.valid()
            || u32::from(level.speech_ms) * 48 > window.end_ts.wrapping_sub(window.first_ts)
        {
            return;
        }
        let Some(source) = self.sources.get(&speaker) else {
            return;
        };
        let age = source.ts.wrapping_sub(window.end_ts) as i32;
        if source.epoch != window.gain_epoch
            || !(-5760..=288_000).contains(&age)
            || now.saturating_duration_since(source.at) >= Duration::from_secs(6)
        {
            return;
        }
        let key = (speaker, reporter);
        if self
            .seen
            .get(&key)
            .is_some_and(|old| !window.newer_than(*old))
            || self
                .reports
                .get(&key)
                .is_some_and(|r| now.saturating_duration_since(r.at) < Duration::from_secs(2))
        {
            return;
        }
        self.seen.insert(key, window);
        self.reports.insert(
            key,
            Report {
                level,
                window,
                at: now,
            },
        );
    }
    pub fn tick(
        &mut self,
        now: Instant,
        eligible: impl Fn(Session, Session) -> bool,
    ) -> Vec<(Session, ControlMsg)> {
        self.reports.retain(|(s, r), report| {
            now.saturating_duration_since(report.at) < Duration::from_secs(6)
                && self.sources.get(s).is_some_and(|source| {
                    source.epoch == report.window.gain_epoch
                        && now.saturating_duration_since(source.at) < Duration::from_secs(6)
                        && (-5760..=icu_proto::audio_level::MAX_AGE_SAMPLES as i32)
                            .contains(&(source.ts.wrapping_sub(report.window.end_ts) as i32))
                })
                && eligible(*r, *s)
        });
        if self
            .last_tick
            .is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(2))
        {
            return vec![];
        }
        self.last_tick = Some(now);
        let speakers: Vec<_> = self
            .caps
            .keys()
            .copied()
            .filter(|s| self.capable(*s))
            .collect();
        let mut out = vec![];
        for speaker in speakers {
            let current: Vec<_> = self
                .reports
                .iter()
                .filter(|(key, report)| {
                    key.0 == speaker
                        && self
                            .consumed
                            .get(key)
                            .is_none_or(|old| report.window.newer_than(*old))
                })
                .map(|(key, report)| (*key, *report))
                .collect();
            let previous = self.last_hint.get(&speaker).copied();
            let Some((_, newest)) = current.iter().min_by_key(|(_, r)| {
                self.sources[&speaker].ts.wrapping_sub(r.window.end_ts) as i32
            }) else {
                // Already-consumed votes are never re-emitted. An empty live
                // set still explicitly reports expiry to the sender.
                if !self.reports.keys().any(|(s, _)| *s == speaker) {
                    out.push((
                        speaker,
                        ControlMsg::AudioLevelHint {
                            window: AudioWindow {
                                gain_epoch: self.sources.get(&speaker).map_or(0, |s| s.epoch),
                                ..Default::default()
                            },
                            correction_cdb: None,
                            reporters: 0,
                            agreeing: 0,
                            speech_ms: 0,
                        },
                    ));
                }
                continue;
            };
            // A newly joined listener's old phrase is not a fresh correction.
            // End progression permits shifted windows; a global FIRST-sample
            // watermark would starve every reporter except the latest phase.
            if previous.is_some_and(|old| newest.window.end_ts.wrapping_sub(old.end_ts) as i32 <= 0)
            {
                continue;
            }
            let reports: Vec<_> = current
                .iter()
                .filter(|(_, report)| comparable(report.window, newest.window))
                .collect();
            let values: Vec<_> = reports
                .iter()
                .map(|(_, r)| r.level.correction_cdb)
                .collect();
            let correction = consensus(&values);
            let agreeing: Vec<_> = reports
                .iter()
                .filter(|(_, r)| {
                    correction.is_some_and(|c| {
                        (i32::from(r.level.correction_cdb) - i32::from(c)).abs() <= 100
                    })
                })
                .collect();
            let representative = agreeing
                .iter()
                .min_by_key(|(_, r)| self.sources[&speaker].ts.wrapping_sub(r.window.end_ts) as i32)
                .map_or(newest, |(_, report)| report);
            let mut window = representative.window;
            window.report_id = previous.map_or(0, |old| old.report_id.wrapping_add(1));
            // Each reporter owns its consumption cursor. Consume conflicting
            // observations too: repeatedly polling is not new evidence.
            for (key, report) in &reports {
                self.consumed.insert(*key, report.window);
            }
            // It is possible that only a newer outlier advanced. Do not turn
            // the older majority into another gain confirmation.
            if correction.is_some()
                && previous.is_some_and(|old| window.end_ts.wrapping_sub(old.end_ts) as i32 <= 0)
            {
                continue;
            }
            self.last_hint.insert(speaker, window);
            out.push((
                speaker,
                ControlMsg::AudioLevelHint {
                    window,
                    correction_cdb: correction,
                    reporters: reports.len() as u16,
                    agreeing: agreeing.len() as u16,
                    // Confidence is capped by the weakest agreeing observation;
                    // several receivers do not multiply suitable speech duration.
                    speech_ms: agreeing
                        .iter()
                        .map(|(_, r)| r.level.speech_ms)
                        .min()
                        .unwrap_or(0),
                },
            ));
        }
        out
    }
}
/// Compare observation intervals in the newest window's clock domain, including
/// wraparound. Require at least half the shorter window in common.
fn comparable(a: AudioWindow, b: AudioWindow) -> bool {
    if a.gain_epoch != b.gain_epoch {
        return false;
    }
    let a_start = a.first_ts.wrapping_sub(b.first_ts) as i32;
    let a_end = a.end_ts.wrapping_sub(b.first_ts) as i32;
    let b_end = b.end_ts.wrapping_sub(b.first_ts) as i32;
    let overlap = a_end.min(b_end) - a_start.max(0);
    let minimum = (a
        .end_ts
        .wrapping_sub(a.first_ts)
        .min(b.end_ts.wrapping_sub(b.first_ts))
        / 2) as i32;
    overlap >= minimum
}

fn consensus(values: &[i16]) -> Option<i16> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let median = if v.len().is_multiple_of(2) {
        ((v[v.len() / 2 - 1] as i32 + v[v.len() / 2] as i32) / 2) as i16
    } else {
        v[v.len() / 2]
    };
    // Agreement has a 2 dB total span, not a 4 dB interval around the median.
    let agreeing = v
        .iter()
        .filter(|x| (**x as i32 - median as i32).abs() <= 100)
        .count();
    (agreeing > v.len() / 2).then_some(median)
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
    fn window(epoch: u32, id: u32) -> AudioWindow {
        AudioWindow {
            gain_epoch: epoch,
            first_ts: id * 96_000,
            end_ts: (id + 1) * 96_000,
            report_id: id,
        }
    }
    fn audio(f: &mut Feedback, epoch: u32, id: u32, now: Instant) {
        f.observe_audio(
            1,
            AudioHeader {
                gain_epoch: epoch,
                ts: (id + 1) * 96_000,
                ..Default::default()
            },
            now,
        );
    }
    #[test]
    fn newest_hint_window_can_end_just_ahead_of_last_datagram() {
        // Repeat across the sender sample clock wrap (about 25 hours).
        for base in [0u32, u32::MAX - 96_479] {
            let now = Instant::now();
            let mut feedback = Feedback::default();
            for session in [1, 2, 3] {
                feedback.join(session, icu_proto::CAP_AUDIO_FEEDBACK);
            }
            let first = AudioWindow {
                first_ts: base,
                end_ts: base.wrapping_add(96_000),
                ..window(1, 0)
            };
            let newest = AudioWindow {
                first_ts: first.first_ts.wrapping_add(960),
                end_ts: first.end_ts.wrapping_add(960),
                ..first
            };
            feedback.observe_audio(
                1,
                AudioHeader {
                    gain_epoch: 1,
                    ts: first.end_ts,
                    ..Default::default()
                },
                now,
            );
            feedback.report(2, 1, level(400), first, now);
            feedback.report(3, 1, level(400), newest, now);
            let hint = feedback
                .tick(now, |_, _| true)
                .into_iter()
                .find(|(speaker, _)| *speaker == 1)
                .unwrap()
                .1;
            match hint {
                ControlMsg::AudioLevelHint {
                    window,
                    correction_cdb: Some(400),
                    ..
                } => assert_eq!(window.end_ts, newest.end_ts),
                _ => panic!("expected agreeing feedback"),
            }
        }
    }

    #[test]
    fn hint_sequence_wraps_and_restarts_only_for_a_new_gain_epoch() {
        let now = Instant::now();
        let mut feedback = Feedback::default();
        for session in [1, 2] {
            feedback.join(session, icu_proto::CAP_AUDIO_FEEDBACK);
        }
        audio(&mut feedback, 7, 0, now);
        let previous = AudioWindow {
            report_id: u32::MAX,
            ..window(7, 0)
        };
        feedback.last_hint.insert(1, previous);
        audio(&mut feedback, 7, 1, now);
        feedback.report(
            2,
            1,
            level(400),
            AudioWindow {
                report_id: 0,
                ..window(7, 1)
            },
            now,
        );
        let hint = feedback
            .tick(now, |_, _| true)
            .into_iter()
            .find(|(speaker, _)| *speaker == 1)
            .unwrap()
            .1;
        match hint {
            ControlMsg::AudioLevelHint {
                window,
                correction_cdb: Some(400),
                ..
            } => {
                assert_eq!(window.report_id, 0);
                assert!(window.newer_than(previous));
            }
            _ => panic!("expected feedback across counter wrap"),
        }
        let later = now + Duration::from_secs(2);
        audio(&mut feedback, 8, 2, later);
        feedback.report(2, 1, level(400), window(7, 2), later);
        assert!(
            feedback.reports.is_empty(),
            "old gain epoch remains rejected"
        );
        feedback.report(
            2,
            1,
            level(400),
            AudioWindow {
                report_id: 0,
                ..window(8, 2)
            },
            later,
        );
        let hint = feedback
            .tick(later, |_, _| true)
            .into_iter()
            .find(|(speaker, _)| *speaker == 1)
            .unwrap()
            .1;
        assert!(matches!(
            hint,
            ControlMsg::AudioLevelHint {
                window: AudioWindow {
                    gain_epoch: 8,
                    report_id: 0,
                    ..
                },
                correction_cdb: Some(400),
                ..
            }
        ));
    }

    #[test]
    fn delayed_duplicate_overlapping_and_old_epoch_reports_never_reconfirm() {
        let now = Instant::now();
        let mut f = Feedback::default();
        for s in 1..=4 {
            f.join(s, icu_proto::CAP_AUDIO_FEEDBACK);
        }
        audio(&mut f, 7, 0, now);
        f.report(2, 1, level(400), window(7, 0), now);
        assert!(f.tick(now, |_, _| true).iter().any(|(s, h)| *s == 1
            && matches!(
                h,
                ControlMsg::AudioLevelHint {
                    correction_cdb: Some(400),
                    ..
                }
            )));
        let later = now + Duration::from_secs(2);
        f.report(3, 1, level(400), window(7, 0), later);
        f.report(2, 1, level(400), window(7, 0), later);
        assert!(f.tick(later, |_, _| true).iter().all(|(s, _)| *s != 1));
        audio(&mut f, 8, 1, later);
        f.report(2, 1, level(400), window(7, 1), later);
        assert!(f.reports.is_empty());
        f.report(2, 1, level(400), window(8, 1), later);
        assert_eq!(f.reports.len(), 1);
        f.forget(1);
        f.join(1, icu_proto::CAP_AUDIO_FEEDBACK);
        assert!(!f.sources.contains_key(&1));
        assert!(f.seen.is_empty());
    }
    #[test]
    fn conflict_outlier_expiry_and_capability_are_bounded() {
        assert_eq!(consensus(&[400, -400]), None);
        assert_eq!(consensus(&[400, 420, -1200]), Some(400));
        let t = Instant::now();
        let mut f = Feedback::default();
        f.join(1, 1);
        f.join(2, 1);
        f.join(3, 0);
        audio(&mut f, 1, 0, t);
        f.report(3, 1, level(400), window(1, 0), t);
        assert!(f.reports.is_empty());
        f.report(2, 1, level(400), window(1, 0), t);
        f.tick(t, |_, _| true);
        assert!(f
            .tick(t + Duration::from_secs(6), |_, _| true)
            .iter()
            .any(|(s, h)| *s == 1
                && matches!(
                    h,
                    ControlMsg::AudioLevelHint {
                        correction_cdb: None,
                        ..
                    }
                )));
    }
    #[test]
    fn a_new_consensus_cannot_count_a_consumed_vote_again() {
        let t = Instant::now();
        let mut f = Feedback::default();
        for s in 1..=3 {
            f.join(s, 1);
        }
        audio(&mut f, 2, 0, t);
        f.report(2, 1, level(400), window(2, 0), t);
        f.report(3, 1, level(400), window(2, 0), t);
        f.tick(t, |_, _| true);
        let later = t + Duration::from_secs(2);
        audio(&mut f, 2, 1, later);
        f.report(2, 1, level(400), window(2, 1), later);
        assert!(f.tick(later, |_, _| true).iter().any(|(s, h)| *s == 1
            && matches!(
                h,
                ControlMsg::AudioLevelHint {
                    correction_cdb: Some(400),
                    reporters: 1,
                    ..
                }
            )));
    }
    #[test]
    fn shifted_windows_keep_all_three_reporters_across_ticks_and_clock_wrap() {
        for base in [0u32, u32::MAX - 192_000] {
            let now = Instant::now();
            let mut f = Feedback::default();
            for session in 1..=4 {
                f.join(session, icu_proto::CAP_AUDIO_FEEDBACK);
            }
            for cycle in 0..12u32 {
                let end = base.wrapping_add((cycle + 1) * 96_000 + 24_000);
                let at = now + Duration::from_millis(u64::from(cycle) * 2100);
                f.observe_audio(
                    1,
                    AudioHeader {
                        gain_epoch: 1,
                        ts: end,
                        ..Default::default()
                    },
                    at,
                );
                // Distinct report timers, delivery delays and a late join.
                for (reporter, phase, delay) in [(2, 0, 30), (3, 12_000, 80), (4, 24_000, 150)] {
                    if reporter == 4 && cycle < 2 {
                        continue;
                    }
                    let first_ts = base.wrapping_add(cycle * 96_000 + phase);
                    let w = AudioWindow {
                        gain_epoch: 1,
                        first_ts,
                        end_ts: first_ts.wrapping_add(96_000),
                        report_id: cycle,
                    };
                    f.report(
                        reporter,
                        1,
                        level(400),
                        w,
                        at + Duration::from_millis(delay),
                    );
                }
                let hints = f.tick(at + Duration::from_millis(200), |_, _| true);
                let hint = hints.into_iter().find(|(s, _)| *s == 1).unwrap().1;
                assert!(
                    matches!(hint, ControlMsg::AudioLevelHint {
                    correction_cdb: Some(400), reporters, agreeing, ..
                } if reporters == if cycle < 2 { 2 } else { 3 } && agreeing == reporters),
                    "cycle {cycle}: {hint:?}"
                );
            }
        }
    }
    #[test]
    fn short_nonoverlapping_phrases_are_not_a_consensus() {
        let now = Instant::now();
        let mut f = Feedback::default();
        for session in 1..=4 {
            f.join(session, icu_proto::CAP_AUDIO_FEEDBACK);
        }
        f.observe_audio(
            1,
            AudioHeader {
                gain_epoch: 1,
                ts: 96_000,
                ..Default::default()
            },
            now,
        );
        for (reporter, first_ts) in [(2, 0), (3, 24_000), (4, 48_000)] {
            f.report(
                reporter,
                1,
                AudioLevel {
                    speech_ms: 200,
                    ..level(400)
                },
                AudioWindow {
                    gain_epoch: 1,
                    first_ts,
                    end_ts: first_ts + 9600,
                    report_id: 0,
                },
                now,
            );
        }
        let hints = f.tick(now, |_, _| true);
        assert!(hints.iter().any(|(s, h)| *s == 1
            && matches!(
                h,
                ControlMsg::AudioLevelHint {
                    reporters: 1,
                    agreeing: 1,
                    speech_ms: 200,
                    ..
                }
            )));
    }
    #[test]
    fn conflicts_consume_votes_and_expired_source_cannot_reconfirm() {
        let now = Instant::now();
        let mut f = Feedback::default();
        for session in 1..=3 {
            f.join(session, icu_proto::CAP_AUDIO_FEEDBACK);
        }
        audio(&mut f, 1, 0, now);
        f.report(2, 1, level(400), window(1, 0), now);
        f.report(3, 1, level(-400), window(1, 0), now);
        assert!(f.tick(now, |_, _| true).iter().any(|(s, h)| *s == 1
            && matches!(
                h,
                ControlMsg::AudioLevelHint {
                    correction_cdb: None,
                    reporters: 2,
                    agreeing: 0,
                    ..
                }
            )));
        assert!(f
            .tick(now + Duration::from_secs(2), |_, _| true)
            .iter()
            .all(|(s, _)| *s != 1));
        assert!(f
            .tick(now + Duration::from_secs(6), |_, _| true)
            .iter()
            .all(|(_, h)| matches!(
                h,
                ControlMsg::AudioLevelHint {
                    correction_cdb: None,
                    ..
                }
            )));
    }
}
