//! Test-side control of the fake sound server.
#![allow(dead_code)]
use super::fake_state::{self, Dev, State};
use rad_plugin_audio::{Kind, Pactl};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

pub struct FakeServer {
    dir: PathBuf,
}

fn dev(name: &str, description: &str) -> Dev {
    Dev {
        name: name.into(),
        description: description.into(),
        mute: false,
    }
}

/// A built example of this package: `target/<profile>/examples/<name>`, found from the running
/// test binary. `cargo test` builds them; with a filter such as `--test x` they need
/// `cargo build --examples` first.
pub fn example(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>/deps");
    let path = profile_dir.join("examples").join(name);
    assert!(
        path.exists(),
        "{} is missing: run `cargo test` or `cargo build --examples`",
        path.display()
    );
    path
}

pub fn fake_pactl_path() -> PathBuf {
    example("fake_pactl")
}

impl FakeServer {
    /// Two sinks and two microphones; the first of each is the default.
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "fake-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fake_state::update(&dir, |state| {
            *state = State {
                sinks: vec![dev("sink-a", "Speakers"), dev("sink-b", "Headphones")],
                sources: vec![
                    dev("mic-a", "Desk microphone"),
                    dev("mic-b", "Webcam microphone"),
                ],
                default_sink: "sink-a".into(),
                default_source: "mic-a".into(),
            };
        });
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A `Pactl` that runs the fake against this server, as the real one would against a socket.
    pub fn pactl(&self) -> Pactl {
        Pactl::new(fake_pactl_path(), Some(self.dir.clone().into_os_string()))
    }

    fn list(state: &mut State, kind: Kind) -> &mut Vec<Dev> {
        match kind {
            Kind::Output => &mut state.sinks,
            Kind::Microphone => &mut state.sources,
        }
    }

    pub fn state(&self) -> State {
        fake_state::read(&self.dir)
    }

    pub fn is_muted(&self, kind: Kind, name: &str) -> bool {
        let mut state = self.state();
        Self::list(&mut state, kind)
            .iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the fake server"))
            .mute
    }

    fn event(&self, line: &str) {
        fake_state::append(&self.dir, "events.log", line);
    }

    /// An application mutes or unmutes a device behind the plugin's back.
    pub fn set_mute(&self, kind: Kind, name: &str, mute: bool) {
        fake_state::update(&self.dir, |s| {
            Self::list(s, kind)
                .iter_mut()
                .find(|d| d.name == name)
                .expect("device")
                .mute = mute;
        });
        self.event("Event 'change' on sink #1");
    }

    pub fn set_default(&self, kind: Kind, name: &str) {
        fake_state::update(&self.dir, |s| match kind {
            Kind::Output => s.default_sink = name.into(),
            Kind::Microphone => s.default_source = name.into(),
        });
        self.event("Event 'change' on server #0");
    }

    pub fn add_device(&self, kind: Kind, name: &str, description: &str) {
        fake_state::update(&self.dir, |s| {
            Self::list(s, kind).push(dev(name, description))
        });
        self.event("Event 'new' on sink #9");
    }

    pub fn remove_device(&self, kind: Kind, name: &str) {
        fake_state::update(&self.dir, |s| {
            Self::list(s, kind).retain(|d| d.name != name)
        });
        self.event("Event 'remove' on sink #9");
    }

    pub fn rename_all(&self, kind: Kind, description: impl Fn(usize) -> String) {
        fake_state::update(&self.dir, |s| {
            for (n, d) in Self::list(s, kind).iter_mut().enumerate() {
                d.description = description(n);
            }
        });
    }

    pub fn emit(&self, line: &str) {
        self.event(line);
    }

    fn marker(&self, name: &str, on: bool) {
        let path = fake_state::file(&self.dir, name);
        if on {
            fs::write(path, "").unwrap();
        } else {
            let _ = fs::remove_file(path);
        }
    }

    /// The server stops answering: every command, and a running `subscribe`, fails.
    pub fn down(&self) {
        self.marker("down", true);
    }
    pub fn up(&self) {
        self.marker("down", false);
        self.event("Event 'change' on server #0");
    }
    pub fn fail_mutations(&self, on: bool) {
        self.marker("fail", on);
    }
    pub fn garbage(&self, on: bool) {
        self.marker("garbage", on);
    }
    pub fn flood(&self, on: bool) {
        self.marker("flood", on);
    }
    /// Commands whose arguments start with `prefix` hang instead of returning.
    pub fn hang(&self, prefixes: &[&str]) {
        fs::write(fake_state::file(&self.dir, "hang"), prefixes.join("\n")).unwrap();
    }

    fn log(&self) -> Vec<String> {
        fs::read_to_string(fake_state::file(&self.dir, "calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Every command line the fake was run with (`subscribe` included), in order.
    pub fn calls(&self) -> Vec<String> {
        self.log()
            .into_iter()
            .filter_map(|l| l.strip_prefix("argv ").map(str::to_owned))
            .collect()
    }

    /// The environment variable names each run of the fake saw, one comma-separated list per run.
    pub fn environments(&self) -> Vec<String> {
        self.log()
            .into_iter()
            .filter_map(|l| l.strip_prefix("env ").map(str::to_owned))
            .collect()
    }

    pub fn calls_starting_with(&self, prefix: &str) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }

    /// How many times the plugin listed devices (sinks or sources).
    pub fn listings(&self) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.contains("list sinks"))
            .count()
    }

    /// Whether any command that changes the server was ever run.
    pub fn mutating_calls(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("set-") || c.starts_with("load-"))
            .collect()
    }

    fn pids(&self, tag: &str) -> Vec<u32> {
        self.log()
            .iter()
            .filter_map(|l| l.strip_prefix(tag).and_then(|p| p.trim().parse().ok()))
            .collect()
    }

    fn alive(pid: u32) -> bool {
        // A reaped process has no /proc entry; a killed but unreaped one would be a zombie.
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
    }

    /// `pactl subscribe` children that are still running.
    pub fn live_subscribers(&self) -> usize {
        self.pids("subscribe-start ")
            .into_iter()
            .filter(|&p| Self::alive(p))
            .count()
    }

    /// Hung commands that are still running.
    pub fn live_hung(&self) -> usize {
        self.pids("hang ")
            .into_iter()
            .filter(|&p| Self::alive(p))
            .count()
    }

    /// How many commands entered the injected hang.
    pub fn hangs_started(&self) -> usize {
        self.pids("hang ").len()
    }

    pub fn subscribers_started(&self) -> usize {
        self.pids("subscribe-start ").len()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Polls `condition` until it holds or `within` passes; returns whether it held.
pub fn eventually(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
