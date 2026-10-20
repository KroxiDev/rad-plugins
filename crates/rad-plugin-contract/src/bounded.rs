use crate::Error;
use serde::Serialize;
use std::io::{self, Write};

// Cap output while serializing, not after allocating an arbitrarily large buffer.
pub(crate) fn json(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, Error> {
    struct Sink {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Drop for Sink {
        fn drop(&mut self) {
            use zeroize::Zeroize;
            self.bytes.zeroize();
        }
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit - self.bytes.len() {
                return Err(io::ErrorKind::OutOfMemory.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut sink, value).map_err(|_| Error::LimitExceeded)?;
    rad_wire::parse_json(&sink.bytes).map_err(|_| Error::InvalidDocument)?;
    Ok(std::mem::take(&mut sink.bytes))
}
