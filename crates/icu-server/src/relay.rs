//! Audio datagram and video stream relay
//! (`PROTOCOL.md` §3, §4; `Server/IMPLEMENTATION.md` §6.5).
//!
//! The server never decodes media. Audio is forwarded datagram by datagram
//! with the `speaker` field overwritten in place; video is forwarded frame by
//! frame on one unidirectional stream per frame and receiver.

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use bytes::{Bytes, BytesMut};
use icu_proto::{
    header::{self, VideoHeader, VIDEO_HEADER_LEN, VIDEO_MAX_FRAME},
    AudioHeader, ControlMsg, Session,
};
use quinn::SendDatagramError;

use crate::{limits::TokenBucket, lock, room::Counters, Server};

/// Relays one audio datagram (`PROTOCOL.md` §3).
///
/// Order: check length and `kind`, rate limit, check that the sender is
/// active and not muted, set `speaker`, then `send_datagram` to every other
/// active, non-deafened peer. Send errors are counted per receiver and never
/// block. The rate limit comes before the room lock on purpose: it needs
/// only the task-local bucket, so a flood is turned away without touching
/// shared state or cloning a single receiver handle.
///
/// This is the one hot path in the server — 16 clients at 50 packets/s mean
/// 800 calls a second, each fanning out to 15 peers. It therefore takes the
/// room lock exactly once (fan-out and talk spurt bookkeeping together),
/// touches each counter once, and reuses the caller's `receivers` buffer
/// instead of allocating one per datagram.
pub fn audio(
    server: &Arc<Server>,
    sender: Session,
    datagram: Bytes,
    bucket: &mut TokenBucket,
    receivers: &mut Vec<(Session, quinn::Connection)>,
) {
    let c = &server.counters;
    Counters::inc(&c.datagrams_in);

    let Ok((audio_header, _)) = AudioHeader::decode(&datagram) else {
        Counters::inc(&c.dropped_malformed);
        return;
    };

    if !bucket.take_one() {
        Counters::inc(&c.dropped_rate);
        return;
    }
    // Derive `speaking` from the flow (§6.5), in the same lock as the fan-out.
    let silence = server.config.timing.speaking_silence();
    let Some((flags, flipped)) =
        server
            .room
            .audio_fanout(sender, audio_header.end(), silence, receivers)
    else {
        Counters::inc(&c.dropped_malformed);
        return;
    };
    if flags.muted() {
        Counters::inc(&c.dropped_muted);
        return;
    }

    // Overwrite `speaker` in place. `try_into_mut` succeeds while we hold the
    // only reference, which is the normal case straight out of
    // `read_datagram`; otherwise one copy.
    let mut buf = match datagram.try_into_mut() {
        Ok(buf) => buf,
        Err(shared) => BytesMut::from(&shared[..]),
    };
    if header::set_speaker_in_place(&mut buf, sender).is_err() {
        Counters::inc(&c.dropped_malformed);
        return;
    }
    let out = buf.freeze();

    let (mut sent, mut too_large, mut unsupported, mut disabled, mut lost) = (0, 0, 0, 0, 0);
    for (session, conn) in receivers.iter() {
        // One refcount bump per receiver, no copy.
        match conn.send_datagram(out.clone()) {
            Ok(()) => sent += 1,
            // `quinn::SendDatagramError` has exactly these four variants; the
            // `Blocked` from `quinn_proto` does not exist here (A1).
            Err(SendDatagramError::TooLarge) => too_large += 1,
            Err(SendDatagramError::UnsupportedByPeer) => unsupported += 1,
            Err(SendDatagramError::Disabled) => disabled += 1,
            Err(SendDatagramError::ConnectionLost(_)) => {
                lost += 1;
                tracing::debug!(session, "datagram receiver gone");
            }
        }
    }
    Counters::add(&c.datagrams_out, sent);
    Counters::add(&c.send_too_large, too_large);
    Counters::add(&c.send_unsupported, unsupported);
    Counters::add(&c.send_disabled, disabled);
    Counters::add(&c.send_connection_lost, lost);

    if let Some(flags) = flipped {
        server.room.broadcast(&ControlMsg::UserState {
            session: sender,
            flags,
        });
    }
}

