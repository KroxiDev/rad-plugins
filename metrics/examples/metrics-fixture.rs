//! Same plugin and SDK, deterministic data instead of proc/NVML. Never probes hardware.
use rad_plugin_metrics::{MetricsPlugin, config, polling::Sample, readings::Memory};
use std::process::ExitCode;
fn main() -> ExitCode {
    if let Some(exit) = rad_plugin_sdk::licenses_exit() {
        return exit;
    }
    let plugin = MetricsPlugin::with_reader(|| Sample {
        cpu: Some(25),
        ram: Some(Memory {
            used: 8 * 1073741824,
            available: 8 * 1073741824,
            total: 16 * 1073741824,
        }),
        gpu: Some(73),
        vram: Some(Memory {
            used: 2 * 1073741824,
            available: 6 * 1073741824,
            total: 8 * 1073741824,
        }),
    });
    match plugin {
        Ok(plugin) => match rad_plugin_sdk::run_stdio(config(), plugin) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("metrics-fixture: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("metrics-fixture: {error}");
            ExitCode::FAILURE
        }
    }
}
