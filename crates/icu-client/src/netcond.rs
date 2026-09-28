//! Network-condition detection: quinn's own transport statistics, composed
//! with the existing loss/congestion feedback, into one signal for the
//! quality controller (`crate::quality`; `docs/reference/client.md` §8,
//! `docs/research/transport.md`).
//!
//! Pure data structure, `Instant`-injected like `dsp::jitter`, so the
//! smoothing logic is testable without a real connection. No custom
//! bandwidth estimator is built here: every uplink number below is already
//! computed by quinn (`quinn::Connection::stats()` /
//! `datagram_send_buffer_space()`), never read anywhere else in this
//! codebase before this module.

use std::time::Duration;

/// One raw reading of a connection's own transport stats, taken every tick.
/// Deliberately not `quinn::ConnectionStats` itself (`#[non_exhaustive]`,
/// not constructible outside `quinn-proto`, and not `Instant`-injectable for
/// tests) — just the slice this module needs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sample {
    /// `ConnectionStats.path.rtt`: current smoothed path RTT.
    pub rtt: Duration,
    /// `ConnectionStats.path.min_rtt`: floor, ignoring ack delay.
    pub min_rtt: Duration,
    /// `ConnectionStats.path.congestion_events`, cumulative.
    pub congestion_events: u64,
    /// `ConnectionStats.path.lost_packets`, cumulative.
    pub lost_packets: u64,
    /// `ConnectionStats.path.sent_packets`, cumulative.
    pub sent_packets: u64,
    /// `ConnectionStats.path.cwnd`: current congestion window, bytes.
    pub cwnd: u64,
    /// `ConnectionStats.udp_tx.bytes`, cumulative — used only for the
    /// cwnd-saturation gate.
    pub udp_tx_bytes: u64,
    /// `send_buffer_size - Connection::datagram_send_buffer_space()`: bytes
    /// of this app's own outgoing audio currently queued unsent. Direct,
    /// pre-loss, app-specific — the standout signal of this set.
    pub datagram_queue_bytes: u64,
}

/// Smoothed network condition for one connection, composing local uplink
/// pressure with the existing downstream feedback messages. The two groups
/// are deliberately kept as separate fields, never merged into one score —
/// they answer different questions (this connection's own uplink vs. a
/// struggling *viewer's* downlink) and the quality controller needs to tell
/// them apart.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetworkCondition {
    /// EWMA of the path RTT, milliseconds.
    pub rtt_ms: f32,
    /// EWMA of `rtt - min_rtt`, milliseconds — the only pre-loss pressure
    /// signal a stock-Cubic-controlled path gives before it actually drops
    /// packets (`docs/research/transport.md`).
    pub queuing_delay_ms: f32,
    /// Congestion events observed during the last tick (a delta; not
    /// smoothed — smoothing a delta double-counts persistence).
    pub congestion_events_delta: u64,
    /// This endpoint's own transport loss ratio over the last tick (a delta
    /// ratio; not smoothed, for the same reason).
    pub loss_ratio: f32,
    /// Bytes of outgoing audio currently queued unsent — a live level, not
    /// smoothed: smoothing a queue depth adds exactly the reaction lag that
    /// would make the signal useless.
    pub datagram_queue_bytes: u64,
    /// Current congestion window, bytes. Only meaningful as a bandwidth
    /// ceiling when `cwnd_limited` is true.
    pub cwnd_bytes: u64,
    /// Whether the connection appears to be pushing against `cwnd_bytes`
    /// (bytes sent this tick are close to the bandwidth-delay-product
    /// `cwnd_bytes / rtt * tick`). At this app's low bitrates the cwnd is
    /// almost always unsaturated, so `cwnd_bytes` alone means nothing
    /// without this gate.
    pub cwnd_limited: bool,
    /// Mirrors the speaker's own `LossHint.loss_permille` (already computed
    /// for Opus FEC in `dsp::opus`), surfaced here so the quality controller
    /// has one place to read everything.
    pub audio_loss_hint_permille: u16,
    /// The last `ControlMsg::VideoCongestionHint.drop_permille` received —
    /// the worst video-drop rate among this sender's own viewers.
    pub video_drop_hint_permille: u16,
}

/// How much a new RTT/queuing-delay sample moves the running EWMA. ~0.3
/// gives roughly a 3-tick time constant, smoothing jitter without lagging a
/// real trend by more than a few ticks.
const RTT_EWMA_ALPHA: f32 = 0.3;

/// Fraction of the bandwidth-delay product that counts as "saturated" for
/// the `cwnd_limited` gate.
const CWND_SATURATION_FRACTION: f64 = 0.9;

/// Folds successive [`Sample`]s into a [`NetworkCondition`]. One instance
/// per engine, replaced by a fresh one at every `Welcome`: the hints and the
/// cumulative counters of a connection that ended say nothing about the
/// next one.
#[derive(Debug, Default)]
pub struct NetworkTracker {
    prev: Option<Sample>,
    cond: NetworkCondition,
}

impl NetworkTracker {
    /// A tracker with no samples yet.
    pub fn new() -> NetworkTracker {
        NetworkTracker::default()
    }

    /// The condition as last computed.
    pub fn condition(&self) -> NetworkCondition {
        self.cond
    }

    /// Updates the audio loss hint field directly, from
    /// `ControlMsg::LossHint` (already computed for FEC elsewhere; this just
    /// surfaces it here too).
    pub fn set_audio_loss_hint(&mut self, permille: u16) {
        self.cond.audio_loss_hint_permille = permille;
    }