/// Rolling per-receiver slot-offer/displacement counts for one congestion
/// window (`PROTOCOL.md` §2.2).
#[derive(Default)]
struct CongestionWindow {
    offers: u32,
    displacements: u32,
}

/// A user's video allowance (`PROTOCOL.md` §7): frame rate and bytes. Kept
/// per user on the [`Server`], not per connection, so reconnecting does not
/// hand out a fresh burst.
pub struct VideoBudget {
    frames: TokenBucket,
    bitrate: TokenBucket,
}

impl VideoBudget {
    /// A full allowance from the configured limits.
    pub fn new(limits: &crate::config::LimitsSection) -> VideoBudget {
        VideoBudget {
            frames: TokenBucket::per_second(limits.video_frames_per_s),
            bitrate: TokenBucket::bitrate(limits.video_kbit_s, Duration::from_secs(2)),
        }
    }
}

/// Per-connection video state: one slot per receiver, and the rolling
/// counts behind `VideoCongestionHint`.
pub struct VideoSender {
    slots: HashMap<Session, Arc<Slot>>,
    congestion: HashMap<Session, CongestionWindow>,
    window_start: Instant,
    last_hint_permille: u16,
}

impl VideoSender {
    /// No receivers yet, an empty congestion window.
    pub fn new() -> VideoSender {
        VideoSender {
            slots: HashMap::new(),
            congestion: HashMap::new(),
            window_start: Instant::now(),
            last_hint_permille: 0,
        }
    }

    /// Records one slot offer for `receiver` at `now`, and returns a new
    /// `VideoCongestionHint` value once the window has closed and the
    /// worst-receiver drop rate changed by at least
    /// [`icu_proto::limits::VIDEO_CONGESTION_HINT_STEP_PERMILLE`] since the
    /// last hint sent — mirrors `LossTracker`'s max-across-reporters rule.
    ///
    /// Simplification: the window only closes on an incoming frame, so a
    /// sender that stops sending video (camera off) freezes at its last
    /// hint rather than decaying to 0 like `LossHint` does on expiry. That is
    /// harmless here: with no video in flight, the hint has nothing left to
    /// act on until the sender resumes and a fresh window closes.
    fn record_and_maybe_hint_at(
        &mut self,
        receiver: Session,
        displaced: bool,
        now: Instant,
    ) -> Option<u16> {
        let entry = self.congestion.entry(receiver).or_default();
        entry.offers += 1;
        if displaced {
            entry.displacements += 1;
        }

        let window = Duration::from_secs(icu_proto::limits::VIDEO_CONGESTION_WINDOW_S);
        if now.saturating_duration_since(self.window_start) < window {
            return None;
        }
        let worst = self
            .congestion
            .values()
            .map(|w| {
                w.displacements
                    .checked_mul(1000)
                    .and_then(|n| n.checked_div(w.offers))
                    .unwrap_or(0)
                    .min(1000) as u16
            })
            .max()
            .unwrap_or(0);
        self.congestion.clear();
        self.window_start = now;

        if worst.abs_diff(self.last_hint_permille)
            >= icu_proto::limits::VIDEO_CONGESTION_HINT_STEP_PERMILLE
        {
            self.last_hint_permille = worst;
            Some(worst)
        } else {
            None
        }
    }
}

impl Default for VideoSender {
    fn default() -> VideoSender {
        VideoSender::new()
    }
}

impl Drop for VideoSender {
    /// The sender is gone (its `uni_task` was aborted), so every forwarder it
    /// spawned has to end too — otherwise it would park on a slot nobody will
    /// ever fill again, for as long as its receiver stays connected.
    fn drop(&mut self) {
        for slot in self.slots.values() {
            slot.close();
        }
    }
}

