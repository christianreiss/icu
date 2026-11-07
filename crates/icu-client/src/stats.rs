//! Counters and latency histograms for `--debug-stats` and the tests.

use std::{
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};

/// Bucket edges in milliseconds; the last bucket is open ended.
const EDGES_MS: [f64; 24] = [
    0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 7.5, 10.0, 12.5, 15.0, 20.0, 25.0, 30.0, 40.0, 50.0, 60.0,
    80.0, 100.0, 150.0, 200.0, 300.0, 500.0, 1000.0,
];

/// A fixed bucket histogram of durations.
#[derive(Debug, Clone)]
pub struct Histogram {
    counts: [u64; EDGES_MS.len() + 1],
    sum_ms: f64,
    max_ms: f64,
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram {
            counts: [0; EDGES_MS.len() + 1],
            sum_ms: 0.0,
            max_ms: 0.0,
        }
    }
}

impl Histogram {
    /// Records one sample.
    pub fn record(&mut self, d: Duration) {
        let ms = d.as_secs_f64() * 1000.0;
        let idx = EDGES_MS
            .iter()
            .position(|e| ms < *e)
            .unwrap_or(EDGES_MS.len());
        self.counts[idx] += 1;
        self.sum_ms += ms;
        self.max_ms = self.max_ms.max(ms);
    }

    /// Number of samples.
    pub fn count(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// Upper bucket edge holding the `q` quantile (0–1), in ms.
    pub fn quantile_ms(&self, q: f64) -> f64 {
        let total = self.count();
        if total == 0 {
            return 0.0;
        }
        let target = (total as f64 * q).ceil() as u64;
        let mut acc = 0;
        for (i, c) in self.counts.iter().enumerate() {
            acc += c;
            if acc >= target {
                return EDGES_MS.get(i).copied().unwrap_or(f64::INFINITY);
            }
        }
        f64::INFINITY
    }

    /// Mean in ms.
    pub fn mean_ms(&self) -> f64 {
        let n = self.count();
        if n == 0 {
            0.0
        } else {
            self.sum_ms / n as f64
        }
    }

    /// Maximum in ms.
    pub fn max_ms(&self) -> f64 {
        self.max_ms
    }

    /// `p50 / p95`, the way the windows show a latency histogram.
    pub fn p50_p95(&self) -> String {
        format!(
            "{} / {}",
            fmt_ms(self.quantile_ms(0.5)),
            fmt_ms(self.quantile_ms(0.95))
        )
    }

    /// One line summary.
    pub fn summary(&self) -> String {
        format!(
            "n={} p50<{:.1} p95<{:.1} p99<{:.1} mean={:.2} max={:.2} ms",
            self.count(),
            self.quantile_ms(0.5),
            self.quantile_ms(0.95),
            self.quantile_ms(0.99),
            self.mean_ms(),
            self.max_ms
        )
    }
}

/// A histogram figure in ms for the windows; the open-ended last bucket
/// reads `>1000 ms`.
pub fn fmt_ms(v: f64) -> String {
    if v.is_infinite() {
        ">1000 ms".to_string()
    } else {
        format!("{v:.1} ms")
    }
}

/// Everything the engine counts. Cheap to share; read by the UI and tests.
#[derive(Debug, Default)]
pub struct DebugStats {
    /// Current timing confidence: one means callback/arrival estimate.
    pub audio_timing_estimated: AtomicU64,
    pub audio_playout_timing_estimated: AtomicU64,
    pub video_timing_estimated: AtomicU64,
    /// Automatic processing, including budget bypasses.
    pub video_processing: Mutex<Histogram>,
    pub video_processing_bypassed: AtomicU64,
    pub video_late_dropped: AtomicU64,
    pub video_queue_age: Mutex<Histogram>,
    pub av_offset_us: AtomicI64,

