mod output;
pub use output::Journal;

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::config::OutputTarget;
use crate::console::ConsoleSink;
use crate::model::{ErrorKind, EventKind, MeasurementEvent, Outcome, RunId, RunKind};
use std::collections::BTreeMap;

// Keep CSV column order and its serializer in one declaration. JSONL serializes
// MeasurementEvent directly so connection_details remains an object there.
macro_rules! csv_event {
    ($event:ident; $first:ident => $first_value:expr $(, $field:ident => $value:expr)* $(,)?) => {
        pub const CSV_HEADER: &str = concat!(stringify!($first) $(, ",", stringify!($field))*);
        const CSV_FIELDS: &[&str] = &[stringify!($first) $(, stringify!($field))*];

        struct CsvEvent<'a>(&'a MeasurementEvent);

        impl serde::Serialize for CsvEvent<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeStruct;
                let $event = self.0;
                let mut record = serializer.serialize_struct("MeasurementEvent", CSV_FIELDS.len())?;
                record.serialize_field(stringify!($first), &$first_value)?;
                $(record.serialize_field(stringify!($field), &$value)?;)*
                record.end()
            }
        }
    };
}

csv_event! { event;
    schema_version => event.schema_version,
    event_id => event.event_id,
    event_kind => event.event_kind,
    event_sequence => event.event_sequence,
    run_id => event.run_id,
    parent_run_id => event.parent_run_id,
    run_kind => event.run_kind,

    scheduled_at_utc => event.scheduled_at_utc.map(crate::model::timestamp_text),
    started_at_utc => event.started_at_utc.map(crate::model::timestamp_text),
    finished_at_utc => event.finished_at_utc.map(crate::model::timestamp_text),
    elapsed_ms => event.elapsed_ms,

    outcome => event.outcome,
    message => event.message,

    command => event.command,
    netband_version => event.netband_version,
    process_id => event.process_id,

    interface => event.interface,
    connection_details => event.connection_details.as_ref().map(serde_json::to_string).transpose().map_err(serde::ser::Error::custom)?,

    provider_id => event.provider_id,
    provider_kind => event.provider_kind,
    server_name => event.server_name,

    scheduler_action => event.scheduler_action,
    scheduler_reason => event.scheduler_reason,
    trigger_reason => event.trigger_reason,
    scheduler_not_before_utc => event.scheduler_not_before_utc.map(crate::model::timestamp_text),
    provider_accounting_date => event.provider_accounting_date,
    provider_daily_starts => event.provider_daily_starts,
    bandwidth_start_reserved => event.bandwidth_start_reserved,

    ping_target_ip => event.ping_target_ip,
    ping_local_ip => event.ping_local_ip,
    ping_sequence => event.ping_sequence,
    ping_packets_sent => event.ping_packets_sent,
    ping_packets_received => event.ping_packets_received,
    ping_rtt_ms => event.ping_rtt_ms,
    ping_icmp_type => event.ping_icmp_type,
    ping_icmp_code => event.ping_icmp_code,

    load_run_id => event.load_run_id,
    load_phase => event.load_phase,

    request_id => event.request_id,
    request_direction => event.request_direction,
    request_stage => event.request_stage,
    request_url => event.request_url,
    request_local_ip => event.request_local_ip,
    request_remote_ip => event.request_remote_ip,
    request_http_status => event.request_http_status,
    request_retry_after_ms => event.request_retry_after_ms,
    request_retry_at_utc => event.request_retry_at_utc.map(crate::model::timestamp_text),

    download_request_id => event.download_request_id,
    download_local_ip => event.download_local_ip,
    download_remote_ip => event.download_remote_ip,
    download_bytes => event.download_bytes,
    download_measurement_duration_ms => event.download_measurement_duration_ms,
    download_mbps => event.download_mbps,
    download_server_tcp_min_rtt_ms => event.download_server_tcp_min_rtt_ms,
    download_server_tcp_rtt_ms => event.download_server_tcp_rtt_ms,
    download_server_tcp_retransmitted_bytes => event.download_server_tcp_retransmitted_bytes,

    upload_request_id => event.upload_request_id,
    upload_local_ip => event.upload_local_ip,
    upload_remote_ip => event.upload_remote_ip,
    upload_bytes => event.upload_bytes,
    upload_measurement_duration_ms => event.upload_measurement_duration_ms,
    upload_mbps => event.upload_mbps,
    upload_server_tcp_min_rtt_ms => event.upload_server_tcp_min_rtt_ms,
    upload_server_tcp_rtt_ms => event.upload_server_tcp_rtt_ms,
    upload_server_tcp_retransmitted_bytes => event.upload_server_tcp_retransmitted_bytes,

    error_kind => event.error_kind,
    os_error_code => event.os_error_code,
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("journal CSV serialization failed: {0}")]
    Csv(#[from] csv::Error),
    #[error("existing output has an incompatible CSV header: {0}")]
    Header(PathBuf),
    #[error("existing output contains a malformed CSV record: {0}")]
    Corrupt(PathBuf),
    #[error("output file is already locked by another Netband process: {0}")]
    Locked(PathBuf),
}

