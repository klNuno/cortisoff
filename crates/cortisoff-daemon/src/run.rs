//! The daemon as a process: `cortisoff --daemon`.
//!
//! The only place in the project that picks a platform. Four moves: ask the
//! host what this machine can do, put back whatever a previous run left
//! applied, start the plugins, then loop.
//!
//! ```text
//! cargo run -p cortisoff-app -- --daemon             # foreground, live log
//! cargo run -p cortisoff-app -- --daemon --doctor    # report and exit
//! ```
//!
//! Ctrl-C hands every screen back before exiting. Killing it does not, which
//! is what the journal is for: the next start restores before any plugin
//! claims anything.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cortisoff_host::{CapabilitySet, LogLevel};
use cortisoff_ipc::Notification;
use cortisoff_runtime::{Runtime, Store};

use crate::{ClientId, DaemonLog, FileJournal, Mark, Paths, Previous, Server, Supervisor};

/// Long enough that an idle machine costs nothing, short enough that Ctrl-C
/// feels immediate. Every real wakeup comes from the platform or from a
/// client, never from this expiring.
const TICK: Duration = Duration::from_millis(500);

pub const USAGE: &str = "cortisoff --daemon, the part that runs the plugins

    --journal <path>  where captured baselines are written
    --log <path>      where starts, exits and errors are written
    --no-log          write no such file
    --plugins <dir>   where installed plugins live
    --store <path>    which plugins are on and their settings
    --doctor          print what this machine can do, then exit
    --verbose         print every event and action
    --autostart <on|off|status>
                      start the daemon when you log on, or stop doing so
    --no-tray         no icon in the notification area
    --no-eco          do not ask the system to schedule it as background work";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Options {
    pub journal: Option<PathBuf>,
    pub log: Option<PathBuf>,
    pub plugins: Option<PathBuf>,
    pub store: Option<PathBuf>,
    pub no_log: bool,
    pub doctor: bool,
    pub verbose: bool,
    pub autostart: Option<String>,
    pub no_tray: bool,
    pub no_eco: bool,
}

impl Options {
    /// Everything after `--daemon`.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut out = Self::default();
        let mut args = args.into_iter();
        let path = |name: &str, next: Option<String>| {
            next.map(PathBuf::from)
                .ok_or_else(|| format!("{name} needs a path after it"))
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--journal" => out.journal = Some(path("--journal", args.next())?),
                "--log" => out.log = Some(path("--log", args.next())?),
                "--plugins" => out.plugins = Some(path("--plugins", args.next())?),
                "--store" => out.store = Some(path("--store", args.next())?),
                "--no-log" => out.no_log = true,
                "--doctor" => out.doctor = true,
                "--verbose" | "-v" => out.verbose = true,
                "--autostart" => {
                    out.autostart = Some(args.next().unwrap_or_else(|| "status".to_owned()))
                }
                "--no-tray" => out.no_tray = true,
                "--no-eco" => out.no_eco = true,
                other => return Err(format!("{other} is not a daemon option\n\n{USAGE}")),
            }
        }
        Ok(out)
    }
}

/// Start the session's daemon from the window or the CLI, detached, unless
/// one already answers. Returns once the process is started, not once it
/// listens.
pub fn spawn_detached(exe: &Path) -> Result<(), String> {
    platform::spawn_detached(exe)
}

/// `--autostart on|off|status`: starting the daemon at logon, from this
/// executable.
pub fn autostart(mode: &str) -> Result<(), String> {
    match mode {
        "on" => {
            platform::start_at_logon(true)?;
            println!("cortisoff starts when you log on");
        }
        "off" => {
            platform::start_at_logon(false)?;
            println!("cortisoff no longer starts when you log on");
        }
        "status" => println!(
            "{}",
            if platform::starts_at_logon() {
                "on"
            } else {
                "off"
            }
        ),
        other => return Err(format!("--autostart takes on, off or status, not {other}")),
    }
    Ok(())
}