    /// Updates the video congestion hint field, from
    /// `ControlMsg::VideoCongestionHint`.
    pub fn set_video_drop_hint(&mut self, permille: u16) {
        self.cond.video_drop_hint_permille = permille;
    }

    /// Folds in one new [`Sample`], taken `elapsed` after the previous call
    /// (the tick period). Returns the updated condition.
    pub fn observe(&mut self, sample: Sample, elapsed: Duration) -> NetworkCondition {
        let rtt_ms = sample.rtt.as_secs_f32() * 1000.0;
        let queuing_ms = sample.rtt.saturating_sub(sample.min_rtt).as_secs_f32() * 1000.0;

        match self.prev {
            None => {
                self.cond.rtt_ms = rtt_ms;
                self.cond.queuing_delay_ms = queuing_ms;
                self.cond.congestion_events_delta = 0;
                self.cond.loss_ratio = 0.0;
                self.cond.cwnd_limited = false;
            }
            Some(prev) => {
                self.cond.rtt_ms += RTT_EWMA_ALPHA * (rtt_ms - self.cond.rtt_ms);
                self.cond.queuing_delay_ms +=
                    RTT_EWMA_ALPHA * (queuing_ms - self.cond.queuing_delay_ms);

                self.cond.congestion_events_delta = sample
                    .congestion_events
                    .saturating_sub(prev.congestion_events);
                let d_lost = sample.lost_packets.saturating_sub(prev.lost_packets);
                let d_sent = sample.sent_packets.saturating_sub(prev.sent_packets);
                self.cond.loss_ratio = if d_sent == 0 {
                    0.0
                } else {
                    d_lost as f32 / d_sent as f32
                };

                let d_udp_bytes = sample.udp_tx_bytes.saturating_sub(prev.udp_tx_bytes);
                let rtt_s = sample.rtt.as_secs_f64().max(0.001);
                let capacity_bytes = (sample.cwnd as f64 / rtt_s) * elapsed.as_secs_f64();
                self.cond.cwnd_limited = capacity_bytes > 0.0
                    && (d_udp_bytes as f64) >= CWND_SATURATION_FRACTION * capacity_bytes;
            }
        }
        self.cond.datagram_queue_bytes = sample.datagram_queue_bytes;
        self.cond.cwnd_bytes = sample.cwnd;
        self.prev = Some(sample);
        self.cond
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(rtt_ms: u64, min_rtt_ms: u64) -> Sample {
        Sample {
            rtt: Duration::from_millis(rtt_ms),
            min_rtt: Duration::from_millis(min_rtt_ms),
            ..Sample::default()
        }
    }

    #[test]
    fn the_first_sample_seeds_rtt_without_smoothing() {
        let mut t = NetworkTracker::new();
        let cond = t.observe(sample(40, 20), Duration::from_secs(1));
        assert_eq!(cond.rtt_ms, 40.0);
        assert_eq!(cond.queuing_delay_ms, 20.0);
    }

    #[test]
    fn rtt_is_smoothed_not_jumped() {
        let mut t = NetworkTracker::new();
        t.observe(sample(40, 20), Duration::from_secs(1));
        let cond = t.observe(sample(140, 20), Duration::from_secs(1));
        assert!(
            cond.rtt_ms > 40.0 && cond.rtt_ms < 140.0,
            "one EWMA step should not jump straight to the new sample: {}",
            cond.rtt_ms
        );
    }

    #[test]
    fn loss_ratio_is_a_delta_not_smoothed() {
        let mut t = NetworkTracker::new();
        t.observe(
            Sample {
                sent_packets: 100,
                lost_packets: 0,
                ..sample(20, 20)
            },
            Duration::from_secs(1),
        );
        let cond = t.observe(
            Sample {
                sent_packets: 200,
                lost_packets: 10,
                ..sample(20, 20)
            },
            Duration::from_secs(1),
        );
        assert!((cond.loss_ratio - 0.1).abs() < 1e-6);
    }

    #[test]
    fn cwnd_limited_only_when_bytes_sent_approach_the_bandwidth_delay_product() {
        let mut t = NetworkTracker::new();
        t.observe(
            Sample {
                cwnd: 100_000,
                udp_tx_bytes: 0,
                ..sample(20, 20)
            },
            Duration::from_secs(1),
        );
        // Bandwidth-delay product over 1 s at 20 ms RTT: 100_000 / 0.02 * 1 = 5_000_000 B.
        let far_below = t.observe(
            Sample {
                cwnd: 100_000,
                udp_tx_bytes: 10_000,
                ..sample(20, 20)
            },
            Duration::from_secs(1),
        );
        assert!(!far_below.cwnd_limited);

        let mut t2 = NetworkTracker::new();
        t2.observe(
            Sample {
                cwnd: 100_000,
                udp_tx_bytes: 0,
                ..sample(20, 20)
            },
            Duration::from_secs(1),
        );
        let saturated = t2.observe(
            Sample {
                cwnd: 100_000,
                udp_tx_bytes: 5_000_000,
                ..sample(20, 20)
            },
            Duration::from_secs(1),
        );
        assert!(saturated.cwnd_limited);
    }

    #[test]
    fn hints_are_surfaced_verbatim() {
        let mut t = NetworkTracker::new();
        t.set_audio_loss_hint(120);
        t.set_video_drop_hint(75);
        let cond = t.condition();
        assert_eq!(cond.audio_loss_hint_permille, 120);
        assert_eq!(cond.video_drop_hint_permille, 75);
    }
}
