//! Scheduler accounting commits precede snapshot replacement and bandwidth admission.
//! A checkpoint anchors the accounting log independently of either snapshot. Complete
//! records beyond it are replayed conservatively; an incomplete record fails closed.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    DeferredOpportunity, ProviderState, STATE_SCHEMA_VERSION, SchedulerError, SchedulerStore,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Checkpoint {
    version: u8,
    installation: String,
    sequence: u64,
    digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    version: u8,
    installation: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    sequence: u64,
    previous_digest: String,
    state: SchedulerStore,
    digest: String,
}

impl Record {
    fn new(sequence: u64, previous_digest: String, state: SchedulerStore) -> Self {
        let mut record = Self {
            sequence,
            previous_digest,
            state,
            digest: String::new(),
        };
        record.digest = record.computed_digest();
        record
    }
    fn computed_digest(&self) -> String {
        digest(&json(&(self.sequence, &self.previous_digest, &self.state)))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Accounting {
    day: NaiveDate,
    runs: Vec<DateTime<Utc>>,
    last_start: Option<DateTime<Utc>>,
    cooldown: Option<DateTime<Utc>>,
    backoff: u8,
    deferred: Option<DeferredOpportunity>,
}

fn accounting(store: &SchedulerStore) -> BTreeMap<&str, Accounting> {
    store
        .providers
        .iter()
        .map(|(id, state)| {
            (
                id.as_str(),
                Accounting {
                    day: state.day_utc,
                    runs: state.runs.clone(),
                    last_start: state.last_started_at_utc,
                    cooldown: state.cooldown_until_utc,
                    backoff: state.backoff_step,
                    deferred: state.deferred.clone(),
                },
            )
        })
        .collect()
}

#[derive(Debug)]
pub(super) struct Persistence {
    path: PathBuf,
    _lock: File,
    checkpoint: Checkpoint,
    committed: SchedulerStore,
    snapshot_digest: Option<String>,
    ledger_len: u64,
    poisoned: bool,
}

struct Log {
    checkpoints: Vec<Checkpoint>,
    states: Vec<SchedulerStore>,
}

impl Persistence {
    pub(super) fn open(path: &Path) -> Result<(Self, SchedulerStore, bool), SchedulerError> {
        if ["lock", "bak", "initialized"]
            .iter()
            .any(|extension| path.with_extension(extension) == path)
        {
            return Err(corrupt(
                path,
                "state path conflicts with a recovery file; use a name such as scheduler.json",
            ));
        }
        let lock = acquire_lock(path)?;
        let marker_path = path.with_extension("initialized");
        let ledger_path = path.with_extension("accounting.jsonl");
        let primary = read_optional(path)?;
        let marker = read_optional(&marker_path)?;
        let ledger = read_optional(&ledger_path)?;
        let backup = path.with_extension("bak");

        // Only a header-only log may resume fresh initialization without a marker.
        // No admission can have occurred before the first checkpoint was installed.
        let fresh = primary.is_none()
            && marker.is_none()
            && !backup.try_exists().map_err(|e| io_error(&backup, e))?;
        let initializing = if primary.is_none() && !fresh {
            let bytes = ledger
                .as_ref()
                .ok_or_else(|| corrupt(path, "initialized state and accounting log are missing"))?;
            let log = read_log(&ledger_path, bytes)?;
            let checkpoint: Checkpoint = parse(
                &marker_path,
                marker
                    .as_ref()
                    .ok_or_else(|| corrupt(path, "initialized state is missing"))?,
            )?;
            validate_checkpoint(&marker_path, &checkpoint, &log)?;
            if log.states.len() <= 1
                && log.states.iter().all(|state| {
                    state.providers.values().all(|provider| {
                        provider.runs.is_empty()
                            && provider.last_started_at_utc.is_none()
                            && provider.cooldown_until_utc.is_none()
                            && provider.backoff_step == 0
                    })
                })
                && !backup.try_exists().map_err(|e| io_error(&backup, e))?
            {
                Some(log.states.last().cloned().unwrap_or_default())
            } else {
                return Err(corrupt(
                    path,
                    "initialized state is missing; preserve the recovery set and restore a verified snapshot",
                ));
            }
        } else {
            None
        };
        let resuming_initialization = initializing.is_some();
        let mut store = match &primary {
            Some(bytes) => parse_store(path, bytes)?,
            None if fresh => SchedulerStore::default(),
            None if resuming_initialization => initializing.unwrap(),
            None => {
                return Err(corrupt(
                    path,
                    "initialized state is missing; preserve the recovery set and restore a verified snapshot",
                ));
            }
        };
        if !fresh && marker.as_ref().is_none_or(Vec::is_empty) {
            return Err(corrupt(
                &marker_path,
                "accounting checkpoint is missing or empty",
            ));
        }

        let log = match ledger {
            Some(bytes) => read_log(&ledger_path, &bytes)?,
            None if fresh => {
                let mut random = [0; 16];
                getrandom::fill(&mut random).map_err(|e| {
                    corrupt(path, format!("cannot create accounting identity: {e}"))
                })?;
                let header = Header {
                    version: STATE_SCHEMA_VERSION,
                    installation: hex(&random),
                };
                let mut bytes = json(&header);
                bytes.push(b'\n');
                atomic_write(&ledger_path, &bytes, FileKind::Ledger)?;
                read_log(&ledger_path, &bytes)?
            }
            None => return Err(corrupt(&ledger_path, "required accounting log is missing")),
        };
        let last = log.checkpoints.last().unwrap().clone();
        if fresh {
            if !log.states.is_empty() {
                return Err(corrupt(
                    &ledger_path,
                    "orphan accounting records are not a new installation",
                ));
            }
        } else if !resuming_initialization {
            let checkpoint = store
                .checkpoint
                .as_ref()
                .ok_or_else(|| corrupt(path, "snapshot has no accounting checkpoint"))?;
            validate_checkpoint(path, checkpoint, &log)?;
            let baseline = checkpoint
                .sequence
                .checked_sub(1)
                .and_then(|i| log.states.get(i as usize));
            if baseline.is_none_or(|baseline| accounting(baseline) != accounting(&store)) {
                return Err(corrupt(
                    path,
                    "snapshot accounting differs from its log record",
                ));
            }
        }
        if !fresh {
            let checkpoint: Checkpoint = parse(&marker_path, marker.as_ref().unwrap())?;
            validate_checkpoint(&marker_path, &checkpoint, &log)?;
        }

        let recovered =
            !fresh && !log.states.is_empty() && store.checkpoint.as_ref() != Some(&last);
        let committed = log.states.last().cloned().unwrap_or_default();
        if recovered {
            for (id, recorded) in &committed.providers {
                match store.providers.get_mut(id) {
                    Some(state) => replay_accounting(state, recorded),
                    None => {
                        store.providers.insert(id.clone(), recorded.clone());
                    }
                }
            }
        }
        store.checkpoint = Some(last.clone());
        // All input validation is complete before changing any recovery artifact.
        if marker.as_deref() != Some(json(&last).as_slice()) {
            atomic_write(&marker_path, &json(&last), FileKind::Checkpoint)?;
        }
        let persistence = Self {
            path: path.to_owned(),
            _lock: lock,
            checkpoint: last,
            committed,
            snapshot_digest: primary.as_deref().map(digest),
            ledger_len: fs::metadata(&ledger_path)
                .map_err(|e| io_error(&ledger_path, e))?
                .len(),
            poisoned: false,
        };
        Ok((persistence, store, recovered))
    }

    pub(super) fn needs_snapshot(&self, store: &SchedulerStore) -> bool {
        self.snapshot_digest.as_ref() != Some(&digest(&json(store)))
    }

    pub(super) fn persist(&mut self, store: &mut SchedulerStore) -> Result<(), SchedulerError> {
        if self.poisoned {
            return Err(corrupt(
                &self.path,
                "previous persistence failure; reopen the scheduler before admitting traffic",
            ));
        }
        // Any write failure makes the in-memory state unsafe to use for admission.
        self.poisoned = true;
        validate_store(&self.path, store)?;
        let previous = read_optional(&self.path)?;
        let marker = self.path.with_extension("initialized");
        let checkpoint: Checkpoint = parse(&marker, &read_required(&marker)?)?;
        let ledger = self.path.with_extension("accounting.jsonl");
        if previous.as_deref().map(digest) != self.snapshot_digest
            || checkpoint != self.checkpoint
            || fs::metadata(&ledger)
                .map_err(|e| io_error(&ledger, e))?
                .len()
                != self.ledger_len
        {
            return Err(corrupt(
                &self.path,
                "recovery files changed while the scheduler was running",
            ));
        }
        if accounting(store) != accounting(&self.committed) {
            let sequence = self
                .checkpoint
                .sequence
                .checked_add(1)
                .ok_or_else(|| corrupt(&self.path, "accounting sequence exhausted"))?;
            let mut state = store.clone();
            state.checkpoint = None;
            let record = Record::new(sequence, self.checkpoint.digest.clone(), state);
            let bytes = json(&record);
            let checkpoint = Checkpoint {
                sequence,
                digest: record.digest.clone(),
                ..self.checkpoint.clone()
            };
            let ledger = self.path.with_extension("accounting.jsonl");
            let mut file = OpenOptions::new()
                .append(true)
                .open(&ledger)
                .map_err(|e| io_error(&ledger, e))?;
            let mut line = bytes;
            line.push(b'\n');
            write_synced(&mut file, &ledger, &line, FileKind::Ledger)?;
            atomic_write(
                &self.path.with_extension("initialized"),
                &json(&checkpoint),
                FileKind::Checkpoint,
            )?;
            self.checkpoint = checkpoint;
            self.ledger_len += line.len() as u64;
            self.committed = record.state;
        }
        store.checkpoint = Some(self.checkpoint.clone());
        if let Some(previous) = previous {
            atomic_write(
                &self.path.with_extension("bak"),
                &previous,
                FileKind::Backup,
            )?;
        }
        let bytes = json(store);
        atomic_write(&self.path, &bytes, FileKind::Snapshot)?;
        self.snapshot_digest = Some(digest(&bytes));
        self.poisoned = false;
        Ok(())
    }
}

fn replay_accounting(state: &mut ProviderState, recorded: &ProviderState) {
    state.day_utc = recorded.day_utc;
    state.runs.clone_from(&recorded.runs);
    state.last_started_at_utc = recorded.last_started_at_utc;
    state.cooldown_until_utc = recorded.cooldown_until_utc;
    state.backoff_step = recorded.backoff_step;
    state.last_observed_utc = state.last_observed_utc.max(recorded.last_observed_utc);
    // Old scheduling details must not undo accounting or retry progress during replay.
    state.deferred.clone_from(&recorded.deferred);
    state
        .interface_triggers
        .clone_from(&recorded.interface_triggers);
}

fn read_log(path: &Path, bytes: &[u8]) -> Result<Log, SchedulerError> {
    if !bytes.ends_with(b"\n") {
        return Err(corrupt(
            path,
            "accounting log has an incomplete final record",
        ));
    }
    let mut lines = bytes[..bytes.len() - 1].split(|b| *b == b'\n');
    let first = lines
        .next()
        .ok_or_else(|| corrupt(path, "accounting log is empty"))?;
    let header: Header = parse(path, first)?;
    if header.version != STATE_SCHEMA_VERSION
        || header.installation.len() != 32
        || !header.installation.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(corrupt(
            path,
            "unsupported accounting header or invalid installation identity",
        ));
    }
    let mut checkpoints = vec![Checkpoint {
        version: header.version,
        installation: header.installation.clone(),
        sequence: 0,
        digest: digest(first),
    }];
    let mut states = Vec::new();
    for line in lines {
        let record: Record = parse(path, line)?;
        let previous = checkpoints.last().unwrap();
        if record.sequence != previous.sequence + 1
            || record.previous_digest != previous.digest
            || record.state.schema_version != STATE_SCHEMA_VERSION
            || record.state.checkpoint.is_some()
            || record.digest != record.computed_digest()
        {
            return Err(corrupt(
                path,
                "accounting sequence, digest chain, or record schema is invalid",
            ));
        }
        validate_store(path, &record.state)?;
        checkpoints.push(Checkpoint {
            sequence: record.sequence,
            digest: record.digest,
            ..previous.clone()
        });
        states.push(record.state);
    }
    Ok(Log {
        checkpoints,
        states,
    })
}

fn validate_checkpoint(
    path: &Path,
    checkpoint: &Checkpoint,
    log: &Log,
) -> Result<(), SchedulerError> {
    let index = usize::try_from(checkpoint.sequence).ok();
    if index.and_then(|i| log.checkpoints.get(i)) != Some(checkpoint) {
        return Err(corrupt(
            path,
            "accounting log does not match the committed checkpoint",
        ));
    }
    Ok(())
}

fn parse_store(path: &Path, bytes: &[u8]) -> Result<SchedulerStore, SchedulerError> {
    let store: SchedulerStore = parse(path, bytes)?;
    if store.schema_version != STATE_SCHEMA_VERSION {
        return Err(SchedulerError::UnsupportedSchema(store.schema_version));
    }
    validate_store(path, &store)?;
    Ok(store)
}

fn validate_store(path: &Path, store: &SchedulerStore) -> Result<(), SchedulerError> {
    for (id, state) in &store.providers {
        if id.is_empty()
            || state.rng_state == 0
            || state.policy_slot_jitter_pct > 100
            || state.policy_min_spacing_ms == 0
            || state.backoff_step > 4
            || state.runs.windows(2).any(|runs| runs[0] > runs[1])
            || state
                .runs
                .last()
                .is_some_and(|run| Some(*run) > state.last_started_at_utc)
        {
            return Err(corrupt(
                path,
                "invalid provider accounting or scheduling state",
            ));
        }
    }
    Ok(())
}

fn json(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).expect("scheduler data serializes")
}
fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn corrupt(path: &Path, message: impl Into<String>) -> SchedulerError {
    SchedulerError::Corrupt {
        path: path.to_owned(),
        message: message.into(),
    }
}
fn io_error(path: &Path, source: io::Error) -> SchedulerError {
    SchedulerError::Io {
        path: path.to_owned(),
        source,
    }
}
fn parse<T: serde::de::DeserializeOwned>(path: &Path, bytes: &[u8]) -> Result<T, SchedulerError> {
    serde_json::from_slice(bytes).map_err(|e| corrupt(path, e.to_string()))
}
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, SchedulerError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path, e)),
    }
}
fn read_required(path: &Path) -> Result<Vec<u8>, SchedulerError> {
    read_optional(path)?.ok_or_else(|| corrupt(path, "required recovery file is missing"))
}