/// One sender-receiver pair: at most
/// [`VIDEO_UNACKED_PER_PAIR`](icu_proto::limits::VIDEO_UNACKED_PER_PAIR)
/// frames unacknowledged by the receiver plus one waiting (`PROTOCOL.md` §4).
pub struct Slot {
    /// `stable_id` of the receiver connection the forwarder writes to. A
    /// session id can come back after wrapping (`room::allocate_session`);
    /// a slot left over from the id's previous owner must not be inherited,
    /// its forwarder has already ended with that old connection.
    conn_id: usize,
    waiting: Mutex<Waiting>,
    notify: tokio::sync::Notify,
    /// Set when nothing will fill this slot again, so that the forwarder
    /// stops waiting instead of parking forever.
    closed: AtomicBool,
}

/// What became of a frame offered to a [`Slot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offer {
    /// It waits to be written.
    Queued,
    /// A frame was already waiting, so one of the two was dropped: the
    /// receiver is behind. This is what `VideoCongestionHint` counts.
    Displaced,
    /// Dropped because the pair waits for a keyframe after an earlier drop.
    /// A consequence of that drop, not more congestion: counted as a slow
    /// receiver's loss, but not again in the hint.
    Skipped,
}

/// What waits in a [`Slot`].
#[derive(Default)]
struct Waiting {
    /// The next frame to write, and whether it is a keyframe.
    frame: Option<(Bytes, bool)>,
    /// A frame of this pair was dropped: every delta after it refers to a
    /// picture the receiver never gets, so only a keyframe passes until one
    /// is taken in (`VIDEO_FLAG_KEYFRAME`).
    need_keyframe: bool,
}

impl Slot {
    fn new(conn_id: usize) -> Slot {
        Slot {
            conn_id,
            waiting: Mutex::new(Waiting::default()),
            notify: tokio::sync::Notify::new(),
            closed: AtomicBool::new(false),
        }
    }

    /// Offers a frame.
    ///
    /// A keyframe needs nothing before it, so it takes the place of whatever
    /// waits. A delta is dropped instead when a frame already waits — the
    /// waiting one stays decodable, and a delta behind a dropped frame would
    /// not be (the waiting one may also be the keyframe the receiver needs)
    /// — and whenever the pair waits for a keyframe.
    fn offer(&self, frame: Bytes, keyframe: bool) -> Offer {
        let offer = {
            let mut waiting = lock(&self.waiting);
            let taken = waiting.frame.is_some();
            if keyframe {
                waiting.need_keyframe = false;
                waiting.frame = Some((frame, true));
                if taken {
                    Offer::Displaced
                } else {
                    Offer::Queued
                }
            } else if taken {
                waiting.need_keyframe = true;
                Offer::Displaced
            } else if waiting.need_keyframe {
                Offer::Skipped
            } else {
                waiting.frame = Some((frame, false));
                Offer::Queued
            }
        };
        self.notify.notify_one();
        offer
    }

    fn take(&self) -> Option<Bytes> {
        lock(&self.waiting).frame.take().map(|(frame, _)| frame)
    }

    /// A frame of this pair was lost after it left the slot (not written or
    /// not acknowledged in time). A keyframe waiting behind it repairs that;
    /// otherwise only a keyframe passes from now on, and a delta already
    /// waiting is dropped with it. Returns whether one was.
    fn lost(&self) -> bool {
        let mut waiting = lock(&self.waiting);
        match waiting.frame {
            Some((_, true)) => false,
            Some((_, false)) => {
                waiting.frame = None;
                waiting.need_keyframe = true;
                true
            }
            None => {
                waiting.need_keyframe = true;
                false
            }
        }
    }

    /// Retires the slot: drops whatever is still waiting and wakes the
    /// forwarder so it can end.
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        lock(&self.waiting).frame = None;
        self.notify.notify_one();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Only the forwarder waits on this notification. Offers wake it too;
    /// the latest offered frame stays in `waiting` until this write ends.
    async fn closed(&self) {
        while !self.is_closed() {
            self.notify.notified().await;
        }
    }
}