/// Start this executable's daemon at logon, or stop doing so. What the
/// window's switch and the installer call.
pub fn start_at_logon(on: bool) -> Result<(), String> {
    platform::start_at_logon(on)
}

/// Whether the daemon starts at logon.
pub fn starts_at_logon() -> bool {
    platform::starts_at_logon()
}

pub fn run(args: Options) -> Result<(), String> {
    platform::attach_console();
    if let Some(mode) = &args.autostart {
        return autostart(mode);
    }

    // Paths read the environment and touch no file, so the log is open before
    // the session lock is even asked for. A process that never becomes the
    // daemon is exactly the one whose reason for leaving gets lost otherwise.
    let paths = Paths::from_env().map_err(|e| e.0)?;
    let journal_path = args.journal.clone().unwrap_or(paths.journal);
    let log_path = args.log.clone().unwrap_or(paths.log);
    let plugins_dir = args.plugins.clone().unwrap_or(paths.plugins);
    let store_path = args.store.clone().unwrap_or(paths.store);

    // The doctor only reads: no lock, no line in the log, nothing that makes
    // a second run look like a daemon.
    let mut log = if args.doctor || args.no_log {
        DaemonLog::off()
    } else {
        DaemonLog::open(&log_path)
    };

    // Before anything is read or applied: a second daemon would capture the
    // first one's tint as its baseline.
    let _session = if args.doctor {
        None
    } else {
        match platform::claim_session() {
            Some(lock) => Some(lock),
            None => {
                let said = "another cortisoff daemon is already running in this session. \
                            `cortisoff-cli stop` ends it";
                // Once, however often the safety-net trigger tries.
                log.line_once(platform::now(), Mark::Refused, said);
                return Err(said.to_owned());
            }
        }
    };
    if !args.doctor && !args.no_eco {
        platform::efficiency_mode();
    }

    let mut warnings = Vec::new();
    if log.writing() {
        let previous = log.start(
            platform::now(),
            &format!(
                "cortisoff {} daemon as pid {}",
                env!("CARGO_PKG_VERSION"),
                std::process::id()
            ),
        );
        if previous == Previous::Cut {
            warnings.push(format!(
                "the previous run ended without handing the screen back. {} has the last thing \
                 it said",
                log.path().display()
            ));
        }
        log_panics_to(&log_path);
    }
    warnings.extend(log.warnings().iter().cloned());

    let (mut host, host_warnings) =
        platform::host(!args.doctor && !args.no_tray).inspect_err(|e| {
            log.line(
                platform::now(),
                Mark::Exit,
                &format!("could not start: {e}"),
            )
        })?;
    warnings.extend(host_warnings);
    let probed = host.probe();

    let journal = FileJournal::open(&journal_path);
    warnings.extend(journal.warnings().iter().cloned());

    let (store, store_warning) = Store::open(&store_path);
    warnings.extend(store_warning);
    let (packages, problems) = cortisoff_runtime::discover(&plugins_dir, &store.links);
    warnings.extend(problems);
    let runtime = Runtime::new(packages, &store, host.waker());

    let mut supervisor = Supervisor::new(probed.clone(), journal, platform::gate(), runtime, store)
        .with_plugins_dir(&plugins_dir);
    for warning in warnings {
        supervisor.warn(warning);
    }

    if args.doctor {
        let report = supervisor.doctor(host.as_ref());
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        return Ok(());
    }

    println!("cortisoff daemon, plugins from {}", plugins_dir.display());
    println!("baselines in {}", journal_path.display());
    if log.writing() {
        println!("log in {}", log.path().display());
    }
    print_capabilities(&probed);

    let mut transport = match platform::listen(host.waker()) {
        Ok(t) => {
            println!("listening on {}", t.address());
            t
        }
        Err(e) => {
            eprintln!("warning: no client will be able to connect: {e}");
            platform::Transport::deaf()
        }
    };

    let mut server = Server::new(supervisor);
    server.supervisor_mut().start(host.as_mut());
    flush(&mut server, &mut transport, args.verbose, &mut log);

    if !platform::watch_for_stop() {
        eprintln!("warning: Ctrl-C will not restore the screen on the way out");
    }

    while !server.stopping() && !platform::stop_requested() {
        let outbound = server.pump(host.as_mut(), TICK);
        transport.write(outbound);
        for incoming in transport.read() {
            match incoming {
                platform::Incoming::Frame(id, frame) => {
                    if args.verbose {
                        println!("client {id} asked {frame:?}");
                    }
                    let answers = server.on_frame(ClientId(id), frame, host.as_mut());
                    transport.write(answers);
                }
                platform::Incoming::Gone(id) => {
                    if args.verbose {
                        println!("client {id} left");
                    }
                    let answers = server.disconnect(ClientId(id), host.as_mut());
                    transport.write(answers);
                    transport.drop_client(id);
                }
                platform::Incoming::Connected(id) => {
                    if args.verbose {
                        println!("client {id} connected");
                    }
                }
            }
        }
        flush(&mut server, &mut transport, args.verbose, &mut log);
    }

    // A client asking to stop has already been served: `Request::Shutdown`
    // restores the baselines itself, so that the exit path that always runs is
    // the one holding the restore. Only Ctrl-C still owes the screen anything.
    let asked = server.stopping();
    if !asked {
        println!("putting every screen back");
        server.supervisor_mut().shutdown(host.as_mut());
        server.supervisor_mut().note_closing();
    }
    flush(&mut server, &mut transport, args.verbose, &mut log);
    for e in server.supervisor().errors() {
        eprintln!("error: {e}");
    }
    log.line(
        platform::now(),
        Mark::Exit,
        if asked {
            "a client asked it to stop, every screen handed back"
        } else {
            "Ctrl-C or the tray icon, every screen handed back"
        },
    );
    Ok(())
}

