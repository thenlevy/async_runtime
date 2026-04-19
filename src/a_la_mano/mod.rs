mod executor;
mod reactor;
mod tcp;

use executor::Executor;

use std::rc::Rc;

async fn run() {
    let tcp_listener =
        tcp::AsyncTcpListener::bind("127.0.0.1:8080").expect("Failed to bind TCP listener");

    loop {
        let (tcp_stream, _addr) = tcp_listener
            .accept()
            .await
            .expect("Failed to accept TCP connection");

        let stream = Rc::new(
            tcp::AsyncTcpStream::from_tcp_stream(tcp_stream).expect("Failed to wrap stream"),
        );
        if let Err(e) = Executor::spawn(handle_connection(stream)) {
            eprintln!("Failed to spawn task: {e}");
        }
    }
}

async fn handle_connection(stream: Rc<tcp::AsyncTcpStream>) {
    let mut lines = stream.get_lines();

    while let Some(line) = lines.next().await {
        let Ok(line) = line.inspect_err(|e| {
            eprintln!("Error while reading line: {e:?}");
        }) else {
            continue;
        };

        let reply = format!("{}!!!\n", line.to_uppercase());
        if let Err(e) = stream.write_all(reply.as_bytes()).await {
            eprintln!("Error while writing line back to client: {e:?}");
        }
    }
}

pub fn start() {
    Executor::block_on(run());
}
