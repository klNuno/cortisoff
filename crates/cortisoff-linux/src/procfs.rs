//! What the kernel says through files: a process's executable and start
//! time, the power supply, whether a sound card is playing.
//!
//! Every parser here takes the text and returns a value, so the tests feed
//! them fixture strings and the readers stay a line each.

use std::fs;
use std::path::Path;

/// The executable behind a process, as a path. Falls back to the short
/// name in `comm` when the link cannot be read, which is what happens for a
/// process of another user.
pub fn exe(pid: u32) -> Option<String> {
    match fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(path) => Some(clean_exe(&path.to_string_lossy())),
        Err(_) => fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|name| name.trim_end().to_owned())
            .filter(|name| !name.is_empty()),
    }
}

/// The kernel appends ` (deleted)` to the link of a binary replaced on disk
/// since it started, which is what every package upgrade does to a running
/// browser. A rule naming the browser must still match it.
pub fn clean_exe(link: &str) -> String {
    link.strip_suffix(" (deleted)").unwrap_or(link).to_owned()
}

/// When the process started, in milliseconds since the epoch. Paired with
/// the pid it names one process for good, because pids are reused.
pub fn started_at_ms(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks = start_ticks(&stat)?;
    let boot = boot_time(&fs::read_to_string("/proc/stat").ok()?)?;
    let hz = rustix::param::clock_ticks_per_second().max(1);
    Some(boot * 1000 + ticks * 1000 / hz)
}

/// Field 22 of `/proc/<pid>/stat`, the start time in clock ticks after
/// boot. The second field is the command name in parentheses and may hold
/// spaces and parentheses of its own, so the count starts after the last
/// closing one.
pub fn start_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // After the name, field 3 is the first one: field 22 is the twentieth.
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// `btime` in `/proc/stat`: when the machine booted, in seconds since the
/// epoch.
pub fn boot_time(stat: &str) -> Option<u64> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime ")?.trim().parse().ok())
}

/// One entry of `/sys/class/power_supply`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Supply {
    /// `Mains`, `USB`, `Battery`, `UPS`.
    pub kind: String,
    pub online: Option<bool>,
    /// A battery's `Charging`, `Discharging`, `Full`, `Not charging`.
    pub status: Option<String>,
    /// `Device` for the battery of a mouse or a headset, which says nothing
    /// about what powers this machine.
    pub scope: Option<String>,
}

pub const POWER_SUPPLIES: &str = "/sys/class/power_supply";

/// `None` when the class is not there at all, a container or an odd kernel,
/// which is a missing capability rather than a desktop on mains.
pub fn supplies(root: &Path) -> Option<Vec<Supply>> {
    let entries = fs::read_dir(root).ok()?;
    let read = |dir: &Path, name: &str| {
        fs::read_to_string(dir.join(name))
            .ok()
            .map(|s| s.trim().to_owned())
    };
    Some(
        entries
            .flatten()
            .map(|entry| {
                let dir = entry.path();
                Supply {
                    kind: read(&dir, "type").unwrap_or_default(),
                    online: read(&dir, "online").map(|s| s == "1"),
                    status: read(&dir, "status"),
                    scope: read(&dir, "scope"),
                }
            })
            .collect(),
    )
}

/// Whether this machine runs on its battery.
///
/// A machine without a battery of its own never does, whatever else the
/// class lists: desktops with a USB-C controller report a `USB` supply that
/// is offline, and reading that as "on battery" would dim a desktop forever.
/// With a battery, any external supply online means mains. A laptop whose
/// adapter the kernel does not list falls back to the battery's own status.
pub fn on_battery(supplies: &[Supply]) -> bool {
    let own = |s: &&Supply| s.scope.as_deref() != Some("Device");
    let batteries: Vec<&Supply> = supplies
        .iter()
        .filter(own)
        .filter(|s| s.kind == "Battery")
        .collect();
    if batteries.is_empty() {
        return false;
    }
    let external: Vec<&Supply> = supplies
        .iter()
        .filter(own)
        .filter(|s| matches!(s.kind.as_str(), "Mains" | "USB"))
        .collect();
    if !external.is_empty() {
        return !external.iter().any(|s| s.online == Some(true));
    }
    batteries
        .iter()
        .any(|b| b.status.as_deref() == Some("Discharging"))
}

pub const ASOUND: &str = "/proc/asound";

