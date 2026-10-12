//! The cortisoff app: one executable that is the window, the daemon
//! (`--daemon`), the installer and the updater.
//!
//! The window is an IPC client like the command line: it lists every plugin
//! the same way, shows what is happening now, and hosts each plugin's own
//! page. Closing it leaves the daemon running.

mod chrome;
pub mod client;
pub mod shell;

use std::path::Path;
use std::time::{Duration, Instant};

use cortisoff_ipc::Request;
use eframe::egui::{self, Ui};

pub use shell::{Shell, View};

/// Window size the layout is designed for, and what a capture uses.
pub const SIZE: [f32; 2] = [1040.0, 720.0];

/// The window as eframe runs it, shared by the binary and the captures so a
/// capture is the real root, with no harness margin around it.
pub struct App(pub Shell);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

/// How this start came about, which decides what it does before the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Double-clicked, from the Start Menu, or from the tray.
    Normal,
    /// The first start of the installed copy, right after installing.
    Installed,
    /// The first start after an update replaced the executable.
    Updated,
}

/// Install on the first run of a downloaded copy, take the session's window
/// slot, make sure a daemon runs, and open the window.
pub fn window(launch: Launch) -> Result<(), String> {
    let _ = cortisoff_update::cleanup_previous();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut problem = None;

    // A development build runs from Cargo's folder and installs nothing.
    if !cfg!(debug_assertions) {
        use cortisoff_update::install;
        let step = before_window(
            launch,
            install::is_legacy(&exe),
            install::is_installed(&exe),
            first_run(),
        );
        match step {
            Before::Migrate => match migrate(&exe, false) {
                // The bundle's build registers itself, rewrites the login
                // item if there was one, and opens the window.
                Ok(target) => {
                    std::process::Command::new(&target)
                        .arg("--updated")
                        .spawn()
                        .map_err(|e| format!("could not start {}: {e}", target.display()))?;
                    return Ok(());
                }
                Err(e) => problem = Some(format!("Not moved into the app bundle: {e}")),
            },
            // An installed copy as new or newer is started as it is, so an
            // old download opened again never undoes an update.
            Before::Place => {
                let next = match install::placement() {
                    install::Placement::UseInstalled(target) => Ok((target, None)),
                    install::Placement::Install => {
                        install::install(&exe).map(|target| (target, Some("--installed")))
                    }
                };
                match next {
                    Ok((target, arg)) => {
                        std::process::Command::new(&target)
                            .args(arg)
                            .spawn()
                            .map_err(|e| format!("could not start {}: {e}", target.display()))?;
                        return Ok(());
                    }
                    Err(e) => problem = Some(format!("Not installed: {e}")),
                }
            }
            Before::Settle => {
                if let Err(e) = install::install(&exe) {
                    problem = Some(format!("Not registered: {e}"));
                }
                if let Err(e) = cortisoff_daemon::start_at_logon(true) {
                    problem = Some(format!("Start at login: {e}"));
                }
            }
            Before::Logon => {
                if let Err(e) = cortisoff_daemon::start_at_logon(true) {
                    problem = Some(format!("Start at login: {e}"));
                }
            }
            // Rewrites the shortcut and the version Settings > Apps shows,
            // and the logon task, so a task an older build registered loses
            // what this one no longer wants: the five minute repetition that
            // undid every stop. The daemon is already stopped by now.
            Before::Register => {
                let _ = register();
            }
            Before::Nothing => {}
        }
    }

    // The copy that just updated or installed may still be closing.
    let patience = match launch {
        Launch::Normal => Duration::ZERO,
        _ => Duration::from_secs(5),
    };
    let Some(_lock) = claim_window(patience) else {
        raise_window();
        return Ok(());
    };

    if !client::daemon_running() {
        if let Err(e) = cortisoff_daemon::spawn_detached(&exe) {
            problem = Some(format!("Could not start cortisoff: {e}"));
        }
    }

    let mut viewport = chrome::viewport(
        egui::ViewportBuilder::default()
            .with_title("cortisoff")
            .with_app_id("cortisoff")
            .with_inner_size(SIZE)
            .with_min_inner_size([760.0, 520.0]),
    );
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon.png"))
    {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "cortisoff",
        options,
        Box::new(move |cc| {
            #[cfg(windows)]
            chrome::adopt(cc);
            let mut shell = Shell::live(&cc.egui_ctx);
            if let Some(problem) = problem {
                shell.warn(problem);
            }
            Ok(Box::new(App(shell)))
        }),
    )
    .map_err(|e| e.to_string())
}

/// What a release build does before its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Before {
    /// Running from v0.2.0's macOS folder: move into the bundle first.
    Migrate,
    /// A copy outside the install path: install it, or start the installed
    /// copy when that one is as new.
    Place,
    /// The installed copy's first run ever, placed there by the user rather
    /// than by the installer: register it and start it at login.
    Settle,
    /// Right after the installer: start at login.
    Logon,
    /// Right after an update: rewrite what the system knows of this build.
    Register,
    Nothing,
}

