//! Wire framing for the plugin pipes: a u32 big-endian length, then 1..=65536 payload bytes.
use std::{
    fmt,
    io::{self, Read, Write},
};

/// Largest payload a frame may carry; the host closes a plugin that exceeds it.
pub const MAX_PAYLOAD: usize = 65_536;

#[derive(Debug)]
pub enum FrameError {
    /// A zero-length payload.
    Empty,
    /// A payload longer than [`MAX_PAYLOAD`], with the length that was declared or offered.
    TooLarge(usize),
    /// The stream ended inside a frame.
    Truncated,
    Io(io::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty frame"),
            Self::TooLarge(n) => write!(f, "frame of {n} bytes exceeds {MAX_PAYLOAD}"),
            Self::Truncated => f.write_str("stream ended inside a frame"),
            Self::Io(e) => write!(f, "frame i/o error: {e}"),
        }
    }
}
impl std::error::Error for FrameError {}

fn check_len(len: usize) -> Result<(), FrameError> {
    match len {
        0 => Err(FrameError::Empty),
        n if n > MAX_PAYLOAD => Err(FrameError::TooLarge(n)),
        _ => Ok(()),
    }
}

/// The complete frame (header and payload) as one buffer, so a single write keeps it contiguous.
pub fn encode(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    check_len(payload.len())?;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Writes one frame and flushes. A refused payload writes nothing.
pub fn write<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    let frame = encode(payload)?;
    writer.write_all(&frame).map_err(FrameError::Io)?;
    writer.flush().map_err(FrameError::Io)
}

/// Reads one frame. `Ok(None)` is a clean end of stream between frames; the length is checked
/// before the payload buffer is allocated.
pub fn read<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    let len = u32::from_be_bytes(header) as usize;
    check_len(len)?;
    let mut payload = zeroize::Zeroizing::new(vec![0u8; len]);
    reader
        .read_exact(&mut payload)
        .map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => FrameError::Truncated,
            _ => FrameError::Io(e),
        })?;
    Ok(Some(std::mem::take(&mut *payload)))
}
