//! `cortisoff-cli`, the command line client.
//!
//! An IPC client and nothing else. It links no actuator, runs no plugin and
//! opens no window: everything it prints came out of the daemon over the
//! wire, the same protocol the window speaks.
//!
//! ```text
//! cortisoff-cli plugins              # every plugin, on or off
//! cortisoff-cli enable night-light
//! cortisoff-cli settings night-light '{"night": 3000}'
//! cortisoff-cli link ~/my-plugin     # load a folder, reload it on save
//! cortisoff-cli trace                # everything the daemon sees and does
//! cortisoff-cli set display.color_temp 3400
//! ```

use std::path::Path;

use cortisoff_host::CapabilityId;
use cortisoff_ipc::{Notification, PluginReport, Request, Response};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    let arg = |i: usize, what: &str| -> Result<String, String> {
        args.get(i)
            .cloned()
            .ok_or_else(|| format!("{command} needs {what}"))
    };

    let result = match command {
        "doctor" => ask(Request::Doctor),
        "state" => ask(Request::State),
        "plugins" => plugins(),
        "windows" => ask(Request::Windows),
        "enable" | "disable" => {
            arg(1, "a plugin id. `cortisoff-cli plugins` lists them").and_then(|id| {
                ask(Request::Enable {
                    id,
                    enabled: command == "enable",
                })
            })
        }
        "settings" => arg(1, "a plugin id").and_then(|id| match args.get(2) {
            None => show_settings(&id),
            Some(json) => {
                json_arg(json).and_then(|settings| ask(Request::Settings { id, settings }))
            }
        }),
        "send" => arg(1, "a plugin id and a message name").and_then(|id| {
            let name = arg(2, "a message name")?;
            let data = match args.get(3) {
                Some(json) => json_arg(json)?,
                None => serde_json::Value::Null,
            };
            ask(Request::Message { id, name, data })
        }),
        "link" => arg(1, "a folder").and_then(|path| {
            let full = std::fs::canonicalize(&path)
                .map_err(|e| format!("{path}: {e}"))?
                .to_string_lossy()
                .into_owned();
            ask(Request::Link { path: full })
        }),
        "remove" => arg(1, "a plugin id").and_then(|id| ask(Request::Remove { id })),
        "rescan" => ask(Request::Rescan),
        "set" => parse_set(&args).and_then(ask),
        "clear" => arg(1, "a capability, such as display.color_temp").and_then(|cap| {
            ask(Request::Clear {
                cap: CapabilityId::new(cap),
            })
        }),
        "trace" => trace(),
        "start" => start(),
        "stop" => ask(Request::Shutdown),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            return;
        }
        "--version" | "-V" => {
            println!("cortisoff-cli {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        other => Err(format!("unknown command {other}\n\n{USAGE}")),
    };

    if let Err(e) = result {
        eprintln!("cortisoff: {e}");
        std::process::exit(1);
    }
}

fn ask(request: Request) -> Result<(), String> {
    match transport::ask(request)? {
        Response::Ok => println!("ok"),
        Response::Doctor(report) => print(&*report),
        Response::State(report) => print(&*report),
        Response::Plugins { plugins } => print(&plugins),
        Response::Windows { windows } => print(&windows),
        // The daemon refusing is not the CLI failing. It answered, in a
        // sentence written to be shown to a person.
        Response::Error { message } => return Err(message),
    }
    Ok(())
}

fn list() -> Result<Vec<PluginReport>, String> {
    match transport::ask(Request::Plugins)? {
        Response::Plugins { plugins } => Ok(plugins),
        Response::Error { message } => Err(message),
        _ => Err("the daemon did not answer with its plugins".to_owned()),
    }
}

/// One line per plugin, for a person. `state` has the full reports as JSON.
fn plugins() -> Result<(), String> {
    let plugins = list()?;
    let width = plugins.iter().map(|p| p.id.len()).max().unwrap_or(0);
    for p in &plugins {
        let state = if !p.supported {
            "n/a"
        } else if !p.enabled {
            "off"
        } else if p.running {
            "on"
        } else {
            "failed"
        };
        let origin = match p.origin {
            cortisoff_ipc::Origin::Builtin => "built in",
            cortisoff_ipc::Origin::Installed => "installed",
            cortisoff_ipc::Origin::Linked => "linked",
        };
        println!(
            "{:<width$}  {state:<6}  {} {}  ({origin})",
            p.id, p.name, p.version
        );
        if let Some(error) = &p.error {
            println!("{:<width$}  {error}", "");
        }
    }
    Ok(())
}

