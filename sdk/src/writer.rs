//! Frame output shared by every thread of the plugin.
use crate::{Correlation, Envelope, Error, Message, PROTOCOL, frame};
use std::{
    io::Write,
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
        let frame = frame::encode(&serde_json::to_vec(&envelope)?)?;
        let mut out = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        out.write_all(&frame)
            .and_then(|()| out.flush())
            .map_err(|e| Error::Frame(frame::FrameError::Io(e)))
    }
}
