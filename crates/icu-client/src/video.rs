//! Video frames over unidirectional streams (`docs/reference/protocol.md` §4): send,
//! receive, decode per speaker, `KeyframeRequest` bookkeeping.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use icu_proto::{
    header::{VIDEO_CODEC_H264, VIDEO_HEADER_LEN, VIDEO_MAX_FRAME},
    limits as pl, Session, VideoHeader,
};

#[cfg(feature = "camera")]
pub mod enhance;
#[cfg_attr(not(feature = "camera"), allow(dead_code))]
mod h264;

/// Minimum spacing between two `KeyframeRequest`s for the same speaker. The
/// server forwards at most [`pl::KEYFRAME_REQUESTS_PER_S`] per second and
/// speaker, so asking faster only burns control budget.
///
/// A tenth longer than the server's own interval: a repeat sent exactly on
/// time arrives a few milliseconds early as often as late, and the server
/// drops an early one without a word — the next try was a second later.
const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(1100)
    .checked_div(pl::KEYFRAME_REQUESTS_PER_S)
    .unwrap();

/// How long a decoded picture is still worth showing. Past this the sender
/// stopped or its stream stalled, and a frozen still is worse than none — the
/// overlay drops the tile, the participant list falls back to the initials.
const FRESH: Duration = Duration::from_secs(3);

/// Longest a decoded picture is held back for its sound. Sound that plays
/// later than this after the picture was decoded — a Bluetooth headset is
/// 150–450 ms behind — is followed up to here and no further: a wrong
/// audio timing can then delay the picture by this much and never hide it.
const SYNC_MAX_HOLD: Duration = Duration::from_millis(600);

/// How far a picture may run ahead of its sound and still be shown at once.
/// Lips up to about 45 ms early are not noticed (ITU-R BT.1359), so on
/// wired outputs nothing is held at all.
const SYNC_LEAD: Duration = Duration::from_millis(40);

/// Pictures whose measured lead the hold is the median of: a second's worth.
const SYNC_WINDOW: usize = 15;

/// Measurements before the hold follows them.
const SYNC_MIN: usize = 5;

/// How much the hold changes per picture: at 15 fps the picture runs 12 %
/// slow or fast while it adjusts, and 400 ms are reached in 3.3 s.
const SYNC_SLEW: Duration = Duration::from_millis(8);

/// Decoded pictures kept per speaker. [`SYNC_MAX_HOLD`] holds 9 at 15 fps;
/// one that has to make room unshown is shown, not dropped.
const PICTURES: usize = 16;

/// How long a speaker's pictures wait so that they appear with their sound
/// instead of ahead of it.
///
/// Sound takes longer than pictures: jitter buffer, mixer and the output
/// device, which on Bluetooth alone is several hundred milliseconds. The
/// mixer reports when a moment of the speaker's capture time is heard
/// (`crate::media::Playout`); a picture decoded before its moment waits for
/// it. Sound is never delayed, and a picture that is late is shown at once.
///
/// 0.1.27 gated each picture on that report and hid the video when the
/// report said −421 ms. Here the report only moves one number, the hold:
/// the median of recent measurements, changed a few milliseconds per
/// picture, at most [`SYNC_MAX_HOLD`]. Every decoded picture is shown, in
/// order, at the latest that long after its decode.
#[derive(Default)]
struct Hold {
    /// µs after their decode that the sound of the last pictures played;
    /// negative when it had played already.
    leads: VecDeque<i64>,
    delay: Duration,
    last_due: Option<Instant>,
}

impl Hold {
    /// When the picture decoded at `at` is to be shown. `lead` is its own
    /// measurement, if the sound's timing is known; `hint` what another
    /// speaker's pictures are held for — the output device is the same —
    /// for as long as this one has no measurements of its own.
    fn due(&mut self, at: Instant, lead: Option<i64>, hint: Option<Duration>) -> Instant {
        if let Some(lead) = lead {
            if self.leads.len() == SYNC_WINDOW {
                self.leads.pop_front();
            }
            self.leads.push_back(lead);
        }
        let wanted = match self.measured() {
            Some(wanted) => Some(wanted),
            None if self.leads.is_empty() => hint,
            None => None,
        };
        if let Some(wanted) = wanted {
            self.delay = if wanted > self.delay {
                (self.delay + SYNC_SLEW).min(wanted)
            } else {
                self.delay.saturating_sub(SYNC_SLEW).max(wanted)
            };
        }
        // Never before its predecessor: the pictures stay in order.
        let due = (at + self.delay).max(self.last_due.unwrap_or(at));
        self.last_due = Some(due);
        due
    }

    /// The hold the measurements ask for, once there are enough of them.
    fn measured(&self) -> Option<Duration> {
        if self.leads.len() < SYNC_MIN {
            return None;
        }
        let mut sorted: Vec<i64> = self.leads.iter().copied().collect();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        Some(
            Duration::from_micros(median.max(0) as u64)
                .saturating_sub(SYNC_LEAD)
                .min(SYNC_MAX_HOLD),
        )
    }

    /// The sound's timing is no longer valid (another device, a gap): what
    /// was measured against it is forgotten, the hold itself stays until
    /// new measurements move it.
    fn forget_sound(&mut self) {
        self.leads.clear();
    }
}

/// A quality tier for the outgoing camera stream, stepped by
/// `crate::quality::QualityController` in response to measured network
/// conditions (`docs/reference/client.md` §8). Video absorbs all of the
/// adaptation — audio's Opus bitrate is never touched by this system.
/// Defined here (not in `camera`, which is feature-gated) so `engine`'s
/// `Shared` and `CameraLink` can hold and expose it unconditionally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoTier {
    /// 150 kbit/s, 15 fps — the original fixed v1 config.
    #[default]
    T0,
    /// 100 kbit/s, 7.5 fps (skip 1 of every 2 captured frames).
    T1,
    /// 60 kbit/s, 5 fps (skip 2 of every 3).
    T2,
    /// 40 kbit/s, 3 fps (skip 4 of every 5).
    T3,
    /// Paused: no frames encoded or sent. Audio is unaffected.
    T4,
}

impl VideoTier {
    /// Target encoder bitrate in bits/s. Meaningless (and unused) at `T4`.
    pub fn bitrate(self) -> u32 {
        match self {
            VideoTier::T0 => 150_000,
            VideoTier::T1 => 100_000,
            VideoTier::T2 => 60_000,
            VideoTier::T3 => 40_000,
            VideoTier::T4 => 0,
        }
    }

    /// Encode 1 of every `n` captured frames; `T4` encodes none.
    pub fn frame_divisor(self) -> u32 {
        match self {
            VideoTier::T0 => 1,
            VideoTier::T1 => 2,
            VideoTier::T2 => 3,
            VideoTier::T3 => 5,
            VideoTier::T4 => 0,
        }
    }

    /// Whether this tier sends no video at all.
    pub fn paused(self) -> bool {
        matches!(self, VideoTier::T4)
    }

    /// One step down (more degraded), saturating at `T4`.
    pub fn step_down(self) -> VideoTier {
        match self {
            VideoTier::T0 => VideoTier::T1,
            VideoTier::T1 => VideoTier::T2,
            VideoTier::T2 => VideoTier::T3,
            VideoTier::T3 | VideoTier::T4 => VideoTier::T4,
        }
    }

    /// One step up (less degraded), saturating at `T0`.
    pub fn step_up(self) -> VideoTier {
        match self {
            VideoTier::T0 => VideoTier::T0,
            VideoTier::T1 => VideoTier::T0,
            VideoTier::T2 => VideoTier::T1,
            VideoTier::T3 => VideoTier::T2,
            VideoTier::T4 => VideoTier::T3,
        }
    }

    /// Wire index for `Shared::video_tier` and the debug-stats counter.
    pub fn as_u8(self) -> u8 {
        match self {
            VideoTier::T0 => 0,
            VideoTier::T1 => 1,
            VideoTier::T2 => 2,
            VideoTier::T3 => 3,
            VideoTier::T4 => 4,
        }
    }

    /// Inverse of [`VideoTier::as_u8`]; anything unrecognized is `T0`.
    pub fn from_u8(v: u8) -> VideoTier {
        match v {
            1 => VideoTier::T1,
            2 => VideoTier::T2,
            3 => VideoTier::T3,
            4 => VideoTier::T4,
            _ => VideoTier::T0,
        }
    }
}

/// A frame as it came off the wire.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Header with the server-set speaker.
    pub header: VideoHeader,
    /// Annex-B H.264 (codec 1) or JFIF (codec 2).
    pub data: Vec<u8>,
}

