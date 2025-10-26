//! An epoll-based reactor.

use std::os::fd::{FromRawFd, OwnedFd};

struct Reactor {
    epoll_fd: OwnedFd,
}

impl Reactor {
    fn new() -> std::io::Result<Self> {
        let epoll_fd = unsafe {
            // SAFETY: epoll_create1 with is always safe to call with a flag of 0.
            let raw_fd = libc::epoll_create1(0);

            if raw_fd == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: At this point, we know that the call to epoll_create1 was successful
            OwnedFd::from_raw_fd(raw_fd)
        };

        Ok(Self { epoll_fd })
    }
}