    /// Audio datagrams sent.
    pub datagrams_sent: AtomicU64,
    /// Audio datagrams received (any speaker).
    pub datagrams_received: AtomicU64,
    /// Wire bytes of audio sent — header plus Opus payload, before QUIC's
    /// own framing. Counted separately from `udp_tx` so the Verbindung
    /// window can tell audio, video and protocol overhead apart.
    pub audio_bytes_sent: AtomicU64,
    /// Wire bytes of audio received.
    pub audio_bytes_received: AtomicU64,
    /// Datagrams that failed to parse.
    pub datagrams_bad: AtomicU64,
    /// Opus frames decoded from real packets.
    pub frames_decoded: AtomicU64,
    /// Frames recovered from FEC.
    pub frames_fec: AtomicU64,
    /// FEC decode attempts, including packets without usable redundancy.
    pub frames_fec_attempted: AtomicU64,
    /// Samples recovered from actual redundancy (excludes any PLC prefix).
    pub frames_fec_samples: AtomicU64,
    /// FEC attempts that produced PLC only.
    pub frames_fec_fallback: AtomicU64,
    /// Frames concealed by PLC.
    pub frames_plc: AtomicU64,
    /// Playback callbacks that found the ring partly empty (audible gap).
    pub playback_underruns: AtomicU64,
    /// Packets that arrived after their playout slot had passed.
    pub jitter_late: AtomicU64,
    /// Frames concealed while a jitter buffer waited for a late packet.
    pub jitter_underruns: AtomicU64,
    /// Frames concealed on purpose to deepen a jitter buffer inside a spurt.
    pub jitter_stretched: AtomicU64,
    /// Packets a jitter buffer wrote off as lost: a later packet was there,
    /// the one for the slot was not.
    pub jitter_lost: AtomicU64,
    /// Packets skipped to shed delay grown by underruns.
    pub jitter_skipped: AtomicU64,
    /// Packets dropped to drain a backlog after a network stall.
    pub jitter_dropped: AtomicU64,
    /// Samples handed to the playback ring.
    pub samples_played: AtomicU64,
    /// Capture blocks processed by the DSP thread.
    pub dsp_blocks: AtomicU64,
    /// Reference (AEC) underruns.
    pub reference_underruns: AtomicU64,
    /// Capture ring overruns (DSP thread too slow), including blocks lost
    /// while a stream reopen held the ring.
    pub capture_overruns: AtomicU64,
    /// Reference ring overruns, counted the same way.
    pub reference_overruns: AtomicU64,
    /// Capture samples skipped because the DSP thread fell more than 100 ms
    /// behind (`engine::MAX_CAPTURE_BACKLOG`).
    pub capture_backlog_dropped: AtomicU64,
    /// Video frames dropped because the decode thread was behind.
    pub video_decode_dropped: AtomicU64,
    /// `PACKET_LOSS_PERC` currently set on the encoder.
    pub packet_loss_perc: AtomicI64,
    /// Last RTT in microseconds, −1 if unknown.
    pub rtt_us: AtomicI64,
    /// Reconnects performed.
    pub reconnects: AtomicU64,
    /// Video frames sent.
    pub video_sent: AtomicU64,
    /// Video frames received.
    pub video_received: AtomicU64,
    /// Wire bytes of video sent — header plus Annex-B frame.
    pub video_bytes_sent: AtomicU64,
    /// Wire bytes of video received.
    pub video_bytes_received: AtomicU64,
    /// IDR frames produced on request.
    pub keyframes_forced: AtomicU64,
    /// Current outgoing video quality tier (0 = full quality, 4 = paused),
    /// as set by `crate::quality::QualityController`.
    pub video_tier: AtomicI64,
    /// Number of times `video_tier` has changed.
    pub video_tier_changes: AtomicU64,
    /// `video::Sender::send`'s 50 ms `open_uni` timeouts — the quality
    /// controller's local, immediate congestion signal.
    pub video_open_uni_drops: AtomicU64,
    /// Time a capture block spends in the DSP thread.
    pub dsp_block: Mutex<Histogram>,
    /// Time from datagram arrival to playout pop.
    pub jitter_delay: Mutex<Histogram>,
    /// Capture callback to datagram send.
    pub capture_to_send: Mutex<Histogram>,
}

impl DebugStats {
    /// Increments a counter.
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds to a counter.
    pub fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    /// Reads a counter.
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// Multi-line report for stderr.
    pub fn report(&self) -> String {
        let rtt = self.rtt_us.load(Ordering::Relaxed);
        let rtt = if rtt < 0 {
            "n/a".to_string()
        } else {
            format!("{:.2} ms", rtt as f64 / 1000.0)
        };
        // Each summary takes its lock only for itself, not for the whole
        // report: the audio threads record into these histograms.
        let dsp_block = self.dsp_block.lock().unwrap().summary();
        let jitter_delay = self.jitter_delay.lock().unwrap().summary();
        let capture_to_send = self.capture_to_send.lock().unwrap().summary();
        format!(
            "icu stats: sent={} recv={} bad={} decoded={} fec={} fec_attempted={} fec_samples={} fec_fallback={} plc={} played={} play_underruns={} jb_late={} jb_underruns={} jb_stretched={} jb_skipped={} jb_dropped={} jb_lost={} dsp_blocks={} ref_underruns={} cap_overruns={} ref_overruns={} cap_backlog={} loss_perc={} rtt={} reconnects={} video_tx={} video_rx={} idr_forced={} video_tier={} video_tier_changes={} video_open_uni_drops={}\n  dsp block      {}\n  jitter delay   {}\n  capture→send   {}\n  video processing {}\n  video queue      {}\n  video bypass={} late={} estimated_capture={} estimated_camera={} estimated_playout={} av_decode_offset_us={}",
            Self::get(&self.datagrams_sent),
            Self::get(&self.datagrams_received),
            Self::get(&self.datagrams_bad),
            Self::get(&self.frames_decoded),
            Self::get(&self.frames_fec),
            Self::get(&self.frames_fec_attempted),
            Self::get(&self.frames_fec_samples),
            Self::get(&self.frames_fec_fallback),
            Self::get(&self.frames_plc),
            Self::get(&self.samples_played),
            Self::get(&self.playback_underruns),
            Self::get(&self.jitter_late),
            Self::get(&self.jitter_underruns),
            Self::get(&self.jitter_stretched),
            Self::get(&self.jitter_skipped),
            Self::get(&self.jitter_dropped),
            Self::get(&self.jitter_lost),
            Self::get(&self.dsp_blocks),
            Self::get(&self.reference_underruns),
            Self::get(&self.capture_overruns),
            Self::get(&self.reference_overruns),
            Self::get(&self.capture_backlog_dropped),
            self.packet_loss_perc.load(Ordering::Relaxed),
            rtt,
            Self::get(&self.reconnects),
            Self::get(&self.video_sent),
            Self::get(&self.video_received),
            Self::get(&self.keyframes_forced),
            self.video_tier.load(Ordering::Relaxed),
            Self::get(&self.video_tier_changes),
            Self::get(&self.video_open_uni_drops),
            dsp_block,
            jitter_delay,
            capture_to_send,
            self.video_processing.lock().unwrap().summary(),
            self.video_queue_age.lock().unwrap().summary(),
            Self::get(&self.video_processing_bypassed),
            Self::get(&self.video_late_dropped),
            Self::get(&self.audio_timing_estimated),
            Self::get(&self.video_timing_estimated),
            Self::get(&self.audio_playout_timing_estimated),
            self.av_offset_us.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_quantiles() {
        let mut h = Histogram::default();
        for i in 0..100 {
            h.record(Duration::from_micros(100 * i));
        }
        assert_eq!(h.count(), 100);
        assert!(h.quantile_ms(0.5) <= 7.5);
        assert!(h.quantile_ms(0.99) <= 10.0);
        assert!(h.max_ms() > 9.8);
        assert!(h.summary().contains("n=100"));
    }
}
