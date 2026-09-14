//! `cortisoff`, the command line client.
//!
//! An IPC client and nothing else. It links no actuator, opens no window and
//! knows nothing about what a rule means: everything it prints came out of the
//! daemon over the wire. That is deliberate, and it is the same contract the
//! dashboard will use.
//!
//!     cortisoff doctor    # what this machine can do, and what it cannot
//!     cortisoff state     # the live world, and who holds what
//!     cortisoff trace     # every event, rule edge and action, live
//!     cortisoff rules     # the rules as authored
//!     cortisoff disable "blue light at night"
//!     cortisoff set display.color_temp 3400
//!     cortisoff clear display.color_temp
//!     cortisoff reload
//!     cortisoff stop

use cortisoff_host::CapabilityId;
use cortisoff_ipc::{Notification, Request, Response};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    let result = match command {
        "doctor" => ask(Request::Doctor),
        "state" => ask(Request::State),
        "rules" => ask(Request::Rules),
        "enable" => switch(args.get(1), true),
        "disable" => switch(args.get(1), false),
        "reload" => ask(Request::Reload),
        "stop" => ask(Request::Shutdown),
        "set" => match parse_set(&args) {
            Ok(request) => ask(request),
            Err(e) => Err(e),
        },
        "clear" => match args.get(1) {
            Some(cap) => ask(Request::Clear {
                cap: CapabilityId::new(cap),
            }),
            None => Err("clear needs a capability, such as display.color_temp".to_owned()),
        },
        "trace" => trace(),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
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
        Response::Ok => {
            println!("ok");
            Ok(())
        }
        Response::Doctor(report) => {
            print(&*report);
            Ok(())
        }
        Response::State(report) => {
            print(&*report);
            Ok(())
        }
        Response::Rules { rules } => {
            print(&rules);
            Ok(())
        }
        // The daemon refusing is not the CLI failing. It answered, in a
        // sentence written to be shown to a person.
        Response::Error { message } => Err(message),
    }
}

/// Flip one rule's switch by name.
///
/// The daemon takes the whole set rather than one rule, so this reads them
/// first. That is the same round trip the dashboard makes when you click a
/// card, and it is why nothing has to agree on a rule's identity beyond its
/// name.
fn switch(name: Option<&String>, enabled: bool) -> Result<(), String> {
    let word = if enabled { "enable" } else { "disable" };
    let name = name.ok_or_else(|| {
        format!("{word} needs a rule name, in quotes. `cortisoff rules` lists them")
    })?;

    let Response::Rules { mut rules } = transport::ask(Request::Rules)? else {
        return Err("the daemon did not answer with its rules".to_owned());
    };
    let rule = rules
        .iter_mut()
        .find(|r| r.name == *name)
        .ok_or_else(|| format!("no rule is called {name:?}. `cortisoff rules` lists them"))?;
    if rule.enabled == enabled {
        println!("{name:?} is already {word}d");
        return Ok(());
    }
    rule.enabled = enabled;

    ask(Request::SaveRules { rules })
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
        Notification::RuleEdge { rule, holding } => {
            println!("{}  {rule}", if *holding { "hold   " } else { "release" });
        }
        Notification::Action(action) => println!("action  {action:?}"),
        Notification::Log { level, message } => println!("{level:?} {message}"),
        Notification::Closing => println!("the daemon is going down"),
    }
}

/// `cortisoff set display.color_temp 3400`. The shape comes from
/// `cortisoff_ipc::parse_value`, shared with the dashboard so the two clients
/// cannot disagree about what `@240` means.
fn parse_set(args: &[String]) -> Result<Request, String> {
    let (Some(cap), Some(raw)) = (args.get(1), args.get(2)) else {
        return Err("set needs a capability and a value, such as \
                    `cortisoff set display.color_temp 3400`"
            .to_owned());
    };
    Ok(Request::Set {
        cap: CapabilityId::new(cap),
        value: cortisoff_ipc::parse_value(raw),
    })
}

fn print<T: serde::Serialize>(report: &T) {
    match serde_json::to_string_pretty(report) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("cortisoff: could not format the answer: {e}"),
    }
}

const USAGE: &str = "cortisoff, the client for a running cortisoffd

    doctor              what this machine can do, and what it cannot
    state               the live world, and who holds what
    trace               every event, rule edge and action, live
    rules               the rules as authored, as JSON
    enable <name>       switch a rule back on
    disable <name>      switch a rule off, keeping everything it is configured with
    set <cap> <value>   a manual override, outranking every rule
    clear <cap>         drop the override and give the rules the capability back
    reload              re-read the rule file
    stop                exit cleanly, restoring every baseline";

#[cfg(windows)]
mod transport {
    use cortisoff_ipc::{Request, Response};

    pub use cortisoff_win::Client;

    pub fn ask(request: Request) -> Result<Response, String> {
        cortisoff_win::ask(cortisoff_win::session_id(), request)
    }

    pub fn connect() -> Result<Client, String> {
        cortisoff_win::connect(cortisoff_win::session_id())
            .ok_or_else(|| "no daemon is listening on this session. start cortisoffd".to_owned())
    }
}

#[cfg(not(windows))]
mod transport {
    use cortisoff_ipc::{Frame, Request, Response};

    const MISSING: &str = "no transport is implemented for this platform yet";

    pub struct Client;

    impl Client {
        pub fn send(&mut self, _frame: &Frame) -> Result<(), String> {
            Err(MISSING.to_owned())
        }

        pub fn recv(&mut self) -> Option<Vec<Frame>> {
            None
        }
    }

    pub fn ask(_request: Request) -> Result<Response, String> {
        Err(MISSING.to_owned())
    }

    pub fn connect() -> Result<Client, String> {
        Err(MISSING.to_owned())
    }
}
