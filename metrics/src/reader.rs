//! All potentially blocking I/O is confined to the polling worker.
use crate::{
    nvml::Nvml,
    polling::{Demand, Sample},
    readings::{Cpu, Memory, read_bounded},
};
use std::{fs::File, path::PathBuf};
pub struct Reader {
    proc_root: PathBuf,
    demand: Demand,
    nvml: Option<Nvml>,
    previous_cpu: Option<Cpu>,
    loader: Option<Box<dyn FnMut() -> Option<Nvml>>>,
}
impl Reader {
    /// Explicit paths and adapter permit controlled fixtures without system probes.
    pub fn new(proc_root: PathBuf, nvml: Option<Nvml>) -> Self {
        Self {
            proc_root,
            demand: Demand::default(),
            nvml,
            previous_cpu: None,
            loader: None,
        }
    }
    pub fn with_nvml_loader(
        proc_root: PathBuf,
        load: impl FnMut() -> Option<Nvml> + 'static,
    ) -> Self {
        let mut reader = Self::new(proc_root, None);
        reader.loader = Some(Box::new(load));
        reader
    }
    pub fn read(&mut self) -> Sample {
        self.read_demand(
            Demand {
                sources: [true; 4],
                ..Default::default()
            },
            true,
        )
    }
    pub(crate) fn read_demand(&mut self, demand: Demand, sample: bool) -> Sample {
        if !demand.sources[0] || demand.cpu_epoch != self.demand.cpu_epoch {
            self.previous_cpu = None;
        }
        let needs_nvml = demand.sources[2] || demand.sources[3];
        if !needs_nvml || demand.nvml_epoch != self.demand.nvml_epoch {
            self.nvml = None;
        }
        self.demand = demand;
        if !sample {
            return Sample::default();
        }
        let percent = if demand.sources[0] {
            let cpu = File::open(self.proc_root.join("stat"))
                .ok()
                .and_then(|f| read_bounded(f).ok())
                .as_deref()
                .and_then(Cpu::parse);
            let percent = cpu
                .zip(self.previous_cpu)
                .and_then(|(now, old)| now.percent_since(old));
            self.previous_cpu = cpu;
            percent
        } else {
            None
        };
        let ram = if demand.sources[1] {
            File::open(self.proc_root.join("meminfo"))
                .ok()
                .and_then(|f| read_bounded(f).ok())
                .and_then(|text| Memory::parse(&text))
        } else {
            None
        };
        if needs_nvml && self.nvml.is_none() {
            self.nvml = self.loader.as_mut().and_then(|load| load());
        }
        let (gpu, vram) = self
            .nvml
            .as_ref()
            .map(|nvml| nvml.read_selected(demand.sources[2], demand.sources[3]))
            .unwrap_or_default();
        if needs_nvml && gpu.is_none() && vram.is_none() {
            // A returned error can be retried on the next shared tick; never spawn a replacement.
            self.nvml = None;
        }
        Sample {
            cpu: percent,
            ram,
            gpu,
            vram,
        }
    }
}

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
