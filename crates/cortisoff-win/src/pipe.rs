//! The Windows transport: a named pipe, one instance per client.
//!
//! Never a localhost TCP socket. Any process on the machine can connect to one
//! of those, and this connection can change the user's display and read which
//! applications they have open. The pipe carries a security descriptor naming
//! the session's own user and nobody else, so another account on the same
//! machine is refused by the kernel rather than by a check this code could
//! forget to write.
//!
//! A reader thread per client and a channel back to the daemon's loop. The
//! loop is a Windows message pump and cannot also wait on a pipe without an
//! I/O completion port or a restructured pump; a thread costs a few kilobytes
//! of stack and no restructuring at all. The price is that a request waits for
//! the next turn of the loop, bounded by the daemon's tick.
//!
//! The handles are opened overlapped, and that is not optional. A synchronous
//! handle has one I/O queue: the reader thread parked in `ReadFile` blocks the
//! daemon's `WriteFile` on the same handle, and the client waits forever for
//! an answer that is queued behind the read waiting for that answer. Two event
//! handles per connection, one per direction, are what break it.

use std::ffi::c_void;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use cortisoff_ipc::{encode, Frame, FrameReader};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcessToken, ResetEvent,
};
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

/// Room for a doctor report on a machine with many rules, and small enough
/// that a client cannot make the daemon allocate anything interesting.
const BUFFER: u32 = 64 * 1024;

/// What the reader threads hand back to the daemon's loop.
pub enum Incoming {
    /// A client connected. Nothing has been asked yet.
    Connected(u64),
    Frame(u64, Frame),
    /// The pipe closed, or the client sent something that is not a frame. The
    /// daemon drops every trace of the client either way.
    Gone(u64),
}

/// The same thing plus the write end, which never leaves this module. The
/// accept thread owns the connection until the daemon's loop takes it, and
/// carrying it on the channel is what keeps that handover ordered against the
/// first frame the client sends.
enum Wire {
    Connected(u64, Arc<Pipe>),
    Frame(u64, Frame),
    Gone(u64),
}

/// A bare handle. Moved to the thread that will own it, or held by a client
/// that reads and writes in turn and never at once. Whoever adopts it closes
/// it, which is why this does not.
struct Handle(HANDLE);
unsafe impl Send for Handle {}

/// One connection: the pipe instance, plus an event per direction so a read
/// and a write can be in flight at the same time.
struct Pipe {
    handle: HANDLE,
    reading: HANDLE,
    writing: HANDLE,
}

// Read on the reader thread, write on the daemon's, never duplicated and never
// closed while either runs.
unsafe impl Send for Pipe {}
unsafe impl Sync for Pipe {}

impl Pipe {
    fn adopt(handle: HANDLE) -> Option<Self> {
        Some(Self {
            handle,
            reading: event()?,
            writing: event()?,
        })
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe {
            let _ = DisconnectNamedPipe(self.handle);
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.reading);
            let _ = CloseHandle(self.writing);
        }
    }
}

/// Manual reset, unsignalled. One per direction per connection, reused for
/// every operation in that direction.
fn event() -> Option<HANDLE> {
    unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok()
}

/// Run one overlapped operation to completion. `start` returns whether the
/// call was accepted; anything other than a clean success or `ERROR_IO_PENDING`
/// is the connection ending.
///
/// Blocking here is the point: the caller is either a reader thread with
/// nothing else to do, or the daemon writing an answer it has already decided
/// on. What it must not do is block the *other* direction, and an overlapped
/// handle with its own event per direction is what guarantees that.
unsafe fn finish(
    handle: HANDLE,
    event: HANDLE,
    start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
) -> Option<u32> {
    let mut overlapped = OVERLAPPED {
        hEvent: event,
        ..Default::default()
    };
    let _ = ResetEvent(event);
    match start(&mut overlapped) {
        Ok(()) => {}
        Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_IO_PENDING.0) => {}
        Err(_) => return None,
    }
    let mut moved = 0u32;
    GetOverlappedResult(handle, &overlapped, &mut moved, true)
        .ok()
        .map(|()| moved)
}

pub struct PipeServer {
    name: String,
    inbox: Receiver<Wire>,
    clients: Vec<(u64, Arc<Pipe>)>,
    /// Kept so the accept thread's sender never hangs up while the server
    /// lives, which would end the daemon's inbox early.
    _outbox: Sender<Wire>,
}