/// A panic writes its own line before the process goes, so the file never
/// blames a crash on something that killed the daemon from outside. The
/// release profile aborts on panic, and a hook still runs before the abort.
fn log_panics_to(path: &Path) {
    let path = path.to_path_buf();
    let inner = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::append(&path, platform::now(), Mark::Panic, &info.to_string());
        inner(info);
    }));
}

/// Hand every subscriber what has happened, then print it here too.
///
/// Quiet by default: an event per focus change is noise, a plugin saying
/// something is not.
fn flush(
    server: &mut Server<FileJournal>,
    transport: &mut platform::Transport,
    verbose: bool,
    log: &mut DaemonLog,
) {
    let outbound = server.fan_out();
    transport.write(outbound);
    for notification in server.drain_log() {
        match notification {
            Notification::Log { level, message } => {
                match level {
                    LogLevel::Error => eprintln!("error: {message}"),
                    LogLevel::Warn => eprintln!("warning: {message}"),
                    LogLevel::Info => println!("{message}"),
                    LogLevel::Trace if verbose => println!("{message}"),
                    LogLevel::Trace => {}
                }
                // Everything the user would want to read the morning after,
                // and nothing per event: an action a second would bury the
                // one line that says why the daemon is gone.
                let mark = match level {
                    LogLevel::Error => Mark::Error,
                    LogLevel::Warn => Mark::Warn,
                    LogLevel::Info => Mark::Info,
                    LogLevel::Trace => continue,
                };
                log.line(platform::now(), mark, &message);
            }
            Notification::Closing => println!("closing"),
            Notification::Action(action) if verbose => println!("action {action:?}"),
            Notification::Event(event) if verbose => println!("event  {event:?}"),
            _ => {}
        }
    }
}

fn print_capabilities(probed: &CapabilitySet) {
    for id in probed.ids() {
        match probed.backend(id) {
            Some(backend) => println!("  {id} via {backend}"),
            None => println!("  {id}"),
        }
    }
    for gap in probed.gaps() {
        println!("  {} unavailable: {}", gap.id, gap.reason);
    }
}

