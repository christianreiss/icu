//! Adaptive jitter buffer, one per speaker (`Client/IMPLEMENTATION.md` §6,
//! `Research/latency-budget.md`).
//!
//! Pure data structure: the caller injects `now` and pops once per frame
//! period. All `seq`/`ts` comparisons are signed differences modulo 2^16 /
//! 2^32 (`PROTOCOL.md` §3). Loss (a `seq` gap) is told apart from a pause
//! (a `ts` jump with gapless `seq`); the latter re-anchors the buffer.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use icu_proto::{seq_diff, ts_diff};

/// A packet as queued in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Sender sequence number.
    pub seq: u16,
    /// Sender sample clock.
    pub ts: u32,
    /// Last packet of a talk spurt.
    pub end: bool,
    /// Opus payload.
    pub payload: Vec<u8>,
}

/// What the decoder should do this frame period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pop {
    /// Decode this packet.
    Packet(Packet),
    /// Packet `seq` was lost. If the following packet is already here it is
    /// handed over so the decoder can pull in-band FEC from it.
    Lost {
        /// The lost sequence number.
        seq: u16,
        /// The packet after the lost one, if present.
        fec_source: Option<Packet>,
    },
    /// Nothing to play: idle, pausing or still prefilling.
    Silence,
}

/// Running counters for `LossReport` and the debug view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitterStats {
    /// Packets handed out for decoding.
    pub played: u64,
    /// Frames reported lost.
    pub lost: u64,
    /// Packets that arrived after their slot had passed.
    pub late: u64,
    /// Packets dropped as duplicates.
    pub duplicates: u64,
    /// Talk spurts started (re-anchors).
    pub spurts: u64,
    /// Frames concealed because the next packet was not here yet; the
    /// buffer waits for it instead of writing it off (delay grows a frame).
    pub underruns: u64,
    /// Packets skipped before decode to shed delay grown by underruns.
    pub skipped: u64,
    /// Packets dropped because the queue held more than `max_delay` (a
    /// stall's burst); keeps the backlog from becoming lasting latency.
    pub dropped: u64,
}

/// Configuration in frames and milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct JitterConfig {
    /// Frame duration in samples at 48 kHz (960 for 20 ms).
    pub frame_samples: u32,
    /// Initial target delay in frames.
    pub start_frames: u32,
    /// Upper clamp for the target delay.
    pub max_delay: Duration,
    /// Consecutive lost frames after which the spurt is given up (100 ms).
    pub max_loss_frames: u32,
}

impl Default for JitterConfig {
    fn default() -> Self {
        JitterConfig {
            frame_samples: 960,
            start_frames: 2,
            max_delay: Duration::from_millis(200),
            max_loss_frames: 5,
        }
    }
}

const BUCKET_MS: f64 = 5.0;
const BUCKETS: usize = 64; // 0..320 ms
const FORGET: f64 = 0.983;

/// The buffer.
#[derive(Debug)]
pub struct JitterBuffer {
    cfg: JitterConfig,
    /// Queued packets keyed by their unwrapped sequence number, with their
    /// arrival time.
    queue: BTreeMap<i64, (Packet, Instant)>,
    /// Unwrapping helper: last seen (seq, unwrapped).
    last_seen: Option<(u16, i64)>,
    /// Next sequence number to play while a spurt is active.
    next: Option<i64>,
    /// Consecutive misses inside a spurt.
    misses: u32,
    /// Arrival time and ts of the first packet of the current spurt.
    anchor: Option<(Instant, u32)>,
    /// Arrival of the first queued packet while prefilling.
    prefill_since: Option<Instant>,
    /// Arrival delay histogram in 5 ms buckets.
    hist: [f64; BUCKETS],
    target: Duration,
    /// Lower clamp: one frame while the encoder is told about loss (FEC needs
    /// n+1), otherwise 5 ms.
    min_target: Duration,
    stats: JitterStats,
    last_ts: Option<u32>,
    last_end: bool,
    last_delay: Option<Duration>,
    /// Frames inserted by underruns this spurt and not yet shed again.
    grown: u32,
}

