//! Video frames over unidirectional streams (`PROTOCOL.md` §4): send,
//! receive, decode per speaker, `KeyframeRequest` bookkeeping.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use icu_proto::{
    header::{VIDEO_CODEC_H264, VIDEO_HEADER_LEN, VIDEO_MAX_FRAME},
    limits as pl, Session, VideoHeader,
};

/// Minimum spacing between two `KeyframeRequest`s for the same speaker. The
/// server forwards at most [`pl::KEYFRAME_REQUESTS_PER_S`] per second and
/// speaker, so asking faster only burns control budget.
const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_secs(1)
    .checked_div(pl::KEYFRAME_REQUESTS_PER_S)
    .unwrap();

/// A quality tier for the outgoing camera stream, stepped by
/// `crate::quality::QualityController` in response to measured network
/// conditions (`Client/IMPLEMENTATION.md` §8). Video absorbs all of the
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

/// A decoded RGBA picture.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// RGBA8, row major.
    pub rgba: Vec<u8>,
    /// When it was decoded.
    pub at: Instant,
}

/// Where received frames go.
pub trait VideoSink: Send + Sync + Clone + 'static {
    /// One complete frame.
    fn deliver(&self, session: Session, frame: EncodedFrame);
}

/// Accepts uni streams for the lifetime of the connection.
pub async fn accept_loop<S: VideoSink>(conn: quinn::Connection, sink: S) {
    loop {
        let mut recv = match conn.accept_uni().await {
            Ok(r) => r,
            Err(_) => return,
        };
        let sink = sink.clone();
        tokio::spawn(async move {
            let bytes = match recv.read_to_end(VIDEO_HEADER_LEN + VIDEO_MAX_FRAME).await {
                Ok(b) => b,
                Err(_) => return,
            };
            let Ok(header) = VideoHeader::decode(&bytes) else {
                return;
            };
            sink.deliver(
                header.speaker,
                EncodedFrame {
                    header,
                    data: bytes[VIDEO_HEADER_LEN..].to_vec(),
                },
            );
        });
    }
}

/// Sends frames, one uni stream each. Drops the frame when flow control
/// blocks (`Client/IMPLEMENTATION.md` §8).
pub struct Sender {
    conn: quinn::Connection,
    seq: u16,
}

impl Sender {
    /// For `conn`.
    pub fn new(conn: quinn::Connection) -> Sender {
        Sender { conn, seq: 0 }
    }

    /// Sends one H.264 access unit.
    pub async fn send(&mut self, keyframe: bool, ts: u32, data: &[u8]) -> Result<(), String> {
        let header = VideoHeader {
            flags: if keyframe {
                icu_proto::header::VIDEO_FLAG_KEYFRAME
            } else {
                0
            },
            speaker: 0,
            seq: self.seq,
            ts,
            codec: VIDEO_CODEC_H264,
        };
        self.seq = self.seq.wrapping_add(1);
        let mut send =
            match tokio::time::timeout(Duration::from_millis(50), self.conn.open_uni()).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return Err(e.to_string()),
                Err(_) => return Err("flow control: frame dropped".into()),
            };
        let _ = send.set_priority(-1);
        let mut buf = Vec::with_capacity(VIDEO_HEADER_LEN + data.len());
        buf.extend_from_slice(&header.encode());
        buf.extend_from_slice(data);
        send.write_all(&buf).await.map_err(|e| e.to_string())?;
        send.finish().map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Per speaker decoding state.
struct Receiver {
    #[cfg(feature = "camera")]
    decoder: Option<openh264::decoder::Decoder>,
    latest: Option<Frame>,
    got_keyframe: bool,
    last_frame: Option<Instant>,
    last_request: Option<Instant>,
    frames: u64,
}

impl Receiver {
    fn new() -> Receiver {
        Receiver {
            #[cfg(feature = "camera")]
            decoder: openh264::decoder::Decoder::new().ok(),
            latest: None,
            got_keyframe: false,
            last_frame: None,
            last_request: None,
            frames: 0,
        }
    }
}

/// All speakers' receivers.
#[derive(Default)]
pub struct Receivers {
    map: HashMap<Session, Receiver>,
}

impl Receivers {
    /// Stores and decodes a frame. Frames before the first IDR are dropped
    /// and a `KeyframeRequest` is queued.
    pub fn push(&mut self, session: Session, frame: EncodedFrame) {
        let r = self.map.entry(session).or_insert_with(Receiver::new);
        r.frames += 1;
        r.last_frame = Some(Instant::now());
        if frame.header.keyframe() {
            r.got_keyframe = true;
        }
        if !r.got_keyframe || frame.header.codec != VIDEO_CODEC_H264 {
            return;
        }
        #[cfg(not(feature = "camera"))]
        let _ = frame;
        #[cfg(feature = "camera")]
        if let Some(dec) = r.decoder.as_mut() {
            use openh264::formats::YUVSource;
            if let Ok(Some(yuv)) = dec.decode(&frame.data) {
                let (w, h) = yuv.dimensions();
                let mut rgba = vec![0u8; w * h * 4];
                yuv.write_rgba8(&mut rgba);
                r.latest = Some(Frame {
                    width: w,
                    height: h,
                    rgba,
                    at: Instant::now(),
                });
            }
        }
    }

    /// Sessions that need an IDR, rate limited to one request per second.
    pub fn take_keyframe_requests(&mut self) -> Vec<Session> {
        let now = Instant::now();
        let mut out = Vec::new();
        for (session, r) in self.map.iter_mut() {
            if r.got_keyframe || r.frames == 0 {
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

    /// Latest decoded frame per session; tiles older than 3 s are hidden.
    pub fn latest(&self) -> Vec<(Session, Frame)> {
        self.map
            .iter()
            .filter_map(|(s, r)| {
                let f = r.latest.clone()?;
                if f.at.elapsed() > Duration::from_secs(3) {
                    None
                } else {
                    Some((*s, f))
                }
            })
            .collect()
    }

    /// Frames received for a session (tests).
    pub fn frames(&self, session: Session) -> u64 {
        self.map.get(&session).map(|r| r.frames).unwrap_or(0)
    }

    /// Whether the session has delivered decodable video.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(keyframe: bool) -> EncodedFrame {
        EncodedFrame {
            header: VideoHeader {
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

    #[test]
    fn keyframe_is_requested_until_an_idr_arrives() {
        let mut r = Receivers::default();
        r.push(3, frame(false));
        assert_eq!(r.take_keyframe_requests(), vec![3]);
        assert!(r.take_keyframe_requests().is_empty(), "rate limited");
        r.push(3, frame(true));
        assert!(r.has_keyframe(3));
        assert!(r.take_keyframe_requests().is_empty());
        assert_eq!(r.frames(3), 2);
        r.remove(3);
        assert_eq!(r.frames(3), 0);
    }
}
