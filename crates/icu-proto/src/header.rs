//! Audio datagram and video stream headers (`docs/reference/protocol.md` §3, §4).
//!
//! Both headers are little-endian and carry their protocol version in `kind`.
//! Reserved bits are written as 0 by the sender and **ignored** by the
//! receiver — a decoder never rejects a header because of them, otherwise no
//! additive minor version would be possible.

use crate::{ProtoError, Session};

/// `kind` value of an audio v1 datagram.
pub const KIND_AUDIO_V1: u8 = 0x01;
/// `kind` value of a video v1 stream.
pub const KIND_VIDEO_V1: u8 = 0x02;

/// Audio header size in bytes.
pub const AUDIO_HEADER_LEN: usize = 14;
/// Smallest valid audio datagram: header plus one Opus byte.
pub const AUDIO_MIN_DATAGRAM: usize = AUDIO_HEADER_LEN + 1;
/// Largest Opus payload in one datagram.
pub const AUDIO_MAX_PAYLOAD: usize = 400;
/// Largest valid audio datagram.
pub const AUDIO_MAX_DATAGRAM: usize = AUDIO_HEADER_LEN + AUDIO_MAX_PAYLOAD;

/// Video header size in bytes.
pub const VIDEO_HEADER_LEN: usize = 12;
/// Largest video frame payload (`docs/reference/protocol.md` §4, §7).
pub const VIDEO_MAX_FRAME: usize = 512 * 1024;
/// Largest picture a receiver decodes, in pixels (`docs/reference/protocol.md` §4): twice
/// the 320×240 profile in each direction, headroom for a later profile. A
/// frame whose SPS asks for more is refused before the decoder sees it.
pub const VIDEO_MAX_WIDTH: usize = 640;
/// See [`VIDEO_MAX_WIDTH`].
pub const VIDEO_MAX_HEIGHT: usize = 480;
/// Most reference frames an SPS may ask for; the decoder's picture buffer
/// grows with it. The encoder uses one.
pub const VIDEO_MAX_REF_FRAMES: u32 = 4;

/// bit0 of the audio `flags`: last packet of a talk spurt.
pub const AUDIO_FLAG_END: u8 = 1 << 0;
/// bit0 of the video `flags`: the frame is an IDR.
pub const VIDEO_FLAG_KEYFRAME: u8 = 1 << 0;

/// `codec = 1`: H.264 Constrained Baseline, Annex-B NALs.
pub const VIDEO_CODEC_H264: u8 = 1;
/// `codec = 2`: MJPEG. Receive/debug path only, never sent in v1.
pub const VIDEO_CODEC_MJPEG: u8 = 2;

/// Byte offset of the `speaker` field, identical in both headers.
pub const SPEAKER_OFFSET: usize = 2;

/// Audio datagram header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioHeader {
    /// bit0 `END`, bit1–7 reserved.
    pub flags: u8,
    /// Session of the speaker. Clients send 0, the server overwrites in place.
    pub speaker: Session,
    /// Per sender packet counter, wraps.
    pub seq: u16,
    /// 48 kHz sample clock of the sender, wraps.
    pub ts: u32,
    /// Applied sender gain generation (protocol minor 6).
    pub gain_epoch: u32,
}

impl AudioHeader {
    /// Whether this is the last packet of a talk spurt.
    pub fn end(&self) -> bool {
        self.flags & AUDIO_FLAG_END != 0
    }

    /// Serializes the header into a fixed size array.
    pub fn encode(&self) -> [u8; AUDIO_HEADER_LEN] {
        let mut out = [0u8; AUDIO_HEADER_LEN];
        out[0] = KIND_AUDIO_V1;
        out[1] = self.flags;
        out[2..4].copy_from_slice(&self.speaker.to_le_bytes());
        out[4..6].copy_from_slice(&self.seq.to_le_bytes());
        out[6..10].copy_from_slice(&self.ts.to_le_bytes());
        out[10..14].copy_from_slice(&self.gain_epoch.to_le_bytes());
        out
    }