impl JitterBuffer {
    /// New buffer with `cfg`.
    pub fn new(cfg: JitterConfig) -> Self {
        let frame = Duration::from_micros(cfg.frame_samples as u64 * 1_000_000 / 48_000);
        JitterBuffer {
            cfg,
            queue: BTreeMap::new(),
            last_seen: None,
            next: None,
            misses: 0,
            anchor: None,
            prefill_since: None,
            hist: [0.0; BUCKETS],
            target: frame * cfg.start_frames,
            min_target: Duration::from_millis(5),
            stats: JitterStats::default(),
            last_ts: None,
            last_end: false,
            last_delay: None,
            grown: 0,
        }
    }

    /// Arrival-to-playout delay of the packet returned by the last `pop`.
    pub fn last_delay(&self) -> Option<Duration> {
        self.last_delay
    }

    fn frame_duration(&self) -> Duration {
        Duration::from_micros(self.cfg.frame_samples as u64 * 1_000_000 / 48_000)
    }

    /// Frame length in samples this stream is played with.
    pub fn frame_samples(&self) -> u32 {
        self.cfg.frame_samples
    }

    /// Sets the frame length from a packet's own duration: the peer picks
    /// it, and pause detection compares `ts` steps against it.
    pub fn set_frame_samples(&mut self, samples: u32) {
        if samples > 0 {
            self.cfg.frame_samples = samples;
        }
    }

    /// Current target delay.
    pub fn target(&self) -> Duration {
        self.target
    }

    /// Whether the encoder side is told about loss; raises the floor to one
    /// frame so FEC has its n+1 packet.
    pub fn set_loss_hinted(&mut self, hinted: bool) {
        self.min_target = if hinted {
            self.frame_duration()
        } else {
            Duration::from_millis(5)
        };
        self.clamp_target();
    }

    /// Counters.
    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    /// Whether a talk spurt is being played.
    pub fn active(&self) -> bool {
        self.next.is_some()
    }

    fn unwrap_seq(&mut self, seq: u16) -> i64 {
        match self.last_seen {
            None => {
                self.last_seen = Some((seq, seq as i64));
                seq as i64
            }
            Some((last, unwrapped)) => {
                let d = seq_diff(seq, last) as i64;
                let u = unwrapped + d;
                if d > 0 {
                    self.last_seen = Some((seq, u));
                }
                u
            }
        }
    }

    /// Queues a packet that arrived at `now`.
    pub fn push(&mut self, packet: Packet, now: Instant) {
        // Pause detection: gapless seq, ts jumped further than one frame.
        if let (Some((last_seq, _)), Some(last_ts)) = (self.last_seen, self.last_ts) {
            let gapless = seq_diff(packet.seq, last_seq) == 1;
            let step = ts_diff(packet.ts, last_ts);
            if gapless && step > 0 && step != self.cfg.frame_samples as i32 {
                // A pause: the spurt ended without us seeing END (or with it).
                self.end_spurt();
            }
        }
        let u = self.unwrap_seq(packet.seq);
        if self.queue.contains_key(&u) {
            self.stats.duplicates += 1;
            return;
        }
        // Arrival delay relative to the spurt anchor feeds the histogram —
        // late packets too, they are exactly the jitter the target must cover.
        if let Some((anchor_arrival, anchor_ts)) = self.anchor {
            let expected = anchor_arrival
                + Duration::from_micros(
                    ts_diff(packet.ts, anchor_ts).max(0) as u64 * 1_000_000 / 48_000,
                );
            let delay = now.saturating_duration_since(expected);
            self.observe(delay);
            // Mid-spurt the target only grows; it shrinks in pauses.
            let before = self.target;
            self.adapt();
            self.target = self.target.max(before);
        } else if self.prefill_since.is_none() {
            self.prefill_since = Some(now);
        }
        if let Some(next) = self.next {
            if u < next {
                self.stats.late += 1;
                return;
            }
        }
        if ts_diff(packet.ts, self.last_ts.unwrap_or(packet.ts)) >= 0 {
            self.last_ts = Some(packet.ts);
        }
        self.last_end = packet.end;
        self.queue.insert(u, (packet, now));
    }

