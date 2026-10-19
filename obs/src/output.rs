//! Terminal, bounded writes of whole SDK frames. A partial frame is never followed by another.
use std::{
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::{Duration, Instant},
};
struct HostOutput(OwnedFd);
impl HostOutput {
    fn open(fd: i32) -> io::Result<Self> {
        // SAFETY: fstat receives initialized storage; duplicate owns a new descriptor.
        unsafe {
            let mut stat = std::mem::zeroed::<libc::stat>();
            if libc::fstat(fd, &mut stat) < 0 {
                return Err(io::Error::last_os_error());
            }
            if ![libc::S_IFIFO, libc::S_IFSOCK].contains(&(stat.st_mode & libc::S_IFMT)) {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let owned = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3);
            if owned < 0 {
                return Err(io::Error::last_os_error());
            }
            let out = OwnedFd::from_raw_fd(owned);
            let flags = libc::fcntl(owned, libc::F_GETFL);
            if flags < 0 || libc::fcntl(owned, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(out))
        }
    }
    fn frame(&mut self, bytes: &[u8]) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(50);
        let mut written = 0;
        while written < bytes.len() {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            // SAFETY: fd is owned; slice remains live for the duration of write/poll.
            let n = unsafe {
                libc::write(
                    self.0.as_raw_fd(),
                    bytes[written..].as_ptr().cast(),
                    bytes.len() - written,
                )
            };
            if n > 0 {
                written += n as usize;
                continue;
            }
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut poll = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: poll points to one initialized descriptor and uses a bounded timeout.
            let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis().max(1) as i32) };
            if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
impl Write for HostOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.frame(bytes).is_err() {
            std::process::exit(74);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn run_host(plugin: crate::ObsPlugin) -> Result<(), rad_plugin_sdk::Error> {
    let out = HostOutput::open(1).unwrap_or_else(|_| std::process::exit(74));
    rad_plugin_sdk::run(crate::config(), plugin, std::io::stdin(), out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::net::UnixStream};
    #[test]
    fn full_frame_survives_partial_writes_when_reader_drains() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let mut out = HostOutput::open(a.as_raw_fd()).unwrap();
        let size = 4096i32;
        // SAFETY: valid socket and pointer to initialized socket-option integer.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    a.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&size as *const i32).cast(),
                    size_of::<i32>() as _,
                )
            },
            0
        );
        let payload = vec![37; 65540];
        let expected = payload.clone();
        let reader = std::thread::spawn(move || {
            let mut all = vec![0; expected.len()];
            b.read_exact(&mut all).unwrap();
            assert_eq!(all, expected);
        });
        out.frame(&payload).unwrap();
        reader.join().unwrap();
    }
    #[test]
    fn nonreading_host_terminates_process_instead_of_following_partial_frame() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "output::tests::stalled_output_child",
                "--ignored",
                "--nocapture",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(74));
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("output failure did not terminate");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    #[ignore = "child-only fail-closed probe, run by nonreading_host test"]
    fn stalled_output_child() {
        let mut output = HostOutput::open(1).unwrap();
        output.write_all(&vec![1; 1024 * 1024]).unwrap();
        panic!("terminal write unexpectedly returned");
    }
}
