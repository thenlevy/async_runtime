use crate::a_la_mano::reactor::{IoSource, Reactor};

use std::{
    cell::RefCell,
    marker::Unpin,
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::{FromRawFd, IntoRawFd, OwnedFd},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

pub struct AsyncTcpListener {
    inner: Rc<OwnedFd>,
    source: Rc<RefCell<IoSource>>,
    reactor: Rc<RefCell<Reactor>>,
}

impl AsyncTcpListener {
    pub fn bind(addr: &str, reactor: Rc<RefCell<Reactor>>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let fd = Rc::new(OwnedFd::from(listener));
        let source = reactor.borrow_mut().add_source(fd.clone())?;
        Ok(Self {
            inner: fd,
            source,
            reactor,
        })
    }

    pub fn accept(&self) -> TcpConnectionAccept {
        TcpConnectionAccept {
            state: TcpConnectionAcceptState::Start,
            source: self.source.clone(),
            reactor: self.reactor.clone(),
        }
    }
}

pub struct TcpConnectionAccept {
    source: Rc<RefCell<IoSource>>,
    state: TcpConnectionAcceptState,
    reactor: Rc<RefCell<Reactor>>,
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
        let source = self.source.borrow();
        let mut currently_waiting_for = source.waiting_for();

        if !currently_waiting_for.readable {
            println!("Registering interest in readability for TcpListener");
            let fd = source.borrow_fd();
            currently_waiting_for.readable = true;
            self.reactor
                .borrow_mut()
                .register_interest(fd, currently_waiting_for)
                .unwrap();
            std::mem::drop(source);
        }
        self.source.borrow_mut().add_reader(cx.waker().clone());
        self.state = TcpConnectionAcceptState::WokenWhenReady;
        Poll::Pending
    }

    fn poll_assume_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<(TcpStream, SocketAddr)>> {
        // SAFETY: The fd of self.source is a valid TCP listener fd.
        let tcp_listener = unsafe { TcpListener::from_raw_fd(self.source.borrow().get_raw_fd()) };

        match tcp_listener.accept() {
            Ok(ret) => {
                // Drop the TcpListener without closing the fd.
                let _ = tcp_listener.into_raw_fd();
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
