//! Reading and writing length prefixed frames on a QUIC stream
//! (`docs/reference/protocol.md` §2).

use icu_proto::{error as ec, framing};
use quinn::{RecvStream, SendStream};

use crate::error::Result;

/// Reads one frame. A bad length prefix is a `PROTOCOL_VIOLATION`.
pub async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>> {
    let mut prefix = [0u8; framing::LEN_PREFIX];
    recv.read_exact(&mut prefix).await?;
    let len = framing::frame_len(prefix);
    framing::check_len(len, max)?;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await?;
    Ok(body)
}

/// Writes one already encoded frame.
pub async fn write_frame(send: &mut SendStream, frame: &[u8]) -> Result<()> {
    send.write_all(frame).await?;
    Ok(())
}

/// Closes a connection with an ICU error code, the code's name as the reason
/// (`docs/reference/protocol.md` §6).
pub fn close(conn: &quinn::Connection, code: u32) {
    conn.close(quinn::VarInt::from_u32(code), ec::name(code).as_bytes());
}
