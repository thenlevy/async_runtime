use {
    async_broadcast::{Receiver, Sender},
    futures::future::Either,
    smol::{
        Timer,
        io::{AsyncBufReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    },
    std::time::Duration,
};

struct Server {
    listener: TcpListener,
    event_receiver: Receiver<Event>,
    event_emiter: Sender<Event>,
}

#[derive(Clone, Copy)]
enum Event {
    TimeOut,
    SomeoneWon,
}

impl Server {
    async fn new(addr: &str, game_duration: Duration) -> Self {
        let listener = TcpListener::bind(addr).await.unwrap();
        let (event_emiter, event_receiver) = async_broadcast::broadcast(2);
        let timer_emitter = event_emiter.clone();
        smol::spawn(async move {
            Timer::after(game_duration).await;
            timer_emitter.broadcast(Event::TimeOut).await.unwrap();
        })
        .detach();
        Self {
            listener,
            event_emiter,
            event_receiver,
        }
    }

    async fn run(&mut self) {
        loop {
            match futures::future::select(
                self.event_receiver.recv(),
                Box::pin(self.listener.accept()),
            )
            .await
            {
                Either::Left((_event, _)) => {
                    break;
                }
                Either::Right((stream, _)) => {
                    let (reader, writer) =
                        smol::io::split(stream.expect("Failed to accept connection").0);
                    let connection = Connection {
                        reader: smol::io::BufReader::new(reader),
                        writer,
                        target: rand::random_range(1..=100),
                        event_emiter: self.event_emiter.clone(),
                        event_receiver: self.event_receiver.clone(),
                    };
                    smol::spawn(connection.handle()).detach();
                }
            }
        }
    }
}

struct Connection {
    reader: smol::io::BufReader<smol::io::ReadHalf<TcpStream>>,
    writer: smol::io::WriteHalf<TcpStream>,
    /// The number that the client is trying to guess
    target: usize,
    event_receiver: Receiver<Event>,
    event_emiter: Sender<Event>,
}

impl Connection {
    async fn handle(mut self) {
        let mut guess_str = String::new();
        loop {
            guess_str.clear();
            match futures::future::select(
                self.reader.read_line(&mut guess_str),
                self.event_receiver.recv(),
            )
            .await
            {
                Either::Left((result, _)) => {
                    let n = result.unwrap();
                    if n == 0 {
                        continue;
                    }
                    if let Ok(guess) = guess_str.trim().parse::<usize>() {
                        if guess < self.target {
                            self.writer.write_all(b"Too low!\n").await.unwrap();
                        } else if guess > self.target {
                            self.writer.write_all(b"Too high!\n").await.unwrap();
                        } else {
                            self.writer.write_all(b"Correct!\n").await.unwrap();
                            self.event_emiter
                                .broadcast(Event::SomeoneWon)
                                .await
                                .unwrap();
                            break;
                        }
                    } else {
                        self.writer
                            .write_all(b"Invalid input. Please enter a number.\n")
                            .await
                            .unwrap();
                    }
                    continue;
                }
                Either::Right((event, _)) => {
                    match event.unwrap() {
                        Event::SomeoneWon => {
                            self.writer
                                .write_all(b"Game over! Someone else won.\n")
                                .await
                                .unwrap();
                        }
                        Event::TimeOut => {
                            self.writer
                                .write_all(b"Game over! Time's up.\n")
                                .await
                                .unwrap();
                        }
                    }
                    break;
                }
            }
        }
    }
}

pub fn execute() {
    smol::block_on(async {
        let mut server = Server::new("127.0.0.1:8080", Duration::from_secs(30)).await;
        server.run().await;
    });
}