/// Whether any playback substream of any card is running. Every status file
/// is `closed` or a few lines starting `state:`; only the running state
/// means samples are going out.
pub fn playing(root: &Path) -> bool {
    let Ok(cards) = fs::read_dir(root) else {
        return false;
    };
    for card in cards.flatten() {
        if !card.file_name().to_string_lossy().starts_with("card") {
            continue;
        }
        let Ok(pcms) = fs::read_dir(card.path()) else {
            continue;
        };
        for pcm in pcms.flatten() {
            let name = pcm.file_name().to_string_lossy().into_owned();
            // `pcm0p` is playback, `pcm0c` is capture: a microphone left open
            // by a voice chat is not sound playing.
            if !(name.starts_with("pcm") && name.ends_with('p')) {
                continue;
            }
            let Ok(subs) = fs::read_dir(pcm.path()) else {
                continue;
            };
            for sub in subs.flatten() {
                if let Ok(status) = fs::read_to_string(sub.path().join("status")) {
                    if running(&status) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

pub fn running(status: &str) -> bool {
    status.lines().any(|line| line.trim() == "state: RUNNING")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real line, from a process whose name holds a space and a closing
    /// parenthesis: the count must start after the last one.
    #[test]
    fn the_start_time_is_field_twenty_two_whatever_the_name_holds() {
        let stat = "4242 (Web Content) R 1 4242 4242 0 -1 4194560 1234 0 0 0 \
                    17 3 0 0 20 0 30 0 987654 123456789 4321 18446744073709551615 \
                    1 1 0 0 0 0 0 4096 0 0 0 17 3 0 0 0 0 0";
        assert_eq!(start_ticks(stat), Some(987_654));
        let odd = "7 (a) b (c)) S 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 55 0 0";
        assert_eq!(start_ticks(odd), Some(55));
        assert_eq!(start_ticks("7 (truncated) S 1"), None);
    }

    #[test]
    fn boot_time_comes_from_the_btime_line() {
        let stat =
            "cpu  1 2 3 4\ncpu0 1 2 3 4\nintr 99\nctxt 1234\nbtime 1760000000\nprocesses 77\n";
        assert_eq!(boot_time(stat), Some(1_760_000_000));
        assert_eq!(boot_time("cpu 1 2 3\n"), None);
    }

    #[test]
    fn an_upgraded_binary_keeps_its_name() {
        assert_eq!(
            clean_exe("/usr/lib/firefox/firefox (deleted)"),
            "/usr/lib/firefox/firefox"
        );
        assert_eq!(clean_exe("/usr/bin/foot"), "/usr/bin/foot");
    }

    fn supply(kind: &str, online: Option<bool>, status: Option<&str>) -> Supply {
        Supply {
            kind: kind.to_owned(),
            online,
            status: status.map(str::to_owned),
            scope: None,
        }
    }

    #[test]
    fn a_laptop_reads_its_adapter() {
        let plugged = [
            supply("Mains", Some(true), None),
            supply("Battery", None, Some("Charging")),
        ];
        assert!(!on_battery(&plugged));
        let unplugged = [
            supply("Mains", Some(false), None),
            supply("Battery", None, Some("Discharging")),
        ];
        assert!(on_battery(&unplugged));
    }

    /// The adapter on the barrel jack is offline, the USB-C one is not.
    #[test]
    fn any_external_supply_online_is_mains() {
        let supplies = [
            supply("Mains", Some(false), None),
            supply("USB", Some(true), None),
            supply("Battery", None, Some("Charging")),
        ];
        assert!(!on_battery(&supplies));
    }

    #[test]
    fn a_desktop_is_never_on_battery() {
        assert!(!on_battery(&[]));
        let usb_c_controller = [supply("USB", Some(false), None)];
        assert!(!on_battery(&usb_c_controller));
        let mouse = Supply {
            scope: Some("Device".to_owned()),
            ..supply("Battery", None, Some("Discharging"))
        };
        assert!(!on_battery(&[mouse]));
    }

    #[test]
    fn a_laptop_without_a_listed_adapter_reads_its_battery() {
        assert!(on_battery(&[supply("Battery", None, Some("Discharging"))]));
        assert!(!on_battery(&[supply("Battery", None, Some("Full"))]));
    }

    #[test]
    fn only_a_running_substream_is_sound() {
        assert!(!running("closed\n"));
        assert!(running(
            "state: RUNNING\nowner_pid   : 1234\ntrigger_time: 1.0\n"
        ));
        assert!(!running("state: PREPARED\nowner_pid   : 1234\n"));
    }
}
