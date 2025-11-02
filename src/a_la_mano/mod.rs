mod executor;
mod reactor;
mod tcp;

use std::{cell::RefCell, rc::Rc};

async fn run(reactor: Rc<RefCell<reactor::Reactor>>) {
    let tcp_listener = tcp::AsyncTcpListener::bind("127.0.0.1:8080", reactor.clone())
        .expect("Failed to bind TCP listener");

    loop {
        println!("Waiting for incoming TCP connections...");
        let (tcp_stream, _addr) = tcp_listener
            .accept()
            .await
            .expect("Failed to accept TCP connection");

        println!("Accepted connection from {:?}", tcp_stream.peer_addr());

        // Handle the TCP connection (e.g., read/write data) here.
    }
}

pub fn start() {
    let mut executor = executor::Executor::new();
    executor.block_on(run(executor.reactor.clone()));
}