/// Relays one incoming video frame (`PROTOCOL.md` §4).
///
/// The header is read and checked first — camera on, sender active, frame
/// rate — and only then the payload (bounded by 512 KiB): a frame that is
/// going to be dropped is refused with `stop(RATE_LIMITED)` instead of being
/// read into memory. The payload is then read completely before it is fanned
/// out, so that the per sender-receiver drop rule can be applied without
/// holding a half written stream open. At the target profile a frame is
/// around 1.2 KiB.
///
/// Header and payload together have `timing.video_frame_timeout_ms`: a
/// sender that opens a stream and stops writing would otherwise hold this
/// task, its buffer and one of its few stream slots (`session::uni_task`)
/// for as long as the connection lives.
pub async fn video(
    server: Arc<Server>,
    sender: Session,
    mut recv: quinn::RecvStream,
    state: Arc<Mutex<VideoSender>>,
    budget: Arc<Mutex<VideoBudget>>,
) {
    Counters::inc(&server.counters.video_frames_in);
    let refuse = |recv: &mut quinn::RecvStream| {
        let _ = recv.stop(quinn::VarInt::from_u32(icu_proto::error::RATE_LIMITED));
    };
    let deadline = tokio::time::Instant::now() + server.config.timing.video_frame_timeout();
    let stalled = |recv: &mut quinn::RecvStream| {
        Counters::inc(&server.counters.video_dropped_stalled);
        tracing::debug!(sender, "video frame stalled, stream stopped");
        refuse(recv);
    };

    let mut header = [0u8; VIDEO_HEADER_LEN];
    match tokio::time::timeout_at(deadline, recv.read_exact(&mut header)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::debug!(sender, error = %e, "video header read failed");
            return;
        }
        Err(_) => {
            stalled(&mut recv);
            return;
        }
    }
    let Ok(decoded) = VideoHeader::decode(&header) else {
        Counters::inc(&server.counters.video_dropped_rate);
        refuse(&mut recv);
        return;
    };
    let keyframe = decoded.keyframe();

    // Only while the camera is on and the sender is active (§6.5).
    if !server.room.flags(sender).is_some_and(|f| f.camera_on()) {
        Counters::inc(&server.counters.video_dropped_not_allowed);
        refuse(&mut recv);
        return;
    }
    // The byte budget decides before the payload is read: a sender in debt
    // is stopped here, and its bytes never cross more than the stream's
    // receive window. An admitted frame is read to its end and charged as
    // it arrives, into debt if need be — a frame larger than the 2 s burst,
    // a big IDR, still passes when the budget is not spent — so frames in
    // flight at the same time see each other's bytes.
    {
        let mut budget = lock(&budget);
        if !budget.bitrate.has_credit() || !budget.frames.take_one() {
            drop(budget);
            Counters::inc(&server.counters.video_dropped_rate);
            refuse(&mut recv);
            return;
        }
        budget.bitrate.charge(VIDEO_HEADER_LEN as f64);
    }

    let mut frame = Vec::from(header);
    let read = tokio::time::timeout_at(deadline, async {
        while let Some(chunk) = recv.read_chunk(usize::MAX, true).await? {
            lock(&budget).bitrate.charge(chunk.bytes.len() as f64);
            if frame.len() + chunk.bytes.len() > VIDEO_HEADER_LEN + VIDEO_MAX_FRAME {
                return Ok(false);
            }
            frame.extend_from_slice(&chunk.bytes);
        }
        Ok::<_, quinn::ReadError>(true)
    })
    .await;
    match read {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            tracing::debug!(sender, "video frame larger than {VIDEO_MAX_FRAME} bytes");
            refuse(&mut recv);
            return;
        }
        Ok(Err(e)) => {
            tracing::debug!(sender, error = %e, "video frame read failed");
            return;
        }
        Err(_) => {
            stalled(&mut recv);
            return;
        }
    }
    // Permission may have changed while the payload was arriving.
    if !server.room.flags(sender).is_some_and(|f| f.camera_on()) {
        Counters::inc(&server.counters.video_dropped_not_allowed);
        return;
    }

    let receivers = server.room.video_receivers(sender);
    if receivers.is_empty() {
        // Retire forwarders even when the last viewer stopped watching.
        // The incoming stream is already drained, so flow control advances.
        let mut state = lock(&state);
        for slot in state.slots.values() {
            slot.close();
        }
        state.slots.clear();
        state.congestion.clear();
        Counters::inc(&server.counters.video_dropped_no_receiver);
        return;
    }

    if header::set_speaker_in_place(&mut frame, sender).is_err() {
        Counters::inc(&server.counters.video_dropped_rate);
        return;
    }
    // `Bytes::from(Vec<u8>)` takes ownership; the fan-out below only clones
    // the refcount.
    let out = Bytes::from(frame);

    // One clock read and one lock for the whole fan-out.
    let now = Instant::now();
    let mut hint = None;
    {
        let mut state = lock(&state);
        // Receivers that left take their slot with them, and so does an id
        // that now belongs to a different connection. Without this the map —
        // and a held-back frame in each entry — would grow for as long as the
        // sender stays connected.
        state.slots.retain(|session, slot| {
            let keep = receivers
                .iter()
                .any(|(r, c)| r == session && c.stable_id() == slot.conn_id);
            if !keep {
                slot.close();
            }
            keep
        });
        for (session, conn) in &receivers {
            let slot = state
                .slots
                .entry(*session)
                .or_insert_with(|| {
                    let slot = Arc::new(Slot::new(conn.stable_id()));
                    crate::spawn_logged(
                        "video forwarder",
                        forward_frames(server.clone(), conn.clone(), slot.clone(), *session),
                    );
                    slot
                })
                .clone();
            let offer = slot.offer(out.clone(), keyframe);
            if offer != Offer::Queued {
                // A frame that never had a stream of its own is dropped, so
                // nothing has to be reset.
                Counters::inc(&server.counters.video_dropped_slow_receiver);
            }
            let displaced = offer == Offer::Displaced;
            // At most one window closes per frame, so at most one hint.
            hint = state
                .record_and_maybe_hint_at(*session, displaced, now)
                .or(hint);
        }
    }
    if let Some(drop_permille) = hint {
        server
            .room
            .send_to(sender, &ControlMsg::VideoCongestionHint { drop_permille });
    }
}

