//! Adaptive jitter buffer, one per speaker (`docs/reference/client.md` §6,
//! `docs/research/latency-budget.md`).
//!
//! Pure data structure: the caller injects `now` and pops once per frame
//! period. All `seq`/`ts` comparisons are signed differences modulo 2^16 /
//! 2^32 (`docs/reference/protocol.md` §3). Loss (a `seq` gap) is told apart from a pause
//! (a `ts` jump with gapless `seq`); the latter re-anchors the buffer.
//!
//! Depth. The playout runs `target` behind the spurt's earliest arrival
//! (the anchor): a packet is on time as long as its arrival delay relative
//! to the anchor stays below that margin. The target is the larger of two
//! terms — the 95 % quantile of the delay histogram plus a frame (jitter),
//! and the tallest delay of the last 20–40 s that the jitter term did not
//! cover, plus two frames (a stall that recurs, as Wi-Fi power saving and
//! congested links produce). Inside a spurt the depth follows a grown
//! target by concealing extra frames — while an underrun is being concealed
//! anyway, or at a quiet frame — and sheds excess by skipping a packet
//! before decode (RFC 6716 §4.4.1), so a monologue adapts without waiting
//! for a pause; between spurts the prefill sets the depth outright.

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
    pub gain_epoch: u32,
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
    /// Frames concealed on purpose to deepen the buffer to a target that
    /// grew inside a spurt (`stretch`).
    pub stretched: u64,
    /// Packets skipped before decode to shed excess delay.
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
/// How much earlier than the anchor predicts a packet may arrive and still
/// move the anchor: the histogram's range.
const MAX_EARLY: Duration = Duration::from_millis((BUCKETS as f64 * BUCKET_MS) as u64);
/// Per-observation forgetting: 1/(1 − FORGET) ≈ 330 packets, 6–7 s of
/// speech at 20 ms, so a spike seen a few spurts ago still counts. The old
/// 0.983 (60 packets) was NetEQ's 2015 inter-arrival factor; NetEQ has
/// since moved to 0.9993 on relative arrival delay — the quantity measured
/// here. With 60 packets of memory a 10 % spike share dropped below the
/// 95 % quantile at a third of all spurt ends and the target collapsed
/// (`spurt_target_does_not_depend_on_the_anchor_packet`).
const FORGET: f64 = 0.997;
/// Consecutive pops with more than target + 2 frames queued before one
/// packet is skipped to shed the excess: half a second at 20 ms frames.
const EXCESS_POPS: u32 = 25;
/// Pops a due stretch or skip waits for a quiet frame before it goes ahead
/// in the middle of speech: a second at 20 ms frames. A pause between
/// words normally comes sooner; a speaker who never pauses gets the
/// adjustment anyway rather than the underrun it prevents.
const QUIET_PATIENCE: u32 = 50;
/// Half of the stall memory: the tallest spike of the current and the
/// previous half-window sets the target, so a stall is remembered for
/// 20–40 s. Shorter, and a monologue's target would sag between stalls
/// that keep coming every few seconds.
const SPIKE_HALF_WINDOW: Duration = Duration::from_secs(20);
/// Frames of headroom above the tallest spike: one because the spike
/// measured a packet, not the stall — the next packet of the same stall
/// can be up to a frame later — and one for a stall that is a little
/// taller than the last.
const SPIKE_HEADROOM_FRAMES: u32 = 2;

