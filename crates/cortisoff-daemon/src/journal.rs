//! The file behind [`JournalStore`], and the only place in the project that
//! promises something survives the process dying.
//!
//! The contract is `record` returning only once the entry is on disk. That
//! rules out buffering and it rules out writing in place: a rewrite that is
//! interrupted halfway leaves a truncated file, which is worse than no file at
//! all because the screen stays tinted and nothing knows why. So every change
//! writes a sibling temporary file, flushes it, and renames it over the real
//! one. A rename is atomic on every filesystem the daemon runs on.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use cortisoff_host::{ActuatorError, CapabilityId, JournalEntry, JournalStore, Value};
use serde::{Deserialize, Serialize};

/// Bumped when the on-disk shape changes. A file from the future is left alone
/// rather than parsed optimistically.
const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct OnDisk {
    version: u32,
    entries: Vec<JournalEntry>,
}

#[derive(Debug)]
pub struct FileJournal {
    path: PathBuf,
    temp: PathBuf,
    entries: BTreeMap<CapabilityId, Value>,
    /// Things the doctor should say out loud. A journal problem never stops
    /// the daemon, because refusing to start is what leaves a screen orange.
    warnings: Vec<String>,
}

fn io(path: &Path, e: std::io::Error) -> ActuatorError {
    ActuatorError::new(format!("journal at {}: {e}", path.display()))
}

impl FileJournal {
    /// Read what a previous run left, creating nothing. The directory is
    /// created on the first write instead, so a read-only run touches no disk.
    ///
    /// A file that cannot be parsed is moved aside rather than deleted and the
    /// run continues with an empty journal, which is a warning and not an
    /// error: the baselines in it are lost either way, and a daemon that
    /// refuses to start cannot restore anything.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let temp = path.with_extension("json.tmp");
        let mut journal = Self {
            path,
            temp,
            entries: BTreeMap::new(),
            warnings: Vec::new(),
        };

        let raw = match fs::read_to_string(&journal.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return journal,
            Err(e) => {
                journal
                    .warnings
                    .push(format!("could not read {}: {e}", journal.path.display()));
                return journal;
            }
        };

        match serde_json::from_str::<OnDisk>(&raw) {
            Ok(disk) if disk.version == VERSION => {
                journal.entries = disk
                    .entries
                    .into_iter()
                    .map(|e| (e.cap, e.baseline))
                    .collect();
            }
            Ok(disk) => journal.warnings.push(format!(
                "{} was written by version {} of the journal format, this build speaks {VERSION}. \
                 it is left untouched, and any value it protects has to be put back by hand",
                journal.path.display(),
                disk.version
            )),
            Err(e) => {
                let aside = journal.path.with_extension("corrupt.json");
                let moved = fs::rename(&journal.path, &aside).is_ok();
                journal.warnings.push(format!(
                    "{} is unreadable ({e}), so the baselines a previous run left are gone. {}",
                    journal.path.display(),
                    if moved {
                        format!("it was kept at {}", aside.display())
                    } else {
                        "it could not be moved aside either".to_owned()
                    }
                ));
            }
        }
        journal
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Whole-file rewrite, durable before it returns. The entry count is a
    /// handful of capabilities, so there is nothing to gain from an append log
    /// and the compaction pass that would come with it.
    fn flush(&self) -> Result<(), ActuatorError> {
        let disk = OnDisk {
            version: VERSION,
            entries: self
                .entries
                .iter()
                .map(|(cap, baseline)| JournalEntry {
                    cap: cap.clone(),
                    baseline: baseline.clone(),
                })
                .collect(),
        };
        let body = serde_json::to_vec(&disk)
            .map_err(|e| ActuatorError::new(format!("journal could not be serialised: {e}")))?;

        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        }

        {
            let mut file = fs::File::create(&self.temp).map_err(|e| io(&self.temp, e))?;
            file.write_all(&body).map_err(|e| io(&self.temp, e))?;
            // The whole point of the module. Without this the bytes sit in the
            // page cache and a power cut loses exactly the entry that was
            // about to protect the screen.
            file.sync_all().map_err(|e| io(&self.temp, e))?;
        }

        fs::rename(&self.temp, &self.path).map_err(|e| io(&self.path, e))?;

        // The rename itself is metadata, and on unix it is durable only once
        // the directory is flushed. Windows has no directory handle to sync
        // and MoveFileEx has already ordered it.
        #[cfg(unix)]
        if let Some(dir) = self.path.parent() {
            if let Ok(handle) = fs::File::open(dir) {
                let _ = handle.sync_all();
            }
        }

        Ok(())
    }
}

