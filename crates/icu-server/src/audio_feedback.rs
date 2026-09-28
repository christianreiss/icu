//! Capability-gated, bounded peer speech measurements. Never uses loss voting.
use icu_proto::{AudioLevel, ControlMsg, Session};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Feedback {
    caps: HashMap<Session, u32>,
    reports: HashMap<(Session, Session), (AudioLevel, Instant)>,
    last_tick: Option<Instant>,
}

impl Feedback {
    pub fn join(&mut self, session: Session, caps: u32) {
        self.caps.insert(session, caps);
    }
    pub fn forget(&mut self, session: Session) {
        self.caps.remove(&session);
        self.reports
            .retain(|(s, r), _| *s != session && *r != session);
    }
    fn capable(&self, s: Session) -> bool {
        self.caps
            .get(&s)
            .is_some_and(|c| c & icu_proto::CAP_AUDIO_FEEDBACK != 0)
    }
    pub fn report(&mut self, reporter: Session, speaker: Session, level: AudioLevel, now: Instant) {
        if reporter == speaker
            || !self.capable(reporter)
            || !self.capable(speaker)
            || !level.valid()
        {
            return;
        }
        if self
            .reports
            .get(&(speaker, reporter))
            .is_some_and(|(_, t)| now.saturating_duration_since(*t) < Duration::from_secs(2))
        {
            return;
        }
        self.reports.insert((speaker, reporter), (level, now));
    }
    pub fn tick(
        &mut self,
        now: Instant,
        eligible: impl Fn(Session, Session) -> bool,
    ) -> Vec<(Session, ControlMsg)> {
        self.reports.retain(|(s, r), (_, t)| {
            now.saturating_duration_since(*t) < Duration::from_secs(6) && eligible(*r, *s)
        });
        if self
            .last_tick
            .is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(2))
        {
            return Vec::new();
        }
        self.last_tick = Some(now);
        self.caps
            .keys()
            .filter(|s| self.capable(**s))
            .map(|s| {
                let values: Vec<_> = self
                    .reports
                    .iter()
                    .filter(|((speaker, _), _)| speaker == s)
                    .map(|(_, (l, _))| l.correction_cdb)
                    .collect();
                (
                    *s,
                    ControlMsg::AudioLevelHint {
                        correction_cdb: consensus(&values),
                        reporters: values.len().min(u16::MAX as usize) as u16,
                    },
                )
            })
            .collect()
    }
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
    #[test]
    fn consensus_rejects_conflict_and_outliers() {
        assert_eq!(consensus(&[]), None);
        assert_eq!(consensus(&[400]), Some(400));
        assert_eq!(consensus(&[400, -400]), None);
        assert_eq!(consensus(&[400, 500]), Some(450));
        assert_eq!(consensus(&[400, 420, -1200]), Some(400));
    }
    #[test]
    fn bounds_capabilities_refresh_and_expiry() {
        let t = Instant::now();
        let mut f = Feedback::default();
        f.join(1, icu_proto::CAP_AUDIO_FEEDBACK);
        f.join(2, icu_proto::CAP_AUDIO_FEEDBACK);
        f.join(3, 0);
        let l = AudioLevel {
            level_cdb: -2600,
            correction_cdb: 200,
            peak_cdb: -1000,
            near_full_permille: 0,
            speech_ms: 1000,
        };
        f.report(3, 1, l, t);
        f.report(2, 2, l, t);
        f.report(2, 99, l, t);
        assert!(f.reports.is_empty());
        f.report(2, 1, l, t);
        f.report(
            2,
            1,
            AudioLevel {
                correction_cdb: 900,
                ..l
            },
            t,
        );
        assert_eq!(f.reports[&(1, 2)].0, l);
        assert!(f.tick(t, |_, _| true).contains(&(
            1,
            ControlMsg::AudioLevelHint {
                correction_cdb: Some(200),
                reporters: 1
            }
        )));
        assert!(f.tick(t + Duration::from_secs(6), |_, _| true).contains(&(
            1,
            ControlMsg::AudioLevelHint {
                correction_cdb: None,
                reporters: 0
            }
        )));
        f.report(2, 1, l, t + Duration::from_secs(7));
        f.forget(2);
        assert!(f.reports.is_empty());
    }
}
