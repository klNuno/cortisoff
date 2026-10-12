//! The window's connection to the daemon.
//!
//! Two connections, each on its own thread, because the transport's client is
//! synchronous and a pipe handle serves one blocking call at a time. One
//! subscribes and only reads notifications. The other carries requests in
//! order and stays open, which matters: the daemon ties a slider preview to
//! the connection that started it, and ends it when that connection closes.
//! Neither thread ever blocks the frame; both wake it when something arrives.

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use cortisoff_ipc::{Frame, Notification, Request, Response};

/// What a request was for, so its answer lands in the right place.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    State,
    Plugins,
    Doctor,
    /// The update setting and the daemon's last check.
    Updates,
    /// Anything whose answer only matters when it is an error: say so.
    Do(String),
}

#[derive(Debug)]
pub enum Inbound {
    /// The subscription is up: a daemon is listening.
    Connected,
    /// No daemon, or it went away.
    Disconnected,
    Notification(Notification),
    Answer(Ask, Response),
    /// A request could not be delivered at all.
    Failed(Ask, String),
}

pub struct Link {
    requests: Sender<(Ask, Request)>,
    inbound: Receiver<Inbound>,
}

impl Link {
    /// Start both threads. `wake` is called after anything arrives, so the
    /// window repaints only when there is something new to show.
    pub fn start(wake: impl Fn() + Send + Sync + Clone + 'static) -> Link {
        let (requests, queue) = channel::<(Ask, Request)>();
        let (out, inbound) = channel();

        let tx = out.clone();
        let wake_sub = wake.clone();
        std::thread::Builder::new()
            .name("cortisoff-subscribe".into())
            .spawn(move || subscribe(tx, wake_sub))
            .expect("a thread for the subscription");

        std::thread::Builder::new()
            .name("cortisoff-requests".into())
            .spawn(move || serve(queue, out, wake))
            .expect("a thread for requests");

        Link { requests, inbound }
    }

    pub fn ask(&self, ask: Ask, request: Request) {
        let _ = self.requests.send((ask, request));
    }

    pub fn drain(&self) -> Vec<Inbound> {
        let mut out = Vec::new();
        loop {
            match self.inbound.try_recv() {
                Ok(item) => out.push(item),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return out,
            }
        }
    }
}

fn subscribe(out: Sender<Inbound>, wake: impl Fn()) {
    let mut announced_down = false;
    loop {
        let subscribed = platform::connect().and_then(|mut client| {
            client.send(&Frame::request(1, Request::Subscribe)).ok()?;
            Some(client)
        });
        let Some(mut client) = subscribed else {
            if !announced_down {
                announced_down = out.send(Inbound::Disconnected).is_ok();
                wake();
            }
            platform::wait_for_daemon();
            continue;
        };
        if out.send(Inbound::Connected).is_err() {
            return;
        }
        wake();
        while let Some(frames) = client.recv() {
            for frame in frames {
                if let Frame::Notification { notification } = frame {
                    if out.send(Inbound::Notification(notification)).is_err() {
                        return;
                    }
                }
            }
            wake();
        }
        announced_down = out.send(Inbound::Disconnected).is_ok();
        wake();
        platform::wait_for_daemon();
    }
}

fn serve(queue: Receiver<(Ask, Request)>, out: Sender<Inbound>, wake: impl Fn()) {
    let mut client: Option<platform::Client> = None;
    while let Ok(first) = queue.recv() {
        // A drag sends a preview per frame. Only the latest per capability is
        // worth the round trip; the others are already out of date.
        let mut batch = vec![first];
        while let Ok(next) = queue.try_recv() {
            if let (_, Request::Preview { cap, .. }) = &next {
                batch.retain(|(_, r)| !matches!(r, Request::Preview { cap: c, .. } if c == cap));
            }
            batch.push(next);
        }
        for (ask, request) in batch {
            if client.is_none() {
                client = platform::connect();
            }
            let Some(c) = client.as_mut() else {
                let _ = out.send(Inbound::Failed(ask, "the daemon is not running".into()));
                continue;
            };
            match c.ask(request) {
                Ok(response) => {
                    let _ = out.send(Inbound::Answer(ask, response));
                }
                Err(e) => {
                    client = None;
                    let _ = out.send(Inbound::Failed(ask, e));
                }
            }
        }
        wake();
    }
}

/// Whether a daemon answers right now, without keeping the connection.
pub fn daemon_running() -> bool {
    platform::connect().is_some()
}

/// One request on a connection of its own, for the paths that run before or
/// without a window: `--uninstall`.
pub fn ask_once(request: Request) -> Result<Response, String> {
    platform::connect()
        .ok_or_else(|| "the daemon is not running".to_owned())?
        .ask(request)
}

#[cfg(windows)]
mod platform {
    pub use cortisoff_win::Client;

    pub fn connect() -> Option<Client> {
        cortisoff_win::connect(cortisoff_win::session_id())
    }

    /// Parked on the event a daemon sets once it listens.
    pub fn wait_for_daemon() {
        cortisoff_win::wait_for_daemon(cortisoff_win::session_id());
    }
}

#[cfg(unix)]
mod platform {
    pub use cortisoff_unix::Client;

    /// How often a missing daemon is looked for again. The socket has no
    /// "now listening" signal yet; Windows has one (`TODO.md`).
    const RETRY: std::time::Duration = std::time::Duration::from_millis(700);

    pub fn connect() -> Option<Client> {
        cortisoff_unix::connect()
    }

    pub fn wait_for_daemon() {
        std::thread::sleep(RETRY);
    }
}
