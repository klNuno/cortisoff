// A release daemon has no console, so a logon task does not open a black
// window. A debug build keeps one, which is what `cargo run` wants.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `cortisoffd`, the daemon itself.
//!
//! The only place in the project that picks a platform. What it does is four
//! moves: ask the host what this machine can do, read the rules, put back
//! whatever a previous run left applied, then loop.
//!
//!     cargo run -p cortisoff-daemon                    # foreground, live log
//!     cargo run -p cortisoff-daemon -- --rules my.toml
//!     cargo run -p cortisoff-daemon -- --doctor        # report and exit
//!
//! Ctrl-C hands every screen back before exiting. Killing it does not, which
//! is what the journal is for: the next start restores before any rule fires.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cortisoff_core::{CapTable, Engine};
use cortisoff_daemon::{
    resting, ClientId, DaemonLog, FileJournal, Mark, Paths, Previous, Server, Supervisor,
};
use cortisoff_host::{CapabilitySet, LogLevel};
use cortisoff_ipc::Notification;

/// Long enough that an idle machine costs nothing, short enough that Ctrl-C
/// feels immediate. Every real wakeup comes from the message pump or from a
/// pipe client, never from this expiring.
const TICK: Duration = Duration::from_millis(500);

fn main() {
    // Started from a terminal, print there. Started by the logon task, there
    // is nothing to attach to and nothing is lost.
    platform::attach_console();
    let args = Args::parse(std::env::args().skip(1));
    if args.help {
        println!("{USAGE}");
        return;
    }
    if let Err(e) = run(args) {
        eprintln!("cortisoffd: {e}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<(), String> {
    if let Some(mode) = &args.autostart {
        return platform::autostart(mode);
    }

    // Paths read the environment and touch no file, so the log is open before
    // the session lock is even asked for. A process that never becomes the
    // daemon is exactly the one whose reason for leaving gets lost otherwise.
    let paths = Paths::from_env().map_err(|e| e.0)?;
    let rules_path = args.rules.clone().unwrap_or(paths.rules);
    let journal_path = args.journal.clone().unwrap_or(paths.journal);
    let log_path = args.log.clone().unwrap_or(paths.log);

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
                let said = "another cortisoffd is already running in this session. \
                            `cortisoff stop` ends it";
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
                "cortisoffd {} as pid {}",
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

    let rules = read_rules(&rules_path).inspect_err(|e| {
        log.line(
            platform::now(),
            Mark::Exit,
            &format!("could not start: {e}"),
        );
    })?;
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

    let engine = Engine::new(
        CapTable::from_set(&probed),
        resting(host.as_ref(), rules.clone()),
    );
    let mut supervisor = Supervisor::new(engine, journal, platform::gate())
        .with_rules_path(&rules_path)
        .with_rules(rules);
    for warning in warnings {
        supervisor.warn(warning);
    }

    if args.doctor {
        let report = supervisor.doctor(host.as_ref(), &probed);
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        return Ok(());
    }

    println!("cortisoffd, rules from {}", rules_path.display());
    println!("baselines in {}", journal_path.display());
    if log.writing() {
        println!("log in {}", log.path().display());
    }
    print_capabilities(&probed);

    let mut transport = match platform::listen() {
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
                    let answers = server.on_frame(ClientId(id), frame, host.as_mut(), &probed);
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
        cortisoff_daemon::append(&path, platform::now(), Mark::Panic, &info.to_string());
        inner(info);
    }));
}

/// The rule file, seeded on first run rather than treated as an error. A
/// daemon that refuses to start because the user has never written a rule is a
/// daemon nobody gets to try.
fn read_rules(path: &Path) -> Result<Vec<cortisoff_core::Rule>, String> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            seed(path)?;
            println!("wrote a starter rule file to {}", path.display());
            std::fs::read_to_string(path).map_err(|e| e.to_string())?
        }
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    cortisoff_core::parse_rules(&source)
        .map_err(|e| format!("{}: {}: {}", path.display(), e.at, e.message))
}