impl JournalStore for FileJournal {
    fn record(&mut self, cap: &CapabilityId, baseline: &Value) -> Result<(), ActuatorError> {
        let previous = self.entries.insert(cap.clone(), baseline.clone());
        match self.flush() {
            Ok(()) => Ok(()),
            Err(e) => {
                // The dispatcher aborts the apply when this fails, so memory
                // has to agree with the disk it could not write.
                match previous {
                    Some(old) => self.entries.insert(cap.clone(), old),
                    None => self.entries.remove(cap),
                };
                Err(e)
            }
        }
    }

    fn clear(&mut self, cap: &CapabilityId) -> Result<(), ActuatorError> {
        let Some(previous) = self.entries.remove(cap) else {
            return Ok(());
        };
        match self.flush() {
            Ok(()) => Ok(()),
            Err(e) => {
                self.entries.insert(cap.clone(), previous);
                Err(e)
            }
        }
    }

    fn dirty(&self) -> Result<Vec<JournalEntry>, ActuatorError> {
        Ok(self
            .entries
            .iter()
            .map(|(cap, baseline)| JournalEntry {
                cap: cap.clone(),
                baseline: baseline.clone(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that removes itself, so these tests need no crate for it.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("cortisoff-journal-{name}"));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join("journal.json")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn cap(s: &str) -> CapabilityId {
        CapabilityId::new(s)
    }

    #[test]
    fn a_missing_file_is_an_empty_journal_and_writes_nothing() {
        let scratch = Scratch::new("missing");
        let journal = FileJournal::open(scratch.file());
        assert!(journal.dirty().unwrap().is_empty());
        assert!(journal.warnings().is_empty());
        assert!(!scratch.file().exists());
    }

    #[test]
    fn an_entry_survives_reopening() {
        let scratch = Scratch::new("survives");
        let mut journal = FileJournal::open(scratch.file());
        journal
            .record(&cap("display.color_temp"), &Value::Int(6500))
            .unwrap();

        let reopened = FileJournal::open(scratch.file());
        assert_eq!(
            reopened.dirty().unwrap(),
            vec![JournalEntry {
                cap: cap("display.color_temp"),
                baseline: Value::Int(6500),
            }]
        );
    }

    #[test]
    fn clearing_the_last_entry_leaves_an_empty_journal_on_disk() {
        let scratch = Scratch::new("clear");
        let mut journal = FileJournal::open(scratch.file());
        journal
            .record(&cap("display.mode"), &Value::Int(1))
            .unwrap();
        journal.clear(&cap("display.mode")).unwrap();
        assert!(FileJournal::open(scratch.file())
            .dirty()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn clearing_something_that_was_never_recorded_is_not_an_error() {
        let scratch = Scratch::new("clear-absent");
        let mut journal = FileJournal::open(scratch.file());
        journal.clear(&cap("display.mode")).unwrap();
    }

    #[test]
    fn the_parent_directory_is_created_on_the_first_write() {
        let scratch = Scratch::new("mkdir");
        let nested = scratch.0.join("a").join("b").join("journal.json");
        let mut journal = FileJournal::open(&nested);
        journal
            .record(&cap("display.vibrance"), &Value::Int(50))
            .unwrap();
        assert!(nested.exists());
    }

    /// Losing the file is bad. Refusing to start because of it is worse, since
    /// nothing else will ever put the screen back.
    #[test]
    fn a_corrupt_file_is_moved_aside_and_warned_about() {
        let scratch = Scratch::new("corrupt");
        fs::write(scratch.file(), b"{ this is not json").unwrap();
        let journal = FileJournal::open(scratch.file());
        assert!(journal.dirty().unwrap().is_empty());
        assert_eq!(journal.warnings().len(), 1);
        assert!(scratch.0.join("journal.corrupt.json").exists());
    }

    /// A newer daemon's file is not guessed at, and it is not destroyed
    /// either: the user may be running two builds.
    #[test]
    fn a_file_from_a_future_version_is_left_alone() {
        let scratch = Scratch::new("future");
        fs::write(scratch.file(), b"{\"version\":99,\"entries\":[]}").unwrap();
        let journal = FileJournal::open(scratch.file());
        assert!(journal.dirty().unwrap().is_empty());
        assert!(journal.warnings()[0].contains("99"));
        assert!(scratch.file().exists());
    }

    #[test]
    fn recording_twice_on_one_capability_keeps_the_newer_baseline() {
        let scratch = Scratch::new("overwrite");
        let mut journal = FileJournal::open(scratch.file());
        journal
            .record(&cap("display.mode"), &Value::Int(1))
            .unwrap();
        journal
            .record(&cap("display.mode"), &Value::Int(2))
            .unwrap();
        let entries = FileJournal::open(scratch.file()).dirty().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].baseline, Value::Int(2));
    }

    #[test]
    fn the_temporary_file_does_not_outlive_the_write() {
        let scratch = Scratch::new("temp");
        let mut journal = FileJournal::open(scratch.file());
        journal
            .record(&cap("display.mode"), &Value::Int(1))
            .unwrap();
        assert!(!scratch.0.join("journal.json.tmp").exists());
    }
}
