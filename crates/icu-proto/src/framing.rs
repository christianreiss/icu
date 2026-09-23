//! Length prefixed framing for the control stream
//! (`PROTOCOL.md` §2).
//!
//! ```text
//! offset 0  size 2    len  u16 LE, 1 <= len <= MAX
//! offset 2  size len  body postcard serialization
//! ```

use serde::{de::DeserializeOwned, Serialize};

use crate::ProtoError;

/// Size of the length prefix in bytes.
pub const LEN_PREFIX: usize = 2;

/// Maximum body size of a control frame (`PROTOCOL.md` §2, §7).
pub const MAX_CONTROL_FRAME: usize = 16 * 1024;

/// Encodes `msg` into a complete frame (prefix + body).
///
/// `max` is the applicable body limit, normally [`MAX_CONTROL_FRAME`].
pub fn encode_frame<T: Serialize>(msg: &T, max: usize) -> Result<Vec<u8>, ProtoError> {
    let body = postcard::to_stdvec(msg).map_err(ProtoError::Encode)?;
    check_len(body.len(), max)?;
    let mut out = Vec::with_capacity(LEN_PREFIX + body.len());
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decodes a frame body. Unknown enum variants surface as
/// [`ProtoError::UnknownVariant`].
pub fn decode_body<T: DeserializeOwned>(body: &[u8], max: usize) -> Result<T, ProtoError> {
    check_len(body.len(), max)?;
    postcard::from_bytes(body).map_err(|e| match e {
        // postcard 1.1.3 reports an unknown enum discriminant as
        // `SerdeDeCustom`; `DeserializeBadEnum` is dead code there (A24,
        // Spike 2b).
        postcard::Error::SerdeDeCustom => ProtoError::UnknownVariant,
        other => ProtoError::Decode(other),
    })
}

/// Validates a length prefix read off the wire.
pub fn check_len(len: usize, max: usize) -> Result<(), ProtoError> {
    if len == 0 {
        return Err(ProtoError::EmptyFrame);
    }
    if len > max {
        return Err(ProtoError::FrameTooLarge { len, max });
    }
    Ok(())
}

/// Reads the length prefix from the first two bytes of a frame.
pub fn frame_len(prefix: [u8; LEN_PREFIX]) -> usize {
    u16::from_le_bytes(prefix) as usize
}

/// Splits one complete frame off the front of `buf`.
///
/// Returns `Ok(None)` when `buf` does not hold a full frame yet, and
/// `Ok(Some((body_range_end, body)))` otherwise, where the caller drops
/// `LEN_PREFIX + body.len()` bytes.
pub fn split_frame(buf: &[u8], max: usize) -> Result<Option<&[u8]>, ProtoError> {
    if buf.len() < LEN_PREFIX {
        return Ok(None);
    }
    let len = frame_len([buf[0], buf[1]]);
    check_len(len, max)?;
    if buf.len() < LEN_PREFIX + len {
        return Ok(None);
    }
    Ok(Some(&buf[LEN_PREFIX..LEN_PREFIX + len]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlMsg, UserFlags};

    #[test]
    fn roundtrip_through_frame() {
        let msg = ControlMsg::Ping { nonce: 0xDEAD_BEEF };
        let frame = encode_frame(&msg, MAX_CONTROL_FRAME).unwrap();
        let body = split_frame(&frame, MAX_CONTROL_FRAME).unwrap().unwrap();
        assert_eq!(body.len(), frame.len() - LEN_PREFIX);
        let back: ControlMsg = decode_body(body, MAX_CONTROL_FRAME).unwrap();
        assert_eq!(back, msg);
    }

    /// `PROTOCOL.md` §2: `len = 0` is a protocol violation.
    #[test]
    fn frame_len_zero_is_rejected() {
        let err = check_len(0, MAX_CONTROL_FRAME).unwrap_err();
        assert!(matches!(err, ProtoError::EmptyFrame));
        assert_eq!(err.error_code(), crate::error::PROTOCOL_VIOLATION);
        assert!(matches!(
            split_frame(&[0x00, 0x00], MAX_CONTROL_FRAME),
            Err(ProtoError::EmptyFrame)
        ));
    }

    /// `PROTOCOL.md` §2: 16384 is the last accepted length, 16385 is not.
    #[test]
    fn frame_len_boundaries_16384_and_16385() {
        assert!(check_len(16384, MAX_CONTROL_FRAME).is_ok());
        let err = check_len(16385, MAX_CONTROL_FRAME).unwrap_err();
        assert!(matches!(
            err,
            ProtoError::FrameTooLarge {
                len: 16385,
                max: 16384
            }
        ));
        assert_eq!(err.error_code(), crate::error::PROTOCOL_VIOLATION);

        // And through the prefix path: 0x4001 = 16385 LE.
        let mut buf = vec![0x01, 0x40];
        buf.resize(2 + 16385, 0);
        assert!(matches!(
            split_frame(&buf, MAX_CONTROL_FRAME),
            Err(ProtoError::FrameTooLarge { .. })
        ));

        // 16384 passes the length check (prefix 0x4000 LE).
        let mut buf = vec![0x00, 0x40];
        buf.resize(2 + 16384, 0);
        assert!(split_frame(&buf, MAX_CONTROL_FRAME).unwrap().is_some());
    }

    #[test]
    fn oversized_message_fails_to_encode() {
        let msg = ControlMsg::Error {
            code: 1,
            detail: "x".repeat(MAX_CONTROL_FRAME + 10),
        };
        assert!(matches!(
            encode_frame(&msg, MAX_CONTROL_FRAME),
            Err(ProtoError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn partial_frames_return_none() {
        let frame = encode_frame(
            &ControlMsg::SetState {
                flags: UserFlags(UserFlags::MUTED),
            },
            MAX_CONTROL_FRAME,
        )
        .unwrap();
        assert!(split_frame(&frame[..1], MAX_CONTROL_FRAME)
            .unwrap()
            .is_none());
        assert!(split_frame(&frame[..frame.len() - 1], MAX_CONTROL_FRAME)
            .unwrap()
            .is_none());
        assert!(split_frame(&frame, MAX_CONTROL_FRAME).unwrap().is_some());
    }
}
