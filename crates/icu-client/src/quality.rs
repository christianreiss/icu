//! Network-adaptive video quality ladder (`docs/reference/client.md` §8).
//!
//! Video absorbs 100 % of the adaptation. Audio's Opus bitrate is never
//! touched here: at this app's wire costs (Opus ≈ 74 kbit/s vs. video
//! 150-400 kbit/s, `docs/research/codec.md`), a single video tier-down step saves
//! more than shaving Opus down to its floor ever could, so there is no
//! scenario where this controller needs to reach for audio bitrate before
//! video has already given up everything. Audio's only reaction to network
//! stress remains the existing, independent `LossHint` → FEC redundancy path
//! (`dsp::opus::Encoder::set_loss_permille`), untouched by this module.
//!
//! Pure state machine, `Instant`-injected, so the hysteresis/threshold logic
//! is testable without a live connection or camera.

use std::time::{Duration, Instant};

use crate::{netcond::NetworkCondition, video::VideoTier};

/// Ticks a degradation condition must hold before stepping the tier down.
const DOWN_CONFIRM_TICKS: u32 = 2;
/// Ticks "all clear" must hold before stepping the tier up, absent backoff.
const UP_CONFIRM_TICKS: u32 = 6;
/// Ceiling for the up-confirm requirement after repeated flapping.
const UP_BACKOFF_MAX_TICKS: u32 = 24;
/// If a step-down follows a step-up within this window, the next step-up
/// requires twice as many clean ticks (capped at [`UP_BACKOFF_MAX_TICKS`]).
const FLAP_WINDOW: Duration = Duration::from_secs(30);

/// How slowly the "good" RTT baseline drifts. Only updated on ticks judged
/// not-bad, so sustained congestion cannot drag the baseline up and mask
/// itself.
const RTT_BASELINE_ALPHA: f32 = 0.05;
/// RTT rise over the rolling baseline that counts as bufferbloat-under-load.
const RTT_STEP_DOWN_MS: f32 = 100.0;
/// Queuing delay (`rtt - min_rtt`) that counts as pre-loss pressure.
const QUEUING_DELAY_ELEVATED_MS: f32 = 30.0;
/// Own-audio loss (`LossHint`) that counts as a step-down trigger.
const AUDIO_LOSS_STEP_DOWN_PERMILLE: u16 = 30;
/// Own-audio loss severe enough to jump straight to paused video.
const LOSS_PAUSE_PERMILLE: u16 = 150;
/// Share of our frames the server drops for the slowest viewer
/// (`VideoCongestionHint`) that counts as a step-down trigger.
const VIDEO_DROP_STEP_DOWN_PERMILLE: u16 = 50;

/// Local `open_uni` timeouts within [`LOCAL_DROP_WINDOW`] that count as a
/// burst — a direct, immediate signal that leads the smoothed remote
/// [`NetworkCondition`].
const LOCAL_DROP_BURST: u32 = 3;
/// Window over which local drops are counted.
const LOCAL_DROP_WINDOW: Duration = Duration::from_secs(5);

/// Decides the outgoing camera [`VideoTier`] from measured network
/// conditions. Degradation is fast (1-2 ticks); recovery is deliberately
/// much slower (6+ clean ticks, with backoff on flapping) — a wrong
/// step-down only briefly costs video quality, but a wrong step-up
/// reintroduces the congestion that risks audio's datagrams, which drop
/// old-favor-new under pressure by design (`docs/research/transport.md`).
#[derive(Debug)]
pub struct QualityController {
    tier: VideoTier,
    rtt_baseline_ms: Option<f32>,
    bad_ticks: u32,
    good_ticks: u32,
    up_confirm_needed: u32,
    last_step_up: Option<Instant>,
    last_local_drops_total: u64,
    local_drop_window_start: Instant,
    local_drops_in_window: u32,
    /// Whether the previous completed window contained a drop burst.
    seen_local_burst: bool,
    /// The current window's burst has already caused a step down.
    local_burst_reported: bool,
}