/// A frame handed to quinn that the receiver has not acknowledged yet.
struct InFlight {
    send: quinn::SendStream,
    /// [`quinn::SendStream::stopped`]: `Ok(None)` once the receiver has
    /// acknowledged all of the finished stream, `Ok(Some(_))` if it stopped
    /// it, an error if the connection is gone.
    acked: Pin<Box<dyn Future<Output = Result<Option<quinn::VarInt>, quinn::StoppedError>> + Send>>,
    /// Open, write and acknowledgement together end here.
    deadline: tokio::time::Instant,
}

/// Writes whatever is in the slot to one receiver, one stream per frame.
///
/// `finish()` only queues a frame; quinn delivers it later. Counting frames
/// handed over let a receiver with too narrow a downlink collect as many as
/// its stream credit and the send window held — seconds of video, with no
/// frame ever dropped and so no `VideoCongestionHint`. So at most
/// [`VIDEO_UNACKED_PER_PAIR`](icu_proto::limits::VIDEO_UNACKED_PER_PAIR)
/// frames stay unacknowledged; the next one waits in the slot, where newer
/// frames replace or drop it. One not acknowledged by its deadline is reset
/// and the pair waits for a keyframe.
async fn forward_frames(
    server: Arc<Server>,
    conn: quinn::Connection,
    slot: Arc<Slot>,
    session: Session,
) {
    let closed = conn.closed();
    let shutdown = server.shutdown_requested();
    tokio::pin!(closed, shutdown);
    let mut in_flight: VecDeque<InFlight> = VecDeque::new();
    let lost = |slot: &Slot| {
        Counters::inc(&server.counters.video_dropped_slow_receiver);
        if slot.lost() {
            Counters::inc(&server.counters.video_dropped_slow_receiver);
        }
    };
    loop {
        if in_flight.len() >= icu_proto::limits::VIDEO_UNACKED_PER_PAIR {
            if let Some(oldest) = in_flight.front_mut() {
                tokio::select! {
                    biased;
                    _ = slot.closed() => return,
                    _ = &mut shutdown => return,
                    _ = &mut closed => return,
                    acked = &mut oldest.acked => {
                        if acked.is_err() {
                            return;
                        }
                    }
                    _ = tokio::time::sleep_until(oldest.deadline) => {
                        // Not delivered in time: stop retransmitting it.
                        let code = quinn::VarInt::from_u32(icu_proto::error::RATE_LIMITED);
                        let _ = oldest.send.reset(code);
                        lost(&slot);
                    }
                }
            }
            in_flight.pop_front();
            continue;
        }
        let frame = match slot.take() {
            Some(frame) => frame,
            None if slot.is_closed() => return,
            None => {
                tokio::select! {
                    _ = slot.notify.notified() => continue,
                    _ = &mut closed => return,
                    _ = &mut shutdown => return,
                }
            }
        };
        // Cover both stream credit and write flow control. A peer can keep
        // control/audio alive while refusing video indefinitely; closing a
        // slot must also cancel a forwarder blocked in either operation.
        let deadline = tokio::time::Instant::now() + server.config.timing.video_frame_timeout();
        let mut send = tokio::select! {
            biased;
            _ = slot.closed() => return,
            _ = &mut shutdown => return,
            opened = tokio::time::timeout_at(deadline, conn.open_uni()) => match opened {
                Ok(Ok(send)) => send,
                Ok(Err(e)) => {
                    tracing::debug!(session, error = %e, "cannot open a video stream");
                    return;
                }
                Err(_) => {
                    lost(&slot);
                    continue;
                }
            },
        };
        let _ = send.set_priority(-1);
        let written = tokio::select! {
            biased;
            _ = slot.closed() => false,
            _ = &mut shutdown => false,
            result = tokio::time::timeout_at(deadline, send.write_chunk(frame)) => {
                matches!(result, Ok(Ok(())))
            },
        };
        if !written {
            let _ = send.reset(quinn::VarInt::from_u32(icu_proto::error::RATE_LIMITED));
            if slot.is_closed() {
                Counters::inc(&server.counters.video_dropped_slow_receiver);
                return;
            }
            lost(&slot);
            continue;
        }
        if send.finish().is_ok() {
            Counters::inc(&server.counters.video_frames_out);
            let acked = Box::pin(send.stopped());
            in_flight.push_back(InFlight {
                send,
                acked,
                deadline,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender() -> VideoSender {
        VideoSender {
            slots: HashMap::new(),
            congestion: HashMap::new(),
            window_start: Instant::now(),
            last_hint_permille: 0,
        }
    }

    const WINDOW: Duration = Duration::from_secs(icu_proto::limits::VIDEO_CONGESTION_WINDOW_S);

    #[test]
    fn closing_a_slot_drops_the_waiting_frame() {
        let slot = Slot::new(0);
        assert_eq!(
            slot.offer(Bytes::from_static(b"frame"), true),
            Offer::Queued
        );
        slot.close();
        assert!(slot.is_closed());
        assert!(slot.take().is_none(), "nothing is written after a close");
    }

    const KEY: bool = true;
    const DELTA: bool = false;

    /// A waiting keyframe never gives way to a delta, and after any drop
    /// only a keyframe passes until one is taken in (S4, readiness review
    /// 2026-09-27): the receiver cannot decode a delta behind a lost frame.
    /// Only a full slot is congestion; a delta skipped while the pair waits
    /// for a keyframe is not counted again.
    #[test]
    fn a_waiting_keyframe_survives_and_a_drop_waits_for_the_next() {
        let slot = Slot::new(0);
        let offer =
            |frame: &'static [u8], keyframe| slot.offer(Bytes::from_static(frame), keyframe);
        assert_eq!(offer(b"k1", KEY), Offer::Queued);
        assert_eq!(offer(b"d2", DELTA), Offer::Displaced, "d2 is dropped");
        assert_eq!(slot.take().as_deref(), Some(&b"k1"[..]), "k1 still waits");
        assert_eq!(
            offer(b"d3", DELTA),
            Offer::Skipped,
            "d3 follows the dropped d2"
        );
        assert!(slot.take().is_none());
        assert_eq!(offer(b"k4", KEY), Offer::Queued, "k4 clears it");
        assert_eq!(slot.take().as_deref(), Some(&b"k4"[..]));
        assert_eq!(offer(b"d5", DELTA), Offer::Queued);
        assert_eq!(
            offer(b"k6", KEY),
            Offer::Displaced,
            "a keyframe takes a waiting delta's place"
        );
        assert_eq!(slot.take().as_deref(), Some(&b"k6"[..]));
    }

    /// A frame lost after it left the slot drops a waiting delta with it and
    /// makes the pair wait for a keyframe; a keyframe already waiting stays
    /// and repairs the loss by itself.
    #[test]
    fn a_frame_lost_in_flight_waits_for_a_keyframe() {
        let slot = Slot::new(0);
        let offer =
            |frame: &'static [u8], keyframe| slot.offer(Bytes::from_static(frame), keyframe);
        assert_eq!(offer(b"d1", DELTA), Offer::Queued);
        assert!(slot.lost(), "the waiting delta goes");
        assert!(slot.take().is_none());
        assert_eq!(offer(b"d2", DELTA), Offer::Skipped);
        assert_eq!(offer(b"k3", KEY), Offer::Queued);
        assert!(!slot.lost(), "a waiting keyframe stays");
        assert_eq!(slot.take().as_deref(), Some(&b"k3"[..]));
        assert_eq!(offer(b"d4", DELTA), Offer::Queued, "d4 follows k3");
        assert!(slot.lost(), "a waiting delta goes with the loss");
        assert!(slot.take().is_none());
        assert_eq!(offer(b"d5", DELTA), Offer::Skipped);
    }

    /// The sender left: every forwarder it spawned must be able to end
    /// instead of parking on a slot nobody will fill again.
    #[test]
    fn dropping_a_video_sender_closes_its_slots() {
        let slot = Arc::new(Slot::new(0));
        let mut vs = sender();
        vs.slots.insert(7, slot.clone());
        drop(vs);
        assert!(slot.is_closed());
    }

    #[test]
    fn no_hint_before_the_window_closes() {
        let t0 = Instant::now();
        let mut vs = sender();
        vs.window_start = t0;
        assert_eq!(vs.record_and_maybe_hint_at(1, true, t0), None);
    }

    #[test]
    fn worst_receiver_wins_and_resets_the_window() {
        let t0 = Instant::now();
        let mut vs = sender();
        vs.window_start = t0;
        // Receiver 1: 1 of 2 offers displaced (500‰). Receiver 2: 0 of 1 (0‰).
        vs.record_and_maybe_hint_at(1, true, t0);
        vs.record_and_maybe_hint_at(1, false, t0);
        vs.record_and_maybe_hint_at(2, false, t0);
        let hint = vs.record_and_maybe_hint_at(2, false, t0 + WINDOW);
        assert_eq!(hint, Some(500), "receiver 1's 500‰ is the worst");
        assert!(vs.congestion.is_empty(), "the window resets once it closes");
    }

    #[test]
    fn unchanged_worst_rate_sends_no_hint() {
        let t0 = Instant::now();
        let mut vs = sender();
        vs.window_start = t0;
        vs.record_and_maybe_hint_at(1, true, t0);
        vs.record_and_maybe_hint_at(1, true, t0 + WINDOW);
        vs.window_start = t0 + WINDOW;
        vs.record_and_maybe_hint_at(1, true, t0 + WINDOW);
        let hint = vs.record_and_maybe_hint_at(1, true, t0 + WINDOW + WINDOW);
        assert_eq!(hint, None, "still 1000‰, below the change threshold");
    }

    #[test]
    fn a_small_change_below_the_step_is_not_reported() {
        let t0 = Instant::now();
        let mut vs = sender();
        vs.window_start = t0;
        vs.last_hint_permille = 500;
        // 1 of 100 offers displaced = 10‰ away from 0, well under the 500‰
        // baseline change of more than the 20‰ step.
        for _ in 0..99 {
            vs.record_and_maybe_hint_at(1, true, t0);
        }
        let hint = vs.record_and_maybe_hint_at(1, true, t0 + WINDOW);
        assert_eq!(hint, Some(1000), "100 % displaced is far past the step");
    }
}
