mod executor;
mod reactor;
mod tcp;

use executor::Executor;

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
        if let Err(e) = Executor::spawn(handle_connection(
            tcp::AsyncTcpStream::from_tcp_stream(tcp_stream).unwrap(),
        )) {
            println!("Failed to spawn task: {e}");
        }
    }
}

async fn handle_connection(mut stream: tcp::AsyncTcpStream) {
    while let Ok(Some(line)) = stream
        .get_line()
        .await
        .inspect_err(|e| println!("Error while reading line: {e:?}"))
    {
        println!("{line}")
    }
}

pub fn start() {
    Executor::block_on(run());
}