impl QualityController {
    /// A controller starting at the full-quality tier, as of `now`.
    pub fn new(now: Instant) -> QualityController {
        QualityController {
            tier: VideoTier::T0,
            rtt_baseline_ms: None,
            bad_ticks: 0,
            good_ticks: 0,
            up_confirm_needed: UP_CONFIRM_TICKS,
            last_step_up: None,
            last_local_drops_total: 0,
            local_drop_window_start: now,
            local_drops_in_window: 0,
            seen_local_burst: false,
            local_burst_reported: false,
        }
    }

    /// The current tier.
    pub fn tier(&self) -> VideoTier {
        self.tier
    }

    /// One tick. `local_drops_total` is the *cumulative* `open_uni` timeout
    /// counter (e.g. `DebugStats::video_open_uni_drops`); the controller
    /// diffs it against the previous call itself. Returns the tier to apply
    /// (unchanged from [`QualityController::tier`] when nothing moved).
    pub fn tick(
        &mut self,
        cond: &NetworkCondition,
        local_drops_total: u64,
        now: Instant,
    ) -> VideoTier {
        let local_delta = local_drops_total.saturating_sub(self.last_local_drops_total);
        self.last_local_drops_total = local_drops_total;
        let window_age = now.duration_since(self.local_drop_window_start);
        if window_age >= LOCAL_DROP_WINDOW {
            self.seen_local_burst = window_age < LOCAL_DROP_WINDOW * 2
                && self.local_drops_in_window >= LOCAL_DROP_BURST;
            self.local_drop_window_start = now;
            self.local_drops_in_window = 0;
            self.local_burst_reported = false;
        }
        self.local_drops_in_window += local_delta as u32;
        let local_burst =
            self.local_drops_in_window >= LOCAL_DROP_BURST && !self.local_burst_reported;

        let baseline = *self.rtt_baseline_ms.get_or_insert(cond.rtt_ms);
        let rtt_bad = cond.rtt_ms - baseline > RTT_STEP_DOWN_MS;
        let queuing_bad = cond.queuing_delay_ms > QUEUING_DELAY_ELEVATED_MS;
        let loss_bad = cond.audio_loss_hint_permille >= AUDIO_LOSS_STEP_DOWN_PERMILLE;
        // A viewer whose downlink cannot keep up makes the server drop our
        // frames for them. That steps down too, but never into the pause:
        // the server measures the drop rate on the frames it forwards, so
        // with nothing sent the hint could never clear. At the last tier
        // that still sends it only holds the tier.
        let viewer_bad = cond.video_drop_hint_permille >= VIDEO_DROP_STEP_DOWN_PERMILLE;
        // Moderate pressure lowers quality but keeps a picture at T3.
        // Only severe audio loss or bursts in consecutive windows pause.
        let pressure = rtt_bad || queuing_bad || loss_bad || viewer_bad;
        let bad = pressure && !self.tier.step_down().paused();

        // Jump straight to pause: severe loss, or a local drop-burst
        // repeating in a second consecutive window.
        let severe = cond.audio_loss_hint_permille >= LOSS_PAUSE_PERMILLE
            || (local_burst && self.seen_local_burst);
        if local_burst {
            self.local_burst_reported = true;
        }

        if severe {
            self.tier = VideoTier::T4;
            self.bad_ticks = 0;
            self.good_ticks = 0;
            self.up_confirm_needed = UP_CONFIRM_TICKS;
            return self.tier;
        }

        if bad || local_burst {
            self.good_ticks = 0;
            self.bad_ticks += 1;
            let confirm = if local_burst { 1 } else { DOWN_CONFIRM_TICKS };
            if self.bad_ticks >= confirm && !self.tier.step_down().paused() {
                self.tier = self.tier.step_down();
                self.bad_ticks = 0;
                if let Some(last_up) = self.last_step_up {
                    if now.duration_since(last_up) < FLAP_WINDOW {
                        self.up_confirm_needed =
                            (self.up_confirm_needed * 2).min(UP_BACKOFF_MAX_TICKS);
                    }
                }
            }
        } else if pressure && !self.tier.paused() {
            // Pressure at the lowest sending tier: hold until clear.
            self.bad_ticks = 0;
            self.good_ticks = 0;
        } else {
            self.bad_ticks = 0;
            self.rtt_baseline_ms = Some(baseline + RTT_BASELINE_ALPHA * (cond.rtt_ms - baseline));

            if self.tier != VideoTier::T0 {
                self.good_ticks += 1;
                if self.good_ticks >= self.up_confirm_needed {
                    self.tier = self.tier.step_up();
                    self.good_ticks = 0;
                    self.last_step_up = Some(now);
                    self.up_confirm_needed = UP_CONFIRM_TICKS;
                }
            } else {
                self.good_ticks = 0;
            }
        }

        self.tier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calm() -> NetworkCondition {
        NetworkCondition {
            rtt_ms: 20.0,
            queuing_delay_ms: 5.0,
            ..NetworkCondition::default()
        }
    }

    #[test]
    fn stays_at_full_quality_when_calm() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        for i in 0..10 {
            let tier = qc.tick(&calm(), 0, t0 + Duration::from_secs(i));
            assert_eq!(tier, VideoTier::T0);
        }
    }

