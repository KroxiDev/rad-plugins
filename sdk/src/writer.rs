//! Frame output shared by every thread of the plugin.
use crate::{Correlation, Envelope, Error, Message, PROTOCOL, frame};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex, PoisonError},
};

/// Serialises whole frames onto the plugin's output; clones share one lock, so frames from the
/// dispatch loop, reply threads and the heartbeat never interleave.
#[derive(Clone)]
pub(crate) struct Writer(
    Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    Arc<Mutex<Option<Vec<String>>>>,
);

impl Writer {
    pub(crate) fn new(output: impl Write + Send + 'static) -> Self {
        Self(
            Arc::new(Mutex::new(Some(Box::new(output)))),
            Arc::new(Mutex::new(None)),
        )
    }

    pub(crate) fn negotiate(&self, capabilities: Vec<String>) {
        *self.1.lock().unwrap_or_else(PoisonError::into_inner) = Some(capabilities);
    }
    pub(crate) fn send(&self, correlation: Correlation, message: Message) -> Result<(), Error> {
        if self
            .1
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|caps| !message.permitted(caps))
        {
            return Err(Error::Protocol("unnegotiated output feature"));
        }
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
        let result = match out.as_mut() {
            Some(output) => output.write_all(&frame).and_then(|()| output.flush()),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        };
        if result.is_err() {
            *out = None;
        }
        result.map_err(|e| Error::Frame(frame::FrameError::Io(e)))
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

#[cfg(test)]
mod tests {
    use super::*;
    struct PartialFailure(Arc<Mutex<Vec<u8>>>, usize);
    impl Write for PartialFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.1 += 1;
            if self.1 == 1 {
                self.0.lock().unwrap().push(bytes[0]);
                return Ok(1);
            }
            if self.1 == 2 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn partial_frame_failure_is_terminal() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer::new(PartialFailure(bytes.clone(), 0));
        let correlation = Correlation {
            generation: 1,
            request: 2,
        };
        let message = Message::Result {
            outcome: crate::Outcome::Success,
        };
        assert!(writer.send(correlation, message.clone()).is_err());
        // The shared writer remains terminal even if a new producer attempts to send.
        assert!(writer.send(correlation, message).is_err());
        assert_eq!(bytes.lock().unwrap().len(), 1);
    }
}