impl JournalError {
    pub fn write(error: io::Error) -> Self {
        Self::Io(error)
    }

    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Self::Io(source) if source.kind() == io::ErrorKind::PermissionDenied)
    }
}

/// Serialize CSV records and flush/sync a single writer.
pub struct JournalWriter<W: Write> {
    writer: csv::Writer<W>,
    sync: Option<fn(&W) -> io::Result<()>>,
}

impl<W: Write> fmt::Debug for JournalWriter<W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournalWriter")
            .finish_non_exhaustive()
    }
}

impl<W: Write> JournalWriter<W> {
    pub fn from_writer(writer: W) -> Result<Self, JournalError> {
        let mut journal = Self::without_header(writer);
        journal.writer.write_record(CSV_FIELDS)?;
        journal.writer.flush()?;
        Ok(journal)
    }

    fn without_header(writer: W) -> Self {
        Self {
            writer: csv::WriterBuilder::new()
                .has_headers(false)
                .terminator(csv::Terminator::CRLF)
                .from_writer(writer),
            sync: None,
        }
    }

    pub fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        for event in events {
            self.writer.serialize(CsvEvent(&event.sanitized()))?;
        }
        self.flush()?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), JournalError> {
        self.writer.flush()?;
        if let Some(sync) = self.sync {
            sync(self.writer.get_ref())?;
        }
        Ok(())
    }

    pub fn into_inner(self) -> Result<W, JournalError> {
        self.writer
            .into_inner()
            .map_err(|error| JournalError::Io(error.into_error()))
    }
}

impl JournalWriter<File> {
    pub fn open_at(
        output: &OutputTarget,
        started_at: DateTime<Utc>,
    ) -> Result<(Self, PathBuf), JournalError> {
        match output {
            OutputTarget::File(path) => Self::open_explicit(path),
            OutputTarget::Directory(directory) => Self::create_timestamped(directory, started_at),
        }
    }

    fn open_explicit(path: &Path) -> Result<(Self, PathBuf), JournalError> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        lock_file(&file, path)?;
        let empty = file.metadata()?.len() == 0;
        if !empty {
            validate_header(&mut file, path)?;
            recover_trailing_record(&mut file, path)?;
        }
        file.seek(SeekFrom::End(0))?;
        let mut journal = if empty {
            Self::from_writer(file)?
        } else {
            Self::without_header(file)
        };
        journal.sync = Some(File::sync_data);
        journal.flush()?;
        Ok((journal, path.to_path_buf()))
    }

    fn create_timestamped(
        directory: &Path,
        started_at: DateTime<Utc>,
    ) -> Result<(Self, PathBuf), JournalError> {
        let filename = format!("netband-{}.csv", started_at.format("%Y%m%dT%H%M%S%.3fZ"));
        let path = directory.join(filename);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        lock_file(&file, &path)?;
        let mut journal = Self::from_writer(file)?;
        journal.sync = Some(File::sync_data);
        journal.flush()?;
        Ok((journal, path))
    }
}

