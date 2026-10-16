//! State of the fake sound server. A directory stands in for `XDG_RUNTIME_DIR`: the fake `pactl`
//! (examples/fake_pactl.rs) keeps everything in it, and the tests change it from outside the way
//! another application would. Nothing here ever talks to a real sound server.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Dev {
    pub name: String,
    pub description: String,
    pub mute: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct State {
    pub sinks: Vec<Dev>,
    pub sources: Vec<Dev>,
    pub default_sink: String,
    pub default_source: String,
}

pub fn file(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

/// Read-modify-write under an exclusive lock, so the fake pactl processes and the test never
/// overwrite each other's changes.
pub fn update<R>(dir: &Path, change: impl FnOnce(&mut State) -> R) -> R {
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(file(dir, "state.lock"))
        .expect("lock file");
    // SAFETY: flock on a descriptor this function owns.
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    let mut state = read(dir);
    let result = change(&mut state);
    let temp = file(dir, "state.tmp");
    fs::write(&temp, serde_json::to_vec(&state).unwrap()).expect("write state");
    fs::rename(&temp, file(dir, "state.json")).expect("publish state");
    drop(lock);
    result
}

pub fn read(dir: &Path) -> State {
    fs::read(file(dir, "state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Appends one line to a log file with a single `write`, so concurrent writers do not mix lines.
pub fn append(dir: &Path, name: &str, line: &str) {
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file(dir, name))
        .expect("log file");
    log.write_all(format!("{line}\n").as_bytes())
        .expect("append");
}