/// Arrival time, `ts` and unwrapped `seq` of the earliest-arriving packet of
/// the current spurt; every later packet's expected arrival is measured
/// from it.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    arrival: Instant,
    ts: u32,
    seq: i64,
}

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
    anchor: Option<Anchor>,
    /// Arrival of the first queued packet while prefilling.
    prefill_since: Option<Instant>,
    /// Arrival delay histogram in 5 ms buckets.
    hist: [f64; BUCKETS],
    /// Tallest spike of the current half-window and when it started.
    spike_cur: Duration,
    spike_since: Option<Instant>,
    /// Tallest spike of the previous half-window.
    spike_prev: Duration,
    target: Duration,
    stats: JitterStats,
    last_ts: Option<u32>,
    last_end: bool,
    last_delay: Option<Duration>,
    /// Frames inserted by underruns this spurt and not yet shed again.
    grown: u32,
    /// Consecutive pops that found more than target + 2 frames queued.
    excess_pops: u32,
    /// Pops a due stretch has waited for a quiet frame.
    stretch_wait: u32,
    /// The last pop concealed a frame without advancing (underrun or
    /// stretch): another concealment continues it instead of starting a
    /// second interruption.
    concealing: bool,
    /// Unwrapped seq of the last slot `pop` consumed (played or written off
    /// as lost). Survives the end of a spurt: a straggler from a finished
    /// spurt is late, not the start of a new one.
    played_hwm: Option<i64>,
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
            spike_cur: Duration::ZERO,
            spike_since: None,
            spike_prev: Duration::ZERO,
            target: frame * cfg.start_frames,
            stats: JitterStats::default(),
            last_ts: None,
            last_end: false,
            last_delay: None,
            grown: 0,
            excess_pops: 0,
            stretch_wait: 0,
            concealing: false,
            played_hwm: None,
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
        // The anchor is the earliest arrival seen this spurt, not the first:
        // a first packet that was itself delayed would make everything after
        // it look punctual, and the histogram would learn no jitter at all
        // from a spurt full of it. A packet that arrives earlier than the
        // anchor predicts therefore becomes the anchor (delay 0), and the
        // remaining packets are measured against it.
        if let Some(anchor) = self.anchor.as_mut() {
            let expected = anchor.arrival
                + Duration::from_micros(
                    ts_diff(packet.ts, anchor.ts).max(0) as u64 * 1_000_000 / 48_000,
                );
            let delay = if now >= expected {
                now - expected
            } else {
                // Only as early as a stalled anchor can explain (the
                // histogram's range). Anything earlier is a `ts` that
                // jumped ahead, not jitter: leave the anchor alone. The
                // subtraction is checked because `Instant` panics below
                // its epoch, and a peer's `ts` is not to be trusted.
                let early = expected - now;
                if early <= MAX_EARLY {
                    if let Some(a) = anchor.arrival.checked_sub(early) {
                        anchor.arrival = a;
                    }
                }
                Duration::ZERO
            };
            self.observe(delay, now);
            // Mid-spurt the target only grows; it shrinks in pauses.
            let before = self.target;
            self.adapt();
            self.target = self.target.max(before);
            if delay >= Duration::from_millis(25) {
                tracing::debug!(
                    seq = packet.seq,
                    delay_ms = delay.as_millis() as u64,
                    target_ms = self.target.as_millis() as u64,
                    queued = self.queue.len(),
                    played = self.stats.played,
                    "jitter spike"
                );
            }
        } else if self.prefill_since.is_none() {
            self.prefill_since = Some(now);
        }
        // Late: its slot has been consumed already — inside the spurt, or in
        // a spurt that has since ended (`played_hwm` outlives `next`).
        if self.played_hwm.is_some_and(|h| u <= h) || self.next.is_some_and(|n| u < n) {
            self.stats.late += 1;
            return;
        }
        if ts_diff(packet.ts, self.last_ts.unwrap_or(packet.ts)) >= 0 {
            self.last_ts = Some(packet.ts);
        }
        self.last_end = packet.end;
        self.queue.insert(u, (packet, now));
    }

    fn observe(&mut self, delay: Duration, now: Instant) {
        // Stall memory: a delay the jitter term does not cover is a spike,
        // and the tallest of the last 20–40 s stays in the target.
        let since = *self.spike_since.get_or_insert(now);
        let elapsed = now.saturating_duration_since(since);
        if elapsed >= SPIKE_HALF_WINDOW {
            self.spike_prev = if elapsed >= 2 * SPIKE_HALF_WINDOW {
                Duration::ZERO
            } else {
                self.spike_cur
            };
            self.spike_cur = Duration::ZERO;
            self.spike_since = Some(now);
        }
        if delay > self.spike_threshold() {
            self.spike_cur = self.spike_cur.max(delay);
        }
        for b in self.hist.iter_mut() {
            *b *= FORGET;
        }
        let idx = ((delay.as_secs_f64() * 1000.0 / BUCKET_MS) as usize).min(BUCKETS - 1);
        self.hist[idx] += 1.0;
    }

    /// The tallest spike remembered.
    fn spike(&self) -> Duration {
        self.spike_cur.max(self.spike_prev)
    }

    /// Upper edge of the histogram bucket at quantile `q`, once four
    /// observations are in.
    fn quantile(&self, q: f64) -> Option<Duration> {
        let total: f64 = self.hist.iter().sum();
        if total < 4.0 {
            return None;
        }
        let mut acc = 0.0;
        let mut bucket = 0usize;
        for (i, b) in self.hist.iter().enumerate() {
            acc += b;
            if acc >= q * total {
                bucket = i;
                break;
            }
        }
        Some(Duration::from_micros(
            (((bucket + 1) as f64) * BUCKET_MS * 1000.0) as u64,
        ))
    }

    /// 95 % quantile of the histogram + 1 frame, or the start target while
    /// the histogram is still empty.
    fn jitter_target(&self) -> Duration {
        match self.quantile(0.95) {
            Some(q) => q + self.frame_duration(),
            None => self.frame_duration() * self.cfg.start_frames,
        }
    }

    /// Delays above this are stalls, not jitter: a whole frame over the
    /// median, i.e. the packet came after its successor was due. The median
    /// rather than the 95 % quantile because the latter is a handful of
    /// packets early in a spurt — one moderately late packet of a burst,
    /// delivered ahead of its taller sibling, once lifted it to the
    /// sibling's height and hid the stall (traced 2026-09-26). The spike
    /// term is a maximum, so with a stable threshold the order inside a
    /// burst no longer matters.
    fn spike_threshold(&self) -> Duration {
        self.quantile(0.5).unwrap_or_default() + self.frame_duration()
    }

    /// Recomputes the target: the jitter term, or the tallest remembered
    /// spike plus its headroom, whichever is larger.
    fn adapt(&mut self) {
        let jitter = self.jitter_target();
        let spike = self.spike();
        self.target = if spike.is_zero() {
            jitter
        } else {
            jitter.max(spike + self.frame_duration() * SPIKE_HEADROOM_FRAMES)
        };
        self.clamp_target();
    }

    /// Lower clamp: one frame, always. Senders code in-band FEC at every
    /// loss level (`dsp::opus::FEC_FLOOR_PERCENT`), and recovering frame n
    /// needs packet n+1 already here when n is due.
    fn clamp_target(&mut self) {
        let floor = self.frame_duration();
        if self.target < floor {
            self.target = floor;
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
        // The next short spurt may already be queued behind this END. It
        // must retain an arrival clock even if no further packet arrives.
        self.prefill_since = self.queue.values().map(|(_, at)| *at).min();
        self.grown = 0;
        self.excess_pops = 0;
        self.stretch_wait = 0;
        self.concealing = false;
    }

    /// How far behind the anchor's schedule slot `slot` is played at `now`:
    /// the margin a packet of that slot had against arriving late.
    fn margin(&self, slot: i64, now: Instant, frame: Duration) -> Duration {
        let Some(anchor) = self.anchor else {
            return Duration::ZERO;
        };
        let ahead = (slot - anchor.seq).clamp(0, u32::MAX as i64) as u32;
        now.saturating_duration_since(anchor.arrival + frame * ahead)
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
            if let Some((k, _)) = self.queue.pop_first() {
                // Dropped counts as consumed: a late twin must not be queued
                // behind the packets that replaced it.
                self.played_hwm = Some(self.played_hwm.map_or(k, |h| h.max(k)));
            }
            self.stats.dropped += 1;
        }
        if self.next.is_some() {
            self.next = self.queue.keys().next().copied();
        }
    }

    /// The most slots a loss burst can span with the packet after it
    /// already here: the queue never holds more than `max_delay`.
    fn max_gap(&self, frame: Duration) -> i64 {
        (self.cfg.max_delay.as_micros() / frame.as_micros().max(1)) as i64
    }

    /// Called once per frame period by the playout side; adjustments that
    /// can wait for a quiet frame are made at once.
    pub fn pop(&mut self, now: Instant) -> Pop {
        self.pop_quiet(now, true)
    }

    /// [`JitterBuffer::pop`] told whether the frame last decoded for this
    /// speaker was quiet: a stretch (an extra concealed frame) or a skip
    /// that is due waits up to `QUIET_PATIENCE` pops for one, so the
    /// adjustment lands in a pause instead of a word.
    pub fn pop_quiet(&mut self, now: Instant, quiet: bool) -> Pop {
        let frame = self.frame_duration();
        self.drain_backlog(frame);
        if self.next.is_none() {
            // Prefill: start once the first packet has waited the target,
            // or once the queue holds the target plus the frame about to
            // play (a burst) — either way the playout then runs `target`
            // behind the earliest arrival.
            let Some((&first, _)) = self.queue.iter().next() else {
                self.concealing = false;
                return Pop::Silence;
            };
            let waited = self
                .prefill_since
                .map(|t| now.saturating_duration_since(t))
                .unwrap_or_default();
            let queued = frame * self.queue.len() as u32;
            if waited < self.target && queued < self.target + frame {
                return Pop::Silence;
            }
            let (first_pkt, first_arrival) = &self.queue[&first];
            self.anchor = Some(Anchor {
                arrival: *first_arrival,
                ts: first_pkt.ts,
                seq: first,
            });
            self.next = Some(first);
            self.misses = 0;
            self.stretch_wait = 0;
            self.concealing = false;
            self.stats.spurts += 1;
            tracing::debug!(
                waited_ms = waited.as_millis() as u64,
                queued_ms = queued.as_millis() as u64,
                target_ms = self.target.as_millis() as u64,
                played = self.stats.played,
                "jitter prefill done"
            );
        }
        let mut next = self.next.unwrap_or(0);
        // Delay beyond what the target asks for is shed by skipping one
        // packet before decode (RFC 6716 §4.4.1): at once when underruns
        // grew it (`grown`); otherwise — a burst at spurt start, a sender
        // whose clock runs fast — only once more than target + 2 frames
        // has been queued for `EXCESS_POPS` pops, so a jitter spike the
        // target is still absorbing is not mistaken for it, and preferably
        // at a quiet frame. Two frames, because the playout sits up to a
        // frame past the target after a prefill or stretch, and the queue
        // is measured in whole frames.
        let queued = frame * self.queue.range(next..).count() as u32;
        let excess = queued > self.target + 2 * frame;
        self.excess_pops = if excess { self.excess_pops + 1 } else { 0 };
        let shed = excess
            && (self.grown > 0
                || (self.excess_pops >= EXCESS_POPS
                    && (quiet || self.excess_pops >= EXCESS_POPS + QUIET_PATIENCE)));
        if shed && self.queue.remove(&next).is_some() {
            tracing::debug!(
                seq = (next & 0xFFFF) as u16,
                queued_ms = queued.as_millis() as u64,
                target_ms = self.target.as_millis() as u64,
                grown = self.grown,
                excess_pops = self.excess_pops,
                played = self.stats.played,
                "jitter skip"
            );
            self.grown = self.grown.saturating_sub(1);
            self.excess_pops = 0;
            self.stats.skipped += 1;
            next += 1;
            self.next = Some(next);
        }
        // Stretch: the target grew more than a frame beyond the depth the
        // playout actually has — the margin against the anchor's schedule,
        // or the queue if a burst filled it deeper — so one more frame is
        // concealed before this packet and the schedule moves a frame
        // later. Right away while a concealment is running anyway (the
        // underrun that revealed the stall), at a quiet frame otherwise;
        // and once started it goes on until the depth reaches the target,
        // so the next spike a little taller than the last is covered
        // without a second interruption. Only with the packet here: a
        // missing one is an underrun below.
        if self.queue.contains_key(&next) {
            let depth = self
                .margin(next, now, frame)
                .max(queued.saturating_sub(frame));
            let short = if self.concealing {
                depth < self.target
            } else {
                depth + frame < self.target
            };
            if short {
                if quiet || self.concealing || self.stretch_wait >= QUIET_PATIENCE {
                    tracing::debug!(
                        seq = (next & 0xFFFF) as u16,
                        depth_ms = depth.as_millis() as u64,
                        target_ms = self.target.as_millis() as u64,
                        played = self.stats.played,
                        "jitter stretch"
                    );
                    self.stretch_wait = 0;
                    self.concealing = true;
                    self.grown = 0;
                    self.stats.stretched += 1;
                    return Pop::Lost {
                        seq: (next & 0xFFFF) as u16,
                        fec_source: None,
                    };
                }
                self.stretch_wait += 1;
            } else {
                self.stretch_wait = 0;
            }
        }
        if let Some((pkt, arrived)) = self.queue.remove(&next) {
            self.last_delay = Some(now.saturating_duration_since(arrived));
            self.misses = 0;
            self.concealing = false;
            self.stats.played += 1;
            let end = pkt.end;
            self.next = Some(next + 1);
            self.played_hwm = Some(next);
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
            Some((k, _)) if k - next > self.max_gap(frame) => {
                // Further ahead than the buffer ever holds: not a loss burst
                // (that ends the spurt after `max_loss_frames` misses) but a
                // `seq` that jumped. Walked slot by slot, one packet 32 767
                // ahead concealed as many frames — 11 minutes of Deep PLC in
                // the mixer. A new spurt starts at it instead.
                tracing::debug!(
                    next = (next & 0xFFFF) as u16,
                    ahead = k - next,
                    "jitter seq jump, re-anchoring"
                );
                self.end_spurt();
                self.prefill_since = Some(now);
                Pop::Silence
            }
            Some((k, pkt)) => {
                // A later packet is here: `next` is lost, not late.
                self.stats.lost += 1;
                self.misses = 0;
                self.concealing = false;
                let fec_source = if k == next + 1 { Some(pkt) } else { None };
                let seq = (next & 0xFFFF) as u16;
                tracing::debug!(
                    seq,
                    present = (k & 0xFFFF) as u16,
                    queued = self.queue.len(),
                    played = self.stats.played,
                    "jitter loss"
                );
                self.next = Some(next + 1);
                self.played_hwm = Some(next);
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
                self.concealing = true;
                let seq = (next & 0xFFFF) as u16;
                tracing::debug!(
                    seq,
                    target_ms = self.target.as_millis() as u64,
                    grown = self.grown,
                    misses = self.misses,
                    played = self.stats.played,
                    last_delay_ms = self.last_delay.map_or(0, |d| d.as_millis() as u64),
                    "jitter underrun"
                );
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
            gain_epoch: 0,
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
    fn queued_short_spurt_plays_after_previous_end_without_more_arrivals() {
        let now = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, true), now);
        jb.push(pkt(1, 1920, true), now + ms(20));
        assert_eq!(jb.pop(now + ms(60)), Pop::Packet(pkt(0, 0, true)));
        assert_eq!(jb.pop(now + ms(80)), Pop::Packet(pkt(1, 1920, true)));
        assert_eq!(jb.pop(now + ms(1000)), Pop::Silence);
        assert_eq!(jb.stats().played, 2);
        assert_eq!(jb.stats().lost, 0);
    }

    #[test]
    fn prefill_then_plays_in_order() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        assert_eq!(jb.pop(t0), Pop::Silence, "one packet, target 40 ms");
        jb.push(pkt(1, 960, false), t0 + ms(20));
        // Two frames queued cover the target but not the frame about to
        // play; the first packet starts once it has waited the target.
        assert_eq!(jb.pop(t0 + ms(20)), Pop::Silence);
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(p) if p.seq == 0));
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Packet(p) if p.seq == 1));
        assert_eq!(
            jb.last_delay(),
            Some(ms(40)),
            "each packet waited the target"
        );
    }

    #[test]
    fn a_burst_that_covers_the_target_starts_at_once() {
        let t0 = Instant::now();
        let mut jb = buffer();
        for i in 0..3u16 {
            jb.push(pkt(i, i as u32 * 960, false), t0);
        }
        assert!(matches!(jb.pop(t0), Pop::Packet(p) if p.seq == 0));
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
        assert_eq!(jb.pop(t0 + ms(540)), Pop::Silence, "prefilling again");
        assert!(matches!(jb.pop(t0 + ms(560)), Pop::Packet(p) if p.seq == 2));
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
        assert_eq!(jb.pop(t0 + ms(620)), Pop::Silence, "prefilling again");
        assert!(matches!(jb.pop(t0 + ms(640)), Pop::Packet(p) if p.seq == 2));
        assert_eq!(jb.stats().lost, lost_before);
    }

    /// One packet with a `seq` as far ahead as a signed step reaches
    /// (32 767) is a jump, not a loss burst: the buffer starts a new spurt
    /// at it instead of concealing every slot in between.
    #[test]
    fn a_seq_far_ahead_starts_a_new_spurt() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, false), t0 + ms(20));
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(p) if p.seq == 0));
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Packet(p) if p.seq == 1));
        let far = 1u16.wrapping_add(32767);
        jb.push(pkt(far, 2 * 960, false), t0 + ms(70));
        let mut t = t0 + ms(80);
        let mut concealed = 0;
        for pops in 0.. {
            assert!(pops < 20, "{concealed} frames concealed and counting");
            match jb.pop(t) {
                Pop::Packet(p) => {
                    assert_eq!(p.seq, far);
                    break;
                }
                Pop::Lost { .. } => concealed += 1,
                Pop::Silence => {}
            }
            t += ms(20);
        }
        assert_eq!(concealed, 0);
        assert_eq!(jb.stats().lost, 0);
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
    fn straggler_after_spurt_end_is_late_not_a_new_spurt() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, false), t0 + ms(20));
        jb.push(pkt(3, 3 * 960, true), t0 + ms(60));
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Packet(p) if p.seq == 0));
        assert!(matches!(jb.pop(t0 + ms(80)), Pop::Packet(p) if p.seq == 1));
        assert!(matches!(jb.pop(t0 + ms(100)), Pop::Lost { seq: 2, .. }));
        assert!(matches!(jb.pop(t0 + ms(120)), Pop::Packet(p) if p.seq == 3));
        assert!(!jb.active(), "END ended the spurt");
        // Packet 2 turns up now, reordered behind the END packet. Before the
        // high-water mark it prefilled and played as a 20 ms spurt of its own.
        jb.push(pkt(2, 2 * 960, false), t0 + ms(130));
        assert_eq!(
            jb.pop(t0 + ms(140)),
            Pop::Silence,
            "straggler must stay silent"
        );
        assert_eq!(jb.pop(t0 + ms(400)), Pop::Silence);
        let st = jb.stats();
        assert_eq!((st.late, st.spurts, st.played), (1, 1, 3));
        // The next real spurt starts normally.
        jb.push(pkt(4, 4 * 960 + 48_000, false), t0 + ms(1200));
        jb.push(pkt(5, 5 * 960 + 48_000, false), t0 + ms(1220));
        assert!(matches!(jb.pop(t0 + ms(1240)), Pop::Packet(p) if p.seq == 4));
        assert_eq!(jb.stats().spurts, 2);
    }

    #[test]
    fn excess_depth_from_a_start_burst_is_shed_one_packet_at_a_time() {
        // Five packets land at once (the receiver joined a running stream
        // during a stall of its own), then the stream continues at pace.
        // With a 40 ms target the buffer would otherwise play 100 ms behind
        // for as long as the sender never pauses.
        let t0 = Instant::now();
        let mut jb = buffer();
        for i in 0..5u16 {
            jb.push(pkt(i, i as u32 * 960, false), t0);
        }
        let mut t = t0;
        let mut delays = Vec::new();
        for i in 0..150u16 {
            if let Pop::Packet(_) = jb.pop(t) {
                delays.push(jb.last_delay().unwrap());
            }
            t += ms(20);
            let seq = 5 + i;
            jb.push(pkt(seq, seq as u32 * 960, false), t);
        }
        let st = jb.stats();
        assert!(st.skipped >= 1, "excess must be shed: {st:?}");
        assert!(st.skipped <= 3, "but gently: {st:?}");
        assert_eq!(st.lost + st.late + st.underruns + st.stretched, 0, "{st:?}");
        // Steady state before the skip: four frames queued at every pop.
        let before = delays[10];
        let after = *delays.last().unwrap();
        assert!(before >= ms(75), "burst depth {before:?}");
        assert!(
            after + ms(15) < before,
            "delay should have dropped by a frame: {before:?} → {after:?}"
        );
        assert!(
            after <= jb.target() + ms(25),
            "{after:?} vs target {:?}",
            jb.target()
        );
    }

    #[test]
    fn steady_stream_within_target_is_never_skipped() {
        let t0 = Instant::now();
        let mut jb = buffer();
        let mut t = t0;
        for i in 0..200u16 {
            jb.push(pkt(i, i as u32 * 960, false), t);
            let _ = jb.pop(t + ms(5));
            t += ms(20);
        }
        let st = jb.stats();
        assert_eq!(
            (st.skipped, st.lost, st.underruns, st.stretched),
            (0, 0, 0, 0),
            "{st:?}"
        );
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
        assert!(jb.target() >= ms(20));
    }

    /// The floor is one frame so FEC always finds packet n+1.
    #[test]
    fn target_never_below_one_frame() {
        let jb = buffer();
        assert!(jb.target() >= ms(20));
    }

    /// A packet 50 ms late runs the buffer dry: one frame is concealed
    /// while waiting for it, and since a 50 ms stall is a spike the jitter
    /// term did not cover, the target jumps to 50 + 2 frames and the
    /// concealment is extended until the depth is there. The packet is then
    /// played, never dropped, and the next stall of that size finds the
    /// buffer deep enough.
    #[test]
    fn late_packet_deepens_the_buffer_and_is_played_not_dropped() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, false), t0 + ms(20));
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(p) if p.seq == 0));
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Packet(p) if p.seq == 1));
        assert!(matches!(jb.pop(t0 + ms(80)), Pop::Lost { seq: 2, .. }));
        jb.push(pkt(2, 2 * 960, false), t0 + ms(90));
        assert_eq!(jb.target(), ms(90), "spike 50 ms + 2 frames");
        for t in [100, 120] {
            assert!(
                matches!(jb.pop(t0 + ms(t)), Pop::Lost { seq: 2, .. }),
                "the concealment is extended to reach the new depth"
            );
        }
        assert!(matches!(jb.pop(t0 + ms(140)), Pop::Packet(p) if p.seq == 2));
        let st = jb.stats();
        assert_eq!(
            (st.late, st.lost, st.underruns, st.stretched),
            (0, 0, 1, 2),
            "{st:?}"
        );
        // The stream goes on punctually, packet i playing at 20 i + 100 ms;
        // packet 6 is 50 ms late again and still in time.
        for i in 3..12u16 {
            let late = if i == 6 { 50 } else { 0 };
            jb.push(pkt(i, i as u32 * 960, false), t0 + ms(20 * i as u64 + late));
            let p = jb.pop(t0 + ms(20 * i as u64 + 100));
            assert!(matches!(p, Pop::Packet(ref q) if q.seq == i), "{i}: {p:?}");
        }
        let st = jb.stats();
        assert_eq!(
            (st.underruns, st.stretched, st.skipped),
            (1, 2, 0),
            "{st:?}"
        );
    }

    /// Plays six punctual packets so the jitter term settles at 25 ms, then
    /// delivers packet 6 thirty ms late: in time (the depth is 40 ms), but a
    /// spike, so the target becomes 30 + 2 frames = 70 ms.
    fn spurt_with_a_spike() -> (JitterBuffer, Instant) {
        let t0 = Instant::now();
        let mut jb = buffer();
        for i in 0..6u16 {
            jb.push(pkt(i, i as u32 * 960, false), t0 + ms(20 * i as u64));
            assert!(matches!(
                jb.pop(t0 + ms(20 * i as u64 + 40)),
                Pop::Packet(_)
            ));
        }
        jb.push(pkt(6, 6 * 960, false), t0 + ms(150));
        assert_eq!(jb.target(), ms(70), "spike 30 ms + 2 frames");
        (jb, t0)
    }

    /// A stretch that is due waits for a quiet frame unless a concealment is
    /// running already, and goes ahead in speech after `QUIET_PATIENCE`.
    #[test]
    fn stretch_waits_for_a_quiet_frame_or_loses_patience() {
        let (mut jb, t0) = spurt_with_a_spike();
        // Loud frames: the stretch is held back, packets play on time.
        let mut t = t0 + ms(160);
        let mut seq = 6u16;
        for _ in 0..QUIET_PATIENCE {
            assert!(matches!(jb.pop_quiet(t, false), Pop::Packet(ref p) if p.seq == seq));
            seq += 1;
            t += ms(20);
            jb.push(pkt(seq, seq as u32 * 960, false), t - ms(40));
        }
        assert_eq!(jb.stats().stretched, 0);
        // A quiet frame: now, and on until the depth reaches the target.
        assert!(matches!(jb.pop_quiet(t, true), Pop::Lost { .. }));
        t += ms(20);
        assert!(matches!(jb.pop_quiet(t, false), Pop::Lost { .. }));
        t += ms(20);
        assert!(matches!(jb.pop_quiet(t, false), Pop::Packet(ref p) if p.seq == seq));
        assert_eq!(jb.stats().stretched, 2);
        assert!(jb.last_delay().unwrap() >= ms(70), "{:?}", jb.last_delay());

        // The same spike on a speaker who never pauses: patience runs out.
        let (mut jb, t0) = spurt_with_a_spike();
        let mut t = t0 + ms(160);
        let mut seq = 6u16;
        let mut stretched_at = None;
        for i in 0..2 * QUIET_PATIENCE {
            if matches!(jb.pop_quiet(t, false), Pop::Lost { .. }) {
                stretched_at = Some(i);
                break;
            }
            seq += 1;
            t += ms(20);
            jb.push(pkt(seq, seq as u32 * 960, false), t - ms(40));
        }
        assert_eq!(stretched_at, Some(QUIET_PATIENCE));
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
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(_)));
        assert!(matches!(jb.pop(t0 + ms(60)), Pop::Packet(_)));
        // An 80 ms stall, then the backlog arrives at once.
        for i in 0..4u64 {
            assert!(matches!(jb.pop(t0 + ms(80 + 20 * i)), Pop::Lost { .. }));
        }
        for i in 2..8u16 {
            jb.push(pkt(i, i as u32 * 960, false), t0 + ms(140));
        }
        assert_eq!(jb.target(), ms(60), "clamped");
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

    /// A `ts` that jumps hours ahead mid-spurt (seq gap, so it is not a
    /// pause) predicts an arrival far in the future; the packet is then
    /// "early" by hours. That must neither panic (`Instant` below its
    /// epoch) nor move the anchor or the target.
    #[test]
    fn ts_far_ahead_does_not_panic_or_move_the_anchor() {
        let t0 = Instant::now();
        let mut jb = buffer();
        jb.push(pkt(0, 0, false), t0);
        jb.push(pkt(1, 960, false), t0 + ms(20));
        assert!(matches!(jb.pop(t0 + ms(40)), Pop::Packet(_)));
        let target = jb.target();
        jb.push(pkt(3, 1 << 30, false), t0 + ms(60));
        jb.push(pkt(4, 4 * 960, false), t0 + ms(80));
        assert_eq!(jb.target(), target);
        // The stream goes on; the next punctual packet reads as delay 0.
        let _ = jb.pop(t0 + ms(80));
        let _ = jb.pop(t0 + ms(100));
        assert!(jb.stats().played >= 1);
    }

    /// Many short talk spurts over a link with occasional delay spikes:
    /// the target must not depend on how late the *first* packet of a
    /// spurt happened to arrive.
    #[test]
    fn spurt_target_does_not_depend_on_the_anchor_packet() {
        use crate::dsp::testsignal::Rng;
        let t0 = Instant::now();
        let mut jb = buffer();
        let mut rng = Rng::new(0x5EED);
        let u = |r: &mut Rng| (r.uniform() + 1.0) / 2.0;
        // (arrival ms, packet)
        let mut events: Vec<(u64, Packet)> = Vec::new();
        let spurts = 30u64;
        let per_spurt = 100u64;
        let gap = 1000u64;
        let mut seq = 0u16;
        let mut spikes = vec![0u32; spurts as usize];
        for s in 0..spurts {
            let start = s * (per_spurt * 20 + gap);
            for i in 0..per_spurt {
                let sent = start + i * 20;
                let delay = if u(&mut rng) < 0.1 {
                    spikes[s as usize] += 1;
                    60.0 + 60.0 * u(&mut rng)
                } else {
                    10.0 * u(&mut rng)
                };
                let ts = ((s * (per_spurt + gap / 20) + i) * 960) as u32;
                events.push((sent + delay as u64, pkt(seq, ts, i == per_spurt - 1)));
                seq = seq.wrapping_add(1);
            }
        }
        events.sort_by_key(|(t, _)| *t);
        let end = events.last().unwrap().0 + 500;
        let mut ei = 0;
        let mut t = 10u64;
        let mut per_spurt_underruns = Vec::new();
        let mut per_spurt_targets = Vec::new();
        let mut last = jb.stats();
        let mut spurt_seen = 0u64;
        while t < end {
            while ei < events.len() && events[ei].0 <= t {
                jb.push(events[ei].1.clone(), t0 + ms(events[ei].0));
                ei += 1;
            }
            let _ = jb.pop(t0 + ms(t));
            let st = jb.stats();
            if st.spurts > spurt_seen {
                spurt_seen = st.spurts;
                per_spurt_underruns.push(st.underruns - last.underruns);
                per_spurt_targets.push(jb.target());
                last = st;
            }
            t += 20;
        }
        per_spurt_underruns.push(jb.stats().underruns - last.underruns);
        eprintln!("spikes per spurt: {spikes:?}");
        eprintln!("underruns per spurt: {per_spurt_underruns:?}");
        eprintln!("target at spurt start: {per_spurt_targets:?}");
        let underruns: u64 = per_spurt_underruns[5..].iter().sum();
        let min_target = per_spurt_targets[5..].iter().min().unwrap();
        let st = jb.stats();
        eprintln!("underruns after warm-up: {underruns}, min target {min_target:?}, {st:?}");
        // 10 % of the packets are delayed by 60–120 ms. The 95 % quantile
        // sits in the middle of that range; the spike term covers the top
        // of it, so after warm-up nothing is written off. Before the anchor
        // followed the earliest arrival and the histogram remembered ~60
        // packets, the target fell to 25–40 ms at a third of the spurt
        // starts, with 16 underruns and 4 % of the packets lost after
        // warm-up.
        assert!(*min_target >= ms(80), "target collapsed to {min_target:?}");
        assert!(underruns <= 3, "{underruns} underruns after warm-up");
        assert!(
            st.lost <= st.played / 50,
            "more than 2 % of the packets written off: {st:?}"
        );
    }

    /// A remembered spike expires after 20–40 s without another, and the
    /// target falls back to the jitter term at the next spurt boundary.
    #[test]
    fn spike_memory_expires() {
        let t0 = Instant::now();
        let mut jb = buffer();
        // Spurt 1 with one 60 ms spike.
        let mut t = t0;
        for i in 0..20u16 {
            let late = if i == 5 { 60 } else { 0 };
            jb.push(pkt(i, i as u32 * 960, i == 19), t + ms(late));
            let _ = jb.pop(t + ms(40));
            t += ms(20);
        }
        while jb.active() {
            let _ = jb.pop(t);
            t += ms(20);
        }
        assert_eq!(jb.target(), ms(100), "spike 60 + 2 frames");
        // Spurts 2 and 3, punctual, 25 s apart: the spike is remembered
        // through the first, forgotten by the second.
        let mut seq = 20u16;
        let mut ts = 20 * 960 + 48_000;
        let mut targets = Vec::new();
        for spurt in 0..2 {
            t += ms(25_000);
            for i in 0..50u16 {
                jb.push(pkt(seq, ts, i == 49), t);
                let _ = jb.pop(t + ms(100));
                seq += 1;
                ts += 960;
                t += ms(20);
            }
            while jb.active() {
                let _ = jb.pop(t);
                t += ms(20);
            }
            targets.push(jb.target());
            ts += 48_000;
            let _ = spurt;
        }
        assert_eq!(targets[0], ms(100), "still remembered after 25 s");
        assert_eq!(targets[1], ms(25), "forgotten after 50 s: q95 + 1 frame");
    }

    // ---------------------------------------------------------------------
    // Link simulations: the mixer's pop cadence (one 480-sample block per
    // 10 ms; a frame covers two blocks, silence one) against a delay model.
    // ---------------------------------------------------------------------

    /// Drives `jb` with `arrivals` (ms, packet), sorted, popping like the
    /// mixer, and returns the stats at `at_ms` (before) and at the end.
    fn play(
        jb: &mut JitterBuffer,
        arrivals: &[(u64, Packet)],
        at_ms: u64,
        end_ms: u64,
        quiet: impl Fn(u16) -> bool,
    ) -> (JitterStats, JitterStats, Duration) {
        let t0 = Instant::now();
        let mut ai = 0;
        let mut pending = 0usize;
        let mut before = None;
        let mut max_delay = Duration::ZERO;
        let mut t = 3u64;
        while t < end_ms {
            if before.is_none() && t >= at_ms {
                before = Some(jb.stats());
            }
            while ai < arrivals.len() && arrivals[ai].0 <= t {
                jb.push(arrivals[ai].1.clone(), t0 + ms(arrivals[ai].0));
                ai += 1;
            }
            while pending < 480 {
                let seq = jb.next.map_or(0, |n| (n & 0xFFFF) as u16);
                match jb.pop_quiet(t0 + ms(t), quiet(seq)) {
                    Pop::Silence => pending += 480,
                    Pop::Packet(_) => {
                        pending += 960;
                        if before.is_some() {
                            max_delay = max_delay.max(jb.last_delay().unwrap_or_default());
                        }
                    }
                    Pop::Lost { .. } => pending += 960,
                }
            }
            pending -= 480;
            t += 10;
        }
        (before.unwrap_or_default(), jb.stats(), max_delay)
    }

    /// The link of `tests/playout.rs::playout_survives_network_jitter`:
    /// up to `spread` ms of jitter per packet and, with `chance` per
    /// packet, a stall of `stall` ms that holds everything back and then
    /// releases it in one burst (`tests/common/mod.rs::JitterProxy`).
    fn wifi_arrivals(
        seed: u64,
        packets: u64,
        spread: f32,
        chance: f32,
        stall: (f32, f32),
    ) -> Vec<(u64, Packet)> {
        use crate::dsp::testsignal::Rng;
        let mut rng = Rng::new(seed);
        let u = |r: &mut Rng| (r.uniform() + 1.0) / 2.0;
        let mut hold = 0.0f32;
        let mut out = Vec::new();
        for i in 0..packets {
            let sent = 20.0 * i as f32;
            if u(&mut rng) < chance && hold <= sent {
                hold = sent + stall.0 + (stall.1 - stall.0) * u(&mut rng);
            }
            let own = sent + spread * u(&mut rng);
            let at = own.max(hold);
            out.push((at as u64, pkt(i as u16, i as u32 * 960, false)));
        }
        out.sort_by_key(|(t, _)| *t);
        out
    }

    /// The test tone (`testsignal::harmonic_voice`) is quiet for 60 % of
    /// every 400 ms; packet `seq` carries frame `seq`.
    fn tone_quiet(seq: u16) -> bool {
        let f = (seq as f64 * 20.0 % 400.0) / 400.0;
        !(0.055..0.445).contains(&f)
    }

    /// Replica of the failing run of `playout_survives_network_jitter`
    /// (seed 7, traced 2026-09-26): stalls delaying the first held packet
    /// by 53 ms at seq 9, 53 at 35, 55 at 55, then 43 at 163, 53 at 173, 50
    /// at 194, 69 at 273, 55 at 321, 46 at 353, 31 at 396, 63 at 421. The
    /// recording window opens 1.1 s after the first packet and lasts 6 s;
    /// inside it nothing may be concealed, stretched or skipped. Before the
    /// fix the first stall grew the depth to ~65 ms by two underruns, the
    /// 75 ms target was never applied, and the 69 ms stall at seq 273 sat
    /// exactly at the margin.
    #[test]
    fn traced_wifi_run_is_clean_after_the_first_second() {
        let stalls: &[(u16, u64)] = &[
            (9, 53),
            (35, 53),
            (55, 55),
            (163, 43),
            (173, 53),
            (194, 50),
            (273, 69),
            (321, 55),
            (353, 46),
            (396, 31),
            (421, 63),
        ];
        // A stall's burst comes out of the proxy's timers in any order; the
        // tallest packet last is the adversarial one (seq 11 before 10 in
        // the traced run), so every burst is released in reverse.
        let mut arrivals = Vec::new();
        let mut hold_until = 0u64;
        let mut burst_first = 0u64;
        for i in 0..450u16 {
            let sent = 20 * i as u64;
            if let Some((_, s)) = stalls.iter().find(|(n, _)| *n == i) {
                hold_until = sent + s;
                burst_first = i as u64;
            }
            let own = sent + (i as u64 * 7) % 15;
            let at = if own < hold_until {
                hold_until - (i as u64 - burst_first)
            } else {
                own
            };
            arrivals.push((at, pkt(i, i as u32 * 960, false)));
        }
        arrivals.sort_by_key(|(t, _)| *t);
        let mut jb = buffer();
        let (before, after, max_delay) = play(&mut jb, &arrivals, 1100, 8000, tone_quiet);
        eprintln!(
            "settle: {before:?}\nwindow: {after:?}\nmax delay {max_delay:?} target {:?}",
            jb.target()
        );
        assert!(before.underruns >= 1, "the first stall costs a concealment");
        assert!(before.stretched >= 1, "and the depth follows the spike");
        assert_eq!(
            (
                after.underruns - before.underruns,
                after.stretched - before.stretched,
                after.skipped - before.skipped,
                after.lost - before.lost,
            ),
            (0, 0, 0, 0),
            "adjustments inside the window: {after:?}"
        );
        assert!(
            jb.target() >= ms(90) && jb.target() <= ms(120),
            "{:?}",
            jb.target()
        );
        assert!(max_delay <= jb.target() + ms(25), "{max_delay:?}");
    }

    /// The same link, random seeds, 40 s each: how often does the steady
    /// state (after 10 s) still get interrupted? Stalls up to 70 ms recur
    /// every 0.7 s; the spike term keeps the depth above the tallest one.
    #[test]
    fn wifi_profile_steady_state_is_almost_never_interrupted() {
        let seeds = 60u64;
        let mut underruns = 0u64;
        let mut stretched = 0u64;
        let mut skipped = 0u64;
        let mut targets = Vec::new();
        for seed in 0..seeds {
            let arrivals = wifi_arrivals(100 + seed, 2000, 15.0, 0.03, (30.0, 70.0));
            let mut jb = buffer();
            let (before, after, _) = play(&mut jb, &arrivals, 10_000, 40_000, tone_quiet);
            underruns += after.underruns - before.underruns;
            stretched += after.stretched - before.stretched;
            skipped += after.skipped - before.skipped;
            targets.push(jb.target());
        }
        targets.sort();
        let minutes = seeds as f64 * 0.5;
        eprintln!(
            "per minute: underruns {:.2} stretched {:.2} skipped {:.2}; target p50 {:?} max {:?}",
            underruns as f64 / minutes,
            stretched as f64 / minutes,
            skipped as f64 / minutes,
            targets[targets.len() / 2],
            targets.last().unwrap()
        );
        assert!(
            (underruns as f64) / minutes <= 0.5,
            "{underruns} underruns in {minutes} min"
        );
        assert!(
            (stretched + skipped) as f64 / minutes <= 1.0,
            "{stretched} stretches, {skipped} skips in {minutes} min"
        );
        assert!(*targets.last().unwrap() <= ms(130), "{targets:?}");
    }

    /// A LAN: 0–3 ms of jitter, no stalls. The target stays at the jitter
    /// term (25 ms), nothing is ever adjusted, and packets wait no more
    /// than the target plus a frame.
    #[test]
    fn lan_stays_shallow_and_untouched() {
        for seed in 0..5u64 {
            let arrivals = wifi_arrivals(seed, 1500, 3.0, 0.0, (0.0, 0.0));
            let mut jb = buffer();
            let (before, after, max_delay) = play(&mut jb, &arrivals, 2000, 30_000, |_| true);
            let st = after;
            assert_eq!(
                (
                    st.underruns - before.underruns,
                    st.stretched,
                    st.skipped,
                    st.lost
                ),
                (0, 0, 0, 0),
                "{st:?}"
            );
            assert!(jb.target() <= ms(40), "{:?}", jb.target());
            assert!(max_delay <= ms(45), "{max_delay:?}");
        }
    }
}