fn acquire_lock(path: &Path) -> Result<File, SchedulerError> {
    let parent = path
        .parent()
        .ok_or_else(|| corrupt(path, "state file has no parent"))?;
    let mut missing = Vec::new();
    let mut directory = parent;
    while !directory.as_os_str().is_empty()
        && !directory.try_exists().map_err(|e| io_error(directory, e))?
    {
        missing.push(directory);
        directory = directory.parent().unwrap_or(Path::new("."));
    }
    fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    for directory in missing {
        sync_directory(
            directory
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
    }
    let path = path.with_extension("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| io_error(&path, e))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(SchedulerError::Locked(path)),
        Err(std::fs::TryLockError::Error(e)) => Err(io_error(&path, e)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Ledger,
    Checkpoint,
    Backup,
    Snapshot,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Write,
    #[cfg(test)]
    PartialWrite,
    Sync,
    Rename,
    DirectorySync,
}

fn step(kind: FileKind, step: Step, path: &Path) -> Result<(), SchedulerError> {
    #[cfg(test)]
    if tests::fail_at(kind, step) {
        return Err(io_error(
            path,
            io::Error::other("injected persistence failure"),
        ));
    }
    let _ = (kind, step, path);
    Ok(())
}

fn write_synced(
    file: &mut File,
    path: &Path,
    bytes: &[u8],
    kind: FileKind,
) -> Result<(), SchedulerError> {
    step(kind, Step::Write, path)?;
    #[cfg(test)]
    if let Err(error) = step(kind, Step::PartialWrite, path) {
        file.write_all(&bytes[..bytes.len() / 2])
            .map_err(|e| io_error(path, e))?;
        file.sync_all().map_err(|e| io_error(path, e))?;
        return Err(error);
    }
    file.write_all(bytes).map_err(|e| io_error(path, e))?;
    step(kind, Step::Sync, path)?;
    file.sync_all().map_err(|e| io_error(path, e))
}

fn atomic_write(path: &Path, bytes: &[u8], kind: FileKind) -> Result<(), SchedulerError> {
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension().unwrap_or_default().to_string_lossy()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|e| io_error(&temporary, e))?;
    write_synced(&mut file, &temporary, bytes, kind)?;
    drop(file);
    step(kind, Step::Rename, path)?;
    fs::rename(&temporary, path).map_err(|e| io_error(path, e))?;
    step(kind, Step::DirectorySync, path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), SchedulerError> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| io_error(path, e))?;
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests;
