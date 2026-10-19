//! All potentially blocking I/O is confined to the polling worker.
use crate::{
    nvml::Nvml,
    polling::Sample,
    readings::{Cpu, Memory, read_bounded},
};
use std::{fs::File, path::PathBuf};
pub struct Reader {
    proc_root: PathBuf,
    nvml: Option<Nvml>,
    previous_cpu: Option<Cpu>,
    loader: Option<Box<dyn FnMut() -> Option<Nvml>>>,
}
impl Reader {
    /// Explicit paths and adapter permit controlled fixtures without system probes.
    pub fn new(proc_root: PathBuf, nvml: Option<Nvml>) -> Self {
        Self {
            proc_root,
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
        let stat = File::open(self.proc_root.join("stat"))
            .ok()
            .and_then(|f| read_bounded(f).ok());
        let cpu = stat.as_deref().and_then(Cpu::parse);
        let percent = cpu
            .zip(self.previous_cpu)
            .and_then(|(now, old)| now.percent_since(old));
        self.previous_cpu = cpu;
        let ram = File::open(self.proc_root.join("meminfo"))
            .ok()
            .and_then(|f| read_bounded(f).ok())
            .and_then(|text| Memory::parse(&text));
        if self.nvml.is_none() {
            self.nvml = self.loader.as_mut().and_then(|load| load());
        }
        let (gpu, vram) = self.nvml.as_ref().map(Nvml::read).unwrap_or_default();
        if gpu.is_none() && vram.is_none() {
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
