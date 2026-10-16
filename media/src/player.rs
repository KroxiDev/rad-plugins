//! What is asked of one MPRIS player: its name, its abilities, its state, and the commands.
use crate::bus::{Bus, Failure, Method};
use glib::{Variant, prelude::ToVariant};
use std::{collections::HashMap, time::Instant};

const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// A method of the MPRIS object of the player at `name`.
fn method<'a>(name: &'a str, interface: &'a str, method: &'a str) -> Method<'a> {
    Method {
        destination: name,
        path: PATH,
        interface,
        name: method,
    }
}

/// A property of one of the two MPRIS interfaces.
fn property(
    bus: &Bus,
    name: &str,
    interface: &str,
    property: &str,
    deadline: Instant,
) -> Result<Variant, Failure> {
    let reply = bus.call(
        &method(name, "org.freedesktop.DBus.Properties", "Get"),
        Some(&(interface, property).to_variant()),
        "(v)",
        deadline,
    )?;
    let (value,) = reply.get::<(Variant,)>().ok_or(Failure::Failed)?;
    Ok(value)
}

/// The name the player gives itself (`Identity`).
pub(crate) fn identity(bus: &Bus, name: &str, deadline: Instant) -> Result<String, Failure> {
    property(bus, name, ROOT, "Identity", deadline)?
        .get::<String>()
        .ok_or(Failure::Failed)
}

/// Whether the player says it can do something (`CanPlay`, `CanPause`, ...).
pub(crate) fn can(
    bus: &Bus,
    name: &str,
    ability: &str,
    deadline: Instant,
) -> Result<bool, Failure> {
    property(bus, name, PLAYER, ability, deadline)?
        .get::<bool>()
        .ok_or(Failure::Failed)
}

/// Asks the player to do something (`Play`, `Next`, ...).
pub(crate) fn command(
    bus: &Bus,
    name: &str,
    command: &str,
    deadline: Instant,
) -> Result<(), Failure> {
    bus.call(&method(name, PLAYER, command), None, "()", deadline)
        .map(drop)
}

/// What a player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Playing,
    Paused,
    Stopped,
}

impl Status {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "Playing" => Some(Self::Playing),
            "Paused" => Some(Self::Paused),
            "Stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// What a button shows of a player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct State {
    pub(crate) status: Status,
    /// The URL of the cover, as the player gives it.
    pub(crate) art_url: Option<String>,
}

/// The state of a player, read with one call. A player that does not say what it is doing
/// (`PlaybackStatus` missing or not one of the three values) is reported as failed.
pub(crate) fn state(bus: &Bus, name: &str, deadline: Instant) -> Result<State, Failure> {
    let reply = bus.call(
        &method(name, "org.freedesktop.DBus.Properties", "GetAll"),
        Some(&(PLAYER,).to_variant()),
        "(a{sv})",
        deadline,
    )?;
    let (properties,) = reply
        .get::<(HashMap<String, Variant>,)>()
        .ok_or(Failure::Failed)?;
    let status = properties
        .get("PlaybackStatus")
        .and_then(Variant::get::<String>)
        .and_then(|text| Status::parse(&text))
        .ok_or(Failure::Failed)?;
    let art_url = properties
        .get("Metadata")
        .and_then(Variant::get::<HashMap<String, Variant>>)
        .and_then(|metadata| metadata.get("mpris:artUrl")?.get::<String>());
    Ok(State { status, art_url })
}
