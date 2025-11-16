use crate::a_la_mano::reactor::{IoSource, Reactor};

use std::{
    cell::RefCell,
    io::{Read, Write},
    marker::Unpin,
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, ready},
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
}

impl Unpin for AsyncTcpStream {}

impl AsyncTcpStream {
    pub fn from_tcp_stream(stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;

        let fd = Rc::new(OwnedFd::from(stream));
        let source = Reactor::add_source(fd.clone())?;
        dbg!("AsyncTcpStream fd: {}", source.borrow().get_raw_fd());
        Ok(Self { _inner: fd, source })
    }

    pub fn get_lines(&self) -> TcpStreamLines<'_> {
        TcpStreamLines::new(self)
    }

    pub fn write_all(&self, buf: &[u8]) -> impl Future<Output = std::io::Result<()>> {
        TcpStreamWriteAll::new(self, buf)
    }
}

pub struct TcpStreamLines<'a> {
    inner: BorrowedFd<'a>,
    source: Rc<RefCell<IoSource>>,
    buf: Box<[u8; BUF_SIZE]>,
    pos: usize,
    cap: usize,
    next_line: Vec<u8>,
}

impl<'a> TcpStreamLines<'a> {
    fn new(stream: &'a AsyncTcpStream) -> Self {
        Self {
            inner: stream._inner.as_ref().as_fd(),
            source: stream.source.clone(),
            buf: Box::new([0; BUF_SIZE]),
            pos: 0,
            cap: 0,
            next_line: Vec::new(),
        }
    }

    fn poll_line(&mut self, cx: &mut Context<'_>) -> Poll<Option<std::io::Result<String>>> {
        loop {
            eprintln!("Buffer pos: {}, cap: {}", self.pos, self.cap);
            // If we have consumed all the bytes of the buffer, fill it
            if self.pos >= self.cap {
                // SAFETY `self._inner` is open because we own a valid reference to it and is a
                // valid fd for a TcpStream.
                let mut stream = unsafe { TcpStream::from_raw_fd(self.inner.as_raw_fd()) };
                eprintln!("Poll reading");
                self.cap = match stream.read(self.buf.as_mut_slice()) {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        eprintln!("Would block");
                        if let Err(e) = self.source.borrow_mut().add_reader(cx.waker().clone()) {
                            let _ = stream.into_raw_fd();
                            return Poll::Ready(Some(Err(e)));
                        }

                        // Drop the stream without closing the associated file
                        let _ = stream.into_raw_fd();
                        return Poll::Pending;
                    }
                    ret => {
                        eprintln!("Ready");
                        self.pos = 0;
                        let _ = stream.into_raw_fd();
                        ret
                    }
                }?;
            }
            if self.cap == 0 {
                return Poll::Ready(None);
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
                    return Poll::Ready(Some(Ok(ret)));
                } else {
                    return Poll::Ready(Some(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Not utf8",
                    ))));
                }
            } else {
                self.next_line
                    .extend_from_slice(&self.buf[self.pos..self.cap]);
                self.pos = self.cap;
            }
        }
    }

    pub fn next(&'a mut self) -> TcpLinesNext<'a> {
        TcpLinesNext { lines: self }
    }

    pub fn for_each<F, Fut>(self, f: F) -> TcpLinesForEach<'a, Fut, F>
    where
        Fut: Future<Output = ()> + Unpin,
        F: FnMut(String) -> Fut,
    {
        TcpLinesForEach {
            lines: self,
            f,
            future: None,
        }
    }
}

struct TcpLinesNext<'a> {
    lines: &'a mut TcpStreamLines<'a>,
}

impl<'a> Future for TcpLinesNext<'a> {
    type Output = Option<std::io::Result<String>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::get_mut(self);
        this.lines.poll_line(cx)
    }
}

pub struct TcpLinesForEach<'a, Fut, F>
where
    Fut: Future<Output = ()> + Unpin,
    F: FnMut(String) -> Fut,
{
    lines: TcpStreamLines<'a>,
    f: F,
    future: Option<Fut>,
}

impl<'a, Fut, F> Unpin for TcpLinesForEach<'a, Fut, F>
where
    Fut: Future<Output = ()> + Unpin,
    F: FnMut(String) -> Fut,
{
}

impl<'a, Fut, F> Future for TcpLinesForEach<'a, Fut, F>
where
    Fut: Future<Output = ()> + Unpin,
    F: FnMut(String) -> Fut,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::get_mut(self);

        loop {
            if let Some(fut) = this.future.as_mut() {
                ready!(Pin::new(fut).poll(cx));
                this.future = None;
            } else if let Some(item) = ready!(this.lines.poll_line(cx)) {
                if let Ok(line) = item {
                    this.future = Some((this.f)(line));
                } else {
                    println!("Error while reading line: {:?}", item.err());
                    this.future = None;
                }
            } else {
                break;
            }
        }
        Poll::Ready(())
    }
}

struct TcpStreamWriteAll<'a, 'b> {
    inner: BorrowedFd<'a>,
    buf: &'b [u8],
    source: Rc<RefCell<IoSource>>,
}

impl Unpin for TcpStreamWriteAll<'_, '_> {}

impl<'a, 'b> TcpStreamWriteAll<'a, 'b> {
    fn new(stream: &'a AsyncTcpStream, buf: &'b [u8]) -> Self {
        Self {
            inner: stream._inner.as_ref().as_fd(),
            buf,
            source: stream.source.clone(),
        }
    }
}

impl<'a, 'b> Future for TcpStreamWriteAll<'a, 'b> {
    type Output = std::io::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::get_mut(self);
        // SAFETY `self._inner` is open because we own it and is a valid fd for a TcpStream.
        let mut stream = unsafe { TcpStream::from_raw_fd(this.inner.as_raw_fd()) };

        while !this.buf.is_empty() {
            match stream.write(this.buf) {
                Ok(0) => {
                    let _ = stream.into_raw_fd();
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    )));
                }
                Ok(n) => {
                    this.buf = &this.buf[n..];
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    let _ = stream.into_raw_fd();
                    if let Err(e) = this.source.borrow_mut().add_writer(cx.waker().clone()) {
                        return Poll::Ready(Err(e));
                    }
                    return Poll::Pending;
                }
                Err(e) => {
                    let _ = stream.into_raw_fd();
                    return Poll::Ready(Err(e));
                }
            }
        }
        let _ = stream.into_raw_fd();
        Poll::Ready(Ok(()))
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
