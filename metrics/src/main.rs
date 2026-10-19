use rad_plugin_metrics::{MetricsPlugin, config};
use std::process::ExitCode;
fn main() -> ExitCode {
    if let Some(exit) = rad_plugin_sdk::licenses_exit() {
        return exit;
    }
    let plugin = match MetricsPlugin::system() {
        Ok(plugin) => plugin,
        Err(error) => {
            eprintln!("rad-plugin-metrics: {error}");
            return ExitCode::FAILURE;
        }
    };
    match rad_plugin_sdk::run_stdio(config(), plugin) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rad-plugin-metrics: {error}");
            ExitCode::FAILURE
        }
    }
}
