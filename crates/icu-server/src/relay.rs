//! Audio datagram and video stream relay
//! (`PROTOCOL.md` §3, §4; `Server/IMPLEMENTATION.md` §6.5).
//!
//! The server never decodes media. Audio is forwarded datagram by datagram
//! with the `speaker` field overwritten in place; video is forwarded frame by
//! frame on one unidirectional stream per frame and receiver.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::{Bytes, BytesMut};
use icu_proto::{
    header::{self, VideoHeader, VIDEO_HEADER_LEN, VIDEO_MAX_FRAME},
    AudioHeader, ControlMsg, Session,
};
use quinn::SendDatagramError;

use crate::{limits::TokenBucket, room::Counters, Server};

/// Relays one audio datagram (`PROTOCOL.md` §3).
///
/// Order: check length and `kind`, check that the sender is active and not
/// muted, rate limit, set `speaker`, then `send_datagram` to every other
/// active, non-deafened peer. Send errors are counted per receiver and never
/// block.
pub fn audio(server: &Arc<Server>, sender: Session, datagram: Bytes, bucket: &mut TokenBucket) {
    Counters::inc(&server.counters.datagrams_in);

    let Ok((audio_header, _)) = AudioHeader::decode(&datagram) else {
        Counters::inc(&server.counters.dropped_malformed);
        return;
    };

    let Some(flags) = server.room.flags(sender) else {
        Counters::inc(&server.counters.dropped_malformed);
        return;
    };
    if flags.muted() {
        Counters::inc(&server.counters.dropped_muted);
        return;
    }
    if !bucket.take_one() {
        Counters::inc(&server.counters.dropped_rate);
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
        Counters::inc(&server.counters.dropped_malformed);
        return;
    }
    let out = buf.freeze();

    for (session, conn) in server.room.audio_receivers(sender) {
        // One refcount bump per receiver, no copy.
        match conn.send_datagram(out.clone()) {
            Ok(()) => Counters::inc(&server.counters.datagrams_out),
            // `quinn::SendDatagramError` has exactly these four variants; the
            // `Blocked` from `quinn_proto` does not exist here (A1).
            Err(SendDatagramError::TooLarge) => Counters::inc(&server.counters.send_too_large),
            Err(SendDatagramError::UnsupportedByPeer) => {
                Counters::inc(&server.counters.send_unsupported)
            }
            Err(SendDatagramError::Disabled) => Counters::inc(&server.counters.send_disabled),
            Err(SendDatagramError::ConnectionLost(_)) => {
                Counters::inc(&server.counters.send_connection_lost);
                tracing::debug!(session, "datagram receiver gone");
            }
        }
    }

    // Derive `speaking` from the flow (§6.5).
    let silence = server.config.timing.speaking_silence();
    if audio_header.end() {
        if let Some(flags) = server.room.end_talk_spurt(sender) {
            server.room.broadcast(&ControlMsg::UserState {
                session: sender,
                flags,
            });
        }
    } else if let Some(flags) = server.room.note_audio(sender, silence) {
        server.room.broadcast(&ControlMsg::UserState {
            session: sender,
            flags,
        });
    }
}

/// Per-sender video state: frame rate bucket, bitrate bucket and one slot per
/// receiver.
pub struct VideoSender {
    frames: TokenBucket,
    bitrate: TokenBucket,
    slots: HashMap<Session, Arc<Slot>>,
}

impl VideoSender {
    /// Builds the per-sender limiter from the configured limits.
    pub fn new(limits: &crate::config::LimitsSection) -> VideoSender {
        VideoSender {
            frames: TokenBucket::per_second(limits.video_frames_per_s),
            bitrate: TokenBucket::bitrate(limits.video_kbit_s, Duration::from_secs(2)),
            slots: HashMap::new(),
        }
    }
}

/// One sender-receiver pair: at most one frame in flight plus one waiting
/// (`PROTOCOL.md` §4).
pub struct Slot {
    waiting: Mutex<Option<Bytes>>,
    notify: tokio::sync::Notify,
}

