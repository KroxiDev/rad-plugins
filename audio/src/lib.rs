//! RAD Action Deck audio plugin: mute, unmute and toggle the microphone and the audio output.
mod actions;
pub mod devices;
mod monitor;
mod pactl;
mod plugin;
mod server;
#[cfg(test)]
mod test_support;
mod timing;
mod watch;

pub use devices::{Device, Kind, Selector, Snapshot};
pub use pactl::Pactl;
pub use plugin::{AudioPlugin, config};
pub use timing::Timing;
