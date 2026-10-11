//! `cortisoff`: the window by default, the daemon with `--daemon`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

use cortisoff_app::Launch;

const HELP: &str = "cortisoff, plugins that act on your desktop

usage:
    cortisoff               open the window, installing on first run
    cortisoff --daemon      run the plugins (see cortisoff --daemon --help)
    cortisoff --uninstall   stop it, and remove the app; plugins stay
    cortisoff --version
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    let result = match first.as_deref() {
        None => cortisoff_app::window(Launch::Normal),
        Some("--installed") => cortisoff_app::window(Launch::Installed),
        Some("--updated") => cortisoff_app::window(Launch::Updated),
        Some("--daemon") => {
            let rest: Vec<String> = args.collect();
            if rest.iter().any(|a| a == "--help" || a == "-h") {
                console();
                print!("{}", cortisoff_daemon::USAGE);
                return ExitCode::SUCCESS;
            }
            cortisoff_daemon::Options::parse(rest).and_then(cortisoff_daemon::run)
        }
        Some("--uninstall") => cortisoff_app::uninstall(),
        Some("--version" | "-V") => {
            console();
            println!("cortisoff {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") => {
            console();
            print!("{HELP}");
            Ok(())
        }
        Some(other) => {
            console();
            Err(format!("unknown argument {other}\n\n{HELP}"))
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cortisoff: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A release build has no console of its own: borrow the terminal it was
/// started from, if any, so `--help` prints somewhere.
fn console() {
    #[cfg(windows)]
    {
        cortisoff_win::attach_parent_console();
    }
}