// Both platforms hand back the same `Incoming` and `Transport` shapes, so
// the loop above needs no `cfg` of its own.

#[cfg(windows)]
mod platform {
    use std::path::Path;

    use crate::{Interference, Outbound, Stamp};
    use cortisoff_host::{Host, Waker};

    pub use cortisoff_win::Incoming;

    /// Local time, the same call the plugins' clock comes from, so a line in
    /// the log and a plugin's schedule never disagree about the hour.
    pub fn now() -> Stamp {
        let now = cortisoff_win::local_time();
        Stamp {
            year: now.0,
            month: now.1,
            day: now.2,
            hour: now.3,
            minute: now.4,
            second: now.5,
        }
    }

    /// The host, and whatever it wants the user warned about before anything
    /// is applied.
    pub fn host(tray: bool) -> Result<(Box<dyn Host>, Vec<String>), String> {
        let mut host = cortisoff_win::WinHost::new().map_err(|e| e.to_string())?;
        if tray {
            host = host.with_tray();
        }
        let warnings = host.warnings();
        Ok((Box::new(host), warnings))
    }

    pub fn attach_console() {
        cortisoff_win::attach_parent_console();
    }

    pub fn claim_session() -> Option<cortisoff_win::InstanceLock> {
        cortisoff_win::claim_session()
    }

    pub fn efficiency_mode() {
        cortisoff_win::efficiency_mode();
    }

    pub fn start_at_logon(on: bool) -> Result<(), String> {
        use cortisoff_win::autostart;
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            autostart::enable(&exe)
        } else {
            autostart::disable()
        }
    }

    pub fn starts_at_logon() -> bool {
        cortisoff_win::autostart::enabled()
    }

    pub fn spawn_detached(exe: &Path) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        std::process::Command::new(exe)
            .arg("--daemon")
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not start {}: {e}", exe.display()))
    }

    /// Read once, before anything is applied. A colour utility that starts
    /// later is the next start's problem, not this call's.
    pub fn gate() -> Interference {
        let processes = cortisoff_win::running_processes();
        Interference::assess(
            processes.iter().map(String::as_str),
            cortisoff_win::current_ramp().as_ref(),
        )
    }

    /// The pipe wakes the pump itself; it needs no waker handed in.
    pub fn listen(_waker: Option<Waker>) -> Result<Transport, String> {
        cortisoff_win::PipeServer::start(cortisoff_win::session_id())
            .map(|pipe| Transport { pipe: Some(pipe) })
    }

    /// A daemon whose transport failed to open still runs its plugins. It
    /// just cannot be asked anything, so it says so and keeps running rather
    /// than refusing to start.
    pub struct Transport {
        pipe: Option<cortisoff_win::PipeServer>,
    }

    impl Transport {
        pub fn deaf() -> Self {
            Self { pipe: None }
        }

        pub fn address(&self) -> &str {
            self.pipe.as_ref().map_or("nothing", |p| p.name())
        }

        pub fn read(&mut self) -> Vec<Incoming> {
            self.pipe.as_mut().map(|p| p.drain()).unwrap_or_default()
        }

        pub fn write(&mut self, outbound: Vec<Outbound>) {
            let Some(pipe) = self.pipe.as_mut() else {
                return;
            };
            for out in outbound {
                pipe.send(out.to.0, &out.frame);
            }
        }

        pub fn drop_client(&mut self, id: u64) {
            if let Some(pipe) = self.pipe.as_mut() {
                pipe.disconnect(id);
            }
        }
    }

    pub fn watch_for_stop() -> bool {
        cortisoff_win::watch_for_stop()
    }

    pub fn stop_requested() -> bool {
        cortisoff_win::stop_requested()
    }
}

#[cfg(unix)]
mod platform {
    use std::path::Path;

    use crate::{Interference, Outbound, Stamp};
    use cortisoff_host::{Host, Waker};

    pub use cortisoff_unix::Incoming;

