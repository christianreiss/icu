//! Pairs AEC reference blocks with capture blocks (`Research/aec.md`).
//!
//! Capture and reference are two PipeWire streams in one `node.group`, so
//! both callbacks push the same number of samples per graph cycle, one right
//! after the other, in either order. AEC3 copes with a reference that runs
//! *early* (it just sees a longer echo path); a reference that runs *late*
//! is fatal once the lateness exceeds the real monitor→mic delay, because
//! the echo then reaches the capture before its reference has been fed.
//!
//! Once both halves of a cycle are in, the occupancy difference
//! `reference − capture` is exactly how many samples the reference lags, and
//! that many of the oldest reference samples are dropped. Between the two
//! halves the difference is off by one quantum in either direction, so
//! nothing is dropped then; the DSP thread waits briefly for the missing
//! half instead. [`CycleClock`] tells the two states apart from the time of
//! each stream's last push.
//!
//! Both rings must start empty together (the engine clears them on every
//! reopen), otherwise the occupancy difference is not the lag.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// How long a ready capture block waits for the other half of its cycle.
pub const HALF_WAIT: Duration = Duration::from_millis(1);
/// Minimum time it waits for a reference that has not delivered a block
/// before a silent reference is fed and an underrun counted. The engine
/// stretches it to 1.5 graph cycles ([`CycleClock::stall_wait`]).
pub const STALL_WAIT: Duration = Duration::from_millis(5);
/// Two pushes this close together belong to the same graph cycle. Both
/// streams run in one `node.group`, so their callbacks follow each other
/// within microseconds; a real gap is a whole cycle (≥ 2.7 ms at 128).
const SAME_CYCLE_NS: u64 = 500_000;

/// What the DSP thread should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Not enough data yet; sleep and ask again.
    Wait,
    /// Drop `drop_ref` oldest reference samples, then pop one block from
    /// each ring.
    Pair {
        /// Stale reference samples to discard first.
        drop_ref: usize,
    },
    /// Pop the capture block and feed a silent reference.
    Silent {
        /// The reference stream is open but did not deliver in time.
        underrun: bool,
    },
}

/// Decides the next step.
///
/// `cap`/`reference` are the ring occupancies, `block` the DSP block size,
/// `ref_open` whether a reference stream is running, `cycle_complete`
/// whether both streams have pushed in the current graph cycle, `waited`
/// how long the ready capture block has been waiting, and `stall_wait` when
/// to give up on the reference.
pub fn step(
    cap: usize,
    reference: usize,
    block: usize,
    ref_open: bool,
    cycle_complete: bool,
    waited: Duration,
    stall_wait: Duration,
) -> Step {
    if cap < block {
        return Step::Wait;
    }
    if !ref_open {
        return Step::Silent { underrun: false };
    }
    if cycle_complete {
        let drop_ref = reference.saturating_sub(cap);
        if reference - drop_ref >= block {
            return Step::Pair { drop_ref };
        }
        if cap >= reference + block {
            // The reference runs a whole block early (capture started
            // first). Skip one reference block; it stays early, never late.
            return Step::Silent { underrun: false };
        }
    } else if waited < HALF_WAIT {
        return Step::Wait;
    } else if reference >= block {
        // The other half is late; pair without trusting the difference.
        return Step::Pair { drop_ref: 0 };
    }
    if waited < stall_wait {
        Step::Wait
    } else {
        Step::Silent { underrun: true }
    }
}

/// Last push time and size per stream, written by the backend callbacks.
pub struct CycleClock {
    base: Instant,
    cap_ns: AtomicU64,
    ref_ns: AtomicU64,
    cap_len: AtomicU64,
    /// The backend's `reference_slack`, in ns.
    slack_ns: AtomicU64,
}

impl Default for CycleClock {
    fn default() -> Self {
        CycleClock {
            base: Instant::now(),
            cap_ns: AtomicU64::new(0),
            ref_ns: AtomicU64::new(u64::MAX / 2),
            cap_len: AtomicU64::new(0),
            slack_ns: AtomicU64::new(0),
        }
    }
}

impl CycleClock {
    fn now_ns(&self) -> u64 {
        self.base.elapsed().as_nanos() as u64
    }

    /// Capture callback pushed `len` samples.
    pub fn capture_pushed(&self, len: usize) {
        self.cap_len.store(len as u64, Ordering::Relaxed);
        self.cap_ns.store(self.now_ns(), Ordering::Release);
    }

