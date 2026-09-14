//! `wallpaper.playback` on Windows, through whichever animated wallpaper
//! engine is running.
//!
//! Neither engine has an API a process can link. Both have a command line that
//! talks to the running instance, so that is the whole adapter: find the
//! running executable, run it again with a control argument, and do not wait.
//!
//! Wallpaper Engine is told `-control pause` or `-control play`. Lively is
//! told `app --play false` or `true`. Only a running engine is ever told
//! anything: `wallpaper64.exe -control play` against an engine that is not
//! running starts it, which is not what a rule about pausing means.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use cortisoff_host::{ids, Actuator, ActuatorError, CapabilityId, Control, Value};

use crate::startup::process_paths;

/// No console flashes up for a child started by a process that has none.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    WallpaperEngine,
    Lively,
}

impl Engine {
    const ALL: [Engine; 2] = [Engine::WallpaperEngine, Engine::Lively];

    fn executables(self) -> &'static [&'static str] {
        match self {
            Engine::WallpaperEngine => &["wallpaper64.exe", "wallpaper32.exe"],
            Engine::Lively => &["lively.exe"],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Engine::WallpaperEngine => "wallpaper-engine",
            Engine::Lively => "lively",
        }
    }

    /// Where the usual installers put it. A Steam library on another drive is
    /// missed here and found instead by the engine running at startup.
    fn installed(self) -> bool {
        let env = |key: &str| std::env::var_os(key).map(PathBuf::from);
        let candidates: Vec<PathBuf> = match self {
            Engine::WallpaperEngine => env("ProgramFiles(x86)")
                .map(|p| p.join(r"Steam\steamapps\common\wallpaper_engine\wallpaper64.exe"))
                .into_iter()
                .collect(),
            Engine::Lively => env("LOCALAPPDATA")
                .map(|p| p.join(r"Programs\Lively Wallpaper\Lively.exe"))
                .into_iter()
                .collect(),
        };
        candidates.iter().any(|p| p.exists())
    }
}

/// What to run so one engine ends up playing or paused.
pub fn command(engine: Engine, exe: &Path, playing: bool) -> (PathBuf, Vec<String>) {
    let args: Vec<&str> = match (engine, playing) {
        (Engine::WallpaperEngine, true) => vec!["-control", "play"],
        (Engine::WallpaperEngine, false) => vec!["-control", "pause"],
        (Engine::Lively, true) => vec!["app", "--play", "true"],
        (Engine::Lively, false) => vec!["app", "--play", "false"],
    };
    (
        exe.to_path_buf(),
        args.into_iter().map(str::to_owned).collect(),
    )
}

pub struct Wallpaper {
    engines: Vec<Engine>,
    backend: String,
}

impl Wallpaper {
    /// `None` when neither engine is installed where it usually is nor
    /// running, which is when this capability honestly does not exist.
    pub fn new() -> Option<Self> {
        let running = running();
        let engines: Vec<Engine> = Engine::ALL
            .into_iter()
            .filter(|e| e.installed() || running_exe(*e, &running).is_some())
            .collect();
        if engines.is_empty() {
            return None;
        }
        let backend = engines
            .iter()
            .map(|e| e.name())
            .collect::<Vec<_>>()
            .join("+");
        Some(Self { engines, backend })
    }

    pub fn backend_name(&self) -> &str {
        &self.backend
    }
}

impl Actuator for Wallpaper {
    fn id(&self) -> CapabilityId {
        CapabilityId::new(ids::WALLPAPER_PLAYBACK)
    }

    /// Always playing. Neither engine says whether it is paused, and an
    /// animated wallpaper is playing unless someone paused it by hand, in
    /// which case restoring it to playing is the harmless mistake.
    fn capture(&self) -> Result<Value, ActuatorError> {
        Ok(Value::Bool(true))
    }

    fn apply(&self, value: &Value) -> Result<(), ActuatorError> {
        let playing = value.as_bool().ok_or_else(|| {
            ActuatorError::on(
                self.id(),
                format!("expected true to play or false to pause, got {value:?}"),
            )
        })?;
        let running = running();
        for engine in &self.engines {
            // An engine that is not running has nothing on screen to pause.
            let Some(exe) = running_exe(*engine, &running) else {
                continue;
            };
            let (program, args) = command(*engine, &exe, playing);
            Command::new(&program)
                .args(&args)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| {
                    ActuatorError::on(
                        self.id(),
                        format!("could not run {}: {e}", program.display()),
                    )
                })?;
        }
        Ok(())
    }

    /// A paused wallpaper stays paused when the daemon dies.
    fn volatile(&self) -> bool {
        false
    }

    fn control(&self) -> Control {
        Control::Switch
    }
}

fn running() -> Vec<PathBuf> {
    let names: Vec<&str> = Engine::ALL
        .iter()
        .flat_map(|e| e.executables().iter().copied())
        .collect();
    process_paths(&names)
}

fn running_exe(engine: Engine, running: &[PathBuf]) -> Option<PathBuf> {
    running
        .iter()
        .find(|path| {
            path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                engine
                    .executables()
                    .iter()
                    .any(|e| n.eq_ignore_ascii_case(e))
            })
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_engine_gets_its_own_control_syntax() {
        let we = Path::new(r"C:\Steam\wallpaper_engine\wallpaper64.exe");
        assert_eq!(
            command(Engine::WallpaperEngine, we, false).1,
            vec!["-control", "pause"]
        );
        assert_eq!(
            command(Engine::WallpaperEngine, we, true).1,
            vec!["-control", "play"]
        );
        let lively = Path::new(r"C:\Lively\Lively.exe");
        assert_eq!(
            command(Engine::Lively, lively, false).1,
            vec!["app", "--play", "false"]
        );
    }

    #[test]
    fn a_running_engine_is_found_by_file_name_in_any_case() {
        let running = vec![
            PathBuf::from(r"C:\Windows\explorer.exe"),
            PathBuf::from(r"D:\Games\wallpaper_engine\Wallpaper32.EXE"),
        ];
        assert_eq!(
            running_exe(Engine::WallpaperEngine, &running),
            Some(running[1].clone())
        );
        assert_eq!(running_exe(Engine::Lively, &running), None);
    }
}
