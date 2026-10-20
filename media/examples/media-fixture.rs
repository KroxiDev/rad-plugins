//! Private-bus entrypoint for the generic checker; never falls back to a desktop bus.
use rad_plugin_media::{MediaPlugin, Timing, config};
fn main() {
    let bus = std::env::args()
        .nth(1)
        .expect("explicit private bus address");
    assert!(bus.starts_with("unix:path="));
    let plugin = MediaPlugin::new(Some(bus), Timing::default());
    if rad_plugin_sdk::run_stdio(config(), plugin).is_err() {
        std::process::exit(1);
    }
}
