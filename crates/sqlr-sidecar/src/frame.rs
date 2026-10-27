//! Length-prefixed frames on the sidecar's pipes (spec §6.1).
//!
//! `u32` big-endian length of what follows · `u8` frame type · `u64` big-endian request id
//! · payload. Frames above [`MAX_FRAME`] are refused; the reader never allocates the length
//! a header claims before the bytes arrive.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The most a frame's type, id and payload may hold.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;
/// A control message: MessagePack of a [`Request`](crate::Request) or
/// [`Response`](crate::Response).
pub const CONTROL: u8 = 0x01;
/// A result chunk in the [`sqlr_core::encode_chunk`] encoding.
pub const CHUNK: u8 = 0x02;
/// A document chunk in the [`sqlr_core::encode_doc_chunk`] encoding (reserved: protocol 1
/// serves relational drivers).
pub const DOC_CHUNK: u8 = 0x03;
/// A line of the driver's log, UTF-8.
pub const LOG: u8 = 0x04;

/// Bytes of type and request id before the payload.
const HEADER: usize = 1 + 8;

/// Why reading or writing a frame failed.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The pipe failed.
    #[error("pipe error: {0}")]
    Io(#[from] std::io::Error),
    /// The frame is above `MAX_FRAME`; carries its length.
    #[error("frame of {0} bytes exceeds the {MAX_FRAME}-byte limit")]
    TooLarge(usize),
    /// The frame is shorter than its header; carries its length.
    #[error("frame of {0} bytes is shorter than its header")]
    TooShort(usize),
    /// The frame type byte is not one of the known types.
    #[error("unknown frame type {0:#04x}")]
    UnknownType(u8),
    /// The pipe or the buffer ended inside a frame.
    #[error("the pipe closed inside a frame")]
    Truncated,
}

/// One message of the sidecar pipes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The frame type: [`CONTROL`], [`CHUNK`], [`DOC_CHUNK`] or [`LOG`].
    pub ty: u8,
    /// The id of the request the frame belongs to.
    pub id: u64,
    /// The frame's content, as the type defines.
    pub payload: Vec<u8>,
}

fn known(ty: u8) -> bool {
    matches!(ty, CONTROL | CHUNK | DOC_CHUNK | LOG)
}

/// Appends the frame to `out`.
pub fn encode_frame(ty: u8, id: u64, payload: &[u8], out: &mut Vec<u8>) -> Result<(), FrameError> {
    let len = HEADER + payload.len();
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(ty);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

/// Decodes one frame from the start of `bytes` (length prefix included); the second value
/// is how many bytes it used.
pub fn decode_frame(bytes: &[u8]) -> Result<(Frame, usize), FrameError> {
    let head: [u8; 4] = bytes
        .get(..4)
        .and_then(|b| b.try_into().ok())
        .ok_or(FrameError::Truncated)?;
    let len = u32::from_be_bytes(head) as usize;
    check_len(len)?;
    let body = bytes.get(4..4 + len).ok_or(FrameError::Truncated)?;
    Ok((parse_body(body)?, 4 + len))
}

fn check_len(len: usize) -> Result<(), FrameError> {
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    if len < HEADER {
        return Err(FrameError::TooShort(len));
    }
    Ok(())
}

fn parse_body(body: &[u8]) -> Result<Frame, FrameError> {
    let ty = body[0];
    if !known(ty) {
        return Err(FrameError::UnknownType(ty));
    }
    let id = u64::from_be_bytes(body[1..HEADER].try_into().expect("8 bytes"));
    Ok(Frame {
        ty,
        id,
        payload: body[HEADER..].to_vec(),
    })
}

/// Writes one frame to `w` and flushes it.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    ty: u8,
    id: u64,
    payload: &[u8],
) -> Result<(), FrameError> {
    let mut buf = Vec::with_capacity(4 + HEADER + payload.len());
    encode_frame(ty, id, payload, &mut buf)?;
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

/// Reads the next frame; `Ok(None)` when the pipe closed between frames.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Frame>, FrameError> {
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < head.len() {
        let n = r.read(&mut head[got..]).await?;
        if n == 0 {
            return if got == 0 {
                Ok(None)
            } else {
                Err(FrameError::Truncated)
            };
        }
        got += n;
    }
    let len = u32::from_be_bytes(head) as usize;
    check_len(len)?;
    // `take` grows the buffer as bytes arrive: a lying header costs nothing up front.
    let mut body = Vec::new();
    let read = r.take(len as u64).read_to_end(&mut body).await?;
    if read < len {
        return Err(FrameError::Truncated);
    }
    parse_body(&body).map(Some)
}