fn show_settings(id: &str) -> Result<(), String> {
    let plugin = list()?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("no plugin {id}. `cortisoff-cli plugins` lists them"))?;
    print(&plugin.settings);
    Ok(())
}

fn json_arg(text: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))
}

/// Stream until the daemon closes, which is what `Notification::Closing`
/// announces one frame ahead of.
fn trace() -> Result<(), String> {
    let mut client = transport::connect()?;
    client.send(&cortisoff_ipc::Frame::request(1, Request::Subscribe))?;
    while let Some(frames) = client.recv() {
        for frame in frames {
            match frame {
                cortisoff_ipc::Frame::Notification { notification } => line(&notification),
                cortisoff_ipc::Frame::Response {
                    response: Response::Error { message },
                    ..
                } => return Err(message),
                _ => {}
            }
        }
    }
    Ok(())
}

fn line(notification: &Notification) {
    match notification {
        Notification::Event(event) => println!("event   {event:?}"),
        Notification::Action(action) => println!("action  {action:?}"),
        Notification::Log { level, message } => println!("{level:?} {message}"),
        Notification::Plugin(p) => println!(
            "plugin  {} {}{}",
            p.id,
            if p.running { "running" } else { "stopped" },
            p.error
                .as_deref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        ),
        Notification::Plugins => println!("plugins the list changed"),
        Notification::Closing => println!("the daemon is going down"),
    }
}

/// `cortisoff-cli set display.color_temp 3400`. The shape comes from
/// `cortisoff_ipc::parse_value`, shared with the window so the two clients
/// cannot disagree about what `@240` means.
fn parse_set(args: &[String]) -> Result<Request, String> {
    let (Some(cap), Some(raw)) = (args.get(1), args.get(2)) else {
        return Err("set needs a capability and a value, such as \
                    `cortisoff-cli set display.color_temp 3400`"
            .to_owned());
    };
    Ok(Request::Set {
        cap: CapabilityId::new(cap),
        value: cortisoff_ipc::parse_value(raw),
    })
}

/// Start the daemon: the app next to this executable, with `--daemon`. The
/// installer puts both in the same folder, and so does Cargo.
fn start() -> Result<(), String> {
    if transport::connect().is_ok() {
        println!("already running");
        return Ok(());
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = me
        .parent()
        .map(|dir| dir.join(format!("cortisoff{}", std::env::consts::EXE_SUFFIX)))
        .filter(|p| p.exists())
        .ok_or_else(|| "cannot find the cortisoff app next to this executable".to_owned())?;
    spawn_daemon(&app)?;
    println!("started {}", app.display());
    Ok(())
}

fn spawn_daemon(app: &Path) -> Result<(), String> {
    let mut command = std::process::Command::new(app);
    command
        .arg("--daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not start {}: {e}", app.display()))
}

fn print<T: serde::Serialize>(report: &T) {
    match serde_json::to_string_pretty(report) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("cortisoff: could not format the answer: {e}"),
    }
}

const USAGE: &str = "cortisoff-cli, the command line client for cortisoff

    plugins                   every plugin, on or off
    enable <id>               switch a plugin on
    disable <id>              switch it off, releasing everything it holds
    settings <id> [json]      print a plugin's settings, or replace them
    send <id> <name> [json]   send a plugin's daemon half a message
    link <folder>             load a plugin from a folder, reloaded on save
    remove <id>               unlink a folder or delete an installed plugin
    rescan                    read the plugin folders again
    doctor                    what this machine can do, and what it cannot
    state                     the live world, every plugin, who holds what
    windows                   the open windows
    trace                     everything the daemon sees and does, live
    set <cap> <value>         a manual override, outranking every plugin
    clear <cap>               drop the override
    start                     start the daemon
    stop                      exit cleanly, restoring every baseline";

const NOT_RUNNING: &str = "cortisoff is not running. `cortisoff-cli start` starts it";

#[cfg(windows)]
mod transport {
    use cortisoff_ipc::{Request, Response};

    pub use cortisoff_win::Client;

    pub fn ask(request: Request) -> Result<Response, String> {
        connect()?.ask(request)
    }

    pub fn connect() -> Result<Client, String> {
        cortisoff_win::connect(cortisoff_win::session_id())
            .ok_or_else(|| super::NOT_RUNNING.to_owned())
    }
}

#[cfg(unix)]
mod transport {
    use cortisoff_ipc::{Request, Response};

    pub use cortisoff_unix::Client;

    pub fn ask(request: Request) -> Result<Response, String> {
        connect()?.ask(request)
    }

    pub fn connect() -> Result<Client, String> {
        cortisoff_unix::connect().ok_or_else(|| super::NOT_RUNNING.to_owned())
    }
}
