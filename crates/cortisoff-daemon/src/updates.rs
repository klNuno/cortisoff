//! Automatic updates, from the daemon.
//!
//! A thread checks the release feed two minutes after the daemon starts, then
//! every six hours, sleeping in between: one wait in the kernel, no timer on
//! the daemon's loop. With automatic installs on it downloads and verifies a
//! newer build into the temporary folder and hands the loop its path. The
//! loop puts it in place only at a quiet moment, when nothing holds any
//! capability, so a night tint never flashes off and back for an update. It
//! then stops the plugins and exits with [`crate::guard::RESTART`], and the
//! guard starts the new build.
//!
//! [`Updates`] is the loop's side, plain state a test drives without a
//! network. [`spawn`] is the thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use cortisoff_host::Waker;
use cortisoff_ipc::{UpdateCheck, UpdateReport};

/// The first check, after the daemon has settled and the network is up.
pub const FIRST_CHECK: Duration = Duration::from_secs(2 * 60);
/// Every check after that.
pub const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// What one check hands the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Current,
    /// Newer, and automatic installs were off when it was found.
    Available(String),
    /// Newer, downloaded and verified at this path.
    Staged {
        path: PathBuf,
        version: String,
    },
    Failed(String),
    /// Worth a line in the log and nothing else: the release's client could
    /// not be fetched or verified, so the app updates alone.
    Warning(String),
}

/// A verified build waiting for its moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub path: PathBuf,
    pub version: String,
}

/// The loop's side: the setting, the last result, and a staged build.
pub struct Updates {
    /// Shared with the thread, which reads it before each download.
    auto: Arc<AtomicBool>,
    /// Whether a thread checks at all.
    checks: bool,
    last: Option<UpdateCheck>,
    staged: Option<Staged>,
    /// Asks the thread to check now rather than at its next turn.
    poke: Option<Sender<()>>,
}

impl Updates {
    pub fn new(auto: bool) -> Self {
        Self {
            auto: Arc::new(AtomicBool::new(auto)),
            checks: false,
            last: None,
            staged: None,
            poke: None,
        }
    }

    /// A thread now checks: it reads `auto_flag` and listens on the other end
    /// of `poke`.
    pub fn checking(&mut self, poke: Sender<()>) {
        self.checks = true;
        self.poke = Some(poke);
    }

    pub fn auto_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.auto)
    }

    pub fn auto(&self) -> bool {
        self.auto.load(Ordering::Relaxed)
    }

    /// Switch automatic installs on or off. Off drops a build waiting for its
    /// moment, which is then only available. On after a check found one asks
    /// the thread to check again now, so it does not wait six hours.
    pub fn set_auto(&mut self, on: bool) {
        self.auto.store(on, Ordering::Relaxed);
        if on {
            if matches!(self.last, Some(UpdateCheck::Available { .. })) {
                if let Some(poke) = &self.poke {
                    let _ = poke.send(());
                }
            }
        } else if let Some(staged) = self.staged.take() {
            let _ = std::fs::remove_file(&staged.path);
            self.last = Some(UpdateCheck::Available {
                version: staged.version,
            });
        }
    }

    /// What the thread found. A build staged while automatic installs were
    /// switched off is dropped rather than installed.
    pub fn found(&mut self, found: Found) {
        let last = match found {
            Found::Current => {
                self.staged = None;
                UpdateCheck::Current
            }
            Found::Available(version) => UpdateCheck::Available { version },
            Found::Staged { path, version } if self.auto() => {
                self.staged = Some(Staged {
                    path,
                    version: version.clone(),
                });
                UpdateCheck::Staged { version }
            }
            Found::Staged { path, version } => {
                let _ = std::fs::remove_file(&path);
                UpdateCheck::Available { version }
            }
            Found::Failed(message) => UpdateCheck::Failed { message },
            // The server writes it to the log; the result stays as it was.
            Found::Warning(_) => return,
        };
        self.last = Some(last);
    }

    /// The staged build, handed over once and only when `quiet`.
    pub fn take(&mut self, quiet: bool) -> Option<Staged> {
        if quiet && self.auto() {
            self.staged.take()
        } else {
            None
        }
    }

    /// A build is waiting for a quiet moment.
    pub fn waiting(&self) -> bool {
        self.staged.is_some()
    }

    /// Putting the build in place failed: keep running this one and say so.
    pub fn failed(&mut self, message: String) {
        self.staged = None;
        self.last = Some(UpdateCheck::Failed { message });
    }

    pub fn report(&self) -> UpdateReport {
        UpdateReport {
            auto: self.auto(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            checks: self.checks,
            last: self.last.clone(),
        }
    }
}

/// Whether this daemon should check at all: a release build running from
/// its install folder, like the window's own install and update logic.
pub fn should_check() -> bool {
    !cfg!(debug_assertions)
        && std::env::current_exe().is_ok_and(|exe| cortisoff_update::install::is_installed(&exe))
}