impl Slot {
    fn new() -> Slot {
        Slot {
            waiting: Mutex::new(None),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// Offers a frame. Returns `true` when it displaced an older waiting one,
    /// which is then dropped.
    fn offer(&self, frame: Bytes) -> bool {
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        let displaced = waiting.replace(frame).is_some();
        self.notify.notify_one();
        displaced
    }

    fn take(&self) -> Option<Bytes> {
        self.waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

/// Relays one incoming video frame (`PROTOCOL.md` §4).
///
/// The frame is read completely (bounded by 512 KiB) before it is fanned out,
/// so that the per sender-receiver drop rule can be applied without holding a
/// half written stream open. At the target profile a frame is around 1.2 KiB.
pub async fn video(
    server: Arc<Server>,
    sender: Session,
    mut recv: quinn::RecvStream,
    state: Arc<Mutex<VideoSender>>,
) {
    Counters::inc(&server.counters.video_frames_in);

    let frame = match recv.read_to_end(VIDEO_HEADER_LEN + VIDEO_MAX_FRAME).await {
        Ok(frame) => frame,
        Err(e) => {
            tracing::debug!(sender, error = %e, "video frame read failed");
            return;
        }
    };
    if frame.len() < VIDEO_HEADER_LEN || VideoHeader::decode(&frame).is_err() {
        Counters::inc(&server.counters.video_dropped_rate);
        return;
    }

    // Only while the camera is on and the sender is active (§6.5).
    let Some(flags) = server.room.flags(sender) else {
        Counters::inc(&server.counters.video_dropped_not_allowed);
        return;
    };
    if !flags.camera_on() {
        Counters::inc(&server.counters.video_dropped_not_allowed);
        return;
    }

    {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.frames.take_one() || !state.bitrate.take(frame.len() as f64) {
            Counters::inc(&server.counters.video_dropped_rate);
            return;
        }
    }

    let receivers = server.room.video_receivers(sender);
    if receivers.is_empty() {
        // Nobody is listening. The stream has already been drained by
        // `read_to_end`, so the sender's flow control keeps moving (§6.5).
        Counters::inc(&server.counters.video_dropped_no_receiver);
        return;
    }

    let mut out = BytesMut::from(&frame[..]);
    if header::set_speaker_in_place(&mut out, sender).is_err() {
        Counters::inc(&server.counters.video_dropped_rate);
        return;
    }
    let out = out.freeze();

    for (session, conn) in receivers {
        let slot = {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .slots
                .entry(session)
                .or_insert_with(|| {
                    let slot = Arc::new(Slot::new());
                    tokio::spawn(forward_frames(
                        server.clone(),
                        conn.clone(),
                        slot.clone(),
                        session,
                    ));
                    slot
                })
                .clone()
        };
        if slot.offer(out.clone()) {
            // The older waiting frame is dropped. It never had a stream of its
            // own, so nothing has to be reset.
            Counters::inc(&server.counters.video_dropped_slow_receiver);
        }
    }
}

/// Writes whatever is in the slot to one receiver, one stream per frame.
async fn forward_frames(
    server: Arc<Server>,
    conn: quinn::Connection,
    slot: Arc<Slot>,
    session: Session,
) {
    loop {
        let frame = match slot.take() {
            Some(frame) => frame,
            None => {
                tokio::select! {
                    _ = slot.notify.notified() => continue,
                    _ = conn.closed() => return,
                    _ = server.shutdown_requested() => return,
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
        // Video is the lowest priority stream (`PROTOCOL.md` §4).
        let _ = send.set_priority(-1);
        if send.write_all(&frame).await.is_err() {
            Counters::inc(&server.counters.video_dropped_slow_receiver);
            let _ = send.reset(quinn::VarInt::from_u32(icu_proto::error::RATE_LIMITED));
            continue;
        }
        // `finish()` is synchronous in quinn 0.11 (A5).
        let _ = send.finish();
        Counters::inc(&server.counters.video_frames_out);
    }
}

/// Forwards a `KeyframeRequest` to the speaker, at most once per second
/// (§6.5).
pub fn keyframe_request(server: &Arc<Server>, speaker: Session) -> bool {
    let min_interval = server.config.timing.keyframe_min_interval();
    let now = Instant::now();
    {
        let mut last = server.keyframes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = last.get(&speaker) {
            if now.saturating_duration_since(*previous) < min_interval {
                return false;
            }
        }
        last.insert(speaker, now);
    }
    // The speaker learns "someone needs a keyframe", not who asked.
    server
        .room
        .send_to(speaker, &ControlMsg::KeyframeRequest { speaker: 0 })
}