    #[test]
    fn steps_down_after_two_confirming_bad_ticks_not_one() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        qc.tick(&calm(), 0, t0);
        let bad = NetworkCondition {
            rtt_ms: 20.0,
            queuing_delay_ms: 5.0,
            audio_loss_hint_permille: 50,
            ..NetworkCondition::default()
        };
        let after_one = qc.tick(&bad, 0, t0 + Duration::from_secs(1));
        assert_eq!(after_one, VideoTier::T0, "one bad tick is not enough");
        let after_two = qc.tick(&bad, 0, t0 + Duration::from_secs(2));
        assert_eq!(after_two, VideoTier::T1);
    }

    /// A viewer the server drops our frames for (`VideoCongestionHint`)
    /// steps the tier down like our own loss does, but only to the last tier
    /// that still sends: paused, no frame would ever refresh the hint. There
    /// it holds until the hint clears.
    #[test]
    fn a_struggling_viewer_steps_down_but_never_pauses() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        let mut now = t0;
        let mut tick = |qc: &mut QualityController, cond: &NetworkCondition| {
            now += Duration::from_secs(2);
            qc.tick(cond, 0, now)
        };
        let below = NetworkCondition {
            video_drop_hint_permille: 40,
            ..calm()
        };
        for _ in 0..4 {
            assert_eq!(tick(&mut qc, &below), VideoTier::T0, "under 50 ‰");
        }
        let dropping = NetworkCondition {
            video_drop_hint_permille: 80,
            ..calm()
        };
        tick(&mut qc, &dropping);
        assert_eq!(tick(&mut qc, &dropping), VideoTier::T1);
        for _ in 0..30 {
            assert_ne!(tick(&mut qc, &dropping), VideoTier::T4, "never paused");
        }
        assert_eq!(qc.tier(), VideoTier::T3);
        for _ in 0..5 {
            assert_eq!(tick(&mut qc, &calm()), VideoTier::T3);
        }
        assert_eq!(tick(&mut qc, &calm()), VideoTier::T2, "recovers once clear");
    }

    /// Paused for another reason, a drop hint left over from before cannot
    /// keep the video paused: nothing is sent that could clear it.
    #[test]
    fn a_stale_drop_hint_does_not_hold_the_pause() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        let severe = NetworkCondition {
            audio_loss_hint_permille: 200,
            video_drop_hint_permille: 80,
            ..calm()
        };
        assert_eq!(qc.tick(&severe, 0, t0), VideoTier::T4);
        let stale = NetworkCondition {
            video_drop_hint_permille: 80,
            ..calm()
        };
        let mut now = t0;
        for _ in 0..6 {
            now += Duration::from_secs(2);
            qc.tick(&stale, 0, now);
        }
        assert_eq!(
            qc.tier(),
            VideoTier::T3,
            "sends again, so the hint can clear"
        );
    }

    #[test]
    fn a_local_drop_burst_steps_down_immediately() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        qc.tick(&calm(), 0, t0);
        // Three drops inside one 5 s window.
        let tier = qc.tick(&calm(), 3, t0 + Duration::from_secs(1));
        assert_eq!(
            tier,
            VideoTier::T1,
            "a burst needs only one confirming tick"
        );
    }

    #[test]
    fn one_local_burst_is_not_counted_again_on_later_ticks() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        assert_eq!(
            qc.tick(&calm(), 3, t0 + Duration::from_secs(1)),
            VideoTier::T1
        );
        for second in 2..5 {
            assert_eq!(
                qc.tick(&calm(), 3, t0 + Duration::from_secs(second)),
                VideoTier::T1
            );
        }
    }

    #[test]
    fn moderate_pressure_keeps_a_low_rate_picture() {
        for pressure in [
            NetworkCondition {
                rtt_ms: 200.0,
                ..calm()
            },
            NetworkCondition {
                queuing_delay_ms: 60.0,
                ..calm()
            },
            NetworkCondition {
                audio_loss_hint_permille: 50,
                ..calm()
            },
        ] {
            let t0 = Instant::now();
            let mut qc = QualityController::new(t0);
            qc.tick(&calm(), 0, t0);
            for second in 1..40 {
                assert_ne!(
                    qc.tick(&pressure, 0, t0 + Duration::from_secs(second)),
                    VideoTier::T4
                );
            }
            assert_eq!(qc.tier(), VideoTier::T3);
        }
    }

    #[test]
    fn severe_loss_jumps_straight_to_paused() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        let severe = NetworkCondition {
            audio_loss_hint_permille: 200,
            ..calm()
        };
        let tier = qc.tick(&severe, 0, t0);
        assert_eq!(tier, VideoTier::T4);
    }

    #[test]
    fn a_repeated_local_drop_burst_across_two_windows_jumps_to_paused() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        qc.tick(&calm(), 0, t0);
        // First burst: steps down once, does not pause.
        let tier = qc.tick(&calm(), 3, t0 + Duration::from_secs(1));
        assert_eq!(tier, VideoTier::T1);
        // Second burst, next window (5 s later): jumps straight to pause.
        // `local_drops_total` is cumulative, so a second burst of 3 more
        // drops is total 6, not another 3.
        let tier = qc.tick(&calm(), 6, t0 + Duration::from_secs(7));
        assert_eq!(tier, VideoTier::T4);
    }

    #[test]
    fn recovery_needs_far_more_clean_ticks_than_degradation_took() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        // Force down to T1.
        let bad = NetworkCondition {
            audio_loss_hint_permille: 50,
            ..calm()
        };
        qc.tick(&bad, 0, t0);
        qc.tick(&bad, 0, t0 + Duration::from_secs(1));
        assert_eq!(qc.tier(), VideoTier::T1);

        // Five clean ticks: not yet enough (needs 6).
        let mut now = t0 + Duration::from_secs(2);
        for _ in 0..5 {
            now += Duration::from_secs(1);
            let tier = qc.tick(&calm(), 0, now);
            assert_eq!(
                tier,
                VideoTier::T1,
                "recovery must not be as fast as degradation"
            );
        }
        now += Duration::from_secs(1);
        assert_eq!(qc.tick(&calm(), 0, now), VideoTier::T0);
    }

    #[test]
    fn flapping_doubles_the_next_recovery_requirement() {
        let t0 = Instant::now();
        let mut qc = QualityController::new(t0);
        let bad = NetworkCondition {
            audio_loss_hint_permille: 50,
            ..calm()
        };
        // Down to T1.
        qc.tick(&bad, 0, t0);
        qc.tick(&bad, 0, t0 + Duration::from_secs(1));
        // Recover to T0 (6 clean ticks).
        let mut now = t0 + Duration::from_secs(1);
        for _ in 0..6 {
            now += Duration::from_secs(1);
            qc.tick(&calm(), 0, now);
        }
        assert_eq!(qc.tier(), VideoTier::T0);
        // Immediately degrade again within the flap window.
        now += Duration::from_secs(1);
        qc.tick(&bad, 0, now);
        now += Duration::from_secs(1);
        qc.tick(&bad, 0, now);
        assert_eq!(qc.tier(), VideoTier::T1);
        // Recovery now needs 12 ticks, not 6: five is not enough.
        for _ in 0..11 {
            now += Duration::from_secs(1);
            let tier = qc.tick(&calm(), 0, now);
            assert_eq!(tier, VideoTier::T1, "backoff must double the requirement");
        }
        now += Duration::from_secs(1);
        assert_eq!(qc.tick(&calm(), 0, now), VideoTier::T0);
    }
}