impl PipeServer {
    /// Start accepting. The name is per session, because cortisoff is one
    /// process per session and two sessions on the same machine must not share
    /// a daemon.
    pub fn start(session: u32) -> Result<Self, String> {
        let name = format!(r"\\.\pipe\cortisoff.{session}");
        let sddl = own_user_only()?;
        // Fail here rather than on the first connection: a daemon that cannot
        // be talked to should say so at startup.
        let first = create_instance(&name, &sddl)?;

        let (outbox, inbox) = channel();
        let accept_name = name.clone();
        let accept_sender = outbox.clone();
        // A raw `HANDLE` is not `Send`, and this one genuinely does move: the
        // accept thread owns it from here and this thread never touches it
        // again.
        let first = Handle(first);
        std::thread::Builder::new()
            .name("cortisoff-pipe-accept".to_owned())
            .spawn(move || {
                let first = first;
                accept_loop(accept_name, sddl, first.0, accept_sender)
            })
            .map_err(|e| format!("could not start the pipe thread: {e}"))?;

        Ok(Self {
            name,
            inbox,
            clients: Vec::new(),
            _outbox: outbox,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Everything that arrived since the last call. Never blocks: the daemon's
    /// loop is already waiting on the message pump and cannot afford to wait
    /// here too.
    pub fn drain(&mut self) -> Vec<Incoming> {
        let mut out = Vec::new();
        while let Ok(item) = self.inbox.try_recv() {
            out.push(match item {
                Wire::Connected(id, pipe) => {
                    self.clients.push((id, pipe));
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
    /// request and the answer, which is not an error worth reporting.
    pub fn send(&mut self, id: u64, frame: &Frame) {
        let Some((_, pipe)) = self.clients.iter().find(|(known, _)| *known == id) else {
            return;
        };
        let line = encode(frame);
        let bytes = line.as_bytes();
        let written = unsafe {
            finish(pipe.handle, pipe.writing, |o| {
                WriteFile(pipe.handle, Some(bytes), None, Some(o))
            })
        };
        if written != Some(bytes.len() as u32) {
            self.clients.retain(|(known, _)| *known != id);
        }
    }

    pub fn disconnect(&mut self, id: u64) {
        self.clients.retain(|(known, _)| *known != id);
    }
}

/// Wait for a client, hand the connected instance to a reader thread, then
/// immediately create the next instance and wait again.
///
/// Creating the next instance before serving the current one is what stops a
/// second client being refused while the first is connected.
fn accept_loop(name: String, sddl: String, first: HANDLE, outbox: Sender<Wire>) {
    let mut waiting = first;
    let mut next_id = 1u64;
    loop {
        // ERROR_PIPE_CONNECTED means the client arrived between the create and
        // this call, which is a connection, not a failure.
        let connected = match Pipe::adopt(waiting) {
            Some(pipe) => {
                let ok = unsafe {
                    finish(pipe.handle, pipe.reading, |o| {
                        ConnectNamedPipe(pipe.handle, Some(o))
                    })
                }
                .is_some()
                    || unsafe { windows::Win32::Foundation::GetLastError() }
                        == ERROR_PIPE_CONNECTED;
                if ok {
                    Some(pipe)
                } else {
                    None
                }
            }
            None => None,
        };
        let Some(pipe) = connected else {
            match create_instance(&name, &sddl) {
                Ok(h) => {
                    waiting = h;
                    continue;
                }
                Err(_) => return,
            }
        };

        let id = next_id;
        next_id += 1;
        let pipe = Arc::new(pipe);
        let reader = Arc::clone(&pipe);
        // The daemon takes the write end here, before any frame can arrive on
        // it, so an answer always has somewhere to go.
        if outbox.send(Wire::Connected(id, pipe)).is_err() {
            return;
        }
        let sender = outbox.clone();
        if std::thread::Builder::new()
            .name(format!("cortisoff-pipe-{id}"))
            .spawn(move || read_loop(id, reader, sender))
            .is_err()
        {
            let _ = outbox.send(Wire::Gone(id));
        }

        match create_instance(&name, &sddl) {
            Ok(h) => waiting = h,
            Err(_) => return,
        }
    }
}

fn read_loop(id: u64, pipe: Arc<Pipe>, outbox: Sender<Wire>) {
    let mut reader = FrameReader::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = unsafe {
            finish(pipe.handle, pipe.reading, |o| {
                ReadFile(pipe.handle, Some(&mut buffer), None, Some(o))
            })
        };
        let Some(read) = read.filter(|n| *n > 0) else {
            let _ = outbox.send(Wire::Gone(id));
            return;
        };
        for line in reader.push(&buffer[..read as usize]) {
            match line {
                Ok(frame) => {
                    if outbox.send(Wire::Frame(id, frame)).is_err() {
                        return;
                    }
                }
                // One unparseable line is enough to stop trusting the
                // connection: a client that cannot frame cannot be answered.
                Err(_) => {
                    let _ = outbox.send(Wire::Gone(id));
                    return;
                }
            }
        }
    }
}

fn create_instance(name: &str, sddl: &str) -> Result<HANDLE, String> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(sddl),
            1, // SDDL_REVISION_1
            &mut descriptor,
            None,
        )
    }
    .map_err(|e| format!("the pipe's security descriptor was refused: {e}"))?;

    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };

    let wide = HSTRING::from(name);
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_OVERLAPPED.0),
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER,
            BUFFER,
            0,
            Some(&attributes),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!("could not create {name}"));
    }
    Ok(handle)
}

