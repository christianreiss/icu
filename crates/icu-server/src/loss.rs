//! `LossReport` aggregation and `LossHint` generation
//! (`PROTOCOL.md` §2.2, `Server/IMPLEMENTATION.md` §6.5).
//!
//! Every receiver reports the loss it measures on a speaker's audio stream.
//! The server forwards the **maximum** over all receivers to that speaker, and
//! sends `LossHint { 0 }` once all reports for a speaker have expired.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use icu_proto::Session;

/// Per-speaker loss aggregation.
#[derive(Debug, Default)]
pub struct LossTracker {
    /// (speaker, reporter) -> (per mille, when)
    reports: HashMap<(Session, Session), (u16, Instant)>,
    /// The last hint actually sent per speaker.
    sent: HashMap<Session, u16>,
}

impl LossTracker {
    /// Records a report. Returns the hint to send when the value changed.
    pub fn report(
        &mut self,
        speaker: Session,
        reporter: Session,
        loss_permille: u16,
        ttl: Duration,
    ) -> Option<u16> {
        self.report_at(speaker, reporter, loss_permille, ttl, Instant::now())
    }

    /// [`LossTracker::report`] with an explicit clock, for tests.
    pub fn report_at(
        &mut self,
        speaker: Session,
        reporter: Session,
        loss_permille: u16,
        ttl: Duration,
        now: Instant,
    ) -> Option<u16> {
        self.reports
            .insert((speaker, reporter), (loss_permille, now));
        self.recompute(speaker, Some(ttl), now)
    }

    /// Drops expired reports and returns every hint that changed as a result.
    pub fn expire(&mut self, ttl: Duration) -> Vec<(Session, u16)> {
        self.expire_at(ttl, Instant::now())
    }

    /// [`LossTracker::expire`] with an explicit clock, for tests.
    pub fn expire_at(&mut self, ttl: Duration, now: Instant) -> Vec<(Session, u16)> {
        let mut touched: Vec<Session> = Vec::new();
        self.reports.retain(|(speaker, _), (_, when)| {
            let alive = now.saturating_duration_since(*when) < ttl;
            if !alive && !touched.contains(speaker) {
                touched.push(*speaker);
            }
            alive
        });
        touched
            .into_iter()
            .filter_map(|speaker| {
                self.recompute(speaker, Some(ttl), now)
                    .map(|value| (speaker, value))
            })
            .collect()
    }

    /// Forgets everything about a session, as speaker and as reporter.
    ///
    /// Returns the hints that changed as a result: when the leaving client was
    /// the only one reporting loss on a speaker, that speaker has to learn it
    /// can stop paying for FEC, exactly as on expiry.
    pub fn forget(&mut self, session: Session) -> Vec<(Session, u16)> {
        let mut touched: Vec<Session> = Vec::new();
        self.reports.retain(|(speaker, reporter), _| {
            let keep = *speaker != session && *reporter != session;
            if !keep && *speaker != session && !touched.contains(speaker) {
                touched.push(*speaker);
            }
            keep
        });
        self.sent.remove(&session);
        let now = Instant::now();
        touched
            .into_iter()
            .filter_map(|speaker| self.recompute(speaker, None, now).map(|v| (speaker, v)))
            .collect()
    }

    /// The maximum live report for a speaker, or `None` when it did not change
    /// since the last hint. `ttl` of `None` counts every report regardless of
    /// age, for callers that have just pruned the map themselves.
    fn recompute(&mut self, speaker: Session, ttl: Option<Duration>, now: Instant) -> Option<u16> {
        let max = self
            .reports
            .iter()
            .filter(|((s, _), (_, when))| {
                *s == speaker && ttl.is_none_or(|ttl| now.saturating_duration_since(*when) < ttl)
            })
            .map(|(_, (value, _))| *value)
            .max()
            .unwrap_or(0);
        match self.sent.get(&speaker) {
            Some(prev) if *prev == max => None,
            _ => {
                self.sent.insert(speaker, max);
                Some(max)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(6);

    #[test]
    fn the_maximum_over_all_receivers_wins() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        assert_eq!(tracker.report_at(1, 2, 30, TTL, t0), Some(30));
        assert_eq!(tracker.report_at(1, 3, 80, TTL, t0), Some(80));
        assert_eq!(
            tracker.report_at(1, 2, 40, TTL, t0),
            None,
            "still below the maximum, nothing changes"
        );
        assert_eq!(
            tracker.report_at(1, 3, 10, TTL, t0),
            Some(40),
            "new maximum"
        );
    }

    #[test]
    fn an_unchanged_value_sends_no_hint() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        assert_eq!(tracker.report_at(1, 2, 50, TTL, t0), Some(50));
        assert_eq!(tracker.report_at(1, 2, 50, TTL, t0), None);
    }

    /// §10: once every report for a speaker has expired, the server sends
    /// `LossHint { 0 }`.
    #[test]
    fn expiry_of_all_reports_sends_zero() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        tracker.report_at(1, 2, 90, TTL, t0);
        assert!(tracker
            .expire_at(TTL, t0 + Duration::from_secs(5))
            .is_empty());
        assert_eq!(
            tracker.expire_at(TTL, t0 + Duration::from_secs(6)),
            vec![(1, 0)]
        );
        assert!(
            tracker
                .expire_at(TTL, t0 + Duration::from_secs(7))
                .is_empty(),
            "only once"
        );
    }

    #[test]
    fn expiry_of_one_report_falls_back_to_the_others() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        tracker.report_at(1, 2, 90, TTL, t0);
        tracker.report_at(1, 3, 20, TTL, t0 + Duration::from_secs(4));
        let out = tracker.expire_at(TTL, t0 + Duration::from_secs(7));
        assert_eq!(out, vec![(1, 20)], "the loud reporter aged out");
    }

    #[test]
    fn forgetting_a_session_removes_it_from_both_sides() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        tracker.report_at(1, 2, 90, TTL, t0);
        tracker.report_at(2, 1, 70, TTL, t0);
        assert_eq!(
            tracker.forget(2),
            vec![(1, 0)],
            "speaker 1 lost its only reporter"
        );
        assert!(tracker.reports.is_empty());
        assert_eq!(tracker.report_at(1, 3, 90, TTL, t0), Some(90));
    }
}