/// Hand every subscriber what has happened, then print it here too.
///
/// Quiet by default: an event per focus change is noise, a rule changing its
/// mind is not.
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
            Notification::RuleEdge { rule, holding } => {
                println!("{} {rule}", if holding { "hold   " } else { "release" });
                let mark = if holding { Mark::Hold } else { Mark::Release };
                log.line(platform::now(), mark, &rule);
            }
            Notification::Log { level, message } => {
                match level {
                    LogLevel::Error => eprintln!("error: {message}"),
                    LogLevel::Warn => eprintln!("warning: {message}"),
                    _ => println!("{message}"),
                }
                // Everything the user would want to read the morning after,
                // and nothing per event: an action a second would bury the
                // one line that says why the daemon is gone.
                let mark = match level {
                    LogLevel::Error => Mark::Error,
                    LogLevel::Warn => Mark::Warn,
                    _ => Mark::Info,
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

/// A first rule file holding only what this machine can do today. Pointing a
/// new user at a file full of unsupported rules is how a working program looks
/// broken.
fn seed(path: &Path) -> Result<(), String> {
    const STARTER: &str = r#"# Your rules. Every key inside `when` has to hold at once, and a rule owns what
# it sets only while its `when` holds. Nothing here needs an undo.
#
# The full list of conditions, and one rule per shipped feature, are in
# `rules.example.toml` in the repository.

[[rule]]
name = "blue light at night"
# End exclusive, and it crosses midnight because the start is after the end.
when.between = "20:00-07:00"
# A film or a game filling the screen keeps its colours. Maximised counts,
# borderless fullscreen too.
when.not.maximized = true
set.display.color_temp = 3200
# Fades rather than switching: one minute down to 3200 K at 20:00, and one
# minute back, done by 07:00. It starts from whatever the screen shows,
# 6600 K on an untouched monitor. Name a `transition.from` to start
# elsewhere, or a longer duration to make the change slower.
transition.duration = "1m"
transition.offset = 0
"#;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, STARTER).map_err(|e| format!("could not write {}: {e}", path.display()))
}

const USAGE: &str = "cortisoffd, the cortisoff daemon

    --rules <path>    rule file to load, default is the per-user one
    --journal <path>  where captured baselines are written
    --log <path>      where starts, exits and rule edges are written
    --no-log          write no such file
    --doctor          print the capability and rule report, then exit
    --verbose         print every event and action, not only rule edges
    --autostart <on|off|status>
                      start this daemon when you log on, or stop doing so
    --no-tray         no icon in the notification area
    --no-eco          do not ask Windows to schedule the daemon as background work
    --help";

#[derive(Default)]
struct Args {
    rules: Option<PathBuf>,
    journal: Option<PathBuf>,
    log: Option<PathBuf>,
    no_log: bool,
    doctor: bool,
    verbose: bool,
    autostart: Option<String>,
    no_tray: bool,
    no_eco: bool,
    help: bool,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> Self {
        let mut out = Self::default();
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--rules" => out.rules = args.next().map(PathBuf::from),
                "--journal" => out.journal = args.next().map(PathBuf::from),
                "--log" => out.log = args.next().map(PathBuf::from),
                "--no-log" => out.no_log = true,
                "--doctor" => out.doctor = true,
                "--verbose" | "-v" => out.verbose = true,
                "--autostart" => {
                    out.autostart = Some(args.next().unwrap_or_else(|| "status".to_owned()))
                }
                "--no-tray" => out.no_tray = true,
                "--no-eco" => out.no_eco = true,
                _ => out.help = true,
            }
        }
        out
    }
}

#[cfg(windows)]
mod platform {
    use cortisoff_daemon::{Interference, Outbound, Stamp};
    use cortisoff_host::Host;

    pub use cortisoff_win::Incoming;

    /// Local time, the same call the rule engine's clock events come from, so
    /// a line in the log and a `between` rule never disagree about the hour.
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

    pub fn autostart(mode: &str) -> Result<(), String> {
        use cortisoff_win::autostart;
        match mode {
            "on" => {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                autostart::enable(&exe)?;
                println!("cortisoffd starts when you log on: {}", exe.display());
            }
            "off" => {
                autostart::disable()?;
                println!("cortisoffd no longer starts when you log on");
            }
            "status" => println!("{}", if autostart::enabled() { "on" } else { "off" }),
            other => return Err(format!("--autostart takes on, off or status, not {other}")),
        }
        Ok(())
    }

    /// Read once, before anything is applied. A colour utility that starts
    /// later is the next reload's problem, not this call's.
    pub fn gate() -> Interference {
        let processes = cortisoff_win::running_processes();
        Interference::assess(
            processes.iter().map(String::as_str),
            cortisoff_win::current_ramp().as_ref(),
        )
    }

    pub fn listen() -> Result<Transport, String> {
        cortisoff_win::PipeServer::start(cortisoff_win::session_id())
            .map(|pipe| Transport { pipe: Some(pipe) })
    }

    /// A daemon whose transport failed to open still filters blue light. It
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

#[cfg(not(windows))]
mod platform {
    use cortisoff_daemon::{Interference, Outbound, Stamp};
    use cortisoff_host::Host;

    /// UTC until a platform here has a local-time call of its own. No daemon
    /// runs on one yet, so nothing reads these lines.
    pub fn now() -> Stamp {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0);
        Stamp::from_unix_utc(secs)
    }

    /// The same shape the Windows transport hands back, so the loop above
    /// needs no `cfg` of its own. Nothing constructs these until a platform
    /// grows a transport.
    #[allow(dead_code)]
    pub enum Incoming {
        Connected(u64),
        Frame(u64, cortisoff_ipc::Frame),
        Gone(u64),
    }

    pub fn host(_tray: bool) -> Result<(Box<dyn Host>, Vec<String>), String> {
        Err("no Host is implemented for this platform yet".to_owned())
    }

    pub fn attach_console() {}

    /// Nothing to lock against until a platform can run a daemon at all.
    pub fn claim_session() -> Option<()> {
        Some(())
    }

    pub fn efficiency_mode() {}

    pub fn autostart(_mode: &str) -> Result<(), String> {
        Err("autostart is not implemented for this platform yet".to_owned())
    }

    pub fn gate() -> Interference {
        Interference::clear()
    }

    pub fn listen() -> Result<Transport, String> {
        Err("no transport is implemented for this platform yet".to_owned())
    }

    pub struct Transport;

    impl Transport {
        pub fn deaf() -> Self {
            Self
        }

        pub fn address(&self) -> &str {
            "nothing"
        }

        pub fn read(&mut self) -> Vec<Incoming> {
            Vec::new()
        }

        pub fn write(&mut self, _outbound: Vec<Outbound>) {}

        pub fn drop_client(&mut self, _id: u64) {}
    }

    pub fn watch_for_stop() -> bool {
        false
    }

    pub fn stop_requested() -> bool {
        false
    }
}
