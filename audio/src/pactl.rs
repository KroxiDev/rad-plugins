//! Running `pactl`, the command line client of PulseAudio and of PipeWire's PulseAudio server.
//!
//! The plugin starts with a nearly empty environment (no `PATH`), so the program is looked up in
//! fixed places and every child gets an environment of its own: only `XDG_RUNTIME_DIR`, which is
//! how `pactl` finds the server's socket. Children read nothing, say nothing on stderr, have
//! bounded output and a deadline, and are killed if the plugin dies.
use std::{
    ffi::OsString,
    fmt, io,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

/// Fixed places `pactl` is looked for.
const SEARCH_PATHS: [&str; 3] = ["/usr/bin/pactl", "/bin/pactl", "/usr/local/bin/pactl"];
/// Most output kept from one command; a device listing is a few tens of KiB.
const MAX_OUTPUT: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub(crate) enum RunError {
    /// `pactl` could not be started at all.
    Spawn(io::Error),
    /// The deadline passed; the child was killed.
    TimedOut,
    /// `pactl` ran and failed. With the server away this is its "connection refused".
    Failed,
    /// More output than [`MAX_OUTPUT`]; the child was killed.
    TooMuchOutput,
    Io(io::Error),
}
impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "cannot start pactl: {e}"),
            Self::TimedOut => f.write_str("pactl did not answer in time"),
            Self::Failed => f.write_str("pactl failed"),
            Self::TooMuchOutput => f.write_str("pactl printed too much"),
            Self::Io(e) => write!(f, "reading pactl: {e}"),
        }
    }
}

/// A way to run `pactl` against one sound server.
#[derive(Debug, Clone)]
pub struct Pactl {
    program: PathBuf,
    runtime_dir: Option<OsString>,
}

impl Pactl {
    /// Runs `program` with `runtime_dir` as the only environment variable (`XDG_RUNTIME_DIR`).
    pub fn new(program: impl Into<PathBuf>, runtime_dir: Option<OsString>) -> Self {
        Self {
            program: program.into(),
            runtime_dir,
        }
    }

    /// The system's `pactl` with this process's `XDG_RUNTIME_DIR`; `None` if it is not installed.
    pub fn system() -> Option<Self> {
        SEARCH_PATHS
            .iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .map(|path| Self::new(path, std::env::var_os("XDG_RUNTIME_DIR")))
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(dir) = &self.runtime_dir {
            command.env("XDG_RUNTIME_DIR", dir);
        }
        let parent = std::process::id();
        // SAFETY: the closure only calls async-signal-safe functions (prctl, getppid), as
        // required between fork and exec. The kernel delivers the signal when the thread that
        // started the child exits, which is why every child is waited for by its starter.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(io::Error::last_os_error());
                }
                // The parent may have died before the signal was armed.
                if libc::getppid() as u32 != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
        command
    }

    /// Runs `pactl args...` to completion and returns its output. Succeeds only if it exits with
    /// status 0 before `deadline`.
    pub(crate) fn run(&self, args: &[&str], deadline: Instant) -> Result<Vec<u8>, RunError> {
        let mut child = self.command().args(args).spawn().map_err(RunError::Spawn)?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let result = collect(&mut child, &mut stdout, deadline);
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }

    /// Starts `pactl subscribe`, whose stdout reports server events line by line. The caller
    /// kills and waits for the child.
    pub(crate) fn subscribe(&self) -> io::Result<(Child, ChildStdout)> {
        let mut child = self.command().arg("subscribe").spawn()?;
        let stdout = child.stdout.take().expect("stdout is piped");
        Ok((child, stdout))
    }
}

fn collect(
    child: &mut Child,
    stdout: &mut ChildStdout,
    deadline: Instant,
) -> Result<Vec<u8>, RunError> {
    use std::io::Read;
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if !readable(stdout, deadline).map_err(RunError::Io)? {
            return Err(RunError::TimedOut);
        }
        // Readable (or hung up), so this read does not block.
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(RunError::Io(e)),
        }
        if output.len() > MAX_OUTPUT {
            return Err(RunError::TooMuchOutput);
        }
    }
    // Output closed: the process is exiting, but it still has to be reaped within the deadline.
    loop {
        match child.try_wait().map_err(RunError::Io)? {
            Some(status) if status.success() => return Ok(output),
            Some(_) => return Err(RunError::Failed),
            None if Instant::now() >= deadline => return Err(RunError::TimedOut),
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }
}

/// Waits until `stdout` has data or is closed. `false` if `deadline` came first.
fn readable(stdout: &ChildStdout, deadline: Instant) -> io::Result<bool> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        let mut poll = libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd, count 1.
        match unsafe { libc::poll(&mut poll, 1, millis) } {
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            0 => {}
            _ => return Ok(true),
        }
    }
}

#[cfg(test)]
mod tests {
    //! `Pactl::run` called from the test's own thread, which outlives the child: the explicit kill
    //! is what ends a hung child here, not the death-signal safety net.
    use super::*;
    use std::fs;

    use crate::test_support::{Dir, fake, pids};

    #[test]
    fn a_child_that_misses_the_deadline_is_killed_and_reaped_before_run_returns() {
        let dir = Dir::new("hang");
        fs::write(dir.0.join("hang"), "-f json list").unwrap();
        let pactl = Pactl::new(fake(), Some(dir.0.clone().into_os_string()));
        let started = Instant::now();
        let result = pactl.run(
            &["-f", "json", "list", "sinks"],
            started + Duration::from_millis(300),
        );
        assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        let hung = pids(&dir, "hang ");
        assert_eq!(hung.len(), 1, "the child really hung");
        assert!(
            !Path::new(&format!("/proc/{}", hung[0])).exists(),
            "killed and waited for"
        );
    }

    #[test]
    fn output_past_the_cap_ends_the_child_and_is_an_error() {
        let dir = Dir::new("flood");
        fs::write(dir.0.join("flood"), "").unwrap();
        let pactl = Pactl::new(fake(), Some(dir.0.clone().into_os_string()));
        let result = pactl.run(
            &["-f", "json", "list", "sinks"],
            Instant::now() + Duration::from_secs(5),
        );
        assert!(matches!(result, Err(RunError::TooMuchOutput)), "{result:?}");
    }

    #[test]
    fn a_missing_program_is_a_spawn_error_and_a_failing_one_is_failed() {
        let dir = Dir::new("failing");
        let missing = Pactl::new(dir.0.join("no-such-pactl"), None);
        let result = missing.run(&["info"], Instant::now() + Duration::from_secs(1));
        assert!(matches!(result, Err(RunError::Spawn(_))), "{result:?}");
        fs::write(dir.0.join("down"), "").unwrap();
        let down = Pactl::new(fake(), Some(dir.0.clone().into_os_string()));
        let result = down.run(&["info"], Instant::now() + Duration::from_secs(2));
        assert!(matches!(result, Err(RunError::Failed)), "{result:?}");
    }
}
