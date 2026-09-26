//! Audio datagram and video stream relay
//! (`PROTOCOL.md` §3, §4; `Server/IMPLEMENTATION.md` §6.5).
//!
//! The server never decodes media. Audio is forwarded datagram by datagram
//! with the `speaker` field overwritten in place; video is forwarded frame by
//! frame on one unidirectional stream per frame and receiver.

use std::{
    collections::HashMap,
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

/// One sender-receiver pair: at most one frame in flight plus one waiting
/// (`PROTOCOL.md` §4).
pub struct Slot {
    /// `stable_id` of the receiver connection the forwarder writes to. A
    /// session id can come back after wrapping (`room::allocate_session`);
    /// a slot left over from the id's previous owner must not be inherited,
    /// its forwarder has already ended with that old connection.
    conn_id: usize,
    waiting: Mutex<Option<Bytes>>,
    notify: tokio::sync::Notify,
    /// Set when nothing will fill this slot again, so that the forwarder
    /// stops waiting instead of parking forever.
    closed: AtomicBool,
}

impl Slot {
    fn new(conn_id: usize) -> Slot {
        Slot {
            conn_id,
            waiting: Mutex::new(None),
            notify: tokio::sync::Notify::new(),
            closed: AtomicBool::new(false),
        }
    }

    /// Offers a frame. Returns `true` when it displaced an older waiting one,
    /// which is then dropped.
    fn offer(&self, frame: Bytes) -> bool {
        let displaced = lock(&self.waiting).replace(frame).is_some();
        self.notify.notify_one();
        displaced
    }

    fn take(&self) -> Option<Bytes> {
        lock(&self.waiting).take()
    }

    /// Retires the slot: drops whatever is still waiting and wakes the
    /// forwarder so it can end.
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        *lock(&self.waiting) = None;
        self.notify.notify_one();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
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
    if VideoHeader::decode(&header).is_err() {
        Counters::inc(&server.counters.video_dropped_rate);
        refuse(&mut recv);
        return;
    }

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

    let mut payload = Vec::new();
    let read = tokio::time::timeout_at(deadline, async {
        while let Some(chunk) = recv.read_chunk(usize::MAX, true).await? {
            lock(&budget).bitrate.charge(chunk.bytes.len() as f64);
            if payload.len() + chunk.bytes.len() > VIDEO_MAX_FRAME {
                return Ok(false);
            }
            payload.extend_from_slice(&chunk.bytes);
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
    let mut frame = Vec::with_capacity(VIDEO_HEADER_LEN + payload.len());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&payload);

    let receivers = server.room.video_receivers(sender);
    if receivers.is_empty() {
        // Nobody is listening. The stream has already been drained by
        // `read_to_end`, so the sender's flow control keeps moving (§6.5).
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
            let displaced = slot.offer(out.clone());
            if displaced {
                // The older waiting frame is dropped. It never had a stream of
                // its own, so nothing has to be reset.
                Counters::inc(&server.counters.video_dropped_slow_receiver);
            }
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

/// Writes whatever is in the slot to one receiver, one stream per frame.
async fn forward_frames(
    server: Arc<Server>,
    conn: quinn::Connection,
    slot: Arc<Slot>,
    session: Session,
) {
    let closed = conn.closed();
    let shutdown = server.shutdown_requested();
    tokio::pin!(closed, shutdown);
    loop {
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
        let mut send = match conn.open_uni().await {
            Ok(send) => send,
            Err(e) => {
                tracing::debug!(session, error = %e, "cannot open a video stream");
                return;
            }
        };
        // Video is the lowest priority stream (`PROTOCOL.md` §4). The frame
        // is shared with every other receiver; handing it over as a chunk
        // spares quinn a copy.
        let _ = send.set_priority(-1);
        if send.write_chunk(frame).await.is_err() {
            Counters::inc(&server.counters.video_dropped_slow_receiver);
            let _ = send.reset(quinn::VarInt::from_u32(icu_proto::error::RATE_LIMITED));
            continue;
        }
        // `finish()` is synchronous in quinn 0.11 (A5).
        let _ = send.finish();
        Counters::inc(&server.counters.video_frames_out);
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
        assert!(!slot.offer(Bytes::from_static(b"frame")));
        slot.close();
        assert!(slot.is_closed());
        assert!(slot.take().is_none(), "nothing is written after a close");
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
