//! `LossReport` aggregation and `LossHint` generation
//! (`docs/reference/protocol.md` §2.2, `docs/reference/server.md` §6.5).
//!
//! Every receiver reports the loss it measures on a speaker's audio stream.
//! The server forwards to that speaker the value more than a third of its
//! listeners agree on ([`agreed`]), and sends `LossHint { 0 }` once all
//! reports for a speaker have expired.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use icu_proto::Session;

/// The hint for a sender from what its receivers measured (`docs/reference/protocol.md`
/// §2.2 Nr. 12 and 16): the largest value that more than a third of its
/// `receivers` measured at least, i.e. the (⌊n/3⌋ + 1)-th largest of n. A
/// receiver without a value in `values` counts as 0, so `values` may be
/// shorter than `receivers`; a longer one counts in full.
///
/// Up to two receivers that is the maximum, as it always was; three to five
/// take the second largest, six to eight the third. The maximum made the
/// worst receiver everyone's: the loss a receiver measures is mostly its own
/// downlink, and one viewer on a bad line — or one who lies — raised every
/// speaker's FEC and stepped down or paused every sender's video for the
/// whole room (external review 2026-09-27, D-1). That receiver is still
/// served: the relay thins video per pair, and its own jitter buffer and
/// packet loss concealment cover its audio.
pub fn agreed(mut values: Vec<u16>, receivers: usize) -> u16 {
    let n = receivers.max(values.len());
    values.sort_unstable_by(|a, b| b.cmp(a));
    values.get(n / 3).copied().unwrap_or(0)
}

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
    /// `listeners` says how many sessions hear `speaker` right now
    /// (`Room::audio_listeners`): those without a report count as no loss.
    pub fn report(
        &mut self,
        speaker: Session,
        reporter: Session,
        loss_permille: u16,
        ttl: Duration,
        listeners: impl Fn(Session) -> usize,
    ) -> Option<u16> {
        self.report_at(
            speaker,
            reporter,
            loss_permille,
            ttl,
            Instant::now(),
            listeners,
        )
    }

    /// [`LossTracker::report`] with an explicit clock, for tests.
    pub fn report_at(
        &mut self,
        speaker: Session,
        reporter: Session,
        loss_permille: u16,
        ttl: Duration,
        now: Instant,
        listeners: impl Fn(Session) -> usize,
    ) -> Option<u16> {
        self.reports
            .insert((speaker, reporter), (loss_permille, now));
        self.recompute(speaker, Some(ttl), now, listeners(speaker))
    }

    /// Drops expired reports and returns every hint that changed as a result.
    pub fn expire(
        &mut self,
        ttl: Duration,
        listeners: impl Fn(Session) -> usize,
    ) -> Vec<(Session, u16)> {
        self.expire_at(ttl, Instant::now(), listeners)
    }

    /// [`LossTracker::expire`] with an explicit clock, for tests.
    pub fn expire_at(
        &mut self,
        ttl: Duration,
        now: Instant,
        listeners: impl Fn(Session) -> usize,
    ) -> Vec<(Session, u16)> {
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
                self.recompute(speaker, Some(ttl), now, listeners(speaker))
                    .map(|value| (speaker, value))
            })
            .collect()
    }

    /// Forgets everything about a session, as speaker and as reporter.
    ///
    /// Returns the hints that changed as a result: when the leaving client was
    /// the only one reporting loss on a speaker, that speaker has to learn it
    /// can stop paying for FEC, exactly as on expiry.
    pub fn forget(
        &mut self,
        session: Session,
        listeners: impl Fn(Session) -> usize,
    ) -> Vec<(Session, u16)> {
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
            .filter_map(|speaker| {
                self.recompute(speaker, None, now, listeners(speaker))
                    .map(|v| (speaker, v))
            })
            .collect()
    }

    /// The hint from the live reports for a speaker heard by `listeners`
    /// ([`agreed`]), or `None` when it did not change since the last hint.
    /// `ttl` of `None` counts every report regardless of age, for callers that
    /// have just pruned the map themselves.
    fn recompute(
        &mut self,
        speaker: Session,
        ttl: Option<Duration>,
        now: Instant,
        listeners: usize,
    ) -> Option<u16> {
        let live: Vec<u16> = self
            .reports
            .iter()
            .filter(|((s, _), (_, when))| {
                *s == speaker && ttl.is_none_or(|ttl| now.saturating_duration_since(*when) < ttl)
            })
            .map(|(_, (value, _))| *value)
            .collect();
        let hint = agreed(live, listeners);
        match self.sent.get(&speaker) {
            Some(prev) if *prev == hint => None,
            _ => {
                self.sent.insert(speaker, hint);
                Some(hint)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(6);

    /// Two listeners per speaker: up to two, the rule is the maximum.
    fn two(_: Session) -> usize {
        2
    }

    #[test]
    fn with_two_listeners_the_maximum_wins() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        assert_eq!(tracker.report_at(1, 2, 30, TTL, t0, two), Some(30));
        assert_eq!(tracker.report_at(1, 3, 80, TTL, t0, two), Some(80));
        assert_eq!(
            tracker.report_at(1, 2, 40, TTL, t0, two),
            None,
            "still below the maximum, nothing changes"
        );
        assert_eq!(
            tracker.report_at(1, 3, 10, TTL, t0, two),
            Some(40),
            "new maximum"
        );
    }

    #[test]
    fn an_unchanged_value_sends_no_hint() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        assert_eq!(tracker.report_at(1, 2, 50, TTL, t0, two), Some(50));
        assert_eq!(tracker.report_at(1, 2, 50, TTL, t0, two), None);
    }

    /// §10: once every report for a speaker has expired, the server sends
    /// `LossHint { 0 }`.
    #[test]
    fn expiry_of_all_reports_sends_zero() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        tracker.report_at(1, 2, 90, TTL, t0, two);
        assert!(tracker
            .expire_at(TTL, t0 + Duration::from_secs(5), two)
            .is_empty());
        assert_eq!(
            tracker.expire_at(TTL, t0 + Duration::from_secs(6), two),
            vec![(1, 0)]
        );
        assert!(
            tracker
                .expire_at(TTL, t0 + Duration::from_secs(7), two)
                .is_empty(),
            "only once"
        );
    }

    #[test]
    fn expiry_of_one_report_falls_back_to_the_others() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        tracker.report_at(1, 2, 90, TTL, t0, two);
        tracker.report_at(1, 3, 20, TTL, t0 + Duration::from_secs(4), two);
        let out = tracker.expire_at(TTL, t0 + Duration::from_secs(7), two);
        assert_eq!(out, vec![(1, 20)], "the loud reporter aged out");
    }

    #[test]
    fn forgetting_a_session_removes_it_from_both_sides() {
        let t0 = Instant::now();
        let mut tracker = LossTracker::default();
        tracker.report_at(1, 2, 90, TTL, t0, two);
        tracker.report_at(2, 1, 70, TTL, t0, two);
        assert_eq!(
            tracker.forget(2, two),
            vec![(1, 0)],
            "speaker 1 lost its only reporter"
        );
        assert!(tracker.reports.is_empty());
        assert_eq!(tracker.report_at(1, 3, 90, TTL, t0, two), Some(90));
    }

    /// D-1 (external review 2026-09-27): more than a third of the
    /// listeners have to agree. Up to two that is the maximum.
    #[test]
    fn the_hint_needs_more_than_a_third_of_the_listeners() {
        assert_eq!(agreed(vec![150], 1), 150);
        assert_eq!(agreed(vec![150, 20], 2), 150, "n = 2: the maximum");
        assert_eq!(agreed(vec![150], 2), 150, "one of two is enough");
        assert_eq!(agreed(vec![150, 40, 0], 3), 40, "n = 3: second largest");
        assert_eq!(agreed(vec![150], 3), 0, "one of three is not");
        assert_eq!(agreed(vec![10, 150, 40, 0], 4), 40, "n = 4: second largest");
        assert_eq!(
            agreed(vec![60, 10, 50, 900, 0, 30], 6),
            50,
            "n = 6: third largest"
        );
        assert_eq!(
            agreed(vec![1000, 0, 0, 0], 4),
            0,
            "a single 1000‰ outlier among four is ignored"
        );
        assert_eq!(
            agreed(vec![1000], 4),
            0,
            "listeners without a report count as no loss"
        );
        assert_eq!(agreed(vec![1000, 30], 4), 30);
        assert_eq!(agreed(Vec::new(), 0), 0);
        assert_eq!(
            agreed(vec![70, 70, 70], 1),
            70,
            "more values than listeners"
        );
    }

    /// One listener on a bad line, or one who lies, among four no longer
    /// raises the speaker's FEC; a second one agreeing does.
    #[test]
    fn one_lossy_listener_of_four_does_not_set_the_hint() {
        let t0 = Instant::now();
        let four = |_: Session| 4;
        let mut tracker = LossTracker::default();
        assert_eq!(tracker.report_at(1, 2, 1000, TTL, t0, four), Some(0));
        assert_eq!(tracker.report_at(1, 3, 40, TTL, t0, four), Some(40));
        assert_eq!(tracker.report_at(1, 4, 0, TTL, t0, four), None);
        assert_eq!(
            tracker.expire_at(TTL, t0 + TTL, |_| 1),
            vec![(1, 0)],
            "all gone"
        );
    }
}
