// Passive fault-profile support, included only by cfg(test) modules.
// This file is not a product module and has no external dependencies.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Instant,
};

#[allow(dead_code)]
pub struct Probe {
    root: PathBuf,
    output: File,
    scenario: &'static str,
    component_clock: &'static str,
    epoch: Instant,
}

impl Probe {
    pub fn begin(scenario: &'static str, component_clock: &'static str) -> Option<Self> {
        let root = std::env::var_os("RAD_FAULT_PROFILE_ROOT")?;
        let root = PathBuf::from(root);
        assert!(root.is_absolute(), "profile root must be absolute");
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|path| path.join(".git").exists())
            .expect("profile support belongs to this checkout");
        let allowed = repository
            .join("target/performance")
            .canonicalize()
            .unwrap();
        let parent = root.parent().unwrap().canonicalize().unwrap();
        assert!(
            parent.starts_with(allowed),
            "profile root outside target/performance"
        );
        let root = parent.join(root.file_name().unwrap());
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("fault.jsonl"))
            .unwrap();
        Some(Self {
            root,
            output,
            scenario,
            component_clock,
            epoch: Instant::now(),
        })
    }

    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sample(&mut self, operation: &str, elapsed_ns: u128) {
        assert!(
            operation
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c == b'_')
        );
        writeln!(self.output,
            "{{\"event\":\"sample\",\"scenario\":\"{}\",\"operation\":\"{}\",\"ns\":{},\"clock\":\"wall_monotonic\",\"component_clock\":\"{}\"}}",
            self.scenario, operation, elapsed_ns, self.component_clock).unwrap();
        self.output.flush().unwrap();
    }

    pub fn snapshot(&mut self, phase: &str) {
        assert!(["before", "during", "after"].contains(&phase));
        let mut rss = 0;
        let mut pss = 0;
        let mut private = 0;
        for line in fs::read_to_string("/proc/self/smaps_rollup")
            .unwrap()
            .lines()
        {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            if !matches!(
                key,
                "Rss" | "Pss" | "Private_Clean" | "Private_Dirty" | "Private_Hugetlb"
            ) {
                continue;
            }
            let value = value
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap();
            match key {
                "Rss" => rss = value,
                "Pss" => pss = value,
                "Private_Clean" | "Private_Dirty" | "Private_Hugetlb" => private += value,
                _ => {}
            }
        }
        let stat = fs::read_to_string("/proc/self/stat").unwrap();
        let values: Vec<_> = stat
            .rsplit(')')
            .next()
            .unwrap()
            .split_whitespace()
            .collect();
        let ticks = values[11].parse::<u64>().unwrap() + values[12].parse::<u64>().unwrap();
        let threads = fs::read_dir("/proc/self/task").unwrap().count();
        let fds = fs::read_dir("/proc/self/fd").unwrap().count();
        writeln!(self.output,
            "{{\"event\":\"phase_snapshot\",\"scenario\":\"{}\",\"phase\":\"{}\",\"elapsed_ns\":{},\"pid\":{},\"rss_kib\":{},\"pss_kib\":{},\"private_kib\":{},\"cpu_ticks\":{},\"threads\":{},\"fds\":{},\"scope\":\"self_process_not_tree\",\"clock\":\"wall_monotonic\",\"component_clock\":\"{}\"}}",
            self.scenario, phase, self.epoch.elapsed().as_nanos(), std::process::id(), rss, pss, private, ticks, threads, fds, self.component_clock).unwrap();
        self.output.flush().unwrap();
    }
}
