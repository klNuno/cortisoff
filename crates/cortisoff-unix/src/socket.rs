//! The unix transport: one socket, a thread per client.
//!
//! Never a localhost TCP port, for the reason the Windows pipe gives: any
//! process on the machine could connect to one, and this connection changes
//! the user's display and reads which applications they have open. The socket
//! is 0600 in a directory only this user can enter (`paths.rs`), and every
//! connection's peer uid is compared with ours as well, so another account is
//! turned away even on a system that ignores the socket's mode.
//!
//! The same shape as `cortisoff_win`'s pipe, so the daemon's loop needs no
//! `cfg`: an accept thread, a reader thread per client, a channel back to the
//! loop, and the waker called after every handover so a `Host::poll` blocked
//! with no timeout returns at once rather than never. A unix stream is full
//! duplex across two descriptors of one socket, so the deadlock the pipe
//! needed overlapped I/O for cannot happen: the reader parks in `read` on its
//! clone while the daemon writes on its own.

use std::fs;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use cortisoff_host::Waker;
use cortisoff_ipc::{encode, Frame, FrameReader};

use crate::paths;

/// How long the daemon waits on one client's full socket buffer before it
/// gives up on that client. A `trace` piped into something that stopped
/// reading would otherwise stall the daemon's loop, and every rule with it.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// What the reader threads hand back to the daemon's loop.
#[derive(Debug)]
pub enum Incoming {
    /// A client connected. Nothing has been asked yet.
    Connected(u64),
    Frame(u64, Frame),
    /// The socket closed, or the client sent something that is not a frame.
    /// The daemon drops every trace of the client either way.
    Gone(u64),
}

/// The same thing plus the write end, which never leaves this module.
/// Carrying it on the channel is what orders the handover of a connection
/// before the first frame its client sends.
enum Wire {
    Connected(u64, UnixStream),
    Frame(u64, Frame),
    Gone(u64),
}

/// Hand one item to the daemon's loop and wake it.
fn deliver(outbox: &Sender<Wire>, waker: Option<&Waker>, item: Wire) -> bool {
    let sent = outbox.send(item).is_ok();
    if let Some(wake) = waker {
        wake();
    }
    sent
}

pub struct SocketServer {
    name: String,
    path: PathBuf,
    /// Device and inode of the socket this server bound, so dropping it never
    /// unlinks one a newer daemon has since put at the same path.
    bound: (u64, u64),
    inbox: Receiver<Wire>,
    clients: Vec<(u64, UnixStream)>,
    closing: Arc<AtomicBool>,
}

impl SocketServer {
    /// Start accepting on this user's socket. `waker` is called after every
    /// connection, frame and hangup handed over, and a SIGINT or SIGTERM
    /// wakes it too (`console.rs`). It is registered for the signals before
    /// anything here can fail, so Ctrl-C still reaches a daemon whose socket
    /// could not be opened.
    pub fn start(waker: Option<Waker>) -> Result<Self, String> {
        if let Some(waker) = &waker {
            crate::console::set_waker(Arc::clone(waker));
        }
        let layout = paths::layout();
        paths::prepare(&layout, paths::own_uid())?;
        Self::bind(layout.socket(), waker)
    }