    fn observe(&mut self, delay: Duration) {
        for b in self.hist.iter_mut() {
            *b *= FORGET;
        }
        let idx = ((delay.as_secs_f64() * 1000.0 / BUCKET_MS) as usize).min(BUCKETS - 1);
        self.hist[idx] += 1.0;
    }

    /// Recomputes the target from the histogram: 95 % quantile + 1 frame.
    fn adapt(&mut self) {
        let total: f64 = self.hist.iter().sum();
        if total < 4.0 {
            return;
        }
        let mut acc = 0.0;
        let mut q95 = 0usize;
        for (i, b) in self.hist.iter().enumerate() {
            acc += b;
            if acc >= 0.95 * total {
                q95 = i;
                break;
            }
        }
        let q = Duration::from_micros((((q95 + 1) as f64) * BUCKET_MS * 1000.0) as u64);
        self.target = q + self.frame_duration();
        self.clamp_target();
    }

    fn clamp_target(&mut self) {
        if self.target < self.min_target {
            self.target = self.min_target;
        }
        if self.target > self.cfg.max_delay {
            self.target = self.cfg.max_delay;
        }
    }

    fn end_spurt(&mut self) {
        if self.next.is_some() || self.anchor.is_some() {
            self.adapt();
        }
        self.next = None;
        self.anchor = None;
        self.misses = 0;
        self.prefill_since = None;
        self.grown = 0;
    }

    /// Caps the queue at `max_delay` (or target + 2 frames, if larger): past
    /// that, drops the oldest packets down to target + 1 frame (at least one
    /// packet stays). Without this a stall longer than `max_loss_frames` ends
    /// the spurt, the burst that follows is queued whole and the next prefill
    /// starts at its oldest packet: packets then arrive as fast as they play,
    /// and the backlog lasts until the next pause. Runs before the prefill for
    /// that reason. Delay grown by underruns inside a spurt is left to the
    /// gentler one-packet skip in `pop`.
    ///
    /// A hard cut is audible; dropping low-energy frames first, or only every
    /// third frame until caught up, would be gentler.
    fn drain_backlog(&mut self, frame: Duration) {
        let cap = self.cfg.max_delay.max(self.target + frame * 2);
        if self.grown > 0 || frame * self.queue.len() as u32 <= cap {
            return;
        }
        while self.queue.len() > 1 && frame * self.queue.len() as u32 > self.target + frame {
            self.queue.pop_first();
            self.stats.dropped += 1;
        }
        if self.next.is_some() {
            self.next = self.queue.keys().next().copied();
        }
    }

