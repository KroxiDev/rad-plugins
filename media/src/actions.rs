//! Running an action: one command for one player.
use crate::{
    bus::{Bus, Failure},
    names::first_instance,
    player,
};
use rad_plugin_sdk::{FailureCode, Invoke, Outcome, is_label};
use std::time::Instant;

#[derive(Clone, Copy)]
enum Operation {
    PlayPause,
    Play,
    Pause,
    Stop,
    Previous,
    Next,
}

impl Operation {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "play_pause" => Self::PlayPause,
            "play" => Self::Play,
            "pause" => Self::Pause,
            "stop" => Self::Stop,
            "previous" => Self::Previous,
            "next" => Self::Next,
            _ => return None,
        })
    }

    /// The MPRIS method that does it.
    fn method(self) -> &'static str {
        match self {
            Self::PlayPause => "PlayPause",
            Self::Play => "Play",
            Self::Pause => "Pause",
            Self::Stop => "Stop",
            Self::Previous => "Previous",
            Self::Next => "Next",
        }
    }

    /// The property in which the player says whether it can do it. A player that says it cannot
    /// is not asked to.
    fn ability(self) -> &'static str {
        match self {
            Self::Play => "CanPlay",
            Self::Pause | Self::PlayPause => "CanPause",
            Self::Next => "CanGoNext",
            Self::Previous => "CanGoPrevious",
            // MPRIS has no property of its own for stopping.
            Self::Stop => "CanControl",
        }
    }
}

fn failure(code: FailureCode) -> Outcome {
    Outcome::Failure { code }
}

/// Runs `call` before `deadline` and reports how it went.
pub(crate) fn perform(bus: &Bus, call: &Invoke, deadline: Instant) -> Outcome {
    let (true, Some(operation), Some(app)) = (
        call.action == "control",
        call.parameters.text("operation").and_then(Operation::parse),
        call.parameters.text("player").filter(|p| is_label(p)),
    ) else {
        return failure(FailureCode::InvalidParameters);
    };
    match control(bus, operation, app, deadline) {
        Ok(()) => Outcome::Success,
        Err(why) => failure(why.code()),
    }
}

/// Asks the player of the application `app` to do `operation`. The player is the chosen
/// application's first instance on the bus, and no other: if there is none, nothing is sent.
fn control(bus: &Bus, operation: Operation, app: &str, deadline: Instant) -> Result<(), Failure> {
    let _connection = bus.connection(deadline)?;
    let names = bus.player_names(deadline)?;
    let name = first_instance(names.iter().map(String::as_str), app).ok_or(Failure::Unavailable)?;
    if !player::can(bus, name, operation.ability(), deadline)? {
        return Err(Failure::Incompatible);
    }
    player::command(bus, name, operation.method(), deadline)
}
