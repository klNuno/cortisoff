//! Asks the running daemon the same read-only question from several threads
//! at the same instant, which is what the dashboard does on every refresh.
//!
//! `cargo run -p cortisoff-win --example burst -- 3 20`: three clients at
//! once, twenty rounds. Prints one line per round, and how many connections
//! were refused. Nothing is applied and nothing is changed.

#[cfg(not(windows))]
fn main() {
    eprintln!("this example drives Windows APIs and runs on Windows only");
}

#[cfg(windows)]
fn main() {
    imp::main();
}

#[cfg(windows)]
mod imp {
    use std::sync::{Arc, Barrier};

    use cortisoff_ipc::Request;

    pub fn main() {
        let clients: usize = std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        let rounds: usize = std::env::args()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        let session = cortisoff_win::session_id();

        let mut refused = 0;
        for round in 1..=rounds {
            let barrier = Arc::new(Barrier::new(clients));
            let handles: Vec<_> = (0..clients)
                .map(|_| {
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        cortisoff_win::ask(session, Request::State).map(|_| ())
                    })
                })
                .collect();
            let errors: Vec<String> = handles
                .into_iter()
                .filter_map(|h| h.join().expect("a client thread panicked").err())
                .collect();
            refused += errors.len();
            println!(
                "round {round}: {} of {clients} answered{}",
                clients - errors.len(),
                errors
                    .first()
                    .map(|e| format!(", first error: {e}"))
                    .unwrap_or_default()
            );
        }
        println!("{refused} refused over {rounds} rounds");
    }
}
