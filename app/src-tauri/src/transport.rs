//! The platform's half of the pipe, behind the calls the window needs.
//!
//! The same shape the CLI uses, and for the same reason: the transport is per
//! platform and is never a localhost TCP socket. Any process on the machine
//! can connect to one of those, and this connection changes the user's screen.

#[cfg(windows)]
mod platform {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;

    use cortisoff_ipc::{Frame, Request, Response};
    pub use cortisoff_win::Client;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn ask(request: Request) -> Result<Response, String> {
        cortisoff_win::ask(cortisoff_win::session_id(), request)
    }

    /// A connection of its own, for requests the daemon ties to the
    /// connection they came on. A preview lives exactly as long as the
    /// client that sent it, so one `ask` per request would end it at once.
    pub fn connect() -> Result<Client, String> {
        cortisoff_win::connect(cortisoff_win::session_id())
            .ok_or_else(|| "the daemon is not running".to_owned())
    }

    /// Total CPU time in milliseconds and working set in bytes of one
    /// process, `None` once it is gone.
    pub fn usage(pid: u32) -> Option<(u64, u64)> {
        cortisoff_win::usage(pid).map(|u| (u.cpu_ms, u.rss_bytes))
    }

    /// A connection that has already asked for the notification stream, so the
    /// caller only has to read.
    pub fn subscribe() -> Result<Client, String> {
        let mut client = cortisoff_win::connect(cortisoff_win::session_id())
            .ok_or_else(|| "the daemon is not running".to_owned())?;
        client.send(&Frame::request(1, Request::Subscribe))?;
        Ok(client)
    }

    /// The daemon the installer put next to this window.
    fn daemon() -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let path = exe
            .parent()
            .ok_or_else(|| "this window has no directory".to_owned())?
            .join("cortisoffd.exe");
        if path.exists() {
            Ok(path)
        } else {
            Err(format!(
                "no daemon next to this window, at {}",
                path.display()
            ))
        }
    }

    /// Start it, detached. A second one refuses to run on its own, so this is
    /// safe to call when the pipe was merely slow.
    pub fn start_daemon() -> Result<(), String> {
        let path = daemon()?;
        std::process::Command::new(&path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not start {}: {e}", path.display()))
    }

    pub fn autostart() -> bool {
        cortisoff_win::autostart::enabled()
    }

    pub fn set_autostart(on: bool) -> Result<(), String> {
        if on {
            cortisoff_win::autostart::enable(&daemon()?)
        } else {
            cortisoff_win::autostart::disable()
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use cortisoff_ipc::{Frame, Request, Response};

    const MISSING: &str = "no transport is implemented for this platform yet";

    pub struct Client;

    impl Client {
        pub fn recv(&mut self) -> Option<Vec<Frame>> {
            None
        }

        pub fn ask(&mut self, _request: Request) -> Result<Response, String> {
            Err(MISSING.to_owned())
        }
    }

    pub fn ask(_request: Request) -> Result<Response, String> {
        Err(MISSING.to_owned())
    }

    pub fn connect() -> Result<Client, String> {
        Err(MISSING.to_owned())
    }

    pub fn usage(_pid: u32) -> Option<(u64, u64)> {
        None
    }

    pub fn subscribe() -> Result<Client, String> {
        Err(MISSING.to_owned())
    }

    pub fn start_daemon() -> Result<(), String> {
        Err(MISSING.to_owned())
    }

    pub fn autostart() -> bool {
        false
    }

    pub fn set_autostart(_on: bool) -> Result<(), String> {
        Err(MISSING.to_owned())
    }
}

pub use platform::{
    ask, autostart, connect, set_autostart, start_daemon, subscribe, usage, Client,
};
