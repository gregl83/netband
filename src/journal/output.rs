//! Own a fixed journal or a directory of durable, recoverable CSV segments.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};

use super::{JournalError, JournalSink, JournalWriter, lock_file};
use crate::config::OutputTarget;
use crate::model::MeasurementEvent;

const LOCK: &str = ".netband-output.lock";
const ACTIVE: &str = ".netband-active";
const PENDING: &str = ".netband-header.tmp";
const MARKER_TEMP: &str = ".netband-active.tmp";

#[derive(Debug)]
struct Directory {
    path: PathBuf,
    _lock: File,
    date: NaiveDate,
    max_bytes: Option<u64>,
}

/// Manage fixed-file or rotating-directory output, ownership, and recovery.
#[derive(Debug)]
pub struct Journal {
    journal: JournalWriter<File>,
    path: PathBuf,
    directory: Option<Directory>,
    bytes: u64,
    has_events: bool,
    poisoned: bool,
}

impl Journal {
    pub fn open_at(
        target: &OutputTarget,
        max_bytes: Option<u64>,
        now: DateTime<Utc>,
    ) -> Result<Self, JournalError> {
        if max_bytes == Some(0) || (max_bytes.is_some() && matches!(target, OutputTarget::File(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "positive rotate_max_bytes requires directory output",
            )
            .into());
        }
        let directory = match target {
            OutputTarget::File(_) => None,
            OutputTarget::Directory(path) => {
                let lock_path = path.join(LOCK);
                regular_or_missing(&lock_path)?;
                let lock = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(&lock_path)?;
                lock_file(&lock, &lock_path)?;
                let directory = Directory {
                    path: path.clone(),
                    _lock: lock,
                    date: now.date_naive(),
                    max_bytes,
                };
                directory.recover()?;
                Some(directory)
            }
        };
        let (journal, path) = match &directory {
            Some(directory) => directory.create_segment(now)?,
            None => JournalWriter::open_at(target, now)?,
        };
        let bytes = journal.writer.get_ref().metadata()?.len();
        tracing::info!(path = %path.display(), "measurement journal opened");
        Ok(Self {
            journal,
            path,
            directory,
            bytes,
            has_events: false,
            poisoned: false,
        })
    }

    /// The current segment, which may change after each successful batch.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append_batch_at(
        &mut self,
        events: &[MeasurementEvent],
        now: DateTime<Utc>,
    ) -> Result<(), JournalError> {
        self.check_healthy()?;
        let result = self.append(events, now);
        self.poisoned = result.is_err();
        result.map_err(|error| contextual(&self.path, "append/rotate", error))
    }

    fn append(
        &mut self,
        events: &[MeasurementEvent],
        now: DateTime<Utc>,
    ) -> Result<(), JournalError> {
        if events.is_empty() {
            return Ok(());
        }
        let Some(directory) = &mut self.directory else {
            return self.journal.append_batch(events);
        };
        // Buffer only the incoming batch, never a segment or historical journal.
        let mut encoded = JournalWriter::without_header(Vec::new());
        encoded.append_batch(events)?;
        let encoded = encoded.into_inner()?;
        let later_date = now.date_naive() > directory.date;
        let oversized = self.has_events
            && directory
                .max_bytes
                .is_some_and(|limit| self.bytes.saturating_add(encoded.len() as u64) > limit);
        if later_date || oversized {
            step(Step::OldSync)?;
            self.journal.flush()?;
            let (journal, path) = directory.create_segment(now)?;
            self.journal = journal;
            self.path = path;
            directory.date = directory.date.max(now.date_naive());
            self.bytes = self.journal.writer.get_ref().metadata()?.len();
            self.has_events = false;
            tracing::info!(path = %self.path.display(), "measurement journal rotated");
        }
        step(Step::BatchWrite)?;
        let mut file = self.journal.writer.get_ref();
        file.write_all(&encoded)?;
        step(Step::BatchSync)?;
        self.journal.flush()?;
        self.bytes += encoded.len() as u64;
        self.has_events = true;
        Ok(())
    }

    fn check_healthy(&self) -> Result<(), JournalError> {
        if self.poisoned {
            return Err(io::Error::other(
                "journal failed; reopen and recover before writing again",
            )
            .into());
        }
        Ok(())
    }
}