    /// Builds a complete datagram from header and Opus payload.
    pub fn datagram(&self, opus: &[u8]) -> Result<Vec<u8>, ProtoError> {
        if opus.is_empty() || opus.len() > AUDIO_MAX_PAYLOAD {
            return Err(ProtoError::BadPayloadLen(opus.len()));
        }
        let mut out = Vec::with_capacity(AUDIO_HEADER_LEN + opus.len());
        out.extend_from_slice(&self.encode());
        out.extend_from_slice(opus);
        Ok(out)
    }

    /// Parses a datagram and returns header plus the Opus payload slice.
    ///
    /// Enforces the length window from `docs/reference/protocol.md` §3: the server drops
    /// datagrams below 15 or above 414 bytes.
    pub fn decode(buf: &[u8]) -> Result<(AudioHeader, &[u8]), ProtoError> {
        if buf.len() < AUDIO_MIN_DATAGRAM || buf.len() > AUDIO_MAX_DATAGRAM {
            return Err(ProtoError::BadPayloadLen(buf.len()));
        }
        if buf[0] != KIND_AUDIO_V1 {
            return Err(ProtoError::BadKind(buf[0]));
        }
        let header = AudioHeader {
            flags: buf[1],
            speaker: u16::from_le_bytes([buf[2], buf[3]]),
            seq: u16::from_le_bytes([buf[4], buf[5]]),
            ts: u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]),
            gain_epoch: u32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]]),
        };
        Ok((header, &buf[AUDIO_HEADER_LEN..]))
    }
}

/// Video frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    /// bit0 `KEYFRAME`, bit1–7 reserved.
    pub flags: u8,
    /// Session of the speaker. Clients send 0, the server overwrites.
    pub speaker: Session,
    /// Per sender frame counter, wraps.
    pub seq: u16,
    /// Same 48 kHz clock as audio.
    pub ts: u32,
    /// [`VIDEO_CODEC_H264`] or [`VIDEO_CODEC_MJPEG`].
    pub codec: u8,
}

impl Default for VideoHeader {
    fn default() -> Self {
        VideoHeader {
            flags: 0,
            speaker: 0,
            seq: 0,
            ts: 0,
            codec: VIDEO_CODEC_H264,
        }
    }
}

impl VideoHeader {
    /// Whether the frame is an IDR.
    pub fn keyframe(&self) -> bool {
        self.flags & VIDEO_FLAG_KEYFRAME != 0
    }

    /// Serializes the header. Byte 11 is reserved and written as 0.
    pub fn encode(&self) -> [u8; VIDEO_HEADER_LEN] {
        let mut out = [0u8; VIDEO_HEADER_LEN];
        out[0] = KIND_VIDEO_V1;
        out[1] = self.flags;
        out[2..4].copy_from_slice(&self.speaker.to_le_bytes());
        out[4..6].copy_from_slice(&self.seq.to_le_bytes());
        out[6..10].copy_from_slice(&self.ts.to_le_bytes());
        out[10] = self.codec;
        out[11] = 0;
        out
    }

    /// Parses a video header. Byte 11 is ignored, whatever it contains.
    pub fn decode(buf: &[u8]) -> Result<VideoHeader, ProtoError> {
        if buf.len() < VIDEO_HEADER_LEN {
            return Err(ProtoError::ShortHeader(buf.len()));
        }
        if buf[0] != KIND_VIDEO_V1 {
            return Err(ProtoError::BadKind(buf[0]));
        }
        Ok(VideoHeader {
            flags: buf[1],
            speaker: u16::from_le_bytes([buf[2], buf[3]]),
            seq: u16::from_le_bytes([buf[4], buf[5]]),
            ts: u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]),
            codec: buf[10],
        })
    }
}

/// Overwrites the `speaker` field of an already serialized header in place.
///
/// Works for both header kinds because the field sits at the same offset. The
/// server uses this so relaying costs no copy (`docs/reference/protocol.md` §3).
pub fn set_speaker_in_place(buf: &mut [u8], speaker: Session) -> Result<(), ProtoError> {
    if buf.len() < SPEAKER_OFFSET + 2 {
        return Err(ProtoError::ShortHeader(buf.len()));
    }
    buf[SPEAKER_OFFSET..SPEAKER_OFFSET + 2].copy_from_slice(&speaker.to_le_bytes());
    Ok(())
}

