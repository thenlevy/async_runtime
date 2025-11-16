mod executor;
mod reactor;
mod tcp;

use executor::Executor;

use std::rc::Rc;

async fn run() {
    let tcp_listener =
        tcp::AsyncTcpListener::bind("127.0.0.1:8080").expect("Failed to bind TCP listener");

    loop {
        eprintln!("Waiting for incoming TCP connections...");
        let (tcp_stream, _addr) = tcp_listener
            .accept()
            .await
            .expect("Failed to accept TCP connection");

        println!("Accepted connection from {:?}", tcp_stream.peer_addr());
        if let Err(e) = Executor::spawn(handle_connection(Rc::new(
            tcp::AsyncTcpStream::from_tcp_stream(tcp_stream).unwrap(),
        ))) {
            println!("Failed to spawn task: {e}");
        }
    }
}

async fn handle_connection(stream: Rc<tcp::AsyncTcpStream>) {
    stream
        .get_lines()
        .for_each({
            |mut line| {
                let spawn_handle = Rc::clone(&stream);
                Box::pin(async move {
                    line = format!("{}!!!\n", line.to_uppercase());
                    spawn_handle
                        .write_all(line.as_bytes())
                        .await
                        .inspect_err(|e| {
                            println!("Error while writing line back to client: {e:?}");
                        })
                        .ok();
                })
            }
        })
        .await;
}

pub fn start() {
    Executor::block_on(run());
}
