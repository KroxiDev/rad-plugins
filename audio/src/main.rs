//! The plugin executable: serves the host over stdin and stdout until the host closes them.
use rad_plugin_audio::{AudioPlugin, Pactl, Timing, config};
use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(exit) = rad_plugin_sdk::licenses_exit() {
        return exit;
    }
    let plugin = AudioPlugin::new(Pactl::system(), Timing::default());
    match rad_plugin_sdk::run_stdio(config(), plugin) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The host does not publish stderr; this only helps someone running the plugin by hand.
            eprintln!("rad-plugin-audio: {error}");
            ExitCode::FAILURE
        }
    }
}
