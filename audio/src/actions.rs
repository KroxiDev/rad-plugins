//! Running an action: mute, unmute or toggle one device.
use crate::{
    Pactl, Selector,
    devices::{Kind, looks_like_option},
    pactl::RunError,
    server::{self, Failure},
};
use rad_plugin_sdk::{FailureCode, Invoke, Outcome};
use std::time::Instant;

#[derive(Clone, Copy)]
enum Operation {
    Toggle,
    Mute,
    Unmute,
}

impl Operation {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "toggle" => Some(Self::Toggle),
            "mute" => Some(Self::Mute),
            "unmute" => Some(Self::Unmute),
            _ => None,
        }
    }

    /// The value `set-sink-mute` / `set-source-mute` take.
    fn argument(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Mute => "1",
            Self::Unmute => "0",
        }
    }
}

fn failure(code: FailureCode) -> Outcome {
    Outcome::Failure { code }
}

/// Runs `call` before `deadline` and reports how it went. `pactl` is `None` when it is not
/// installed.
pub(crate) fn perform(pactl: Option<&Pactl>, call: &Invoke, deadline: Instant) -> Outcome {
    let (Some(kind), Some(operation), Some(selector)) = (
        Kind::from_definition(&call.action),
        call.parameters.text("operation").and_then(Operation::parse),
        call.parameters
            .text("device")
            .and_then(Selector::from_value),
    ) else {
        return failure(FailureCode::InvalidParameters);
    };
    if matches!(&selector, Selector::Named(name) if looks_like_option(name)) {
        return failure(FailureCode::InvalidParameters);
    }
    let Some(pactl) = pactl else {
        return failure(FailureCode::Unavailable);
    };
    match mute(pactl, kind, operation, &selector, deadline) {
        Ok(()) => Outcome::Success,
        Err(why) => failure(why.code()),
    }
}

/// Changes the mute state of the device `selector` picks. A named device must exist right now,
/// so a missing one is reported and nothing else is touched.
///
/// The output default is left to the server (`@DEFAULT_SINK@`), which keeps it right if the default
/// changes between our commands. The microphone default is resolved here first: the server's
/// default source may be a monitor, which `@DEFAULT_SOURCE@` would happily mute although it is
/// not a microphone and the visual shows it unavailable.
fn mute(
    pactl: &Pactl,
    kind: Kind,
    operation: Operation,
    selector: &Selector,
    deadline: Instant,
) -> Result<(), Failure> {
    let target = match (kind, selector) {
        (Kind::Output, Selector::Default) => kind.default_alias().to_owned(),
        (_, selector) => {
            let snapshot = server::snapshot(pactl, deadline)?;
            let device = snapshot
                .resolve(kind, selector)
                .ok_or(Failure::Unavailable)?;
            if looks_like_option(&device.name) {
                return Err(Failure::Failed);
            }
            device.name.clone()
        }
    };
    match pactl.run(
        &[kind.mute_command(), &target, operation.argument()],
        deadline,
    ) {
        Ok(_) => Ok(()),
        // The command failed: say whether it was because the server or the device is gone.
        Err(RunError::Failed) => Err(match server::snapshot(pactl, deadline) {
            Ok(snapshot) if snapshot.resolve(kind, selector).is_some() => Failure::Failed,
            Ok(_) => Failure::Unavailable,
            Err(why) => why,
        }),
        Err(other) => Err(other.into()),
    }
}