/// Start the thread that checks. It ends when the loop drops its end of
/// either channel.
pub fn spawn(
    auto: Arc<AtomicBool>,
    pokes: Receiver<()>,
    found: Sender<Found>,
    wake: Option<Waker>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("cortisoff-updates".into())
        .spawn(move || {
            let dir = std::env::temp_dir().join("cortisoff-update");
            let mut wait = FIRST_CHECK;
            let mut staged: Option<Staged> = None;
            loop {
                match pokes.recv_timeout(wait) {
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                wait = EVERY;
                let (result, warning) = check_once(
                    auto.load(Ordering::Relaxed),
                    &mut staged,
                    || {
                        cortisoff_update::check(
                            cortisoff_update::FEED_URL,
                            env!("CARGO_PKG_VERSION"),
                        )
                    },
                    |release| cortisoff_update::download_pair(release, &dir),
                );
                if let Some(warning) = warning {
                    if found.send(Found::Warning(warning)).is_err() {
                        return;
                    }
                }
                if found.send(result).is_err() {
                    return;
                }
                if let Some(wake) = &wake {
                    wake();
                }
            }
        })
        .map(drop)
}

/// One check, and why the release's client was left out when it was. A
/// version already staged by an earlier check is not downloaded again while
/// its file is still there.
fn check_once(
    auto: bool,
    staged: &mut Option<Staged>,
    check: impl FnOnce() -> Result<Option<cortisoff_update::Release>, String>,
    download: impl FnOnce(&cortisoff_update::Release) -> Result<(PathBuf, Option<String>), String>,
) -> (Found, Option<String>) {
    let release = match check() {
        Ok(Some(release)) => release,
        Ok(None) => return (Found::Current, None),
        Err(e) => return (Found::Failed(e), None),
    };
    if !auto {
        return (Found::Available(release.version), None);
    }
    if let Some(done) = staged
        .as_ref()
        .filter(|s| s.version == release.version && s.path.exists())
    {
        let found = Found::Staged {
            path: done.path.clone(),
            version: done.version.clone(),
        };
        return (found, None);
    }
    match download(&release) {
        Ok((path, warning)) => {
            *staged = Some(Staged {
                path: path.clone(),
                version: release.version.clone(),
            });
            let found = Found::Staged {
                path,
                version: release.version,
            };
            (found, warning)
        }
        Err(e) => (Found::Failed(e), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str) -> cortisoff_update::Release {
        cortisoff_update::Release {
            version: version.to_owned(),
            notes: String::new(),
            pub_date: None,
            platform: "windows-x86_64".to_owned(),
            url: "https://example.invalid/cortisoff".to_owned(),
            signature: String::new(),
            client: None,
        }
    }

    fn staged(version: &str) -> Found {
        Found::Staged {
            path: std::env::temp_dir().join(format!("cortisoff-test-not-there-{version}")),
            version: version.to_owned(),
        }
    }

    #[test]
    fn a_check_with_automatic_installs_off_never_downloads() {
        let mut none = None;
        let (found, _) = check_once(
            false,
            &mut none,
            || Ok(Some(release("9.0.0"))),
            |_| panic!("downloaded with automatic installs off"),
        );
        assert_eq!(found, Found::Available("9.0.0".into()));
    }

    #[test]
    fn a_version_already_staged_is_not_downloaded_again() {
        let file = std::env::temp_dir().join(format!("cortisoff-staged-{}", std::process::id()));
        std::fs::write(&file, b"build").unwrap();
        let mut done = Some(Staged {
            path: file.clone(),
            version: "9.0.0".into(),
        });
        let (found, _) = check_once(
            true,
            &mut done,
            || Ok(Some(release("9.0.0"))),
            |_| panic!("downloaded twice"),
        );
        assert_eq!(
            found,
            Found::Staged {
                path: file.clone(),
                version: "9.0.0".into()
            }
        );
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn a_client_left_out_is_a_warning_and_the_app_still_stages() {
        let mut none = None;
        let (found, warning) = check_once(
            true,
            &mut none,
            || Ok(Some(release("9.0.0"))),
            |_| Ok((PathBuf::from("staged-app"), Some("client refused".into()))),
        );
        assert_eq!(
            found,
            Found::Staged {
                path: PathBuf::from("staged-app"),
                version: "9.0.0".into()
            }
        );
        assert_eq!(warning.as_deref(), Some("client refused"));

        // The warning leaves the result and the staged build as they are.
        let mut updates = Updates::new(true);
        updates.found(found);
        updates.found(Found::Warning("client refused".into()));
        assert!(updates.waiting());
        assert_eq!(
            updates.report().last,
            Some(UpdateCheck::Staged {
                version: "9.0.0".into()
            })
        );
    }

    #[test]
    fn a_staged_build_waits_for_quiet_and_goes_once() {
        let mut updates = Updates::new(true);
        updates.found(staged("9.0.0"));
        assert!(updates.waiting());
        assert_eq!(updates.take(false), None);
        assert_eq!(updates.take(true).map(|s| s.version), Some("9.0.0".into()));
        assert_eq!(updates.take(true), None);
    }

    #[test]
    fn switching_automatic_installs_off_drops_the_staged_build() {
        let mut updates = Updates::new(true);
        updates.found(staged("9.0.0"));
        updates.set_auto(false);
        assert!(!updates.waiting());
        assert_eq!(updates.take(true), None);
        assert_eq!(
            updates.report().last,
            Some(UpdateCheck::Available {
                version: "9.0.0".into()
            })
        );
    }

    #[test]
    fn switching_it_back_on_asks_for_a_check_now() {
        let (poke, pokes) = std::sync::mpsc::channel();
        let mut updates = Updates::new(false);
        updates.checking(poke);
        updates.found(Found::Available("9.0.0".into()));
        updates.set_auto(true);
        assert!(pokes.try_recv().is_ok());
        assert!(updates.report().auto && updates.report().checks);
    }
}
