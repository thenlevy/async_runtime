//! An epoll-based reactor.

use {
    core::task::Waker,
    libc::epoll_ctl,
    std::{
        borrow::Borrow,
        cell::RefCell,
        collections::HashMap,
        os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        rc::Rc,
    },
};

pub struct Reactor {
    epoll_fd: OwnedFd,
    sources: HashMap<EventKey, Rc<RefCell<IoSource>>>,
}

pub struct IoSource {
    fd: Rc<OwnedFd>,
    key: EventKey,

    // Wakers for tasks that are polling the readability or writability of this source.
    poll_reader: Option<Waker>,
    poll_writer: Option<Waker>,

    // Wakers for tasks that are waiting for this source to be readable or writable.
    readers: Vec<Waker>,
    writers: Vec<Waker>,
}

impl IoSource {
    fn drain_writers_into(&mut self, wakers: &mut Vec<Waker>) {
        if let Some(waker) = self.poll_writer.take() {
            wakers.push(waker);
        }
        wakers.extend(self.writers.drain(..));
    }

    fn drain_readers_into(&mut self, wakers: &mut Vec<Waker>) {
        if let Some(waker) = self.poll_reader.take() {
            wakers.push(waker);
        }
        wakers.extend(self.readers.drain(..));
    }

    fn waiting_for(&self) -> Event {
        let mut event = Event::none(self.key);
        if self.poll_reader.is_some() || !self.readers.is_empty() {
            event.readable = true;
        }
        if self.poll_writer.is_some() || !self.writers.is_empty() {
            event.writable = true;
        }
        event
    }
}

impl Reactor {
    const MAX_EVENT: u32 = 1024;

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