/// A decoded RGBA picture. Cloned by every viewer on every repaint, so the
/// pixels are shared: a clone is a reference count, not a copy.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// RGBA8, row major.
    pub rgba: Arc<Vec<u8>>,
    /// When it was decoded.
    pub at: Instant,
    /// Original capture time; decode completion never replaces this.
    pub stamp: crate::media::Stamp,
}

/// The per-speaker H.264 decoder. Remembers when an access unit failed to
/// decode — a lost or damaged reference — so the receiver waits for the
/// next IDR and asks for one ([`Receivers::finish`]).
#[cfg(feature = "camera")]
pub struct Decoder {
    inner: openh264::decoder::Decoder,
    failed: bool,
}

#[cfg(feature = "camera")]
impl Decoder {
    /// A fresh decoder.
    pub fn new() -> Result<Decoder, String> {
        openh264::decoder::Decoder::new()
            .map(|inner| Decoder {
                inner,
                failed: false,
            })
            .map_err(|e| e.to_string())
    }

    /// Whether a decode failed since the last call.
    fn take_failed(&mut self) -> bool {
        std::mem::take(&mut self.failed)
    }
}

/// Stands in for the decoder in a build without `camera`: nothing is ever
/// decoded, the frames are only counted.
#[cfg(not(feature = "camera"))]
#[derive(Debug)]
pub struct Decoder;

#[cfg(not(feature = "camera"))]
impl Decoder {
    /// Never fails: it never decodes.
    fn take_failed(&mut self) -> bool {
        false
    }
}

/// Decodes one access unit to a picture, `None` when the decoder has
/// nothing to show yet or cannot read it (then it remembers the failure).
#[cfg(feature = "camera")]
pub fn decode(decoder: &mut Decoder, data: &[u8]) -> Option<Frame> {
    use openh264::formats::YUVSource;
    // The sender's encoder is not ours to trust: the size is checked
    // before the decoder allocates for it, and the decoder reads exactly
    // what was checked (`h264`).
    let au = match h264::sanitize(data) {
        Ok(au) => au,
        Err(e) => {
            tracing::debug!(?e, "h264 refused before decode");
            decoder.failed = true;
            return None;
        }
    };
    let yuv = match decoder.inner.decode(&au) {
        Ok(yuv) => yuv?,
        Err(e) => {
            tracing::debug!(%e, "h264 decode");
            decoder.failed = true;
            return None;
        }
    };
    let (w, h) = yuv.dimensions();
    if !picture_size_ok(w, h) {
        tracing::debug!(w, h, "h264 picture empty or above the ceiling");
        decoder.failed = true;
        return None;
    }
    let mut rgba = vec![0u8; w * h * 4];
    yuv.write_rgba8(&mut rgba);
    Some(Frame {
        width: w,
        height: h,
        rgba: Arc::new(rgba),
        at: Instant::now(),
        stamp: crate::media::Stamp::default(),
    })
}

/// Whether a decoded picture may go on screen: within the receiver
/// ceiling, and not empty — a texture of zero width or height is a wgpu
/// validation error, which panics, and the release build aborts on a panic.
/// The SPS check alone cannot promise either: cropping happens after it.
#[cfg_attr(not(feature = "camera"), allow(dead_code))]
fn picture_size_ok(w: usize, h: usize) -> bool {
    use icu_proto::header::{VIDEO_MAX_HEIGHT, VIDEO_MAX_WIDTH};
    (1..=VIDEO_MAX_WIDTH).contains(&w) && (1..=VIDEO_MAX_HEIGHT).contains(&h)
}

/// See the `camera` version; without the feature there is no picture.
#[cfg(not(feature = "camera"))]
pub fn decode(_decoder: &mut Decoder, _data: &[u8]) -> Option<Frame> {
    None
}

/// Where received frames go.
pub trait VideoSink: Send + Sync + Clone + 'static {
    /// One complete frame.
    fn deliver(&self, session: Session, frame: EncodedFrame);
    /// A frame was dropped because the decode thread is behind.
    fn overflow(&self) {}
    fn stale(&self, _session: Session) {}
    fn queued(&self, _age: Duration) {}
    fn tick(&self) {}
}

/// How far a frame's `seq` may lag behind the newest one and still count as
/// merely late: the frames' streams complete out of order by a frame or
/// two (the server keeps one per receiver in flight and one waiting). A
/// step further back is a new stream on a reused session id, not a late
/// frame of the old one.
const REORDER_DEPTH: i16 = 32;

/// Frames waiting for the decode thread. At 15 fps per speaker this is a
/// few seconds' worth; past it the decoder is hopelessly behind and dropping
/// (then waiting for the next IDR) beats growing the queue.
const DECODE_QUEUE: usize = 64;

/// Accepts uni streams for the lifetime of the connection.
///
/// The frames are decoded on one dedicated thread, `icu-video-decode`, not
/// on the tokio workers: an H.264 decode plus the RGBA conversion blocks a
/// worker for milliseconds, and the same two workers drive QUIC and stamp
/// the audio datagrams' arrival times (`docs/reference/client.md` §4). One
/// thread also keeps a speaker's frames in arrival order, which
/// `spawn_blocking` per frame would not.
pub async fn accept_loop<S: VideoSink>(conn: quinn::Connection, sink: S) {
    let queue = Arc::new((
        Mutex::new(DecodeQueue::default()),
        std::sync::Condvar::new(),
    ));
    let worker = queue.clone();
    let decode_sink = sink.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("icu-video-decode".into())
        .spawn(move || {
            let mut reorder = Reorder::default();
            let mut ready = Vec::new();
            loop {
                let mut q = worker.0.lock().unwrap();
                if q.frames.is_empty() && !q.closed {
                    q = worker
                        .1
                        .wait_timeout(q, Duration::from_millis(16))
                        .unwrap()
                        .0;
                }
                let item = q.pop();
                let closed = q.closed;
                drop(q);
                decode_sink.tick();
                let now = Instant::now();
                let last = item.is_none() && closed;
                if let Some((session, frame, at)) = item {
                    let age = now.saturating_duration_since(at);
                    decode_sink.queued(age);
                    if age > Duration::from_millis(100) {
                        decode_sink.stale(session);
                    } else {
                        reorder.push(session, frame, now, &mut ready);
                    }
                }
                reorder.release_due(now, &mut ready);
                for (session, frame) in ready.drain(..) {
                    decode_sink.deliver(session, frame);
                }
                if last {
                    return;
                }
            }
        })
    {
        tracing::warn!(error=%e,"cannot spawn video decode thread");
        return;
    }
    let reads = Arc::new(tokio::sync::Semaphore::new(32));
    loop {
        let Ok(mut recv) = conn.accept_uni().await else {
            break;
        };
        let at = Instant::now();
        let queue = queue.clone();
        let sink = sink.clone();
        let Ok(permit) = reads.clone().acquire_owned().await else {
            break;
        };
        tokio::spawn(async move {
            let _permit = permit;
            let Ok(Ok(mut data)) = tokio::time::timeout_at(
                tokio::time::Instant::from_std(at + Duration::from_millis(100)),
                recv.read_to_end(VIDEO_HEADER_LEN + VIDEO_MAX_FRAME),
            )
            .await
            else {
                return;
            };
            let Ok(header) = VideoHeader::decode(&data) else {
                return;
            };
            data.drain(..VIDEO_HEADER_LEN);
            let frame = EncodedFrame { header, data };
            let dropped = queue.0.lock().unwrap().push(header.speaker, frame, at);
            for _ in 0..dropped {
                sink.overflow();
            }
            queue.1.notify_one();
        });
    }
    queue.0.lock().unwrap().closed = true;
    queue.1.notify_one();
}

/// How long a frame that arrived ahead of its predecessor waits for it.
/// Each frame is a stream of its own, so one lost packet of frame N lets
/// frame N+1 finish first; the retransmission follows within about one
/// round trip. Less than a frame interval at 15 fps, and only frames that
/// really came out of order pay it.
const REORDER_HOLD: Duration = Duration::from_millis(60);

/// Frames waiting for a predecessor, per speaker. One more means the
/// predecessor is lost, not late.
const REORDER_HELD: usize = 3;

/// A speaker's place in its frame sequence, for [`Reorder`].
struct Order {
    /// The `seq` that continues the stream.
    next: u16,
    media_epoch: u32,
    /// Frames ahead of `next`, with when the first of them began to wait.
    held: Vec<EncodedFrame>,
    since: Option<Instant>,
    seen: Instant,
}

