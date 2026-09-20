//! The daemon's own file log: what it did, and how it stopped.
//!
//! Deliberately small. The notifications a client subscribes to live in memory
//! and die with the process, so the one question they cannot answer is the one
//! that matters the morning after: why is the daemon not running any more. A
//! file answers it, as long as it holds the starts, the exits, the rule edges
//! and anything at warning level or worse, and nothing per event.
//!
//! The shape of a line is `<stamp> <mark> <text>`, and the mark is what makes
//! the file readable by a program as well as by a person: a run whose last
//! line is not an `exit` was killed or crashed, which is exactly what
//! [`DaemonLog::start`] reports back so the next run can say so.
//!
//! Nothing here calls the operating system beyond opening a file: the wall
//! clock arrives as a [`Stamp`], so the whole module is tested on any
//! platform.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Rotated past this, keeping one older file. A daemon that logs rule edges
/// and little else writes a few lines a day, so this is months of history and
/// still nothing a text editor struggles with.
const MAX_BYTES: u64 = 256 * 1024;

/// How much of the tail is read to find the previous run's last line.
const TAIL_BYTES: u64 = 8 * 1024;

/// What a line is about. Written as the second field so the file can be read
/// with `findstr exit` as well as by eye.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// A run begins.
    Start,
    /// A run ends on purpose, and the screen has been handed back.
    Exit,
    /// This process is not the daemon: another one already holds the session.
    Refused,
    /// A rule took what it claims.
    Hold,
    /// A rule gave it back.
    Release,
    Warn,
    Error,
    Panic,
    /// Anything else worth a line.
    Info,
}

impl Mark {
    fn word(self) -> &'static str {
        match self {
            Mark::Start => "start",
            Mark::Exit => "exit",
            Mark::Refused => "refused",
            Mark::Hold => "hold",
            Mark::Release => "release",
            Mark::Warn => "warning",
            Mark::Error => "error",
            Mark::Panic => "panic",
            Mark::Info => "info",
        }
    }
}

/// A wall clock reading, local time, handed in rather than read here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Stamp {
    /// Seconds since the Unix epoch, as UTC. The fallback for a platform with
    /// no local-time call of its own yet; Windows hands in `GetLocalTime`.
    pub fn from_unix_utc(secs: u64) -> Self {
        let days = (secs / 86_400) as i64;
        let rest = secs % 86_400;

        // civil_from_days, shifting the year to start in March so a leap day
        // lands at the end of it and the month arithmetic needs no table.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let day_of_era = z.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);

        Self {
            year: year as u16,
            month: month as u8,
            day: day as u8,
            hour: (rest / 3600) as u8,
            minute: (rest % 3600 / 60) as u8,
            second: (rest % 60) as u8,
        }
    }
}

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// How the run before this one ended, as far as the file can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Previous {
    /// No file, or nothing in it: this is the first run to log anything.
    None,
    /// It wrote an exit line, so it handed the screen back on the way out.
    Ended,
    /// It did not. It was killed, it crashed, or the machine went down with it.
    Cut,
}

/// One line, opening the file for it and closing it again.
///
/// For a writer that holds no state and may run at any moment: the panic hook
/// writes through this while the [`DaemonLog`] it would otherwise use is
/// borrowed by the thread that is unwinding. Appends are atomic enough for
/// that, and a failure here is silent because the process is already dying.
pub fn append(path: &Path, stamp: Stamp, mark: Mark, text: &str) {
    let text = text.replace(['\n', '\r'], " ");
    if let Ok(mut file) = OpenOptions::new().append(true).create(true).open(path) {
        let _ = writeln!(file, "{stamp} {:<7} {text}", mark.word());
    }
}

/// An open log file, or nothing at all when `--no-log` was passed or the file
/// could not be opened. A logging failure never stops the daemon: a daemon
/// that refuses to start is what leaves a screen orange.
#[derive(Debug)]
pub struct DaemonLog {
    path: PathBuf,
    file: Option<File>,
    warnings: Vec<String>,
}