fn before_window(launch: Launch, legacy: bool, installed: bool, first_run: bool) -> Before {
    if legacy {
        return Before::Migrate;
    }
    match launch {
        Launch::Normal if !installed => Before::Place,
        Launch::Normal if first_run => Before::Settle,
        Launch::Normal => Before::Nothing,
        Launch::Installed => Before::Logon,
        Launch::Updated => Before::Register,
    }
}

/// No settings and no daemon log yet: cortisoff never ran for this user.
/// Anyone who turned start at login off has run it, so this never turns
/// it back on.
fn first_run() -> bool {
    cortisoff_daemon::Paths::from_env().is_ok_and(|paths| {
        [paths.store.parent(), paths.log.parent()]
            .into_iter()
            .flatten()
            .all(|folder| !folder.exists())
    })
}

/// Move a build v0.2.0's updater put in `~/Applications/Cortisoff` into
/// `~/Applications/Cortisoff.app`, which also removes the old folder, and
/// stop the daemon running from there. `daemon_coming` is the daemon's own
/// update: the old build asked for `--register` and exited, and its guard
/// is starting this build as the daemon, so wait for it before stopping it.
/// Answers the bundle's executable.
fn migrate(exe: &Path, daemon_coming: bool) -> Result<std::path::PathBuf, String> {
    let target = cortisoff_update::install::install(exe)?;
    if daemon_coming {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !client::daemon_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    stop_daemon();
    Ok(target)
}

/// `--register`: what a new build writes about itself once it is in place,
/// the shortcut, the version Settings > Apps shows and the logon task. The
/// window's update runs it on `--updated`; a daemon that updated itself
/// starts the new build with it, since only the new build knows its version.
/// Does nothing outside an installed release build.
///
/// From v0.2.0's macOS folder, it moves the install into the bundle, then
/// lets the bundle's build point the login item at itself and run the
/// daemon.
pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if cfg!(debug_assertions) || !cortisoff_update::install::is_installed(&exe) {
        return Ok(());
    }
    if cortisoff_update::install::is_legacy(&exe) {
        let target = migrate(&exe, true)?;
        std::process::Command::new(&target)
            .arg("--register")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", target.display()))?;
        return cortisoff_daemon::spawn_detached(&target);
    }
    cortisoff_update::install::install(&exe)?;
    if cortisoff_daemon::starts_at_logon() {
        cortisoff_daemon::start_at_logon(true)?;
    }
    Ok(())
}

/// `--uninstall`, and the button on the settings page: stop the daemon, stop
/// starting it at login, remove the app. Plugins and settings stay.
pub fn uninstall() -> Result<(), String> {
    stop_daemon();
    let mut problems = Vec::new();
    if cortisoff_daemon::starts_at_logon() {
        if let Err(e) = cortisoff_daemon::start_at_logon(false) {
            problems.push(e);
        }
    }
    if let Err(e) = cortisoff_update::install::uninstall() {
        problems.push(e);
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Put a downloaded update in place and start it. The daemon stops first so
/// the new window starts a daemon of the new version.
pub(crate) fn restart_into(staged: &Path) -> Result<(), String> {
    stop_daemon();
    cortisoff_update::apply(staged)?;
    cortisoff_update::relaunch(&["--updated".to_owned()])
}

/// Ask the daemon to exit, which restores every baseline, and wait until it
/// is gone.
fn stop_daemon() {
    if client::ask_once(Request::Shutdown).is_err() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while client::daemon_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
type WindowLock = cortisoff_win::InstanceLock;
#[cfg(unix)]
type WindowLock = cortisoff_unix::InstanceLock;

fn claim_window(patience: Duration) -> Option<WindowLock> {
    let deadline = Instant::now() + patience;
    loop {
        #[cfg(windows)]
        let lock = cortisoff_win::claim_dashboard();
        #[cfg(unix)]
        let lock = cortisoff_unix::claim_dashboard();
        if lock.is_some() || Instant::now() >= deadline {
            return lock;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn raise_window() {
    #[cfg(windows)]
    {
        cortisoff_win::raise_dashboard();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_outside_the_install_path_is_placed() {
        assert_eq!(
            before_window(Launch::Normal, false, false, true),
            Before::Place
        );
        assert_eq!(
            before_window(Launch::Normal, false, false, false),
            Before::Place
        );
    }

    #[test]
    fn a_self_placed_copy_settles_only_on_the_very_first_run() {
        assert_eq!(
            before_window(Launch::Normal, false, true, true),
            Before::Settle
        );
        // Run before: start at login stays as the user left it.
        assert_eq!(
            before_window(Launch::Normal, false, true, false),
            Before::Nothing
        );
    }

    #[test]
    fn installer_and_updater_starts_keep_their_steps() {
        for first_run in [false, true] {
            assert_eq!(
                before_window(Launch::Installed, false, true, first_run),
                Before::Logon
            );
            assert_eq!(
                before_window(Launch::Updated, false, true, first_run),
                Before::Register
            );
        }
    }

    #[test]
    fn the_old_macos_folder_always_migrates_first() {
        for launch in [Launch::Normal, Launch::Installed, Launch::Updated] {
            assert_eq!(before_window(launch, true, true, false), Before::Migrate);
        }
    }
}
