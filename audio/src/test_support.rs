//! Helpers for the unit tests that run the fake `pactl` (the `fake_pactl` example), which `cargo
//! test` builds. Integration tests have their own, fuller scaffolding in `tests/support`.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// `target/<profile>`, found from the running test binary.
pub(crate) fn profile_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().and_then(Path::parent).unwrap().to_path_buf()
}

pub(crate) fn fake() -> PathBuf {
    let fake = profile_dir().join("examples").join("fake_pactl");
    assert!(
        fake.exists(),
        "run `cargo test`, which builds the fake_pactl example"
    );
    fake
}

/// A directory standing in for the sound server's runtime directory; removed on drop.
pub(crate) struct Dir(pub(crate) PathBuf);

impl Dir {
    pub(crate) fn new(name: &str) -> Self {
        let dir = profile_dir().join(format!("unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// One output and one microphone, each the default.
    pub(crate) fn with_devices(name: &str) -> Self {
        let dir = Self::new(name);
        fs::write(
            dir.0.join("state.json"),
            r#"{"sinks":[{"name":"sink-a","description":"Speakers","mute":false}],
                "sources":[{"name":"mic-a","description":"Microphone","mute":false}],
                "default_sink":"sink-a","default_source":"mic-a"}"#,
        )
        .unwrap();
        dir
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The process ids the fake logged after `tag` (for instance `"subscribe-start "`).
pub(crate) fn pids(dir: &Dir, tag: &str) -> Vec<u32> {
    fs::read_to_string(dir.0.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix(tag).and_then(|p| p.trim().parse().ok()))
        .collect()
}

/// Of the processes logged after `tag`, how many are still running (a zombie is not).
pub(crate) fn alive(dir: &Dir, tag: &str) -> usize {
    pids(dir, tag)
        .into_iter()
        .filter(|pid| {
            fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|stat| {
                    !stat
                        .rsplit(')')
                        .next()
                        .unwrap_or("")
                        .trim_start()
                        .starts_with('Z')
                })
                .unwrap_or(false)
        })
        .count()
}

/// Whether `pid` has left the process table for good. A killed child nobody waited for stays in
/// it as a zombie, which `alive` does not count.
pub(crate) fn gone(pid: u32) -> bool {
    !Path::new(&format!("/proc/{pid}")).exists()
}

/// Whether the fake logged at least one process after `tag` and every one of them is `gone`.
pub(crate) fn all_gone(dir: &Dir, tag: &str) -> bool {
    let pids = pids(dir, tag);
    !pids.is_empty() && pids.into_iter().all(gone)
}

/// The fake server reports a device event on its `subscribe` streams.
pub(crate) fn emit_event(dir: &Dir, line: &str) {
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.0.join("events.log"))
        .unwrap();
    writeln!(log, "{line}").unwrap();
}