/// Puts each speaker's frames back into `seq` order before they are
/// decoded.
///
/// [`Receivers::accept`] decodes strictly in order: a frame ahead of its
/// predecessor is a gap (everything waits for an IDR), and the predecessor,
/// arriving a moment later, is dropped as late. The worst case is the
/// P-frame after an IDR overtaking it — the IDR is many packets, the
/// P-frame one, so a single lost packet is enough: the IDR was thrown
/// away, the picture froze until the next requested one, and that one
/// could be overtaken again. Holding the early frame for [`REORDER_HOLD`] costs nothing while
/// frames arrive in order and turns a reordering into no loss at all.
#[derive(Default)]
struct Reorder {
    speakers: HashMap<Session, Order>,
}

impl Reorder {
    /// Takes a frame as it arrived at `now`; what may be decoded now is
    /// appended to `out`, in order.
    fn push(
        &mut self,
        session: Session,
        frame: EncodedFrame,
        now: Instant,
        out: &mut Vec<(Session, EncodedFrame)>,
    ) {
        let seq = frame.header.seq;
        let Some(order) = self.speakers.get_mut(&session) else {
            let mut order = Order {
                next: seq.wrapping_add(1),
                media_epoch: frame.header.media_epoch,
                held: Vec::new(),
                since: None,
                seen: now,
            };
            if frame.header.keyframe() {
                out.push((session, frame));
            } else {
                // A stream's first frame is its IDR. A P-frame first may
                // have overtaken it (camera just switched on), so it waits
                // like any early frame; joining mid-stream it is not
                // decodable anyway and only asks for its IDR 60 ms later.
                order.next = seq.wrapping_sub(1);
                order.held.push(frame);
                order.since = Some(now);
            }
            self.speakers.insert(session, order);
            return;
        };
        order.seen = now;
        let step = seq.wrapping_sub(order.next) as i16;
        if frame.header.media_epoch != order.media_epoch || step <= -REORDER_DEPTH {
            // Another stream (the camera restarted, or a new peer on a
            // reused session id): nothing held belongs in front of it.
            order.release(session, out);
            order.media_epoch = frame.header.media_epoch;
            order.next = seq.wrapping_add(1);
            out.push((session, frame));
        } else if step < 0 {
            // Older than what was passed on already: `accept` drops it.
            out.push((session, frame));
        } else if step == 0 {
            order.next = seq.wrapping_add(1);
            out.push((session, frame));
            order.release_consecutive(session, out);
        } else if order.held.iter().any(|f| f.header.seq == seq) {
            // The same frame twice; one waits already.
        } else if frame.header.keyframe()
            || step as usize > REORDER_HELD
            || order.held.len() >= REORDER_HELD
        {
            // An IDR needs no predecessor; a frame this far ahead, or one
            // more than may wait, means the missing one is lost.
            order.held.push(frame);
            order.release(session, out);
        } else {
            order.held.push(frame);
            order.since.get_or_insert(now);
        }
    }

    /// Passes on what has waited [`REORDER_HOLD`] for a predecessor that
    /// did not come, and forgets speakers that stopped.
    fn release_due(&mut self, now: Instant, out: &mut Vec<(Session, EncodedFrame)>) {
        for (session, order) in &mut self.speakers {
            if order
                .since
                .is_some_and(|t| now.saturating_duration_since(t) >= REORDER_HOLD)
            {
                order.release(*session, out);
            }
        }
        self.speakers
            .retain(|_, order| now.saturating_duration_since(order.seen) <= FRESH);
    }
}

impl Order {
    /// Held frames in stream order.
    fn sort(&mut self) {
        let next = self.next;
        self.held
            .sort_by_key(|f| f.header.seq.wrapping_sub(next) as i16);
    }

    /// Passes on everything held; the stream continues after the newest.
    fn release(&mut self, session: Session, out: &mut Vec<(Session, EncodedFrame)>) {
        self.sort();
        for frame in self.held.drain(..) {
            self.next = frame.header.seq.wrapping_add(1);
            out.push((session, frame));
        }
        self.since = None;
    }

    /// Passes on the held frames that follow without a gap.
    fn release_consecutive(&mut self, session: Session, out: &mut Vec<(Session, EncodedFrame)>) {
        self.sort();
        let ready = self
            .held
            .iter()
            .zip(0u16..)
            .take_while(|(f, i)| f.header.seq == self.next.wrapping_add(*i))
            .count();
        for frame in self.held.drain(..ready) {
            self.next = frame.header.seq.wrapping_add(1);
            out.push((session, frame));
        }
        if self.held.is_empty() {
            self.since = None;
        }
    }
}

/// At most two waiting access units per speaker, 64 total and 8 MiB.
/// Dropping a reference is detected by the ordinary sequence/IDR machinery.
#[derive(Default)]
struct DecodeQueue {
    frames: VecDeque<(Session, EncodedFrame, Instant)>,
    bytes: usize,
    closed: bool,
}
impl DecodeQueue {
    fn pop(&mut self) -> Option<(Session, EncodedFrame, Instant)> {
        let item = self.frames.pop_front()?;
        self.bytes -= item.1.data.len();
        Some(item)
    }
    fn push(&mut self, session: Session, frame: EncodedFrame, at: Instant) -> usize {
        if self.closed {
            return 1;
        }
        let mut dropped = 0;
        while self.frames.iter().filter(|item| item.0 == session).count() >= 2 {
            let index = self
                .frames
                .iter()
                .position(|item| item.0 == session)
                .unwrap();
            self.bytes -= self.frames.remove(index).unwrap().1.data.len();
            dropped += 1;
        }
        while self.frames.len() >= DECODE_QUEUE || self.bytes + frame.data.len() > 8 * 1024 * 1024 {
            if self.pop().is_none() {
                break;
            }
            dropped += 1;
        }
        self.bytes += frame.data.len();
        self.frames.push_back((session, frame, at));
        dropped
    }
}

/// Dropping a Quinn stream normally finishes it. An incomplete or revoked
/// video frame must instead reset, including write errors and task cancellation.
struct PendingFrame {
    stream: quinn::SendStream,
    finished: bool,
}

impl Drop for PendingFrame {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.stream.reset(0u32.into());
        }
    }
}

/// Sends frames, one uni stream each. Drops the frame when flow control
/// blocks (`docs/reference/client.md` §8). Shared by reference: the camera
/// thread sends on it while the session task may be taking it down.
pub struct Sender {
    conn: quinn::Connection,
    seq: AtomicU16,
}

impl Sender {
    /// For `conn`.
    pub fn new(conn: quinn::Connection) -> Sender {
        Sender {
            conn,
            seq: AtomicU16::new(0),
        }
    }

    /// Sends one H.264 access unit.
    pub async fn send(&self, keyframe: bool, ts: u32, data: &[u8]) -> Result<(), String> {
        self.send_until(keyframe, ts, data, std::future::pending(), |send| {
            send.finish().map_err(|e| e.to_string())
        })
        .await
    }

    /// Like `send`, but resets an in-flight frame when its source is revoked.
    pub(crate) async fn send_until(
        &self,
        keyframe: bool,
        ts: u32,
        data: &[u8],
        cancelled: impl std::future::Future<Output = ()>,
        finish: impl FnOnce(&mut quinn::SendStream) -> Result<(), String>,
    ) -> Result<(), String> {
        self.send_captured_until(
            keyframe,
            ts,
            data,
            cancelled,
            finish,
            (crate::media::Stamp::default(), Instant::now(), false),
        )
        .await
    }

    pub(crate) async fn send_captured_until(
        &self,
        keyframe: bool,
        ts: u32,
        data: &[u8],
        cancelled: impl std::future::Future<Output = ()>,
        finish: impl FnOnce(&mut quinn::SendStream) -> Result<(), String>,
        captured: (crate::media::Stamp, Instant, bool),
    ) -> Result<(), String> {
        tokio::pin!(cancelled);
        // The encoder has produced this frame, so it takes its number even
        // when it is dropped below: the receivers then see the gap and wait
        // for an IDR instead of decoding the next P-frame against a
        // reference they never got.
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let deadline = tokio::time::Instant::from_std(captured.1 + Duration::from_millis(100));
        if tokio::time::Instant::now() >= deadline {
            return Err("flow control: frame dropped".into());
        }
        let header = VideoHeader {
            media_time: captured.0.micros,
            media_epoch: captured.0.epoch,
            flags: if keyframe {
                icu_proto::header::VIDEO_FLAG_KEYFRAME
            } else {
                0
            } | if captured.2 {
                0
            } else {
                icu_proto::header::MEDIA_FLAG_TIMING_ESTIMATED
            },
            speaker: 0,
            seq,
            ts,
            codec: VIDEO_CODEC_H264,
        };
        let send = tokio::select! {
            biased;
            () = &mut cancelled => return Err("camera source changed".into()),
            opened = tokio::time::timeout_at(deadline.min(tokio::time::Instant::now()+Duration::from_millis(50)), self.conn.open_uni()) => match opened {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return Err(e.to_string()),
                Err(_) => return Err("flow control: frame dropped".into()),
            },
        };
        let mut pending = PendingFrame {
            stream: send,
            finished: false,
        };
        let send = &mut pending.stream;
        let _ = send.set_priority(-1);
        // Header and body as two writes on the stream: no copy of the frame
        // into a joined buffer.
        let written = tokio::select! {
            biased;
            () = &mut cancelled => {
                let _ = send.reset(0u32.into());
                return Err("camera source changed".into());
            }
            result = tokio::time::timeout_at(deadline, async {
                send.write_all(&header.encode()).await?;
                send.write_all(data).await
            }) => match result {Ok(result)=>result,Err(_)=>return Err("flow control: frame dropped".into())},
        };
        written.map_err(|e| e.to_string())?;
        let result = finish(send);
        pending.finished = result.is_ok();
        result
    }
}