    /// Reference callback pushed a block.
    pub fn reference_pushed(&self) {
        self.ref_ns.store(self.now_ns(), Ordering::Release);
    }

    /// Both streams pushed in the same graph cycle.
    pub fn cycle_complete(&self) -> bool {
        self.skew() < Duration::from_nanos(SAME_CYCLE_NS)
    }

    /// Distance between the last capture and reference push.
    pub fn skew(&self) -> Duration {
        let c = self.cap_ns.load(Ordering::Acquire);
        let r = self.ref_ns.load(Ordering::Acquire);
        Duration::from_nanos(c.abs_diff(r))
    }

    /// [`STALL_WAIT`], stretched to 1.5 graph cycles for large quanta,
    /// and at least the backend's reference slack.
    pub fn stall_wait(&self) -> Duration {
        let cycle_ns = self.cap_len.load(Ordering::Relaxed) * 1_000_000_000 / 48_000;
        let slack = Duration::from_nanos(self.slack_ns.load(Ordering::Relaxed));
        STALL_WAIT
            .max(Duration::from_nanos(cycle_ns * 3 / 2))
            .max(slack)
    }

    /// Sets how late the reference may run (`AudioBackend::reference_slack`).
    pub fn set_slack(&self, slack: Duration) {
        self.slack_ns
            .store(slack.as_nanos() as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    //! Simulates the graph: each cycle pushes one quantum to both rings in
    //! a random order, and the DSP thread polls at random points. The rings
    //! hold absolute sample indices, so the lag of every pair is exact.

    use super::*;
    use crate::dsp::testsignal::Rng;
    use std::collections::VecDeque;

    const BLOCK: usize = 480;

    fn st(cap: usize, r: usize, block: usize, open: bool, complete: bool, w: Duration) -> Step {
        step(cap, r, block, open, complete, w, STALL_WAIT)
    }

    struct Sim {
        cap: VecDeque<usize>,
        reference: VecDeque<usize>,
        /// Graph cycle of each stream's last push.
        cap_cycle: usize,
        ref_cycle: usize,
        /// Lag (capture index − reference index) of every paired block;
        /// positive = reference late.
        lags: Vec<i64>,
        underruns: usize,
    }

    /// Runs `cycles` graph cycles of `quantum` samples. The reference loses
    /// its pushes in `stall` (cycle range). `policy(sim, waited)` returns
    /// true when it consumed a block. Each poll that has to wait counts as
    /// 1 ms.
    fn simulate(
        quantum: usize,
        cycles: usize,
        stall: std::ops::Range<usize>,
        seed: u64,
        mut policy: impl FnMut(&mut Sim, Duration) -> bool,
    ) -> Sim {
        let mut rng = Rng::new(seed);
        let mut sim = Sim {
            cap: VecDeque::new(),
            reference: VecDeque::new(),
            cap_cycle: 0,
            ref_cycle: usize::MAX,
            lags: Vec::new(),
            underruns: 0,
        };
        let mut waited_ms = 0u64;
        for c in 0..cycles {
            let ref_first = rng.next_u64() & 1 == 0;
            let t0 = c * quantum;
            for half in 0..2 {
                if (half == 0) == ref_first {
                    if !stall.contains(&c) {
                        sim.reference.extend(t0..t0 + quantum);
                        sim.ref_cycle = c;
                    }
                } else {
                    sim.cap.extend(t0..t0 + quantum);
                    sim.cap_cycle = c;
                }
                // The DSP thread may wake after either half.
                if !rng.next_u64().is_multiple_of(3) {
                    loop {
                        if policy(&mut sim, Duration::from_millis(waited_ms)) {
                            waited_ms = 0;
                        } else {
                            if sim.cap.len() >= BLOCK {
                                waited_ms += 1;
                            }
                            break;
                        }
                    }
                }
            }
        }
        sim
    }

    fn fixed(sim: &mut Sim, waited: Duration) -> bool {
        let complete = sim.cap_cycle == sim.ref_cycle;
        match st(
            sim.cap.len(),
            sim.reference.len(),
            BLOCK,
            true,
            complete,
            waited,
        ) {
            Step::Wait => false,
            Step::Pair { drop_ref } => {
                sim.reference.drain(..drop_ref);
                sim.lags.push(sim.cap[0] as i64 - sim.reference[0] as i64);
                sim.cap.drain(..BLOCK);
                sim.reference.drain(..BLOCK);
                true
            }
            Step::Silent { underrun } => {
                sim.underruns += underrun as usize;
                sim.cap.drain(..BLOCK);
                true
            }
        }
    }

    /// The pre-fix rule from `engine.rs`: pair as soon as the capture is
    /// there, zero-fill on a short reference, drain only beyond 4 blocks.
    fn legacy(sim: &mut Sim, _waited: Duration) -> bool {
        if sim.cap.len() < BLOCK {
            return false;
        }
        if sim.reference.len() >= BLOCK {
            sim.lags.push(sim.cap[0] as i64 - sim.reference[0] as i64);
            sim.reference.drain(..BLOCK);
            while sim.reference.len() > 4 * BLOCK {
                sim.reference.drain(..BLOCK);
            }
        } else {
            sim.underruns += 1;
        }
        sim.cap.drain(..BLOCK);
        true
    }

    #[test]
    fn legacy_rule_leaves_reference_late() {
        let worst = (0..20)
            .map(|seed| {
                *simulate(240, 2000, 0..0, seed, legacy)
                    .lags
                    .iter()
                    .max()
                    .unwrap()
            })
            .max()
            .unwrap();
        assert!(worst >= 480, "legacy worst lag {worst}");
    }

    #[test]
    fn reference_is_never_late() {
        for quantum in [128usize, 240, 256, 512, 1024, 2048] {
            for seed in 0..20 {
                let s = simulate(quantum, 3000, 0..0, seed, fixed);
                assert!(
                    s.lags.len() * BLOCK > 2000 * quantum,
                    "q={quantum} too few pairs"
                );
                assert!(
                    s.lags.iter().all(|&l| l == 0),
                    "q={quantum} seed={seed} lags {:?}",
                    s.lags
                        .iter()
                        .filter(|&&l| l != 0)
                        .take(5)
                        .collect::<Vec<_>>()
                );
                assert_eq!(s.underruns, 0, "q={quantum} seed={seed}");
            }
        }
    }

    #[test]
    fn stall_never_leaves_reference_late() {
        for seed in 0..20 {
            let s = simulate(240, 3000, 1000..1050, seed, fixed);
            assert!(s.underruns > 0, "stall must be visible");
            // The lost samples leave the reference early, never late.
            assert!(s.lags.iter().all(|&l| l <= 0), "seed={seed}");
            assert!(s.lags.iter().all(|&l| l > -(12_000 + 2 * BLOCK as i64)));
        }
    }

    #[test]
    fn reference_starting_late_ends_less_than_a_block_early() {
        for quantum in [240usize, 1024] {
            for start in 1..6 {
                for seed in 0..10 {
                    let s = simulate(quantum, 2000, 0..start, seed, fixed);
                    assert!(s.lags.iter().all(|&l| l <= 0), "q={quantum} start={start}");
                    let tail = &s.lags[s.lags.len() / 2..];
                    assert!(
                        tail.iter().all(|&l| l > -(BLOCK as i64)),
                        "q={quantum} start={start} seed={seed} lag {:?}",
                        tail.iter().min()
                    );
                }
            }
        }
    }

    #[test]
    fn edge_cases() {
        let z = Duration::ZERO;
        assert_eq!(
            st(480, 0, 480, false, false, z),
            Step::Silent { underrun: false }
        );
        assert_eq!(st(479, 960, 480, true, true, z), Step::Wait);
        assert_eq!(
            st(480, 960, 480, true, true, z),
            Step::Pair { drop_ref: 480 }
        );
        assert_eq!(st(480, 960, 480, true, false, z), Step::Wait);
        assert_eq!(
            st(480, 960, 480, true, false, HALF_WAIT),
            Step::Pair { drop_ref: 0 }
        );
        assert_eq!(st(480, 100, 480, true, true, HALF_WAIT), Step::Wait);
        // Reference a whole block early: skip it without an underrun.
        assert_eq!(
            st(960, 400, 480, true, true, z),
            Step::Silent { underrun: false }
        );
        assert_eq!(
            st(480, 100, 480, true, true, STALL_WAIT),
            Step::Silent { underrun: true }
        );
    }

    #[test]
    fn cycle_clock_detects_same_cycle() {
        let c = CycleClock::default();
        assert!(!c.cycle_complete());
        c.capture_pushed(240);
        assert!(!c.cycle_complete());
        c.reference_pushed();
        assert!(c.cycle_complete());
    }
}