    /// `start` on an explicit path, for the tests.
    pub(crate) fn bind(path: PathBuf, waker: Option<Waker>) -> Result<Self, String> {
        remove_stale(&path)?;
        let listener = UnixListener::bind(&path)
            .map_err(|e| format!("could not listen on {}: {e}", path.display()))?;
        let bound = match restrict(&path) {
            Ok(bound) => bound,
            Err(e) => {
                let _ = fs::remove_file(&path);
                return Err(e);
            }
        };

        let (outbox, inbox) = channel();
        let closing = Arc::new(AtomicBool::new(false));
        let accept_closing = Arc::clone(&closing);
        let uid = paths::own_uid();
        let spawned = std::thread::Builder::new()
            .name("cortisoff-socket-accept".to_owned())
            .spawn(move || accept_loop(listener, uid, outbox, waker, accept_closing));
        if let Err(e) = spawned {
            let _ = fs::remove_file(&path);
            return Err(format!("could not start the socket thread: {e}"));
        }

        Ok(Self {
            name: path.display().to_string(),
            path,
            bound,
            inbox,
            clients: Vec::new(),
            closing,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Everything that arrived since the last call. Never blocks: the daemon's
    /// loop is already waiting in `Host::poll` and cannot afford to wait here
    /// too.
    pub fn drain(&mut self) -> Vec<Incoming> {
        let mut out = Vec::new();
        while let Ok(item) = self.inbox.try_recv() {
            out.push(match item {
                Wire::Connected(id, stream) => {
                    self.clients.push((id, stream));
                    Incoming::Connected(id)
                }
                Wire::Frame(id, frame) => Incoming::Frame(id, frame),
                Wire::Gone(id) => {
                    self.clients.retain(|(known, _)| *known != id);
                    Incoming::Gone(id)
                }
            });
        }
        out
    }

    /// Write one frame. A failed write means the client went away between the
    /// request and the answer, or stopped reading for longer than
    /// [`WRITE_TIMEOUT`], neither of them an error worth reporting. The client
    /// is hung up on, since half a frame may already be on the wire, and its
    /// reader then reports it gone.
    pub fn send(&mut self, id: u64, frame: &Frame) {
        let Some(index) = self.clients.iter().position(|(known, _)| *known == id) else {
            return;
        };
        let line = encode(frame);
        if self.clients[index].1.write_all(line.as_bytes()).is_err() {
            let (_, stream) = self.clients.remove(index);
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub fn disconnect(&mut self, id: u64) {
        if let Some(index) = self.clients.iter().position(|(known, _)| *known == id) {
            let (_, stream) = self.clients.remove(index);
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for SocketServer {
    /// Stop accepting, hang up on every client, and take the socket file
    /// away, so a client finds no daemon rather than a socket nobody answers.
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        for (_, stream) in self.clients.drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        while let Ok(item) = self.inbox.try_recv() {
            if let Wire::Connected(_, stream) = item {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
        // The accept thread is parked in `accept`; one connection wakes it to
        // read the flag and let the listener go.
        let _ = UnixStream::connect(&self.path);
        if let Ok(meta) = fs::symlink_metadata(&self.path) {
            if (meta.dev(), meta.ino()) == self.bound {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

/// Clear the way for a new socket at `path`, removing a dead daemon's file
/// only once a connect to it has been refused. A socket something answers on
/// belongs to a live daemon, and anything that is not a socket belongs to
/// someone who did not expect us to delete it.
fn remove_stale(path: &Path) -> Result<(), String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    if !meta.file_type().is_socket() {
        return Err(format!(
            "{} exists and is not a socket, refusing to replace it",
            path.display()
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(format!(
            "another daemon already listens on {}",
            path.display()
        )),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!(
                "could not remove the stale socket {}: {e}",
                path.display()
            )),
        },
        // Gone between the stat and the connect: the way is clear.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "could not tell whether {} is still in use: {e}",
            path.display()
        )),
    }
}

/// 0600 on the socket, and its identity for the drop. The directory already
/// keeps other users out; the mode is the second lock, and Linux checks it on
/// every connect.
fn restrict(path: &Path) -> Result<(u64, u64), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("could not restrict {}: {e}", path.display()))?;
    let meta = fs::symlink_metadata(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    Ok((meta.dev(), meta.ino()))
}

/// Accept, check who is calling, hand the write end to the daemon's loop and
/// the read end to a thread of its own. A unix listener queues clients in its
/// backlog, so nothing like the pipe's next-instance dance is needed for two
/// of them arriving in the same instant.
fn accept_loop(
    listener: UnixListener,
    uid: u32,
    outbox: Sender<Wire>,
    waker: Option<Waker>,
    closing: Arc<AtomicBool>,
) {
    let mut next_id = 1u64;
    loop {
        let accepted = listener.accept();
        if closing.load(Ordering::SeqCst) {
            return;
        }
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                ) =>
            {
                continue
            }
            // Out of descriptors, most likely. Retrying at once would spin a
            // core; a short pause lets a client leave and free one.
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if peer_uid(&stream).is_some_and(|peer| peer != uid) {
            continue;
        }

        let Ok(writer) = stream.try_clone() else {
            continue;
        };
        let _ = writer.set_write_timeout(Some(WRITE_TIMEOUT));
        let id = next_id;
        next_id += 1;
        // The daemon takes the write end here, before any frame can arrive on
        // it, so an answer always has somewhere to go.
        if !deliver(&outbox, waker.as_ref(), Wire::Connected(id, writer)) {
            return;
        }
        let sender = outbox.clone();
        let reader_waker = waker.clone();
        if std::thread::Builder::new()
            .name(format!("cortisoff-socket-{id}"))
            .spawn(move || read_loop(id, stream, sender, reader_waker))
            .is_err()
        {
            deliver(&outbox, waker.as_ref(), Wire::Gone(id));
        }
    }
}

fn read_loop(id: u64, mut stream: UnixStream, outbox: Sender<Wire>, waker: Option<Waker>) {
    let mut reader = FrameReader::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) => {
                deliver(&outbox, waker.as_ref(), Wire::Gone(id));
                return;
            }
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                deliver(&outbox, waker.as_ref(), Wire::Gone(id));
                return;
            }
        };
        for line in reader.push(&buffer[..read]) {
            match line {
                Ok(frame) => {
                    if !deliver(&outbox, waker.as_ref(), Wire::Frame(id, frame)) {
                        return;
                    }
                }
                // One unparseable line is enough to stop trusting the
                // connection: a client that cannot frame cannot be answered.
                Err(_) => {
                    deliver(&outbox, waker.as_ref(), Wire::Gone(id));
                    return;
                }
            }
        }
    }
}

/// Who is on the other end, as the kernel saw them at connect time. `None`
/// where this system offers no way to ask, and there the directory alone
/// keeps other users out.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` are live and sized for SO_PEERCRED.
    let ok = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (ok == 0).then_some(cred.uid)
}

#[cfg(target_vendor = "apple")]
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both out pointers are live.
    let ok = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (ok == 0).then_some(uid)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn peer_uid(_stream: &UnixStream) -> Option<u32> {
    None
}

/// Connect to this user's daemon, for a client. `None` when nothing is
/// listening, which is the normal answer when the daemon is not running.
///
/// No retry loop like the pipe's: a unix listener queues clients arriving
/// together instead of refusing all but one, so a refusal is an absence. A
/// socket in a directory that belongs to someone else is not connected to,
/// since whoever owns the directory could be answering in the daemon's place.
pub fn connect() -> Option<Client> {
    let layout = paths::layout();
    paths::check_owner(&layout.dir, paths::own_uid()).ok()?;
    connect_at(&layout.socket())
}

/// `connect` on an explicit path, for the tests.
pub(crate) fn connect_at(path: &Path) -> Option<Client> {
    UnixStream::connect(path).ok().map(|stream| Client {
        stream,
        reader: FrameReader::new(),
    })
}

/// The client half, synchronous on purpose: a CLI has nothing else to do while
/// it waits for its answer.
pub struct Client {
    stream: UnixStream,
    reader: FrameReader,
}

impl Client {
    pub fn send(&mut self, frame: &Frame) -> Result<(), String> {
        let line = encode(frame);
        self.stream
            .write_all(line.as_bytes())
            .map_err(|e| format!("the daemon closed the connection: {e}"))
    }

    /// Block until at least one frame arrives. `None` once the daemon has hung
    /// up, which is how `trace` ends.
    pub fn recv(&mut self) -> Option<Vec<Frame>> {
        let mut buffer = [0u8; 8192];
        loop {
            let read = match self.stream.read(&mut buffer) {
                Ok(0) => return None,
                Ok(read) => read,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            };
            let frames: Vec<Frame> = self
                .reader
                .push(&buffer[..read])
                .into_iter()
                .flatten()
                .collect();
            if !frames.is_empty() {
                return Some(frames);
            }
        }
    }

    /// One request and its answer on this connection, which stays open. What
    /// a client that must be the same client across requests uses: the daemon
    /// ties a slider preview to the connection that started it and ends the
    /// preview when that connection closes, so a preview sent through a
    /// throwaway connection ends the instant it is answered.
    pub fn ask(
        &mut self,
        request: cortisoff_ipc::Request,
    ) -> Result<cortisoff_ipc::Response, String> {
        self.send(&Frame::request(1, request))?;
        while let Some(frames) = self.recv() {
            for frame in frames {
                if let Frame::Response { response, .. } = frame {
                    return Ok(response);
                }
            }
        }
        Err("the daemon closed the connection without answering".to_owned())
    }
}

/// One frame in, one answer out, for a client that only wants to ask something
/// and print it. Notifications arriving in the meantime are skipped. The
/// connection closes with the answer, so anything the daemon ties to a
/// connection, a preview, ends here too: hold a `Client` for those.
pub fn ask(request: cortisoff_ipc::Request) -> Result<cortisoff_ipc::Response, String> {
    connect()
        .ok_or_else(|| "cortisoff is not running in this session".to_owned())?
        .ask(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use cortisoff_ipc::{Request, RequestId, Response};
    use std::time::Instant;

    /// A waker that reports each call on a channel, the way a host's poll
    /// would notice one.
    fn waker() -> (Waker, Receiver<()>) {
        let (woke, woken) = channel();
        let waker: Waker = Arc::new(move || {
            let _ = woke.send(());
        });
        (waker, woken)
    }

    /// A loop shaped like the daemon's, whose only wakeup is the waker, with
    /// a fallback far longer than any answer should take. An answer inside
    /// that window can only come from a client waking it.
    #[test]
    fn a_request_is_answered_through_a_real_socket() {
        let scratch = Scratch::new("ask");
        let path = scratch.join("cortisoff.sock");
        let (waker, woken) = waker();
        let (ready, started) = channel();
        let server_path = path.clone();
        std::thread::spawn(move || {
            let mut server = SocketServer::bind(server_path, Some(waker)).expect("socket");
            ready.send(()).unwrap();
            loop {
                let _ = woken.recv_timeout(Duration::from_secs(5));
                for incoming in server.drain() {
                    if let Incoming::Frame(client, Frame::Request { id, .. }) = incoming {
                        server.send(client, &Frame::response(id, Response::Ok));
                    }
                }
            }
        });
        started.recv().unwrap();

        for _ in 0..3 {
            let asked = Instant::now();
            let mut client = connect_at(&path).expect("connect");
            let answer = client.ask(Request::State).expect("answer");
            assert!(matches!(answer, Response::Ok));
            assert!(asked.elapsed() < Duration::from_millis(1_000));
        }
    }

    #[test]
    fn a_frame_calls_the_waker() {
        let scratch = Scratch::new("wake");
        let path = scratch.join("cortisoff.sock");
        let (waker, woken) = waker();
        let mut server = SocketServer::bind(path.clone(), Some(waker)).expect("socket");
        let mut client = connect_at(&path).expect("connect");

        woken
            .recv_timeout(Duration::from_secs(5))
            .expect("the connection woke the loop");
        let connected = server.drain();
        assert!(
            matches!(connected.as_slice(), [Incoming::Connected(_)]),
            "{connected:?}"
        );

        client
            .send(&Frame::request(9, Request::State))
            .expect("sent");
        woken
            .recv_timeout(Duration::from_secs(5))
            .expect("the frame woke the loop");
        let framed = server.drain();
        assert!(
            framed.iter().any(|incoming| matches!(
                incoming,
                Incoming::Frame(
                    _,
                    Frame::Request {
                        id: RequestId(9),
                        ..
                    }
                )
            )),
            "{framed:?}"
        );
    }

    #[test]
    fn the_socket_is_private() {
        let scratch = Scratch::new("mode");
        let path = scratch.join("cortisoff.sock");
        let _server = SocketServer::bind(path.clone(), None).expect("socket");
        let mode = fs::symlink_metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let scratch = Scratch::new("stale");
        let path = scratch.join("cortisoff.sock");
        // What a killed daemon leaves: the file, and nobody listening on it.
        drop(UnixListener::bind(&path).expect("listener"));
        assert!(path.exists());
        assert!(connect_at(&path).is_none());

        let _server = SocketServer::bind(path.clone(), None).expect("replaced");
        assert!(connect_at(&path).is_some());
    }

    #[test]
    fn a_live_socket_is_not_replaced() {
        let scratch = Scratch::new("live");
        let path = scratch.join("cortisoff.sock");
        let _first = SocketServer::bind(path.clone(), None).expect("first");
        let refused = SocketServer::bind(path.clone(), None)
            .err()
            .expect("a second server on a live socket");
        assert!(refused.contains("already listens"), "{refused}");
        assert!(connect_at(&path).is_some());
    }

    #[test]
    fn a_file_that_is_not_a_socket_is_left_alone() {
        let scratch = Scratch::new("file");
        let path = scratch.join("cortisoff.sock");
        fs::write(&path, b"not mine").unwrap();
        assert!(SocketServer::bind(path.clone(), None).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not mine");
    }

    #[test]
    fn dropping_the_server_removes_only_its_own_socket() {
        let scratch = Scratch::new("drop");
        let path = scratch.join("cortisoff.sock");

        drop(SocketServer::bind(path.clone(), None).expect("socket"));
        assert!(!path.exists());

        // An older server must not take away the socket a newer one bound.
        let old = SocketServer::bind(path.clone(), None).expect("old");
        fs::remove_file(&path).unwrap();
        let _new = UnixListener::bind(&path).expect("new");
        drop(old);
        assert!(path.exists());
    }

    #[test]
    fn a_disconnected_client_sees_the_end() {
        let scratch = Scratch::new("hang");
        let path = scratch.join("cortisoff.sock");
        let (waker, woken) = waker();
        let mut server = SocketServer::bind(path.clone(), Some(waker)).expect("socket");
        let mut client = connect_at(&path).expect("connect");
        woken.recv_timeout(Duration::from_secs(5)).expect("woken");
        let Some(Incoming::Connected(id)) = server.drain().into_iter().next() else {
            panic!("no connection");
        };
        server.disconnect(id);
        assert!(client.recv().is_none());
        let refused = client.ask(Request::State).unwrap_err();
        assert!(
            refused.contains("the daemon closed the connection"),
            "{refused}"
        );
    }
}
