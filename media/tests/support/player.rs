//! Fake MPRIS players: the `fake_player` example, driven through its standard input and output.
use super::{PrivateBus, example};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::process::CommandExt,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Lines {
    seen: Mutex<Vec<String>>,
    arrived: Condvar,
}

pub struct FakePlayer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Arc<Lines>,
    reader: Option<JoinHandle<()>>,
    pings: u32,
}

impl FakePlayer {
    /// Starts a fake that owns `bus_name` on `bus`, once the name is owned.
    pub fn start(bus: &PrivateBus, bus_name: &str) -> Self {
        Self::start_owning(bus, &[bus_name])
    }

    /// Starts one fake that owns all of `bus_names`: many players for the price of one process.
    pub fn start_owning(bus: &PrivateBus, bus_names: &[&str]) -> Self {
        let bus_name = bus_names.join(" ");
        let mut command = Command::new(example("fake_player"));
        command
            .args(bus_names)
            .env_clear()
            .env("DBUS_SESSION_BUS_ADDRESS", bus.address())
            .env("FAKE_PLAYER_PRIVATE_BUS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // SAFETY: as in `PrivateBus::start`.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let mut child = command.spawn().expect("start the fake player");
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take();
        let lines = Arc::new(Lines::default());
        let sink = Arc::clone(&lines);
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                sink.seen.lock().unwrap().push(line);
                sink.arrived.notify_all();
            }
        });
        let player = Self {
            child,
            stdin,
            lines,
            reader: Some(reader),
            pings: 0,
        };
        assert!(
            player.wait_for_line(|l| l == "ready", Duration::from_secs(10)),
            "the fake player {bus_name} never became ready"
        );
        player
    }

    fn wait_for_line(&self, wanted: impl Fn(&str) -> bool, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let mut seen = self.lines.seen.lock().unwrap();
        loop {
            if seen.iter().any(|l| wanted(l)) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            seen = self.lines.arrived.wait_timeout(seen, left).unwrap().0;
        }
    }

    /// Sends one command line (see the `fake_player` example).
    pub fn command(&mut self, line: &str) -> &mut Self {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
        self
    }

    /// Waits until the fake has handled everything sent so far, and every call it logged is here.
    pub fn sync(&mut self) -> &mut Self {
        self.pings += 1;
        let token = format!("pong {}", self.pings);
        self.command(&format!("ping {}", self.pings));
        assert!(
            self.wait_for_line(|l| l == token, Duration::from_secs(10)),
            "the fake player did not answer a ping"
        );
        self
    }

    /// The player methods called so far, in order (`Play`, `Next`, ...).
    pub fn calls(&mut self) -> Vec<String> {
        self.sync();
        self.lines
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|l| l.strip_prefix("call ").map(str::to_owned))
            .collect()
    }

    /// How many times a Player property was read by anyone, `PlaybackStatus` for instance.
    pub fn reads_of(&mut self, property: &str) -> usize {
        self.sync();
        let wanted = format!("read org.mpris.MediaPlayer2.Player.{property}");
        self.lines
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|l| **l == wanted)
            .count()
    }

    /// Freezes the whole process, as a hung player: it answers nothing until `thaw`.
    pub fn freeze(&self) {
        self.signal(libc::SIGSTOP);
    }

    pub fn thaw(&self) {
        self.signal(libc::SIGCONT);
    }

    fn signal(&self, signal: i32) {
        // SAFETY: plain `kill` on the id of our own child.
        unsafe { libc::kill(self.child.id() as i32, signal) };
    }

    /// Ends the player, so its name leaves the bus.
    pub fn quit(mut self) {
        self.thaw();
        self.stdin = None;
        let _ = self.child.wait();
    }
}

impl Drop for FakePlayer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