        Ok(Self {
            epoll_fd,
            sources: HashMap::new(),
        })
    }

    pub fn add_source(&mut self, fd: Rc<OwnedFd>) -> std::io::Result<Rc<RefCell<IoSource>>> {
        let key = EventKey::new();
        let epoll_event: EpollEvent = Event::none(key).into();
        let mut libc_epoll_event: libc::epoll_event = epoll_event.into();

        // SAFETY: self.epoll_fd is a valid epoll file descriptor created with epoll_create1.
        // fd is a valid file descriptor as guaranteed by the OwnedFd type.
        let ret = unsafe {
            epoll_ctl(
                self.epoll_fd.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                fd.as_ref().as_raw_fd(),
                (&mut libc_epoll_event) as *mut libc::epoll_event,
            )
        };

        if ret == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let new_source = Rc::new(RefCell::new(IoSource {
            fd: fd,
            key,
            poll_reader: None,
            poll_writer: None,
            readers: Vec::new(),
            writers: Vec::new(),
        }));
        self.sources.insert(key, new_source.clone());
        Ok(new_source)
    }

    pub fn react(&mut self) -> std::io::Result<()> {
        let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; Self::MAX_EVENT as usize];
        let mut interests = Vec::new();

        let res = {
            let nb_events = unsafe {
                libc::epoll_wait(
                    self.epoll_fd.as_raw_fd(),
                    events.as_mut_ptr(),
                    Self::MAX_EVENT as i32,
                    -1,
                )
            };

            if nb_events == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(nb_events)
            }
        };
        match res {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(()),
            Err(e) => Err(e),
            Ok(nb_events) => {
                assert!(nb_events >= 0);
                let wakers = Vec::with_capacity(nb_events as usize);
                let events = &events[0..nb_events as usize];
                for event in events {
                    let epoll_event = EpollEvent::from(*event);
                    let event = Event::from(epoll_event);

                    if let Some(mut source) = self.sources.get(&event.key).map(|rc| rc.borrow_mut())
                    {
                        if event.readable {
                            source.drain_readers_into(&mut wakers.clone());
                        }
                        if event.writable {
                            source.drain_writers_into(&mut wakers.clone());
                        }
                        let event = source.waiting_for();
                        if event.readable || event.writable {
                            interests.push((source.fd.clone(), event));
                        }
                    }
                }
                for (fd, interest) in interests {
                    self.register_interest(fd.as_fd(), interest).unwrap();
                }
                for waker in wakers {
                    waker.wake();
                }
                Ok(())
            }
        }
    }

    pub fn register_interest(
        &mut self,
        fd: BorrowedFd<'_>,
        interest: Event,
    ) -> std::io::Result<()> {
        let epoll_event: EpollEvent = interest.into();
        let mut libc_epoll_event: libc::epoll_event = epoll_event.into();

        // SAFETY: self.epoll_fd is a valid epoll file descriptor created with epoll_create1.
        // fd is a valid file descriptor as guaranteed by the BorrowedFd type.
        let ret = unsafe {
            epoll_ctl(
                self.epoll_fd.as_raw_fd(),
                libc::EPOLL_CTL_MOD,
                fd.as_raw_fd(),
                (&mut libc_epoll_event) as *mut libc::epoll_event,
            )
        };

        if ret == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn deregister_source(&mut self, key: &EventKey) -> std::io::Result<()> {
        if let Some(source_rc) = self.sources.remove(key) {
            let source = source_rc.as_ref().borrow();
            // SAFETY: self.epoll_fd is a valid epoll file descriptor created with epoll_create1.
            // the source.fd is valid as long as the IoSource is alive.
            let ret = unsafe {
                epoll_ctl(
                    self.epoll_fd.as_raw_fd(),
                    libc::EPOLL_CTL_DEL,
                    source.fd.as_ref().as_raw_fd(),
                    std::ptr::null_mut(),
                )
            };

            if ret == -1 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn waiting_on_events(&self) -> bool {
        self.sources.values().any(|source_rc| {
            let source = source_rc.as_ref().borrow();
            let event = source.waiting_for();
            event.readable || event.writable
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventKey(u64);

impl EventKey {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_KEY: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_KEY.fetch_add(1, Ordering::Relaxed))
    }
}
pub struct Event {
    key: EventKey,
    readable: bool,
    writable: bool,
}

impl Event {
    fn none(key: EventKey) -> Self {
        Self {
            key,
            readable: false,
            writable: false,
        }
    }

    fn write(key: EventKey) -> Self {
        Self {
            key,
            readable: false,
            writable: true,
        }
    }

    fn read(key: EventKey) -> Self {
        Self {
            key,
            readable: true,
            writable: false,
        }
    }

    fn read_write(key: EventKey) -> Self {
        Self {
            key,
            readable: true,
            writable: true,
        }
    }
}

struct EpollEvent {
    events: EpollEvents,
    key: EventKey,
}

impl From<EpollEvent> for Event {
    fn from(epoll_event: EpollEvent) -> Self {
        Self {
            key: epoll_event.key,
            readable: epoll_event.events.has_readable(),
            writable: epoll_event.events.has_writable(),
        }
    }
}

impl From<Event> for EpollEvent {
    fn from(event: Event) -> Self {
        let mut epoll_events = EpollEvents::new();
        if event.readable {
            epoll_events.set_readable();
        }
        if event.writable {
            epoll_events.set_writable();
        }
        Self {
            events: epoll_events,
            key: EventKey(event.key.0),
        }
    }
}

struct EpollEvents(u32);

// These are no-ops https://doc.rust-lang.org/stable/reference/expressions/operator-expr.html#r-expr.as.numeric.int-same-size
const EPOLLOUT: u32 = libc::EPOLLOUT as u32;
const EPOLLIN: u32 = libc::EPOLLIN as u32;

impl EpollEvents {
    fn new() -> Self {
        EpollEvents(libc::EPOLLONESHOT as u32)
    }

    fn set_writable(&mut self) {
        self.0 |= EPOLLOUT;
    }

    fn set_readable(&mut self) {
        self.0 |= EPOLLIN;
    }

    fn has_readable(&self) -> bool {
        (self.0 & EPOLLIN) != 0
    }

    fn has_writable(&self) -> bool {
        (self.0 & EPOLLOUT) != 0
    }
}

impl From<EpollEvent> for libc::epoll_event {
    fn from(epoll_event: EpollEvent) -> Self {
        Self {
            events: epoll_event.events.0,
            u64: epoll_event.key.0,
        }
    }
}

impl From<libc::epoll_event> for EpollEvent {
    fn from(epoll_event: libc::epoll_event) -> Self {
        Self {
            events: EpollEvents(epoll_event.events),
            key: EventKey(epoll_event.u64),
        }
    }
}