/// Per speaker decoding state.
struct Receiver {
    /// `None` while [`Receivers::accept`] has lent it to the decode thread,
    /// or when no decoder could be created.
    decoder: Option<Decoder>,
    /// Decoded pictures with the time each is due ([`Hold`]).
    pictures: VecDeque<(u64, Frame, Instant)>,
    shown: Mutex<Option<(u64, Frame)>>,
    audio: Option<crate::media::Playout>,
    /// `audio` comes from the output device's own timing, not from an
    /// estimate that knows nothing of the device's delay.
    audio_accurate: bool,
    hold: Hold,
    /// The newest due picture the windows were woken for.
    announced: u64,
    media_epoch: u32,
    /// Counts decoded pictures, mirroring `crate::camera::PreviewSlot::seq`:
    /// a viewer that already uploaded `seq` knows the pixels have not changed
    /// and can skip both the clone and the texture upload.
    seq: u64,
    /// The decoder can take the next frame: an IDR arrived and nothing was
    /// lost since. Cleared by a gap in `seq` or a failed decode; until the
    /// next IDR the frames are dropped and one is asked for.
    got_keyframe: bool,
    /// The `seq` of the newest frame seen. Anything not newer came in late
    /// (the frames' streams complete in any order) and is dropped.
    last_seq: Option<u16>,
    last_frame: Option<Instant>,
    last_request: Option<Instant>,
    frames: u64,
}

impl Receiver {
    fn new() -> Receiver {
        Receiver {
            #[cfg(feature = "camera")]
            decoder: Decoder::new().ok(),
            #[cfg(not(feature = "camera"))]
            decoder: Some(Decoder),
            pictures: VecDeque::with_capacity(PICTURES),
            shown: Mutex::new(None),
            audio: None,
            audio_accurate: false,
            hold: Hold::default(),
            announced: 0,
            media_epoch: 0,
            seq: 0,
            got_keyframe: false,
            last_seq: None,
            last_frame: None,
            last_request: None,
            frames: 0,
        }
    }

    /// Takes in a decoded picture and says whether its hold rests on
    /// measurements of this speaker's own sound.
    fn present(&mut self, picture: Frame, hint: Option<Duration>) -> bool {
        self.seq += 1;
        let lead = self
            .audio
            .filter(|_| self.audio_accurate)
            .and_then(|audio| audio.target(picture.stamp, picture.at))
            .map(|target| {
                if target >= picture.at {
                    target.duration_since(picture.at).as_micros() as i64
                } else {
                    -(picture.at.duration_since(target).as_micros() as i64)
                }
            });
        let due = self.hold.due(picture.at, lead, hint);
        if self.pictures.len() == PICTURES {
            // More pictures than the hold can ever cover (a frame rate far
            // above the profile's): the oldest goes on screen now rather
            // than unseen.
            if let Some((seq, frame, _)) = self.pictures.pop_front() {
                let mut shown = self.shown.lock().unwrap();
                if shown.as_ref().is_none_or(|(old, _)| *old < seq) {
                    *shown = Some((seq, frame));
                }
            }
        }
        self.pictures.push_back((self.seq, picture, due));
        self.hold.measured().is_some()
    }

    /// The newest picture that is due, if the windows have not been woken
    /// for it yet.
    fn take_due(&mut self, now: Instant) -> bool {
        let newest = self
            .pictures
            .iter()
            .rev()
            .find(|(_, _, due)| *due <= now)
            .map(|(seq, _, _)| *seq);
        match newest {
            Some(seq) if seq > self.announced => {
                self.announced = seq;
                true
            }
            _ => false,
        }
    }

    fn selected(&self, now: Instant) -> Option<(u64, Frame)> {
        let mut shown = self.shown.lock().unwrap();
        for (seq, frame, due) in self.pictures.iter().rev() {
            if shown.as_ref().is_some_and(|(old, _)| old >= seq) {
                break;
            }
            // Not yet: its sound has not played ([`Hold`]). The hold only
            // ever postpones a picture, by a bounded time; nothing here
            // decides whether a picture is shown at all.
            if *due > now {
                continue;
            }
            if now.saturating_duration_since(frame.at) > FRESH {
                continue;
            }
            *shown = Some((*seq, frame.clone()));
            break;
        }
        shown
            .as_ref()
            .filter(|(_, frame)| now.saturating_duration_since(frame.at) <= FRESH)
            .cloned()
    }
}

/// All speakers' receivers.
#[derive(Default)]
pub struct Receivers {
    map: HashMap<Session, Receiver>,
    /// The hold last measured for any speaker ([`Hold::due`]).
    hold_hint: Option<Duration>,
}

impl Receivers {
    /// Stores and decodes a frame in one go (tests). The engine splits it
    /// into [`Receivers::accept`], [`decode`] and [`Receivers::finish`], so
    /// the decode runs outside the lock the windows take for every repaint.
    pub fn push(&mut self, session: Session, frame: EncodedFrame) {
        if let Some(mut decoder) = self.accept(session, &frame) {
            let picture = decode(&mut decoder, &frame.data);
            self.finish(session, decoder, picture);
        }
    }

    /// Books a frame for `session` and, when it is decodable — H.264, after
    /// an IDR with nothing lost since — lends out the session's decoder.
    /// Frames are decoded in `seq` order: one older than a frame already
    /// seen is dropped, and after a gap (a frame the server or the sender's
    /// flow control dropped) the following ones refer to a picture this
    /// decoder never had, so everything up to the next IDR is dropped and a
    /// `KeyframeRequest` is queued ([`Receivers::take_keyframe_requests`]).
    /// Otherwise the picture stays corrupt until the sender's next periodic
    /// IDR, up to 2 s later.
    pub fn accept(&mut self, session: Session, frame: &EncodedFrame) -> Option<Decoder> {
        let r = self.map.entry(session).or_insert_with(Receiver::new);
        r.frames += 1;
        r.last_frame = Some(Instant::now());
        if frame.header.media_epoch != r.media_epoch {
            if r.media_epoch != 0
                && frame.header.media_epoch.wrapping_sub(r.media_epoch) as i32 <= 0
            {
                return None;
            }
            r.media_epoch = frame.header.media_epoch;
            r.pictures.clear();
            *r.shown.lock().unwrap() = None;
            r.got_keyframe = false;
            r.last_seq = None;
            r.audio = None;
            r.hold.forget_sound();
            r.hold.last_due = None;
        }
        let seq = frame.header.seq;
        if let Some(last) = r.last_seq {
            // Modulo 2^16 (`docs/reference/protocol.md` §3): a step past half the range is
            // a step back.
            let step = seq.wrapping_sub(last) as i16;
            if step <= 0 && step > -REORDER_DEPTH {
                return None;
            }
            // A gap, or a new stream (`REORDER_DEPTH`): start over at an IDR.
            if step != 1 {
                r.got_keyframe = false;
            }
        }
        r.last_seq = Some(seq);
        if frame.header.keyframe() {
            r.got_keyframe = true;
        }
        if !r.got_keyframe || frame.header.codec != VIDEO_CODEC_H264 {
            return None;
        }
        r.decoder.take()
    }

    /// Returns the decoder [`Receivers::accept`] lent out, with the picture
    /// it made of the frame. A session removed or reset in the meantime has
    /// a fresh decoder waiting for its IDR; the old one is dropped with it.
    /// A frame that failed to decode means a broken reference: wait for the
    /// next IDR and ask for it.
    pub fn finish(&mut self, session: Session, mut decoder: Decoder, picture: Option<Frame>) {
        let Some(r) = self.map.get_mut(&session) else {
            return;
        };
        if r.decoder.is_some() {
            return;
        }
        if decoder.take_failed() {
            r.got_keyframe = false;
        }
        r.decoder = Some(decoder);
        if let Some(picture) = picture {
            if r.present(picture, self.hold_hint) {
                self.hold_hint = Some(r.hold.delay);
            }
        }
    }