    pub fn now() -> Stamp {
        let now = cortisoff_unix::local_time();
        Stamp {
            year: now.0,
            month: now.1,
            day: now.2,
            hour: now.3,
            minute: now.4,
            second: now.5,
        }
    }

    #[cfg(target_os = "macos")]
    pub fn host(_tray: bool) -> Result<(Box<dyn Host>, Vec<String>), String> {
        let host = cortisoff_mac::MacHost::new()?;
        let warnings = host.warnings();
        Ok((Box::new(host), warnings))
    }

    #[cfg(target_os = "linux")]
    pub fn host(_tray: bool) -> Result<(Box<dyn Host>, Vec<String>), String> {
        let host = cortisoff_linux::LinuxHost::new()?;
        let warnings = host.warnings();
        Ok((Box::new(host), warnings))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn host(_tray: bool) -> Result<(Box<dyn Host>, Vec<String>), String> {
        Err("cortisoff has no host for this system".to_owned())
    }

    pub fn attach_console() {}

    pub fn claim_session() -> Option<cortisoff_unix::InstanceLock> {
        cortisoff_unix::claim_session()
    }

    pub fn efficiency_mode() {
        cortisoff_unix::efficiency_mode();
    }

    #[cfg(target_os = "linux")]
    use cortisoff_linux::autostart;
    #[cfg(target_os = "macos")]
    use cortisoff_mac::autostart;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub fn start_at_logon(on: bool) -> Result<(), String> {
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            autostart::enable(&exe)
        } else {
            autostart::disable()
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub fn starts_at_logon() -> bool {
        autostart::enabled()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn start_at_logon(_on: bool) -> Result<(), String> {
        Err("starting at login is not supported on this system".to_owned())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn starts_at_logon() -> bool {
        false
    }

    pub fn spawn_detached(exe: &Path) -> Result<(), String> {
        use std::os::unix::process::CommandExt;
        std::process::Command::new(exe)
            .arg("--daemon")
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not start {}: {e}", exe.display()))
    }

    /// No portable way to read a gamma ramp here, so the name list decides.
    pub fn gate() -> Interference {
        let processes = cortisoff_unix::running_processes();
        Interference::assess(processes.iter().map(String::as_str), None)
    }

    pub fn listen(waker: Option<Waker>) -> Result<Transport, String> {
        cortisoff_unix::SocketServer::start(waker).map(|socket| Transport {
            socket: Some(socket),
        })
    }

    pub struct Transport {
        socket: Option<cortisoff_unix::SocketServer>,
    }

    impl Transport {
        pub fn deaf() -> Self {
            Self { socket: None }
        }

        pub fn address(&self) -> &str {
            self.socket.as_ref().map_or("nothing", |s| s.name())
        }

        pub fn read(&mut self) -> Vec<Incoming> {
            self.socket.as_mut().map(|s| s.drain()).unwrap_or_default()
        }

        pub fn write(&mut self, outbound: Vec<Outbound>) {
            let Some(socket) = self.socket.as_mut() else {
                return;
            };
            for out in outbound {
                socket.send(out.to.0, &out.frame);
            }
        }

        pub fn drop_client(&mut self, id: u64) {
            if let Some(socket) = self.socket.as_mut() {
                socket.disconnect(id);
            }
        }
    }

    pub fn watch_for_stop() -> bool {
        cortisoff_unix::watch_for_stop()
    }

    pub fn stop_requested() -> bool {
        cortisoff_unix::stop_requested()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn options_read_their_paths() {
        let o = parse(&["--plugins", "/p", "--doctor", "--no-tray"]).unwrap();
        assert_eq!(o.plugins, Some(PathBuf::from("/p")));
        assert!(o.doctor && o.no_tray);
    }

    #[test]
    fn a_path_option_without_a_path_says_so() {
        assert!(parse(&["--journal"]).unwrap_err().contains("--journal"));
    }

    #[test]
    fn an_unknown_option_is_refused_with_the_usage() {
        assert!(parse(&["--rules", "x"]).unwrap_err().contains("--plugins"));
    }
}
