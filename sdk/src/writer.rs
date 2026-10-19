//! Frame output shared by every thread of the plugin.
use crate::{Correlation, Envelope, Error, Message, PROTOCOL, frame};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex, PoisonError},
};

/// Serialises whole frames onto the plugin's output; clones share one lock, so frames from the
/// dispatch loop, reply threads and the heartbeat never interleave.
#[derive(Clone)]
pub(crate) struct Writer(Arc<Mutex<Box<dyn Write + Send>>>);

impl Writer {
    pub(crate) fn new(output: impl Write + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Box::new(output))))
    }

    pub(crate) fn send(&self, correlation: Correlation, message: Message) -> Result<(), Error> {
        let envelope = Envelope {
            protocol: PROTOCOL,
            correlation,
            message,
        };
        let mut payload = Payload(zeroize::Zeroizing::new(Vec::new()));
        serde_json::to_writer(&mut payload, &envelope)
            .map_err(|_| Error::Frame(frame::FrameError::TooLarge(frame::MAX_PAYLOAD + 1)))?;
        let frame = zeroize::Zeroizing::new(frame::encode(&payload.0)?);
        let mut out = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        out.write_all(&frame)
            .and_then(|()| out.flush())
            .map_err(|e| Error::Frame(frame::FrameError::Io(e)))
    }
}

// Bound allocation during serialization, including escaping expansion.
struct Payload(zeroize::Zeroizing<Vec<u8>>);
impl Write for Payload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > frame::MAX_PAYLOAD - self.0.len() {
            return Err(io::ErrorKind::OutOfMemory.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
