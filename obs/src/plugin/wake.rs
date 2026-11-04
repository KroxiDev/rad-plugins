//! One eventfd joins SDK demand and socket readiness in the worker's wait.
use rad_plugin_sdk::FailureCode;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub(super) struct Wake(OwnedFd);
impl Wake {
    pub fn new() -> Result<Self, FailureCode> {
        // SAFETY: no pointer arguments; ownership of a fresh fd transfers exactly once.
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if fd < 0 {
            return Err(FailureCode::Unavailable);
        }
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }
    pub fn notify_one(&self) {
        self.notify_all();
    }
    pub fn notify_all(&self) {
        let value = 1_u64;
        // SAFETY: live owned eventfd and initialized u64 of the required size.
        // EAGAIN means a wake is already pending; no additional notification is needed.
        loop {
            let written = unsafe {
                libc::write(
                    self.0.as_raw_fd(),
                    (&value as *const u64).cast(),
                    size_of::<u64>(),
                )
            };
            if written >= 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
        }
    }
    pub fn wait(&self, mut sockets: Vec<libc::pollfd>, timeout: i32) {
        sockets.push(libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        // SAFETY: the contiguous initialized descriptor buffer lives throughout poll.
        // Socket retirement may race the snapshot; POLLNVAL simply causes a fresh turn.
        let ready = unsafe { libc::poll(sockets.as_mut_ptr(), sockets.len() as _, timeout) };
        if ready > 0 && sockets.last().unwrap().revents & libc::POLLIN != 0 {
            let mut value = 0_u64;
            // SAFETY: live eventfd and writable u64 buffer; one read consumes its counter.
            unsafe {
                libc::read(
                    self.0.as_raw_fd(),
                    (&mut value as *mut u64).cast(),
                    size_of::<u64>(),
                );
            }
        }
    }
}
