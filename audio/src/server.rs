//! What the plugin asks of the sound server, built on [`Pactl`].
use crate::{
    Pactl, Snapshot,
    devices::{Kind, parse_defaults, parse_devices},
    pactl::RunError,
};
use rad_plugin_sdk::FailureCode;
use std::time::Instant;

/// Why the server could not be asked, in the terms the plugin reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// No `pactl`, or no server behind it.
    Unavailable,
    TimedOut,
    Failed,
}

impl Failure {
    pub(crate) fn code(self) -> FailureCode {
        match self {
            Self::Unavailable => FailureCode::Unavailable,
            Self::TimedOut => FailureCode::TimedOut,
            Self::Failed => FailureCode::Failed,
        }
    }
}

impl From<RunError> for Failure {
    fn from(error: RunError) -> Self {
        match error {
            // `pactl` exits non-zero when it cannot reach the server.
            RunError::Spawn(_) | RunError::Failed => Self::Unavailable,
            RunError::TimedOut => Self::TimedOut,
            RunError::TooMuchOutput | RunError::Io(_) => Self::Failed,
        }
    }
}

/// Reads sinks, sources and the defaults, all before `deadline`.
pub(crate) fn snapshot(pactl: &Pactl, deadline: Instant) -> Result<Snapshot, Failure> {
    let sinks = pactl.run(&["-f", "json", "list", "sinks"], deadline)?;
    let sources = pactl.run(&["-f", "json", "list", "sources"], deadline)?;
    let info = pactl.run(&["-f", "json", "info"], deadline)?;
    let parse = |_| Failure::Failed;
    let (default_sink, default_source) = parse_defaults(&info).map_err(parse)?;
    Ok(Snapshot {
        sinks: parse_devices(&sinks, Kind::Output).map_err(parse)?,
        sources: parse_devices(&sources, Kind::Microphone).map_err(parse)?,
        default_sink,
        default_source,
    })
}
