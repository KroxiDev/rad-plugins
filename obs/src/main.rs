fn main() -> std::process::ExitCode {
    if let Some(exit) = rad_plugin_sdk::licenses_exit() {
        return exit;
    }
    if rad_plugin_obs::run_host(rad_plugin_obs::ObsPlugin::new()).is_err() {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