fn lock_file(file: &File, path: &Path) -> Result<(), JournalError> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(JournalError::Locked(path.to_path_buf())),
        Err(std::fs::TryLockError::Error(source)) => Err(JournalError::Io(source)),
    }
}

fn validate_header(file: &mut File, path: &Path) -> Result<(), JournalError> {
    file.seek(SeekFrom::Start(0))?;
    let mut header = Vec::new();
    BufReader::new(file).read_until(b'\n', &mut header)?;
    while matches!(header.last(), Some(b'\n' | b'\r')) {
        header.pop();
    }
    if header != CSV_HEADER.as_bytes() {
        return Err(JournalError::Header(path.to_path_buf()));
    }
    Ok(())
}

fn recover_trailing_record(file: &mut File, path: &Path) -> Result<(), JournalError> {
    let length = file.metadata()?.len();
    if length == CSV_HEADER.len() as u64 {
        file.write_all(b"\r\n")?;
        file.sync_data()?;
        return Ok(());
    }

    file.seek(SeekFrom::End(-1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    let terminated = matches!(last[0], b'\n' | b'\r');

    file.seek(SeekFrom::Start(0))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(&mut *file);
    let mut record = csv::ByteRecord::new();
    let mut record_number = 0_u64;
    let mut truncate_at = None;
    while reader.read_byte_record(&mut record)? {
        let start = record
            .position()
            .map_or_else(|| reader.position().byte(), csv::Position::byte);
        let end = reader.position().byte();
        let structurally_valid = record.len() == CSV_FIELDS.len()
            && record
                .iter()
                .all(|field| std::str::from_utf8(field).is_ok());

        if record_number > 0 && !structurally_valid {
            if !terminated && end == length {
                truncate_at = Some(start);
                break;
            }
            return Err(JournalError::Corrupt(path.to_path_buf()));
        }
        if record_number > 0 && !terminated && end == length {
            truncate_at = Some(start);
            break;
        }
        record_number += 1;
    }
    drop(reader);

    if let Some(mut offset) = truncate_at {
        file.seek(SeekFrom::Start(offset))?;
        let mut boundary = [0];
        if file.read(&mut boundary)? == 1 && boundary[0] == b'\n' {
            offset += 1;
        }
        file.set_len(offset)?;
        file.sync_data()?;
        tracing::warn!(
            path = %path.display(),
            truncated_bytes = length - offset,
            "discarded incomplete trailing CSV record"
        );
    }
    Ok(())
}

pub trait JournalSink {
    fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError>;

    fn flush(&mut self) -> Result<(), JournalError> {
        Ok(())
    }
}

impl<W: Write> JournalSink for JournalWriter<W> {
    fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        JournalWriter::append_batch(self, events)
    }

    fn flush(&mut self) -> Result<(), JournalError> {
        JournalWriter::flush(self)
    }
}

struct ActiveRun {
    parent: Option<RunId>,
    kind: RunKind,
    started_at: DateTime<Utc>,
    started: tokio::time::Instant,
}

pub struct OutputCoordinator<J, C> {
    journal: J,
    console: C,
    sequence: u64,
    failed: bool,
    runs: BTreeMap<RunId, ActiveRun>,
}

impl<J, C> OutputCoordinator<J, C>
where
    J: JournalSink,
    C: ConsoleSink,
{
    pub fn new(journal: J, console: C) -> Self {
        Self {
            journal,
            console,
            sequence: 0,
            failed: false,
            runs: BTreeMap::new(),
        }
    }

    pub fn publish_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        if self.failed {
            return Err(JournalError::write(io::Error::other(
                "journal publication previously failed",
            )));
        }
        let mut events = events.to_vec();
        for event in &mut events {
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or_else(|| JournalError::write(io::Error::other("event sequence exhausted")))?;
            event.event_sequence = Some(self.sequence);
            if let Some(run) = self.runs.get(&event.run_id) {
                event.parent_run_id = run.parent;
                event.run_kind = run.kind;
            }
        }
        if let Err(error) = self.journal.append_batch(&events) {
            self.failed = true;
            return Err(error);
        }
        crate::diagnostics::record_events(&events);
        for event in &events {
            self.console.offer(event);
        }
        Ok(())
    }

    pub fn start_session(&mut self, id: RunId, command: &str) -> Result<(), JournalError> {
        if !self.runs.is_empty() {
            return Err(JournalError::write(io::Error::other(
                "session is already active",
            )));
        }
        self.sequence = 0;
        self.begin_run(id, None, RunKind::Session, Some(command))
    }

    pub fn start_run(
        &mut self,
        id: RunId,
        parent: RunId,
        kind: RunKind,
    ) -> Result<(), JournalError> {
        if !self.runs.contains_key(&parent) {
            return Err(JournalError::write(io::Error::other(
                "parent run is not active",
            )));
        }
        self.begin_run(id, Some(parent), kind, None)
    }

    fn begin_run(
        &mut self,
        id: RunId,
        parent: Option<RunId>,
        kind: RunKind,
        command: Option<&str>,
    ) -> Result<(), JournalError> {
        if self.runs.contains_key(&id) {
            return Err(JournalError::write(io::Error::other(
                "run is already active",
            )));
        }
        let started_at = Utc::now();
        let started = tokio::time::Instant::now();
        let mut event =
            MeasurementEvent::new(id, EventKind::RunStarted, Outcome::Started, started_at);
        event.parent_run_id = parent;
        event.run_kind = kind;
        event.started_at_utc = Some(started_at);
        event.finished_at_utc = None;
        if let Some(command) = command {
            event.command = Some(command.to_owned());
            event.netband_version = Some(env!("CARGO_PKG_VERSION").to_owned());
            event.process_id = Some(std::process::id());
        }
        self.publish_batch(&[event])?;
        self.runs.insert(
            id,
            ActiveRun {
                parent,
                kind,
                started_at,
                started,
            },
        );
        Ok(())
    }

    pub fn finish_run(
        &mut self,
        id: RunId,
        outcome: Outcome,
        error: Option<&str>,
    ) -> Result<(), JournalError> {
        if self.runs.values().any(|run| run.parent == Some(id)) {
            return Err(JournalError::write(io::Error::other(
                "run has active children",
            )));
        }
        let run = self
            .runs
            .get(&id)
            .ok_or_else(|| JournalError::write(io::Error::other("run is not active")))?;
        let mut event = MeasurementEvent::new(id, EventKind::RunFinished, outcome, Utc::now());
        event.started_at_utc = Some(run.started_at);
        event.elapsed_ms = Some(run.started.elapsed().as_secs_f64() * 1000.0);
        if let Some(error) = error {
            event.error_kind = Some(ErrorKind::Internal);
            event.message = Some(error.to_owned());
        }
        self.publish_batch(&[event])?;
        self.runs.remove(&id);
        Ok(())
    }

    pub fn finish_session(
        &mut self,
        id: RunId,
        outcome: Outcome,
        error: Option<&str>,
    ) -> Result<(), JournalError> {
        if error.is_some() {
            let children: Vec<_> = self
                .runs
                .iter()
                .filter(|(_, run)| run.parent == Some(id))
                .map(|(id, _)| *id)
                .collect();
            for child in children {
                self.finish_run(child, Outcome::Error, error)?;
            }
        }
        self.finish_run(id, outcome, error)
    }

    pub fn flush(&mut self) -> Result<(), JournalError> {
        self.journal.flush()
    }

    pub fn into_parts(self) -> (J, C) {
        (self.journal, self.console)
    }
}