    /// Whether a picture became due that the windows have not been woken
    /// for: a new one that is shown at once, or one whose hold ran out.
    pub fn take_due(&mut self, now: Instant) -> bool {
        let mut any = false;
        for r in self.map.values_mut() {
            any |= r.take_due(now);
        }
        any
    }

    /// How long `session`'s pictures are currently held for their sound.
    pub fn hold(&self, session: Session) -> Option<Duration> {
        self.map.get(&session).map(|r| r.hold.delay)
    }

    /// Sessions that need an IDR (none yet, or one lost since), rate limited
    /// to one request per second and speaker (`docs/reference/protocol.md` §2.2 Nr. 13) —
    /// as long as their frames still arrive: a sender that stopped (camera
    /// off, stalled) was asked every second for the rest of the session.
    pub fn take_keyframe_requests(&mut self) -> Vec<Session> {
        self.take_keyframe_requests_at(Instant::now())
    }

    /// [`Receivers::take_keyframe_requests`] at `now`.
    fn take_keyframe_requests_at(&mut self, now: Instant) -> Vec<Session> {
        let mut out = Vec::new();
        for (session, r) in self.map.iter_mut() {
            if r.got_keyframe
                || r.last_frame
                    .is_none_or(|t| now.saturating_duration_since(t) > FRESH)
            {
                continue;
            }
            let due = r
                .last_request
                .map(|t| now.duration_since(t) >= KEYFRAME_REQUEST_MIN_INTERVAL)
                .unwrap_or(true);
            if due {
                r.last_request = Some(now);
                out.push(*session);
            }
        }
        out
    }

    /// Latest decoded frame per session; tiles older than [`FRESH`] are
    /// hidden.
    pub fn latest(&self) -> Vec<(Session, Frame)> {
        let now = Instant::now();
        self.map
            .iter()
            .filter_map(|(session, r)| r.selected(now).map(|(_, frame)| (*session, frame)))
            .collect()
    }
    /// Sequence of the picture due for presentation, shared by every surface.
    pub fn seqs(&self) -> Vec<(Session, u64)> {
        let now = Instant::now();
        self.map
            .iter()
            .filter_map(|(session, r)| r.selected(now).map(|(seq, _)| (*session, seq)))
            .collect()
    }
    pub fn picture(&self, session: Session) -> Option<(u64, Frame)> {
        self.map.get(&session)?.selected(Instant::now())
    }
    /// When a moment of `session`'s capture time is heard. `accurate`: the
    /// output device reported that time itself; an estimate is kept for the
    /// diagnostics but holds no picture.
    pub fn audio_playout(
        &mut self,
        session: Session,
        anchor: crate::media::Playout,
        accurate: bool,
    ) {
        if let Some(r) = self.map.get_mut(&session) {
            if r.media_epoch == anchor.stamp.epoch {
                r.audio = Some(anchor);
                r.audio_accurate = accurate;
            }
        }
    }
    pub fn timing_target(&self, session: Session, stamp: crate::media::Stamp) -> Option<Instant> {
        self.map.get(&session)?.audio?.target(stamp, Instant::now())
    }
    pub fn clear_audio_playout(&mut self) {
        for r in self.map.values_mut() {
            r.audio = None;
            r.hold.forget_sound();
        }
    }
    pub fn invalidate(&mut self, session: Session) {
        if let Some(r) = self.map.get_mut(&session) {
            r.got_keyframe = false;
        }
    }
    /// Frames received for a session (tests).
    pub fn frames(&self, session: Session) -> u64 {
        self.map.get(&session).map(|r| r.frames).unwrap_or(0)
    }

    /// Whether the session's video is decodable right now: an IDR arrived
    /// and nothing was lost or failed since.
    pub fn has_keyframe(&self, session: Session) -> bool {
        self.map
            .get(&session)
            .map(|r| r.got_keyframe)
            .unwrap_or(false)
    }

    /// Drops a session.
    pub fn remove(&mut self, session: Session) {
        self.map.remove(&session);
    }

    /// Drops everything (reconnect).
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

/// The participant list's live pictures: one GPU texture per session,
/// re-uploaded only when that session's `seq` moved. Like
/// [`crate::camera::CameraPreview`] it owns the texture, so it belongs to the
/// window that draws rather than to the engine, which outlives any window.
#[cfg(feature = "gui")]
pub struct VideoTextures {
    frames: crate::engine::VideoFrames,
    /// Texture plus the `seq` already uploaded into it, per session.
    shown: HashMap<Session, (egui::TextureHandle, u64)>,
}

#[cfg(feature = "gui")]
impl VideoTextures {
    /// Reads from [`crate::engine::Engine::video_frames_source`].
    pub fn new(frames: crate::engine::VideoFrames) -> VideoTextures {
        VideoTextures {
            frames,
            shown: HashMap::new(),
        }
    }

