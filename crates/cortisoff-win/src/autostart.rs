//! Starting the daemon when the user logs on, through Task Scheduler.
//!
//! Not the `Run` registry key: a task can wait a few seconds for the desktop
//! to settle, and it never needs a UAC prompt. The task is registered from XML
//! rather than with `schtasks /SC ONLOGON`, because that form means any user
//! logging on and needs an administrator. A logon trigger naming this user,
//! running with this user's own token, does not.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// The name the task shows in Task Scheduler.
pub const TASK_NAME: &str = "cortisoff";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn enable(daemon: &Path) -> Result<(), String> {
    enable_named(TASK_NAME, daemon)
}

pub fn disable() -> Result<(), String> {
    disable_named(TASK_NAME)
}

pub fn enabled() -> bool {
    exists(TASK_NAME)
}

fn enable_named(name: &str, daemon: &Path) -> Result<(), String> {
    let user = current_user().ok_or("could not tell which user is logged on")?;
    let xml = task_xml(daemon, &user);
    let file = std::env::temp_dir().join(format!("{name}-task.xml"));
    // UTF-16 with a byte order mark. schtasks reads other encodings in ways
    // that depend on the system locale, and fails on an accent in a path.
    let mut bytes = vec![0xFF, 0xFE];
    for unit in xml.encode_utf16() {
        bytes.extend(unit.to_le_bytes());
    }
    std::fs::write(&file, bytes).map_err(|e| format!("could not write {}: {e}", file.display()))?;
    let file_arg = file.to_string_lossy().into_owned();
    let result = schtasks(&["/Create", "/TN", name, "/XML", &file_arg, "/F"]);
    let _ = std::fs::remove_file(&file);
    result
}

fn disable_named(name: &str) -> Result<(), String> {
    if !exists(name) {
        return Ok(());
    }
    schtasks(&["/Delete", "/TN", name, "/F"])
}

fn exists(name: &str) -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", name])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn schtasks(args: &[&str]) -> Result<(), String> {
    let out = Command::new("schtasks")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("could not run schtasks: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    Err(format!("schtasks refused: {}", said.trim()))
}

fn current_user() -> Option<String> {
    let user = std::env::var("USERNAME").ok().filter(|u| !u.is_empty())?;
    Some(match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => format!("{domain}\\{user}"),
        _ => user,
    })
}

/// The task, as Task Scheduler's own schema. Ten seconds after logon, this
/// user's token, no time limit, never a second copy, and nothing that stops it
/// on battery.
///
/// One trigger, the logon. A crash in between is the guard's to answer
/// (`cortisoff-daemon`, `guard.rs`): `--daemon` waits on the process that
/// runs the plugins and starts it again only when it crashed. The task used to
/// carry a second trigger repeating every five minutes for that, which could
/// not tell a crash from a stop and undid every stop within five minutes.
///
/// `RestartOnFailure` is no safety net, whatever its name suggests: on
/// Windows 11 26200 neither a non-zero exit nor an action that cannot be
/// launched at all arms it. It stays because it costs nothing. `TRAPS.md`,
/// platform and process model.
pub fn task_xml(daemon: &Path, user: &str) -> String {
    let command = escape(&daemon.to_string_lossy());
    let user = escape(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts the cortisoff daemon when you log on.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
      <Delay>PT10S</Delay>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>--daemon</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_names_the_user_and_the_daemon_escaped() {
        let xml = task_xml(Path::new(r"C:\Tom & Jerry\cortisoff.exe"), r"PC\tom");
        assert!(xml.contains(r"<Command>C:\Tom &amp; Jerry\cortisoff.exe</Command>"));
        assert!(xml.contains("<Arguments>--daemon</Arguments>"));
        assert_eq!(xml.matches(r"<UserId>PC\tom</UserId>").count(), 2);
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
    }

    /// A stop asked by the user lasts until the next logon: nothing in the
    /// task starts the daemon on a timer.
    #[test]
    fn only_a_logon_starts_the_daemon() {
        let xml = task_xml(Path::new(r"C:\cortisoff.exe"), "tom");
        assert_eq!(
            xml.matches("Trigger>").count(),
            2,
            "one trigger, open and close"
        );
        assert!(xml.contains("<LogonTrigger>"));
        assert!(!xml.contains("<Repetition>"));
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
    }

    /// Creates a real task and deletes it straight away. Ignored by default
    /// because it changes the machine, however briefly:
    /// `cargo test -p cortisoff-win -- --ignored autostart`.
    #[test]
    #[ignore = "registers and removes a real scheduled task"]
    fn autostart_registers_and_removes_a_logon_task_without_elevation() {
        let name = "cortisoff-selftest";
        let exe = std::env::current_exe().unwrap();
        enable_named(name, &exe).expect("a logon task for this user needs no elevation");
        assert!(exists(name));
        disable_named(name).expect("and it goes away again");
        assert!(!exists(name));
    }
}
