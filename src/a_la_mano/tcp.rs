use crate::a_la_mano::reactor::{IoSource, Reactor};

use std::{
    cell::RefCell,
    io::Read,
    marker::Unpin,
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::{FromRawFd, IntoRawFd, OwnedFd},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

pub struct AsyncTcpListener {
    _inner: Rc<OwnedFd>,
    source: Rc<RefCell<IoSource>>,
}

impl AsyncTcpListener {
    pub fn bind(addr: &str) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let fd = Rc::new(OwnedFd::from(listener));
        let source = Reactor::add_source(fd.clone())?;
        Ok(Self { _inner: fd, source })
    }

    pub fn accept(&self) -> TcpConnectionAccept {
        TcpConnectionAccept {
            state: TcpConnectionAcceptState::Start,
            source: self.source.clone(),
        }
    }
}

const BUF_SIZE: usize = 4096;

pub struct AsyncTcpStream {
    _inner: Rc<OwnedFd>,
    source: Rc<RefCell<IoSource>>,
    buf: Box<[u8; BUF_SIZE]>,
    pos: usize,
    cap: usize,
    next_line: Vec<u8>,
}

impl Unpin for AsyncTcpStream {}

impl AsyncTcpStream {
    pub fn from_tcp_stream(stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;

        let fd = Rc::new(OwnedFd::from(stream));
        let source = Reactor::add_source(fd.clone())?;
        println!("AsyncTcpStream fd: {}", source.borrow().get_raw_fd());
        Ok(Self {
            _inner: fd,
            source,
            buf: Box::new([0; BUF_SIZE]),
            pos: 0,
            cap: 0,
            next_line: Vec::new(),
        })
    }

    pub fn get_line(&mut self) -> impl Future<Output = std::io::Result<Option<String>>> {
        NextTcpStreamLine { stream: self }
    }

    fn poll_line(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<Option<String>>> {
        loop {
            // If we have consumed all the bytes of the buffer, fill it
            if self.pos >= self.cap {
                // SAFETY `self._inner` is open because we own it and is a valid fd for a TcpStream.
                let mut stream =
                    unsafe { TcpStream::from_raw_fd(self.source.borrow().get_raw_fd()) };
                self.pos = 0;
                println!("Poll reading");
                self.cap = match stream.read(self.buf.as_mut_slice()) {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        println!("Would block");
                        if let Err(e) = self.source.borrow_mut().add_reader(cx.waker().clone()) {
                            let _ = stream.into_raw_fd();
                            return Poll::Ready(Err(e));
                        }

                        // Drop the stream without closing the associated file
                        let _ = stream.into_raw_fd();
                        return Poll::Pending;
                    }
                    ret => {
                        println!("Ready");
                        let _ = stream.into_raw_fd();
                        ret
                    }
                }?;
            }
            if self.cap == 0 {
                return Poll::Ready(Ok(None));
            }

            if let Some(i) = self.buf[self.pos..self.cap]
                .iter()
                .position(|b| *b == b'\n')
            {
                // Do not take the \n
                self.next_line
                    .extend_from_slice(&self.buf[self.pos..(self.pos + i)]);
                self.pos += i + 1;
                if let Ok(mut ret) = String::from_utf8(std::mem::take(&mut self.next_line)) {
                    if ret.ends_with('\r') {
                        ret.pop();
                    }
                    return Poll::Ready(Ok(Some(ret)));
                } else {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Not utf8",
                    )));
                }
            } else {
                self.next_line
                    .extend_from_slice(&self.buf[self.pos..self.cap]);
                self.pos = self.cap;
            }
        }
    }
}

struct NextTcpStreamLine<'a> {
    stream: &'a mut AsyncTcpStream,
}

impl<'a> Future for NextTcpStreamLine<'a> {
    type Output = std::io::Result<Option<String>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::get_mut(self);
        this.stream.poll_line(cx)
    }
}

pub struct TcpConnectionAccept {
    source: Rc<RefCell<IoSource>>,
    state: TcpConnectionAcceptState,
}

impl Unpin for TcpConnectionAccept {}

#[derive(Debug, Clone, Copy)]
enum TcpConnectionAcceptState {
    Start,
    FirstAttemptBlocked,
    WokenWhenReady,
    Finished,
}

impl TcpConnectionAccept {
    fn poll_start(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<(TcpStream, SocketAddr)>> {
        println!("Polling TcpConnectionAccept in Start state");

        let source = self.source.borrow();
        // SAFETY: The fd of self.source is a valid TCP listener fd.
        let tcp_listener = unsafe { TcpListener::from_raw_fd(source.get_raw_fd()) };

        std::mem::drop(source);

        let ret = tcp_listener.accept();
        println!("First accept attempt returned: {:?}", ret);
        match ret {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                self.state = TcpConnectionAcceptState::FirstAttemptBlocked;
                // Drop the TcpListener without closing the fd.
                let _ = tcp_listener.into_raw_fd();
                self.poll_first_attempt_blocked(cx)
            }
            res => {
                // Drop the TcpListener without closing the fd.
                let _ = tcp_listener.into_raw_fd();
                Poll::Ready(res)
            }
        }
    }

    fn poll_first_attempt_blocked(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<(TcpStream, SocketAddr)>> {
        if let Err(e) = self.source.borrow_mut().add_reader(cx.waker().clone()) {
            return Poll::Ready(Err(e));
        };
        self.state = TcpConnectionAcceptState::WokenWhenReady;
        Poll::Pending
    }

    fn poll_assume_ready(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<(TcpStream, SocketAddr)>> {
        // SAFETY: The fd of self.source is a valid TCP listener fd.
        let tcp_listener = unsafe { TcpListener::from_raw_fd(self.source.borrow().get_raw_fd()) };

        match tcp_listener.accept() {
            Ok(ret) => {
                // Drop the TcpListener without closing the fd.
                let _ = tcp_listener.into_raw_fd();
                self.state = TcpConnectionAcceptState::Finished;
                Poll::Ready(Ok(ret))
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                panic!("TcpListener was not actually ready");
            }
            Err(e) => {
                // Drop the TcpListener without closing the fd.
                let _ = tcp_listener.into_raw_fd();
                Poll::Ready(Err(e))
            }
        }
    }
}

impl Future for TcpConnectionAccept {
    type Output = std::io::Result<(TcpStream, SocketAddr)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.as_mut();
        match this.state {
            TcpConnectionAcceptState::Start => this.poll_start(cx),
            TcpConnectionAcceptState::FirstAttemptBlocked => this.poll_first_attempt_blocked(cx),
            TcpConnectionAcceptState::WokenWhenReady => this.poll_assume_ready(cx),
            TcpConnectionAcceptState::Finished => {
                panic!("poll called after TcpConnectionAccept is finished")
            }
        }
    }
}