    /// Uploads what is new and forgets whoever stopped sending. Call once per
    /// frame, before the rows are drawn. A session that left, reconnected or
    /// simply went quiet for [`FRESH`] drops out of `seqs`, its texture is
    /// released here, and its row falls back to the initials circle — which
    /// is also why the eviction runs before the uploads and not after.
    pub fn refresh(&mut self, ctx: &egui::Context) -> Vec<Session> {
        let mut fresh = Vec::new();
        let seqs = self.frames.seqs();
        self.shown
            .retain(|session, _| seqs.iter().any(|(s, _)| s == session));
        for (session, seq) in seqs {
            if self.shown.get(&session).is_some_and(|(_, up)| *up == seq) {
                continue;
            }
            let Some((seq, frame)) = self.frames.picture(session) else {
                continue;
            };
            fresh.push(session);
            // The decoder writes alpha 255 for every pixel, so premultiplied
            // is both correct and one multiply per pixel cheaper.
            let image =
                egui::ColorImage::from_rgba_premultiplied([frame.width, frame.height], &frame.rgba);
            match self.shown.get_mut(&session) {
                Some((tex, uploaded)) => {
                    tex.set(image, egui::TextureOptions::LINEAR);
                    *uploaded = seq;
                }
                None => {
                    let tex = ctx.load_texture(
                        format!("video-{session}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.shown.insert(session, (tex, seq));
                }
            }
        }
        fresh
    }

    /// One session's picture, for `crate::ui::widgets::avatar`.
    pub fn get(&self, session: Session) -> Option<egui::load::SizedTexture> {
        self.shown
            .get(&session)
            .map(|(tex, _)| egui::load::SizedTexture::from_handle(tex))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise real flow-control stalls: both an explicit source revocation
    /// and dropping the entire sending task must reset, never implicitly FIN.
    #[tokio::test]
    async fn cancelled_video_writes_reset_instead_of_finishing_partial_frames() {
        use rustls::pki_types::PrivatePkcs8KeyDer;
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der = cert.cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        let mut config =
            quinn::ServerConfig::with_single_cert(vec![der.clone()], key.into()).unwrap();
        let mut transport = quinn::TransportConfig::default();
        transport.stream_receive_window(64u32.into());
        transport.receive_window(128u32.into());
        config.transport_config(Arc::new(transport));
        let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(der).unwrap();
        let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(
            quinn::ClientConfig::with_root_certificates(Arc::new(roots)).unwrap(),
        );
        let connect = client
            .connect(server.local_addr().unwrap(), "localhost")
            .unwrap();
        let (outgoing, incoming) =
            tokio::join!(connect, async { server.accept().await.unwrap().await });
        let outgoing = outgoing.unwrap();
        let incoming = incoming.unwrap();
        for mode in 0..3 {
            let sender = Sender::new(outgoing.clone());
            let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
            let task = tokio::spawn(async move {
                sender
                    .send_until(
                        true,
                        0,
                        &vec![0x42; 4096],
                        async {
                            let _ = cancelled.await;
                        },
                        |s| s.finish().map_err(|e| e.to_string()),
                    )
                    .await
            });
            let mut recv = tokio::time::timeout(Duration::from_secs(2), incoming.accept_uni())
                .await
                .unwrap()
                .unwrap();
            let mut header = [0; VIDEO_HEADER_LEN];
            tokio::time::timeout(Duration::from_secs(2), recv.read_exact(&mut header))
                .await
                .unwrap()
                .unwrap();
            assert!(!task.is_finished(), "body is blocked by the receive window");
            if mode == 2 {
                let result = tokio::time::timeout(Duration::from_millis(500), task)
                    .await
                    .expect("full-frame deadline")
                    .unwrap();
                assert_eq!(result, Err("flow control: frame dropped".into()));
            } else if mode == 1 {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                cancel.send(()).unwrap();
                assert!(task.await.unwrap().is_err());
            }
            let result = tokio::time::timeout(Duration::from_secs(2), recv.read_to_end(8192))
                .await
                .unwrap();
            assert!(
                matches!(
                    result,
                    Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(_)))
                ),
                "partial frame must reset: {result:?}"
            );
        }
        outgoing.close(0u32.into(), b"done");
        incoming.close(0u32.into(), b"done");
        // A frame already past its deadline is not sent, but it was
        // encoded: its number is spent, so the receivers see the gap.
        let sender = Sender::new(outgoing.clone());
        let late = Instant::now() - Duration::from_millis(200);
        let result = sender
            .send_captured_until(
                false,
                0,
                &[0x42; 16],
                std::future::pending(),
                |s| s.finish().map_err(|e| e.to_string()),
                (crate::media::Stamp::default(), late, true),
            )
            .await;
        assert_eq!(result, Err("flow control: frame dropped".into()));
        assert_eq!(sender.seq.load(Ordering::Relaxed), 1);
    }

    /// A decoded test picture.
    fn picture_at(at: Instant, micros: u64) -> Frame {
        Frame {
            width: 2,
            height: 2,
            rgba: Arc::new(vec![0; 16]),
            at,
            stamp: crate::media::Stamp { micros, epoch: 1 },
        }
    }

    /// Plays `frames` pictures into `r`, one every `period_ms` starting at
    /// `start`, and renders every 16 ms in between like a window does.
    /// `sound(i)` is how many ms after its decode picture `i`'s sound
    /// plays, as the output device reports it; `None`: no report.
    ///
    /// Checked at every render, whatever the sound says: once a picture
    /// was shown there always is one, it never goes back to an older one,
    /// and no picture waits longer than [`SYNC_MAX_HOLD`]. Returns the
    /// time of the last render.
    fn play(
        r: &mut Receiver,
        start: Instant,
        frames: u64,
        period_ms: u64,
        accurate: bool,
        sound: impl Fn(u64) -> Option<i64>,
    ) -> Instant {
        let ms = Duration::from_millis;
        let first = r.seq;
        let mut decoded: Vec<(u64, Instant)> = Vec::new();
        let mut last_shown = r.shown.lock().unwrap().as_ref().map(|(seq, _)| *seq);
        let mut rendered = start;
        for i in 0..frames {
            let at = start + ms(i * period_ms);
            let micros = 1_000_000 + (first + i) * period_ms * 1000;
            r.audio = sound(i).map(|offset| crate::media::Playout {
                stamp: crate::media::Stamp { micros, epoch: 1 },
                at: if offset >= 0 {
                    at + ms(offset as u64)
                } else {
                    at - ms((-offset) as u64)
                },
            });
            r.audio_accurate = accurate;
            r.present(picture_at(at, micros), None);
            decoded.push((r.seq, at));
            let mut now = at;
            while now < at + ms(period_ms) {
                let shown = r.selected(now).map(|(seq, _)| seq);
                assert!(
                    shown.is_some() || last_shown.is_none(),
                    "the picture disappeared at picture {i}"
                );
                assert!(shown >= last_shown, "back to an older picture at {i}");
                // Everything decoded at least the longest hold ago is on
                // screen or was replaced by something newer.
                let overdue = decoded
                    .iter()
                    .rev()
                    .find(|(_, t)| *t + SYNC_MAX_HOLD <= now)
                    .map(|(seq, _)| *seq);
                assert!(shown >= overdue, "picture {i}: {shown:?} < {overdue:?}");
                last_shown = shown;
                rendered = now;
                now += ms(16);
            }
        }
        rendered
    }

    /// The pictures wait for sound that plays later, as far as the hold
    /// goes, and are never taken off the screen for it: not at the −421 ms
    /// that blacked out 0.1.27, not by a report that is nonsense.
    #[test]
    fn pictures_wait_for_their_sound_and_never_disappear() {
        let ms = Duration::from_millis;
        let now = Instant::now();
        for offset in [-5000i64, -421, -101, -40, 0, 40, 101, 250, 421, 900, 5000] {
            let mut r = Receiver::new();
            let end = play(&mut r, now, 150, 66, true, |_| Some(offset));
            // Sound that played long ago is no timing at all; sound further
            // away than the hold reaches is followed to its end.
            let wanted = if offset < -2000 {
                Duration::ZERO
            } else {
                ms(offset.max(0) as u64)
                    .saturating_sub(SYNC_LEAD)
                    .min(SYNC_MAX_HOLD)
            };
            assert_eq!(r.hold.delay, wanted, "sound {offset} ms after the decode");
            let (seq, shown) = r.selected(end).unwrap();
            if wanted.is_zero() {
                assert_eq!(seq, r.seq, "nothing to wait for: the newest, at once");
            } else if wanted < SYNC_MAX_HOLD {
                // In sync: what is on screen is heard now, give or take a
                // picture and the lead nobody notices.
                let heard = shown.at + ms(offset as u64);
                assert!(heard <= end + SYNC_LEAD, "sound {offset}: picture early");
                assert!(heard + ms(66) > end, "sound {offset}: picture late");
            }
            assert!(
                r.selected(end + Duration::from_secs(6)).is_none(),
                "stalled video still expires"
            );
        }
    }

    /// A playback time the backend only estimated says nothing about the
    /// device's delay: it holds no picture, whatever it claims.
    #[test]
    fn an_estimated_playback_time_holds_nothing() {
        let now = Instant::now();
        let mut r = Receiver::new();
        let end = play(&mut r, now, 60, 66, false, |_| Some(421));
        assert_eq!(r.hold.delay, Duration::ZERO);
        assert_eq!(r.selected(end).unwrap().0, r.seq);
    }

    /// The speaker stops talking, the output device changes, the timing
    /// returns with another value: the hold stays while nothing is known
    /// and follows when something is, and the picture is there throughout.
    #[test]
    fn the_hold_survives_silence_and_follows_a_new_device() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut r = Receiver::new();
        // Garbage in between must not move it: one report in four is off.
        let noisy = |i: u64| Some(if i % 4 == 3 { 5000 } else { 300 });
        let end = play(&mut r, t0, 90, 66, true, noisy);
        assert_eq!(r.hold.delay, ms(260));

        r.audio = None;
        r.hold.forget_sound();
        let end = play(&mut r, end + ms(66), 60, 66, true, |_| None);
        assert_eq!(r.hold.delay, ms(260), "nothing known: kept");

        // Wired speakers now: the first reports do not jump, then it goes.
        let end = play(&mut r, end + ms(66), 3, 66, true, |_| Some(20));
        assert_eq!(r.hold.delay, ms(260));
        play(&mut r, end + ms(66), 60, 66, true, |_| Some(20));
        assert_eq!(r.hold.delay, Duration::ZERO);
    }

    /// More pictures than the hold has room for are shown early, in order,
    /// rather than dropped unseen.
    #[test]
    fn a_flood_of_pictures_is_shown_not_dropped() {
        let t0 = Instant::now();
        let mut r = Receiver::new();
        // 125 pictures a second against a 600 ms hold: 75 would wait.
        let end = play(&mut r, t0, 600, 8, true, |_| Some(5000));
        assert_eq!(r.hold.delay, SYNC_MAX_HOLD);
        assert_eq!(r.pictures.len(), PICTURES);
        let (seq, _) = r.selected(end).unwrap();
        assert!(r.seq - seq <= PICTURES as u64, "{seq} of {}", r.seq);
    }

    /// A speaker whose own sound has not been timed yet — silent so far, on
    /// camera — is held like the others: the output device is the same.
    #[test]
    fn a_silent_speaker_is_held_like_the_others() {
        let now = Instant::now();
        let mut hold = Hold::default();
        for i in 0..100u64 {
            hold.due(
                now + Duration::from_millis(i * 66),
                None,
                Some(Duration::from_millis(300)),
            );
        }
        assert_eq!(hold.delay, Duration::from_millis(300));
        // Its own measurements count once it has them.
        for i in 100..200u64 {
            hold.due(
                now + Duration::from_millis(i * 66),
                Some(140_000),
                Some(Duration::from_millis(300)),
            );
        }
        assert_eq!(hold.delay, Duration::from_millis(100));
    }

