//! Reading and writing length prefixed frames on a QUIC stream
//! (`PROTOCOL.md` §2).

use icu_proto::{framing, ProtoError};
use quinn::{RecvStream, SendStream};

use crate::error::{Error, Result};

/// Reads one frame. A bad length prefix is a `PROTOCOL_VIOLATION`.
pub async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>> {
    let mut prefix = [0u8; framing::LEN_PREFIX];
    recv.read_exact(&mut prefix).await?;
    let len = framing::frame_len(prefix);
    framing::check_len(len, max).map_err(|e: ProtoError| {
        Error::coded(icu_proto::error::PROTOCOL_VIOLATION, e.to_string())
    })?;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await?;
    Ok(body)
}

/// Writes one already encoded frame.
pub async fn write_frame(send: &mut SendStream, frame: &[u8]) -> Result<()> {
    send.write_all(frame).await?;
    Ok(())
}
