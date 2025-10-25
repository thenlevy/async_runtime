# Single threaded multi-task server

This repository serves as an example of how to perform concurent tasks on a single thread.

The goal is to implement a TCP server that:

- Starts a timer on startup
- Accepts incoming connexion and make them play a "Guesing Game"

The server must broadcast a message to all conected clients, and then shutdown on one of the following event:

- The timer has reached a deadline
- One of the client has won the Guessing Game.

Therefore the server must concurently handles conexions and check the timer. The goal is to do it while being idle most of the time.

We propose two solutions to this exercise:

- One that relies heavily on the `async/await` features of Rust, and that is using a well established async runtime
- One where we implement Futures ourselves and write a custom runtime.