impl JournalSink for Journal {
    fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        self.append_batch_at(events, Utc::now())
    }

    fn flush(&mut self) -> Result<(), JournalError> {
        self.check_healthy()?;
        let result = self.journal.flush();
        self.poisoned = result.is_err();
        result.map_err(|error| contextual(&self.path, "flush", error))
    }
}

impl Directory {
    fn recover(&self) -> Result<(), JournalError> {
        let marker = self.path.join(ACTIVE);
        if !regular_or_missing(&marker)? {
            return Ok(());
        }
        let name = fs::read_to_string(&marker)?;
        let name = name.trim_end_matches('\n');
        if !name.starts_with("netband-")
            || !name.ends_with(".csv")
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
        {
            return Err(JournalError::Corrupt(marker));
        }
        let path = self.path.join(name);
        if !regular_or_missing(&path)? {
            return Err(JournalError::Corrupt(path));
        }
        // Open without create: a missing recovery segment must never be recreated.
        let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
        lock_file(&file, &path)?;
        super::validate_header(&mut file, &path)?;
        super::recover_trailing_record(&mut file, &path)?;
        file.sync_data()?;
        Ok(())
    }

    fn create_segment(
        &self,
        now: DateTime<Utc>,
    ) -> Result<(JournalWriter<File>, PathBuf), JournalError> {
        self.create(now)
            .map_err(|error| contextual(&self.path, "create segment", error))
    }

    fn create(&self, now: DateTime<Utc>) -> Result<(JournalWriter<File>, PathBuf), JournalError> {
        // Stage the header outside the CSV namespace. A crash before publication can
        // leave only a temporary file, never an untracked, partial CSV header.
        let pending = self.path.join(PENDING);
        if regular_or_missing(&pending)? {
            let stale = OpenOptions::new().read(true).write(true).open(&pending)?;
            lock_file(&stale, &pending)?;
            fs::remove_file(&pending)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&pending)?;
        step(Step::NewLock)?;
        lock_file(&file, &pending)?;
        step(Step::HeaderWrite)?;
        let mut journal = JournalWriter::from_writer(file)?;
        journal.sync = Some(File::sync_data);
        step(Step::HeaderSync)?;
        journal.flush()?;
        let stem = format!("netband-{}", now.format("%Y%m%dT%H%M%S%.3fZ"));
        let mut sequence = 0_u64;
        let path = loop {
            let suffix = if sequence == 0 {
                String::new()
            } else {
                format!("-{sequence}")
            };
            let path = self.path.join(format!("{stem}{suffix}.csv"));
            step(Step::Publish)?;
            match fs::hard_link(&pending, &path) {
                Ok(()) => break path,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => sequence += 1,
                Err(error) => return Err(error.into()),
            }
        };
        // Unlink staging before any event writes. After a crash, truncating a leftover
        // staging file must never truncate an acknowledged segment through a hard link.
        fs::remove_file(&pending)?;
        step(Step::SegmentDirectorySync)?;
        sync_directory(&self.path)?;
        let temporary = self.path.join(MARKER_TEMP);
        regular_or_missing(&temporary)?;
        let mut marker = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        step(Step::MarkerWrite)?;
        writeln!(marker, "{}", path.file_name().unwrap().to_str().unwrap())?;
        step(Step::MarkerSync)?;
        marker.sync_all()?;
        drop(marker);
        step(Step::MarkerRename)?;
        fs::rename(&temporary, self.path.join(ACTIVE))?;
        step(Step::MarkerDirectorySync)?;
        sync_directory(&self.path)?;
        Ok((journal, path))
    }
}

fn regular_or_missing(path: &Path) -> Result<bool, JournalError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(JournalError::Corrupt(path.to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn sync_directory(path: &Path) -> Result<(), JournalError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    let _ = path;
    Ok(())
}

fn contextual(path: &Path, operation: &str, error: JournalError) -> JournalError {
    match error {
        JournalError::Io(error) => io::Error::new(
            error.kind(),
            format!("{operation} {}: {error}", path.display()),
        )
        .into(),
        error => error,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    OldSync,
    NewLock,
    HeaderWrite,
    HeaderSync,
    Publish,
    SegmentDirectorySync,
    MarkerWrite,
    MarkerSync,
    MarkerRename,
    MarkerDirectorySync,
    BatchWrite,
    BatchSync,
}

#[inline(always)]
fn step(_step: Step) -> Result<(), JournalError> {
    #[cfg(test)]
    tests::inject(_step)?;
    Ok(())
}

#[cfg(test)]
mod tests;