/// `D:P` so nothing is inherited, then one entry: this user, full control.
/// Everyone else, including other logged-in accounts, is refused by the kernel.
fn own_user_only() -> Result<String, String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|e| format!("could not read this process's token: {e}"))?;

    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    let mut buffer = vec![0u8; needed as usize];
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr() as *mut c_void),
            needed,
            &mut needed,
        )
    };
    let sid = read.map(|()| unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid });
    unsafe {
        let _ = CloseHandle(token);
    }
    let sid = sid.map_err(|e| format!("could not read this process's user: {e}"))?;

    let mut text = windows::core::PWSTR::null();
    unsafe { windows::Win32::Security::Authorization::ConvertSidToStringSidW(sid, &mut text) }
        .map_err(|e| format!("could not read this user's SID: {e}"))?;
    let owned = unsafe { text.to_string() }.map_err(|e| e.to_string())?;
    unsafe {
        windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(
            text.0 as *mut c_void,
        ));
    }
    Ok(format!("D:P(A;;GA;;;{owned})"))
}

/// The session this process belongs to. The pipe name carries it, so two users
/// logged into the same machine each get their own daemon.
pub fn session_id() -> u32 {
    let mut session = 0u32;
    let ok = unsafe {
        windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(
            windows::Win32::System::Threading::GetCurrentProcessId(),
            &mut session,
        )
    };
    if ok.is_ok() {
        session
    } else {
        0
    }
}

/// Connect to a running daemon, for a client. `None` when nothing is
/// listening, which is the normal answer when the daemon is not running.
///
/// The daemon keeps one instance waiting and creates the next one only once
/// a client has taken it, so two clients arriving in the same instant find
/// one instance between them: the loser gets `ERROR_PIPE_BUSY`, not "no
/// daemon". The dashboard asks three questions at once on every refresh and
/// hit exactly that. `WaitNamedPipeW` blocks until the next instance exists,
/// which is microseconds away, so busy is retried and only a real absence is
/// `None`.
pub fn connect(session: u32) -> Option<Client> {
    use windows::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_MODE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Pipes::WaitNamedPipeW;

    let name = HSTRING::from(format!(r"\\.\pipe\cortisoff.{session}"));
    // Enough for a burst of clients, each of them served in well under the
    // wait, and short enough that a wedged daemon reads as absent.
    const ATTEMPTS: u32 = 8;
    const WAIT_MS: u32 = 500;
    for _ in 0..ATTEMPTS {
        let opened = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        match opened {
            Ok(handle) if handle != INVALID_HANDLE_VALUE => {
                return Some(Client {
                    pipe: Handle(handle),
                    reader: FrameReader::new(),
                })
            }
            Ok(_) => return None,
            Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_BUSY.0) => {
                // A timeout here means the daemon is alive but slow to re-arm;
                // trying again is right either way.
                let _ = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), WAIT_MS) };
            }
            Err(_) => return None,
        }
    }
    None
}

/// The client half, synchronous on purpose: a CLI has nothing else to do while
/// it waits for its answer.
pub struct Client {
    pipe: Handle,
    reader: FrameReader,
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.pipe.0);
        }
    }
}

impl Client {
    pub fn send(&mut self, frame: &Frame) -> Result<(), String> {
        let line = encode(frame);
        let bytes = line.as_bytes();
        let mut written = 0u32;
        unsafe { WriteFile(self.pipe.0, Some(bytes), Some(&mut written), None) }
            .map_err(|e| format!("the daemon closed the connection: {e}"))
    }

    /// Block until at least one frame arrives. `None` once the daemon has hung
    /// up, which is how `trace` ends.
    pub fn recv(&mut self) -> Option<Vec<Frame>> {
        let mut buffer = [0u8; 8192];
        loop {
            let mut read = 0u32;
            let ok = unsafe { ReadFile(self.pipe.0, Some(&mut buffer), Some(&mut read), None) };
            if ok.is_err() || read == 0 {
                return None;
            }
            let frames: Vec<Frame> = self
                .reader
                .push(&buffer[..read as usize])
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
pub fn ask(
    session: u32,
    request: cortisoff_ipc::Request,
) -> Result<cortisoff_ipc::Response, String> {
    connect(session)
        .ok_or_else(|| "no daemon is listening on this session. start cortisoffd first".to_owned())?
        .ask(request)
}
