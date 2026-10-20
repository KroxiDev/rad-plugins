//! Uses the SDK bounded terminal stdout path shared by all plugins.
pub fn run_host(plugin: crate::ObsPlugin) -> Result<(), rad_plugin_sdk::Error> {
    rad_plugin_sdk::run_stdio(crate::config(), plugin)
}
