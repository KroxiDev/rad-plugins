//! Shared, bounded metrics for RAD visual sources.
pub mod nvml;
mod plugin;
pub mod polling;
pub mod readings;
pub use plugin::{MetricsPlugin, config};
pub mod reader;

impl MetricsPlugin {
    /// Production reader; initialization and every call stay on the isolated worker.
    pub fn system() -> std::io::Result<Self> {
        Self::with_poller(polling::Poller::with_demand_factory(|| {
            // SAFETY: fixed NVIDIA library soname with the documented vendor ABI.
            let mut reader = reader::Reader::with_nvml_loader("/proc".into(), || unsafe {
                nvml::Nvml::load(c"libnvidia-ml.so.1")
            });
            move |demand, sample| reader.read_demand(demand, sample)
        })?)
    }
}