/// Signed difference of two `seq` values modulo 2^16 (`docs/reference/protocol.md` §3).
///
/// A difference beyond half the value range is a step backwards, not a huge
/// step forwards.
pub fn seq_diff(a: u16, b: u16) -> i16 {
    a.wrapping_sub(b) as i16
}

/// Signed difference of two `ts` values modulo 2^32 (`docs/reference/protocol.md` §3).
pub fn ts_diff(a: u32, b: u32) -> i32 {
    a.wrapping_sub(b) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §10: header encode/decode roundtrip.
    #[test]
    fn audio_header_roundtrip() {
        for header in [
            AudioHeader::default(),
            AudioHeader {
                gain_epoch: 0,
                flags: AUDIO_FLAG_END,
                speaker: 0xBEEF,
                seq: 0xCAFE,
                ts: 0xDEAD_BEEF,
            },
            AudioHeader {
                gain_epoch: 0,
                flags: 0,
                speaker: u16::MAX,
                seq: u16::MAX,
                ts: u32::MAX,
            },
        ] {
            let dg = header.datagram(&[0x78; 40]).unwrap();
            assert_eq!(dg.len(), AUDIO_HEADER_LEN + 40);
            let (back, payload) = AudioHeader::decode(&dg).unwrap();
            assert_eq!(back, header);
            assert_eq!(payload, &[0x78; 40]);
        }
        assert!(AudioHeader {
            flags: AUDIO_FLAG_END,
            ..Default::default()
        }
        .end());
    }

    #[test]
    fn video_header_roundtrip_and_reserved_byte_ignored() {
        let header = VideoHeader {
            flags: VIDEO_FLAG_KEYFRAME,
            speaker: 3,
            seq: 1234,
            ts: 96_000,
            codec: VIDEO_CODEC_H264,
        };
        let mut buf = header.encode().to_vec();
        assert_eq!(buf[11], 0, "sender writes the reserved byte as 0");
        assert_eq!(VideoHeader::decode(&buf).unwrap(), header);
        // Reserved byte and reserved flag bits must not make a receiver reject.
        buf[11] = 0xFF;
        buf[1] |= 0xFE;
        let back = VideoHeader::decode(&buf).unwrap();
        assert!(back.keyframe());
        assert_eq!(back.speaker, 3);
        assert!(header.keyframe());
    }

    #[test]
    fn reserved_audio_flag_bits_are_ignored() {
        let mut dg = AudioHeader::default().datagram(&[1, 2, 3]).unwrap();
        dg[1] = 0xFE; // all reserved bits set, END clear
        let (header, payload) = AudioHeader::decode(&dg).unwrap();
        assert!(!header.end());
        assert_eq!(payload, &[1, 2, 3]);
    }

    /// `docs/reference/protocol.md` §3: drop below 15 and above 414 bytes.
    #[test]
    fn audio_length_window() {
        let ok_min = AudioHeader::default().datagram(&[0]).unwrap();
        assert_eq!(ok_min.len(), 15);
        assert!(AudioHeader::decode(&ok_min).is_ok());

        let ok_max = AudioHeader::default()
            .datagram(&[0; AUDIO_MAX_PAYLOAD])
            .unwrap();
        assert_eq!(ok_max.len(), 414);
        assert!(AudioHeader::decode(&ok_max).is_ok());

        assert!(matches!(
            AudioHeader::decode(&ok_min[..10]),
            Err(ProtoError::BadPayloadLen(10))
        ));
        let mut too_big = ok_max.clone();
        too_big.push(0);
        assert!(matches!(
            AudioHeader::decode(&too_big),
            Err(ProtoError::BadPayloadLen(415))
        ));
        assert!(matches!(
            AudioHeader::default().datagram(&[]),
            Err(ProtoError::BadPayloadLen(0))
        ));
        assert!(matches!(
            AudioHeader::default().datagram(&[0; AUDIO_MAX_PAYLOAD + 1]),
            Err(ProtoError::BadPayloadLen(401))
        ));
    }

    #[test]
    fn wrong_kind_is_rejected() {
        let mut dg = AudioHeader::default().datagram(&[1]).unwrap();
        dg[0] = 0x03; // a hypothetical audio v2
        assert!(matches!(
            AudioHeader::decode(&dg),
            Err(ProtoError::BadKind(0x03))
        ));
        let mut v = VideoHeader::default().encode();
        v[0] = KIND_AUDIO_V1;
        assert!(matches!(
            VideoHeader::decode(&v),
            Err(ProtoError::BadKind(0x01))
        ));
        assert!(matches!(
            VideoHeader::decode(&v[..11]),
            Err(ProtoError::ShortHeader(11))
        ));
    }

    #[test]
    fn speaker_overwrite_in_place() {
        let mut dg = AudioHeader {
            gain_epoch: 0,
            speaker: 0,
            seq: 42,
            ts: 960,
            flags: 0,
        }
        .datagram(&[9; 20])
        .unwrap();
        set_speaker_in_place(&mut dg, 7).unwrap();
        let (header, payload) = AudioHeader::decode(&dg).unwrap();
        assert_eq!(header.speaker, 7);
        assert_eq!((header.seq, header.ts), (42, 960));
        assert_eq!(payload, &[9; 20]);

        let mut vid = VideoHeader::default().encode().to_vec();
        set_speaker_in_place(&mut vid, 9).unwrap();
        assert_eq!(VideoHeader::decode(&vid).unwrap().speaker, 9);

        assert!(matches!(
            set_speaker_in_place(&mut [0u8; 3], 1),
            Err(ProtoError::ShortHeader(3))
        ));
    }

    /// §10: wrap tests across the boundary.
    #[test]
    fn seq_wraps_at_2_16() {
        // 20 ms frames wrap after ~22 min.
        assert_eq!(seq_diff(1, 0), 1);
        assert_eq!(seq_diff(0, 1), -1);
        assert_eq!(seq_diff(0, u16::MAX), 1, "0 follows 65535");
        assert_eq!(seq_diff(u16::MAX, 0), -1);
        assert_eq!(seq_diff(5, u16::MAX - 4), 10);
        assert!(seq_diff(3, u16::MAX) > 0);
        assert!(seq_diff(u16::MAX, 3) <= 0);
        assert_eq!(seq_diff(7, 7), 0);
        assert!(seq_diff(7, 7) <= 0);
        // Exactly half the range: 0x8000 is the ambiguous point and comes out
        // negative, which is the documented "step backwards" reading.
        assert_eq!(seq_diff(0x8000, 0), i16::MIN);
        assert!(seq_diff(0x8000, 0) <= 0);
    }

    #[test]
    fn ts_wraps_at_2_32() {
        // 48 kHz wraps after ~24.8 h.
        assert_eq!(ts_diff(960, 0), 960);
        assert_eq!(ts_diff(0, 960), -960);
        assert_eq!(ts_diff(479, u32::MAX - 480), 960, "960 samples across wrap");
        assert!(ts_diff(479, u32::MAX - 480) > 0);
        assert!(ts_diff(u32::MAX - 480, 479) <= 0);
        assert_eq!(ts_diff(0x8000_0000, 0), i32::MIN);
        assert!(ts_diff(0x8000_0000, 0) <= 0);
    }

    /// A full talk spurt across the seq wrap stays gapless.
    #[test]
    fn talk_spurt_across_the_wrap_has_no_gap() {
        let mut seq = u16::MAX - 2;
        let mut ts = u32::MAX - 2 * 960;
        let mut prev: Option<(u16, u32)> = None;
        for _ in 0..6 {
            if let Some((pseq, pts)) = prev {
                assert_eq!(seq_diff(seq, pseq), 1, "no gap across the wrap");
                assert_eq!(ts_diff(ts, pts), 960);
            }
            prev = Some((seq, ts));
            seq = seq.wrapping_add(1);
            ts = ts.wrapping_add(960);
        }
    }
}