    /// Called once per frame period by the playout side.
    pub fn pop(&mut self, now: Instant) -> Pop {
        let frame = self.frame_duration();
        self.drain_backlog(frame);
        if self.next.is_none() {
            // Prefill: start once the target delay has elapsed since the first
            // queued packet, or enough packets are queued to cover it.
            let Some((&first, _)) = self.queue.iter().next() else {
                return Pop::Silence;
            };
            let waited = self
                .prefill_since
                .map(|t| now.saturating_duration_since(t))
                .unwrap_or_default();
            let queued = frame * self.queue.len() as u32;
            if waited < self.target && queued < self.target {
                return Pop::Silence;
            }
            let (first_pkt, first_arrival) = &self.queue[&first];
            self.anchor = Some((*first_arrival, first_pkt.ts));
            self.next = Some(first);
            self.misses = 0;
            self.stats.spurts += 1;
        }
        let mut next = self.next.unwrap_or(0);
        // Delay grown by underruns beyond what the target asks for is shed by
        // skipping one packet before decode (RFC 6716 §4.4.1).
        let queued = frame * self.queue.range(next..).count() as u32;
        if self.grown > 0 && queued > self.target + 2 * frame && self.queue.remove(&next).is_some()
        {
            self.grown -= 1;
            self.stats.skipped += 1;
            next += 1;
            self.next = Some(next);
        }
        if let Some((pkt, arrived)) = self.queue.remove(&next) {
            self.last_delay = Some(now.saturating_duration_since(arrived));
            self.misses = 0;
            self.stats.played += 1;
            let end = pkt.end;
            self.next = Some(next + 1);
            if end {
                self.end_spurt();
            }
            return Pop::Packet(pkt);
        }
        // Slot `next` is empty.
        let later = self
            .queue
            .range(next + 1..)
            .next()
            .map(|(k, (p, _))| (*k, p.clone()));
        match later {
            Some((k, pkt)) => {
                // A later packet is here: `next` is lost, not late.
                self.stats.lost += 1;
                self.misses = 0;
                let fec_source = if k == next + 1 { Some(pkt) } else { None };
                let seq = (next & 0xFFFF) as u16;
                self.next = Some(next + 1);
                Pop::Lost { seq, fec_source }
            }
            None => {
                if self.last_end {
                    // The spurt ended cleanly; nothing more to play.
                    self.end_spurt();
                    return Pop::Silence;
                }
                self.misses += 1;
                if self.misses > self.cfg.max_loss_frames {
                    self.end_spurt();
                    return Pop::Silence;
                }
                // Nothing queued past `next`: the buffer ran dry, the packet is
                // most likely just late. Conceal this period but keep waiting
                // for it — writing it off would drop it on arrival and leave
                // the buffer just as shallow for the next jitter spike.
                self.stats.underruns += 1;
                self.grown += 1;
                let seq = (next & 0xFFFF) as u16;
                Pop::Lost {
                    seq,
                    fec_source: None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(seq: u16, ts: u32, end: bool) -> Packet {
        Packet {
            seq,
            ts,
            end,
            payload: vec![seq as u8],
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn buffer() -> JitterBuffer {
        JitterBuffer::new(JitterConfig::default())
    }

    #[test]
    fn prefill_then_plays_in_order() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        assert_eq!(jb.pop(t0), Pop::Silence, "one packet, target 2 frames");
        jb.push(pkt(1, 960, false), t0 + ms(20));
        // Two frames queued = 40 ms = target → starts.
        assert!(matches!(jb.pop(t0 + ms(20)), Pop::Packet(p) if p.seq == 0));
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(p) if p.seq == 1));
    }

    #[test]
    fn seq_gap_is_loss_with_fec_source() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, false), t0 + ms(20));
        jb.push(pkt(3, 3 * 960, false), t0 + ms(60));
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Packet(p) if p.seq == 0));
        assert!(matches!(jb.pop(t0 + ms(80)), Pop::Packet(p) if p.seq == 1));
        match jb.pop(t0 + ms(100)) {
            Pop::Lost { seq, fec_source } => {
                assert_eq!(seq, 2);
                assert_eq!(fec_source.map(|p| p.seq), Some(3));
            }
            other => panic!("expected loss, got {other:?}"),
        }
        assert!(matches!(jb.pop(t0 + ms(120)), Pop::Packet(p) if p.seq == 3));
        assert_eq!(jb.stats().lost, 1);
    }

    #[test]
    fn ts_jump_with_gapless_seq_is_a_pause_not_loss() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, true), t0 + ms(20));
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(p) if p.seq == 0));
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Packet(p) if p.seq == 1));
        assert_eq!(jb.pop(t0 + ms(80)), Pop::Silence);
        assert_eq!(jb.pop(t0 + ms(400)), Pop::Silence);
        // Resume after 500 ms: seq 2, ts far ahead.
        jb.push(pkt(2, 960 + 48_000 / 2, false), t0 + ms(520));
        jb.push(pkt(3, 960 + 48_000 / 2 + 960, false), t0 + ms(540));
        assert!(matches!(jb.pop(t0 + ms(540)), Pop::Packet(p) if p.seq == 2));
        assert_eq!(jb.stats().lost, 0, "a pause is not loss");
        assert_eq!(jb.stats().spurts, 2, "re-anchored");
    }

    #[test]
    fn pause_without_end_flag_re_anchors() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, false), t0 + ms(20));
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(_)));
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Packet(_)));
        // Sender stopped without END (packet lost); PLC for up to 100 ms.
        for i in 0..5 {
            assert!(
                matches!(jb.pop(t0 + ms(80 + 20 * i)), Pop::Lost { .. }),
                "PLC frame {i}"
            );
        }
        assert_eq!(jb.pop(t0 + ms(200)), Pop::Silence, "> 100 ms → mute");
        assert!(!jb.active());
        // Gapless resume with a ts jump → new spurt, no additional loss.
        let lost_before = jb.stats().lost;
        jb.push(pkt(2, 960 + 20_000, false), t0 + ms(600));
        jb.push(pkt(3, 960 + 20_000 + 960, false), t0 + ms(620));
        assert!(matches!(jb.pop(t0 + ms(620)), Pop::Packet(p) if p.seq == 2));
        assert_eq!(jb.stats().lost, lost_before);
    }

    #[test]
    fn wrap_over_seq_and_ts_boundary() {
        let t0 = Instant::now();
        let mut jb = buffer();
        let mut seq = u16::MAX - 1;
        let mut ts = u32::MAX - 960;
        for i in 0..6u64 {
            jb.push(pkt(seq, ts, false), t0 + ms(20 * i));
            seq = seq.wrapping_add(1);
            ts = ts.wrapping_add(960);
        }
        let mut expect = u16::MAX - 1;
        for i in 0..6u64 {
            match jb.pop(t0 + ms(120 + 20 * i)) {
                Pop::Packet(p) => assert_eq!(p.seq, expect),
                other => panic!("expected packet {expect}, got {other:?}"),
            }
            expect = expect.wrapping_add(1);
        }
        assert_eq!(jb.stats().lost, 0);
        assert_eq!(jb.stats().late, 0);
    }

    #[test]
    fn late_and_duplicate_packets_are_counted() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, false), t0 + ms(20));
        jb.push(pkt(1, 960, false), t0 + ms(21));
        assert_eq!(jb.stats().duplicates, 1);
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(_)));
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Packet(_)));
        jb.push(pkt(0, 0, false), t0 + ms(61));
        assert_eq!(jb.stats().late, 1);
    }

    #[test]
    fn target_adapts_to_arrival_jitter_and_is_clamped() {
        let t0 = Instant::now();
        let mut jb = buffer();
        assert_eq!(jb.target(), ms(40));
        // Spurt 1 with 60 ms of arrival jitter on every second packet.
        let mut t = t0;
        for i in 0..40u64 {
            let jitter = if i % 2 == 1 { ms(60) } else { ms(0) };
            jb.push(pkt(i as u16, i as u32 * 960, i == 39), t + jitter);
            // Pop as we go so the anchor exists.
            let _ = jb.pop(t + ms(40));
            t += ms(20);
        }
        while jb.active() {
            let _ = jb.pop(t);
            t += ms(20);
        }
        assert!(
            jb.target() >= ms(60) && jb.target() <= ms(200),
            "target {:?} should cover the observed jitter",
            jb.target()
        );
        jb.set_loss_hinted(true);
        assert!(jb.target() >= ms(20));
    }

    #[test]
    fn min_target_follows_loss_hint() {
        let mut jb = buffer();
        jb.set_loss_hinted(true);
        assert!(jb.target() >= ms(20));
        jb.set_loss_hinted(false);
        assert!(jb.target() >= ms(5));
    }

    #[test]
    fn late_packet_after_underrun_is_played_not_dropped() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, false), t0 + ms(20));
        assert!(matches!(jb.pop(t0 + ms(20)), Pop::Packet(p) if p.seq == 0));
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(p) if p.seq == 1));
        // Packet 2 is 30 ms late: the buffer runs dry and conceals once.
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Lost { seq: 2, .. }));
        jb.push(pkt(2, 2 * 960, false), t0 + ms(70));
        assert!(matches!(jb.pop(t0 + ms(80)), Pop::Packet(p) if p.seq == 2));
        let st = jb.stats();
        assert_eq!((st.late, st.lost, st.underruns), (0, 0, 1));
    }

    #[test]
    fn peer_with_10_ms_frames_is_not_a_pause_every_packet() {
        let t0 = Instant::now();
        let mut jb = buffer();
        for i in 0..20u64 {
            jb.set_frame_samples(480);
            jb.push(pkt(i as u16, i as u32 * 480, false), t0 + ms(10 * i));
        }
        assert!(jb.stats().spurts <= 1);
        for i in 0..20u64 {
            let _ = jb.pop(t0 + ms(200 + 10 * i));
        }
        let st = jb.stats();
        assert_eq!((st.played, st.lost), (20, 0));
    }

    #[test]
    fn delay_grown_by_underruns_is_shed_again() {
        let t0 = Instant::now();
        // The target is clamped below the stall, so the stall's backlog is
        // excess delay.
        let mut jb = JitterBuffer::new(JitterConfig {
            max_delay: ms(60),
            ..JitterConfig::default()
        });
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, false), t0 + ms(20));
        assert!(matches!(jb.pop(t0 + ms(20)), Pop::Packet(_)));
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(_)));
        // A 100 ms stall, then the backlog arrives at once.
        for i in 0..4u64 {
            assert!(matches!(jb.pop(t0 + ms(60 + 20 * i)), Pop::Lost { .. }));
        }
        for i in 2..8u16 {
            jb.push(pkt(i, i as u32 * 960, false), t0 + ms(140));
        }
        let mut t = t0 + ms(140);
        while jb.stats().skipped == 0 && t < t0 + ms(400) {
            let _ = jb.pop(t);
            t += ms(20);
        }
        assert!(jb.stats().skipped > 0, "grown delay is shed");
        assert_eq!(jb.stats().late, 0);
    }

    #[test]
    fn backlog_after_a_long_stall_is_dropped_not_kept_as_latency() {
        // Continuous speech, 20 ms frames, 20 ms network delay, playout every
        // 20 ms. At t = 3 s the network stalls for 400 ms, and everything
        // sent meanwhile arrives at once when it ends.
        let t0 = Instant::now();
        let mut jb = buffer();
        let arrival = |i: u64| {
            let normal = 20 * i + 20;
            if (3000..3400).contains(&normal) {
                3400
            } else {
                normal
            }
        };
        let mut sent = 0u64;
        let mut delay_after = Duration::ZERO;
        for tick in 0..300u64 {
            let now_ms = 20 * tick + 10;
            while arrival(sent) <= now_ms {
                jb.push(
                    pkt(sent as u16, sent as u32 * 960, false),
                    t0 + ms(arrival(sent)),
                );
                sent += 1;
            }
            if let Pop::Packet(_) = jb.pop(t0 + ms(now_ms)) {
                if now_ms >= 5000 {
                    delay_after = delay_after.max(jb.last_delay().unwrap_or_default());
                }
            }
        }
        let dropped = jb.stats().dropped;
        assert!(
            delay_after < ms(100),
            "backlog stays as latency: {delay_after:?} (dropped {dropped})"
        );
        assert!((10..=20).contains(&dropped), "dropped {dropped}");
    }
}
