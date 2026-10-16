use crate::frame::FrameError;
use std::fmt;

/// Why [`crate::run`] stopped. A clean end of the host's stream is not an error.
#[derive(Debug)]
pub enum Error {
    Frame(FrameError),
    Json(serde_json::Error),
    /// The host broke the protocol (wrong first message, wrong protocol version, ...).
    Protocol(&'static str),
    /// The host's API is outside the range the plugin declared; nothing was answered.
    IncompatibleHost {
        requested: u32,
        min: u32,
        max: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(e) => e.fmt(f),
            Self::Json(e) => write!(f, "invalid message: {e}"),
            Self::Protocol(why) => write!(f, "protocol violation: {why}"),
            Self::IncompatibleHost {
                requested,
                min,
                max,
            } => {
                write!(
                    f,
                    "host API {requested} is outside the supported range {min}..={max}"
                )
            }
        }
    }
}
impl std::error::Error for Error {}
impl From<FrameError> for Error {
    fn from(e: FrameError) -> Self {
        Self::Frame(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
