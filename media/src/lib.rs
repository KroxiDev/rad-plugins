//! RAD Action Deck media plugin: control a media player and show its cover through MPRIS.
mod actions;
mod address;
mod bus;
pub mod cover;
mod monitor;
pub mod names;
mod options;
mod player;
mod plugin;
mod timing;
mod visual;
mod wake;
mod watch;

pub use address::session_bus_address;
pub use plugin::{MediaPlugin, config};
pub use timing::Timing;
