//! Auto-calibrated VAD threshold: tracks a noise floor on the RNNoise
//! voice-probability signal and derives a threshold from it, so hands-free
//! (VAD) mode does not need a manually tuned threshold per room/mic.
//!
//! Only samples taken while the gate is confirmed closed feed the floor
//! (`Gate::is_open() == false`), so real speech never pollutes the
//! estimate. A short cooldown after an open→closed transition skips
//! RNNoise's own trailing probability tail. A starvation watchdog nudges
//! the floor up even while nominally open, so a threshold that started too
//! low (gate never closes) cannot deadlock the tracker forever.

use std::time::{Duration, Instant};

const MARGIN: f32 = 0.18;
const MIN_THRESHOLD: f32 = 0.15;
const MAX_THRESHOLD: f32 = 0.9;
/// Fast: chase a quieter ambience down.
const ALPHA_DOWN: f32 = 0.30;
/// Slow: absorb sustained new noise without reacting to a single spike.
const ALPHA_UP: f32 = 0.03;
/// Skip samples this long after an open→closed transition (RNNoise's own
/// smoothing leaves a short probability tail after speech ends).
const CLOSE_COOLDOWN: Duration = Duration::from_millis(150);
/// If no trusted sample has landed in this long, absorb the current
/// sample anyway (breaks a stuck-open gate in a noisy room).
const STARVATION: Duration = Duration::from_secs(4);

/// Tracks the noise floor and derives the live VAD threshold. One instance
/// per `dsp_thread`; not thread-safe, owned entirely by that thread.
#[derive(Debug)]
pub struct AutoThreshold {
    floor: f32,
    closed_at: Option<Instant>,
    prev_gate_open: bool,
    last_trusted: Instant,
}

impl AutoThreshold {
    /// New tracker, seeded near the low end so the threshold is usable
    /// immediately rather than needing a silent warm-up period.
    pub fn new(now: Instant) -> AutoThreshold {
        AutoThreshold {
            floor: 0.05,
            closed_at: None,
            prev_gate_open: false,
            last_trusted: now,
        }
    }

    /// One 10 ms block. `gate_open` is the gate's openness as of the last
    /// completed frame (up to one Opus frame stale — fine here). `prob` is
    /// this block's RNNoise voice probability.
    pub fn update(&mut self, now: Instant, gate_open: bool, prob: f32) {
        if self.prev_gate_open && !gate_open {
            self.closed_at = Some(now);
        }
        self.prev_gate_open = gate_open;

        let cooled = self
            .closed_at
            .is_none_or(|t| now.duration_since(t) >= CLOSE_COOLDOWN);
        let trusted = !gate_open && cooled;
        let starved = now.duration_since(self.last_trusted) >= STARVATION;

        if trusted {
            let alpha = if prob < self.floor {
                ALPHA_DOWN
            } else {
                ALPHA_UP
            };
            self.floor += alpha * (prob - self.floor);
            self.last_trusted = now;
        } else if starved {
            self.floor += ALPHA_UP * (prob - self.floor);
        }
    }

    /// The current auto-calibrated threshold, always within
    /// `[MIN_THRESHOLD, MAX_THRESHOLD]`.
    pub fn threshold(&self) -> f32 {
        (self.floor + MARGIN).clamp(MIN_THRESHOLD, MAX_THRESHOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn floor_decays_fast_on_cold_silence() {
        let t0 = Instant::now();
        let mut a = AutoThreshold::new(t0);
        let mut t = t0;
        for _ in 0..50 {
            t += ms(10);
            a.update(t, false, 0.02);
        }
        assert!(
            a.floor < 0.03,
            "floor should converge near 0.02, got {}",
            a.floor
        );
    }

    #[test]
    fn floor_does_not_rise_while_gate_open() {
        let t0 = Instant::now();
        let mut a = AutoThreshold::new(t0);
        let before = a.floor;
        let mut t = t0;
        // Well under STARVATION.
        for _ in 0..200 {
            t += ms(10);
            a.update(t, true, 0.9);
        }
        assert_eq!(a.floor, before, "gate stayed open, floor must not move");
    }

    #[test]
    fn watchdog_recovers_a_stuck_open_gate() {
        let t0 = Instant::now();
        let mut a = AutoThreshold::new(t0);
        let mut t = t0;
        let mut samples = 0;
        while t.duration_since(t0) < Duration::from_secs(10) {
            t += ms(10);
            a.update(t, true, 0.5);
            samples += 1;
        }
        assert!(samples > 0);
        assert!(
            a.floor > 0.05,
            "watchdog should have nudged the floor up from its seed, got {}",
            a.floor
        );
        assert!(
            a.floor < 0.5,
            "watchdog must not jump straight to the observed prob, got {}",
            a.floor
        );
    }

    #[test]
    fn watchdog_nudge_self_corrects_after_a_pause() {
        let t0 = Instant::now();
        let mut a = AutoThreshold::new(t0);
        let mut t = t0;
        // Stuck open long enough for the watchdog to nudge the floor up.
        while t.duration_since(t0) < Duration::from_secs(5) {
            t += ms(10);
            a.update(t, true, 0.5);
        }
        let nudged = a.floor;
        assert!(nudged > 0.05, "watchdog should have nudged floor up");
        // A real pause: gate closes, quiet resumes.
        for _ in 0..50 {
            t += ms(10);
            a.update(t, false, 0.02);
        }
        assert!(
            a.floor < nudged,
            "a genuine pause should pull the floor back down quickly, {} -> {}",
            nudged,
            a.floor
        );
    }

    #[test]
    fn close_cooldown_ignores_the_trailing_tail() {
        let t0 = Instant::now();
        let mut a = AutoThreshold::new(t0);
        let mut t = t0;
        // Open, then close.
        t += ms(10);
        a.update(t, true, 0.9);
        t += ms(10);
        a.update(t, false, 0.9); // trailing tail, still within cooldown
        assert!(a.floor < 0.1, "trailing-tail sample must not be trusted");
        // Cooldown elapses; a genuinely quiet sample now lands.
        t += CLOSE_COOLDOWN + ms(10);
        a.update(t, false, 0.02);
        assert!(
            a.floor < 0.05,
            "post-cooldown quiet sample should be trusted"
        );
    }

    #[test]
    fn threshold_stays_within_clamp_bounds() {
        let t0 = Instant::now();
        let mut a = AutoThreshold::new(t0);
        let mut t = t0;
        for i in 0..2000 {
            t += ms(10);
            let prob = if i % 2 == 0 { 0.0 } else { 1.0 };
            let open = i % 3 == 0;
            a.update(t, open, prob);
            let th = a.threshold();
            assert!(
                (MIN_THRESHOLD..=MAX_THRESHOLD).contains(&th),
                "threshold left clamp bounds: {th}"
            );
        }
    }
}