impl DaemonLog {
    /// Opens the file for appending, creating the directory if it has to.
    /// Writes nothing: the first line belongs to [`DaemonLog::start`], which
    /// is called once this process knows it is the daemon.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut log = Self {
            path,
            file: None,
            warnings: Vec::new(),
        };
        if let Some(parent) = log.path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                log.warnings
                    .push(format!("could not create {}: {e}", parent.display()));
                return log;
            }
        }
        match OpenOptions::new().append(true).create(true).open(&log.path) {
            Ok(file) => log.file = Some(file),
            Err(e) => log
                .warnings
                .push(format!("could not open {}: {e}", log.path.display())),
        }
        log
    }

    /// A log that writes nowhere.
    pub fn off() -> Self {
        Self {
            path: PathBuf::new(),
            file: None,
            warnings: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether anything is actually written. `false` for [`DaemonLog::off`]
    /// and for a file that would not open.
    pub fn writing(&self) -> bool {
        self.file.is_some()
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Rotate if the file has grown past its cap, read how the previous run
    /// ended, then write the first line of this one.
    pub fn start(&mut self, stamp: Stamp, what: &str) -> Previous {
        if self.file.is_none() {
            return Previous::None;
        }
        self.rotate_if_large();
        let previous = self.previous_run();
        self.line(stamp, Mark::Start, what);
        if previous == Previous::Cut {
            self.line(
                stamp,
                Mark::Warn,
                "the previous run wrote no exit line: it was killed, it crashed, \
                 or the machine went down with it",
            );
        }
        previous
    }

    /// One line, on disk before this returns as far as the operating system is
    /// concerned. No `sync_all`: losing the tail to a machine that lost power
    /// costs a diagnosis, and paying for a flush on every rule edge costs the
    /// user's disk all day. The journal is the file that promises more.
    pub fn line(&mut self, stamp: Stamp, mark: Mark, text: &str) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        // One newline per line, whatever the text carried.
        let text = text.replace(['\n', '\r'], " ");
        let written =
            writeln!(file, "{stamp} {:<7} {text}", mark.word()).and_then(|()| file.flush());
        if let Err(e) = written {
            self.warnings
                .push(format!("could not write {}: {e}", self.path.display()));
            self.file = None;
        }
    }

    /// A line that would only repeat the one already at the end of the file is
    /// dropped.
    ///
    /// The logon task also tries to start the daemon every five minutes, so on
    /// a machine where someone started it by hand instead, a copy is refused
    /// the session nearly three hundred times a day. Written out, that is the
    /// only thing left in the file within a week, and the file exists to
    /// answer a question that needs the lines around it.
    pub fn line_once(&mut self, stamp: Stamp, mark: Mark, text: &str) {
        let text = text.replace(['\n', '\r'], " ");
        let rest = format!("{:<7} {text}", mark.word());
        // The stamp is fixed width, so everything a repeat would share with the
        // line before it starts at the same offset.
        let last = self.last_line();
        if last.as_deref().and_then(|line| line.get(20..)) == Some(rest.as_str()) {
            return;
        }
        self.line(stamp, mark, &text);
    }

    fn rotate_if_large(&mut self) {
        let too_big = self
            .file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|m| m.len() > MAX_BYTES);
        if !too_big {
            return;
        }
        let older = self.path.with_extension("previous.log");
        self.file = None;
        if let Err(e) = fs::rename(&self.path, &older) {
            self.warnings
                .push(format!("could not rotate {}: {e}", self.path.display()));
        }
        match OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
        {
            Ok(file) => self.file = Some(file),
            Err(e) => self
                .warnings
                .push(format!("could not reopen {}: {e}", self.path.display())),
        }
    }

    /// The last line of whatever is already in the file, read from the tail so
    /// a long file costs the same as a short one.
    fn previous_run(&mut self) -> Previous {
        let Some(last) = self.last_line() else {
            return Previous::None;
        };
        match last.split_whitespace().nth(2) {
            // `refused` is a second process saying it is not the daemon, so it
            // says nothing about the run that holds the session.
            Some(word) if word == Mark::Exit.word() => Previous::Ended,
            Some(_) => Previous::Cut,
            None => Previous::None,
        }
    }

    fn last_line(&mut self) -> Option<String> {
        let mut file = File::open(&self.path).ok()?;
        let len = file.metadata().ok()?.len();
        if len == 0 {
            return None;
        }
        let from = len.saturating_sub(TAIL_BYTES);
        file.seek(SeekFrom::Start(from)).ok()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        // Lossy on purpose: a tail cut mid-character must not lose the line.
        let tail = String::from_utf8_lossy(&bytes);
        tail.lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> Stamp {
        Stamp {
            year: 2026,
            month: 9,
            day: 20,
            hour: 3,
            minute: 18,
            second: 10,
        }
    }

    #[test]
    fn a_stamp_prints_padded_and_sortable() {
        assert_eq!(stamp().to_string(), "2026-09-20 03:18:10");
    }

    #[test]
    fn unix_seconds_become_a_civil_date() {
        assert_eq!(
            Stamp::from_unix_utc(0),
            Stamp {
                year: 1970,
                month: 1,
                day: 1,
                ..Default::default()
            }
        );
        // A leap day, and a second that is not midnight.
        assert_eq!(
            Stamp::from_unix_utc(1_600_000_000).to_string(),
            "2020-09-13 12:26:40"
        );
        assert_eq!(
            Stamp::from_unix_utc(1_709_209_845).to_string(),
            "2024-02-29 12:30:45"
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cortisoff-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("cortisoffd.log")
    }

    #[test]
    fn the_first_run_has_no_previous_one_and_the_directory_is_created() {
        let path = scratch("first");
        let mut log = DaemonLog::open(&path);
        assert!(log.writing(), "{:?}", log.warnings());
        assert_eq!(log.start(stamp(), "pid 1"), Previous::None);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "2026-09-20 03:18:10 start   pid 1\n");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_run_that_exited_is_told_apart_from_one_that_was_killed() {
        let path = scratch("ended");
        let mut log = DaemonLog::open(&path);
        log.start(stamp(), "pid 1");
        log.line(stamp(), Mark::Exit, "a client asked to stop");
        drop(log);

        let mut second = DaemonLog::open(&path);
        assert_eq!(second.start(stamp(), "pid 2"), Previous::Ended);
        // This one is killed: its last line is a rule edge.
        second.line(stamp(), Mark::Hold, "blue light at night");
        drop(second);

        let mut third = DaemonLog::open(&path);
        assert_eq!(third.start(stamp(), "pid 3"), Previous::Cut);
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("warning the previous run wrote no exit line"),
            "{text}"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A second process refusing to start says nothing about the run holding
    /// the session, so it must not read as a clean exit.
    #[test]
    fn a_refused_process_does_not_count_as_an_exit() {
        let path = scratch("refused");
        let mut log = DaemonLog::open(&path);
        log.start(stamp(), "pid 1");
        log.line(stamp(), Mark::Refused, "another cortisoffd holds it");
        drop(log);

        let mut second = DaemonLog::open(&path);
        assert_eq!(second.start(stamp(), "pid 2"), Previous::Cut);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The safety-net trigger starts a copy every five minutes. Each one is
    /// refused, and none of them may push the useful history out of the file.
    #[test]
    fn a_refusal_that_repeats_the_last_line_is_not_written_again() {
        let path = scratch("once");
        let mut log = DaemonLog::open(&path);
        log.start(stamp(), "pid 1");
        let said = "another cortisoffd is already running in this session";
        for _ in 0..50 {
            log.line_once(stamp(), Mark::Refused, said);
        }
        // Something else happened, so the next refusal is news again.
        log.line_once(stamp(), Mark::Hold, "blue light at night");
        log.line_once(stamp(), Mark::Refused, said);
        drop(log);

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("refused").count(), 2, "{text}");
        assert_eq!(text.lines().count(), 4, "{text}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_file_past_its_cap_is_rotated_and_one_older_file_is_kept() {
        let path = scratch("rotate");
        let mut log = DaemonLog::open(&path);
        log.start(stamp(), "pid 1");
        let filler = "x".repeat(1024);
        for _ in 0..(MAX_BYTES / 1024 + 1) {
            log.line(stamp(), Mark::Info, &filler);
        }
        drop(log);
        assert!(fs::metadata(&path).unwrap().len() > MAX_BYTES);

        let mut second = DaemonLog::open(&path);
        second.start(stamp(), "pid 2");
        let older = path.with_extension("previous.log");
        assert!(older.exists(), "the old file is kept, not deleted");
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("2026-09-20 03:18:10 start   pid 2"),
            "{text}"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A message carrying a newline would otherwise forge a line, and the mark
    /// of the forged one is what the next run reads.
    #[test]
    fn a_message_never_becomes_two_lines() {
        let path = scratch("newline");
        let mut log = DaemonLog::open(&path);
        log.start(stamp(), "pid 1");
        log.line(stamp(), Mark::Error, "first\nsecond\r\nthird");
        drop(log);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Logging is a convenience. A path that cannot be opened is a warning and
    /// the daemon carries on.
    #[test]
    fn a_log_that_cannot_be_opened_warns_and_writes_nothing() {
        let mut log = DaemonLog::off();
        assert!(!log.writing());
        assert_eq!(log.start(stamp(), "pid 1"), Previous::None);
        log.line(stamp(), Mark::Error, "nowhere to write this");
        assert!(log.warnings().is_empty());
    }
}