    #[test]
    fn decode_queue_bounds_bytes_and_a_single_speakers_share() {
        let mut q = DecodeQueue::default();
        let now = Instant::now();
        for speaker in 0..64 {
            for _ in 0..4 {
                let mut f = frame(true);
                f.data = vec![0; VIDEO_MAX_FRAME];
                q.push(speaker, f, now);
            }
        }
        assert!(q.frames.len() <= DECODE_QUEUE);
        assert!(q.bytes <= 8 * 1024 * 1024);
        for speaker in 0..64 {
            assert!(q.frames.iter().filter(|f| f.0 == speaker).count() <= 2);
        }
        assert_eq!(
            q.bytes,
            q.frames.iter().map(|f| f.1.data.len()).sum::<usize>()
        );
    }

    fn frame(keyframe: bool) -> EncodedFrame {
        EncodedFrame {
            header: VideoHeader {
                media_time: 0,
                media_epoch: 0,
                flags: if keyframe { 1 } else { 0 },
                speaker: 3,
                seq: 0,
                ts: 0,
                codec: VIDEO_CODEC_H264,
            },
            data: vec![0, 0, 0, 1, 0x65],
        }
    }

    #[test]
    fn video_tier_u8_roundtrips_and_saturates() {
        for tier in [
            VideoTier::T0,
            VideoTier::T1,
            VideoTier::T2,
            VideoTier::T3,
            VideoTier::T4,
        ] {
            assert_eq!(VideoTier::from_u8(tier.as_u8()), tier);
        }
        assert_eq!(VideoTier::T3.step_down(), VideoTier::T4);
        assert_eq!(VideoTier::T4.step_down(), VideoTier::T4, "saturates at T4");
        assert_eq!(VideoTier::T0.step_up(), VideoTier::T0, "saturates at T0");
        assert_eq!(VideoTier::T4.step_up(), VideoTier::T3);
        assert!(VideoTier::T4.paused());
        assert!(!VideoTier::T0.paused());
    }

    /// Real access units: `n` pictures of a moving pattern, an IDR first and
    /// wherever `idr` says, P-frames in between. `seq` counts from 0.
    #[cfg(feature = "camera")]
    fn encoded(n: usize, idr: &[usize]) -> Vec<EncodedFrame> {
        use openh264::{
            encoder::FrameType,
            formats::{RgbSliceU8, YUVBuffer},
        };
        let mut enc = crate::camera::encoder(VideoTier::T0).unwrap();
        (0..n)
            .map(|i| {
                let rgb: Vec<u8> = (0..240usize)
                    .flat_map(|y| {
                        (0..320usize).flat_map(move |x| [(x + 4 * i) as u8, y as u8, (x ^ y) as u8])
                    })
                    .collect();
                let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (320, 240)));
                if idr.contains(&i) {
                    enc.force_intra_frame();
                }
                let bs = enc.encode(&yuv).unwrap();
                let keyframe = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
                EncodedFrame {
                    header: VideoHeader {
                        media_time: 0,
                        media_epoch: 0,
                        flags: u8::from(keyframe),
                        speaker: 3,
                        seq: i as u16,
                        ts: 0,
                        codec: VIDEO_CODEC_H264,
                    },
                    data: bs.to_vec(),
                }
            })
            .collect()
    }

    /// `frame` as the `seq`-th of its stream.
    #[cfg(feature = "camera")]
    fn numbered(frame: &EncodedFrame, seq: u16) -> EncodedFrame {
        let mut frame = frame.clone();
        frame.header.seq = seq;
        frame
    }

    /// Decoded pictures so far for session 3.
    #[cfg(feature = "camera")]
    fn pictures(r: &Receivers) -> u64 {
        r.map.get(&3).map_or(0, |r| r.seq)
    }

    #[cfg(feature = "camera")]
    #[test]
    fn keyframe_is_requested_until_an_idr_arrives() {
        let frames = encoded(2, &[]);
        assert!(frames[0].header.keyframe() && !frames[1].header.keyframe());
        let mut r = Receivers::default();
        r.push(3, numbered(&frames[1], 10));
        assert_eq!(r.take_keyframe_requests(), vec![3], "a P-frame first");
        assert!(r.take_keyframe_requests().is_empty(), "rate limited");
        r.push(3, numbered(&frames[0], 11));
        assert!(r.has_keyframe(3));
        assert_eq!(pictures(&r), 1);
        r.push(3, numbered(&frames[1], 12));
        assert_eq!(pictures(&r), 2);
        assert!(r.take_keyframe_requests().is_empty());
        assert_eq!(r.frames(3), 3);
        r.remove(3);
        assert_eq!(r.frames(3), 0);
    }

    /// A frame lost on the way (the server dropped it for a slow receiver,
    /// or the sender's flow control did): the next P-frames refer to it, so
    /// they wait for an IDR, which is asked for at once instead of up to
    /// 2 s of broken picture until the periodic one.
    #[cfg(feature = "camera")]
    #[test]
    fn a_gap_waits_for_a_requested_idr() {
        let frames = encoded(7, &[5]);
        let mut r = Receivers::default();
        for f in &frames[..2] {
            r.push(3, f.clone());
        }
        assert_eq!(pictures(&r), 2);
        assert!(r.take_keyframe_requests().is_empty());
        // Frame 2 never arrives.
        r.push(3, frames[3].clone());
        r.push(3, frames[4].clone());
        assert_eq!(pictures(&r), 2, "not decoded against a missing reference");
        assert!(!r.has_keyframe(3));
        assert_eq!(r.take_keyframe_requests(), vec![3], "asked for an IDR");
        r.push(3, frames[5].clone());
        r.push(3, frames[6].clone());
        assert_eq!(pictures(&r), 4, "decoding again from the IDR on");
        assert!(r.has_keyframe(3));
    }

    /// The sequence numbers `Reorder` passes on for session 3.
    #[cfg(feature = "camera")]
    fn passed(out: &[(Session, EncodedFrame)]) -> Vec<u16> {
        out.iter().map(|(_, f)| f.header.seq).collect()
    }

    /// The P-frame after an IDR is one packet, the IDR many: one lost
    /// packet and the P-frame arrives first. It waits for the IDR; nothing
    /// is lost and no IDR has to be asked for.
    #[cfg(feature = "camera")]
    #[test]
    fn a_frame_that_overtook_its_predecessor_waits_for_it() {
        let frames = encoded(6, &[3]);
        assert!(frames[3].header.keyframe());
        let t0 = Instant::now();
        let mut reorder = Reorder::default();
        let mut out = Vec::new();
        for i in [0, 1, 2, 4, 3, 5] {
            reorder.push(3, frames[i].clone(), t0, &mut out);
        }
        assert_eq!(passed(&out), [0, 1, 2, 3, 4, 5]);
        let mut r = Receivers::default();
        for (session, frame) in out {
            r.push(session, frame);
        }
        assert_eq!(pictures(&r), 6, "every frame decoded");
        assert!(r.has_keyframe(3));
        assert!(r.take_keyframe_requests().is_empty());

        // Without the hold: the IDR is dropped as late and the picture
        // waits for another one.
        let mut r = Receivers::default();
        for i in [0, 1, 2, 4, 3, 5] {
            r.push(3, frames[i].clone());
        }
        assert_eq!(pictures(&r), 3);
        assert!(!r.has_keyframe(3));
    }

    /// A predecessor that never comes holds its successors for
    /// `REORDER_HOLD` and no longer; then the gap is real.
    #[cfg(feature = "camera")]
    #[test]
    fn a_lost_frame_delays_its_successors_only_briefly() {
        let frames = encoded(5, &[]);
        let t0 = Instant::now();
        let mut reorder = Reorder::default();
        let mut out = Vec::new();
        reorder.push(3, frames[0].clone(), t0, &mut out);
        reorder.push(3, frames[2].clone(), t0, &mut out);
        reorder.release_due(t0 + REORDER_HOLD - Duration::from_millis(1), &mut out);
        assert_eq!(passed(&out), [0], "still waiting for 1");
        reorder.release_due(t0 + REORDER_HOLD, &mut out);
        assert_eq!(passed(&out), [0, 2]);
        // The stream goes on after the gap without waiting again.
        reorder.push(3, frames[3].clone(), t0 + REORDER_HOLD, &mut out);
        assert_eq!(passed(&out), [0, 2, 3]);
        // The straggler is passed on for `accept` to drop.
        reorder.push(3, frames[1].clone(), t0 + REORDER_HOLD, &mut out);
        assert_eq!(passed(&out), [0, 2, 3, 1]);
    }

    /// What cannot be a reordering is not held: an IDR, a frame far ahead,
    /// more frames than may wait, another stream.
    #[cfg(feature = "camera")]
    #[test]
    fn only_a_plausible_reordering_is_held() {
        let frames = encoded(2, &[]);
        let (idr, delta) = (&frames[0], &frames[1]);
        let t0 = Instant::now();

        // An IDR ahead releases what waited and itself.
        let mut reorder = Reorder::default();
        let mut out = Vec::new();
        reorder.push(3, numbered(idr, 10), t0, &mut out);
        reorder.push(3, numbered(delta, 12), t0, &mut out);
        reorder.push(3, numbered(idr, 13), t0, &mut out);
        assert_eq!(passed(&out), [10, 12, 13]);

        // Far ahead: a loss burst, not a reordering.
        let mut reorder = Reorder::default();
        let mut out = Vec::new();
        reorder.push(3, numbered(idr, 10), t0, &mut out);
        reorder.push(
            3,
            numbered(delta, 11 + REORDER_HELD as u16 + 1),
            t0,
            &mut out,
        );
        assert_eq!(passed(&out), [10, 15]);

        // One more than may wait, in whatever order they came.
        let mut reorder = Reorder::default();
        let mut out = Vec::new();
        reorder.push(3, numbered(idr, 10), t0, &mut out);
        for seq in [14, 12, 13] {
            reorder.push(3, numbered(delta, seq), t0, &mut out);
        }
        assert_eq!(passed(&out), [10], "three wait");
        reorder.push(3, numbered(delta, 12), t0, &mut out);
        assert_eq!(passed(&out), [10], "a duplicate is not a fourth");
        reorder.push(3, numbered(delta, 11), t0, &mut out);
        assert_eq!(passed(&out), [10, 11, 12, 13, 14]);

        // A new stream on a reused session id, and a new media epoch.
        let mut reorder = Reorder::default();
        let mut out = Vec::new();
        reorder.push(3, numbered(idr, 5000), t0, &mut out);
        reorder.push(3, numbered(idr, 0), t0, &mut out);
        reorder.push(3, numbered(delta, 1), t0, &mut out);
        let mut restarted = numbered(idr, 700);
        restarted.header.media_epoch = 9;
        reorder.push(3, restarted, t0, &mut out);
        assert_eq!(passed(&out), [5000, 0, 1, 700]);

        // `seq` wraps.
        let mut reorder = Reorder::default();
        let mut out = Vec::new();
        reorder.push(3, numbered(idr, u16::MAX - 1), t0, &mut out);
        reorder.push(3, numbered(delta, 0), t0, &mut out);
        reorder.push(3, numbered(delta, u16::MAX), t0, &mut out);
        assert_eq!(passed(&out), [u16::MAX - 1, u16::MAX, 0]);

        // A stream's first frame is its IDR: a P-frame that got ahead of
        // it waits for it too, and alone it is passed on after the hold.
        let mut reorder = Reorder::default();
        let mut out = Vec::new();
        reorder.push(3, numbered(delta, 1), t0, &mut out);
        assert!(out.is_empty());
        reorder.push(3, numbered(idr, 0), t0, &mut out);
        reorder.push(3, numbered(delta, 2), t0, &mut out);
        assert_eq!(passed(&out), [0, 1, 2]);
        let mut reorder = Reorder::default();
        let mut out = Vec::new();
        reorder.push(3, numbered(delta, 7), t0, &mut out);
        reorder.release_due(t0 + REORDER_HOLD, &mut out);
        reorder.push(3, numbered(delta, 8), t0 + REORDER_HOLD, &mut out);
        assert_eq!(passed(&out), [7, 8]);

        // A speaker that stopped is forgotten.
        reorder.release_due(
            t0 + REORDER_HOLD + FRESH + Duration::from_millis(1),
            &mut out,
        );
        assert!(reorder.speakers.is_empty());
    }

    /// The frames' streams complete in any order: a frame older than one
    /// already decoded is dropped instead of being decoded out of order.
    #[cfg(feature = "camera")]
    #[test]
    fn late_frames_are_dropped() {
        let frames = encoded(3, &[]);
        let mut r = Receivers::default();
        for f in &frames {
            r.push(3, f.clone());
        }
        assert_eq!(pictures(&r), 3);
        assert!(r.accept(3, &frames[1]).is_none(), "older");
        assert!(r.accept(3, &frames[2]).is_none(), "the same again");
        assert!(r.has_keyframe(3), "late is not lost");
        // `seq` wraps: 65535 → 0 is one step forward.
        let mut r = Receivers::default();
        r.push(3, numbered(&frames[0], u16::MAX));
        r.push(3, numbered(&frames[1], 0));
        assert_eq!(pictures(&r), 2);
    }

    /// Session ids are reused. A frame of a peer that left can still land
    /// after its `UserLeft` and recreate the receiver; the next peer with
    /// that id starts its `seq` anew and must not be taken for a late
    /// frame of the old stream.
    #[cfg(feature = "camera")]
    #[test]
    fn a_new_stream_on_a_reused_session_starts_over() {
        let frames = encoded(3, &[]);
        let mut r = Receivers::default();
        r.push(3, numbered(&frames[1], 30_001));
        assert_eq!(pictures(&r), 0, "a straggler of the old stream");
        r.push(3, numbered(&frames[0], 0));
        r.push(3, numbered(&frames[1], 1));
        assert_eq!(pictures(&r), 2, "the new stream from its IDR on");
        assert!(
            r.accept(3, &numbered(&frames[0], 0)).is_none(),
            "late again"
        );
    }

    /// A frame the decoder cannot read leaves it without a valid reference:
    /// wait for the next IDR and ask for it.
    #[cfg(feature = "camera")]
    #[test]
    fn a_decode_error_asks_for_an_idr() {
        let frames = encoded(3, &[]);
        let mut r = Receivers::default();
        r.push(3, frames[0].clone());
        assert_eq!(pictures(&r), 1);
        let mut broken = frames[1].clone();
        let keep = broken.data.len() / 2;
        broken.data.truncate(keep);
        r.push(3, broken);
        assert!(!r.has_keyframe(3), "decode failed");
        assert_eq!(r.take_keyframe_requests(), vec![3]);
        r.push(3, frames[2].clone());
        assert_eq!(pictures(&r), 1, "dropped until the IDR");
    }

    /// A modified sender's 1280×720 IDR — a real one, well under the size
    /// limit on the wire — is refused before the decoder allocates for it,
    /// and counts as a failed decode: the receiver asks for a new IDR.
    #[cfg(feature = "camera")]
    #[test]
    fn a_picture_above_the_ceiling_is_refused_before_decoding() {
        use openh264::formats::{RgbSliceU8, YUVBuffer};
        let (w, h) = (1280usize, 720usize);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
        let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (w, h)));
        let mut enc = crate::camera::encoder(VideoTier::T0).unwrap();
        let data = enc.encode(&yuv).unwrap().to_vec();
        assert!(data.len() <= VIDEO_MAX_FRAME);
        assert!(matches!(
            h264::check(&data),
            Err(h264::Reject::TooLarge { .. })
        ));

        let mut decoder = Decoder::new().unwrap();
        assert!(decode(&mut decoder, &data).is_none());
        assert!(decoder.take_failed());

        let ok = encoded(1, &[]);
        assert_eq!(h264::check(&ok[0].data), Ok(()), "the own encoder passes");
    }

    /// An IDR is asked for while the sender's frames arrive, and no longer
    /// once they have stopped.
    #[test]
    fn a_sender_that_stopped_is_not_asked_for_an_idr() {
        let mut r = Receivers::default();
        let t0 = Instant::now();
        r.push(3, frame(false));
        assert_eq!(r.take_keyframe_requests_at(t0), vec![3], "a P-frame first");
        assert_eq!(
            r.take_keyframe_requests_at(t0 + Duration::from_secs(2)),
            vec![3],
            "still sending"
        );
        assert!(
            r.take_keyframe_requests_at(t0 + FRESH + Duration::from_secs(2))
                .is_empty(),
            "stopped"
        );
    }

    /// Nothing empty and nothing above the ceiling reaches a texture.
    #[test]
    fn empty_and_oversize_pictures_are_refused() {
        assert!(picture_size_ok(320, 240));
        assert!(picture_size_ok(640, 480));
        assert!(picture_size_ok(1, 1));
        for (w, h) in [(0, 240), (320, 0), (0, 0), (641, 480), (640, 481)] {
            assert!(!picture_size_ok(w, h), "{w}×{h}");
        }
    }

    /// A session that delivered frames but nothing decodable is not offered
    /// to the participant list: there is no picture to put in the circle,
    /// and the row has to keep its initials.
    #[test]
    fn seqs_skips_sessions_without_a_decoded_picture() {
        let mut r = Receivers::default();
        r.push(3, frame(true));
        assert_eq!(r.frames(3), 1, "the frame arrived");
        assert!(r.seqs().is_empty(), "but nothing decoded out of it");
        assert!(r.picture(3).is_none());
        assert!(r.latest().is_empty());
    }
}
